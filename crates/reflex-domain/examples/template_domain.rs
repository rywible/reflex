//! Template domain: the minimal complete implementation of the `Domain`
//! trait, using only the public reflex-domain API.
//!
//! Run it:
//!
//! ```text
//! cargo run -p reflex-domain --example template_domain
//! ```
//!
//! The domain rewrites a small integer program:
//!
//! - a `Task` describes a start expression and a target value;
//! - `State`s are the current expression;
//! - each `Candidate` is one rewrite step;
//! - `apply_candidates` lowers the expression towards the target;
//! - `reconstruct_artifact` returns the rewritten program;
//! - `verify` checks the artifact's semantics against the task by
//!   interpretation;
//! - `evaluate_utility` reports the rewrite as raw utility.
//!
//! Every method is deterministic and arena-stamped; the example also runs
//! itself through the [`ConformanceKit`] at the end, which is what a real
//! domain should do in its test suite.

use reflex_canonical::{CanonicalEncode, CanonicalError, CanonicalWriter};
use reflex_domain::conformance::{ConformanceKit, ConformanceReport, FixtureTask};
use reflex_domain::{
    CandidateBatch, CandidateBatchBuilder, CandidateIndex, Domain, DomainCapabilities, DomainError,
    EpisodeArena, FeatureBatch, SolvedRoot, StateHandle, TransitionBatch, TransitionOutcome,
    UtilityContext, UtilityObservation, VerifyBudget, VerifyError,
};
use reflex_economics::{BetterDirection, ConfidenceClass};
use reflex_types::{
    ActionSchemaId, AndGroupRef, ArtifactId, CandidateId, Digest, DomainWitnessRef,
    FeatureSchemaId, StateId, TaskId,
};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// One "rewrite one +1 into a constant fold" task.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TemplateTask {
    pub start: u32,
    pub target: u32,
}

impl CanonicalEncode for TemplateTask {
    fn encode_canonical(&self, out: &mut CanonicalWriter) -> Result<(), CanonicalError> {
        out.write_u32(self.start)?;
        out.write_u32(self.target)?;
        Ok(())
    }
}

/// Search state: the current expression value.
#[derive(Clone, Debug)]
pub struct TemplateState {
    pub value: u32,
}

impl CanonicalEncode for TemplateState {
    fn encode_canonical(&self, out: &mut CanonicalWriter) -> Result<(), CanonicalError> {
        out.write_u32(self.value)?;
        Ok(())
    }
}

/// One rewrite step: add `delta`.
#[derive(Clone, Debug)]
pub struct TemplateCandidate {
    pub delta: u32,
}

impl CanonicalEncode for TemplateCandidate {
    fn encode_canonical(&self, out: &mut CanonicalWriter) -> Result<(), CanonicalError> {
        out.write_u32(self.delta)?;
        Ok(())
    }
}

/// The artifact: a rewritten expression claiming to reach the target.
#[derive(Clone, Debug)]
pub struct TemplateArtifact {
    pub value: u32,
    pub id: ArtifactId,
}

impl CanonicalEncode for TemplateArtifact {
    fn encode_canonical(&self, out: &mut CanonicalWriter) -> Result<(), CanonicalError> {
        out.write_u32(self.value)?;
        out.write_digest(&self.id.0)?;
        Ok(())
    }
}

/// The verification: does the artifact actually equal the target?
#[derive(Clone, Debug)]
pub struct TemplateVerification {
    pub accepted: bool,
    pub artifact: ArtifactId,
}

