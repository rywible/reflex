# Linearize partial Preferences before Candidate sorting

Optimization Goals and equal-priority metric tiers define a partial Preference, not a comparator suitable for a general-purpose sort. Incomparable parents can make pairwise equality non-transitive, and passing that relation directly to Rust's sort is invalid. The Runtime therefore computes a deterministic linear extension before Candidate ranking.

Each Search Frontier parent retains a domination count under the complete Goal Set. Parents are ordered by fewer dominators, then by their stable Frontier position. This preserves strict Pareto preference without scalarizing Measurement values; incomparable parents receive only a deterministic operational order. Candidate comparators use the resulting integer parent rank and ordinary stable tie fields, so they implement a total order.

The domination index is incremental. A newly admitted parent is compared once with each retained parent, counts are updated, and the compact ranks are reused until the Search Frontier changes. Repeated Candidate cohorts do not rebuild a quadratic table. Runtime revision 14 pins this scheduling behavior.

This order is advisory. Verification, Admission, Pareto retention, and Measurement semantics continue to use their domain-defined correctness and partial-order rules directly.
