# Reflex

Reflex is an entirely Rust-implemented, domain-independent system for improving verifiable artifacts through learned, locally executed CPU search.

## Language

**Optimizer**:
A discovery process that starts from a Seed and searches for a Verified Artifact that satisfies the Seed-relative Correctness Claim and is better under an Optimization Goal.
_Avoid_: Generator, synthesizer

**Seed**:
The initial Verified Artifact whose behavior establishes the correctness reference for one Optimization Run.
_Avoid_: Prompt, specification

**Seed Source**:
A reproducible, provenance-preserving source of Verified Artifacts from which Reflex constructs Optimization Runs; it may be a finite corpus or an unbounded deterministic stream.
_Avoid_: Prompt feed, unverified generator, training split

**Seed Scope**:
The caller-selected Seed or reproducible portion of a Seed Source to which an Improvement Session may apply its Optimization Goals.
_Avoid_: Replay Corpus, Campaign, training split

**Verification**:
The mechanical establishment of a Correctness Claim under explicit formal semantics and assumptions. Tests, sampling, benchmarks, and model confidence do not constitute Verification.
_Avoid_: Testing, validation, evaluation

**Verification Kernel**:
The minimal, versioned, domain-supplied authority whose judgment establishes a Correctness Claim under identified semantics and assumptions.
_Avoid_: Test suite, learned verifier, optimizer

**Verification Record**:
The replayable evidence binding a Correctness Claim and its assumptions to the exact semantics and Verification Kernel revision that accepted it.
_Avoid_: Test result, confidence score, benchmark report

**Scientific Confirmation**:
Internal evidence that an empirical claim about Reflex itself passed a previously frozen experiment on sealed data under its declared analysis and decision rules.
_Avoid_: Verification, successful demo, promotion

**Correctness Claim**:
A precise, domain-defined proposition that a Candidate must satisfy relative to its Seed in order to be correct.
_Avoid_: Goal, objective, benchmark

**Measurement**:
A typed observation about a Verified Candidate, recorded with the method and environment needed to interpret it.
_Avoid_: Fitness, reward

**Preference**:
The run-specific priorities and tolerances used to navigate trade-offs among Objectives and incomparable points on a Pareto Frontier.
_Avoid_: Global score, domain preference

**Optimization Goal**:
A caller-supplied declaration of Constraints, Objectives, Preference, and any Success Condition used to determine what desirable improvement means. Reflex cannot begin autonomous improvement without at least one.
_Avoid_: Goal, prompt, Correctness Claim

**Constraint**:
A run-specific eligibility bound on a Measurement that a result must satisfy independently of whether it is correct.
_Avoid_: Correctness condition, invariant

**Objective**:
A Measurement dimension and direction that an Optimization Goal seeks to improve.
_Avoid_: Reward, scalar cost

**Success Condition**:
An optional threshold at which an Optimization Goal is considered sufficiently achieved.
_Avoid_: Verification, stopping budget

**Optimization Run**:
A bounded search that applies one Optimization Goal and Correctness Claim to derivatives of one Seed.
_Avoid_: Training run, generation

**Improvement Session**:
The public unit of autonomous work combining one Domain Definition, one or more caller-supplied Optimization Goals, a Seed Scope, a Resource Envelope, and optional prior Domain Bundle, producing streamed Pareto improvements and an updated bundle.
_Avoid_: Campaign, Optimization Run, experiment

**Campaign**:
A bounded portfolio of Optimization Goals within one domain that shares knowledge and a total search budget.
_Avoid_: Global research process, batch

**Domain Definition**:
The typed, introspectable description of a domain's semantic identity, Artifact representation, Seed Sources, primitive Operators, Verification Kernel, and Measurements that Reflex requires to operate autonomously. Goal templates and specialized features are optional accelerators.
_Avoid_: Search strategy, opaque adapter, plugin

**Semantic Identity**:
The content-addressed identity of a Domain Definition's meaning, including its Artifact encoding, formal semantics, and Verification Kernel contract; a change requires explicit migration before prior knowledge is reused.
_Avoid_: Package version, display name, compatibility guess

**Structural Protocol**:
The common description of typed structure and composition that Reflex searches generically while each Domain Definition retains a specialized physical representation.
_Avoid_: Universal object model, serialization format

**Agenda Graph**:
The Campaign's network of externally grounded Optimization Goals, Milestones, formal Obligations, and their relationships.
_Avoid_: Discovery Graph, task list

**Discovery Graph**:
The persistent network of Artifacts and their derivation, dependency, reuse, and compression relationships.
_Avoid_: Agenda Graph, training dataset

**Experience Ledger**:
The cross-Campaign history of search attempts, outcomes, resource costs, provenance, predictions, and later-attributed effects from which training data can be derived.
_Avoid_: Discovery Graph, training dataset, event log

**Training Target**:
A reproducible, revisable interpretation of Experience Ledger observations used to train a Model Revision, including delayed credit assigned from later consequences.
_Avoid_: Verification, immutable truth, raw event

**Replay Corpus**:
A versioned, diversity-preserving body of Experience Ledger material used to derive Training Targets, including failures, older experience, and recent discoveries.
_Avoid_: Selection Corpus, Scientific Corpus, event log

**Selection Corpus**:
A versioned body of fresh operational evaluation material withheld from training and used for bounded comparison of challenger revisions before its cases rotate into the Replay Corpus.
_Avoid_: Replay Corpus, Scientific Corpus, benchmark leaderboard

