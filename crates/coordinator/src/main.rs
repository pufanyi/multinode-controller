use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::Arc,
};

use agent_jobstore::SqliteJobStore;
use agent_protocol::{
    ClientRequest, ClientResponse, CoordinatorMessage, JobFinished, JobId, LogLine, NodeId,
    NodeInfo, NodeSummary, RunProcessRequest, RunSpec, RunStarted, TaskAssignment, TaskExited,
    TaskId, TaskSpec, TaskStarted, WireMessage, WorkerError, WorkerMessage,
};
use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use clap::Parser;
use futures_util::{
    stream::{SplitSink, SplitStream},
    Sink, SinkExt, StreamExt,
};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::{broadcast, mpsc, Mutex},
};
use tokio_tungstenite::{accept_async, tungstenite::Message, WebSocketStream};

#[derive(Parser, Debug)]
#[command(about = "Coordinator daemon for the multinode agent runtime")]
struct Args {
    #[arg(long, default_value = "127.0.0.1:8765")]
    listen: String,

    #[arg(long)]
    db: Option<PathBuf>,
}

#[derive(Clone)]
struct AppState {
    inner: Arc<Mutex<CoordinatorState>>,
    events: broadcast::Sender<CoordinatorEvent>,
    store: SqliteJobStore,
}

#[derive(Default)]
struct CoordinatorState {
    nodes: HashMap<NodeId, WorkerHandle>,
}

struct WorkerHandle {
    info: NodeInfo,
    sender: mpsc::Sender<CoordinatorMessage>,
    last_seen: DateTime<Utc>,
    running_tasks: usize,
}

#[derive(Clone, Debug)]
enum CoordinatorEvent {
    Started(TaskStarted),
    Log(LogLine),
    Exited(TaskExited),
    Error(WorkerError),
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let db_path = args.db.unwrap_or_else(default_db_path);
    let store = SqliteJobStore::open(db_path)?;
    let (events, _) = broadcast::channel(1024);
    let state = AppState {
        inner: Arc::new(Mutex::new(CoordinatorState::default())),
        events,
        store,
    };

    let listener = TcpListener::bind(&args.listen)
        .await
        .with_context(|| format!("failed to bind {}", args.listen))?;
    println!("agent-coordinator listening on ws://{}", args.listen);

    loop {
        let (stream, addr) = listener.accept().await?;
        let state = state.clone();
        tokio::spawn(async move {
            if let Err(error) = handle_connection(stream, state).await {
                eprintln!("connection {addr} ended with error: {error:#}");
            }
        });
    }
}

fn default_db_path() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".agent-runtime").join("coordinator.sqlite")
}

async fn handle_connection(stream: TcpStream, state: AppState) -> Result<()> {
    let ws = accept_async(stream).await?;
    let (mut sink, mut stream) = ws.split();
    let first = read_wire(&mut stream)
        .await?
        .ok_or_else(|| anyhow!("connection closed before role registration"))?;

    match first {
        WireMessage::Worker(WorkerMessage::Register(info)) => {
            handle_worker(info, state, sink, stream).await
        }
        WireMessage::ClientRequest(request) => handle_client(request, state, &mut sink).await,
        other => {
            send_wire(
                &mut sink,
                WireMessage::ClientResponse(ClientResponse::Error(format!(
                    "unexpected first message: {other:?}"
                ))),
            )
            .await?;
            Ok(())
        }
    }
}

async fn handle_worker(
    info: NodeInfo,
    state: AppState,
    mut sink: SplitSink<WebSocketStream<TcpStream>, Message>,
    mut stream: SplitStream<WebSocketStream<TcpStream>>,
) -> Result<()> {
    let (tx, mut rx) = mpsc::channel::<CoordinatorMessage>(256);
    let node_id = info.node_id.clone();
    println!("worker registered: {} ({})", info.node_name, info.hostname);
    state.store.upsert_node(&info)?;

    {
        let mut guard = state.inner.lock().await;
        guard.nodes.insert(
            node_id.clone(),
            WorkerHandle {
                info,
                sender: tx,
                last_seen: Utc::now(),
                running_tasks: 0,
            },
        );
    }

    let writer = tokio::spawn(async move {
        while let Some(message) = rx.recv().await {
            if send_wire(&mut sink, WireMessage::Coordinator(message))
                .await
                .is_err()
            {
                break;
            }
        }
    });

    while let Some(message) = read_wire(&mut stream).await? {
        match message {
            WireMessage::Worker(WorkerMessage::Heartbeat(heartbeat)) => {
                let mut guard = state.inner.lock().await;
                if let Some(node) = guard.nodes.get_mut(&heartbeat.node_id) {
                    node.last_seen = heartbeat.timestamp;
                    node.running_tasks = heartbeat.running_tasks;
                }
            }
            WireMessage::Worker(WorkerMessage::TaskStarted(event)) => {
                state.store.record_task_started(&event)?;
                let _ = state.events.send(CoordinatorEvent::Started(event));
            }
            WireMessage::Worker(WorkerMessage::LogLine(line)) => {
                state.store.record_log_line(&line)?;
                let _ = state.events.send(CoordinatorEvent::Log(line));
            }
            WireMessage::Worker(WorkerMessage::TaskExited(event)) => {
                state.store.record_task_exited(&event)?;
                {
                    let mut guard = state.inner.lock().await;
                    if let Some(node) = guard.nodes.get_mut(&event.node_id) {
                        node.running_tasks = node.running_tasks.saturating_sub(1);
                    }
                }
                let _ = state.events.send(CoordinatorEvent::Exited(event));
            }
            WireMessage::Worker(WorkerMessage::Error(error)) => {
                let _ = state.events.send(CoordinatorEvent::Error(error));
            }
            _ => {}
        }
    }

    writer.abort();
    state.inner.lock().await.nodes.remove(&node_id);
    println!("worker disconnected: {node_id}");
    Ok(())
}

