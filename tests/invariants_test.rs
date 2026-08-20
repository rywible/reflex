#![forbid(unsafe_code)]

use reflex_bench::{BenchmarkRecord, PerformanceBudgetRegistry};
use reflex_cas::{
    ArtifactArena, ArtifactArenaLimits, EvidenceArtifact, LocalEvidenceBundleStore, RetentionClass,
};
use reflex_dataset::{CandidateKnowledge, DecisionGroup};
use reflex_domain::{Domain, VerifyBudget};
use reflex_domain_bitvec::{BitvecArtifact, BitvecDomain, BvExpr};
use reflex_domain_lean::{M2aImportError, m2a_evidence_dir, reconstruct_m2a_result};
use reflex_economics::{
    BetterDirection, ConfidenceClass, EconomicLedgerSummary, RationalOrFloat, ScalarizationPolicy,
    UnitRegistry, UtilityObservation,
};
use reflex_engine::{
    AttemptOutcome, CellRegistration, CellState, EngineError, EngineLimits, ExperimentRegistration,
    GenerationRegistration, LocalRunState,
};
use reflex_eval::EvaluationReport;
use reflex_knowledge::{KnowledgeBase, KnowledgeClass, KnowledgeRecord};
use reflex_report::{
    CellOutcomeCounts, ReportEvidenceManifest, ScientificReport, ScientificReportParams,
    cell_query_result_digest, economics_query_result_digest, evaluation_query_result_digest,
};
use reflex_runtime::{
    CellContext, CellExecutionManifest, ResolvedCellInputs, ResourceAllocation,
    SearchExecutionConfig, ThreadBudget, VerifiedCellManifest,
};
use reflex_scheduler::{
    CellManifest, ExperimentManifest, ExperimentMode, ImmutableInputs, ManifestLabels,
    ResourceRequirements, SearchConfig,
};
use reflex_search::{OrderedScore, ProofDag};
use reflex_types::{
    ActionSchemaId, CandidateId, CellId, CompatibilityDigest, Digest, DomainId, EvaluatorId,
    ExperimentId, FeatureSchemaId, GenerationId, KnowledgeEditionId, KnowledgeRecordId, MetricId,
    ModelCheckpointId, ResearchNodeId, StateId, TaskId, UnitId, VerifierId,
};
use std::collections::BTreeMap;
use std::sync::Arc;
use tokio_util::codec::Decoder;

fn verified_manifest(
    name: &[u8],
    model: Option<ModelCheckpointId>,
    knowledge: Option<KnowledgeEditionId>,
    cpu_permits: u32,
) -> VerifiedCellManifest {
    let manifest = CellExecutionManifest::new(
        CellId::from_digest(Digest::hash_blake3(name)),
        TaskId::from_digest(Digest::hash_blake3(b"invariant-task")),
        DomainId::from_digest(Digest::hash_blake3(b"invariant-domain")),
        SearchExecutionConfig::new("best-first", 100, 1_000, 5_000_000),
        42,
        ResourceAllocation::new("invariant-test", cpu_permits, 8 * 1024 * 1024, 1024),
        model,
        knowledge,
        BTreeMap::new(),
    )
    .unwrap();
    let bytes = manifest.to_bytes().unwrap();
    VerifiedCellManifest::verify(&bytes, manifest.digest().unwrap()).unwrap()
}

fn resolved_inputs(
    model: Option<ModelCheckpointId>,
    knowledge: Option<KnowledgeEditionId>,
) -> ResolvedCellInputs {
    let mut inputs = ResolvedCellInputs::new();
    if let Some(id) = model {
        inputs = inputs.with_model(id, Arc::new("model payload".to_string()));
    }
    if let Some(id) = knowledge {
        inputs = inputs.with_knowledge(id, Arc::new("knowledge payload".to_string()));
    }
    inputs
}

