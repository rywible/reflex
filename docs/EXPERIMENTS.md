# Reflex Experimental Standard

This is the normative internal standard for empirical claims about Reflex. It governs development, evaluation, and promotion of the library itself; it is not part of the consumer-facing Reflex interface and imposes no experimental workflow on library users.

The evidence and primary-source rationale behind this standard are recorded in [Scientific validity for Reflex experiments](./research/scientific-validity.md).

## Two independent standards

- **Verification** establishes that an Artifact satisfies its formal Correctness Claim under recorded semantics and assumptions.
- **Scientific Confirmation** establishes an empirical claim about Reflex, such as whether shared knowledge improves held-out optimization under a fixed resource budget.

Neither implies the other. Artifact Verification is replayed independently of empirical evaluation.

## Internal language

**Experiment Specification**: An immutable, content-addressed plan frozen before confirmation. It declares the hypothesis, treatments, corpora, Primary Outcomes, budgets, independent replicate count, stopping and exclusion rules, analysis, multiplicity handling, and decision thresholds.

**Exploratory Run**: A run used to develop the system, tune it, investigate failures, or form hypotheses. Its outcomes cannot confirm claims suggested by those same outcomes.

**Confirmatory Run**: A run executed exactly against a frozen Experiment Specification without prior access to its outcomes.

**Protocol Deviation**: A departure from the Experiment Specification. It is retained and reported; the affected analysis becomes exploratory unless a predeclared contingency applies.

**Semantic Split**: Assignment of complete semantic-equivalence and information-ancestry groups to disjoint corpora.

**Development Corpus**: Data allowed to influence implementation, training, tuning, heuristics, Knowledge Revisions, thresholds, and experiment design.

**Selection Corpus**: Held-out data used for routine champion-challenger decisions. Repeated use makes it development data for scientific purposes.

**Sealed Audit Corpus**: Data unavailable to all search, training, consolidation, tuning, selection, and human decision-making until a Confirmatory Run. Exposure consumes its seal for related future claims.

**Independent Replicate**: A fresh process-level execution of the complete experimental unit with preassigned independent random streams. Candidates within one Campaign are not independent replicates.

**Primary Outcome**: A predeclared Measurement-derived endpoint used to judge the empirical hypothesis, not a reward imposed on Reflex's multi-objective search.

**Null Result**: A completed registered comparison that does not meet its decision threshold or remains inconclusive.

**Operational Promotion**: A practical champion replacement selected using the Selection Corpus.

**Confirmed Promotion**: An Operational Promotion whose predeclared empirical claim also passed a fresh Sealed Audit Corpus.

**Reproduction Bundle**: The immutable inputs, provenance, raw outputs, environment, and commands needed for a cold replay.

## Mandatory practice

1. Freeze and hash the Experiment Specification before exposing confirmatory outcomes.
2. Separate corpora by semantics and information ancestry, not surface syntax or random rows.
3. Run paired treatments on identical Campaign instances under equal, disclosed verifier, CPU, wall-time, thread, memory, training, and consolidation budgets.
4. Compare against competitive tuned baselines and one-mechanism-at-a-time ablations.
5. Choose the number of Independent Replicates from a predeclared precision or power target using only development data; publish every assigned seed.
6. Predeclare Primary Outcomes, practical decision thresholds, uncertainty methods, stopping rules, reference points, and multiple-comparison handling.
7. Publish full paired distributions, effect sizes, uncertainty intervals, anytime curves, and Pareto results rather than only favorable aggregates.
8. Retain crashes, timeouts, regressions, Refuted and Unknown Candidates, failed promotions, exclusions, Protocol Deviations, and Null Results.
9. Record enough immutable provenance to reconstruct every reported datum and reproduce the analysis from a cold process.
10. Replay every promoted Artifact separately through its pinned trusted Verifier.

## Evaluation isolation

The internal Experimental Harness runs outside the Reflex Runtime module and uses the same public interface as a consumer. It creates isolated processes, pins Runtime and Knowledge Revisions, enforces budgets, controls corpus access, collects raw results, and emits Reproduction Bundles.

Evaluation has three explicit modes:

- **Frozen**: the evaluated Runtime cannot learn from held-out Campaigns.
- **Online adaptation**: learning is part of the treatment, every replicate starts from the same frozen revision, all adaptation resources count against the budget, and evaluation state is discarded afterward.
- **Production**: outcomes may enter persistent domain memory and therefore cannot remain held out.

No audit Artifact, Experience, Knowledge Revision, normalization statistic, model update, or human observation may flow into development while its corpus remains sealed.

## First bit-vector claim

The initial confirmatory hypothesis is:

> Under equal CPU, memory, and verifier-call budgets, shared and consolidated Reflex improves previously unseen bit-vector semantic functions more effectively than isolated non-learned search.

Its independent experimental unit is an entire Campaign. Semantic-function digests and generator ancestry define the split groups. The final Experiment Specification must fix the smallest worthwhile effect, baselines, ablations, Primary Outcomes, replicate count, seed schedule, and promotion rule using Development Corpus pilots before the Sealed Audit Corpus is exposed.

## Reporting rule

Until an empirical claim passes a previously sealed Confirmatory Run, label it **exploratory** regardless of how impressive it appears. Scientific Confirmation applies to the stated empirical claim only; it never upgrades measured performance into Verification.