impl CanonicalEncode for TemplateVerification {
    fn encode_canonical(&self, out: &mut CanonicalWriter) -> Result<(), CanonicalError> {
        out.write_bool(self.accepted)?;
        out.write_digest(&self.artifact.0)?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Canonical identity helpers
// ---------------------------------------------------------------------------

fn task_id_of(task: &TemplateTask) -> Result<TaskId, DomainError> {
    reflex_canonical::content_id(b"template:task", task)
        .map(TaskId::from_digest)
        .map_err(|error| {
            DomainError::InvalidTask(format!("canonical task identity failed: {error}"))
        })
}

fn state_id_of(value: u32, target: u32) -> Result<StateId, DomainError> {
    reflex_canonical::content_id(b"template:state", &TemplateStateIdParts { value, target })
        .map(StateId::from_digest)
        .map_err(|error| {
            DomainError::InvalidTask(format!("canonical state identity failed: {error}"))
        })
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TemplateStateIdParts {
    value: u32,
    target: u32,
}

impl CanonicalEncode for TemplateStateIdParts {
    fn encode_canonical(&self, out: &mut CanonicalWriter) -> Result<(), CanonicalError> {
        out.write_u32(self.value)?;
        out.write_u32(self.target)?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// The domain
// ---------------------------------------------------------------------------

/// The template domain. Replace the bodies with your world's semantics.
pub struct TemplateDomain {
    capabilities: DomainCapabilities,
    target: u32,
}

impl TemplateDomain {
    pub fn new(target: u32) -> Self {
        Self {
            capabilities: DomainCapabilities {
                domain_id: "template".to_string(),
                domain_digest: Digest::from_blake3_bytes([0x10; 32]),
                action_schema: ActionSchemaId::from_digest(Digest::from_blake3_bytes([0x20; 32])),
                feature_schema: FeatureSchemaId::from_digest(Digest::from_blake3_bytes([0x30; 32])),
                feature_dimension: 1,
                max_candidates_per_state: 8,
                deterministic_generation: true,
                supports_exact_cache: true,
            },
            target,
        }
    }
}

impl Domain for TemplateDomain {
    type Task = TemplateTask;
    type State = TemplateState;
    type Candidate = TemplateCandidate;
    type Transition = TemplateState;
    type Artifact = TemplateArtifact;
    type Verification = TemplateVerification;

    fn capabilities(&self) -> DomainCapabilities {
        self.capabilities.clone()
    }

    fn task_id(&self, task: &Self::Task) -> Result<TaskId, DomainError> {
        task_id_of(task)
    }

    fn initial_state(
        &self,
        task: &Self::Task,
        arena: &mut EpisodeArena,
    ) -> Result<StateHandle, DomainError> {
        Ok(arena.insert_state(
            TemplateState { value: task.start },
            state_id_of(task.start, task.target)?,
        ))
    }

    fn state_id(&self, state: StateHandle, arena: &EpisodeArena) -> Result<StateId, DomainError> {
        let state = arena
            .get_state::<TemplateState>(state)
            .ok_or(DomainError::InvalidStateHandle(state.index()))?;
        state_id_of(state.value, self.target)
    }

    fn enumerate_candidates(
        &self,
        state: StateHandle,
        arena: &EpisodeArena,
        output: &mut CandidateBatchBuilder,
    ) -> Result<(), DomainError> {
        let state = arena
            .get_state::<TemplateState>(state)
            .ok_or(DomainError::InvalidStateHandle(state.index()))?;
        let remaining = self.target.saturating_sub(state.value);
        let count = remaining
            .min(self.capabilities.max_candidates_per_state)
            .max(1) as usize;
        output.ids.clear();
        output.classes.clear();
        output.tie_breaks.clear();
        output.payload_handles.clear();
        output.flags.clear();
        for delta in 1..=count as u32 {
            let id = reflex_canonical::content_id(
                b"template:candidate",
                &TemplateCandidateIdParts {
                    state: state.value,
                    delta,
                },
            )
            .map(CandidateId::from_digest)
            .map_err(|error| {
                DomainError::Enumeration(format!("canonical candidate identity failed: {error}"))
            })?;
            // Candidate payloads live in the SoA lanes; `delta` travels in
            // the tie-break lane, the class lane carries the action class.
            output.add(
                id,
                0,
                u64::from(delta),
                reflex_domain::CandidateHandle(0),
                0,
            );
        }
        Ok(())
    }

    fn extract_features(
        &self,
        states: &[StateHandle],
        candidates: &CandidateBatch,
        arena: &EpisodeArena,
        output: &mut FeatureBatch,
    ) -> Result<(), DomainError> {
        for (row, handle) in states.iter().enumerate() {
            let state = arena
                .get_state::<TemplateState>(*handle)
                .ok_or(DomainError::InvalidStateHandle(handle.index()))?;
            for col in 0..(output.cols.min(candidates.len())) {
                // Guidance only: the model may score candidates freely, but
                // legality lives in apply_candidates (§11.3).
                let progress = candidates.tie_breaks[col] as f32 / self.target.max(1) as f32;
                let _ = state;
                output.row_mut(row)[col] = progress;
            }
        }
        Ok(())
    }

    fn apply_candidates(
        &self,
        state: StateHandle,
        candidates: &CandidateBatch,
        selection: &[CandidateIndex],
        arena: &mut EpisodeArena,
        output: &mut TransitionBatch,
    ) -> Result<(), DomainError> {
        let state = arena
            .get_state::<TemplateState>(state)
            .ok_or(DomainError::InvalidStateHandle(state.index()))?;
        let current = state.value;
        for index in selection {
            if index.0 >= candidates.len() {
                return Err(DomainError::InvalidCandidateIndex(index.0));
            }
            let delta = candidates.tie_breaks[index.0] as u32;
            let next = current.saturating_add(delta);
            if next >= self.target {
                // Closed: the artifact is the reached value; the witness
                // names it. Verification happens later, via Domain::verify.
                let artifact = ArtifactId::from_digest(Digest::from_blake3_bytes([0x40; 32]));
                output.add(TransitionOutcome::closed(artifact));
            } else {
                let child = arena.insert_state(
                    TemplateState { value: next },
                    state_id_of(next, self.target)?,
                );
                output.add(TransitionOutcome::obligations(
                    (u64::from(current) << 32) | u64::from(delta),
                    vec![child],
                ));
            }
        }
        Ok(())
    }

    fn reconstruct_artifact(
        &self,
        solved: SolvedRoot,
        arena: &EpisodeArena,
    ) -> Result<Self::Artifact, DomainError> {
        // The artifact is the value reached at the deepest point of the
        // solved route: the last AND child, or the root when the root
        // itself closed.
        let terminal = match solved.solved_edges.last() {
            Some((state, _, children)) => children.last().copied().unwrap_or(*state),
            None => solved.root_state,
        };
        let state = arena
            .get_state::<TemplateState>(terminal)
            .ok_or(DomainError::InvalidStateHandle(terminal.index()))?;
        Ok(TemplateArtifact {
            value: state.value,
            id: ArtifactId::from_digest(Digest::from_blake3_bytes([0x40; 32])),
        })
    }

    fn verify(
        &self,
        artifact: &Self::Artifact,
        _budget: VerifyBudget,
    ) -> Result<Self::Verification, VerifyError> {
        // Deterministic, budget-respecting verification: semantics must
        // agree with the target.
        Ok(TemplateVerification {
            accepted: artifact.value == self.target,
            artifact: artifact.id,
        })
    }

    fn evaluate_utility(
        &self,
        _artifact: &Self::Artifact,
        verification: &Self::Verification,
        context: &UtilityContext,
        output: &mut Vec<UtilityObservation>,
    ) -> Result<(), DomainError> {
        let _ = verification;
        context.validate()?;
        output.push(UtilityObservation {
            subject: context.subject,
            evaluator: reflex_types::EvaluatorId::from_digest(Digest::from_blake3_bytes(
                [0x51; 32],
            )),
            metric: reflex_types::MetricId::from_digest(Digest::from_blake3_bytes([0x52; 32])),
            value: reflex_economics::RationalOrFloat::Float(
                1.0 + f64::from(context.verified_actions_count),
            ),
            unit: reflex_types::UnitId::from_digest(Digest::from_blake3_bytes([0x53; 32])),
            direction: BetterDirection::HigherIsBetter,
            population: context.population,
            evidence: context.accepted_evidence.clone(),
            confidence: ConfidenceClass::ObservedExact,
            observed_at_generation: context.observed_at_generation,
            restricted_work: false,
        });
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TemplateCandidateIdParts {
    state: u32,
    delta: u32,
}

impl CanonicalEncode for TemplateCandidateIdParts {
    fn encode_canonical(&self, out: &mut CanonicalWriter) -> Result<(), CanonicalError> {
        out.write_u32(self.state)?;
        out.write_u32(self.delta)?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

fn main() {
    let task = TemplateTask {
        start: 1,
        target: 4,
    };
    let domain = TemplateDomain::new(task.target);

    // 1. A full manual search pass against the typed API.
    let mut arena = EpisodeArena::new();
    let root = domain
        .initial_state(&task, &mut arena)
        .expect("initial state");
    println!("task id: {}", domain.task_id(&task).unwrap());
    println!("root state id: {}", domain.state_id(root, &arena).unwrap());

    let mut builder = CandidateBatchBuilder::new();
    domain
        .enumerate_candidates(root, &arena, &mut builder)
        .expect("enumerate");
    let batch = builder.build();
    println!("enumerated {} candidates", batch.len());

    let mut features = FeatureBatch::new(batch.len(), 1, domain.capabilities().feature_schema);
    domain
        .extract_features(&[root], &batch, &arena, &mut features)
        .expect("features");

    // 2. Greedy walk: always take the largest step.
    let mut cursor = root;
    let mut transitions = TransitionBatch::new();
    let mut solved_edges = Vec::new();
    let mut steps = 0;
    while steps < 64 {
        let mut builder = CandidateBatchBuilder::new();
        domain
            .enumerate_candidates(cursor, &arena, &mut builder)
            .expect("enumerate");
        let batch = builder.build();
        let picked = batch.len().saturating_sub(1);
        let selection = vec![CandidateIndex(picked)];
        transitions.outcomes.clear();
        domain
            .apply_candidates(cursor, &batch, &selection, &mut arena, &mut transitions)
            .expect("apply");
        steps += 1;
        match &transitions.outcomes[0] {
            TransitionOutcome::Closed { witness } => {
                solved_edges.push((cursor, batch.payload_handles[picked], Vec::new()));
                println!(
                    "closed after {steps} steps (witness artifact {})",
                    witness.artifact
                );
                break;
            }
            TransitionOutcome::Obligations { group } => {
                solved_edges.push((
                    cursor,
                    batch.payload_handles[picked],
                    group.children.clone(),
                ));
                cursor = group.children[0];
            }
            other => {
                println!("stopped early: {other:?}");
                return;
            }
        }
    }

    // 3. Reconstruct + verify + utility.
    let solved = SolvedRoot {
        root_state: root,
        solved_edges,
    };
    let artifact = domain
        .reconstruct_artifact(solved, &arena)
        .expect("reconstruct");
    let budget = VerifyBudget {
        max_cpu_ns: 10_000_000,
        max_wall_ns: 10_000_000,
        max_memory_bytes: 1 << 20,
    };
    let verification = domain.verify(&artifact, budget).expect("verify");
    println!(
        "artifact value {} verified: {}",
        artifact.value, verification.accepted
    );
    assert!(verification.accepted, "template artifact must verify");

    let mut observations = Vec::new();
    domain
        .evaluate_utility(
            &artifact,
            &verification,
            &UtilityContext {
                subject: reflex_types::ResearchNodeId::from_digest(Digest::hash_blake3(
                    b"template-subject",
                )),
                population: Digest::hash_blake3(b"template-population"),
                observed_at_generation: reflex_types::GenerationId::from_digest(
                    Digest::hash_blake3(b"template-generation"),
                ),
                accepted_verification: Digest::hash_blake3(b"template-verification"),
                accepted_evidence: vec![Digest::hash_blake3(b"template-verification")],
                cell_cpu_ns: 1_000_000,
                model_inference_cpu_ns: 100_000,
                retrieval_cpu_ns: 50_000,
                verified_actions_count: 1,
            },
            &mut observations,
        )
        .expect("utility");
    println!("utility observations: {}", observations.len());

    // 4. Run the conformance kit (P5.6): this is what a real domain's test
    // suite should assert `all_passed()` on.
    let report: ConformanceReport = ConformanceKit::new().run(&domain, &task);
    for check in &report.checks {
        println!(
            "[{:>4}] {}: {}",
            if check.passed { "PASS" } else { "FAIL" },
            check.name,
            check.details
        );
    }
    assert!(report.all_passed(), "template domain must pass conformance");

    // 5. Erased facade: the same domain behind `ErasedDomain` handles
    // (this is the runtime-facing surface).
    let erased: std::sync::Arc<dyn reflex_domain::ErasedDomain> = std::sync::Arc::new(
        reflex_domain::DomainAdapter::new(std::sync::Arc::new(domain)),
    );
    let _ = erased; // (covered in depth by the domain SDK tests)
    println!("template domain passed the conformance kit; erased facade available");
}

// Keep the compiler honest that these helpers exist for porting:
#[allow(dead_code)]
fn _porting_notes() {
    let _ = AndGroupRef {
        group_id: 0,
        children: vec![],
    };
    let _ = DomainWitnessRef {
        artifact: ArtifactId::from_digest(Digest::ZERO),
        verification: None,
    };
    let _ = FixtureTask {
        start: 0,
        target: 1,
        candidates_per_state: 1,
        allow_duplicate_ids: false,
    };
}