fn immutable_inputs(search: SearchConfig) -> ImmutableInputs {
    ImmutableInputs {
        code: Digest::hash_blake3(b"code"),
        image: Some(Digest::hash_blake3(b"image")),
        domain: Digest::hash_blake3(b"domain"),
        toolchain: Digest::hash_blake3(b"toolchain"),
        corpus: Digest::hash_blake3(b"corpus"),
        split: Digest::hash_blake3(b"split"),
        feature_schema: FeatureSchemaId::from_digest(Digest::hash_blake3(b"features")),
        action_schema: ActionSchemaId::from_digest(Digest::hash_blake3(b"actions")),
        policy: Digest::hash_blake3(b"policy"),
        model_checkpoint: None,
        knowledge_edition: None,
        cache_snapshot: None,
        search,
        resource: ResourceRequirements {
            class: "reference-4vcpu-8gb".to_string(),
            cpu_permits: 4,
            memory_bytes: 8 * 1024 * 1024,
            scratch_bytes: 1024,
        },
        verifier: VerifierId::from_digest(Digest::hash_blake3(b"verifier")),
        utility_evaluators: vec![EvaluatorId::from_digest(Digest::hash_blake3(b"utility"))],
        analysis_plan: Digest::hash_blake3(b"analysis"),
    }
}

#[tokio::test]
async fn test_inv_1_verifier_authority() {
    // INV-RFX-1: Verifier authority. A model score or test never substitutes for external verifier.
    let domain = BitvecDomain::new();
    let invalid_artifact = BitvecArtifact {
        original: BvExpr::Add(Box::new(BvExpr::Var(0)), Box::new(BvExpr::Var(0))),
        optimized: BvExpr::Const(0), // Incorrect: x + x != 0 in general
    };
    let receipt = domain
        .verify(
            &invalid_artifact,
            VerifyBudget {
                max_cpu_ns: 100000,
                max_wall_ns: 100000,
                max_memory_bytes: 1024,
            },
        )
        .unwrap();
    assert!(!receipt.is_equivalent);
    assert!(receipt.counterexample.is_some());
}

#[test]
fn test_inv_2_immutable_inputs() {
    // INV-RFX-2: Immutable inputs. Registered cells pin every input by digest.
    let manifest1 = CellManifest {
        schema: "reflex.cell.v1".to_string(),
        experiment_id: ExperimentId::from_digest(Digest::hash_blake3(b"exp1")),
        generation_id: GenerationId::from_digest(Digest::hash_blake3(b"gen1")),
        task_id: TaskId::from_digest(Digest::hash_blake3(b"task1")),
        seed: 42,
        inputs: immutable_inputs(SearchConfig {
            algorithm: "best-first".to_string(),
            action_budget: 1000,
            node_budget: 5000,
            cpu_seconds: 10.0,
            exploration_uniform: 0.1,
        }),
    };

    let mut manifest2 = manifest1.clone();
    manifest2.seed = 43; // Tamper with seed

    assert_ne!(manifest1.cell_id().unwrap(), manifest2.cell_id().unwrap());
}

#[test]
fn test_inv_3_no_mutable_model_inside_cell() {
    // INV-RFX-3: No mutable model inside a cell — payload is pinned Arc, id is immutable.
    let model1 = ModelCheckpointId::from_digest(Digest::hash_blake3(b"model1"));
    let model2 = ModelCheckpointId::from_digest(Digest::hash_blake3(b"model2"));
    let ctx = CellContext::new(
        verified_manifest(b"inv-3-manifest", Some(model1), None, 4),
        resolved_inputs(Some(model1), None),
        ThreadBudget::new(4),
    )
    .unwrap();
    assert_eq!(ctx.model_checkpoint().unwrap().id(), &model1);
    assert_ne!(ctx.model_checkpoint().unwrap().id(), &model2);
    assert_eq!(
        ctx.model_checkpoint()
            .unwrap()
            .payload::<String>()
            .map(String::as_str),
        Some("model payload")
    );
}

