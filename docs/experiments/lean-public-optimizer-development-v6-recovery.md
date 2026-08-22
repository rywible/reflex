# Lean public-path optimizer Development v6 recovery follow-up

## Result

Reflex completed the same leakage-corrected, pre-2025 4-training/4-held-out Development diagnostic as v6 after changing Domain Bundle recovery to replay every retained Artifact and Accepted Experience outcome, but not historical Refuted or Unknown Experience. Full recovered all four held-out Proof Collapses and removed 12,922 proof nodes, matching Bootstrap instead of exhausting its equal Verification envelope after two collapses. This establishes that historical negative-outcome replay caused the v6 deficit.

| Treatment | Strict collapses | Nodes removed | Verification requests | CPU seconds | Peak accounted resident GB |
| --- | ---: | ---: | ---: | ---: | ---: |
| Full | 4/4 | 12,922 | 254 | 87.786 | 18.477 |
| no-model | 4/4 | 12,922 | 254 | 87.047 | 18.477 |
| no-derived | 4/4 | 12,922 | 254 | 86.814 | 18.477 |
| Bootstrap | 4/4 | 12,922 | 242 | 85.189 | 17.915 |

Full, no-model, and no-derived all completed with `NoEligibleWork`; the original v6 versions exhausted 256 requests after 2/4 collapses and 998 removed nodes. The recovery change therefore restores useful held-out search without weakening the correctness path: retained Artifacts still replay their Verification Records, Accepted Experience remains kernel-checked as positive training evidence, and every future Candidate still passes Verification. Refuted and Unknown observations remain canonical, revision-bound advisory state and cannot admit or export an Artifact.

This is a successful recovery-performance correction but negative evidence for learned advantage. Full does not beat either learned ablation or Bootstrap in discoveries, nodes removed, or CPU per discovery. Its 87.786 CPU-seconds are 3.0% above Bootstrap's 85.189 seconds. The next Development target is a constrained diagnostic that records proposal order and per-opportunity model forecasts, then demonstrates that learned Potential finds valuable collapses earlier than Bootstrap rather than merely reaching the same fixed point.

The run used release Rust with NEON enabled inside the registered host-isolated supervisor: CPUs 0–6 were available, CPU 7 remained reserved, memory was hard-limited to 40 GiB with a 16 GiB host reserve, and swap was disabled. All inputs were the September 30 and December 31, 2024 catalogs used by v6. No 2026 checkout, catalog, Artifact, label, or relationship was accessed.

The complete selected declarations, treatment outputs, host isolation, hashes, and resource accounting are retained in [the raw report](./lean-public-optimizer-development-v6-recovery.json). Its report content SHA-256 is `71e2a81636e088e92b97311e77dfd27d6c90241588156b42113b12323b9ce982`; the newline-terminated retained JSON file SHA-256 is `e9e9c08cdb9b4a597e9e97e2b82db2516cc1b5d7e56dd665fdd83609a245beb8`.
