# Count moved search state once during Verification admission

The pre-Verification Resource reservation is recomputed from the post-selection ownership graph. Stable Runtime state, the prospective deferred Search Tail, selected Candidate storage, generation temporaries, Candidate Fates, the existing checkpoint, the next checkpoint buffer, and pending durability bytes are charged as distinct allocations. Artifact payloads moved from the prior deferred vector into selected and prospective-deferred vectors are charged once in their new owner, not once through the stale pre-epoch state and again through the prospective state.

Rust moves a `ProposedCandidate` and its domain Artifact without cloning the Artifact's dynamic heap. `Vec::append` and the selection partitions may allocate new element buffers, which are charged separately, but they transfer each Artifact payload. Treating the old deferred payload as still live made a conservative bound mathematically false rather than merely cautious. The Runtime continues to reserve both checkpoint buffers because they genuinely coexist until the next durability barrier, and it retains conservative pipeline reserves for selected Candidates becoming Experience and admitted Artifacts.

## Context

Backpressure allowed the v22 32×8 Lean training run to execute 41 epochs and kernel-Verify 352 Candidates across all 32 correctness claims. It admitted 25 strict improvements across 23 claims. On the next eight-Candidate cohort, selection completed but Verification did not start. The admission formula began with `resident_before_epoch`, which included the complete old deferred Candidate payload, then added the complete prospective deferred payload after those same Candidates had been moved through selection. As the retained proof terms and Experience Ledger grew, the false duplicate charge crossed the 32 GiB Runtime allowance even though observed peak residency was 19.734 GB.

Weakening the Resource Envelope or its safety factor is not acceptable. Recomputing the ownership graph removes only the impossible overlap and keeps every allocation that can coexist. Small epoch context vectors and the frontier-key index are now named explicitly in the transaction reservation rather than being hidden in the obsolete prior-state base.

Runtime revision 17 pins this admission policy because a revision-16 Resume could otherwise advance Candidate Fate and Verification order past a boundary where its original Runtime would stop. Lean Development schema v23 preserves v22 as an activation-null before one identical retry.
