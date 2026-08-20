//! Verification receipt types (serde-backed, not on the native hot path).

use reflex_types::{ArtifactId, AssumptionId, Digest, UnresolvedCode, VerifierId};
use serde::{Deserialize, Serialize};

/// Declared limits for one verification call (§10.1, §10.5).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct VerifyBudget {
    pub max_cpu_ns: u64,
    pub max_wall_ns: u64,
    pub max_memory_bytes: u64,
}

/// Command that deterministically re-runs a verification from CAS inputs
/// (§10.5, P5.5).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplayCommand {
    pub command: String,
    pub args: Vec<String>,
}

/// Infrastructure failure classes of a verification attempt (P5.5 step 4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum VerificationFailure {
    MalformedReceipt,
    CompatibilityMismatch,
    Internal,
}

/// Verdict of a verification attempt (§10.5).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum VerificationStatus {
    Accepted,
    Rejected,
    Indeterminate(UnresolvedCode),
    Error(VerificationFailure),
}

impl VerificationStatus {
    pub fn is_accepted(self) -> bool {
        matches!(self, VerificationStatus::Accepted)
    }

    pub fn is_censored(self) -> bool {
        matches!(
            self,
            VerificationStatus::Indeterminate(_) | VerificationStatus::Error(_)
        )
    }
}

/// Authority trace of one verification (§10.5).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VerificationReceipt {
    pub verifier: VerifierId,
    pub implementation: Digest,
    pub semantic_anchor: Digest,
    pub artifact: ArtifactId,
    pub status: VerificationStatus,
    pub proof_or_certificate: Option<Digest>,
    pub assumptions: Vec<AssumptionId>,
    pub cpu_ns: u64,
    pub wall_ns: u64,
    pub peak_rss_bytes: u64,
    pub replay_command: ReplayCommand,
}

impl VerificationReceipt {
    pub fn is_well_formed(&self) -> bool {
        if self.implementation == Digest::ZERO {
            return false;
        }
        if self.artifact == ArtifactId::from_digest(Digest::ZERO) {
            return false;
        }
        match self.status {
            VerificationStatus::Accepted => !self.replay_command.command.is_empty(),
            VerificationStatus::Rejected
            | VerificationStatus::Indeterminate(_)
            | VerificationStatus::Error(_) => true,
        }
    }
}
