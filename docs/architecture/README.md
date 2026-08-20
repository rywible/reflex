# Architecture

This directory describes the Reflex architecture as built. It is the
companion to the master plan
(`docs/reflex-framework-master-plan-rust.md`); where the plan is
aspirational, this document describes the code as it exists and marks
planned items explicitly.

## Components

| Component | Crate | Role |
|---|---|---|
| Reflex | `bins/reflex` | CLI: cell runs, training, verification, reporting |
| Reflex Analytics | `bins/reflex-analytics` | Parquet/DataFusion analytics (isolated per ADR 0004) |
| Domain | `reflex-domain` | Domain trait, verifier authority, transition outcomes |
| Runtime | `reflex-runtime` | Cell context, thread budget, process-tree accounting |
| Scheduler | `reflex-scheduler` | Cell manifests, promotion policy, experiment manifests |
| Search | `reflex-search` | Frontier search, proof DAG, search stats (incl. ML) |
| Dataset | `reflex-dataset` | Decision graphs, recovered segments, label finalization |
| Artifact arena | `reflex-cas` | Bounded content-addressed memory-primary artifacts and bundle publication |
| Ledger | `reflex-ledger` | Append-only segment ledger with CRC integrity |
| Engine | `reflex-engine` | Single-owner local run state and attempt-epoch fencing |
| Knowledge | `reflex-knowledge` | Knowledge records and editions |
| Protocol | `reflex-protocol` | Domain wire protocol, framing, batching |
| ML | `reflex-ml-burn`, `reflex-ml-core`, `reflex-ml-micro` | Inference/checkpoint tiers |

## Data Flow (search loop)

1. Domain produces candidate batches (`CandidateBatch`) and transitions
   (`TransitionOutcome`), scored under a `SearchBudget`.
2. `reflex-search` explores under the budget, accounting ML overhead in
   `SearchStats::ml_overhead_ns` (INV-RFX-8); verified routes are marked
   in the `ProofDag` (INV-RFX-6).
3. Events are appended to bounded ledger buffers (`reflex-ledger`, segment
   format in `docs/storage/`); immutable artifacts enter `ArtifactArena` by
   digest and publish durably only in an atomic evidence bundle (INV-RFX-9/13).
4. `reflex-dataset` ingests recovered segments into decision graphs and
   finalizes labels; `reflex-knowledge` records explicit facts
   (INV-RFX-19).
5. `reflex-scheduler` evaluates promotion via
   `PromotionPolicy::evaluate_promotion` (INV-RFX-20) and records the
   result through attempt-epoch-checked `LocalRunState::finalize`
   (INV-RFX-10/12).
6. `reflex-report` accepts only evidence-bound reconstructions naming source
   artifacts, checked query IDs/plans, population, and units, then derives a
   canonical report identity (`ScientificReport::to_canonical_json`,
   INV-RFX-11).

## Runtime Boundaries

- Every cell pins model and knowledge at launch (`CellContext::new`,
  INV-RFX-3/4); no mid-cell substitution.
- A single `ThreadBudget` brokers CPU permits (INV-RFX-16); `BufferPool`
  bounds buffer reuse (INV-RFX-15).
- One in-memory state machine owns cell transitions and monotonic attempt
  epochs (INV-RFX-12); bulk data occupies a bounded arena (INV-RFX-13).
- Native v1 is one process (ADR 0014). A cell cannot complete until
  its permits, buffers, child processes, and in-flight requests reconcile to
  zero (INV-RFX-23, ADR 0012).
- External domains and verifiers use the fail-closed boundary documented in
  [`docs/security/untrusted-domains.md`](../security/untrusted-domains.md).

The v1 closure command and required scientific reports are documented in
[`docs/release-v1.md`](../release-v1.md).

## Binary Surface

Each bin embeds build metadata via `build.rs` (git commit + dirty marker,
target triple, profile) and the schema-compat line
`arena:<n> bundle:<n> ledger:<n> proto:<n>` (ADRs 0009 and 0014); `--version`
prints it. All bins and test harnesses are `#![forbid(unsafe_code)]`;
exceptions are tracked in
`unsafe-allowlist.txt` and enforced by `scripts/check-unsafe.sh`.

## Planned Items

- DataFusion-backed analytics queries beyond shard evaluation (analytics
  bin exists; query surface grows per master plan §22.3).
- GA release of the Burn checkpoint migration (ADR 0006 expiry).
- `publish = false` on member crates (ADR 0010 gap).