async fn handle_client(
    request: ClientRequest,
    state: AppState,
    sink: &mut SplitSink<WebSocketStream<TcpStream>, Message>,
) -> Result<()> {
    match request {
        ClientRequest::ListNodes => {
            let nodes = list_nodes(&state).await;
            send_wire(
                sink,
                WireMessage::ClientResponse(ClientResponse::Nodes(nodes)),
            )
            .await
        }
        ClientRequest::RunProcess(request) => run_process(request, state, sink).await,
    }
}

async fn list_nodes(state: &AppState) -> Vec<NodeSummary> {
    state
        .inner
        .lock()
        .await
        .nodes
        .values()
        .map(|node| NodeSummary {
            node_id: node.info.node_id.clone(),
            node_name: node.info.node_name.clone(),
            hostname: node.info.hostname.clone(),
            online: true,
            last_seen: node.last_seen,
            running_tasks: node.running_tasks,
            labels: node.info.labels.clone(),
        })
        .collect()
}

async fn run_process(
    request: RunProcessRequest,
    state: AppState,
    sink: &mut SplitSink<WebSocketStream<TcpStream>, Message>,
) -> Result<()> {
    if request.argv.is_empty() {
        return send_wire(
            sink,
            WireMessage::ClientResponse(ClientResponse::Error(
                "run argv cannot be empty".to_owned(),
            )),
        )
        .await;
    }

    let job_id = JobId::new();
    let cwd = request.cwd.unwrap_or_else(|| PathBuf::from("."));
    let mut selected_nodes = request.nodes;
    let mut dispatch = Vec::new();
    let mut assignments = Vec::new();
    let mut subscription = state.events.subscribe();

    {
        let mut guard = state.inner.lock().await;
        if selected_nodes.is_empty() {
            selected_nodes = guard.nodes.keys().cloned().collect();
        }

        for node_id in selected_nodes {
            let Some(node) = guard.nodes.get_mut(&node_id) else {
                send_wire(
                    sink,
                    WireMessage::ClientResponse(ClientResponse::Error(format!(
                        "node {node_id} is not connected"
                    ))),
                )
                .await?;
                continue;
            };

            let task_id = TaskId::new();
            let run = RunSpec::simple(
                job_id.clone(),
                task_id.clone(),
                node_id.clone(),
                request.argv.clone(),
                cwd.clone(),
                request.env.clone(),
            );
            let assignment = TaskAssignment {
                job_id: job_id.clone(),
                task_id,
                node_id,
            };
            node.running_tasks += 1;
            dispatch.push((node.sender.clone(), TaskSpec { run }));
            assignments.push(assignment);
        }
    }

    if assignments.is_empty() {
        return send_wire(
            sink,
            WireMessage::ClientResponse(ClientResponse::Error(
                "no connected nodes matched the request".to_owned(),
            )),
        )
        .await;
    }

    send_wire(
        sink,
        WireMessage::ClientResponse(ClientResponse::RunStarted(RunStarted {
            job_id: job_id.clone(),
            tasks: assignments.clone(),
        })),
    )
    .await?;

    for (sender, spec) in dispatch {
        sender
            .send(CoordinatorMessage::StartTask(spec))
            .await
            .map_err(|_| anyhow!("failed to dispatch task to worker"))?;
    }

    if !request.wait {
        return Ok(());
    }

    let mut pending: HashSet<TaskId> = assignments
        .iter()
        .map(|assignment| assignment.task_id.clone())
        .collect();
    let mut exits = Vec::new();

    while !pending.is_empty() {
        match subscription.recv().await? {
            CoordinatorEvent::Started(event) if pending.contains(&event.task_id) => {
                send_wire(
                    sink,
                    WireMessage::ClientResponse(ClientResponse::TaskStarted(event)),
                )
                .await?;
            }
            CoordinatorEvent::Log(line) if pending.contains(&line.task_id) => {
                send_wire(
                    sink,
                    WireMessage::ClientResponse(ClientResponse::LogLine(line)),
                )
                .await?;
            }
            CoordinatorEvent::Exited(event) if pending.remove(&event.task_id) => {
                send_wire(
                    sink,
                    WireMessage::ClientResponse(ClientResponse::TaskExited(event.clone())),
                )
                .await?;
                exits.push(event);
            }
            CoordinatorEvent::Error(error) => {
                send_wire(
                    sink,
                    WireMessage::ClientResponse(ClientResponse::Error(error.message)),
                )
                .await?;
            }
            _ => {}
        }
    }

    let success = exits.iter().all(|exit| exit.success);
    send_wire(
        sink,
        WireMessage::ClientResponse(ClientResponse::JobFinished(JobFinished {
            job_id,
            success,
            exits,
        })),
    )
    .await
}

async fn read_wire(
    stream: &mut SplitStream<WebSocketStream<TcpStream>>,
) -> Result<Option<WireMessage>> {
    let Some(message) = stream.next().await else {
        return Ok(None);
    };
    let message = message?;
    if message.is_close() {
        return Ok(None);
    }
    let text = message.into_text()?;
    Ok(Some(WireMessage::from_text(&text)?))
}

async fn send_wire<S>(sink: &mut S, wire: WireMessage) -> Result<()>
where
    S: Sink<Message> + Unpin,
    S::Error: std::error::Error + Send + Sync + 'static,
{
    sink.send(Message::Text(wire.to_text()?.into())).await?;
    Ok(())
}
