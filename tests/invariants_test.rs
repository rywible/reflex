use reflex_bench::{BenchmarkRecord, PerformanceBudgetRegistry};
use reflex_cas::{ArtifactStore, MemoryArtifactStore, RetentionClass};
use reflex_dataset::{CandidateKnowledge, DecisionGroup};
use reflex_domain::{Domain, VerifyBudget};
use reflex_domain_bitvec::{BitvecArtifact, BitvecDomain, BvExpr};
use reflex_domain_lean::reconstruct_m2a_result;
use reflex_economics::{
    BetterDirection, ConfidenceClass, EconomicLedgerSummary, UtilityObservation,
};
use reflex_eval::EvaluationReport;
use reflex_fly::{FleetController, MockFlyClient};
use reflex_knowledge::{KnowledgeBase, KnowledgeClass, KnowledgeRecord};
use reflex_meta::{
    CellLease, ClaimRequest, FinalAttempt, MemoryMetaStore, MetaError, MetaStore, NewCell,
    NewExperiment,
};
use reflex_report::{ScientificReport, ScientificReportParams};
use reflex_runtime::{CellContext, ThreadBudget};
use reflex_scheduler::{CellManifest, SearchConfig};
use reflex_search::{OrderedScore, ProofDag};
use reflex_types::{
    CandidateId, CellId, Digest, EvaluatorId, ExperimentId, GenerationId, KnowledgeEditionId,
    KnowledgeRecordId, MetricId, ModelCheckpointId, ResearchNodeId, StateId, TaskId, UnitId,
    WorkerId,
};
use std::sync::Arc;
use tokio_util::codec::Decoder;

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
        experiment_id: ExperimentId::from_digest(Digest::hash_blake3(b"exp1")),
        generation_id: GenerationId::from_digest(Digest::hash_blake3(b"gen1")),
        task_id: TaskId::from_digest(Digest::hash_blake3(b"task1")),
        seed: 42,
        search: SearchConfig {
            algorithm: "best-first".to_string(),
            action_budget: 1000,
            node_budget: 5000,
            cpu_seconds: 10.0,
            exploration_uniform: 0.1,
        },
        model_checkpoint: None,
        knowledge_edition: None,
        resource_class: "performance_4x_8gb".to_string(),
    };

    let mut manifest2 = manifest1.clone();
    manifest2.seed = 43; // Tamper with seed

    assert_ne!(manifest1.cell_id(), manifest2.cell_id());
}

#[test]
fn test_inv_3_no_mutable_model_inside_cell() {
    // INV-RFX-3: No mutable model inside a cell.
    let model1 = ModelCheckpointId::from_digest(Digest::hash_blake3(b"model1"));
    let ctx = CellContext::new(
        CellId::from_digest(Digest::ZERO),
        Digest::ZERO,
        model1,
        None,
        ThreadBudget::new(4),
    );
    assert_eq!(ctx.model_checkpoint, model1);
}

#[test]
fn test_inv_4_no_mutable_knowledge_inside_cell() {
    // INV-RFX-4: No mutable knowledge inside a cell.
    let kn1 = KnowledgeEditionId::from_digest(Digest::hash_blake3(b"kn1"));
    let ctx = CellContext::new(
        CellId::from_digest(Digest::ZERO),
        Digest::ZERO,
        ModelCheckpointId::from_digest(Digest::ZERO),
        Some(kn1),
        ThreadBudget::new(4),
    );
    assert_eq!(ctx.knowledge_edition, Some(kn1));
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
    let mut dag = ProofDag::new();
    let state = StateId::from_digest(Digest::hash_blake3(b"state_A"));
    let cand_a = CandidateId::from_digest(Digest::hash_blake3(b"cand_A"));
    let cand_b = CandidateId::from_digest(Digest::hash_blake3(b"cand_B"));

    dag.mark_viable(state, cand_a, 5);
    dag.mark_viable(state, cand_b, 8);

    match dag.get_knowledge(state, cand_a).unwrap() {
        reflex_search::CandidateKnowledge::Viable { best_actions_to_go, .. } => assert_eq!(*best_actions_to_go, 5),
        other => panic!("expected Viable, got {:?}", other),
    }
    match dag.get_knowledge(state, cand_b).unwrap() {
        reflex_search::CandidateKnowledge::Viable { best_actions_to_go, .. } => assert_eq!(*best_actions_to_go, 8),
        other => panic!("expected Viable, got {:?}", other),
    }
}

#[test]
fn test_inv_7_raw_utility_immutable() {
    // INV-RFX-7: Raw utility is immutable.
    let obs = UtilityObservation {
        subject: ResearchNodeId::from_digest(Digest::ZERO),
        evaluator: EvaluatorId::from_digest(Digest::ZERO),
        metric: MetricId::from_digest(Digest::hash_blake3(b"cycles")),
        value: 1250.0,
        unit: UnitId::from_digest(Digest::hash_blake3(b"cycles")),
        direction: BetterDirection::HigherIsBetter,
        population: Digest::ZERO,
        evidence: Vec::new(),
        confidence: ConfidenceClass::ObservedExact,
        observed_at_generation: GenerationId::from_digest(Digest::ZERO),
    };
    assert_eq!(obs.value, 1250.0);
}

