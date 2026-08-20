use crate::expr::{BvExpr, ShiftKind};

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BvCandidate {
    pub resulting_expr: BvExpr,
    pub rule_name: String,
    pub cost_delta: i32,
    /// 0=root, 1=left subtree, 2=right subtree, 3=both/synthesis
    pub edit_class: u8,
}

impl reflex_canonical::CanonicalEncode for BvCandidate {
    fn encode_canonical(
        &self,
        out: &mut reflex_canonical::CanonicalWriter,
    ) -> Result<(), reflex_canonical::CanonicalError> {
        self.resulting_expr.encode_canonical(out)?;
        out.write_str(&self.rule_name)?;
        out.write_i32(self.cost_delta)?;
        out.write_u8(self.edit_class)?;
        Ok(())
    }
}

pub fn generate_rewrites(expr: &BvExpr) -> Vec<BvCandidate> {
    let mut candidates = Vec::new();
    let curr_cost = expr.cost() as i32;
    push_root_rewrites(expr, curr_cost, &mut candidates);
    push_recursive_rewrites(expr, curr_cost, &mut candidates);
    dedupe_candidates(candidates)
}

fn push_candidate(
    out: &mut Vec<BvCandidate>,
    resulting_expr: BvExpr,
    rule_name: impl Into<String>,
    curr_cost: i32,
    edit_class: u8,
) {
    if resulting_expr.validate().is_err() {
        return;
    }
    let delta = resulting_expr.cost() as i32 - curr_cost;
    out.push(BvCandidate {
        resulting_expr,
        rule_name: rule_name.into(),
        cost_delta: delta,
        edit_class,
    });
}

fn push_root_rewrites(expr: &BvExpr, curr_cost: i32, out: &mut Vec<BvCandidate>) {
    match expr {
        BvExpr::Xor(a, b) if a == b => {
            push_candidate(out, BvExpr::Const(0), "xor_same_zero", curr_cost, 0);
        }
        BvExpr::Sub(a, b) if a == b => {
            push_candidate(out, BvExpr::Const(0), "sub_same_zero", curr_cost, 0);
        }
        BvExpr::And(a, b) if a == b => {
            push_candidate(out, (**a).clone(), "and_idempotent", curr_cost, 0);
        }
        BvExpr::Or(a, b) if a == b => {
            push_candidate(out, (**a).clone(), "or_idempotent", curr_cost, 0);
        }
        BvExpr::Add(a, b) => {
            if let BvExpr::Const(0) = **b {
                push_candidate(out, (**a).clone(), "add_zero_r", curr_cost, 0);
            } else if let BvExpr::Const(0) = **a {
                push_candidate(out, (**b).clone(), "add_zero_l", curr_cost, 0);
            } else if let (BvExpr::Const(c1), BvExpr::Const(c2)) = (&**a, &**b) {
                push_candidate(
                    out,
                    BvExpr::Const(c1.wrapping_add(*c2)),
                    "add_const_fold",
                    curr_cost,
                    0,
                );
            }
            push_candidate(
                out,
                BvExpr::Add(b.clone(), a.clone()),
                "add_commute",
                curr_cost,
                0,
            );
        }
        BvExpr::Xor(a, b) => {
            if let BvExpr::Const(0) = **b {
                push_candidate(out, (**a).clone(), "xor_zero_r", curr_cost, 0);
            } else if let BvExpr::Const(0) = **a {
                push_candidate(out, (**b).clone(), "xor_zero_l", curr_cost, 0);
            }
            push_candidate(
                out,
                BvExpr::Xor(b.clone(), a.clone()),
                "xor_commute",
                curr_cost,
                0,
            );
        }
        BvExpr::Sub(a, b) => {
            if let BvExpr::Const(0) = **b {
                push_candidate(out, (**a).clone(), "sub_zero_r", curr_cost, 0);
            }
        }
        BvExpr::Shift {
            kind,
            value,
            amount,
        } => {
            if let BvExpr::Const(0) = **amount {
                push_candidate(out, (**value).clone(), "shift_zero_amt", curr_cost, 0);
            }
            if let (BvExpr::Const(v), BvExpr::Const(a)) = (&**value, &**amount) {
                let shifted = match kind {
                    ShiftKind::LogicalLeft => v.wrapping_shl((*a as u32) & 7),
                    ShiftKind::LogicalRight => v.wrapping_shr((*a as u32) & 7),
                };
                push_candidate(
                    out,
                    BvExpr::Const(shifted),
                    "shift_const_fold",
                    curr_cost,
                    0,
                );
            }
        }
        BvExpr::Select(c, t, e) => {
            if let BvExpr::Const(0) = **c {
                push_candidate(out, (**e).clone(), "select_false_branch", curr_cost, 0);
            }
            if let BvExpr::Const(n) = &**c
                && *n != 0
            {
                push_candidate(out, (**t).clone(), "select_true_branch", curr_cost, 0);
            }
            if t == e {
                push_candidate(out, (**t).clone(), "select_branches_equal", curr_cost, 0);
            }
        }
        _ => {}
    }
}

