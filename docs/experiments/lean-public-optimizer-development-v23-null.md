# Lean public-path optimizer Development v23 activation-null result

## Result

Move-aware resident accounting lowered the reported peak from 19.734 GB in v22 to 18.824 GB, but it did not change a single search outcome. The 32-claim training run again kernel-Verified 352 Candidates, admitted 25 strict Proof Collapses across 23 claims, selected a final undispatched cohort, and promoted no Model Revision. The activation gate correctly skipped every counterfactual treatment.

The checked-in [machine summary](./lean-public-optimizer-development-v23-null-summary.json) records the activation-null. The retained training Domain Bundle remains at `/tmp/reflex-lean-public-optimizer-development-v23-32x8-work/training.bundle` with SHA-256 `87292585b88cb6b6a59938ce2d36d8d5fdffa86460f44137d40db6a9bd03944b`; its training phase file has SHA-256 `db600eab5ca61812895bbc08ecbdb7e34db66c3e567d1ba55c487ed44bc9842d`.

## Falsified hypothesis

The identical stop falsifies the v22 claim that impossible resident overlap was the limiting gate. ADR 0072 remains a necessary accounting correction—Rust moves deferred Artifact payloads rather than cloning them—but its strongest experimental prediction failed. The lower peak proves that the new formula took effect; the unchanged Verification trace proves that a different earlier refusal controlled the run.

## Exact limiting gate

The pre-Verification durable check executes before resident admission and mixed unrelated units. It added in-memory Candidate pipeline and Recovery reserves to an encoded checkpoint length, then compared that sum with the one-GiB durable envelope.

The v23 bundle is 90,731,283 encoded bytes. Its current complete Experience and Recovery segments have a conservative codec-owned encoded-size bound of 246,819,697 bytes, only 23% of the envelope. Recovery holds 134 deferred Candidates whose canonical encodings total 84,951,685 bytes. Even charging that entire retained canonical body a second time leaves ample durable headroom for an eight-Candidate Experience cohort. The refusal was dimensionally invalid, not conservative.

A minimized BitVec case independently reproduced the defect: the old preflight refused a second request under 5,120 durable bytes while successfully publishing a 1,471-byte restart bundle. Encoded forecasting publishes the complete 2,245-byte result under the same limit.

## Decision

Preserve v23 as an activation-null. ADR 0073 moves durable prediction behind the bundle codec, retains canonical Candidate bytes after novelty filtering, advances Runtime revision 18 and Development schema v24, and adds explicit internal Resource-refusal telemetry. Validate resource and crash semantics, then make one identical retry. Do not change search breadth, model capacity, or the registered envelope.

This is diagnosed Development evidence, not Scientific Confirmation.
