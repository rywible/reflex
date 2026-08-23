# Bound experimental Bundle decodes by the active domain

Internal experiment transformations accept an explicit maximum logical Domain Bundle size from their caller. The shared ablation helper does not reuse the BitVec causal harness Resource Envelope as a universal decode ceiling. Each harness supplies the same domain-appropriate logical limit under which it produced and will import the transformed bundle.

## Context

The v24 Lean retry passed Model activation: its 32-claim training Session consumed the registered 1,024 total Verification requests, retained 992 Candidate Experience entries, admitted 30 strict improvements across 27 claims, and promoted Model Revision generation 1. The encoded training bundle was 98,370,253 bytes, but its decoded logical segments totaled 450,307,376 bytes because the Experience segment alone was 358,197,859 bytes.

After a valid 128-request Bootstrap treatment, the harness attempted to derive the no-model and no-derived treatment seeds through the ablation helper owned by the BitVec causal harness. That helper decoded every source under BitVec's 256 MiB resident constant. It therefore rejected the valid Lean training bundle before transforming any state. The optimizer, Verification Kernel, training result, Bootstrap result, and host isolation were unaffected.

## Consequences

The helper's interface now makes its logical decode limit visible and mandatory. BitVec callers preserve their 256 MiB bound; Lean callers use the registered 32 GiB Runtime resident limit. A regression test proves that the supplied limit governs import. Lean Development schema v25 preserves v24 as an activation-partial and reruns the frozen protocol without changing Runtime revision 18, corpus, models, search breadth, or Resource Envelopes.

The v25 rerun completed all four treatments. Both large ablation seeds imported successfully, confirming the defect was isolated to the former shared ceiling.
