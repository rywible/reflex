# Lean foundation development result v1

Status: Development evidence only. The final latency assignment ran from clean revision e3e3992ed1bdc7aabb7b887e91d67e66a279f4ee, but the five cases were discovered during earlier exploration and no Temporal Audit Corpus was opened. These results establish implementation and latency gates; they are not Scientific Confirmation.

The environment is mathlib commit 7178aee7a431bb7527da15c3507836d8dfefcda4, toolchain leanprover/lean4:v4.15.0-rc1, and Lean commit ffac974dba799956a97d63ffcb13a774f700149c, on the recorded eight-core AArch64 host.

The dependency-closed binary catalog contains 1,007,619 declarations across all imported declaration kinds, of which 458,619 are transitively eligible. Its content SHA-256 is ddbbf4fbf34a85e3fe572e23a637e58210f51bb5ef6575bfaf7d8e816e6d9bd4; the file is 65 MiB, built in 211,096 ms, and checksum-validated and loaded in 756 ms.

The fixed five-case suite performed two warmups and 20 measured single-candidate repetitions per case. Kernel-certified warm Proof Collapse latency was 4 ms p50 and 8 ms p95, against gates of 1,000 ms and 5,000 ms. A new worker restored and replayed a retained collapse in 34,060 ms, against the 60,000 ms gate. The clean report content SHA-256 is a707738afa1c3fb3421e9f7d10b159f9ee3cd726857da5d97a47e0cb1d4518ca.

The public improve integration then reduced ContinuousMap.compactOpen_eq_iInf_induced from a 10,041-node elaborated proof body to a one-node kernel-accepted proof using ContinuousMap.compactOpen_eq_sInf_induced. It retained the improved Artifact in a Domain Bundle and replayed that Artifact through a new LeanDomain and new Lean process in under 60 seconds. The complete ignored integration test took 105.48 seconds because it separately built the source corpus, ran the Improvement Session, and performed clean-process bundle recovery.

The first public attempt exposed and led to correction of a Structural Protocol violation: the Lean view used pre-order indexes while the Runtime's canonical root is the final post-order node. No pre-fix outcome is retained as a successful optimization result.
