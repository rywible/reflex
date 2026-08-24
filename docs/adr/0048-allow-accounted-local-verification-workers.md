# Allow resource-accounted local Verification workers

Domain capabilities execute in-process by default, but a Verification Kernel may own pinned local domain-native worker processes when the domain's actual trusted authority cannot be safely embedded in Rust. The Runtime grants each batch an allowance and charges worker lanes, CPU, resident memory, deadlines, and failures to the same Resource Envelope; the worker may verify and return evidence but owns no search, ranking, Admission, training, scheduling, or persistence behavior.

## Considered Options

Reimplementing the Lean kernel in Rust would change the trusted authority rather than integrate Lean, while linking Lean through native FFI would violate the safe Rust implementation constraint and enlarge the failure boundary. Unaccounted subprocesses would make hard Resource Envelopes and equal-budget experiments false, so the process exception is inseparable from typed allowance and usage accounting.
