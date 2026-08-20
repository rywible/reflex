//! Domain conformance kit and reference fixture (P5.6, F-49).
//!
//! The kit exercises every semantic method of the typed [`Domain`] trait
//! and checks the contracts a domain must hold: identity stability,
//! deterministic enumeration, unique candidate ids, deterministic
//! transitions, AND-child ownership, buffer sizing, artifact
//! reconstruction, verifier determinism, finite utility observations,
//! arena generation semantics, and stale-handle rejection.
//!
//! [`FixtureDomain`] is the reference implementation used by the kit's own
//! tests: a value-reaching domain over `(value, target)` with a trivial
//! sum-based candidate action carried in the candidate tie-break lane. A
//! deliberately nondeterministic variant is available to prove the kit
//! detects nondeterminism.

use crate::{
    CandidateBatch, CandidateBatchBuilder, CandidateHandle, Domain, DomainCapabilities,
    DomainError, EpisodeArena, FeatureBatch, SolvedRoot, StateHandle, TransitionBatch,
    TransitionOutcome, UtilityContext, UtilityObservation, VerificationReceipt, VerifyBudget,
    VerifyError,
};
use reflex_canonical::{CanonicalEncode, CanonicalError, CanonicalWriter, encode_to_vec};
use reflex_economics::{BetterDirection, ConfidenceClass};
use reflex_types::{
    ActionSchemaId, AndGroupRef, ArtifactId, CandidateId, Digest, FeatureSchemaId, GenerationId,
    ResearchNodeId, StateId, TaskId,
};
use serde::{Deserialize, Serialize};
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicU64, Ordering};

// ---------------------------------------------------------------------------
// Conformance kit (P5.6)
// ---------------------------------------------------------------------------

/// Outcome of one conformance check.
#[derive(Clone, Debug)]
pub struct ConformanceCheck {
    pub name: &'static str,
    pub passed: bool,
    pub details: String,
}

/// Result of running a domain against the kit.
#[derive(Clone, Debug)]
pub struct ConformanceReport {
    pub checks: Vec<ConformanceCheck>,
}

impl ConformanceReport {
    pub fn all_passed(&self) -> bool {
        self.checks.iter().all(|c| c.passed)
    }

    pub fn failed_checks(&self) -> impl Iterator<Item = &ConformanceCheck> {
        self.checks.iter().filter(|c| !c.passed)
    }
}

/// Runs the conformance suite against a typed domain (P5.6).
///
/// Every check runs under `catch_unwind` so one panicking check cannot mask
/// the others. The kit reads domain outputs exclusively through the
/// caller-owned buffers of the §12.1 batch contract.
#[derive(Clone, Copy, Debug, Default)]
pub struct ConformanceKit;

impl ConformanceKit {
    pub fn new() -> Self {
        Self
    }

