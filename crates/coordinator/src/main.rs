use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{Arc, Mutex as StdMutex},
};

use agent_auth::{authenticate_request, unauthorized_response, AuthConfig, AuthRole};
use agent_jobstore::SqliteJobStore;
use agent_protocol::{
    ClientRequest, ClientResponse, CoordinatorMessage, CreateLeaseRequest, DiagnoseJobRequest,
    JobDiagnosis, JobFinished, JobId, JobStatusRequest, JobSummary, KillJobRequest, LeaseId,
    LeaseSummary, ListJobsRequest, LogLine, LogStream, NodeId, NodeInfo, NodeSummary,
    ReleaseLeaseRequest, RunProcessRequest, RunSpec, RunStarted, TailJobRequest, TaskAssignment,
    TaskExited, TaskId, TaskSpec, TaskStarted, WireMessage, WorkerError, WorkerMessage,
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
    accept_async, accept_hdr_async,
    tungstenite::{
        handshake::server::{Callback, ErrorResponse, Request, Response},
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
    leases: HashMap<LeaseId, LeaseRecord>,
}

struct WorkerHandle {
    info: NodeInfo,
    sender: mpsc::Sender<CoordinatorMessage>,
    last_seen: DateTime<Utc>,
    running_tasks: HashMap<TaskId, JobId>,
}

#[derive(Clone, Debug)]
struct LeaseRecord {
    lease_id: LeaseId,
    nodes: Vec<NodeId>,
    gpus_per_node: Option<u32>,
    exclusive: bool,
    created_at: DateTime<Utc>,
}

impl LeaseRecord {
    fn summary(&self) -> LeaseSummary {
        LeaseSummary {
            lease_id: self.lease_id.clone(),
            nodes: self.nodes.clone(),
            gpus_per_node: self.gpus_per_node,
            exclusive: self.exclusive,
            created_at: self.created_at,
        }
    }
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
        let ws = accept_async(stream).await?;
        return Ok((ws, None));
    }

    let role_slot = Arc::new(StdMutex::new(None));
    let callback = AuthCallback {
        auth,
        role_slot: Arc::clone(&role_slot),
    };
    let ws = accept_hdr_async(stream, callback).await?;
    let role = *role_slot.lock().expect("auth role mutex poisoned");
    Ok((ws, role))
}

struct AuthCallback {
    auth: AuthConfig,
    role_slot: Arc<StdMutex<Option<AuthRole>>>,
}

