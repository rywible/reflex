# Constitutional Invariants Traceability Matrix

Every invariant is release-blocking. A change that weakens, removes, or
silently skips an invariant fails verification. Each row names the exact
enforcing API and the exact test that gates the invariant; the
performance-measurement column lists the budget/measurement that must be
re-verified when the enforcing code path changes (see
`docs/performance/`).

| ID | Title | Authority | Owner Crate | Enforcing API | Verification Test | Performance Measurement |
|---|---|---|---|---|---|---|
| INV-RFX-1 | Verifier authority | §2, §10.5 | `reflex-domain` | `Verifier::verify` | `test_inv_1_verifier_authority` | verify latency p99 per artifact (§3.1) |
| INV-RFX-2 | Immutable inputs | §2, §7.1 | `reflex-scheduler` | `CellManifest::cell_id` | `test_inv_2_immutable_inputs` | manifest digest recompute cost (§3.3) |
| INV-RFX-3 | No mutable model inside a cell | §2, §13.5 | `reflex-runtime` | `CellContext::new` | `test_inv_3_no_mutable_model_inside_cell` | cell launch path latency (§3.3) |
| INV-RFX-4 | No mutable knowledge inside a cell | §2, §16.3 | `reflex-runtime` | `CellContext::new` (`knowledge_edition`) | `test_inv_4_no_mutable_knowledge_inside_cell` | cell launch path latency (§3.3) |
| INV-RFX-5 | Censored means unknown | §2, §14.4 | `reflex-domain` | `TransitionOutcome::{is_accepted,is_censored}`, `DecisionGraph::finalize_labels` | `test_inv_5_censored_means_unknown` | labeling throughput (§3.4) |
| INV-RFX-6 | Multiple valid routes survive | §2, §11.6 | `reflex-dataset` | `DecisionGraph::record_verified_route`, `ProofDag::mark_verified_route` | `test_inv_6_multiple_valid_routes_survive` | route marking cost per edge (§3.1) |
| INV-RFX-7 | Raw utility is immutable | §2, §16.5 | `reflex-economics` | `UtilityObservation` (derived, versioned scalars) | `test_inv_7_raw_utility_immutable` | scalarization compute per observation (§3.4) |
| INV-RFX-8 | Search cost includes ML | §2, §3.2 | `reflex-search` | `SearchStats::ml_overhead_ns` | `test_inv_8_search_cost_includes_ml` | search CPU per node incl. ML (§3.1) |
| INV-RFX-9 | No partial evidence publication | §2, §7.3 | `reflex-cas` | `LocalEvidenceBundleStore::commit` | `test_inv_9_no_partial_artifact_publication` | bundle snapshot throughput and barrier p99 (§3.3) |
| INV-RFX-10 | One accepted attempt | §2, §9.3 | `reflex-engine` | `LocalRunState::finalize` (`AttemptTicket`) | `test_inv_10_one_accepted_attempt` | in-memory finalize p99 (§3.3) |
| INV-RFX-11 | Replayable claims | §2, §8.5 | `reflex-report` | `ScientificReport::to_canonical_json` | `test_inv_11_replayable_claims` | report serialization cost (§3.4) |
| INV-RFX-12 | Single-process coordination authority | §2, §4.4 | `reflex-engine` | `LocalRunState::{claim_next,finalize}` | `test_inv_12_no_shared_sqlite` | in-memory claim/finalize latency (§3.3) |
| INV-RFX-13 | Bounded artifact arena | §2, §7.3 | `reflex-cas` | `ArtifactArena::{put_arc,get_arc}` | `test_inv_13_bulk_data_in_cas` | arena resident bytes and insertion throughput (§3.3) |
| INV-RFX-14 | No per-candidate process boundary | §2, §10.4 | `reflex-protocol` | `DomainClient::expand_batch` | `test_inv_14_no_per_candidate_ipc` | batch expansion latency p99 (§3.1) |
| INV-RFX-15 | Bounded memory and queues | §2, §3.2 | `reflex-runtime` | `BufferPool::{acquire,release}`, `ThreadBudget::acquire` | `test_inv_15_bounded_memory` | peak RSS, queue depth (§3.1) |
| INV-RFX-16 | One CPU budget | §2, §3.5 | `reflex-runtime` | `ThreadBudget::acquire` | `test_inv_16_one_cpu_budget` | thread permit latency (§3.1) |
| INV-RFX-17 | Deterministic registered mode | §2, §11.3 | `reflex-search` | `FrontierKey: Ord` (cmp) | `test_inv_17_deterministic_registered_mode` | frontier ordering cost per node (§3.1) |
| INV-RFX-18 | Exploratory mode is labeled | §2, §15.2 | `reflex-scheduler` | `ExperimentManifest::mode`, `CellManifest::inputs` | `test_inv_18_exploratory_mode_labeled` | overlay labeling cost (§3.4) |
| INV-RFX-19 | Knowledge is explicit | §2, §16.1 | `reflex-knowledge` | `KnowledgeBase::{add_record,create_edition}` | `test_inv_19_knowledge_is_explicit` | edition write latency (§3.4) |
| INV-RFX-20 | No learned proposal before taste gate | §2, §17.5 | `reflex-scheduler` | `PromotionPolicy::evaluate_promotion` | `test_inv_20_proposal_taste_gate` | promotion gate latency (§3.4) |
| INV-RFX-21 | No silent fallback | §2, §10.2 | `reflex-runtime` | `VerifiedCellManifest::verify`, `CellContext::new` | `test_inv_21_no_silent_fallback` | fallback path absence check (§3.3) |
| INV-RFX-22 | Performance is correctness | §2, §3.4 | `reflex-bench` | `PerformanceBudgetRegistry` | `test_inv_22_performance_budget_gate` | per-benchmark budget table (§3.4) |
| INV-RFX-23 | Cleanup is part of completion | §2, §18.2 | `reflex-runtime`, `reflex-domain-host` | `CellContext::check_clean_shutdown`, `DomainWorkerSupervisor::shutdown_and_drain` | `test_inv_23_cleanup_on_completion` | cell resource reconciliation latency (§3.4) |
| INV-RFX-24 | Historical science remains historical | §2, §21 | `reflex-domain-lean` | `LeanDomain` artifact receipts, `reconstruct_m2a_result` | `test_inv_24_historical_science_immutable` | reconstruction latency (§3.4) |

