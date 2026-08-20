# Atomic Evidence Bundle

Normative native v1 specification for the `bundle:1` compatibility component
(ADR 0014).

An evidence bundle is the only durable publication boundary for a native run.
Its canonical manifest names the exact build and compatibility line, experiment
and cell manifests, accepted attempts and epochs, ledger segments, every
reachable arena artifact, verifier receipts, model/knowledge identities,
datasets, query plans, populations, units, and a sorted exhaustive member
inventory with lengths and BLAKE3 digests.

Publication is fail-closed:

1. freeze the accepted-attempt view and arena roots;
2. drain authoritative ledger buffers to a declared cut;
3. write members and the canonical manifest to a sibling temporary path;
4. reject missing, extra, duplicate, mis-sized, or digest-mismatched members;
5. flush and fsync files and the staged directory;
6. atomically rename to the digest-derived final name and fsync the parent;
7. reopen and verify the final bundle before recording publication success.

Readers ignore staging paths. A crash before rename cannot publish evidence; a
crash after rename leaves a complete replay root. Completion additionally
requires zero arena pins, buffers, writers, child processes, requests, and
staging paths owned by the experiment.