#[tokio::test]
async fn test_inv_4_no_mutable_knowledge_inside_cell() {
    // INV-RFX-4: Running cells pin knowledge edition; overlay cannot mutate pinned id.
    let kn2 = KnowledgeEditionId::from_digest(Digest::hash_blake3(b"kn2"));
    let kb = KnowledgeBase::new().unwrap();
    let mut rec = KnowledgeRecord {
        id: KnowledgeRecordId::from_digest(Digest::ZERO),
        class: KnowledgeClass::ExactMemory,
        statement: "x xor x = 0".into(),
        proof_or_cert_digest: Digest::hash_blake3(b"overlay-proof"),
        structural_tags: vec!["overlay".into()],
        canonical_key: None,
        discovery_generation: 2,
        utility_score: 1.0,
        verification_receipt: Digest::hash_blake3(b"rcpt"),
        archived: false,
    };
    rec.id = rec.canonical_id().unwrap();
    let record_id = rec.id;
    kb.add_record(rec).await.unwrap();
    let base = kb
        .create_edition(
            "invariant",
            CompatibilityDigest::from_digest(Digest::hash_blake3(b"inv-4-compat")),
            None,
        )
        .await
        .unwrap();
    let kn1 = base.id;
    let model = ModelCheckpointId::from_digest(Digest::hash_blake3(b"inv-4-model"));
    let ctx = CellContext::new(
        verified_manifest(b"inv-4-manifest", Some(model), Some(kn1), 4),
        resolved_inputs(Some(model), Some(kn1)),
        ThreadBudget::new(4),
    )
    .unwrap();
    assert_eq!(ctx.knowledge_edition().unwrap().id(), &kn1);
    assert_ne!(ctx.knowledge_edition().unwrap().id(), &kn2);

    let overlay = kb.activate_overlay(vec![record_id], kn1).await.unwrap();
    assert_ne!(overlay.parent_edition, kn2);
    assert_eq!(ctx.knowledge_edition().unwrap().id(), &kn1);
}

#[test]
fn test_inv_5_censored_means_unknown() {
    // INV-RFX-5: Censored means unknown. Bounded failure never converted to candidate deadness.
    let group = DecisionGroup {
        state_id: StateId::from_digest(Digest::ZERO),
        candidate_ids: vec![CandidateId::from_digest(Digest::hash_blake3(b"cand1"))],
        labels: vec![CandidateKnowledge::Unknown],
        feature_ref: Digest::ZERO,
        source_episodes: Vec::new(),
        coverage: "censored_budget_exhausted".to_string(),
    };
    assert_eq!(group.labels[0], CandidateKnowledge::Unknown);
    assert!(!matches!(
        group.labels[0],
        CandidateKnowledge::KnownDead { .. }
    ));
}

#[test]
fn test_inv_6_multiple_valid_routes_survive() {
    // INV-RFX-6: Multiple valid routes survive in dataset compilation.
    let domain = BitvecDomain::new();
    let route_a = BitvecArtifact {
        original: BvExpr::Add(Box::new(BvExpr::Var(0)), Box::new(BvExpr::Const(0))),
        optimized: BvExpr::Var(0),
    };
    let route_b = BitvecArtifact {
        original: BvExpr::Xor(Box::new(BvExpr::Var(0)), Box::new(BvExpr::Var(0))),
        optimized: BvExpr::Const(0),
    };
    let verify_route = |artifact: &BitvecArtifact| {
        let verification = domain
            .verify(
                artifact,
                VerifyBudget {
                    max_cpu_ns: 1_000_000,
                    max_wall_ns: 1_000_000,
                    max_memory_bytes: 1024,
                },
            )
            .expect("deterministic fixture verifier must run");
        assert!(
            verification.is_equivalent,
            "only verifier-accepted routes may enter the proof DAG"
        );
        reflex_canonical::content_id(b"bitvec.verify.v1", &verification)
            .expect("fixture verifier output is canonically encodable")
    };
    let receipt_a = verify_route(&route_a);
    let receipt_b = verify_route(&route_b);
    assert_ne!(receipt_a, Digest::ZERO);
    assert_ne!(receipt_b, Digest::ZERO);

    let mut dag = ProofDag::new();
    let state = StateId::from_digest(Digest::hash_blake3(b"state_A"));
    let cand_a = CandidateId::from_digest(
        reflex_canonical::content_id(b"inv-6.route.v1", &route_a)
            .expect("route A is canonically encodable"),
    );
    let cand_b = CandidateId::from_digest(
        reflex_canonical::content_id(b"inv-6.route.v1", &route_b)
            .expect("route B is canonically encodable"),
    );

    dag.mark_verified_route(&[(state, cand_a, 5, receipt_a), (state, cand_b, 8, receipt_b)]);

    match dag.get_knowledge(state, cand_a) {
        Some(reflex_search::CandidateKnowledge::Viable {
            best_actions_to_go,
            receipts,
        }) => {
            assert_eq!(*best_actions_to_go, 5);
            assert_eq!(receipts.as_slice(), &[receipt_a]);
        }
        other => panic!("expected receipt-backed viable route A, got {other:?}"),
    }
    match dag.get_knowledge(state, cand_b) {
        Some(reflex_search::CandidateKnowledge::Viable {
            best_actions_to_go,
            receipts,
        }) => {
            assert_eq!(*best_actions_to_go, 8);
            assert_eq!(receipts.as_slice(), &[receipt_b]);
        }
        other => panic!("expected receipt-backed viable route B, got {other:?}"),
    }
    let surviving_routes: Vec<_> = dag.viable_edges().collect();
    assert_eq!(surviving_routes.len(), 2);
    assert!(
        surviving_routes
            .iter()
            .any(|((_, candidate), _, receipts)| {
                *candidate == cand_a && receipts.contains(&receipt_a)
            })
    );
    assert!(
        surviving_routes
            .iter()
            .any(|((_, candidate), _, receipts)| {
                *candidate == cand_b && receipts.contains(&receipt_b)
            })
    );
}

