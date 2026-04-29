use agent_protocol::{OperationRequest, PolicyDecision, RunSpec};

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
