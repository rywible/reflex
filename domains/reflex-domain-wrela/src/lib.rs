//! Wrela external domain — kernel packages, certificate verification, and catalog campaigns.
//!
//! Package **identity** is content-addressed: any semantic, range, cost, or verifier change
//! changes the package **digest** (`KernelPackage::identity`).

mod adapter;
mod catalog;
mod certificate;
mod commands;
mod domain;
mod interval;
mod kernel_package;
mod split;
mod verify;

pub use adapter::{
    WrelaAdapterState, WrelaReplayState, adapter_handshake, build_handshake_request,
    validate_protocol_before_search,
};
pub use catalog::{
    ApplicationTelemetry, CatalogEdition, CatalogEntry, CatalogLockfile, apply_edition_entry,
};
pub use certificate::{CertificateStrategy, evaluate_kernel_bounds};
pub use commands::{
    CommandReceipt, ConformanceReport, CostReport, reflex_check_candidate,
    reflex_check_candidate_batch, reflex_conformance, reflex_cost_candidate, reflex_export,
};
pub use domain::{
    WrelaArtifact, WrelaDomain, WrelaReceiptClass, WrelaTask, WrelaVerificationReceipt,
};
pub use interval::{Interval, eval_polynomial, true_range_on_domain};
pub use kernel_package::{
    KernelPackage, PACKAGE_VERSION, PackageDiagnostic, UnsupportedConstruct, ValidateOutcome,
    export_package_json, import_package_json, validate_package,
};
pub use split::{SplitGroup, is_held_out, split_group_for_kernel};
pub use verify::{
    CertificatePayload, VerifiedCertificate, verify_artifact_fields, verify_certificate,
};
