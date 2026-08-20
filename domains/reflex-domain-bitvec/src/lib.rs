mod collection;
mod corpus;
mod domain;
mod expr;
mod features;
mod policy;
mod rewrites;
mod verify;

pub use collection::{
    BitvecVerificationReceipt, CollectionOutput, CollectionReport, LaneCollectionReport,
    check_train_eval_disjoint, run_evaluation_collection, run_first_collection,
    train_eval_task_ids,
};
pub use corpus::FROZEN_CORPUS;
pub use corpus::{BitvecTask, frozen_corpus, frozen_eval, frozen_train, generate_tasks};
pub use domain::{BitvecDomain, BitvecState};
pub use expr::{
    BvExpr, BvValidateError, MAX_EXPR_DEPTH, MAX_VAR_INDEX, ShiftKind, assignment_count,
    assignment_from_index,
};
pub use features::{BITVEC_FEATURE_DIM, extract_row, layout as feature_layout};
pub use policy::{
    BitvecOraclePolicy, cost_first_policy, simplification_first_policy, uniform_policy,
};
pub use rewrites::{BvCandidate, generate_rewrites, simplify_expr};
pub use verify::{BitvecVerification, verify_equivalent};

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BitvecArtifact {
    pub optimized: BvExpr,
    pub original: BvExpr,
}

impl reflex_canonical::CanonicalEncode for BitvecArtifact {
    fn encode_canonical(
        &self,
        out: &mut reflex_canonical::CanonicalWriter,
    ) -> Result<(), reflex_canonical::CanonicalError> {
        self.optimized.encode_canonical(out)?;
        self.original.encode_canonical(out)?;
        Ok(())
    }
}
