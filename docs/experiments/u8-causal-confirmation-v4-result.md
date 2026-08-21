# Unary `u8` causal confirmation v4: recovery deviation

Status: **Protocol Deviation — no confirmatory claim**

All forty preregistered assignments completed on 2026-08-21 at git revision `fd2236cb62e65487703e3451f52c73af27b27610`. Every evaluation consumed the exact 10,500-request, one-worker envelope and produced all 8,190 audit-origin Pareto Artifacts. Completed recovery nevertheless rejected all twenty bundles whose treatments retained Derived Operators (`full` and `no-model`) as corrupt; all `no-derived` and `bootstrap` bundles recovered successfully.

Because valid recovery was mandatory, no paired contrast was computed. V4 makes no confirmatory or null-effect claim.

## Retained evidence

- Experiment Specification SHA-256: `0740f47b468148090e13f7e81ced137c38c21ddb2f4906cac2305e58192d60fb`
- Audit Corpus SHA-256: `0eae44e4ca7a2ba050f02ab87c0e2de27fecde457d8744636e29afdd6d404787`
- Report content SHA-256: `28fcc68bbe29b340dd4f2a62a0430c41eb5753aa75624e2a5c98e43a4137fb42`
- Original uncompressed report file SHA-256: `f2aaf16d5d67f789f937a4f26edb39a7d1252778d8afeecb20a74197f221e5c0`
- Deterministically compressed report file SHA-256: `b122b8d2de9b0b24a097d3a2ead9a85319c1ead4779c0c9d2c659d55355e065e`
- Consumed audit artifact file SHA-256: `6df55be8298c98dab70474e5e94a27b0eb72f777d2fde40f900e09b72b4965b0`
- Full training bundle SHA-256: `c710799142ea58fe26f065e32af762b33060482ab2dcc72d79a485aaab5bf22f`
- Full Knowledge Revision: `9743f1771c98d355b17908c6e25a9ec5b93670a9aa7d3a8ebf92c5331c8400fd`
- Full Model Revision: `5cac5293745e5276b63df289fb1e3fc447fe44a13d1b977a1e63181fb0e97fed`

The complete report is [`u8-causal-confirmation-v4.json.gz`](./u8-causal-confirmation-v4.json.gz). All 81,900 exposed semantic groups are retained in [`u8-causal-confirmation-v4-consumed-audit.json`](./u8-causal-confirmation-v4-consumed-audit.json) and are Development Corpus for later experiments.

## Root cause and correction

The v3 correction allowed predecessor counters to be historical prefixes, but it still assumed the current champion summarized the entire current ledger. In these 8,190-root Campaigns, the 4,096-entry active-knowledge cap correctly prevents a new Knowledge Revision from being promoted. The unchanged pre-Campaign champion therefore records `0/0` Derived Operator evidence while the later ledger contains 341 accepted trials. Recovery incorrectly treated this legitimate state as corruption.

Knowledge Revisions now encode an explicit ledger-attempt watermark. Recovery validates each immutable revision exactly against the append-only Experience prefix it claims to summarize, including exact evidence counters, verified support ancestry, semantic diversity, and activation decisions. It also accepts any matching verified parent derivation rather than depending on one arbitrary derivation for an equivalent parent Artifact.

An explicit release-mode gate now trains the full stack, evaluates one complete 8,190-Seed consumed-v4 replicate, and successfully reopens the resulting Derived-Operator bundle. A successor confirmation requires another post-format-change baseline and entirely fresh audit semantic groups disjoint from v1 through v4.
