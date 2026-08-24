# Lean public-path optimizer Development v21 activation-null result

## Result

The restart-complete pagination follow-up still stopped at the 32-claim training activation gate, before counterfactual treatments. It generated 460 Candidates, selected 48, kernel-Verified 40, admitted two strict Proof Collapses, and promoted no Model Revision. The result isolates a producer-consumer scheduling defect beyond v20's lost enumeration tail: generation compounded an already large deferred Candidate inventory before its next Verification Cohort.

The checked-in [machine summary](./lean-public-optimizer-development-v21-null-summary.json) records the activation-null result. No complete harness report exists because the registered gate rejected a placebo Full-versus-no-model comparison. The retained training Domain Bundle remains at `/tmp/reflex-lean-public-optimizer-development-v21-32x8-work/training.bundle` with SHA-256 `fe27c0460b6b5b2fa39899704f8f90d75e754a939912f67c38a89cf0fb051997`; its training phase file has SHA-256 `ae6e3b9c421db5688b21ba12a9e75f6fd3e42206df376f8d22d7c8b33311f077`.

## Diagnosis

The first cohort supplied complete uncovered-claim coverage: 32 protected-origin checks plus eight ordinary Bootstrap checks. It left 229 policy-deferred Candidates and 47 novelty-filtered Fates. The next epoch selected another eight Candidates, but generated another Primitive page before dispatch. The conservative resident reservation then refused the combined deferred, fresh, selection, and checkpoint pipeline, so those eight never crossed the kernel.

The Session used 18.396 GB of its 32 GiB Runtime allowance, 78.5 MB of its 1 GiB durable allowance, about 46.8 seconds of ten minutes, and 72 of 1,024 total Verification requests. Low observed peak does not make the refusal spurious: the Runtime stopped before allocating the estimated heavyweight transient pool. Weakening that check would trade a diagnosed scheduling imbalance for another host-memory failure.

Pagination did change search behavior as intended: v21 generated more Candidates than v20, retained restart-complete later pages, and no longer classified an empty key-only tail as the cause. It was insufficient because Verification drained eight choices per later cohort while generation attempted to refill its entire breadth target on every epoch.

## Decision

ADR 0071 turns the generation target into a bounded inventory watermark. Existing deferred Candidates count toward it; generation resumes only for the deficit after Verification drains the pool. No Candidate is dropped, no envelope is enlarged, and pending Enumeration Cursors remain durable.

Preserve v21 as a second activation-null. Advance to Runtime revision 16 and Development schema v22, run the full correctness suite, then rerun the identical 32×8 corpus once. Do not increase model capacity.

This is diagnosed Development evidence, not Scientific Confirmation.
