# Highest-leverage improvements to the CPU verifier-guided loop

Date: 2026-08-22

## Executive decision

Reflex should not increase model capacity next. The highest-leverage sequence is:

1. make the learned challenger and Bootstrap comparator behaviorally honest, then trace why retained state loses a valuable Bootstrap discovery;
2. retain donor/premise relationships in Candidate identity and features, and retrieve structurally relevant library Artifacts before ranking;
3. make learned guidance cooperate with, rather than replace, a protected Bootstrap queue;
4. train a claim-conditioned ranker on top-of-budget order, separately from calibrated Potential heads;
5. spend Verification through cost-aware active search with explicit exploration and logged propensities;
6. collect broader Experience across more independent correctness claims at the same total budget;
7. consolidate repeated verified structure into lemmas and Derived Operators only after downstream utility is measured;
8. scale models only after those changes expose residual model-limited error.

The radical take is that Reflex does not presently have a model-size problem. It has a comparator-identity problem, an information-loss problem, an attribution problem, a proposal/retrieval problem, and an objective mismatch. A fast ranker cannot recover a useful Candidate that generation never offers within the finite choice window. Nor can it rank two proof substitutions differently when their donor identities and donor-to-claim relationships were discarded before feature extraction. Pointwise calibration loss is not the operational objective when only the first few Verification requests matter.

Statements below marked **Evidence** report primary-source results or repository observations. Statements marked **Inference** are proposed consequences for Reflex.

## What the current evidence establishes

**Evidence.** The active v7 run used 1,008 kernel-labeled attempts across 16 correctness claims, with 13 Accepted and 995 Refuted outcomes. At 128 held-out Verification requests, fresh Bootstrap found three strict Proof Collapses and removed 9,164 proof nodes; Full found two and removed 998. More importantly for attribution, Full, no-model, and no-derived found exactly the same two improvements in the same observer order ([v7 active result](../experiments/lean-public-optimizer-development-v7-active.md)).

**Inference.** The current model's observed marginal causal ranking value is zero, not merely “worse than Bootstrap.” The Full-minus-Bootstrap gap cannot be assigned to the model because removing the model did not recover the missing `hfdifferential_apply` collapse. Something about retained Artifacts, Experience, Knowledge, generation limits, novelty filtering, or resumed scheduling separates no-model from fresh Bootstrap. That path must be localized before another trainer treatment.

**Evidence.** A code audit finds two more fundamental mismatches. When no champion exists, learning compares a challenger against `FtrlModel::zero()`, but operational Bootstrap is represented by `None` and enters a different ordering branch. The `None` branch alternates protected Derived and ordinary queues 2:6, while the `Some(zero_model)` branch splits every eighth ordinary Candidate into an “exploration” queue and alternates 2 derived, 1 exploration, and 5 model-ranked Candidates ([learning source](../../crates/reflex/src/learning.rs), [ordering source](../../crates/reflex/src/runtime.rs)). A zero FTRL predictor is therefore not a behaviorally equivalent Bootstrap Revision.

**Evidence.** Lean's proof-substitution Operator emits exact-proposition donor proofs first, then other library proofs, but the resulting generic Candidate retains the Seed declaration/proposition and only the substituted proof term. The 16 features encode parent/Candidate size, epoch, and Operator bucket; they contain neither donor identity nor donor proposition, provenance, retrieval rank, or donor-to-claim relationship ([Lean Operator source](../../crates/reflex-lean/src/domain.rs), [feature source](../../crates/reflex/src/runtime.rs)). Consequently, two one-node proof substitutions from different donors can have identical features even when one is kernel-Accepted and the other is Refuted.

**Inference.** Initial promotion currently establishes “better pointwise loss than a zero predictor,” not “better allocation than operational Bootstrap.” Donor-blind feature collisions also impose an irreducible ranking error that no parameter increase, class weight, pairwise loss, or nonlinear model can remove. These two defects precede model-family work.