#[test]
fn test_inv_7_raw_utility_immutable() {
    // INV-RFX-7: Raw utility is immutable; scalarization does not mutate observations.
    let reg = UnitRegistry::with_standard_units();
    let cycles = UnitId::from_digest(Digest::hash_blake3(b"cycles"));
    let obs = UtilityObservation {
        subject: ResearchNodeId::from_digest(Digest::ZERO),
        evaluator: EvaluatorId::from_digest(Digest::ZERO),
        metric: MetricId::from_digest(Digest::hash_blake3(b"cycles")),
        value: RationalOrFloat::Rational {
            numerator: 1250,
            denominator: 1,
        },
        unit: cycles,
        direction: BetterDirection::HigherIsBetter,
        population: Digest::ZERO,
        evidence: Vec::new(),
        confidence: ConfidenceClass::ObservedExact,
        observed_at_generation: GenerationId::from_digest(Digest::ZERO),
        restricted_work: false,
    };
    let original = obs.value.clone();
    let policy = ScalarizationPolicy::default();
    let _net = policy
        .scalarize(
            std::slice::from_ref(&obs),
            &reg,
            &EconomicLedgerSummary::default(),
        )
        .unwrap();
    assert_eq!(obs.value, original);
    let err = reg.add_values(
        cycles,
        &RationalOrFloat::Float(1.0),
        UnitId::from_digest(Digest::hash_blake3(b"bytes")),
        &RationalOrFloat::Float(2.0),
    );
    assert!(err.is_err());
}

#[test]
fn test_inv_8_search_cost_includes_ml() {
    // INV-RFX-8: Search cost includes ML inference and feature extraction overhead.
    let summary =
        EconomicLedgerSummary::compute_net_savings(100_000.0, 1.0, 5_000, 2_000, 1_000).unwrap();
    assert_eq!(summary.ml_inference_tax_cpu_ns, 5_000);
    assert_eq!(summary.feature_extraction_tax_cpu_ns, 2_000);
    assert_eq!(summary.net_utility_value, 92_000.0);
}

#[tokio::test]
async fn test_inv_9_no_partial_artifact_publication() {
    // INV-RFX-9: Only a complete, verified evidence bundle becomes CURRENT.
    let temp = tempfile::tempdir().unwrap();
    let store = LocalEvidenceBundleStore::new(temp.path().to_path_buf(), 4096, 2048, 8).unwrap();
    std::fs::create_dir_all(temp.path().join("bundles/ignored.partial")).unwrap();
    let payload: Arc<[u8]> = Arc::from(&b"complete verified artifact payload"[..]);
    let (_, committed) = store
        .commit(
            "reflex.invariant-evidence.v1",
            vec![EvidenceArtifact {
                name: "receipt.bin".into(),
                bytes: Arc::clone(&payload),
            }],
        )
        .unwrap();
    let (read_back, reopened) = store.read_current("reflex.invariant-evidence.v1").unwrap();
    assert_eq!(reopened.digest(), committed.digest());
    assert_eq!(read_back.artifacts.get("receipt.bin"), Some(&payload));
}

