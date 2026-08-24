# Export portable Domain Bundles

The durable product of autonomous improvement is a portable, content-addressed Domain Bundle containing compatible domain schemas, a Knowledge Revision, a Model Revision, retained Experience Ledger state, provenance, Verification requirements, and the recovery state needed to resume autonomous improvement. Bundles move through explicit local files rather than an implicit network registry, and imports establish compatibility and replay required Verification before trusting their Artifacts.

## Considered Options

An opaque Runtime checkpoint would simplify persistence but couple learned capability to one process layout and prevent deliberate reuse across machines or applications. A result-only export would be portable but could not continue learning without reconstructing lost experience and recovery state. A managed remote registry would ease distribution but violate Reflex's local-first operating model.