**Evidence.** The fixed-Experience studies found that richer structure contains some signal but does not yet produce a strong operational ranker. Structural v2 improved descendant-Potential loss by 92% but regressed protected heads; the conservative family moved one additional Accepted Candidate into the top 16 but not earlier cutoffs; inverse-frequency class weighting erased that small gain and sharply worsened dominant losses ([v1](../experiments/lean-model-feature-development-v1.md), [v4](../experiments/lean-model-feature-development-v4.md), [v5](../experiments/lean-model-feature-development-v5.md)). Structural extraction cost 0.587 CPU seconds for all 1,008 Candidates, while model training took less than one millisecond and the original training Session used 72.2 CPU seconds.

**Inference.** Structural extraction is already cheap relative to Verification, but the loss is misaligned. The statistical unit is also 16 correctness claims—not 1,008 independent examples. More negative attempts within the same claims cannot substitute for breadth across independent claims.

## Ranked interventions

| Rank | Intervention | Expected verified discoveries per CPU-second | Implementation risk | Cheapest decisive diagnostic |
| ---: | --- | --- | --- | --- |
| 0 | Behaviorally real Bootstrap comparator, Candidate-fate trace, and retained-state ablation | Very high diagnostic value | Low–medium | Compare `None` and zero-FTRL ordering, then trace Bootstrap/no-model/Full at 128 requests |
| 1 | Donor-aware premise/Operator-argument retrieval and features | Very high | Medium | Collision audit plus retrieval recall@k against known shorter proofs |
| 2 | Protected cooperative Bootstrap + learned queues | Very high | Low–medium | Replay 1:1 queue interleaving, then one equal-budget public-path treatment |
| 3 | Within-claim pairwise/listwise ranking and real cost targets | High | Low–medium | Retrain donor-aware linear FTRL from the retained Experience; no new Verification |
| 4 | Goal-relative search-state features and cheap feature crosses | High | Medium | Paired fixed-Experience top-k curves after donor-aware ranking exists |
| 5 | Cost-aware active search and explicit value-of-information lanes | High | Medium | Seeded randomized top-m logging on Development claims with equal CPU envelopes |
| 6 | Broader, curriculum-shaped expert iteration | Medium–high | Medium | Reallocate the same 1,024 requests from 16 deep claims to 64 shallower claims |
| 7 | Verified library compression and lemma invention | High long-term upside | High | Mine and verify one abstraction, then measure downstream held-out search savings |
| 8 | Larger linear/tree models | Low until prior gates pass | Medium overfitting risk | Nested 16/32/64 sweep only after a same-size ranker wins causally |

### 0. Make Bootstrap behavior real, then trace Candidate fate

**Inference.** Bootstrap must be one explicit policy implementation at every layer: Model Revision identity, offline comparison, rollback predecessor, and Runtime ordering. Either represent `Bootstrap` and `Learned` as distinct decision-policy variants and compare their actual ranked outputs, or make the zero model execute exactly the Bootstrap branch. Do not let `None` and `Some(zero)` silently denote different schedulers. Model promotion may retain a cheap calibration gate, but Operational Promotion must include the constrained policy-level discovery gate against the real Bootstrap policy.

Before any kernel run, feed one frozen mixed Candidate vector through `None` and `Some(FtrlModel::zero())`. The test should either prove byte-identical selected order or deliberately name them as different policies. On the retained Selection claims, report both pointwise loss and actual top-k order for the real Bootstrap policy; a surrogate loss against a behaviorally different scheduler is not a valid comparator.

**Inference.** Record one compact event for every Candidate at the existing four held-out roots: claim digest, Candidate and parent keys, Operator, generation ordinal, proposal limit, novelty-filter outcome and reason, Bootstrap position, learned position, queue chosen, Verification outcome and CPU, and whether Admission produced a strict improvement. The missing `hfdifferential_apply` Candidate then has only four possible fates:

1. never generated: generation order, choice-window allocation, or retrieval is the bottleneck;
2. generated then filtered: retained Artifact or Experience suppression is the bottleneck;
3. retained but placed beyond request 128: selection policy is the bottleneck;
4. verified but not Accepted: claim construction or kernel evidence differs.