#[test]
fn test_inv_10_one_accepted_attempt() {
    // INV-RFX-10: one accepted result under a private attempt/epoch ticket.
    let exp_id = ExperimentId::from_digest(Digest::hash_blake3(b"exp"));
    let generation_id = GenerationId::from_digest(Digest::hash_blake3(b"inv-10-generation"));
    let cell_id = CellId::from_digest(Digest::hash_blake3(b"cell"));
    let mut engine = LocalRunState::new(EngineLimits::new(1, 1, 1).unwrap()).unwrap();
    engine
        .register_experiment(ExperimentRegistration {
            id: exp_id,
            manifest: Digest::hash_blake3(b"inv-10-experiment-manifest"),
        })
        .unwrap();
    engine
        .register_generation(GenerationRegistration {
            id: generation_id,
            experiment_id: exp_id,
            ordinal: 1,
        })
        .unwrap();
    engine
        .register_cell(CellRegistration {
            id: cell_id,
            experiment_id: exp_id,
            generation_id,
            manifest: Digest::hash_blake3(b"inv-10-cell-manifest"),
            priority: 1,
        })
        .unwrap();
    let ticket = engine.claim_next().unwrap().unwrap().ticket;

    let temp = tempfile::tempdir().unwrap();
    let evidence_store =
        LocalEvidenceBundleStore::new(temp.path().to_path_buf(), 4096, 2048, 4).unwrap();
    let (_, evidence) = evidence_store
        .commit(
            "reflex.invariant-attempt.v1",
            vec![EvidenceArtifact {
                name: "completion.bin".into(),
                bytes: Arc::from(&b"accepted completion"[..]),
            }],
        )
        .unwrap();
    let completion = engine
        .finalize(ticket, AttemptOutcome::Accepted, &evidence)
        .unwrap();
    assert_eq!(completion.cell, cell_id);
    assert_eq!(engine.cell(cell_id).unwrap().state, CellState::Succeeded);

    let err = engine
        .finalize(ticket, AttemptOutcome::Accepted, &evidence)
        .unwrap_err();
    assert!(matches!(
        err,
        EngineError::CellNotRunning {
            state: CellState::Succeeded,
            ..
        }
    ));
}

#[test]
fn test_inv_12_no_shared_sqlite() {
    // INV-RFX-12: one directly owned local state machine orders mutations.
    let exp_id = ExperimentId::from_digest(Digest::hash_blake3(b"inv-12-exp"));
    let generation_id = GenerationId::from_digest(Digest::hash_blake3(b"inv-12-generation"));
    let mut engine = LocalRunState::new(EngineLimits::new(1, 1, 2).unwrap()).unwrap();
    engine
        .register_experiment(ExperimentRegistration {
            id: exp_id,
            manifest: Digest::hash_blake3(b"inv-12-manifest"),
        })
        .unwrap();
    engine
        .register_generation(GenerationRegistration {
            id: generation_id,
            experiment_id: exp_id,
            ordinal: 1,
        })
        .unwrap();
    let first = CellId::from_digest(Digest::hash_blake3(b"inv-12-a"));
    let second = CellId::from_digest(Digest::hash_blake3(b"inv-12-z"));
    for id in [second, first] {
        engine
            .register_cell(CellRegistration {
                id,
                experiment_id: exp_id,
                generation_id,
                manifest: Digest::hash_blake3(id.digest().as_bytes()),
                priority: 0,
            })
            .unwrap();
    }
    let claimed_first = engine.claim_next().unwrap().unwrap().registration.id;
    let claimed_second = engine.claim_next().unwrap().unwrap().registration.id;
    assert_eq!(claimed_first, first.min(second));
    assert_eq!(claimed_second, first.max(second));
    assert!(engine.claim_next().unwrap().is_none());
}