fn push_recursive_rewrites(expr: &BvExpr, curr_cost: i32, out: &mut Vec<BvCandidate>) {
    match expr {
        BvExpr::Add(a, b) => {
            for sub in generate_rewrites(a) {
                push_candidate(
                    out,
                    BvExpr::Add(Box::new(sub.resulting_expr.clone()), b.clone()),
                    format!("left:{}", sub.rule_name),
                    curr_cost,
                    1,
                );
            }
            for sub in generate_rewrites(b) {
                push_candidate(
                    out,
                    BvExpr::Add(a.clone(), Box::new(sub.resulting_expr.clone())),
                    format!("right:{}", sub.rule_name),
                    curr_cost,
                    2,
                );
            }
        }
        BvExpr::Sub(a, b) => {
            for sub in generate_rewrites(a) {
                push_candidate(
                    out,
                    BvExpr::Sub(Box::new(sub.resulting_expr.clone()), b.clone()),
                    format!("left:{}", sub.rule_name),
                    curr_cost,
                    1,
                );
            }
            for sub in generate_rewrites(b) {
                push_candidate(
                    out,
                    BvExpr::Sub(a.clone(), Box::new(sub.resulting_expr.clone())),
                    format!("right:{}", sub.rule_name),
                    curr_cost,
                    2,
                );
            }
        }
        BvExpr::Xor(a, b) => {
            for sub in generate_rewrites(a) {
                push_candidate(
                    out,
                    BvExpr::Xor(Box::new(sub.resulting_expr.clone()), b.clone()),
                    format!("left:{}", sub.rule_name),
                    curr_cost,
                    1,
                );
            }
            for sub in generate_rewrites(b) {
                push_candidate(
                    out,
                    BvExpr::Xor(a.clone(), Box::new(sub.resulting_expr.clone())),
                    format!("right:{}", sub.rule_name),
                    curr_cost,
                    2,
                );
            }
        }
        BvExpr::And(a, b) | BvExpr::Or(a, b) => {
            let ctor = |l: BvExpr, r: BvExpr| match expr {
                BvExpr::And(_, _) => BvExpr::And(Box::new(l), Box::new(r)),
                _ => BvExpr::Or(Box::new(l), Box::new(r)),
            };
            for sub in generate_rewrites(a) {
                push_candidate(
                    out,
                    ctor(sub.resulting_expr.clone(), (**b).clone()),
                    format!("left:{}", sub.rule_name),
                    curr_cost,
                    1,
                );
            }
            for sub in generate_rewrites(b) {
                push_candidate(
                    out,
                    ctor((**a).clone(), sub.resulting_expr.clone()),
                    format!("right:{}", sub.rule_name),
                    curr_cost,
                    2,
                );
            }
        }
        BvExpr::Shift {
            kind,
            value,
            amount,
        } => {
            for sub in generate_rewrites(value) {
                push_candidate(
                    out,
                    BvExpr::Shift {
                        kind: *kind,
                        value: Box::new(sub.resulting_expr.clone()),
                        amount: amount.clone(),
                    },
                    format!("left:{}", sub.rule_name),
                    curr_cost,
                    1,
                );
            }
            for sub in generate_rewrites(amount) {
                push_candidate(
                    out,
                    BvExpr::Shift {
                        kind: *kind,
                        value: value.clone(),
                        amount: Box::new(sub.resulting_expr.clone()),
                    },
                    format!("right:{}", sub.rule_name),
                    curr_cost,
                    2,
                );
            }
        }
        BvExpr::Select(c, t, e) => {
            for (sub, name, class) in [
                (generate_rewrites(c), "cond", 1),
                (generate_rewrites(t), "then", 2),
                (generate_rewrites(e), "else", 2),
            ] {
                for s in sub {
                    let rebuilt = match name {
                        "cond" => {
                            BvExpr::Select(Box::new(s.resulting_expr.clone()), t.clone(), e.clone())
                        }
                        "then" => {
                            BvExpr::Select(c.clone(), Box::new(s.resulting_expr.clone()), e.clone())
                        }
                        _ => {
                            BvExpr::Select(c.clone(), t.clone(), Box::new(s.resulting_expr.clone()))
                        }
                    };
                    push_candidate(
                        out,
                        rebuilt,
                        format!("{name}:{}", s.rule_name),
                        curr_cost,
                        class,
                    );
                }
            }
        }
        _ => {}
    }
}

fn dedupe_candidates(candidates: Vec<BvCandidate>) -> Vec<BvCandidate> {
    let mut seen = std::collections::HashSet::new();
    let mut deduped = Vec::new();
    for c in candidates {
        if seen.insert(c.resulting_expr.clone()) {
            deduped.push(c);
        }
    }
    deduped
}

pub fn simplify_expr(expr: &BvExpr) -> BvExpr {
    let rewrites = generate_rewrites(expr);
    rewrites
        .into_iter()
        .min_by_key(|c| c.resulting_expr.cost())
        .map(|c| c.resulting_expr)
        .unwrap_or_else(|| expr.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rewrite_deterministic_order() {
        let expr = BvExpr::Add(
            Box::new(BvExpr::Var(0)),
            Box::new(BvExpr::Xor(
                Box::new(BvExpr::Var(0)),
                Box::new(BvExpr::Var(0)),
            )),
        );
        let a = generate_rewrites(&expr);
        let b = generate_rewrites(&expr);
        assert_eq!(a, b);
    }
}
