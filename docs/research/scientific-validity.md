# Scientific validity for Reflex experiments

Reflex needs two independent kinds of evidence:

- **Verification** establishes that an Artifact satisfies its formal Correctness Claim under recorded semantics and assumptions.
- **Scientific confirmation** establishes an empirical claim about Reflex itself, such as “shared knowledge improves held-out optimization under a fixed resource budget.”

Neither implies the other. A kernel-checked Artifact does not prove that the search method is effective, and a statistically convincing speedup does not make an unverified Artifact correct. Lean exemplifies the first boundary: tactics produce proof terms, but the trusted kernel is what checks them ([Lean Language Reference](https://lean-lang.org/doc/reference/latest/)).

## Canonical experimental language

**Experiment Specification**: An immutable, content-addressed plan frozen before a Confirmatory Run. It states the empirical hypotheses, treatments, Benchmark Corpus revision, Semantic Split, Primary Outcomes, resource budgets, Independent Replicate count, stopping and exclusion rules, analysis, multiplicity handling, and decision thresholds.

**Confirmatory Run**: A run performed exactly against a frozen Experiment Specification without prior access to its outcomes.

**Exploratory Run**: A run used to discover hypotheses, tune systems, inspect failure modes, or design later experiments. Its findings may motivate but cannot constitute a preregistered confirmation.

**Protocol Deviation**: Any departure from an Experiment Specification. Deviations are retained and reported; affected analyses become exploratory unless the specification's predeclared contingency applies.

**Semantic Split**: Assignment of whole semantic equivalence or ancestry groups—not surface expressions—to disjoint corpora.

**Development Corpus**: Instances that may influence implementations, hyperparameters, heuristics, models, Knowledge Revisions, thresholds, or experimental design.

**Selection Corpus**: Held-out instances used for routine champion–challenger selection. Repeated use makes this development data for scientific purposes.

**Sealed Audit Corpus**: Instances unavailable to all search, training, consolidation, tuning, and selection processes until a Confirmatory Run. Exposure consumes the seal for future confirmation of related designs.

**Independent Replicate**: One fresh execution of the complete experimental unit, with a new process and preassigned random streams. Candidates or verifier calls inside one execution are not independent replicates.

**Primary Outcome**: A predeclared Measurement-derived endpoint used to decide the empirical hypothesis. It is not a scalar reward imposed on Reflex's multi-objective domain.

**Null Result**: A completed registered comparison that does not meet its decision threshold, including an inconclusive result. It remains part of the evidence.

**Operational Promotion**: Replacement of a champion using the Selection Corpus for practical progress.

**Confirmed Promotion**: A promotion whose predeclared empirical claim also passed a fresh Sealed Audit Corpus. This designation concerns system performance, not Artifact correctness.

**Reproduction Bundle**: The immutable inputs, provenance, raw outputs, and commands needed for a cold replay.

## Required protocol

### 1. Freeze the question before seeing confirmatory outcomes

Commit and hash an Experiment Specification before unsealing audit data. At minimum, it must declare:

- The directional empirical hypothesis and smallest effect worth acting on.
- Champion, challenger, baselines, ablations, and all tuning allowed for each.
- Corpus generation, Semantic Split, and immutable corpus hashes.
- Primary Outcomes, fixed targets/reference points, aggregation, uncertainty method, and promotion rule.
- Equal resource envelopes, replicate count and rationale, seed schedule, stopping rule, timeouts, and exclusions.
- The family of confirmatory comparisons and its multiple-comparison correction.

Preregistration distinguishes planned tests from analyses suggested by observed outcomes and reduces hidden analytic flexibility ([Nosek et al., 2018](https://doi.org/10.1073/pnas.1708274114)). In particular, a stopping rule must be chosen before observations begin; optional stopping and selective condition reporting can inflate false-positive findings ([Simmons et al., 2011](https://doi.org/10.1177/0956797611417632)). Reflex may explore continuously, but only frozen protocols produce confirmatory evidence.

### 2. Split by semantics and information ancestry

For the bit-vector experiment, compute the semantic-function digest (the exhaustive truth table) before splitting. Every syntactic expression with that digest, every derivative Artifact, and every experience or reusable rewrite specific to that function belongs to the same split. Group related generator families as well when shared templates would make the audit task predictable.

Use three immutable partitions:

1. Development Corpus for engineering and training.
2. Selection Corpus for routine Operational Promotion.
3. Sealed Audit Corpus for infrequent Confirmed Promotion and external claims.

A model, archive, Macro Operator, Knowledge Revision, feature normalizer, threshold, or human decision influenced by audit instances is contaminated and must not be evaluated as held out. This is learn–predict separation applied to Reflex's entire external knowledge substrate, not only neural weights ([Kaufman et al., 2012](https://doi.org/10.1145/2382577.2382579)). Repeated adaptive inspection also overfits a conventional holdout, so a Selection Corpus cannot double as a permanent audit set ([Dwork et al., 2015](https://doi.org/10.1126/science.aaa9375)).

### 3. Compare paired work under equal economic constraints

Run every treatment on identical Campaign instances and a preassigned paired seed schedule, starting from the same permissible Knowledge Revision and cache state. Randomize or interleave treatment order. Give all methods the same predeclared limits on:

- Verifier calls and other domain operations.
- CPU-seconds, wall time, worker/thread count, and peak resident memory.
- Training, tuning, archive construction, and consolidation expense.

No single resource is a complete definition of fairness. Report both search-normalized results (for example, quality per verifier call) and end-to-end results under a fixed CPU/memory envelope. Preserve anytime curves rather than selecting a favorable cutoff after the run. COCO similarly treats function evaluations as a central runtime measure and evaluates optimization methods against fixed targets across problem instances ([Hansen et al., 2021](https://doi.org/10.1080/10556788.2020.1808977)).

### 4. Use competitive baselines and causal ablations

The initial bit-vector study should include:

- Uniform/random Opportunity allocation.
- The best fixed hand-authored heuristic.
- Strong non-learned search, including best-first/beam and evolutionary search.
- Equality saturation when the rule set makes it applicable.
- Shared versus isolated Discovery Graphs.
- Knowledge Consolidation on versus off.
- Learned allocator on versus off while everything else remains fixed.
- Warm verified knowledge versus an empty Archive.

Tune every competing method with the same Development Corpus access and tuning budget. An ablation changes one claimed mechanism at a time. Report all attempted baselines, their versions, tuning spaces, and tuning costs; do not retain only baselines the challenger beats. The NeurIPS reproducibility program and checklist require training details, hyperparameter-selection methods, baselines, repeated-run uncertainty, and compute disclosure ([Pineau et al., 2021](https://www.jmlr.org/papers/v22/20-303.html), [NeurIPS checklist](https://neurips.cc/public/guides/PaperChecklist)).

### 5. Replicate the actual source of randomness

Choose the replicate count from a predeclared precision or power target, using pilot data from the Development Corpus only. Assign and publish all master seeds before the Confirmatory Run; derive separate deterministic streams for corpus generation, search, model initialization/training, and scheduling. Never discard an unfavorable seed or add trials “until significant.”

The unit used for uncertainty must match the intended generalization claim. If the claim is about unseen semantic functions, resample or model variation across semantic functions and independent Campaign executions—not millions of correlated Candidates. Random initialization and environment variation can materially change apparent algorithm rankings ([Henderson et al., 2018](https://doi.org/10.1609/aaai.v32i1.11694)).

### 6. Report magnitude, uncertainty, and multiplicity

Publish raw per-instance paired outcomes and distributions. For each Primary Outcome, report a practical effect size and a confidence interval with its method and captured sources of variation. Useful multi-objective endpoints include:

- Probability or fraction of strict Pareto improvement.
- Time or verifier calls to fixed targets, with failures retained as censored outcomes.
- Frontier coverage or hypervolume against a reference point frozen from development data.
- Goals improved per CPU-second.
- Useful retained knowledge per resident byte.

Do not collapse these into an undeclared universal score. Predeclare which endpoints control promotion, their tolerances, and how regressions are handled. If inferential tests cover several algorithms, outcomes, or targets, define the family in advance and control family-wise error (for example, Holm's sequential procedure) rather than promoting on whichever comparison happens to pass ([Holm, 1979](https://doi.org/10.2307/4615733)). Mark all other comparisons exploratory. Across multiple benchmark instances, use paired/non-parametric comparisons where their assumptions fit and show the per-instance results ([Demšar, 2006](https://www.jmlr.org/papers/v7/demsar06a.html)).

### 7. Treat wall-clock performance as an experiment

Abstract operation counts and formal cost models are Measurements distinct from observed hardware performance. For timing runs:

- Use a quiet, dedicated machine; pin affinity, thread count, and relevant power/frequency settings where possible.
- Fix warm-up, compilation, cache, and process-start policy before the run.
- Randomize contender order and repeat with independent process launches.
- Retain every raw timing sample; never report only the best run.
- Record CPU model/topology, microcode, RAM, OS/kernel, Rust toolchain, compiler flags, dependencies, and relevant system settings.

Small environmental changes can reverse a systems-performance conclusion, and setup randomization helps detect or avoid that bias ([Mytkowicz et al., 2009](https://doi.org/10.1145/1508244.1508275)). Hierarchical repetition and effect-size confidence intervals provide a rigorous way to quantify noisy performance changes ([Kalibera and Jones, 2013](https://doi.org/10.1145/2464157.2464160)).

### 8. Make every result replayable

The Experience Ledger must resolve every reported datum to:

- Experiment Specification, code commit, binary, and dependency-lock hashes.
- Domain semantics, Verifier, axioms/trust basis, model, and Knowledge Revision identifiers.
- Corpus generator/version, split manifest, instance digests, and all random seeds.
- Full configuration, machine environment, raw measurements, Verification evidence, exclusions, and Protocol Deviations.

Ship an exact command that reconstructs the in-memory Runtime from a cold state and reproduces tables from raw records. Automated provenance should describe which entities and activities produced each result; W3C PROV provides a standard vocabulary for those relationships ([PROV-O](https://www.w3.org/TR/prov-o/)). The NeurIPS checklist likewise asks for the exact command/environment and resource details needed to reproduce experimental results ([NeurIPS checklist](https://neurips.cc/public/guides/PaperChecklist)).

### 9. Retain negative evidence

Store and report every registered run: no-improvement Campaigns, regressions, Refuted and Unknown Candidates, timeouts, resource exhaustion, failed promotions, and specified exclusions. `Unknown` is neither correctness failure nor evidence of absence. Unexpected analyses and reruns remain valuable, but are labeled Exploratory and linked to the original protocol. Outcome-independent reporting is the point of Registered Reports and protects negative results from disappearing after outcomes are known ([Chambers, 2013](https://doi.org/10.1016/j.cortex.2012.12.016)).

### 10. Gate promotion and replay correctness separately

An Operational Promotion may use the Selection Corpus, but it must still use paired equal-budget trials and a written decision rule. A Confirmed Promotion additionally requires:

- A frozen Experiment Specification and previously sealed audit revision.
- Every promotion-critical threshold met with its predeclared uncertainty rule.
- No unpermitted regression on protected Outcomes or Constraints.
- Zero Verification failures among retained Artifacts.
- A second cold replay that reproduces the decision from the Reproduction Bundle.

After audit outcomes influence development, retire that audit revision and construct a newly sealed one for future confirmation.

Every retained Artifact is also replayed independently of this empirical gate: reconstruct its exact Correctness Claim, semantics, assumptions, and proof/certificate/exhaustive evidence; invoke the pinned trusted Verifier from a clean process; and retain the result and hashes. Use an independently implemented checker where available. Formal evidence verifies the encoded claim, not whether humans chose the right claim, so specification review remains necessary.

## Minimal validity gate for any published claim

A Reflex result is scientifically reportable only if reviewers can answer **yes** to all of these:

1. Was the empirical claim and stopping/analysis rule frozen before confirmatory outcomes were visible?
2. Was the audit data separated by semantic identity and information ancestry?
3. Did contenders receive paired instances and equal, fully disclosed resource budgets?
4. Were strong baselines, causal ablations, and all registered outcomes reported?
5. Were there enough genuinely independent replicates, with all seeds retained?
6. Are practical effects, uncertainty, and multiple comparisons handled as specified?
7. Are performance conditions controlled and raw measurements preserved?
8. Can the result be reproduced cold from immutable provenance?
9. Does every retained Artifact separately replay through its trusted Verifier?

If any answer is no, report the work as exploratory evidence—not as scientific confirmation.
