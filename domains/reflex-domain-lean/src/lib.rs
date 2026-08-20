mod dag;
mod datasets;
mod domain;
mod kernel;
mod m1_5;
mod m2a;
mod m2b;
mod protocol;

pub use dag::{
    ProofDag, ProofDagEdge, coverage_report, mine_subset_dag, register_subset_before_search,
    validate_imported_dag,
};
pub use datasets::{DatasetVariant, m2b_dataset_variants};
pub use domain::{LeanDomain, LeanGoalState, LeanKernelReceipt, LeanProofArtifact, LeanTask};
pub use kernel::{
    KernelReceipt, KernelSandboxConfig, LeanAvailability, is_lean_available, probe_lean,
    probe_lean_sandboxed, verify_with_kernel, verify_with_kernel_sandboxed,
};
pub use m1_5::{M15Manifest, M15ModelRecord, load_m15_manifest};
pub use m2a::{
    M2aArmRecord, M2aBundle, M2aImportError, M2aIncident, M2aManifest, M2aReconstructionResult,
    default_evidence_dir as m2a_evidence_dir, load_manifest, reconstruct_m2a_from_evidence,
    reconstruct_m2a_result,
};
pub use m2b::{
    M2bCell, M2bMatrix, default_evidence_dir as m2b_evidence_dir, load_m2b_matrix, reconcile_matrix,
};
pub use protocol::{LeanProtocolBridge, lean_domain_digest};