## Enforcement

- **Gate**: every row's verification test runs in `cargo xtask check` and
  `cargo test --workspace`; a failing invariant blocks release.
- **Performance**: rows list the measurement that must be re-run when the
  enforcing API changes; budgets live in `docs/performance/` and
  `PerformanceBudgetRegistry`.
- **Traceability**: master-plan section references (§N) point at
  `docs/reflex-framework-master-plan-rust.md`; changes to that document
  must not contradict a row without an ADR.
- **Evidence**: `cargo xtask check` and `tests/invariants_test.rs` are
  the release gate; `evidence/invariants/report.json` maps each row to
  its test. Scientific experiment logs live under `evidence/` by name
  (ledgers, campaigns, reconstructions) — not as per-task stamp files.
- **Native topology**: ADR 0014 makes the single-process, memory-primary path
  authoritative for v1. SQL and remote object-store adapters cannot satisfy an
  invariant or release gate in place of the in-memory state machine,
  `ArtifactArena`, and atomic evidence bundle.

## Change Process

1. An invariant row may only change via an ADR that names the invariant,
   the old and new enforcing API, and the verification test.
2. The ADR must be approved before the code change; the code change and
   ADR land in the same release.
3. Any commit that touches an enforcing API must re-run the row's test.
   Invariant coverage is recorded in `evidence/invariants/report.json`.
