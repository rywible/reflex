# Refill a bounded Candidate inventory

Generation maintains one bounded lookahead inventory rather than materializing its complete target again before every Verification Cohort. The existing generation policy still computes the target from cohort choices, two-cohort parent breadth, remaining Verification allowance, and available resident capacity. The Runtime subtracts already deferred Candidates from that target and generates only the deficit. When deferred inventory meets or exceeds the target, pending Primitive Enumeration Cursors do not advance and the cohort is selected entirely from retained work.

No Candidate is discarded to enforce the bound. Deferred Candidates remain exact restart-complete state with their original Candidate Fates, provenance, and parent references; they drain through ordinary protected, learned, and Bootstrap allocation. Newly admitted parents wait in the round-robin pending queue until inventory creates refill headroom. This preserves the Verification feedback loop while preventing generation throughput from outrunning the expensive authority that supplies its labels.

## Context

The v21 32×8 Lean activation follow-up implemented restart-complete Primitive pagination and generated 460 Candidates across two epochs. Its first cohort Verified 40 Candidates and left 229 policy-deferred Candidates. Before selecting the next eight-Candidate cohort, the Runtime generated another page. It selected 48 Candidates cumulatively but only the first 40 crossed Verification: the conservative pre-dispatch resident reservation refused the compounded heavyweight pool. The Session ended after 72 total Verification requests, using 18.396 GB of its 32 GiB Runtime allowance, and no Model Revision could promote from only 40 labels.

The refusal was correct; relaxing resident accounting would make peak safety depend on optimistic Artifact-size estimates. Dropping deferred Candidates would corrupt exact policy counterfactuals and bias Experience toward whichever alternatives happened to arrive later. Increasing the Resource Envelope would hide a producer-consumer imbalance and violate the fixed-budget breadth comparison. Inventory refill fixes the scheduling cause while preserving all three constraints.

## Consequences

Search may delay newly admitted parents while a large retained pool drains. That is intentional backpressure: their pending cursors remain durable, and every completed cohort reopens inventory capacity. Candidate count is only the policy target; exact retained and transient bytes remain authoritative, so a single heavyweight Candidate can still cause a legitimate Resource Envelope completion.

Runtime revision 16 pins refill scheduling because resuming a revision-15 tail under a different producer policy would change Candidate Fate epochs and allocation order. Lean Development schema v22 preserves v21 as an activation-null before rerunning the identical corpus and budgets.
