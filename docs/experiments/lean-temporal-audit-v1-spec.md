# Lean 2026 Temporal Audit v1

Status: **superseded before audit exposure by the public-optimizer v2 protocol**

No declaration from the 2026 audit snapshot was accessed under this protocol. Final pre-freeze review established that v1 trained and ranked a standalone Lean Potential model directly in the Experimental Harness rather than exercising the public Improvement Session. Its implementation is retained as negative design evidence, but it is ineligible for execution or Scientific Confirmation.

This is the human-readable registration for the executable protocol in xtask/src/lean_audit.rs and xtask/src/lean_audit_confirm.rs. The content-addressed freeze manifest and exact audit lock are committed before any checkout or declaration from the audit snapshot is accessed.

## Claim

A small local CPU-only Reflex portfolio trained exclusively on verified consequences available through 2024 predicts which unseen late-2024 Lean theorems acquire useful consequences before July 2026. Full must Pareto-dominate the virtual best registered baseline across separate Potential heads and reduce CPU time-to-matched future utility by at least 10×, with a simultaneous one-sided 99% lower bound above 3×. Exact kernel replay, elegance, recovery, and resource limits are independent no-regression gates.

This is a test of temporal mathematical taste, not theorem correctness. The official pinned Lean kernel separately verifies every replayed proof and relationship.

## Pre-cutoff learning and audit artifacts

Training Targets are derived from the union of the June→September and September→December 2024 Temporal Snapshot Pairs. The seven heads are anticipation, direct inbound descendants, direct theorem reuse, exact-statement compression, declaration survival, later dependency cost, and dead-end risk. Declaration survival is not Semantic Migration, and dependency cost is not measured kernel CPU.

The audit-artifact population is every eligible theorem in the 2024-12-31 catalog whose semantic-family digest was absent from both training pairs. Artifact identity includes the declaration, exact Lean source module, and semantic-family digest. Each contender freezes 256 Artifacts independently for every head before audit exposure.

Full, Bootstrap, no-model, no-consolidation, and immediate-only are compared with uniform, dependency-light, and historical-reuse baselines. The virtual-best baseline is selected independently per head from all three registered baselines. No contender may change its rank order after the future snapshot is visible.

## Audit boundary and one-shot execution

The audit snapshot is the latest mathlib commit whose commit timestamp is strictly before 2026-07-01T00:00:00Z. At lock creation, the harness queries GitHub’s primary commits API for exactly the newest default-branch commit through 2026-06-30T23:59:59Z, verifies that the returned SHA is the requested pin, and records the complete response alongside its URL, timestamp, and SHA-256. Its selected Lean toolchain, local toolchain alias, runtime version, and Lean commit are also recorded in the content-addressed lock before checkout. This is the only networked preparation step; confirmation is fully local. The final command builds the audit catalog and analyzes it in one execution. A repository-fixed receipt consumes the seal, so changing output paths cannot create another attempt. Failure, timeout, or any deviation is retained in a fixed failure report and cannot be discarded or rerun in pursuit of a favorable result.

## Economics and anytime behavior

The Resource Envelope is eight total lanes: six in-process lanes and two heavyweight one-thread Lean verifier processes. A supervising process kills and seals a run that exceeds 48 GiB combined resident memory or 24 hours wall time. CPU checkpoints are 1, 4, 16, and 64 CPU-hours. Each head has only 256 frozen Artifacts; measured training, ranking, and per-Artifact target-evaluation CPU determine the exact prefix and family-weighted outcome available at every checkpoint. After exhaustion, the final outcome is carried forward at later checkpoints. Exhaustion by one hour is not an additional confirmation gate, and the runner never burns CPU merely to fill a checkpoint.

CPU time-to-utility includes frozen training CPU, per-head ranking CPU, theorem fetch wall time as a conservative external CPU upper bound, and exact kernel replay CPU upper bounds. For each head, the common target is the lower of Full and virtual-best final directional utility over the first 16 kernel-replayed Artifacts. The point gate requires the geometric mean of baseline CPU divided by Full CPU to be at least 10×. Only if that gate passes does the predeclared nested bootstrap evaluate whether the one-sided 99% lower bound exceeds 3×.

Wall time, controller CPU, verifier-lifetime CPU upper bounds, kernel-call CPU upper bounds, measured controller high-water RSS, hard combined resident bounds, catalog bytes, model bytes, and report bytes remain separate Measurements.

## Statistical units and intervals

The statistical unit is a semantic theorem family nested in the exact source module recorded by Lean’s environment. Comparisons are paired by frozen family identity. Module-clustered two-stage bootstrap resampling samples modules and then families within modules. Ten thousand deterministic resamples are used. Pareto intervals use Bonferroni alpha 0.01 / (7 heads × 3 baselines), yielding simultaneous one-sided 99% family-wise lower bounds.

Full’s point estimate must weakly improve all seven heads over the virtual-best baseline and strictly improve at least one. Every simultaneous lower bound must be nonnegative. Missing artifacts and failures remain in the denominator; they are never silently excluded.

The causal-ablation gate is separate from the baseline gate. Full must weakly improve all seven heads and strictly improve at least one head against each of Bootstrap, no-model, no-consolidation, and immediate-only.

## Kernel replay, elegance, and recovery

For each contender and head, the first 16 Artifacts are fetched from the pinned December worker and replayed without repair through the audit kernel. A failed replay contributes zero directional utility and fails the migration no-regression gate. Proof nodes, proof depth, encoded bytes, dependencies, axioms, diagnostics, cumulative CPU upper bounds, and kernel-evidence hashes are retained per Artifact. Proof and dependency Measurements may not regress under replay.

Up to 64 future relationships rooted in Full’s frozen Artifacts are separately replayed and certified. Exact, definitional, specialization, derivation, family-collapse, and corpus-compression relationships remain typed rather than collapsed into one score. The report retains each relationship’s source and target identity, kernel result, and evidence hash so the certificate is replayable rather than merely counted.

After the primary workers stop, a clean audit worker replays every unique sampled proof again. Recovery must reproduce every accept/reject state and the content hash of the returned kernel evidence. Combined resident usage must remain within 48 GiB. Any kernel replay, recovery, or resource regression fails the mechanical confirmation gate.

## Blinded mathematical critique

The mechanical JSON report is written and hashed before a critique packet is created. The packet contains 32 deterministically shuffled Artifacts without treatment or head labels. Only after the mechanical seal may the designated mathematical reviewer inspect generality, proof collapse, local proof elegance, family-level elegance, corpus compression, and plausible human value. Each item records low, medium, high, or not-observed for the five elegance dimensions; human value is unlikely, plausible, clear, or not-assessable, with mandatory notes. Critique cannot change the mechanical decision; it is published as a separate qualitative assessment.

## Decision and retention

The mechanical controller cannot declare Scientific Confirmation while critique is pending. A completed mechanical execution emits either a mechanically passing or failing sealed result, then a blinded packet. Its content-addressed finalizer requires one complete assessment for every blinded ID, preserves the mechanical decision unchanged, and treats any declared critique-protocol deviation as a Null Result. A sealed execution failure instead finalizes directly as a Null Result bound to the receipt and failure report because no valid blinded packet exists to assess. Every final report states whether the preregistered gates and critique completed. Any miss still triggers Wrela as the third domain; Wrela is not a rescue analysis.

The final freeze manifest SHA-256 values, audit lock, and exact reproduction command are appended to this registration before audit checkout.
