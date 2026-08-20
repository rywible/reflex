use crate::expr::{BvExpr, MAX_EXPR_DEPTH};
use crate::rewrites::BvCandidate;

/// Feature vector dimension for the bit-vector ranker schema.
pub const BITVEC_FEATURE_DIM: usize = 24;

const MAX_COST_NORM: f32 = 32.0;

/// Stable feature layout (identity-free; candidate index is not encoded).
pub mod layout {
    pub const STATE_DEPTH: usize = 0;
    pub const STATE_COST: usize = 1;
    pub const CAND_DEPTH: usize = 2;
    pub const CAND_COST: usize = 3;
    pub const COST_DELTA: usize = 4;
    pub const EDIT_CLASS_BASE: usize = 5; // 5..9 one-hot (4 classes)
    pub const STATE_HIST_BASE: usize = 9; // 9..19 (10 bins, normalized)
    pub const CAND_HIST_BASE: usize = 19; // 19..=23 uses 5 slots: first 5 cand hist bins
}

pub fn extract_row(state: &BvExpr, candidate: Option<&BvCandidate>) -> [f32; BITVEC_FEATURE_DIM] {
    let mut row = [0.0f32; BITVEC_FEATURE_DIM];
    row[layout::STATE_DEPTH] = state.depth() as f32 / MAX_EXPR_DEPTH as f32;
    row[layout::STATE_COST] = state.cost() as f32 / MAX_COST_NORM;

    let cand_expr = candidate.map(|c| &c.resulting_expr);
    if let Some(c) = candidate {
        row[layout::CAND_DEPTH] = c.resulting_expr.depth() as f32 / MAX_EXPR_DEPTH as f32;
        row[layout::CAND_COST] = c.resulting_expr.cost() as f32 / MAX_COST_NORM;
        row[layout::COST_DELTA] = (c.cost_delta as f32 / MAX_COST_NORM).clamp(-1.0, 1.0);
        let class = c.edit_class.min(3) as usize;
        row[layout::EDIT_CLASS_BASE + class] = 1.0;
    }

    write_histogram(&mut row, layout::STATE_HIST_BASE, state);
    if let Some(expr) = cand_expr {
        write_histogram_partial(&mut row, layout::CAND_HIST_BASE, expr);
        row[layout::CAND_HIST_BASE + 4] = if expr.has_constants() { 1.0 } else { 0.0 };
    }

    row
}

fn write_histogram(row: &mut [f32; BITVEC_FEATURE_DIM], base: usize, expr: &BvExpr) {
    let hist = expr.operator_histogram();
    let total = hist.iter().sum::<u32>().max(1) as f32;
    for (i, count) in hist.iter().enumerate().take(10) {
        if base + i < BITVEC_FEATURE_DIM {
            row[base + i] = *count as f32 / total;
        }
    }
}

fn write_histogram_partial(row: &mut [f32; BITVEC_FEATURE_DIM], base: usize, expr: &BvExpr) {
    let hist = expr.operator_histogram();
    let total = hist.iter().sum::<u32>().max(1) as f32;
    for i in 0..5 {
        row[base + i] = hist[i] as f32 / total;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rewrites::generate_rewrites;

    #[test]
    fn test_feature_golden_stable() {
        let state = BvExpr::Xor(Box::new(BvExpr::Var(0)), Box::new(BvExpr::Var(0)));
        let rewrites = generate_rewrites(&state);
        let row = extract_row(&state, rewrites.first());
        let row2 = extract_row(&state, rewrites.first());
        assert_eq!(row, row2);
    }

    #[test]
    fn test_identity_permutation_invariant() {
        let state = BvExpr::Add(Box::new(BvExpr::Var(0)), Box::new(BvExpr::Const(0)));
        let rewrites = generate_rewrites(&state);
        assert!(rewrites.len() >= 2);
        let row_a = extract_row(&state, Some(&rewrites[0]));
        let row_b = extract_row(&state, Some(&rewrites[1]));
        // State-derived slots match; edit-class / delta differ by candidate semantics only.
        assert_eq!(row_a[0], row_b[0]);
        assert_eq!(row_a[1], row_b[1]);
        assert_eq!(row_a[9..19], row_b[9..19]);
    }
}
