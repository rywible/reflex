# Run from memory with asynchronous durability

Reflex keeps all live Campaign state, graph indexes, Experience Ledger data, verification caches, and learning state in RAM so proposal, checking, and allocation never wait on a database or filesystem. Local disk is used only through asynchronous checkpoints and a recovery tail, preserving valuable verified knowledge across crashes without making storage latency part of the search loop.

## Consequences

Resident memory is an explicit Campaign resource, hot data uses compact arena-backed and content-deduplicated representations, and search workers operate on in-memory indexes. Recovery reconstructs the complete in-memory runtime before work resumes; disposable Campaigns may opt out of durability without changing the execution model.
