# Domain Docs

Before domain-sensitive work, read:

- `CONTEXT.md` for canonical vocabulary.
- Relevant records under `docs/adr/`.

This is a single-context repository.

Use glossary terms consistently in code, tests, issues, and design documents. When required vocabulary is missing or ambiguous, invoke `domain-modeling`. Surface conflicts with existing ADRs explicitly rather than silently overriding them.

Missing domain documentation is not an error. `domain-modeling` creates glossary entries and ADRs lazily when terms and consequential decisions resolve.
