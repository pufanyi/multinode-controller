use std::{path::PathBuf, time::Duration};

use agent_executor::{ExecutionEvent, ExecutionRequest, Executor, LocalExecutor};
use agent_policy::{AllowAllPolicy, PolicyEngine};
use agent_protocol::{
    CoordinatorMessage, DecisionKind, NodeHeartbeat, NodeInfo, OperationRequest, TaskSpec,
    WireMessage, WorkerError, WorkerMessage,
};
use agent_sandbox_linux::{NoneSandbox, SandboxBackend};
use anyhow::{Context, Result};
use chrono::Utc;
use clap::Parser;
use futures_util::{SinkExt, StreamExt};
use tokio::{sync::mpsc, time::sleep};
use tokio_tungstenite::{connect_async, tungstenite::Message};

#[derive(Parser, Debug, Clone)]
#[command(about = "Worker daemon for the multinode agent runtime")]
struct Args {
    #[arg(long, default_value = "ws://127.0.0.1:8765")]
    coordinator: String,

    #[arg(long)]
    node_name: Option<String>,

    #[arg(long)]
    workspace_root: Option<PathBuf>,

    #[arg(long)]
    policy: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let mut backoff = Duration::from_secs(1);

    loop {
        match run_worker(args.clone()).await {
            Ok(()) => backoff = Duration::from_secs(1),
            Err(error) => {
                eprintln!("worker connection failed: {error:#}");
                sleep(backoff).await;
                backoff = (backoff * 2).min(Duration::from_secs(30));
            }
        }
    }
}

async fn run_worker(args: Args) -> Result<()> {
    let hostname = hostname::get()
        .context("failed to read hostname")?
        .to_string_lossy()
        .to_string();
    let node_name = args.node_name.clone().unwrap_or_else(|| hostname.clone());
    let node = NodeInfo::new(node_name, hostname);
    let node_id = node.node_id.clone();

    let (ws, _) = connect_async(&args.coordinator)
        .await
        .with_context(|| format!("failed to connect to {}", args.coordinator))?;
    let (mut write, mut read) = ws.split();
    let (out_tx, mut out_rx) = mpsc::channel::<WireMessage>(512);

    let writer = tokio::spawn(async move {
        while let Some(message) = out_rx.recv().await {
            let text = match message.to_text() {
                Ok(text) => text,
                Err(error) => {
                    eprintln!("failed to serialize worker message: {error:#}");
                    continue;
                }
            };
            if write.send(Message::Text(text.into())).await.is_err() {
                break;
            }
        }
    });

    out_tx
        .send(WireMessage::Worker(WorkerMessage::Register(node.clone())))
        .await?;

    let heartbeat_tx = out_tx.clone();
    let heartbeat_node_id = node_id.clone();
    let executor = LocalExecutor::default();
    let heartbeat_executor = executor.clone();
    let heartbeat = tokio::spawn(async move {
        loop {
            let running_tasks = heartbeat_executor.running_count().await;
            if heartbeat_tx
                .send(WireMessage::Worker(WorkerMessage::Heartbeat(
                    NodeHeartbeat {
                        node_id: heartbeat_node_id.clone(),
                        timestamp: Utc::now(),
                        running_tasks,
                    },
                )))
                .await
                .is_err()
            {
                break;
            }
            sleep(Duration::from_secs(5)).await;
        }
    });

    println!("agent-worker connected as {node_id}");
    let policy = AllowAllPolicy;
    let sandbox = NoneSandbox;

    while let Some(message) = read.next().await {
        let message = message?;
        if message.is_close() {
            break;
        }
        let text = message.into_text()?;
        let wire = WireMessage::from_text(&text)?;

        match wire {
            WireMessage::Coordinator(CoordinatorMessage::StartTask(spec)) => {
                let out_tx = out_tx.clone();
                let executor = executor.clone();
                let policy = policy.clone();
                let sandbox = sandbox.clone();
                tokio::spawn(async move {
                    if let Err(error) =
                        start_task(spec, policy, sandbox, executor, out_tx.clone()).await
                    {
                        let _ = out_tx
                            .send(WireMessage::Worker(WorkerMessage::Error(WorkerError {
                                node_id: None,
                                task_id: None,
                                message: format!("{error:#}"),
                            })))
                            .await;
                    }
                });
            }
            WireMessage::Coordinator(CoordinatorMessage::KillTask(task_id)) => {
                if let Err(error) = executor.kill(&task_id).await {
                    out_tx
                        .send(WireMessage::Worker(WorkerMessage::Error(WorkerError {
                            node_id: Some(node_id.clone()),
                            task_id: Some(task_id),
                            message: format!("{error:#}"),
                        })))
                        .await?;
                }
            }
            WireMessage::Coordinator(CoordinatorMessage::Ping) => {}
            _ => {}
        }
    }

    heartbeat.abort();
    writer.abort();
    Ok(())
}

async fn start_task(
    spec: TaskSpec,
    policy: AllowAllPolicy,
    sandbox: NoneSandbox,
    executor: LocalExecutor,
    out_tx: mpsc::Sender<WireMessage>,
) -> Result<()> {
    let operation = OperationRequest::process_start(&spec.run, "agent-worker");
    let decision = policy.authorize(&operation, &spec.run);

    match decision.decision {
        DecisionKind::Allow => {}
        DecisionKind::Ask | DecisionKind::Deny => {
            out_tx
                .send(WireMessage::Worker(WorkerMessage::Error(WorkerError {
                    node_id: Some(spec.run.node_id.clone()),
                    task_id: Some(spec.run.task_id.clone()),
                    message: decision.reason,
                })))
                .await?;
            return Ok(());
        }
    }

    let command = sandbox.prepare(&spec.run, &decision).await?;
    let (event_tx, mut event_rx) = mpsc::channel::<ExecutionEvent>(256);
    executor
        .start(
            ExecutionRequest {
                spec: spec.run,
                command,
            },
            event_tx,
        )
        .await?;

    while let Some(event) = event_rx.recv().await {
        let worker_message = match event {
            ExecutionEvent::Started(started) => WorkerMessage::TaskStarted(started),
            ExecutionEvent::Log(line) => WorkerMessage::LogLine(line),
            ExecutionEvent::Exited(exited) => WorkerMessage::TaskExited(exited),
        };
        if out_tx
            .send(WireMessage::Worker(worker_message))
            .await
            .is_err()
        {
            break;
        }
    }

    Ok(())
}