impl Callback for AuthCallback {
    #[allow(clippy::result_large_err)]
    fn on_request(
        self,
        request: &Request,
        response: Response,
    ) -> std::result::Result<Response, ErrorResponse> {
        let role = authenticate_request(request, &self.auth).map_err(unauthorized_response)?;
        *self.role_slot.lock().expect("auth role mutex poisoned") = Some(role);
        Ok(response)
    }
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
        ClientRequest::JobStatus(request) => job_status(request, state, sink).await,
        ClientRequest::CreateLease(request) => create_lease(request, state, sink).await,
        ClientRequest::ListLeases => list_leases(state, sink).await,
        ClientRequest::ReleaseLease(request) => release_lease(request, state, sink).await,
        ClientRequest::DiagnoseJob(request) => diagnose_job(request, state, sink).await,
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

async fn job_status(
    request: JobStatusRequest,
    state: AppState,
    sink: &mut SplitSink<WebSocketStream<TcpStream>, Message>,
) -> Result<()> {
    let Some(job) = state.store.get_job(&request.job_id)? else {
        return send_wire(
            sink,
            WireMessage::ClientResponse(ClientResponse::Error(format!(
                "job {} was not found",
                request.job_id
            ))),
        )
        .await;
    };

    send_wire(sink, WireMessage::ClientResponse(ClientResponse::Job(job))).await
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

async fn create_lease(
    request: CreateLeaseRequest,
    state: AppState,
    sink: &mut SplitSink<WebSocketStream<TcpStream>, Message>,
) -> Result<()> {
    let mut guard = state.inner.lock().await;
    let mut selected_nodes = request.nodes;

    if selected_nodes.is_empty() {
        let count = request.count.unwrap_or(guard.nodes.len()).max(1);
        selected_nodes = guard.nodes.keys().take(count).cloned().collect();
    }

    if selected_nodes.is_empty() {
        return send_wire(
            sink,
            WireMessage::ClientResponse(ClientResponse::Error(
                "no connected nodes are available for a lease".to_owned(),
            )),
        )
        .await;
    }

    for node_id in &selected_nodes {
        if !guard.nodes.contains_key(node_id) {
            return send_wire(
                sink,
                WireMessage::ClientResponse(ClientResponse::Error(format!(
                    "node {node_id} is not connected"
                ))),
            )
            .await;
        }
    }

    for lease in guard.leases.values() {
        let overlaps = selected_nodes
            .iter()
            .any(|node_id| lease.nodes.contains(node_id));
        if overlaps && (lease.exclusive || request.exclusive) {
            return send_wire(
                sink,
                WireMessage::ClientResponse(ClientResponse::Error(format!(
                    "requested nodes overlap with active lease {}",
                    lease.lease_id
                ))),
            )
            .await;
        }
    }

    let record = LeaseRecord {
        lease_id: LeaseId::new(),
        nodes: selected_nodes,
        gpus_per_node: request.gpus_per_node,
        exclusive: request.exclusive,
        created_at: Utc::now(),
    };
    let summary = record.summary();
    guard.leases.insert(record.lease_id.clone(), record);
    drop(guard);

    send_wire(
        sink,
        WireMessage::ClientResponse(ClientResponse::LeaseCreated(summary)),
    )
    .await
}

async fn list_leases(
    state: AppState,
    sink: &mut SplitSink<WebSocketStream<TcpStream>, Message>,
) -> Result<()> {
    let leases = state
        .inner
        .lock()
        .await
        .leases
        .values()
        .map(LeaseRecord::summary)
        .collect();
    send_wire(
        sink,
        WireMessage::ClientResponse(ClientResponse::Leases(leases)),
    )
    .await
}

async fn release_lease(
    request: ReleaseLeaseRequest,
    state: AppState,
    sink: &mut SplitSink<WebSocketStream<TcpStream>, Message>,
) -> Result<()> {
    let removed = state
        .inner
        .lock()
        .await
        .leases
        .remove(&request.lease_id)
        .is_some();

    if removed {
        send_wire(sink, WireMessage::ClientResponse(ClientResponse::Ack)).await
    } else {
        send_wire(
            sink,
            WireMessage::ClientResponse(ClientResponse::Error(format!(
                "lease {} was not found",
                request.lease_id
            ))),
        )
        .await
    }
}

async fn diagnose_job(
    request: DiagnoseJobRequest,
    state: AppState,
    sink: &mut SplitSink<WebSocketStream<TcpStream>, Message>,
) -> Result<()> {
    let Some(job) = state.store.get_job(&request.job_id)? else {
        return send_wire(
            sink,
            WireMessage::ClientResponse(ClientResponse::Error(format!(
                "job {} was not found",
                request.job_id
            ))),
        )
        .await;
    };
    let logs = state
        .store
        .tail_logs(&request.job_id, request.lines.max(1))?;
    let diagnosis = build_job_diagnosis(job, logs);
    send_wire(
        sink,
        WireMessage::ClientResponse(ClientResponse::JobDiagnosis(diagnosis)),
    )
    .await
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
        if let Some(lease_id) = &request.lease_id {
            let Some(lease) = guard.leases.get(lease_id) else {
                return send_wire(
                    sink,
                    WireMessage::ClientResponse(ClientResponse::Error(format!(
                        "lease {lease_id} was not found"
                    ))),
                )
                .await;
            };

            if selected_nodes.is_empty() {
                selected_nodes = lease.nodes.clone();
            } else if selected_nodes
                .iter()
                .any(|node_id| !lease.nodes.contains(node_id))
            {
                return send_wire(
                    sink,
                    WireMessage::ClientResponse(ClientResponse::Error(format!(
                        "run nodes must be a subset of lease {lease_id}"
                    ))),
                )
                .await;
            }
        }

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
            let mut env = request.env.clone();
            if let Some(node_env) = request.node_env.get(&node_id) {
                env.extend(node_env.clone());
            }

            let mut run = RunSpec::simple(
                job_id.clone(),
                task_id.clone(),
                node_id.clone(),
                request.argv.clone(),
                cwd.clone(),
                env,
            );
            run.timeout = request.timeout;
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

fn build_job_diagnosis(job: JobSummary, logs: Vec<LogLine>) -> JobDiagnosis {
    let mut hints = Vec::new();
    let mut recent_errors = Vec::new();
    let failed_tasks = job
        .tasks
        .iter()
        .filter(|task| task.status == "failed")
        .count();
    let running_tasks = job
        .tasks
        .iter()
        .filter(|task| matches!(task.status.as_str(), "queued" | "running"))
        .count();

    for line in logs.iter().rev() {
        let lower = line.line.to_ascii_lowercase();
        let is_error_stream = matches!(line.stream, LogStream::Stderr | LogStream::System);
        let is_error_text = lower.contains("error")
            || lower.contains("exception")
            || lower.contains("traceback")
            || lower.contains("failed")
            || lower.contains("killed")
            || lower.contains("out of memory")
            || lower.contains("no space left")
            || lower.contains("permission denied")
            || lower.contains("module not found")
            || lower.contains("command not found")
            || lower.contains("nccl")
            || lower.contains("cuda");
        if is_error_stream || is_error_text {
            recent_errors.push(line.clone());
        }
        if recent_errors.len() >= 20 {
            break;
        }
    }
    recent_errors.reverse();

    let all_text = logs
        .iter()
        .map(|line| line.line.to_ascii_lowercase())
        .collect::<Vec<_>>()
        .join("\n");

    if all_text.contains("out of memory") || all_text.contains("cuda out of memory") {
        hints.push(
            "CUDA OOM detected; reduce batch size, sequence length, or per-process GPU memory use."
                .to_owned(),
        );
    }
    if all_text.contains("nccl") {
        hints.push(
            "NCCL output detected; check MASTER_ADDR/MASTER_PORT, node reachability, IB/NVLink setup, and matching world size."
                .to_owned(),
        );
    }
    if all_text.contains("no space left") || all_text.contains("disk quota") {
        hints.push(
            "Disk capacity or quota issue detected; check dataset/cache/output paths.".to_owned(),
        );
    }
    if all_text.contains("permission denied") {
        hints.push(
            "Permission issue detected; check file ownership, execute bits, and policy rules."
                .to_owned(),
        );
    }
    if all_text.contains("module not found") || all_text.contains("no module named") {
        hints.push(
            "Python dependency issue detected; verify the active environment on every node."
                .to_owned(),
        );
    }
    if all_text.contains("command not found") || all_text.contains(": not found") {
        hints.push(
            "Command lookup failed; verify PATH and installed binaries on every node.".to_owned(),
        );
    }
    if all_text.contains("task timed out after") {
        hints.push("Runtime timeout killed the job; increase --timeout-seconds or inspect why the task exceeded its limit.".to_owned());
    } else if all_text.contains("connection refused") || all_text.contains("timed out") {
        hints.push("Network connection issue detected; verify ports, hostnames, firewall rules, and rendezvous address.".to_owned());
    }
    if hints.is_empty() && failed_tasks > 0 {
        hints.push(
            "One or more tasks failed; inspect recent stderr and worker logs for the root cause."
                .to_owned(),
        );
    }
    if hints.is_empty() && running_tasks > 0 {
        hints.push(
            "Job is still running or queued; tail logs and check node liveness before intervening."
                .to_owned(),
        );
    }
    if hints.is_empty() {
        hints.push("No obvious failure pattern found in recent logs.".to_owned());
    }

    let summary = if job.status == "succeeded" {
        format!(
            "job {} succeeded across {} task(s)",
            job.job_id,
            job.tasks.len()
        )
    } else if running_tasks > 0 {
        format!(
            "job {} is {} with {} running/queued task(s)",
            job.job_id, job.status, running_tasks
        )
    } else if failed_tasks > 0 {
        format!(
            "job {} failed with {} failed task(s) out of {}",
            job.job_id,
            failed_tasks,
            job.tasks.len()
        )
    } else {
        format!("job {} status is {}", job.job_id, job.status)
    };

    JobDiagnosis {
        job_id: job.job_id,
        status: job.status,
        summary,
        hints,
        recent_errors,
        tasks: job.tasks,
    }
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
