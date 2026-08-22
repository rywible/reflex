# Make Lean Artifacts replay-complete

The Lean Domain Definition stores each theorem Seed as its actual elaborated core proposition and proof body together with declaration name, universe parameters, complete direct statement-and-proof dependencies, allowed axioms, pinned environment identity, Artifact-format revision, Verification Kernel contract revision, and worker-source digest. Derived Candidates retain the Seed declaration identity. Lean's trust-zero kernel—not Rust structural equality—decides whether a Candidate proposition is definitionally equal to its Seed claim.

The imported declaration catalog covers every declaration kind exposed by the pinned environment. Eligibility is dependency-closed: local sorry, unsafe, partial, or unknown state rejects a declaration, and every missing or ineligible dependency rejects all dependents. Structural views are canonical post-order with children before parents and the root last. The persistent worker runs under a hard Linux address-space ceiling that is conservatively charged as resident usage; actual compiler identity, clean pinned source and dependency checkouts, and the worker source digest are validated before use.

## Considered Options

Representing Seeds as theorem constants makes every proof appear to have one node and destroys proof-optimization meaning. Requiring byte-identical propositions rejects valid Lean definitional equality. Theorem-only catalogs cannot establish dependency closure through definitions, recursors, constructors, and axioms. Caller-provided memory estimates do not bound an external verifier. These alternatives all produce attractive but invalid optimization or performance results, so Reflex fails closed instead.