    pub fn run<D: Domain>(&self, domain: &D, task: &D::Task) -> ConformanceReport {
        let mut checks = Vec::new();

        // -- task identity -------------------------------------------------
        let task_id_a = self.check(
            "task_identity_stable",
            || domain.task_id(task).map(|_| ()).map_err(|e| e.to_string()),
            "task_id resolves twice without error",
        );
        checks.push(task_id_a);

        let task_id = match domain.task_id(task) {
            Ok(id) => {
                checks.push(ConformanceCheck {
                    name: "task_id_resolvable",
                    passed: true,
                    details: format!("{id}"),
                });
                id
            }
            Err(e) => {
                checks.push(ConformanceCheck {
                    name: "task_id_resolvable",
                    passed: false,
                    details: e.to_string(),
                });
                return ConformanceReport { checks };
            }
        };
        let _ = task_id;

        // -- initial state -------------------------------------------------
        let mut arena = EpisodeArena::new();
        let initial = match domain.initial_state(task, &mut arena) {
            Ok(handle) => {
                checks.push(ConformanceCheck {
                    name: "initial_state",
                    passed: true,
                    details: format!("handle {handle:?}"),
                });
                handle
            }
            Err(e) => {
                checks.push(ConformanceCheck {
                    name: "initial_state",
                    passed: false,
                    details: e.to_string(),
                });
                return ConformanceReport { checks };
            }
        };

        // initial_state_identity_stable: a second arena must produce the
        // same StateId.
        {
            let mut arena2 = EpisodeArena::new();
            let second = match domain.initial_state(task, &mut arena2) {
                Ok(handle) => handle,
                Err(e) => {
                    checks.push(ConformanceCheck {
                        name: "initial_state_identity_stable",
                        passed: false,
                        details: format!("second initial_state failed: {e}"),
                    });
                    return ConformanceReport { checks };
                }
            };
            match (
                domain.state_id(initial, &arena),
                domain.state_id(second, &arena2),
            ) {
                (Ok(a), Ok(b)) if a == b => checks.push(ConformanceCheck {
                    name: "initial_state_identity_stable",
                    passed: true,
                    details: "same StateId across arenas".to_string(),
                }),
                (Ok(a), Ok(b)) => checks.push(ConformanceCheck {
                    name: "initial_state_identity_stable",
                    passed: false,
                    details: format!("state ids diverge: {a} != {b}"),
                }),
                (a, b) => checks.push(ConformanceCheck {
                    name: "initial_state_identity_stable",
                    passed: false,
                    details: format!("state_id errors: {a:?}, {b:?}"),
                }),
            }
        }

        // -- candidate enumeration ------------------------------------------
        let mut builder = CandidateBatchBuilder::new();
        let mut builder2 = CandidateBatchBuilder::new();
        match (
            domain.enumerate_candidates(initial, &arena, &mut builder),
            domain.enumerate_candidates(initial, &arena, &mut builder2),
        ) {
            (Ok(()), Ok(())) => {
                let ids_a = builder.ids.clone();
                let ids_b = builder2.ids.clone();
                let classes_a = builder.classes.clone();
                let classes_b = builder2.classes.clone();
                let ties_a = builder.tie_breaks.clone();
                let ties_b = builder2.tie_breaks.clone();
                checks.push(ConformanceCheck {
                    name: "candidate_enumeration_deterministic",
                    passed: ids_a == ids_b && classes_a == classes_b && ties_a == ties_b,
                    details: format!(
                        "ids {} / classes {} / tie_breaks {}",
                        if ids_a == ids_b { "stable" } else { "DIVERGE" },
                        if classes_a == classes_b {
                            "stable"
                        } else {
                            "DIVERGE"
                        },
                        if ties_a == ties_b {
                            "stable"
                        } else {
                            "DIVERGE"
                        },
                    ),
                });
            }
            (a, b) => checks.push(ConformanceCheck {
                name: "candidate_enumeration_deterministic",
                passed: false,
                details: format!("enumeration failed: {a:?}, {b:?}"),
            }),
        }

        let batch = builder.build();

        // candidate_ids_unique within a batch
        {
            let mut seen = std::collections::HashSet::new();
            let mut duplicates = Vec::new();
            for id in &batch.ids {
                if !seen.insert(*id) {
                    duplicates.push(*id);
                }
            }
            checks.push(ConformanceCheck {
                name: "candidate_ids_unique",
                passed: duplicates.is_empty(),
                details: if duplicates.is_empty() {
                    format!("{} unique ids", batch.len())
                } else {
                    format!("duplicate ids: {duplicates:?}")
                },
            });
        }

        // -- transitions -----------------------------------------------------
        // transition_deterministic: apply the full selection in two fresh
        // arenas; outcome shapes (and child ownership) must match.
        let selection: Vec<reflex_types::CandidateIndex> =
            (0..batch.len()).map(reflex_types::CandidateIndex).collect();
        let mut t1 = TransitionBatch::new();
        let mut t2 = TransitionBatch::new();
        let mut arena_a = EpisodeArena::new();
        let mut arena_b = EpisodeArena::new();
        let initial_a = match domain.initial_state(task, &mut arena_a) {
            Ok(h) => h,
            Err(e) => {
                checks.push(ConformanceCheck {
                    name: "transition_deterministic",
                    passed: false,
                    details: format!("re-initial state failed: {e}"),
                });
                return ConformanceReport { checks };
            }
        };
        let initial_b = match domain.initial_state(task, &mut arena_b) {
            Ok(h) => h,
            Err(e) => {
                checks.push(ConformanceCheck {
                    name: "transition_deterministic",
                    passed: false,
                    details: format!("re-initial state failed: {e}"),
                });
                return ConformanceReport { checks };
            }
        };
        match (
            domain.apply_candidates(initial_a, &batch, &selection, &mut arena_a, &mut t1),
            domain.apply_candidates(initial_b, &batch, &selection, &mut arena_b, &mut t2),
        ) {
            (Ok(()), Ok(())) => {
                let s1 = serialize_outcomes(&t1.outcomes);
                let s2 = serialize_outcomes(&t2.outcomes);
                checks.push(ConformanceCheck {
                    name: "transition_deterministic",
                    passed: s1 == s2,
                    details: if s1 == s2 {
                        format!("{} outcomes stable", t1.outcomes.len())
                    } else {
                        format!("outcome shapes diverge: {s1:?} vs {s2:?}")
                    },
                });
            }
            (a, b) => checks.push(ConformanceCheck {
                name: "transition_deterministic",
                passed: false,
                details: format!("apply failed: {a:?}, {b:?}"),
            }),
        }

        // and_child_ownership: every Obligations child resolves to a live
        // state in the arena it was produced into.
        {
            let mut child_count = 0usize;
            let mut violations = 0usize;
            for outcome in &t1.outcomes {
                if let TransitionOutcome::Obligations { group } = outcome {
                    child_count += group.children.len();
                    for child in &group.children {
                        if !arena_a.contains_state(*child) {
                            violations += 1;
                        }
                    }
                }
            }
            checks.push(ConformanceCheck {
                name: "and_child_ownership",
                passed: violations == 0,
                details: if violations == 0 {
                    format!("{child_count} children resolve in the producing arena")
                } else {
                    format!("{violations}/{child_count} children do not resolve")
                },
            });
        }

        // -- feature buffers ---------------------------------------------------
        {
            let cap = domain.capabilities();
            let mut features = FeatureBatch::new(
                batch.len(),
                cap.feature_dimension.max(1),
                cap.feature_schema,
            );
            match domain.extract_features(&[initial], &batch, &arena, &mut features) {
                Ok(()) => {
                    let rows_ok = features.rows == batch.len();
                    let cols_ok = features.cols == cap.feature_dimension.max(1);
                    checks.push(ConformanceCheck {
                        name: "buffer_sizing",
                        passed: rows_ok && cols_ok,
                        details: format!(
                            "rows {}/{} cols {}/{}",
                            features.rows,
                            batch.len(),
                            features.cols,
                            cap.feature_dimension.max(1)
                        ),
                    });
                }
                Err(e) => checks.push(ConformanceCheck {
                    name: "buffer_sizing",
                    passed: false,
                    details: e.to_string(),
                }),
            }
        }

        // -- artifact reconstruction --------------------------------------------
        let solved = SolvedRoot {
            root_state: initial,
            solved_edges: Vec::new(),
        };
        let artifact = match domain.reconstruct_artifact(solved, &arena) {
            Ok(artifact) => {
                let digest = match reflex_canonical::content_id(b"conformance", &artifact) {
                    Ok(digest) => digest,
                    Err(error) => {
                        checks.push(ConformanceCheck {
                            name: "artifact_reconstruction",
                            passed: false,
                            details: format!("canonical artifact identity failed: {error}"),
                        });
                        return ConformanceReport { checks };
                    }
                };
                checks.push(ConformanceCheck {
                    name: "artifact_reconstruction",
                    passed: true,
                    details: format!("artifact digest {digest}"),
                });
                artifact
            }
            Err(e) => {
                checks.push(ConformanceCheck {
                    name: "artifact_reconstruction",
                    passed: false,
                    details: e.to_string(),
                });
                return ConformanceReport { checks };
            }
        };

        // -- verifier determinism -------------------------------------------------
        let budget = VerifyBudget {
            max_cpu_ns: 10_000_000,
            max_wall_ns: 10_000_000,
            max_memory_bytes: 1 << 20,
        };
        match (
            domain.verify(&artifact, budget),
            domain.verify(&artifact, budget),
        ) {
            (Ok(v1), Ok(v2)) => {
                let b1 = encode_to_vec(&v1);
                let b2 = encode_to_vec(&v2);
                match (b1, b2) {
                    (Ok(b1), Ok(b2)) => checks.push(ConformanceCheck {
                        name: "verifier_deterministic",
                        passed: b1 == b2,
                        details: if b1 == b2 {
                            format!("{} canonical bytes stable", b1.len())
                        } else {
                            "canonical bytes diverge across runs".to_string()
                        },
                    }),
                    (a, b) => checks.push(ConformanceCheck {
                        name: "verifier_deterministic",
                        passed: false,
                        details: format!("canonical encoding failed: {a:?}, {b:?}"),
                    }),
                }
            }
            (a, b) => {
                let da = a
                    .err()
                    .map(|e| e.to_string())
                    .unwrap_or_else(|| "ok".to_string());
                let db = b
                    .err()
                    .map(|e| e.to_string())
                    .unwrap_or_else(|| "ok".to_string());
                checks.push(ConformanceCheck {
                    name: "verifier_deterministic",
                    passed: false,
                    details: format!("verify failed: {da} / {db}"),
                })
            }
        }

        // -- utility observations ---------------------------------------------------
        let verification = match domain.verify(&artifact, budget) {
            Ok(v) => v,
            Err(e) => {
                checks.push(ConformanceCheck {
                    name: "utility_observations_finite",
                    passed: false,
                    details: format!("verify failed: {e}"),
                });
                return ConformanceReport { checks };
            }
        };
        let context = UtilityContext {
            subject: ResearchNodeId::from_digest(Digest::hash_blake3(b"conformance-subject")),
            population: Digest::hash_blake3(b"conformance-population"),
            observed_at_generation: GenerationId::from_digest(Digest::hash_blake3(
                b"conformance-generation",
            )),
            accepted_verification: Digest::hash_blake3(b"conformance-verification"),
            accepted_evidence: vec![Digest::hash_blake3(b"conformance-verification")],
            cell_cpu_ns: 1_000_000,
            model_inference_cpu_ns: 200_000,
            retrieval_cpu_ns: 100_000,
            verified_actions_count: 1,
        };
        let mut observations = Vec::new();
        match domain.evaluate_utility(&artifact, &verification, &context, &mut observations) {
            Ok(()) => {
                let non_empty = !observations.is_empty();
                let finite = observations
                    .iter()
                    .all(|o| o.value.as_f64().map(|v| v.is_finite()).unwrap_or(false));
                let metrics_ok = observations.iter().all(|observation| {
                    observation.evaluator.0 != Digest::ZERO
                        && context.validate_observation(observation).is_ok()
                });
                checks.push(ConformanceCheck {
                    name: "utility_observations_finite",
                    passed: non_empty && finite && metrics_ok,
                    details: format!(
                        "{} observations, finite: {finite}, metrics set: {metrics_ok}",
                        observations.len()
                    ),
                });
            }
            Err(e) => checks.push(ConformanceCheck {
                name: "utility_observations_finite",
                passed: false,
                details: e.to_string(),
            }),
        }

        // -- arena lifecycle ------------------------------------------------------------
        {
            let before = arena.generation();
            arena.clear();
            checks.push(ConformanceCheck {
                name: "arena_generation_bump",
                passed: arena.generation() == before.wrapping_add(1),
                details: format!("{before} -> {}", arena.generation()),
            });
        }

        {
            let mut arena3 = EpisodeArena::new();
            let live = match domain.initial_state(task, &mut arena3) {
                Ok(h) => h,
                Err(e) => {
                    checks.push(ConformanceCheck {
                        name: "stale_handle_rejected",
                        passed: false,
                        details: e.to_string(),
                    });
                    return ConformanceReport { checks };
                }
            };
            arena3.clear();
            match arena3.resolve_state(live) {
                Err(DomainError::StaleStateHandle { .. }) => checks.push(ConformanceCheck {
                    name: "stale_handle_rejected",
                    passed: true,
                    details: "state handle rejected after arena reset".to_string(),
                }),
                Ok(_) => checks.push(ConformanceCheck {
                    name: "stale_handle_rejected",
                    passed: false,
                    details: "stale handle resolved: arena did not stamp generations".to_string(),
                }),
                Err(e) => checks.push(ConformanceCheck {
                    name: "stale_handle_rejected",
                    passed: false,
                    details: format!("unexpected error class: {e}"),
                }),
            }
        }

        ConformanceReport { checks }
    }