**Scientific Corpus**:
A sealed body of internal evaluation material that cannot influence search, training, tuning, or Operational Promotion and exists only for Scientific Confirmation.
_Avoid_: Selection Corpus, public benchmark, training data

**Artifact**:
A verified domain object retained with its provenance and Verification Record as reusable knowledge for future search.
_Avoid_: Candidate, result

**Obligation**:
A formal condition mechanically induced by an Operator or Verifier that must be discharged for a derivation to be correct.
_Avoid_: Optimization Goal, Instrumental Goal

**Milestone**:
A desirable intermediate outcome named in advance but not required for a Goal to succeed.
_Avoid_: Obligation, Success Condition

**Emergent Opportunity**:
An unplanned search direction exposed by discovery that may support one or more Optimization Goals.
_Avoid_: Required Subgoal, Candidate

**Potential**:
A calibrated, contextual forecast over multiple possible downstream outcomes and time horizons from investing further search in an Artifact or Emergent Opportunity.
_Avoid_: Verified value, reward, scalar score

**Knowledge Consolidation**:
The verified reorganization of accumulated Artifacts into more compact, reusable, and search-effective knowledge, potentially including independently Verified generalizations. It may compact cold history and active indexes without invalidating surviving Verification Records or reproducibility claims.
_Avoid_: Model training, data compression, pruning

**Reference Domain**:
A production-quality domain implementation that exercises and demonstrates the complete public Domain Definition contract independently of an external application.
_Avoid_: Toy example, benchmark fixture, application integration

**Primitive Operator**:
A domain-supplied transformation that proposes Candidates from existing search state.
_Avoid_: Derived Operator, verifier, learned policy

**Derived Operator**:
A reusable transformation discovered through Knowledge Consolidation from verified derivations that becomes immediately eligible for search and initial exploration. Later Knowledge Revisions may specialize or deactivate it, while every proposed result remains subject to the Verification Kernel.
_Avoid_: Primitive Operator, unchecked generated code, model action

**Knowledge Revision**:
An immutable, reproducible version of the Discovery Graph and its active knowledge indexes that Campaigns may pin and challengers may replace through promotion.
_Avoid_: Model checkpoint, database snapshot

**Model Revision**:
A complete, immutable, and reproducible learned decision system scoped to one Domain Definition, including its predictors and the state required to interpret them. Campaigns may pin it, challengers may replace it through promotion, and cross-domain transfer requires explicit compatibility.
_Avoid_: Knowledge Revision, live model

**Bootstrap Revision**:
The initial valid Model Revision that provides deterministic, domain-independent search and allocation before learned experience exists.
_Avoid_: Prototype engine, random checkpoint, special runtime

**Runtime Controller**:
The deterministic authority that interprets learned proposals while enforcing Verification, Admission, Operational Promotion, and Resource Envelope rules.
_Avoid_: Learned policy, Verification Kernel, domain adapter

**Specialist Revision**:
A compatible Knowledge Revision or Model Revision retained because its strengths serve a subset of Optimization Goals even though it is not safe to promote as the default.
_Avoid_: Failed challenger, global champion

**Domain Bundle**:
A portable, content-addressed data package of compatible domain knowledge, learned state, retained Experience, provenance, Verification requirements, and recovery state from which autonomous improvement can resume when paired with an installed Domain Definition of matching Semantic Identity.
_Avoid_: Runtime checkpoint, remote registry, model file, executable plugin

**Operational Promotion**:
The automatic, atomic, and rollback-capable adoption of a challenger Knowledge Revision or Model Revision after sufficient evidence shows improvement without violating protected Measurement tolerances. Incomparable challengers may instead become Specialist Revisions.
_Avoid_: Scientific Confirmation, manual approval, unchecked replacement

**External Adoption**:
The caller-authorized application of an exported Artifact to a system outside Reflex after inspecting its Measurements, provenance, and Verification Record.
_Avoid_: Operational Promotion, automatic deployment, Admission

**Resource Envelope**:
The caller-declared hard limits within which Reflex may autonomously allocate computation, memory, elapsed time, and durable storage across search, Verification, training, and Knowledge Consolidation.
_Avoid_: Optimization Goal, scalar reward, stopping prediction

**Protected Allocation**:
A minimum share of a Resource Envelope reserved for an activity whose starvation would compromise safety or long-term learning, while remaining resources are allocated adaptively.
_Avoid_: Fixed schedule, objective weight, predicted demand

**Candidate**:
A proposed derivative of a Seed that has not yet passed Verification.
_Avoid_: Artifact, solution

**Verified Candidate**:
A Candidate that has passed Verification but has not necessarily earned retention.
_Avoid_: Artifact

**Pareto Frontier**:
The set of Verified Candidates for which no other retained candidate is at least as good in every relevant Measurement and better in at least one.
_Avoid_: Best result, leaderboard

**Search Frontier**:
The changing portfolio of Artifacts and Emergent Opportunities currently eligible to receive further search investment, including dominated stepping stones with sufficient Potential.
_Avoid_: Pareto Frontier, Discovery Graph

**Admission**:
The decision to retain a Verified Candidate as an Artifact because it contributes result quality, novelty, reuse, compression, information, or Potential.
_Avoid_: Verification, promotion
