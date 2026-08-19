# Constitutional Invariants Traceability Matrix

Every invariant is release-blocking.

| ID | Title | Authority | Owner Crate | Enforcing API | Verification Test | Failure Mode |
|---|---|---|---|---|---|---|
| INV-RFX-1 | Verifier authority | §2, §10.5 | `reflex-domain` | `Verifier::verify` | `test_inv_verifier_authority` | Artifact rejected if verifier receipt missing or invalid |
| INV-RFX-2 | Immutable inputs | §2, §7.1 | `reflex-scheduler` | `CellManifest::cell_id` | `test_inv_immutable_inputs` | Manifest tampering alters cell ID and fails verification |
| INV-RFX-3 | No mutable model inside a cell | §2, §13.5 | `reflex-runtime` | `CellContext::pin_model` | `test_inv_no_mutable_model_inside_cell` | Active model pinned at cell launch, mid-cell promotion ignored |
| INV-RFX-4 | No mutable knowledge inside a cell | §2, §16.3 | `reflex-runtime` | `CellContext::pin_knowledge` | `test_inv_no_mutable_knowledge_inside_cell` | Running cell observes exactly one pinned base edition |
| INV-RFX-5 | Censored means unknown | §2, §14.4 | `reflex-dataset` | `DecisionGroup::from_events` | `test_inv_censored_means_unknown` | Budget exhaustion mapped to unknown/censored, never dead |
| INV-RFX-6 | Multiple valid routes survive | §2, §11.6 | `reflex-dataset` | `ProofDag::mark_route` | `test_inv_multiple_valid_routes` | All certified proof paths retain viable status |
| INV-RFX-7 | Raw utility is immutable | §2, §16.5 | `reflex-economics` | `UtilityObservation` | `test_inv_raw_utility_immutable` | Reward scalarization is derived and versioned |
| INV-RFX-8 | Search cost includes ML | §2, §3.2 | `reflex-search` | `SearchStats::ml_overhead` | `test_inv_search_cost_includes_ml` | Feature extraction and inference accounted in search CPU |
| INV-RFX-9 | No partial artifact publication | §2, §7.3 | `reflex-cas` | `ArtifactStore::put_stream` | `test_inv_no_partial_artifact_publication` | Atomic rename + fsync ensures only complete CAS objects exist |
| INV-RFX-10 | One accepted attempt | §2, §9.3 | `reflex-meta` | `MetaStore::finalize_attempt` | `test_inv_one_accepted_attempt` | Monotonic fencing token rejects stale attempts |
| INV-RFX-11 | Replayable claims | §2, §8.5 | `reflex-report` | `ReportGenerator::reconstruct` | `test_inv_replayable_claims` | Strict reports reconstruct from immutable CAS + ledgers |
| INV-RFX-12 | No shared SQLite | §2, §4.4 | `reflex-meta-sqlite` | `SqliteMetaStore` | `test_inv_no_shared_sqlite` | Local same-host metadata only; single writer actor |
| INV-RFX-13 | Bulk data stays out of metadata DBs | §2, §4.5 | `reflex-meta` | `MetaStore::publish_artifact_refs` | `test_inv_bulk_data_in_cas` | Proofs, ledgers, checkpoints stored in CAS by digest |
| INV-RFX-14 | No per-candidate process boundary | §2, §10.4 | `reflex-protocol` | `DomainClient::expand_batch` | `test_inv_no_per_candidate_ipc` | Batch state expansion and candidate scoring |
| INV-RFX-15 | Bounded memory and queues | §2, §3.2 | `reflex-runtime` | `BufferPool`, `ThreadBudget` | `test_inv_bounded_memory` | Bounded capacity and backpressure across queues and pools |
| INV-RFX-16 | One CPU budget | §2, §3.5 | `reflex-runtime` | `ThreadBudget::acquire` | `test_inv_one_cpu_budget` | Single thread permit broker across compute tasks |
| INV-RFX-17 | Deterministic registered mode | §2, §11.3 | `reflex-search` | `FrontierKey::order` | `test_inv_deterministic_registered_mode` | Tie breaks, seeds, and candidate ordering explicit and stable |
| INV-RFX-18 | Exploratory mode is labeled | §2, §15.2 | `reflex-scheduler` | `ExperimentManifest` | `test_inv_exploratory_mode_labeled` | Dynamic overlays cannot masquerade as confirmatory evidence |
| INV-RFX-19 | Knowledge is explicit | §2, §16.1 | `reflex-knowledge` | `KnowledgeEdition` | `test_inv_knowledge_is_explicit` | New facts enter library records, not implicit weight memory |
| INV-RFX-20 | No learned proposal before taste gate | §2, §17.5 | `reflex-scheduler` | `ProposalGate::evaluate` | `test_inv_proposal_taste_gate` | Learned generation disabled until critic passes fixed pool gate |
| INV-RFX-21 | No silent fallback | §2, §10.2 | `reflex-runtime` | `CellContext::validate` | `test_inv_no_silent_fallback` | Resource or model substitutions require manifest authority |
| INV-RFX-22 | Performance is correctness | §2, §3.4 | `reflex-bench` | `PerformanceBudgetRegistry` | `test_inv_performance_budget_gate` | Regressions beyond accepted budget fail fast/promotion gate |
| INV-RFX-23 | Cleanup is part of completion | §2, §18.6 | `reflex-fly` | `FleetController::cleanup` | `test_inv_cleanup_on_completion` | Distributed runs verify zero orphaned Machines/volumes |
| INV-RFX-24 | Historical science remains historical | §2, §21 | `domains/reflex-domain-lean` | `LeanMigration` | `test_inv_historical_science_immutable` | Prior Project Reflex artifacts and findings are immutable |
