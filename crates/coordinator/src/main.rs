use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{Arc, Mutex as StdMutex},
};

use agent_auth::{authenticate_request, unauthorized_response, AuthConfig, AuthRole};
use agent_jobstore::SqliteJobStore;
use agent_protocol::{
    ClientRequest, ClientResponse, CoordinatorMessage, JobFinished, JobId, KillJobRequest,
    ListJobsRequest, LogLine, LogStream, NodeId, NodeInfo, NodeSummary, RunProcessRequest, RunSpec,
    RunStarted, TailJobRequest, TaskAssignment, TaskExited, TaskId, TaskSpec, TaskStarted,
    WireMessage, WorkerError, WorkerMessage,
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
use tokio_tungstenite::{
    accept_hdr_async,
    tungstenite::{
        handshake::server::{Request, Response},
        Message,
    },
    WebSocketStream,
};

#[derive(Parser, Debug)]
#[command(about = "Coordinator daemon for the multinode agent runtime")]
struct Args {
    #[arg(long, default_value = "127.0.0.1:8765")]
    listen: String,

    #[arg(long)]
    db: Option<PathBuf>,

    /// Shared bearer token file for both workers and clients.
    #[arg(long)]
    token_file: Option<PathBuf>,

    /// Bearer token file accepted from worker daemons. Overrides --token-file.
    #[arg(long)]
    worker_token_file: Option<PathBuf>,

    /// Bearer token file accepted from agentctl/MCP clients. Overrides --token-file.
    #[arg(long)]
    client_token_file: Option<PathBuf>,
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
    running_tasks: HashMap<TaskId, JobId>,
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
    let db_path = args.db.clone().unwrap_or_else(default_db_path);
    let auth = AuthConfig::from_files(
        args.token_file.as_ref(),
        args.worker_token_file.as_ref(),
        args.client_token_file.as_ref(),
    )?;
    let store = SqliteJobStore::open(db_path)?;
    let (events, _) = broadcast::channel(1024);
    let state = AppState {
        inner: Arc::new(Mutex::new(CoordinatorState::default())),
        events,
        store,
    };
    if !auth.enabled() {
        eprintln!("warning: coordinator authentication is disabled; use --token-file for trusted clusters");
    }

    let listener = TcpListener::bind(&args.listen)
        .await
        .with_context(|| format!("failed to bind {}", args.listen))?;
    println!("agent-coordinator listening on ws://{}", args.listen);

    loop {
        let (stream, addr) = listener.accept().await?;
        let state = state.clone();
        let auth = auth.clone();
        tokio::spawn(async move {
            if let Err(error) = handle_connection(stream, state, auth).await {
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

async fn handle_connection(stream: TcpStream, state: AppState, auth: AuthConfig) -> Result<()> {
    let (ws, auth_role) = accept_authenticated(stream, auth).await?;
    let (mut sink, mut stream) = ws.split();
    let first = read_wire(&mut stream)
        .await?
        .ok_or_else(|| anyhow!("connection closed before role registration"))?;

    match first {
        WireMessage::Worker(WorkerMessage::Register(info)) => {
            ensure_role(auth_role, AuthRole::Worker)?;
            handle_worker(info, state, sink, stream).await
        }
        WireMessage::ClientRequest(request) => {
            ensure_role(auth_role, AuthRole::Client)?;
            handle_client(request, state, &mut sink).await
        }
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

async fn accept_authenticated(
    stream: TcpStream,
    auth: AuthConfig,
) -> Result<(WebSocketStream<TcpStream>, Option<AuthRole>)> {
    if !auth.enabled() {
        let ws = accept_hdr_async(stream, |_request: &Request, response: Response| {
            Ok(response)
        })
        .await?;
        return Ok((ws, None));
    }

    let role_slot = Arc::new(StdMutex::new(None));
    let role_for_callback = Arc::clone(&role_slot);
    let ws = accept_hdr_async(stream, move |request: &Request, response: Response| {
        let role = authenticate_request(request, &auth).map_err(unauthorized_response)?;
        *role_for_callback.lock().expect("auth role mutex poisoned") = Some(role);
        Ok(response)
    })
    .await?;
    let role = *role_slot.lock().expect("auth role mutex poisoned");
    Ok((ws, role))
}

fn ensure_role(auth_role: Option<AuthRole>, expected: AuthRole) -> Result<()> {
    if auth_role.is_some_and(|role| role != expected) {
        return Err(anyhow!("authenticated role does not match first message"));
    }
    Ok(())
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
                running_tasks: HashMap::new(),
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
                }
            }
            WireMessage::Worker(WorkerMessage::TaskStarted(event)) => {
                state.store.record_task_started(&event)?;
                {
                    let mut guard = state.inner.lock().await;
                    if let Some(node) = guard.nodes.get_mut(&event.node_id) {
                        node.running_tasks
                            .insert(event.task_id.clone(), event.job_id.clone());
                    }
                }
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
                        node.running_tasks.remove(&event.task_id);
                    }
                }
                let _ = state.events.send(CoordinatorEvent::Exited(event));
            }
            WireMessage::Worker(WorkerMessage::Error(error)) => {
                record_worker_error_exit(&state, &error).await?;
                let _ = state.events.send(CoordinatorEvent::Error(error));
            }
            _ => {}
        }
    }

    writer.abort();
    mark_node_lost(&state, &node_id, "worker disconnected").await?;
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
        ClientRequest::ListJobs(request) => list_jobs(request, state, sink).await,
        ClientRequest::TailJob(request) => tail_job(request, state, sink).await,
        ClientRequest::KillJob(request) => kill_job(request, state, sink).await,
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
            running_tasks: node.running_tasks.len(),
            labels: node.info.labels.clone(),
        })
        .collect()
}

