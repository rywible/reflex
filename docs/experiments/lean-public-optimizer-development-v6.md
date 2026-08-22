# Lean public-path optimizer Development v6

## Result

Reflex safely completed a leakage-corrected, pre-2025 4-training/4-held-out Lean proof-optimization diagnostic through the public `improve` path and all four causal treatments. Every selected opportunity had a human-facing Seed declaration, at most 100,000 proof nodes, a distinct statement fingerprint, and a strictly shorter library proof that the trust-zero Lean kernel accepted for the Seed claim. Held-out names were absent from the September snapshot, and held-out statement fingerprints were disjoint from selected training fingerprints.

Bootstrap found all four held-out Proof Collapses and removed 12,922 proof nodes. Full, no-model, and no-derived each found two and removed 998 nodes. Full therefore did not beat its counterfactuals. This is negative Development evidence, not Scientific Confirmation.

| Treatment | Strict collapses | Nodes removed | Verification requests | CPU seconds | Peak accounted resident GB |
| --- | ---: | ---: | ---: | ---: | ---: |
| Full | 2/4 | 998 | 256 | 68.218 | 18.477 |
| no-model | 2/4 | 998 | 256 | 68.629 | 18.477 |
| no-derived | 2/4 | 998 | 256 | 68.672 | 18.477 |
| Bootstrap | 4/4 | 12,922 | 242 | 85.145 | 17.915 |

The trained session used 146 Verification requests before sealing its 3.87 MB compressed bundle. On held-out resume, mandatory Artifact and Experience replay consumed enough of the equal 256-request envelope that Full exhausted the envelope after two collapses. Bootstrap had no historical replay tax and reached all four. The next performance target is therefore Knowledge Consolidation of historical Experience: accepted exported Artifacts must remain kernel-replayed, while heuristic training evidence needs a compact, reproducible representation that does not re-spend every historical refutation on every resume.

## Safety and validity corrections

The original unpaged 4+4 run reached roughly 36.5 GiB Rust RSS and was killed by the 40 GiB cgroup; the host and SSH session survived. Persistent `Arc`-linked Lean expressions reduced the analogous Rust-side point to roughly 4 GiB. Deterministic LZ4 Bundle framing, resource-bounded expansion, single-Candidate Experience replay, and single-item Lean protocol transactions then made restore and failure publication bounded. A separate kernel smoke collapsed a 10,041-node proof to one node and cleanly restored it in 107.45 seconds with a 124,329-byte bundle.

Development v5 selected two held-out declarations that had also appeared in training because their feature-derived semantic groups changed across snapshots. That run is invalid for generalization and was not used. V6 additionally requires names absent from the earlier snapshot and statement-fingerprint disjointness after training selection. Earlier tiny diagnostics established that an 8-request envelope could miss an available 401-to-1 collapse, while a 64-request envelope found it in every treatment.

All runs used only the September and December 2024 catalogs. No 2026 checkout, catalog, Artifact, label, or relationship was accessed. The complete v6 inputs, selected declarations, treatment outputs, host isolation, hashes, and resource accounting are retained in [the raw report](./lean-public-optimizer-development-v6.json), whose content SHA-256 is `c7cb56bad45190beebecc00d53d66ae08c3034e5cc73b2a8533ffea771f16272`.