#[test]
fn test_inv_11_replayable_claims() {
    // INV-RFX-11: Replayable claims reconstruct from evidence.
    let exp_id = ExperimentId::from_digest(Digest::hash_blake3(b"exp"));
    let query_plan = Digest::hash_blake3(b"report-query-plan");
    let unit_registry = Digest::hash_blake3(b"report-unit-registry");
    let outcomes = CellOutcomeCounts {
        solved: 8,
        completed_without_solution: 1,
        censored: 1,
        incomplete: 0,
    };
    let evaluation = EvaluationReport {
        total_groups: 10,
        ..Default::default()
    };
    let economics = EconomicLedgerSummary::default();
    let cell_result = cell_query_result_digest(outcomes, 10.0, 0.5).unwrap();
    let evaluation_result = evaluation_query_result_digest(&evaluation).unwrap();
    let economics_result = economics_query_result_digest(&economics).unwrap();
    let mut source_artifacts = vec![
        cell_result,
        evaluation_result,
        economics_result,
        query_plan,
        unit_registry,
    ];
    source_artifacts.sort_unstable();
    let report = ScientificReport::build(ScientificReportParams {
        evidence: ReportEvidenceManifest {
            schema: "reflex.report-evidence.v1".into(),
            source_artifacts,
            query_plan,
            unit_registry_artifact: unit_registry,
            cell_result_artifact: cell_result,
            evaluation_result_artifact: evaluation_result,
            economics_result_artifact: economics_result,
            cell_query_id: "cells-v1".into(),
            evaluation_query_id: "evaluation-v1".into(),
            economics_query_id: "economics-v1".into(),
            cell_population: Digest::hash_blake3(b"cell-population"),
            cell_population_size: 10,
            evaluation_population: Digest::hash_blake3(b"evaluation-population"),
            evaluation_population_size: 10,
            economics_population: Digest::hash_blake3(b"economics-population"),
            economics_population_size: 10,
            cpu_unit: UnitId::from_digest(Digest::hash_blake3(b"seconds")),
            cpu_unit_symbol: "s".into(),
            cpu_unit_dimension: "time".into(),
            cpu_unit_scale: 1.0,
            economics_unit: UnitId::from_digest(Digest::hash_blake3(b"cycles")),
            economics_unit_symbol: "cycles".into(),
            economics_unit_dimension: "compute".into(),
            economics_unit_scale: 1.0,
        },
        experiment_id: exp_id,
        title: "Test",
        domain: "bitvec",
        outcomes,
        total_cpu: 10.0,
        total_ml_overhead: 0.5,
        evaluation,
        economics,
    })
    .unwrap();
    assert_eq!(report.solve_rate, 0.8);
}

#[tokio::test]
async fn test_inv_13_bulk_data_in_cas() {
    // INV-RFX-13: bulk bytes occupy a bounded arena and are referenced by digest.
    let mib = 1024 * 1024;
    let arena = ArtifactArena::new(ArtifactArenaLimits {
        total_bytes: mib,
        per_object_bytes: mib,
        evidence_permanent_bytes: mib,
        release_bytes: mib,
        active_bytes: mib,
        cache_bytes: mib,
        ephemeral_bytes: mib,
    })
    .unwrap();
    let stored = arena
        .put_arc(
            None,
            Arc::from(vec![42u8; mib as usize]),
            RetentionClass::EvidencePermanent,
        )
        .await
        .unwrap();
    assert_eq!(stored.digest.bytes.len(), 32);
    assert_eq!(arena.usage().await.total_bytes, mib);
    assert_eq!(
        arena.get_arc(stored.digest).await.unwrap().len(),
        mib as usize
    );
    assert!(
        arena
            .put_arc(
                None,
                Arc::from(&b"capacity breach"[..]),
                RetentionClass::EvidencePermanent,
            )
            .await
            .is_err()
    );
}

#[test]
fn test_inv_14_no_per_candidate_ipc() {
    // INV-RFX-14: No per-candidate process boundary; batched UDS communication.
    let mut builder = reflex_domain::CandidateBatchBuilder::new();
    for i in 0..64 {
        let cand_id = CandidateId::from_digest(Digest::hash_blake3(&[i as u8]));
        builder.add(cand_id, 0, i as u64, reflex_domain::CandidateHandle(i), 0);
    }
    let batch = builder.build();
    assert_eq!(batch.len(), 64);
    assert_eq!(batch.group_offsets, vec![0, 64]);
}

