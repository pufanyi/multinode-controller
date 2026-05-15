use std::{collections::HashMap, process::Stdio, sync::Arc};

use agent_protocol::{LogLine, LogStream, TaskExited, TaskId, TaskStarted};
use agent_sandbox_linux::SandboxCommand;
use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use chrono::Utc;
#[cfg(unix)]
use nix::{
    sys::signal::{killpg, Signal},
    unistd::Pid,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, BufReader},
    process::{Child, Command},
    sync::{mpsc, Mutex},
    time::{sleep, Duration},
};

#[derive(Clone, Debug)]
pub enum ExecutionEvent {
    Started(TaskStarted),
    Log(LogLine),
    Exited(TaskExited),
}

#[derive(Clone, Debug)]
pub struct ExecutionRequest {
    pub spec: agent_protocol::RunSpec,
    pub command: SandboxCommand,
}

#[derive(Clone, Debug)]
pub struct TaskHandle {
    pub task_id: TaskId,
    pub pid: Option<u32>,
}

#[async_trait]
pub trait Executor: Send + Sync {
    async fn start(
        &self,
        request: ExecutionRequest,
        events: mpsc::Sender<ExecutionEvent>,
    ) -> Result<TaskHandle>;

    async fn kill(&self, task_id: &TaskId) -> Result<()>;
}

#[derive(Clone, Debug, Default)]
pub struct LocalExecutor {
    processes: Arc<Mutex<HashMap<TaskId, u32>>>,
}

impl LocalExecutor {
    pub async fn running_count(&self) -> usize {
        self.processes.lock().await.len()
    }
}

#[async_trait]
impl Executor for LocalExecutor {
    async fn start(
        &self,
        request: ExecutionRequest,
        events: mpsc::Sender<ExecutionEvent>,
    ) -> Result<TaskHandle> {
        let spec = request.spec;
        let command = request.command;
        let mut child_command = Command::new(&command.program);
        child_command
            .args(&command.args)
            .current_dir(&command.cwd)
            .envs(&command.env)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        #[cfg(unix)]
        if command.kill_process_group {
            child_command.process_group(0);
        }

        let mut child = child_command
            .spawn()
            .with_context(|| format!("failed to spawn {:?}", command.program))?;
        let pid = child.id();

        if let Some(pid) = pid {
            self.processes
                .lock()
                .await
                .insert(spec.task_id.clone(), pid);
        }

        let started = TaskStarted {
            job_id: spec.job_id.clone(),
            task_id: spec.task_id.clone(),
            node_id: spec.node_id.clone(),
            pid,
            timestamp: Utc::now(),
        };
        let _ = events.send(ExecutionEvent::Started(started)).await;

        if let Some(stdout) = child.stdout.take() {
            spawn_log_reader(stdout, LogStream::Stdout, spec.clone(), events.clone());
        }
        if let Some(stderr) = child.stderr.take() {
            spawn_log_reader(stderr, LogStream::Stderr, spec.clone(), events.clone());
        }

        let processes = self.processes.clone();
        let task_id = spec.task_id.clone();
        let job_id = spec.job_id.clone();
        let node_id = spec.node_id.clone();
        let timeout = spec.timeout;
        let wait_events = events.clone();
        tokio::spawn(async move {
            let (exit_code, success) = wait_with_optional_timeout(
                &mut child,
                pid,
                timeout.map(|value| value.seconds),
                &job_id,
                &task_id,
                &node_id,
                &wait_events,
            )
            .await;
            processes.lock().await.remove(&task_id);
            let _ = wait_events
                .send(ExecutionEvent::Exited(TaskExited {
                    job_id,
                    task_id,
                    node_id,
                    exit_code,
                    success,
                    timestamp: Utc::now(),
                }))
                .await;
        });

        Ok(TaskHandle {
            task_id: spec.task_id,
            pid,
        })
    }

    async fn kill(&self, task_id: &TaskId) -> Result<()> {
        let Some(pid) = self.processes.lock().await.get(task_id).copied() else {
            return Err(anyhow!("task {task_id} is not running"));
        };

        #[cfg(unix)]
        {
            let process_group = Pid::from_raw(pid as i32);
            killpg(process_group, Signal::SIGTERM).context("failed to kill process group")?;
            Ok(())
        }

        #[cfg(not(unix))]
        {
            let _ = pid;
            Err(anyhow!("process group kill is only implemented on Unix"))
        }
    }
}

async fn wait_with_optional_timeout(
    child: &mut Child,
    pid: Option<u32>,
    timeout_seconds: Option<u64>,
    job_id: &agent_protocol::JobId,
    task_id: &TaskId,
    node_id: &agent_protocol::NodeId,
    events: &mpsc::Sender<ExecutionEvent>,
) -> (Option<i32>, bool) {
    let Some(seconds) = timeout_seconds.filter(|seconds| *seconds > 0) else {
        return exit_result(child.wait().await);
    };

    tokio::select! {
        exit = child.wait() => exit_result(exit),
        _ = sleep(Duration::from_secs(seconds)) => {
            let _ = events
                .send(ExecutionEvent::Log(LogLine {
                    job_id: job_id.clone(),
                    task_id: task_id.clone(),
                    node_id: node_id.clone(),
                    stream: LogStream::System,
                    timestamp: Utc::now(),
                    line: format!("task timed out after {seconds}s; terminating process group"),
                    offset: 0,
                }))
                .await;
            terminate_child(child, pid).await;
            (None, false)
        }
    }
}

fn exit_result(exit: std::io::Result<std::process::ExitStatus>) -> (Option<i32>, bool) {
    match exit {
        Ok(status) => (status.code(), status.success()),
        Err(_) => (None, false),
    }
}

async fn terminate_child(child: &mut Child, pid: Option<u32>) {
    #[cfg(unix)]
    if let Some(pid) = pid {
        let process_group = Pid::from_raw(pid as i32);
        let _ = killpg(process_group, Signal::SIGTERM);
        sleep(Duration::from_secs(5)).await;
        if matches!(child.try_wait(), Ok(None)) {
            let _ = killpg(process_group, Signal::SIGKILL);
        }
        let _ = child.wait().await;
        return;
    }

    let _ = child.start_kill();
    let _ = child.wait().await;
}

fn spawn_log_reader<R>(
    reader: R,
    stream: LogStream,
    spec: agent_protocol::RunSpec,
    events: mpsc::Sender<ExecutionEvent>,
) where
    R: AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut lines = BufReader::new(reader).lines();
        let mut offset = 0_u64;

        while let Ok(Some(line)) = lines.next_line().await {
            let line_len = line.len() as u64;
            let event = LogLine {
                job_id: spec.job_id.clone(),
                task_id: spec.task_id.clone(),
                node_id: spec.node_id.clone(),
                stream: stream.clone(),
                timestamp: Utc::now(),
                line,
                offset,
            };
            offset += line_len + 1;
            if events.send(ExecutionEvent::Log(event)).await.is_err() {
                break;
            }
        }
    });
}
