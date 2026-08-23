# Lean public-path optimizer Development v20 activation-null result

## Result

The first 32-training-claim, eight-held-out-claim breadth treatment stopped at the activation gate, before any counterfactual treatment ran. Training admitted three kernel-Verified Proof Collapses from 48 Candidate Verification requests but promoted no Model Revision. The harness correctly rejected a Full-versus-no-model comparison that would have been a placebo.

This was not evidence that broader Lean Experience fails to train a useful model. It exposed a Runtime search-completeness defect: bounded generation retained only the first page of each Primitive Operator's legal Applications and then removed the processed parent from pending work. Correct proof donors later in deterministic Premise Retrieval order were therefore unavailable to later Verification Cohorts. The v21 follow-up established a second interacting defect: generation compounded a large deferred inventory before draining it through the next cohort, causing the conservative pre-Verification residency check to refuse that cohort.

The checked-in [machine summary](./lean-public-optimizer-development-v20-null-summary.json) records the activation-null result. No complete harness report exists because the registered activation gate terminated report construction. The retained training Domain Bundle remains at `/tmp/reflex-lean-public-optimizer-development-v20-32x8-work/training.bundle` with SHA-256 `170dcecddfd2acc1b9bafb602a59c3a5ab48bd477aca0e0673e2786631dfc945`. Runtime phase telemetry remains beside it.

## Resource diagnosis

The stop was not caused by the host or registered Resource Envelope. Training used 19.322 GB of the 32 GiB Runtime allowance, 89.2 MB of the 1 GiB durable allowance, roughly 48.5 seconds of ten minutes, and 80 of 1,024 total Verification requests. The host supervisor retained one CPU and 16 GiB for the operating and SSH environment.

The preceding 32×8 preflight reported 17.289 GB of fixed training residency and 17.071 GB of dynamic headroom. Its complete report remains at `/tmp/reflex-lean-public-optimizer-preflight-v1-32x8.json`, with file SHA-256 `019a327edbe9914d9112067ae492d42104a410d85067c762c6b426284f89c567` and content SHA-256 `ab61f7a4209720db90c34ad46b7a8d84140cd98fc706e7f7ee9672e4fbb370f7`.

## Search diagnosis

Training generated 418 Candidates over three epochs, selected 56, accepted three, and refuted 45. Of 377 retained Candidate Fates, 66 were novelty-filtered and 263 policy-deferred. With 32 roots and nine Primitive Operators, parent breadth commonly allocated only one legal Application to each parent-Operator pair. `ApplicationWriter` signaled that alternatives remained, but the Runtime treated the processed parent key as complete. It then labeled the empty pending tail `ResourceEnvelopeExhausted`, despite every material resource retaining ample headroom.

Increasing the generation window would only move the loss boundary and could recreate the earlier heavyweight-Candidate memory failure. ADR 0070 instead makes each parent-Operator position restart-complete, pages one bounded slice at a time, and rotates incomplete parents behind unvisited roots. ADR 0071 makes later generation refill the bounded lookahead inventory only after deferred Candidates drain below its target.

## Decision

Preserve this activation-null as negative Development evidence. Advance the Runtime and harness schemas, validate exact crash recovery and BitVec behavior, then rerun the same 32×8 corpus and budget as a new treatment after both defects are fixed. Do not reinterpret or overwrite v20, and do not increase model capacity.

This is a diagnosed Development null, not Scientific Confirmation and not a statement about mathematical taste.