async fn list_jobs(
    request: ListJobsRequest,
    state: AppState,
    sink: &mut SplitSink<WebSocketStream<TcpStream>, Message>,
) -> Result<()> {
    let jobs = state.store.list_jobs(request.limit.max(1))?;
    send_wire(
        sink,
        WireMessage::ClientResponse(ClientResponse::Jobs(jobs)),
    )
    .await
}

async fn tail_job(
    request: TailJobRequest,
    state: AppState,
    sink: &mut SplitSink<WebSocketStream<TcpStream>, Message>,
) -> Result<()> {
    let logs = state
        .store
        .tail_logs(&request.job_id, request.lines.max(1))?;
    send_wire(
        sink,
        WireMessage::ClientResponse(ClientResponse::Logs(logs)),
    )
    .await
}

async fn kill_job(
    request: KillJobRequest,
    state: AppState,
    sink: &mut SplitSink<WebSocketStream<TcpStream>, Message>,
) -> Result<()> {
    let dispatch = {
        let guard = state.inner.lock().await;
        guard
            .nodes
            .values()
            .flat_map(|node| {
                node.running_tasks
                    .iter()
                    .filter(|(_, job_id)| **job_id == request.job_id)
                    .map(|(task_id, _)| (node.sender.clone(), task_id.clone()))
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>()
    };

    for (sender, task_id) in dispatch {
        sender
            .send(CoordinatorMessage::KillTask(task_id))
            .await
            .map_err(|_| anyhow!("failed to dispatch kill to worker"))?;
    }

    send_wire(sink, WireMessage::ClientResponse(ClientResponse::Ack)).await
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
                task_id: task_id.clone(),
                node_id: node_id.clone(),
            };
            state
                .store
                .record_task_dispatched(&job_id, &task_id, &node_id, Utc::now())?;
            node.running_tasks.insert(task_id, job_id.clone());
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
        return send_wire(sink, WireMessage::ClientResponse(ClientResponse::Ack)).await;
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

async fn record_worker_error_exit(state: &AppState, error: &WorkerError) -> Result<()> {
    let (Some(node_id), Some(task_id)) = (&error.node_id, &error.task_id) else {
        return Ok(());
    };

    let job_id = {
        let mut guard = state.inner.lock().await;
        let Some(node) = guard.nodes.get_mut(node_id) else {
            return Ok(());
        };
        node.running_tasks.remove(task_id)
    };

    let Some(job_id) = job_id else {
        return Ok(());
    };

    let event = TaskExited {
        job_id,
        task_id: task_id.clone(),
        node_id: node_id.clone(),
        exit_code: None,
        success: false,
        timestamp: Utc::now(),
    };
    state.store.record_task_exited(&event)?;
    let _ = state.events.send(CoordinatorEvent::Exited(event));
    Ok(())
}

async fn mark_node_lost(state: &AppState, node_id: &NodeId, reason: &str) -> Result<()> {
    let lost_tasks = {
        let mut guard = state.inner.lock().await;
        guard
            .nodes
            .remove(node_id)
            .map(|node| node.running_tasks.into_iter().collect::<Vec<_>>())
            .unwrap_or_default()
    };

    for (task_id, job_id) in lost_tasks {
        let timestamp = Utc::now();
        let line = LogLine {
            job_id: job_id.clone(),
            task_id: task_id.clone(),
            node_id: node_id.clone(),
            stream: LogStream::System,
            timestamp,
            line: reason.to_owned(),
            offset: 0,
        };
        state.store.record_log_line(&line)?;
        let _ = state.events.send(CoordinatorEvent::Log(line));

        let event = TaskExited {
            job_id,
            task_id,
            node_id: node_id.clone(),
            exit_code: None,
            success: false,
            timestamp,
        };
        state.store.record_task_exited(&event)?;
        let _ = state.events.send(CoordinatorEvent::Exited(event));
    }

    Ok(())
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
