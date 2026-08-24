# Lean Premise Retrieval Development v1

This Development diagnostic asks one narrow pre-causal question: does the bounded Lean proof-substitution enumerator expose the already known shorter proof for each of the four held-out v11 claims before learned ranking? It is not Scientific Confirmation and uses no 2026 data.

The executable ignored test pins the final pre-2025 Lean/mathlib environment, fetches the exact four held-out Seeds and the original 47-Artifact v11 Operator library by declaration identity, kernel-replays all 51 Artifacts, builds the production retrieval index, and queries only the first 16 donors. On the 8-core Arm Neoverse-V2 development host, with execution restricted to CPUs 0-5, the known donor ranks were `[1, 1, 1, 1]`; recall@1/4/8/16 was `[4, 4, 4, 4] / 4`. Index construction consumed 20.636 ms process CPU, the four queries consumed 5.465 ms process CPU, conservative retained library plus index accounting was 63,780,482 bytes, and declared reusable scratch at top-16 plus one boundary donor was 22,928 bytes.

The independent synthetic release diagnostic used 4,096 donors and 64 top-16 queries. Index construction took 43.478 ms, indexed queries took 54.798 µs, and full structural rescanning took 182.408 ms, a 3,328-fold observed query-time ratio. The contiguous index accounted for 1,131,520 bytes and declared 22,784 scratch bytes. This timing is a single Development observation, not a population estimate; the enforced regression gate is only that indexed lookup remains at least four times faster.

Run the real diagnostic with:

```sh
REFLEX_LEAN_LAKE=/path/to/lake \
REFLEX_LEAN_MATHLIB=/path/to/pinned/mathlib \
taskset -c 0-5 cargo test --release -p reflex-lean \
  premise_retrieval_recalls_known_shorter_proofs_in_the_v11_corpus \
  -- --ignored --nocapture
```

Run the synthetic diagnostic with:

```sh
taskset -c 0-5 cargo test --release -p reflex-lean \
  indexed_retrieval_is_four_times_faster_than_rescanning_donor_structure \
  -- --ignored --nocapture
```

The result establishes proposal availability and retrieval cost only. It does not show that a learned policy causally beats Bootstrap, nor that these four Development claims estimate performance on unseen mathematics. The next valid intervention is the equal-budget public-path treatment under the new Semantic Identity.
