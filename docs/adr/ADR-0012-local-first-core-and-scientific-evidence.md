# ADR 0012: Local-First Core and Scientific Evidence

## Status
Accepted

## Date
2026-08-20

## Context

The original architecture coupled the framework to a specific hosted fleet API and
required a repeated four-file evidence stamp for every implementation task.
Neither is product functionality. Provider-specific fleet orchestration makes
the core harder to test and maintain, while task stamps can appear complete
without establishing a scientific claim.

The durable requirements underneath those choices remain valid: workers must
use immutable inputs, fenced publication must reject stale attempts, cell-owned
resources must be reconciled before completion, and scientific claims must
reconstruct from immutable evidence.

This decision assigns INV-RFX-23 to
`reflex-runtime::CellContext::check_clean_shutdown`. The invariant test remains
`test_inv_23_cleanup_on_completion` and now exercises local resource ownership.

## Decision

1. Remove the hosted-fleet integration, provider-specific deployment files,
   lifecycle commands, object-store requirements, and provider phase from the
   core plan.
2. Keep portable worker execution, optional PostgreSQL fencing, and generic
   S3-compatible object storage behind existing interfaces. Deploying those
   components is an operator concern, not a core provider integration. This
   transitional decision is superseded for the v1 native path by ADR 0014.
3. Define INV-RFX-23 as zero live cell-owned resources at completion: compute
   permits and buffers are checked by `CellContext`; external worker processes
   and in-flight requests are drained by `DomainWorkerSupervisor`.
4. Replace the provider-specific fault canary with hermetic local multi-process
   and Turmoil tests for fencing, retry, publication order, and cleanup.
5. Remove the task manifest, junior-agent protocol, task-specific xtask
   commands, and `evidence/tasks/<ID>` stamps. The release gate is
   `cargo xtask check`; performance decisions use the checked-in budget
   registry; scientific artifacts live under `evidence/<experiment>/`.
6. A task card describes scope and acceptance criteria only. It is never
   evidence that those criteria passed.

## Consequences

- The core has no cloud-provider API or credentials and retains portable
  storage and coordination seams.
- Cleanup remains constitutional and becomes testable without external cloud
  state.
- Removing task stamps reduces maintenance and prevents passing-shaped
  bookkeeping from substituting for experiments.
- Canonical hardware claims and real external-domain campaigns remain explicit
  release gates until their raw artifacts and verifier receipts exist.

## Related

- Master-plan sections: §2, §3, §18, §20, §22, §23, and task cards P0/P11/P16.
- Invariants: INV-RFX-9, INV-RFX-10, INV-RFX-11, INV-RFX-21, INV-RFX-23.
- ADRs: 0003 (fenced leases), 0005 (SQLite single writer), 0014
  (memory-primary runtime and atomic evidence bundles; superseding authority).
