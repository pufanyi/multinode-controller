use std::{collections::HashMap, process::Stdio, sync::Arc};

use agent_protocol::{LogLine, LogStream, TaskExited, TaskId, TaskStarted};
use agent_sandbox_linux::SandboxCommand;
use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use chrono::Utc;
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, BufReader},
    process::Command,
    sync::{mpsc, Mutex},
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
            unsafe {
                child_command.pre_exec(|| {
                    if libc::setsid() == -1 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
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
        let wait_events = events.clone();
        tokio::spawn(async move {
            let exit = child.wait().await;
            processes.lock().await.remove(&task_id);
            let (exit_code, success) = match exit {
                Ok(status) => (status.code(), status.success()),
                Err(_) => (None, false),
            };
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
            let process_group = -(pid as i32);
            let rc = unsafe { libc::kill(process_group, libc::SIGTERM) };
            if rc == -1 {
                return Err(std::io::Error::last_os_error())
                    .context("failed to kill process group");
            }
            Ok(())
        }

        #[cfg(not(unix))]
        {
            let _ = pid;
            Err(anyhow!("process group kill is only implemented on Unix"))
        }
    }
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