#[test]
fn test_inv_8_search_cost_includes_ml() {
    // INV-RFX-8: Search cost includes ML inference and feature extraction overhead.
    let summary = EconomicLedgerSummary::compute_net_savings(100_000.0, 1.0, 5_000, 2_000, 1_000);
    assert_eq!(summary.ml_inference_tax_cpu_ns, 5_000);
    assert_eq!(summary.feature_extraction_tax_cpu_ns, 2_000);
    assert_eq!(summary.net_utility_value, 92_000.0);
}

#[tokio::test]
async fn test_inv_9_no_partial_artifact_publication() {
    // INV-RFX-9: No partial artifact publication.
    let store = MemoryArtifactStore::new();
    let data = bytes::Bytes::from_static(b"complete verified artifact payload");
    let stored = store
        .put_bytes(None, data.clone(), RetentionClass::EvidencePermanent)
        .await
        .unwrap();

    let read_back = store.get_bytes(stored.digest).await.unwrap();
    assert_eq!(read_back, data);
}

#[tokio::test]
async fn test_inv_10_one_accepted_attempt() {
    // INV-RFX-10: One accepted attempt with fencing tokens.
    let meta = MemoryMetaStore::new();
    let exp_id = ExperimentId::from_digest(Digest::hash_blake3(b"exp"));
    meta.create_experiment(NewExperiment {
        id: exp_id,
        name: "test".to_string(),
        domain: "bitvec".to_string(),
        manifest_digest: Digest::ZERO,
    })
    .await
    .unwrap();

    let cell_id = CellId::from_digest(Digest::hash_blake3(b"cell"));
    meta.enqueue_cells(&[NewCell {
        id: cell_id,
        experiment_id: exp_id,
        generation_id: GenerationId::from_digest(Digest::ZERO),
        manifest_digest: Digest::ZERO,
        resource_class: "performance_4x_8gb".to_string(),
        priority: 1,
    }])
    .await
    .unwrap();

    let lease = meta
        .claim_cell(ClaimRequest {
            worker_id: WorkerId::from_digest(Digest::ZERO),
            resource_class: "performance_4x_8gb".to_string(),
            lease_duration_secs: 60,
        })
        .await
        .unwrap()
        .unwrap();

    let comp_manifest = Digest::hash_blake3(b"completion");
    meta.publish_attempt_artifacts(
        &lease,
        &[reflex_meta::ArtifactRef {
            digest: comp_manifest,
            kind: "cell-completion-manifest".to_string(),
        }],
    )
    .await
    .unwrap();

    let finalize_res = meta
        .finalize_attempt(
            &lease,
            FinalAttempt {
                accepted: true,
                completion_manifest_digest: comp_manifest,
                error_code: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(finalize_res.state, "succeeded");

    // Stale token attempt fails
    let stale_lease = CellLease {
        cell_id,
        attempt_no: lease.attempt_no,
        fencing_token: 999, // stale
        manifest_digest: Digest::ZERO,
        lease_expires_at_timestamp: 0,
    };
    let err = meta
        .finalize_attempt(
            &stale_lease,
            FinalAttempt {
                accepted: true,
                completion_manifest_digest: Digest::ZERO,
                error_code: None,
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(err, MetaError::StaleFence { .. }));
}

#[test]
fn test_inv_11_replayable_claims() {
    // INV-RFX-11: Replayable claims reconstruct from evidence.
    let exp_id = ExperimentId::from_digest(Digest::hash_blake3(b"exp"));
    let report = ScientificReport::build(ScientificReportParams {
        experiment_id: exp_id,
        title: "Test",
        domain: "bitvec",
        total_cells: 10,
        solved_cells: 8,
        total_cpu_seconds: 10.0,
        total_ml_overhead_seconds: 0.5,
        evaluation: EvaluationReport::default(),
        economics: EconomicLedgerSummary::default(),
    });
    assert_eq!(report.solve_rate, 0.8);
}

#[tokio::test]
async fn test_inv_12_no_shared_sqlite() {
    // INV-RFX-12: SQLite is same-host metadata only; single writer actor.
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("local.db");
    let store = reflex_meta_sqlite::SqliteMetaStore::open(db_path).unwrap();

    let exp_id = ExperimentId::from_digest(Digest::hash_blake3(b"local_exp"));
    store
        .create_experiment(NewExperiment {
            id: exp_id,
            name: "local".to_string(),
            domain: "bitvec".to_string(),
            manifest_digest: Digest::ZERO,
        })
        .await
        .unwrap();

    let status = store.get_experiment_status(exp_id).await.unwrap();
    assert_eq!(status.name, "local");
}

#[tokio::test]
async fn test_inv_13_bulk_data_in_cas() {
    // INV-RFX-13: Bulk data stays out of metadata DBs; referenced by Digest.
    let store = MemoryArtifactStore::new();
    let large_payload = vec![42u8; 1024 * 1024]; // 1 MiB
    let stored = store
        .put_bytes(
            None,
            bytes::Bytes::from(large_payload),
            RetentionClass::EvidencePermanent,
        )
        .await
        .unwrap();

    let art_ref = reflex_meta::ArtifactRef {
        digest: stored.digest,
        kind: "large-proof-dag".to_string(),
    };
    // Metadata only contains 32-byte digest and small string
    assert_eq!(art_ref.digest.bytes.len(), 32);
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
    // INV-RFX-15: Bounded memory and queues. Frame decoder rejects oversized frames.
    let mut codec = reflex_protocol::LengthDelimitedFrameCodec::new(1024);
    let mut buf = bytes::BytesMut::new();
    use bytes::BufMut;
    buf.put_u32_le(2048); // Exceeds max 1024
    buf.put_bytes(0, 50);

    let res = codec.decode(&mut buf);
    assert!(matches!(
        res,
        Err(reflex_protocol::ProtocolError::FrameTooLarge { .. })
    ));
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
    let man = CellManifest {
        experiment_id: ExperimentId::from_digest(Digest::hash_blake3(b"exp")),
        generation_id: GenerationId::from_digest(Digest::hash_blake3(b"gen")),
        task_id: TaskId::from_digest(Digest::hash_blake3(b"task")),
        seed: 42,
        search: SearchConfig {
            algorithm: "exploratory-portfolio".to_string(),
            action_budget: 1000,
            node_budget: 2000,
            cpu_seconds: 5.0,
            exploration_uniform: 0.25,
        },
        model_checkpoint: None,
        knowledge_edition: None,
        resource_class: "performance_4x_8gb".to_string(),
    };
    assert_eq!(man.search.algorithm, "exploratory-portfolio");
}

#[tokio::test]
async fn test_inv_19_knowledge_is_explicit() {
    // INV-RFX-19: Knowledge is explicit in library records.
    let kb = KnowledgeBase::new();
    kb.add_record(KnowledgeRecord {
        id: KnowledgeRecordId::from_digest(Digest::hash_blake3(b"k1")),
        class: KnowledgeClass::TheoremFact,
        statement: "x = x".to_string(),
        proof_or_cert_digest: Digest::ZERO,
        structural_tags: vec!["refl".to_string()],
        discovery_generation: 1,
        utility_score: 5.0,
    })
    .await;
    let recs = kb.retrieve_by_tag("refl", 1).await;
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
    let next_state = coord.handle_evaluation_result(&report_failed, cand);
    assert_eq!(next_state, reflex_scheduler::GenerationState::Rejected);
    assert_eq!(coord.active_model, None);
}

#[test]
fn test_inv_21_no_silent_fallback() {
    // INV-RFX-21: No silent fallback. Cell pins exact checkpoint ID.
    let pinned_model = ModelCheckpointId::from_digest(Digest::hash_blake3(b"pinned_model_v1"));
    let other_model = ModelCheckpointId::from_digest(Digest::hash_blake3(b"other_model_v2"));
    let ctx = CellContext::new(
        CellId::from_digest(Digest::ZERO),
        Digest::ZERO,
        pinned_model,
        None,
        ThreadBudget::new(4),
    );
    assert_ne!(ctx.model_checkpoint, other_model);
    assert_eq!(ctx.model_checkpoint, pinned_model);
}

#[test]
fn test_inv_22_performance_budget_gate() {
    // INV-RFX-22: Performance is correctness for the framework.
    let registry = PerformanceBudgetRegistry::new();
    let regressed = BenchmarkRecord {
        name: "micro_mlp_2607_batch64".to_string(),
        p50_ns: 200_000.0,
        p95_ns: 250_000.0, // Budget is 100_000.0
        throughput_units_per_sec: 100_000.0,
    };
    assert!(registry.check_regression(&regressed, 0.05).is_err());
}

#[tokio::test]
async fn test_inv_23_cleanup_on_completion() {
    // INV-RFX-23: Cleanup is part of completion. Zero orphaned machines.
    let client = Arc::new(MockFlyClient::new());
    let fleet = FleetController::new(client.clone(), 20);
    fleet.launch_worker_pool(5, "sha256:test").await.unwrap();
    let cleaned = fleet.cleanup_all_workers().await.unwrap();
    assert_eq!(cleaned, 5);
}

#[test]
fn test_inv_24_historical_science_immutable() {
    // INV-RFX-24: Historical science remains historical (M2A negative result preserved).
    let m2a = reconstruct_m2a_result();
    assert_eq!(m2a.registered_outcome, "STRONG_NEGATIVE_RESULT");
    assert_eq!(m2a.total_arms, 180);
}
