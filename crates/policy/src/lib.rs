use std::path::{Path, PathBuf};

use agent_protocol::{DecisionKind, OperationRequest, PolicyDecision, RiskLevel, RunSpec};
use anyhow::{Context, Result};
use serde::Deserialize;

pub trait PolicyEngine: Send + Sync {
    fn authorize(&self, operation: &OperationRequest, spec: &RunSpec) -> PolicyDecision;
}

#[derive(Clone, Debug, Default)]
pub struct AllowAllPolicy;

impl PolicyEngine for AllowAllPolicy {
    fn authorize(&self, _operation: &OperationRequest, _spec: &RunSpec) -> PolicyDecision {
        PolicyDecision::allow_all()
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct PolicyConfig {
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub mode: PolicyMode,
    #[serde(default)]
    pub allow_commands: Vec<String>,
    #[serde(default)]
    pub deny_commands: Vec<String>,
    #[serde(default)]
    pub sandbox: Option<SandboxPolicyConfig>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyMode {
    #[default]
    AllowAll,
    DenyAll,
}

#[derive(Clone, Debug, Deserialize)]
pub struct SandboxPolicyConfig {
    pub profile: Option<String>,
    pub backend: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct ConfigPolicy {
    config: PolicyConfig,
    source: Option<PathBuf>,
}

impl Default for PolicyConfig {
    fn default() -> Self {
        Self {
            version: 1,
            mode: PolicyMode::AllowAll,
            allow_commands: Vec::new(),
            deny_commands: Vec::new(),
            sandbox: Some(SandboxPolicyConfig {
                profile: Some("none".to_owned()),
                backend: Some("none".to_owned()),
            }),
        }
    }
}

impl ConfigPolicy {
    pub fn from_path(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let input = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read policy {}", path.display()))?;
        let config = serde_yaml::from_str(&input)
            .with_context(|| format!("failed to parse policy {}", path.display()))?;
        Ok(Self {
            config,
            source: Some(path.to_owned()),
        })
    }

    fn allow_decision(&self) -> PolicyDecision {
        let mut decision = PolicyDecision::allow_all();
        decision.reason = self
            .source
            .as_ref()
            .map(|source| format!("allowed by policy {}", source.display()))
            .unwrap_or_else(|| "allow_all mode".to_owned());
        decision.sandbox_profile = self
            .config
            .sandbox
            .as_ref()
            .and_then(|sandbox| sandbox.profile.clone())
            .or_else(|| Some("none".to_owned()));
        decision
    }

    fn deny_decision(&self, reason: String) -> PolicyDecision {
        PolicyDecision {
            decision: DecisionKind::Deny,
            reason,
            risk: RiskLevel::High,
            sandbox_profile: None,
            requires_human_approval: false,
            warnings: Vec::new(),
        }
    }
}

impl PolicyEngine for ConfigPolicy {
    fn authorize(&self, _operation: &OperationRequest, spec: &RunSpec) -> PolicyDecision {
        let command = spec.argv.first().map(String::as_str).unwrap_or_default();

        if command_matches_any(command, &self.config.deny_commands) {
            return self.deny_decision(format!("command {command:?} denied by worker policy"));
        }

        match self.config.mode {
            PolicyMode::AllowAll => self.allow_decision(),
            PolicyMode::DenyAll => {
                if command_matches_any(command, &self.config.allow_commands) {
                    self.allow_decision()
                } else {
                    self.deny_decision(format!(
                        "command {command:?} is not allowed by worker policy"
                    ))
                }
            }
        }
    }
}

fn command_matches_any(command: &str, patterns: &[String]) -> bool {
    patterns
        .iter()
        .any(|pattern| command_matches(command, pattern))
}

fn command_matches(command: &str, pattern: &str) -> bool {
    if command == pattern {
        return true;
    }
    Path::new(command)
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name == pattern)
}