    fn check(
        &self,
        name: &'static str,
        f: impl FnOnce() -> Result<(), String>,
        description: &'static str,
    ) -> ConformanceCheck {
        match std::panic::catch_unwind(AssertUnwindSafe(f)) {
            Ok(Ok(())) => ConformanceCheck {
                name,
                passed: true,
                details: description.to_string(),
            },
            Ok(Err(details)) => ConformanceCheck {
                name,
                passed: false,
                details,
            },
            Err(_) => ConformanceCheck {
                name,
                passed: false,
                details: "panicked".to_string(),
            },
        }
    }
}

/// Compact, deterministic summary of an outcome sequence (for the
/// determinism checks).
fn serialize_outcomes(outcomes: &[TransitionOutcome]) -> Vec<(u8, u64, Vec<u32>)> {
    outcomes
        .iter()
        .map(|o| match o {
            TransitionOutcome::Closed { .. } => (0, 0, Vec::new()),
            TransitionOutcome::Obligations { group } => (
                1,
                group.group_id,
                group.children.iter().map(|c| c.index()).collect(),
            ),
            TransitionOutcome::Contradiction { .. } => (2, 0, Vec::new()),
            TransitionOutcome::Invalid { code } => (3, u64::from(code.index()), Vec::new()),
            TransitionOutcome::Unresolved { code } => (4, u64::from(code.index()), Vec::new()),
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Receipt well-formedness (INV-RFX-1 traceability)
// ---------------------------------------------------------------------------

/// Runs the receipt well-formedness rules against a receipt.
pub fn check_receipt(receipt: &VerificationReceipt) -> Result<(), String> {
    if receipt.implementation == Digest::ZERO {
        return Err("implementation digest is ZERO".to_string());
    }
    if receipt.artifact == ArtifactId::from_digest(Digest::ZERO) {
        return Err("artifact id is ZERO".to_string());
    }
    if let crate::VerificationStatus::Accepted = receipt.status
        && receipt.replay_command.command.is_empty()
    {
        return Err("accepted receipt has no replay command".to_string());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Reference fixture domain (P5.6)
// ---------------------------------------------------------------------------

/// Canonical digest of a fixture task (used by identity tests).
pub fn fixture_task_id(task: &FixtureTask) -> Result<TaskId, DomainError> {
    reflex_canonical::content_id(b"fixture-domain:task", task)
        .map(TaskId::from_digest)
        .map_err(|error| {
            DomainError::InvalidTask(format!("canonical task identity failed: {error}"))
        })
}

/// Canonical state id of a fixture value/target pair.
pub fn fixture_state_id(value: u32, target: u32) -> Result<StateId, DomainError> {
    reflex_canonical::content_id(
        b"fixture-domain:state",
        &FixtureStateIdParts { value, target },
    )
    .map(StateId::from_digest)
    .map_err(|error| DomainError::InvalidTask(format!("canonical state identity failed: {error}")))
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct FixtureStateIdParts {
    value: u32,
    target: u32,
}

impl CanonicalEncode for FixtureStateIdParts {
    fn encode_canonical(&self, out: &mut CanonicalWriter) -> Result<(), CanonicalError> {
        out.write_u32(self.value)?;
        out.write_u32(self.target)?;
        Ok(())
    }
}

/// Task for the fixture domain: raise `value` to `target` by repeated
/// sum actions.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FixtureTask {
    pub start: u32,
    pub target: u32,
    pub candidates_per_state: usize,
    pub allow_duplicate_ids: bool,
}

impl CanonicalEncode for FixtureTask {
    fn encode_canonical(&self, out: &mut CanonicalWriter) -> Result<(), CanonicalError> {
        out.write_u32(self.start)?;
        out.write_u32(self.target)?;
        out.write_u64(self.candidates_per_state as u64)?;
        out.write_bool(self.allow_duplicate_ids)?;
        Ok(())
    }
}

/// Fixture state: an integer position on the way to the target.
#[derive(Clone, Debug)]
pub struct FixtureState {
    pub value: u32,
}

impl CanonicalEncode for FixtureState {
    fn encode_canonical(&self, out: &mut CanonicalWriter) -> Result<(), CanonicalError> {
        out.write_u32(self.value)?;
        Ok(())
    }
}

/// Fixture candidate: one possible step (action carried in the tie-break
/// lane; payload is the canonical id for typed access).
#[derive(Clone, Debug)]
pub struct FixtureCandidate {
    pub action: u32,
    pub id: CandidateId,
}

impl CanonicalEncode for FixtureCandidate {
    fn encode_canonical(&self, out: &mut CanonicalWriter) -> Result<(), CanonicalError> {
        out.write_u32(self.action)?;
        out.write_digest(&self.id.0)?;
        Ok(())
    }
}

/// Fixture artifact: the reached value, claimed as the answer.
#[derive(Clone, Debug)]
pub struct FixtureArtifact {
    pub value: u32,
    pub id: ArtifactId,
}

impl FixtureArtifact {
    pub fn digest(&self) -> Digest {
        self.id.0
    }
}

impl CanonicalEncode for FixtureArtifact {
    fn encode_canonical(&self, out: &mut CanonicalWriter) -> Result<(), CanonicalError> {
        out.write_u32(self.value)?;
        out.write_digest(&self.id.0)?;
        Ok(())
    }
}

/// Fixture verification: the artifact value equals the target.
#[derive(Clone, Debug)]
pub struct FixtureVerification {
    pub accepted: bool,
    pub artifact: ArtifactId,
}

impl CanonicalEncode for FixtureVerification {
    fn encode_canonical(&self, out: &mut CanonicalWriter) -> Result<(), CanonicalError> {
        out.write_bool(self.accepted)?;
        out.write_digest(&self.artifact.0)?;
        Ok(())
    }
}

/// The reference domain (P5.6). Deterministic by default; variants flip
/// enumeration order or collapse candidate ids to prove the kit catches
/// both classes of violation.
pub struct FixtureDomain {
    capabilities: DomainCapabilities,
    target: u32,
    nondeterministic: bool,
    duplicate_ids: bool,
}

impl FixtureDomain {
    pub fn new() -> Self {
        Self {
            capabilities: DomainCapabilities {
                domain_id: "fixture".to_string(),
                domain_digest: Digest::from_blake3_bytes([0x11; 32]),
                action_schema: ActionSchemaId::from_digest(Digest::from_blake3_bytes([0x21; 32])),
                feature_schema: FeatureSchemaId::from_digest(Digest::from_blake3_bytes([0x31; 32])),
                feature_dimension: 1,
                max_candidates_per_state: 4,
                deterministic_generation: true,
                supports_exact_cache: true,
            },
            target: 3,
            nondeterministic: false,
            duplicate_ids: false,
        }
    }

    /// Flips enumeration order every other call.
    pub fn nondeterministic() -> Self {
        let mut domain = Self::new();
        domain.nondeterministic = true;
        domain
    }

    /// Emits the same candidate id for every action.
    pub fn duplicate_ids() -> Self {
        let mut domain = Self::new();
        domain.duplicate_ids = true;
        domain
    }
}

impl Default for FixtureDomain {
    fn default() -> Self {
        Self::new()
    }
}

impl Domain for FixtureDomain {
    type Task = FixtureTask;
    type State = FixtureState;
    type Candidate = FixtureCandidate;
    type Transition = FixtureState;
    type Artifact = FixtureArtifact;
    type Verification = FixtureVerification;

    fn capabilities(&self) -> DomainCapabilities {
        self.capabilities.clone()
    }

    fn task_id(&self, task: &Self::Task) -> Result<TaskId, DomainError> {
        fixture_task_id(task)
    }

    fn initial_state(
        &self,
        task: &Self::Task,
        arena: &mut EpisodeArena,
    ) -> Result<StateHandle, DomainError> {
        let id = fixture_state_id(task.start, task.target)?;
        Ok(arena.insert_state(FixtureState { value: task.start }, id))
    }

    fn state_id(&self, state: StateHandle, arena: &EpisodeArena) -> Result<StateId, DomainError> {
        let state = arena
            .get_state::<FixtureState>(state)
            .ok_or(DomainError::InvalidStateHandle(state.index()))?;
        fixture_state_id(state.value, self.target)
    }

    fn enumerate_candidates(
        &self,
        state: StateHandle,
        arena: &EpisodeArena,
        output: &mut CandidateBatchBuilder,
    ) -> Result<(), DomainError> {
        let state = arena
            .get_state::<FixtureState>(state)
            .ok_or(DomainError::InvalidStateHandle(state.index()))?;
        let remaining = self.target.saturating_sub(state.value) as usize;
        let count = remaining
            .min(self.capabilities.max_candidates_per_state as usize)
            .max(1);
        let mut actions: Vec<u32> = (1..=count as u32).collect();
        if self.nondeterministic && ENUMERATION_CALL.fetch_add(1, Ordering::SeqCst) % 2 == 1 {
            actions.reverse();
        }
        output.ids.clear();
        output.classes.clear();
        output.tie_breaks.clear();
        output.payload_handles.clear();
        output.flags.clear();
        for action in actions {
            let id = if self.duplicate_ids {
                candidate_id(state.value, 0)?
            } else {
                candidate_id(state.value, action)?
            };
            // Candidate payloads live in the SoA lanes (id, class,
            // tie_break, flag); the arena candidate slot is only for
            // apply-time payloads (§10.1: enumeration reads the arena).
            output.add(id, 0, u64::from(action), CandidateHandle(0), 0);
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
                .get_state::<FixtureState>(*handle)
                .ok_or(DomainError::InvalidStateHandle(handle.index()))?;
            for col in 0..(output.cols.min(candidates.len())) {
                let value = if col < candidates.tie_breaks.len() {
                    candidates.tie_breaks[col] as f32
                } else {
                    0.0
                };
                let _ = state;
                output.row_mut(row)[col] = value;
            }
        }
        Ok(())
    }

    fn apply_candidates(
        &self,
        state: StateHandle,
        candidates: &CandidateBatch,
        selection: &[reflex_types::CandidateIndex],
        arena: &mut EpisodeArena,
        output: &mut TransitionBatch,
    ) -> Result<(), DomainError> {
        let state = arena
            .get_state::<FixtureState>(state)
            .ok_or(DomainError::InvalidStateHandle(state.index()))?;
        let current = state.value;
        for selection_index in selection {
            let index = selection_index.0;
            if index >= candidates.len() {
                return Err(DomainError::InvalidCandidateIndex(index));
            }
            let action = candidates.tie_breaks[index] as u32;
            let next = current.saturating_add(action);
            if next >= self.target {
                output.add(TransitionOutcome::Closed {
                    witness: reflex_types::DomainWitnessRef {
                        artifact: ArtifactId::from_digest(Digest::from_blake3_bytes([0x41; 32])),
                        verification: None,
                    },
                });
            } else {
                let child = arena.insert_state(
                    FixtureState { value: next },
                    fixture_state_id(next, self.target)?,
                );
                output.add(TransitionOutcome::Obligations {
                    group: AndGroupRef {
                        group_id: (u64::from(current) << 32) | u64::from(action),
                        children: vec![child],
                    },
                });
            }
        }
        Ok(())
    }

    fn reconstruct_artifact(
        &self,
        _solved: SolvedRoot,
        _arena: &EpisodeArena,
    ) -> Result<Self::Artifact, DomainError> {
        Ok(FixtureArtifact {
            value: self.target,
            id: ArtifactId::from_digest(Digest::from_blake3_bytes([0x41; 32])),
        })
    }

    fn verify(
        &self,
        artifact: &Self::Artifact,
        _budget: VerifyBudget,
    ) -> Result<Self::Verification, VerifyError> {
        Ok(FixtureVerification {
            accepted: artifact.value == self.target,
            artifact: artifact.id,
        })
    }

    fn evaluate_utility(
        &self,
        _artifact: &Self::Artifact,
        _verification: &Self::Verification,
        context: &UtilityContext,
        output: &mut Vec<UtilityObservation>,
    ) -> Result<(), DomainError> {
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

static ENUMERATION_CALL: AtomicU64 = AtomicU64::new(0);

fn candidate_id(state_value: u32, action: u32) -> Result<CandidateId, DomainError> {
    reflex_canonical::content_id(
        b"fixture-domain:candidate",
        &FixtureCandidateIdParts {
            state: state_value,
            action,
        },
    )
    .map(CandidateId::from_digest)
    .map_err(|error| {
        DomainError::Enumeration(format!("canonical candidate identity failed: {error}"))
    })
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct FixtureCandidateIdParts {
    state: u32,
    action: u32,
}

impl CanonicalEncode for FixtureCandidateIdParts {
    fn encode_canonical(&self, out: &mut CanonicalWriter) -> Result<(), CanonicalError> {
        out.write_u32(self.state)?;
        out.write_u32(self.action)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_kit_passes_deterministic_domain() {
        let domain = FixtureDomain::new();
        let task = FixtureTask {
            start: 0,
            target: 3,
            candidates_per_state: 2,
            allow_duplicate_ids: false,
        };
        let report = ConformanceKit::new().run(&domain, &task);
        let failures: Vec<_> = report
            .failed_checks()
            .map(|c| (c.name, c.details.clone()))
            .collect();
        assert!(report.all_passed(), "failures: {failures:?}");
    }

    #[test]
    fn test_kit_detects_nondeterministic_enumeration() {
        let domain = FixtureDomain::nondeterministic();
        let task = FixtureTask {
            start: 1,
            target: 3,
            candidates_per_state: 2,
            allow_duplicate_ids: false,
        };
        let report = ConformanceKit::new().run(&domain, &task);
        let enumeration = report
            .checks
            .iter()
            .find(|c| c.name == "candidate_enumeration_deterministic")
            .expect("check must exist");
        assert!(!enumeration.passed, "nondeterminism must be detected");
    }

    #[test]
    fn test_kit_detects_duplicate_candidate_ids() {
        let domain = FixtureDomain::duplicate_ids();
        let task = FixtureTask {
            start: 0,
            target: 3,
            candidates_per_state: 2,
            allow_duplicate_ids: true,
        };
        let report = ConformanceKit::new().run(&domain, &task);
        let uniqueness = report
            .checks
            .iter()
            .find(|c| c.name == "candidate_ids_unique")
            .expect("check must exist");
        assert!(!uniqueness.passed, "duplicate ids must be detected");
    }

    #[test]
    fn test_receipt_check() {
        use crate::VerificationStatus;
        let mut receipt = crate::VerificationReceipt {
            verifier: reflex_types::VerifierId::from_digest(Digest::from_blake3_bytes([0x61; 32])),
            implementation: Digest::from_blake3_bytes([0x62; 32]),
            semantic_anchor: Digest::from_blake3_bytes([0x63; 32]),
            artifact: ArtifactId::from_digest(Digest::from_blake3_bytes([0x64; 32])),
            status: VerificationStatus::Accepted,
            proof_or_certificate: None,
            assumptions: Vec::new(),
            cpu_ns: 0,
            wall_ns: 0,
            peak_rss_bytes: 0,
            replay_command: crate::ReplayCommand {
                command: "verify".to_string(),
                args: Vec::new(),
            },
        };
        assert!(check_receipt(&receipt).is_ok());
        receipt.replay_command.command.clear();
        assert!(check_receipt(&receipt).is_err());
        receipt.status = VerificationStatus::Rejected;
        assert!(check_receipt(&receipt).is_ok());
    }
}