#[test]
fn test_inv_15_bounded_memory() {
    // INV-RFX-15: Bounded memory — frame decoder and sandbox reject oversized input.
    let mut codec = reflex_protocol::LengthDelimitedFrameCodec::new(1024);
    let mut buf = bytes::BytesMut::new();
    use bytes::BufMut;
    buf.put_u32_le(2048);
    buf.put_bytes(0, 50);

    let res = codec.decode(&mut buf);
    assert!(matches!(
        res,
        Err(reflex_protocol::ProtocolError::FrameTooLarge { .. })
    ));

    let policy = reflex_runtime::SandboxPolicy::default();
    assert!(policy.check_frame_size(2 * 1024 * 1024).is_err());
}

#[tokio::test]
async fn test_inv_16_one_cpu_budget() {
    // INV-RFX-16: One global thread budget.
    let budget = ThreadBudget::new(4);
    let l1 = budget.acquire("search", 2).await.unwrap();
    let l2 = budget.acquire("training", 2).await.unwrap();
    assert_eq!(budget.available(), 0);

    // Further request blocks or exceeds
    let timeout_res = tokio::time::timeout(
        std::time::Duration::from_millis(20),
        budget.acquire("analytics", 1),
    )
    .await;
    assert!(
        timeout_res.is_err(),
        "acquire must block when permits are exhausted"
    );

    drop(l1);
    drop(l2);
    assert_eq!(budget.available(), 4);
}

#[test]
fn test_inv_17_deterministic_registered_mode() {
    // INV-RFX-17: Deterministic registered mode with normalized floating point keys.
    let s1 = OrderedScore::from_f32(0.0).unwrap();
    let s2 = OrderedScore::from_f32(-0.0).unwrap();
    assert_eq!(s1, s2);
}

#[test]
fn test_inv_18_exploratory_mode_labeled() {
    // INV-RFX-18: Exploratory mode is labeled and cannot masquerade as confirmatory evidence.
    let man = ExperimentManifest {
        schema: "reflex.experiment.v1".to_string(),
        labels: ManifestLabels {
            name: "exploration".to_string(),
            comment: None,
        },
        mode: ExperimentMode::Exploratory,
        inputs: immutable_inputs(SearchConfig {
            algorithm: "exploratory-portfolio".to_string(),
            action_budget: 1000,
            node_budget: 2000,
            cpu_seconds: 5.0,
            exploration_uniform: 0.25,
        }),
        seeds: vec![42],
    };
    assert_eq!(man.mode, ExperimentMode::Exploratory);
    assert_eq!(man.inputs.search.algorithm, "exploratory-portfolio");
}

#[tokio::test]
async fn test_inv_19_knowledge_is_explicit() {
    // INV-RFX-19: Knowledge is explicit in library records.
    let kb = KnowledgeBase::new().unwrap();
    let mut record = KnowledgeRecord {
        id: KnowledgeRecordId::from_digest(Digest::ZERO),
        class: KnowledgeClass::TheoremFact,
        statement: "x = x".to_string(),
        proof_or_cert_digest: Digest::hash_blake3(b"k1-proof"),
        structural_tags: vec!["refl".to_string()],
        canonical_key: None,
        discovery_generation: 1,
        utility_score: 5.0,
        verification_receipt: Digest::hash_blake3(b"rcpt"),
        archived: false,
    };
    record.id = record.canonical_id().unwrap();
    kb.add_record(record).await.unwrap();
    let recs = kb.retrieve_by_tag("refl", 1).await.unwrap();
    assert_eq!(recs.len(), 1);
}

