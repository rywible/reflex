# Lean public-path optimizer Development v22 activation-null result

## Result

Deferred-inventory backpressure worked: 32-claim training ran 41 search epochs and kernel-Verified 352 Candidates, compared with 40 in v21. It admitted 25 strict Proof Collapses spanning 23 correctness claims. The activation gate still found no promoted Model Revision and correctly skipped every counterfactual treatment.

The checked-in [machine summary](./lean-public-optimizer-development-v22-null-summary.json) records the activation-null result. No complete harness report exists because the registered gate rejected a placebo comparison. The retained training Domain Bundle remains at `/tmp/reflex-lean-public-optimizer-development-v22-32x8-work/training.bundle` with SHA-256 `b8a70dba48d81d68f826ea3fc92535984f0e806f18d45a57cf726282f30bf917`; its training phase file has SHA-256 `55c05af31b775c44efeb3713739c22a2ed4de2090ab3c532d129795b3a942295`.

## What improved

Training used 384 total Verification requests: 32 Seed replays and 352 Candidate checks. After the 32 protected-origin checks, Bootstrap supplied 320 ordinary checks. Generation produced 601 Candidates; selection drained 360, with the final eight selected but not dispatched. Candidate Experience covered all 32 claims, included 25 Accepted and 327 Refuted outcomes, and retained 789 Candidate Fates.

This is enough to reject the v21 producer-consumer failure. Generation no longer compounded the full breadth target every epoch; the Runtime spent about five times as many Candidate requests and admitted more than twelve times as many Artifacts.

## Remaining stop

The final cohort stopped at resident admission. Observed peak residency was 19.734 GB under the 32 GiB Runtime allowance, durable use was 90.7 MB under 1 GiB, and CPU and elapsed use were about 110 seconds under ten minutes. The low observed peak is expected because the refused allocation never occurred.

Code inspection found a false overlap in the predictive reservation. `resident_before_epoch` charged every deferred Artifact payload. Selection then moved those same payloads into the selected and prospective-deferred vectors without cloning their dynamic heaps, but the transaction added the complete prospective payload again. Once proof terms, admitted Artifacts, and Experience grew, this impossible double ownership crossed the envelope.

ADR 0072 recomputes the post-selection ownership graph. It still separately charges element buffers, the selected Candidate pipeline, Candidate Fates, generation context, both coexisting checkpoints, and pending durability. Only the moved Artifact payload's duplicate charge is removed.

## Decision

Preserve v22 as an activation-null. Advance to Runtime revision 17 and Development schema v23, validate resource and crash semantics, then make one identical retry. If v23 reaches the registered training boundary without promotion, treat model activation as the result and audit the ranking/calibration gate rather than changing search capacity.

This is diagnosed Development evidence, not Scientific Confirmation.
