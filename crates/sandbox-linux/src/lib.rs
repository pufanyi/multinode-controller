use std::{collections::BTreeMap, path::PathBuf};

use agent_protocol::{PolicyDecision, RunSpec};
use anyhow::{bail, Result};
use async_trait::async_trait;

#[derive(Clone, Debug)]
pub struct SandboxCommand {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub env: BTreeMap<String, String>,
    pub kill_process_group: bool,
}

#[derive(Clone, Debug)]
pub struct SandboxHandle {
    pub task_id: agent_protocol::TaskId,
}

#[async_trait]
pub trait SandboxBackend: Send + Sync {
    async fn prepare(&self, spec: &RunSpec, decision: &PolicyDecision) -> Result<SandboxCommand>;

    async fn cleanup(&self, handle: &SandboxHandle) -> Result<()>;
}

#[derive(Clone, Debug, Default)]
pub struct NoneSandbox;

#[async_trait]
impl SandboxBackend for NoneSandbox {
    async fn prepare(&self, spec: &RunSpec, _decision: &PolicyDecision) -> Result<SandboxCommand> {
        let Some((program, args)) = spec.argv.split_first() else {
            bail!("run spec argv cannot be empty");
        };

        Ok(SandboxCommand {
            program: PathBuf::from(program),
            args: args.to_vec(),
            cwd: spec.cwd.clone(),
            env: spec.env.clone(),
            kill_process_group: true,
        })
    }

    async fn cleanup(&self, _handle: &SandboxHandle) -> Result<()> {
        Ok(())
    }
}
