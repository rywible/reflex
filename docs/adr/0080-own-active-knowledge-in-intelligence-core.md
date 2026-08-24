# Own active Knowledge in IntelligenceCore

`IntelligenceCore` is the sole authority for the active immutable Knowledge Revision. The Runtime Controller supplies verified derivation observations, resource-bounded execution, Verification Kernel decisions, Shadow Campaign results, and durable publication. It does not maintain, activate, or persist a second mutable `KnowledgeState`.

## Considered Options

Keeping an active Knowledge Revision in both Runtime and `IntelligenceCore` made their agreement a recoverable-state invariant. A crash could expose one authority before the other, promotion required duplicated mutation, and a valid checkpoint could still be semantically ambiguous about which revision governed the next Campaign.

Moving Verification, Resource Envelope accounting, activation timing, or durability into the compiler would instead give an advisory Module authority it must not possess. Exposing compiler recipes and promotion gates to Runtime would also make the seam shallow and force Runtime to understand compiler lifecycle mechanics.

## Decision

The current Runtime v25 Bundle carries a Revisions segment v7 containing exactly one authenticated `IntelligenceCore` checkpoint. The Core checkpoint authenticates its Knowledge Compiler, active Knowledge Revision, Model Ecology, causal Experience, and Runtime Policy. No standalone Knowledge payload is encoded. Runtime v24 added operational-action provenance and a separate causal repair parent without replacing the verified Artifact parent used by feature replay and correctness recovery. Runtime v25 adds an authenticated generation-phase bit: a crash after the action barrier resumes the already retained complete generation prefix at Selection instead of rerunning generation, while a crash before the barrier resumes from the preceding checkpoint.

Knowledge work crosses one private reactor seam:

1. Runtime passes verified derivation observations and the current Root and Pareto identities to `IntelligenceCore`.
2. Core constructs compiler strategy identities, chooses roots, interprets broad or focused mining, and returns an opaque bounded work item containing only the product and explicit Verification obligations needed for execution. Its preflight derives a conservative resident bound from the exact observation count, identity bytes, and fully expanded primitive or Derived-Operator steps; Runtime never substitutes a per-observation magic constant.
3. Runtime reproduces every candidate witness. An unavailable witness produces a Core-owned planning-failure transition; it never becomes an unrecorded retry loop.
4. Runtime preflights the aggregate Resource Vector, obtains one typed Verify Investment receipt per canonical obligation, and asks Core to stage one bounded Open transition containing the complete batch and reservation.
5. Runtime gives the staged transition to one private prepared-publication primitive. It computes and reserves the complete peak bound without allocation, reserves Experience capacity, and materializes one checkpoint. A proposed immutable view supplies that checkpoint's checksum and component identities to both usage-dependent Bundle seals without restoring a second Core. The primitive transfers the final allocation to the durability writer without cloning, crosses the barrier, and only then performs an infallible move-only commit into the live Core before any Verification request dispatches.
6. Runtime submits the exact opened batch to the installed Verification Kernel and returns authoritative, request-bound settlements to Core. Batch CPU and requests are additive; wall time and peak resident memory are attributed once to the canonical leader.
7. Core validates identity, order, resources, verdict coverage, and the opened revision, then stages compiler, causal, and policy consequences in one immutable terminal transition.
8. Runtime publishes the terminal transition through the same primitive and swaps it only after the second barrier. A crash after Open recovers the same reservation and terminally invalidates the interrupted batch without replaying or releasing requests.

A Completed Consolidate action and its exact provisional challenger are staged in one Core transition. A crash after that barrier therefore resumes the retained challenger and its obligations; it neither reruns mining nor silently loses the product. Later compression credit follows the producing Consolidate decision recorded on that challenger rather than whichever consolidation action happens to be current at promotion time.

Shadow Open reserves and charges the complete symmetric two-arm Verification-request sub-envelope before either arm dispatches. Each arm charges its retained live graph before growth, subtracts that graph from the Kernel allowance, and reports external-worker CPU, elapsed time, and peak resident memory as part of the exact arm Resource Vector.

A verified challenger is not active merely because its obligations passed. Core owns eligibility and promotion mechanics; Runtime owns the Shadow Campaign execution and the authority to publish the resulting transition. Campaigns and Cohorts use an immutable revision pinned from Core, never a mutable compiler view.

## Migration

Completed Runtime v24, v23, v22, v21, and v20 Bundles are decoded only through their frozen segment formats. Runtime v24 defaults the absent generation-phase bit to false; interrupted v24 execution is incompatible because its exact barrier side is not encoded. Runtime v23 Experience has Rejection Advisories but no action/repair causal parent and migrates that field to absent. Earlier standalone Knowledge state is validated against the recovered Artifacts, Experience derivations, primitive Operators, and revision header, then consumed exactly once by an eligible migrated Core. The standalone value is discarded before the v25 checkpoint is sealed.

For a legacy Core checkpoint, recovery authenticates the raw legacy checkpoint identity against the outer Revisions header before restoring it. Restore then migrates the state to the current canonical Core format, and the next publication atomically reseals that format. Recovery never compares a migrated checkpoint identity with the header that authenticated its legacy encoding, and it never appends a current-format delta behind a legacy-format base.

Current-v25 Core state cannot be overwritten through the legacy import path. Interrupted legacy Bundles remain incompatible because importing only one side of partially completed work would not be restart-complete. A failed import, replay, or publication leaves its source byte-identical.

Resume and Fork do not trust authenticated Core bytes as semantic correctness evidence. The Core supplies an authenticated manifest for every retained Verified or Promoted Knowledge obligation, bound to the exact Core revision and Knowledge root. Before Seed reading, Runtime reserves the bounded manifest, witness reconstruction, Operator scratch, Kernel request, and ordered-result working sets and charges the exact replay request count. It reconstructs each witness from retained support and requires indexed, evidence-bound Kernel acceptance for the entire manifest. A structural or semantic mismatch rejects recovery without publishing over the source; an external worker interruption durably preserves the already-charged setup state.

## Consequences

- There is one restart-complete Knowledge authority and one authenticated lineage.
- Runtime can evolve execution, scheduling, and resource policy without depending on compiler recipes or promotion internals.
- The compiler can evolve products and obligation kinds without expanding a public interface; new domain adapters implement witness reproduction and Verification Kernel submission behind Runtime.
- Shadow treatment and control must be derived from Core-provided immutable manifests or pins. They cannot activate a revision.
- Bundle migration code remains deliberately versioned and removable only when support for the frozen legacy formats is retired by a later decision.
