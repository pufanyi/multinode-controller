use std::{collections::BTreeMap, fmt, path::PathBuf, time::Duration};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
pub struct JobId(pub String);

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
pub struct TaskId(pub String);

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
pub struct NodeId(pub String);

macro_rules! id_impl {
    ($ty:ty, $prefix:literal) => {
        #[allow(clippy::new_without_default)]
        impl $ty {
            pub fn new() -> Self {
                Self(format!("{}_{}", $prefix, Uuid::new_v4().simple()))
            }
        }

        impl From<String> for $ty {
            fn from(value: String) -> Self {
                Self(value)
            }
        }

        impl From<&str> for $ty {
            fn from(value: &str) -> Self {
                Self(value.to_owned())
            }
        }

        impl fmt::Display for $ty {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

id_impl!(JobId, "job");
id_impl!(TaskId, "task");
id_impl!(NodeId, "node");

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RunSpec {
    pub job_id: JobId,
    pub task_id: TaskId,
    pub node_id: NodeId,
    pub argv: Vec<String>,
    pub cwd: PathBuf,
    pub env: BTreeMap<String, String>,
    pub timeout: Option<DurationSpec>,
    pub resources: ResourceSpec,
    pub filesystem: FilesystemSpec,
    pub network: NetworkSpec,
    pub devices: DeviceSpec,
    pub sandbox: SandboxRequest,
    pub secrets: Vec<SecretRef>,
    pub log_policy: LogPolicy,
}

impl RunSpec {
    pub fn simple(
        job_id: JobId,
        task_id: TaskId,
        node_id: NodeId,
        argv: Vec<String>,
        cwd: PathBuf,
        env: BTreeMap<String, String>,
    ) -> Self {
        Self {
            job_id,
            task_id,
            node_id,
            argv,
            cwd,
            env,
            timeout: None,
            resources: ResourceSpec::default(),
            filesystem: FilesystemSpec::default(),
            network: NetworkSpec::default(),
            devices: DeviceSpec::default(),
            sandbox: SandboxRequest::none(),
            secrets: Vec::new(),
            log_policy: LogPolicy::default(),
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct DurationSpec {
    pub seconds: u64,
}

impl From<Duration> for DurationSpec {
    fn from(value: Duration) -> Self {
        Self {
            seconds: value.as_secs(),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ResourceSpec {
    pub cpu_cores: Option<f32>,
    pub memory_mb: Option<u64>,
    pub gpu_count: Option<u32>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct FilesystemSpec {
    pub workspace: Option<PathBuf>,
    pub read_paths: Vec<PathBuf>,
    pub write_paths: Vec<PathBuf>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkMode {
    Allow,
    Deny,
    AllowList,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NetworkSpec {
    pub mode: NetworkMode,
    pub allow: Vec<String>,
}

impl Default for NetworkSpec {
    fn default() -> Self {
        Self {
            mode: NetworkMode::Allow,
            allow: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DeviceSpec {
    pub gpu: bool,
    pub infiniband: bool,
}

impl Default for DeviceSpec {
    fn default() -> Self {
        Self {
            gpu: true,
            infiniband: true,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SandboxRequest {
    pub profile: String,
}

impl SandboxRequest {
    pub fn none() -> Self {
        Self {
            profile: "none".to_owned(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SecretRef {
    pub name: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LogPolicy {
    pub redact_secrets: bool,
    pub max_line_bytes: usize,
}

impl Default for LogPolicy {
    fn default() -> Self {
        Self {
            redact_secrets: true,
            max_line_bytes: 16 * 1024,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionKind {
    Allow,
    Ask,
    Deny,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskLevel {
    Low,
    Medium,
    High,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PolicyDecision {
    pub decision: DecisionKind,
    pub reason: String,
    pub risk: RiskLevel,
    pub sandbox_profile: Option<String>,
    pub requires_human_approval: bool,
    pub warnings: Vec<String>,
}

impl PolicyDecision {
    pub fn allow_all() -> Self {
        Self {
            decision: DecisionKind::Allow,
            reason: "allow_all mode".to_owned(),
            risk: RiskLevel::Medium,
            sandbox_profile: Some("none".to_owned()),
            requires_human_approval: false,
            warnings: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OperationRequest {
    pub subject: Subject,
    pub action: String,
    pub resource: Resource,
    pub scope: Scope,
    pub context: OperationContext,
}

impl OperationRequest {
    pub fn process_start(spec: &RunSpec, requested_by: impl Into<String>) -> Self {
        Self {
            subject: Subject {
                subject_type: "agent".to_owned(),
                name: requested_by.into(),
                session_id: None,
            },
            action: "process.start".to_owned(),
            resource: Resource {
                node_id: spec.node_id.clone(),
                argv: spec.argv.clone(),
                cwd: spec.cwd.clone(),
            },
            scope: Scope {
                workspace: spec.filesystem.workspace.clone(),
            },
            context: OperationContext {
                requested_by: "worker".to_owned(),
                job_id: spec.job_id.clone(),
                task_id: spec.task_id.clone(),
            },
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Subject {
    #[serde(rename = "type")]
    pub subject_type: String,
    pub name: String,
    pub session_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Resource {
    pub node_id: NodeId,
    pub argv: Vec<String>,
    pub cwd: PathBuf,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Scope {
    pub workspace: Option<PathBuf>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OperationContext {
    pub requested_by: String,
    pub job_id: JobId,
    pub task_id: TaskId,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NodeInfo {
    pub node_id: NodeId,
    pub node_name: String,
    pub hostname: String,
    pub os: String,
    pub arch: String,
    pub labels: Vec<String>,
    pub started_at: DateTime<Utc>,
}

impl NodeInfo {
    pub fn new(node_name: String, hostname: String) -> Self {
        Self {
            node_id: NodeId(node_name.clone()),
            node_name,
            hostname,
            os: std::env::consts::OS.to_owned(),
            arch: std::env::consts::ARCH.to_owned(),
            labels: Vec::new(),
            started_at: Utc::now(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NodeHeartbeat {
    pub node_id: NodeId,
    pub timestamp: DateTime<Utc>,
    pub running_tasks: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NodeSummary {
    pub node_id: NodeId,
    pub node_name: String,
    pub hostname: String,
    pub online: bool,
    pub last_seen: DateTime<Utc>,
    pub running_tasks: usize,
    pub labels: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TaskSpec {
    pub run: RunSpec,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TaskAssignment {
    pub job_id: JobId,
    pub task_id: TaskId,
    pub node_id: NodeId,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TaskStarted {
    pub job_id: JobId,
    pub task_id: TaskId,
    pub node_id: NodeId,
    pub pid: Option<u32>,
    pub timestamp: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogStream {
    Stdout,
    Stderr,
    System,
}

impl fmt::Display for LogStream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Stdout => f.write_str("stdout"),
            Self::Stderr => f.write_str("stderr"),
            Self::System => f.write_str("system"),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LogLine {
    pub job_id: JobId,
    pub task_id: TaskId,
    pub node_id: NodeId,
    pub stream: LogStream,
    pub timestamp: DateTime<Utc>,
    pub line: String,
    pub offset: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TaskExited {
    pub job_id: JobId,
    pub task_id: TaskId,
    pub node_id: NodeId,
    pub exit_code: Option<i32>,
    pub success: bool,
    pub timestamp: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkerError {
    pub node_id: Option<NodeId>,
    pub task_id: Option<TaskId>,
    pub message: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum WorkerMessage {
    Register(NodeInfo),
    Heartbeat(NodeHeartbeat),
    TaskStarted(TaskStarted),
    LogLine(LogLine),
    TaskExited(TaskExited),
    Error(WorkerError),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
#[allow(clippy::large_enum_variant)]
pub enum CoordinatorMessage {
    StartTask(TaskSpec),
    KillTask(TaskId),
    Ping,
    UpdatePolicy(serde_json::Value),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RunProcessRequest {
    pub nodes: Vec<NodeId>,
    pub argv: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub env: BTreeMap<String, String>,
    pub wait: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RunStarted {
    pub job_id: JobId,
    pub tasks: Vec<TaskAssignment>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JobFinished {
    pub job_id: JobId,
    pub success: bool,
    pub exits: Vec<TaskExited>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum ClientRequest {
    ListNodes,
    RunProcess(RunProcessRequest),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum ClientResponse {
    Nodes(Vec<NodeSummary>),
    RunStarted(RunStarted),
    TaskStarted(TaskStarted),
    LogLine(LogLine),
    TaskExited(TaskExited),
    JobFinished(JobFinished),
    Error(String),
    Ack,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
#[allow(clippy::large_enum_variant)]
pub enum WireMessage {
    Worker(WorkerMessage),
    Coordinator(CoordinatorMessage),
    ClientRequest(ClientRequest),
    ClientResponse(ClientResponse),
}

impl WireMessage {
    pub fn to_text(&self) -> anyhow::Result<String> {
        Ok(serde_json::to_string(self)?)
    }

    pub fn from_text(input: &str) -> anyhow::Result<Self> {
        Ok(serde_json::from_str(input)?)
    }
}