If the trace points to retained state, split no-model once into Artifacts-only, then plus negative-Experience suppression, then plus Knowledge. This is a causal decomposition of the current failure and costs far less than another training campaign.

### 1. Preserve donor relationships and retrieve before generating

**Evidence.** LeanDojo identifies premise selection as a key theorem-proving bottleneck and reports that accessible-premise analysis plus hard negatives materially improves retrieval in its Lean prover ([LeanDojo/ReProver](https://arxiv.org/abs/2306.15626)). DeepMath formulates premise selection as a two-stage retrieval problem over a large formal library ([DeepMath](https://papers.nips.cc/paper_files/paper/2016/hash/f197002b9a0853eca5e046d9ca4663d5-Abstract.html)). TacticToe combines learned tactic choice with k-nearest-neighbor premise selection and MCTS while remaining effective on one CPU ([TacticToe](https://arxiv.org/abs/1804.00596)).

**Evidence.** Reflex's Lean substitution Operator currently emits exact structural proposition matches first and then scans all library proofs in stored order; several other Operators similarly traverse every proof until their output limit fills ([Lean Operator source](../../crates/reflex-lean/src/domain.rs)). The v7 corpus deliberately contains a kernel-accepted shorter library proof for each Seed, but fingerprint equality need not imply syntactic proposition equality ([Lean corpus decision](../adr/0055-separate-lean-seeds-from-library-artifacts.md)).

**Inference.** Candidate provenance must retain a stable donor Artifact key and proposal relationship long enough to derive Experience and features. This is not a claim that donor provenance establishes correctness; only the kernel does that. It lets learning distinguish which premise and which relation produced an otherwise identical proof shape. At minimum add:

- donor Artifact key as provenance, not a raw learned identifier;
- exact-proposition, statement-fingerprint, and structural-similarity relation flags;
- donor statement/proof structural summaries and signed differences to the Seed claim;
- retrieval tier, rank, margin, and prior Operator × relation outcomes.

Before changing the ranker, compute a feature-collision table grouped by claim, Operator, and current feature bits. Report groups containing both Accepted and Refuted substitutions; those are provably unrankable by the current representation. The first representation success is to split the mixed-label groups containing known shorter donors without using declaration names or future information.

A ranker over already-generated Candidates is downstream of the more important decision: which library Artifacts become Operator arguments before the generation limit. Add an Operator-specific, in-memory retrieval index with progressively weaker tiers:

1. exact proposition identity;
2. stable statement fingerprint candidates, always kernel checked;
3. symbol-independent constructor-path and subtree sketches;
4. dependency-context and previously successful Operator co-occurrence;
5. novelty-preserving approximate neighbors.

Use contiguous sparse postings, IDF weighting, and top-k heaps; no neural embedding is required. Query features must include both the Seed-relative claim and the proposed premise. The decisive offline diagnostic is recall@1/4/8/16 of the already known shorter proof for every Development claim, plus retrieval CPU and bytes. Kernel-replay only those top-k results afterward. If `apply_hfdifferential` is not in the retrieved prefix, model work cannot fix the current miss.

### 2. Make learning additive to a strong search prior

**Evidence.** ENIGMA supports a solo learned clause queue and a cooperative mode that alternates learned guidance with the base E strategy. Its authors report that single-queue learned variants can be extremely weak, while layered or cooperative integration protects useful base behavior; the system's learning loop repeatedly adds proofs found by both modes ([ENIGMA guidance integration](https://link.springer.com/chapter/10.1007/978-3-030-79876-5_31), [ENIGMA Anonymous](https://pmc.ncbi.nlm.nih.gov/articles/PMC7324011/), [Make E Smart Again](https://pmc.ncbi.nlm.nih.gov/articles/PMC7324009/)). TacticToe is especially relevant to Reflex's hardware goal: on one CPU it proved 66.4% of 7,164 HOL4 theorems in 60 seconds each, and combining it with E raised coverage to 69.0% ([TacticToe](https://arxiv.org/abs/1804.00596)). More generally, SATzilla demonstrated that complementary algorithms can make a portfolio stronger than any constituent when selection is controlled carefully ([SATzilla](https://jair.org/index.php/jair/article/view/10621)).

**Inference.** A young Model Revision should not own one replacement priority queue. After per-claim coverage, selection should merge deduplicated queues such as:

- active-Preference Bootstrap order;
- learned within-claim rank;
- uncertainty or novelty exploration;
- protected Derived Operator exploration.

Start with a fixed, auditable ratio, such as three Bootstrap, three learned, one uncertainty, and one derived slot per eight remaining requests. All queues continue to respect the caller's Preference; this is scheduling, not a universal scalarization of Potential. Later, a budget allocator can learn the queue ratio. An even safer alternative is a learned residual over Bootstrap rank with a bounded maximum displacement until causal evidence permits more authority.

The first success condition is not that the model predicts better loss. It is that the cooperative policy retains all three Bootstrap collapses by request 128 and adds or advances at least one verified improvement without worse CPU per discovery.

### 3. Optimize order within a claim, not global class frequency

**Evidence.** RankNet trains on pairwise preferences through a logistic loss on score differences, while ListNet treats each ranked list as the training instance rather than reducing the problem to independent classifications ([RankNet through LambdaMART](https://www.microsoft.com/en-us/research/publication/from-ranknet-to-lambdarank-to-lambdamart-an-overview/), [ListNet](https://www.microsoft.com/en-us/research/wp-content/uploads/2016/02/tr-2007-40.pdf)). These objectives match top-of-list decisions more directly than pointwise mean squared or classification loss.

**Inference.** Treat a correctness claim as the query and one comparable frontier/Operator context as the list. Derive ordered grades only from mechanically grounded outcomes, for example:

- strict admitted improvement over the Seed;
- other Accepted Candidate;
- Refuted Candidate;
- Unknown omitted from preference pairs.

Train pairwise FTRL on donor-aware feature differences, sampling hard Refuted Candidates that the current policy ranks ahead of an Accepted Candidate. Do not run this treatment on current mixed-label collision groups: their feature difference is exactly zero, so changing the loss cannot help. Normalize total weight per claim and per decision list so a claim with hundreds of failures does not dominate a claim with ten. Preserve the seven calibrated Potential heads as separate forecasts; add a dedicated operational rank head rather than asking calibration loss to stand in for search utility. Record actual per-Candidate or per-worker-task Verification CPU for the cost head; a constant request-count target cannot rank cost.

This needs no larger model and no new labels. On the retained bundle, compare baseline pointwise FTRL, pairwise FTRL, and a top-one listwise linear loss with identical features, roles, epochs, and claim groups. The primary offline outcome is macro-by-claim Accepted and strict-improvement recall through budgets 1/4/8/16/32/64/128, not aggregate row loss. Because only eight claims are in Replay and eight in Selection, this remains a Development diagnostic.

### 4. Represent the decision context, not only Candidate size

**Evidence.** ENIGMA's fast symbol-independent guidance concatenates clause, conjecture, and problem features; its sparse features include syntax-tree cuts, anonymized symbol arities, and structural statistics. The authors report gradient-boosted trees as their strongest classifiers with evaluation speed comparable to the earlier linear classifier, while making a GNN evaluate efficiently on one CPU was substantially harder ([ENIGMA Anonymous](https://pmc.ncbi.nlm.nih.gov/articles/PMC7324011/)). PACT shows that kernel-level Lean proof terms contain enough reusable structure to create multiple self-supervised tasks, improving held-out proof success from 32% to 48% in its setting ([PACT](https://arxiv.org/abs/2102.06203)).

**Inference.** Candidate-only size summaries cannot answer “useful for this claim, at this point in search.” After the ranking loss exists, extend features in this order:

1. Seed/goal structure and Candidate-to-goal similarity;
2. parent, Candidate, and signed delta structure;
3. retrieval tier, retrieval rank, and retrieval margin;
4. exact Operator identity and Operator × structural crosses;
5. derivation depth, parent improvement status, novelty, and prior failed-neighbor density;
6. stable symbol-independent constructor paths and small subtree hashes.

Do not restore epoch as a shortcut. Cache the Seed and parent summaries; compute Candidate deltas in the same post-order pass already measured at under 1% of current training-session CPU. Before a hidden layer, test explicit crosses or a tiny bounded-depth tree ranker. ENIGMA is evidence that cheap relational features and trees can beat a linear model without paying GNN inference costs, not evidence that Reflex should import XGBoost or another language runtime.

### 5. Use active search, not generic uncertainty sampling

**Evidence.** Active search is defined specifically as finding as many rare positive items as possible under a labeling budget. Its authors show that ordinary predictive accuracy and generic active-classification concerns are secondary to discovery utility, and that nonmyopic policies can outperform myopic ones by an arbitrarily large amount in some settings ([Bayesian Optimal Active Search](https://arxiv.org/abs/1206.6406), [efficient nonmyopic active search](https://proceedings.mlr.press/v70/jiang17d.html)). Contextual bandits provide a lightweight way to learn action allocation from context and partial feedback; LinUCB was designed for changing action pools with a linear reward model and uncertainty ([LinUCB](https://www.microsoft.com/en-us/research/wp-content/uploads/2016/02/p661.pdf)).

**Inference.** Reflex currently needs two different lanes:

- exploitation: highest predicted probability of a strict Accepted improvement under the active Preference, subject to predicted Verification cost;
- information investment: Candidates whose result is expected to improve future decisions across claims, Operators, or structural neighborhoods.

Do not conflate the second lane with a lower-confidence-bound penalty or “every eighth enumeration item.” A verifier makes exploration epistemically safe—the Candidate still cannot be admitted unless accepted—so uncertainty is an economic question about spending, not a correctness reason to suppress novel Candidates. Keep outcome forecasts vector-valued; compare them using the run's Preference and Constraints while treating CPU, memory, and request count as resource consumption.

A practical first allocator is a per-Operator/queue Bayesian or linear bandit with a fixed minimum-coverage floor. Score expected Accepted strict discoveries and predicted CPU separately. Use seeded Thompson/UCB exploration only within a bounded top-m set, and record the probability of every chosen action.

### 6. Prefer more independent claims to more failures per claim

**Evidence.** Formal Mathematics Statement Curriculum Learning found that, at the same compute budget, interleaving proof search and learning substantially outperformed proof search alone, and a varied-difficulty statement set induced a curriculum of progressively solved problems ([statement curriculum](https://arxiv.org/abs/2202.01344)). HTPS improved its held-out Metamath result from 65.4% to 82.6% through online learning from searches on previously unproved theorems ([HTPS](https://arxiv.org/abs/2205.11491)). MetaGen found that verifier-compatible synthetic theorems and proofs improved a Metamath prover ([MetaGen](https://arxiv.org/abs/2002.07019)). Prioritized Experience Replay shows the general benefit of replaying decision-relevant experience more often than uniform history, while correcting the induced sampling bias ([prioritized replay](https://arxiv.org/abs/1511.05952)). AlphaProof likewise couples formal verification with RL and problem variants, but its published system trains on millions of auto-formalized problems and uses large test-time RL; that scale is not evidence for a CPU-local architecture ([AlphaProof](https://www.nature.com/articles/s41586-025-09833-y)).

**Inference.** The transferable lesson is expert iteration, not large models: search produces stronger verified traces; the small policy learns their decisions; the stronger policy changes the next search distribution. For the next data treatment, hold the 1,024-request envelope fixed and compare approximately 16 claims × 64 requests with 64 claims × 16 requests, using equal CPU ceilings and macro-by-claim outcomes. Prefer claims spanning Operators, proof sizes, and structural families. Replay missed-positive ranking pairs, new proofs, and hard near-neighbor Refutations; do not merely repeat all 995 negatives or inverse-weight labels.

Reflex must keep its verified-Seed rule. It can borrow curriculum scheduling from statement-curriculum systems without importing unverified generated statements as Seeds. Safe data augmentation comes from verified proof transformations, kernel-accepted variants, induced Obligations, and independently replayable historical consequences.

### 7. Turn repeated proofs into verified search vocabulary

**Evidence.** DreamCoder alternates solving, symbolic library extension, and replay so newly learned abstractions shorten later search ([DreamCoder](https://arxiv.org/abs/2006.08381)). Stitch synthesizes abstractions that compress recurring program structure and reports three-to-four orders of magnitude less time and two orders less memory than the compared DreamCoder abstraction learner while retaining comparable or better compressivity; its reference implementation includes a Rust command-line path ([Stitch paper](https://arxiv.org/abs/2211.16605), [Stitch source](https://github.com/mlb2251/stitch)).

**Inference.** Apply the pattern to verified proofs: mine repeated closed subproofs and derivation fragments, anti-unify them into candidate lemmas or Derived Operators, and ask the Lean kernel to verify every generalization. Compression is only a proposal prior. Promotion requires a second measurement: the new Artifact or Operator must improve held-out discovery count, proof collapse, or CPU under an equal budget. This prevents “beautiful” but inert abstractions from polluting the active library and gives Potential a mechanically observable delayed consequence.

This is a later intervention because current no-derived and Full treatments are identical. First prove that a retrieved or learned Derived Operator enters the selected prefix; then evaluate one mined abstraction end to end.

### 8. Scale models last, and scale the right family

**Evidence.** ENIGMA's CPU-focused evaluations found feature-rich gradient-boosted guidance competitive in real time and reported that GNN integration required batching and substantial engineering to become usable on one CPU ([ENIGMA-NG](https://arxiv.org/abs/1903.03182), [ENIGMA Anonymous](https://pmc.ncbi.nlm.nih.gov/articles/PMC7324011/)). The Reflex Selection corpus presently contains only eight claim groups, while the candidate model already has 112 effective coefficients ([scaling specification](../experiments/lean-model-scaling-development-v1-spec.md)).

**Inference.** More linear coordinates on eight Replay claims are more likely to memorize claim/operator accidents than discover mathematical regularity. Keep the nested 16/32/64 sweep, but unlock it only after pairwise structural-16 beats pointwise baseline-16 and wins the online cooperative treatment. If linear feature crosses leave clear residual error, compare a tiny bounded-depth oblivious-tree ranker before an MLP. Measure end-to-end CPU, cache bytes, and verified discovery curves; parameter count and Selection loss alone do not advance a model.

## Concrete next experiment

Run **Lean Comparator, Candidate Fate, and Cooperative Queue Development v1** on the existing frozen corpus and four held-out claims. Runtime revision 6 cannot truthfully migrate the v7 Bundle because v7 never retained Candidate Fates or queue attribution; create one bounded replacement v9 training Bundle under the unchanged corpus, model, features, and Verification envelope. This is a required semantic migration, not a trainer or capacity treatment. It starts with a no-kernel audit and then uses two short search phases.

Implementation status: the shared Operational Policy module and frozen mixed-prefix tests establish that operational Bootstrap and a zero FTRL predictor select the same Candidate order when forecasts contain no ranking information, while deliberately recording their first policy-identity divergence: the first unprotected Candidate is attributed to Learned by the 1:1 learned-first cooperative rule and to Bootstrap by the actual Bootstrap branch. The v9 trace retains exact Candidate identity, Operator digest, actual per-Operator proposal limit, filter reason, full unprotected Bootstrap/learned positions, selected queue, verdict, strict Admission consequence, and accounted Verification-batch CPU. The harness reports cumulative strict discoveries and completed-batch CPU at 1/4/8/16/32/64/128 Candidate verifications. The no-kernel feature audit reports exact Accepted/Refuted collision groups, the fraction of Accepted examples trapped in them, their Bootstrap top-k occupancy, and proof-substitution donor identities reconstructed by matching proof-term digests against the exact frozen Operator library. Fixed-Experience promotion remains explicitly diagnostic; only the equal-budget public-path treatment is causal.

### Phase 0: comparator and information audit without Verification

1. Materialize one frozen mixed Candidate list and compare the exact selected order under operational Bootstrap (`None`) and `Some(FtrlModel::zero())`. Record the first divergence and the queue rule that caused it.
2. Group all retained substitutions by correctness claim, Operator, and current feature bit pattern. Where possible, reconstruct the donor by matching the substituted proof-term digest to library Artifacts. Report mixed Accepted/Refuted groups, their top-k occupancy, and the fraction of Accepted substitutions that are feature-indistinguishable from a Refuted substitution.
3. Redefine the experimental comparator as the actual Bootstrap policy. Do not count promotion over a zero predictor as activation evidence.

If the known shorter proof is in a mixed-label collision group, donor-aware relation features and retrieval move ahead of the pairwise trainer. This phase requests no new kernel labels.

### Phase A: localize the lost Bootstrap discovery

Run fresh Bootstrap, retained-state no-model, and current Full at the same 128-request and CPU envelopes, with deterministic candidate-fate tracing. Freeze the code revision, treatment order, corpus, worker lanes, memory ceiling, and host reserve. Report cumulative strict discoveries and CPU at requests 1/4/8/16/32/64/128.

The phase ends as soon as the trace classifies `apply_hfdifferential` as not generated, filtered, ranked past budget, or kernel rejected. Do not change features or training during this phase. If retained-state no-model diverges before selection, split retained state as described in rank 0.

### Phase B: test cooperative selection without changing the model

Only if Phase A shows that the useful Candidate reaches selection and Phase 0 shows that its rank is representable, replay the same frontier with a protected Bootstrap queue and current learned queue, initially 1:1 after per-claim coverage. Deduplicate across queues and retain the current model, features, Experience, and Operators unchanged. If either gate fails, implement donor-aware retrieval/representation first and repeat the trace; do not tune the current ranker around missing information.

Development success requires all of the following:

- recover Bootstrap's three strict collapses by request 128;
- no worse strict-discovery count at every registered budget after request 16;
- lower or equal CPU per strict discovery than fresh Bootstrap at 128;
- a nonzero, explicitly attributed learned-queue contribution before claiming model value.

Failure is equally informative: it directs the next change to retrieval/generation rather than ranking. Four claims are enough for this mechanistic diagnostic, not for Scientific Confirmation.

## Causal and scientific evaluation requirements

**Evidence.** Logged search is partial-feedback data: only selected Candidates receive verifier outcomes. Offline replay is unbiased only under logging policies that give the evaluated actions support; inverse-propensity and doubly robust estimators address policy mismatch when action probabilities are known ([unbiased bandit replay](https://arxiv.org/abs/1003.5956), [doubly robust evaluation](https://arxiv.org/abs/1103.4601), [counterfactual risk minimization](https://arxiv.org/abs/1502.02362)). Time-uniform confidence sequences retain coverage under repeated monitoring and data-dependent stopping, unlike ordinary fixed-sample intervals ([confidence sequences](https://arxiv.org/abs/1810.08240)).

**Inference.** Fixed-Experience reranking is a useful representation diagnostic, but it is not causal evidence for a deployed search policy because the old policy determined which Candidates were verified. Reflex should therefore:

1. log the complete considered action set, queue, rank, chosen action, and nonzero choice probability for bounded randomized Development traffic;
2. use correctness claims—not attempts or pairwise expansions—as the independent grouping unit;
3. compare online policies on paired claims with equal request, CPU, memory, and worker envelopes and randomized treatment order;
4. report full budget curves, practical effects, and all protected outcomes;
5. freeze stopping and analysis before confirmatory data, or use a predeclared anytime-valid rule;
6. retire repeatedly inspected Selection claims from promotion decisions;
7. keep the 2026 Temporal Audit Corpus sealed until the complete policy, feature, training, and analysis protocol is frozen.

The immediate v1 experiment remains Development. A later causal claim needs substantially more independent claims, a fresh Selection Corpus, repeated paired runs where runtime variation matters, and cold reproduction from the immutable Domain Bundle.