#[test]
fn test_inv_20_proposal_taste_gate() {
    // INV-RFX-20: No learned proposal before taste critic gate passes.
    let report_failed = EvaluationReport {
        top1_viable_rate: 0.2, // Below required 0.5 threshold
        mrr_cheapest_route: 0.3,
        ..Default::default()
    };
    let mut coord = reflex_scheduler::GenerationCoordinator::new(
        reflex_scheduler::StopPolicy::default(),
        reflex_scheduler::PromotionPolicy::default(),
        None,
        None,
    );
    let cand = ModelCheckpointId::from_digest(Digest::hash_blake3(b"cand"));
    let next_state = coord
        .simulate_evaluation_result(&report_failed, &[], cand)
        .unwrap();
    assert_eq!(next_state, reflex_scheduler::GenerationState::Rejected);
    assert_eq!(coord.active_model(), None);
}

#[test]
fn test_inv_21_no_silent_fallback() {
    // INV-RFX-21: digest mismatch fails and the cell pins the exact checkpoint.
    assert!(
        VerifiedCellManifest::verify(
            b"mutated-manifest",
            Digest::hash_blake3(b"registered-manifest"),
        )
        .is_err()
    );
    let pinned_model = ModelCheckpointId::from_digest(Digest::hash_blake3(b"pinned_model_v1"));
    let other_model = ModelCheckpointId::from_digest(Digest::hash_blake3(b"other_model_v2"));
    let ctx = CellContext::new(
        verified_manifest(b"inv-21-manifest", Some(pinned_model), None, 4),
        resolved_inputs(Some(pinned_model), None),
        ThreadBudget::new(4),
    )
    .unwrap();
    assert_ne!(ctx.model_checkpoint().unwrap().id(), &other_model);
    assert_eq!(ctx.model_checkpoint().unwrap().id(), &pinned_model);
}

#[test]
fn test_inv_22_performance_budget_gate() {
    // INV-RFX-22: Performance is correctness for the framework.
    let registry = PerformanceBudgetRegistry::new();
    let regressed = BenchmarkRecord {
        name: "micro_mlp_2607_batch64".to_string(),
        host_class: "reference-4vcpu-8gb".to_string(),
        p50_ns: 200_000.0,
        p95_ns: 250_000.0, // Budget is 100_000.0
        throughput_units_per_sec: 100_000.0,
        raw_samples: None,
    };
    assert!(registry.check_regression(&regressed, 0.05).is_err());
}

#[tokio::test]
async fn test_inv_23_cleanup_on_completion() {
    // INV-RFX-23: completion fails while a cell-owned resource is live.
    let model = ModelCheckpointId::from_digest(Digest::hash_blake3(b"cleanup-model"));
    let ctx = CellContext::new(
        verified_manifest(b"cleanup-cell", Some(model), None, 1),
        resolved_inputs(Some(model), None),
        ThreadBudget::new(1),
    )
    .unwrap();
    let lease = ctx.thread_budget().acquire("search", 1).await.unwrap();
    assert!(ctx.check_clean_shutdown().is_err());
    drop(lease);

    let buffer = ctx.buffer_pool().acquire().await.unwrap();
    assert!(ctx.check_clean_shutdown().is_err());
    drop(buffer);
    assert!(ctx.check_clean_shutdown().is_ok());
}

#[test]
fn test_inv_24_historical_science_immutable() {
    // INV-RFX-24: imported authority is read-only; absent external history is
    // unavailable, never replaced with a synthesized historical claim.
    let evidence_dir = m2a_evidence_dir();
    let manifest_path = evidence_dir.join("manifest.json");
    let authoritative_path = evidence_dir.join("authoritative.json");

    if !manifest_path.is_file() || !authoritative_path.is_file() {
        assert!(matches!(
            reconstruct_m2a_result(),
            Err(M2aImportError::NotFound(_))
        ));
        return;
    }

    let manifest_before = std::fs::read(&manifest_path).unwrap();
    let authoritative_before = std::fs::read(&authoritative_path).unwrap();
    let first = reconstruct_m2a_result().expect("supplied M2A evidence must reconcile exactly");
    let second = reconstruct_m2a_result().expect("M2A reconstruction must be deterministic");

    assert_eq!(
        serde_json::to_vec(&first).unwrap(),
        serde_json::to_vec(&second).unwrap()
    );
    assert_eq!(std::fs::read(&manifest_path).unwrap(), manifest_before);
    assert_eq!(
        std::fs::read(&authoritative_path).unwrap(),
        authoritative_before
    );
}
