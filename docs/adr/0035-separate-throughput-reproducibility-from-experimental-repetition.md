# Separate throughput reproducibility from experimental repetition

Ordinary parallel Campaigns maximize throughput and need not reproduce the exact discovery sequence, but they preserve enough provenance to replay and reverify accepted results. The internal Experimental Harness controls scheduling, randomness, reduction order, and hardware and software metadata when an experiment requires exact repetition.

## Considered Options

Forcing deterministic scheduling on every production Campaign would simplify repetition but constrain parallel throughput. Allowing nondeterminism without replayable accepted evidence would make high performance easy at the cost of attribution and trust.
