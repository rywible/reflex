use reflex_canonical::{CanonicalEncode, CanonicalError, CanonicalWriter};
use serde::{Deserialize, Serialize};

/// Maximum AST depth accepted by the bounded bit-vector language.
pub const MAX_EXPR_DEPTH: u32 = 8;

/// Maximum variable index (inclusive); arity is `max_var_index + 1`.
pub const MAX_VAR_INDEX: u8 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ShiftKind {
    LogicalLeft,
    LogicalRight,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BvExpr {
    Var(u8),
    Const(u8),
    Add(Box<BvExpr>, Box<BvExpr>),
    Sub(Box<BvExpr>, Box<BvExpr>),
    Xor(Box<BvExpr>, Box<BvExpr>),
    And(Box<BvExpr>, Box<BvExpr>),
    Or(Box<BvExpr>, Box<BvExpr>),
    Shift {
        kind: ShiftKind,
        value: Box<BvExpr>,
        amount: Box<BvExpr>,
    },
    Select(Box<BvExpr>, Box<BvExpr>, Box<BvExpr>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BvValidateError {
    OverDepth { depth: u32, max: u32 },
    OverVarIndex { index: u8, max: u8 },
}

impl BvExpr {
    pub fn eval(&self, env: &[u8]) -> u8 {
        match self {
            BvExpr::Var(idx) => env.get(*idx as usize).copied().unwrap_or(0),
            BvExpr::Const(val) => *val,
            BvExpr::Add(a, b) => a.eval(env).wrapping_add(b.eval(env)),
            BvExpr::Sub(a, b) => a.eval(env).wrapping_sub(b.eval(env)),
            BvExpr::Xor(a, b) => a.eval(env) ^ b.eval(env),
            BvExpr::And(a, b) => a.eval(env) & b.eval(env),
            BvExpr::Or(a, b) => a.eval(env) | b.eval(env),
            BvExpr::Shift {
                kind,
                value,
                amount,
            } => {
                let v = value.eval(env);
                let shift = (amount.eval(env) as u32) & 7;
                match kind {
                    ShiftKind::LogicalLeft => v.wrapping_shl(shift),
                    ShiftKind::LogicalRight => v.wrapping_shr(shift),
                }
            }
            BvExpr::Select(cond, then_e, else_e) => {
                if cond.eval(env) != 0 {
                    then_e.eval(env)
                } else {
                    else_e.eval(env)
                }
            }
        }
    }

    pub fn cost(&self) -> u32 {
        match self {
            BvExpr::Var(_) | BvExpr::Const(_) => 1,
            BvExpr::Add(a, b)
            | BvExpr::Sub(a, b)
            | BvExpr::Xor(a, b)
            | BvExpr::And(a, b)
            | BvExpr::Or(a, b) => 1 + a.cost() + b.cost(),
            BvExpr::Shift { value, amount, .. } => 1 + value.cost() + amount.cost(),
            BvExpr::Select(c, t, e) => 1 + c.cost() + t.cost() + e.cost(),
        }
    }

    pub fn depth(&self) -> u32 {
        match self {
            BvExpr::Var(_) | BvExpr::Const(_) => 1,
            BvExpr::Add(a, b)
            | BvExpr::Sub(a, b)
            | BvExpr::Xor(a, b)
            | BvExpr::And(a, b)
            | BvExpr::Or(a, b) => 1 + a.depth().max(b.depth()),
            BvExpr::Shift { value, amount, .. } => 1 + value.depth().max(amount.depth()),
            BvExpr::Select(c, t, e) => 1 + c.depth().max(t.depth()).max(e.depth()),
        }
    }

    pub fn max_var_index(&self) -> Option<u8> {
        match self {
            BvExpr::Var(v) => Some(*v),
            BvExpr::Const(_) => None,
            BvExpr::Add(a, b)
            | BvExpr::Sub(a, b)
            | BvExpr::Xor(a, b)
            | BvExpr::And(a, b)
            | BvExpr::Or(a, b) => max_option(a.max_var_index(), b.max_var_index()),
            BvExpr::Shift { value, amount, .. } => {
                max_option(value.max_var_index(), amount.max_var_index())
            }
            BvExpr::Select(c, t, e) => max_option(
                max_option(c.max_var_index(), t.max_var_index()),
                e.max_var_index(),
            ),
        }
    }

    pub fn arity(&self) -> usize {
        self.max_var_index().map(|v| v as usize + 1).unwrap_or(0)
    }

    pub fn validate(&self) -> Result<(), BvValidateError> {
        let depth = self.depth();
        if depth > MAX_EXPR_DEPTH {
            return Err(BvValidateError::OverDepth {
                depth,
                max: MAX_EXPR_DEPTH,
            });
        }
        if let Some(idx) = self.max_var_index()
            && idx > MAX_VAR_INDEX
        {
            return Err(BvValidateError::OverVarIndex {
                index: idx,
                max: MAX_VAR_INDEX,
            });
        }
        Ok(())
    }

    pub fn count_nodes(&self) -> u32 {
        match self {
            BvExpr::Var(_) | BvExpr::Const(_) => 1,
            BvExpr::Add(a, b)
            | BvExpr::Sub(a, b)
            | BvExpr::Xor(a, b)
            | BvExpr::And(a, b)
            | BvExpr::Or(a, b) => 1 + a.count_nodes() + b.count_nodes(),
            BvExpr::Shift { value, amount, .. } => 1 + value.count_nodes() + amount.count_nodes(),
            BvExpr::Select(c, t, e) => 1 + c.count_nodes() + t.count_nodes() + e.count_nodes(),
        }
    }

    pub fn operator_histogram(&self) -> [u32; 10] {
        let mut hist = [0u32; 10];
        self.accumulate_histogram(&mut hist);
        hist
    }

    fn accumulate_histogram(&self, hist: &mut [u32; 10]) {
        match self {
            BvExpr::Var(_) => hist[0] += 1,
            BvExpr::Const(_) => hist[1] += 1,
            BvExpr::Add(_, _) => hist[2] += 1,
            BvExpr::Sub(_, _) => hist[3] += 1,
            BvExpr::Xor(_, _) => hist[4] += 1,
            BvExpr::And(_, _) => hist[5] += 1,
            BvExpr::Or(_, _) => hist[6] += 1,
            BvExpr::Shift { kind, .. } => match kind {
                ShiftKind::LogicalLeft => hist[7] += 1,
                ShiftKind::LogicalRight => hist[8] += 1,
            },
            BvExpr::Select(_, _, _) => hist[9] += 1,
        }
        match self {
            BvExpr::Add(a, b)
            | BvExpr::Sub(a, b)
            | BvExpr::Xor(a, b)
            | BvExpr::And(a, b)
            | BvExpr::Or(a, b) => {
                a.accumulate_histogram(hist);
                b.accumulate_histogram(hist);
            }
            BvExpr::Shift { value, amount, .. } => {
                value.accumulate_histogram(hist);
                amount.accumulate_histogram(hist);
            }
            BvExpr::Select(c, t, e) => {
                c.accumulate_histogram(hist);
                t.accumulate_histogram(hist);
                e.accumulate_histogram(hist);
            }
            _ => {}
        }
    }

    pub fn has_constants(&self) -> bool {
        matches!(self, BvExpr::Const(_))
            || match self {
                BvExpr::Add(a, b)
                | BvExpr::Sub(a, b)
                | BvExpr::Xor(a, b)
                | BvExpr::And(a, b)
                | BvExpr::Or(a, b) => a.has_constants() || b.has_constants(),
                BvExpr::Shift { value, amount, .. } => {
                    value.has_constants() || amount.has_constants()
                }
                BvExpr::Select(c, t, e) => {
                    c.has_constants() || t.has_constants() || e.has_constants()
                }
                _ => false,
            }
    }
}

fn max_option(a: Option<u8>, b: Option<u8>) -> Option<u8> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x.max(y)),
        (Some(x), None) => Some(x),
        (None, Some(y)) => Some(y),
        (None, None) => None,
    }
}

impl CanonicalEncode for BvExpr {
    fn encode_canonical(&self, out: &mut CanonicalWriter) -> Result<(), CanonicalError> {
        match self {
            BvExpr::Var(v) => {
                out.write_u8(0)?;
                out.write_u8(*v)?;
            }
            BvExpr::Const(c) => {
                out.write_u8(1)?;
                out.write_u8(*c)?;
            }
            BvExpr::Add(a, b) => {
                out.write_u8(2)?;
                a.encode_canonical(out)?;
                b.encode_canonical(out)?;
            }
            BvExpr::Sub(a, b) => {
                out.write_u8(3)?;
                a.encode_canonical(out)?;
                b.encode_canonical(out)?;
            }
            BvExpr::Xor(a, b) => {
                out.write_u8(4)?;
                a.encode_canonical(out)?;
                b.encode_canonical(out)?;
            }
            BvExpr::And(a, b) => {
                out.write_u8(5)?;
                a.encode_canonical(out)?;
                b.encode_canonical(out)?;
            }
            BvExpr::Or(a, b) => {
                out.write_u8(6)?;
                a.encode_canonical(out)?;
                b.encode_canonical(out)?;
            }
            BvExpr::Shift {
                kind,
                value,
                amount,
            } => {
                out.write_u8(7)?;
                out.write_u8(match kind {
                    ShiftKind::LogicalLeft => 0,
                    ShiftKind::LogicalRight => 1,
                })?;
                value.encode_canonical(out)?;
                amount.encode_canonical(out)?;
            }
            BvExpr::Select(c, t, e) => {
                out.write_u8(8)?;
                c.encode_canonical(out)?;
                t.encode_canonical(out)?;
                e.encode_canonical(out)?;
            }
        }
        Ok(())
    }
}

/// Decode mixed-radix assignment index into independent u8 values per variable.
pub fn assignment_from_index(index: usize, arity: usize) -> Vec<u8> {
    let mut env = vec![0u8; arity];
    let mut rem = index;
    for slot in env.iter_mut() {
        *slot = (rem % 256) as u8;
        rem /= 256;
    }
    env
}

pub fn assignment_count(arity: usize) -> usize {
    if arity == 0 {
        1
    } else {
        256usize.pow(arity as u32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_shift_wrapping_semantics() {
        let expr = BvExpr::Shift {
            kind: ShiftKind::LogicalLeft,
            value: Box::new(BvExpr::Const(0b0000_0001)),
            amount: Box::new(BvExpr::Const(3)),
        };
        assert_eq!(expr.eval(&[]), 0b0000_1000);
    }

    #[test]
    fn test_select_semantics() {
        let expr = BvExpr::Select(
            Box::new(BvExpr::Var(0)),
            Box::new(BvExpr::Const(42)),
            Box::new(BvExpr::Const(7)),
        );
        assert_eq!(expr.eval(&[0]), 7);
        assert_eq!(expr.eval(&[1]), 42);
    }

    #[test]
    fn test_reject_over_depth() {
        let deep = (0..MAX_EXPR_DEPTH + 1).fold(BvExpr::Const(0), |acc, _| {
            BvExpr::Xor(Box::new(acc), Box::new(BvExpr::Const(0)))
        });
        assert!(matches!(
            deep.validate(),
            Err(BvValidateError::OverDepth { .. })
        ));
    }

    #[test]
    fn test_exhaustive_truth_table_xor_self() {
        let expr = BvExpr::Xor(Box::new(BvExpr::Var(0)), Box::new(BvExpr::Var(0)));
        for i in 0..256 {
            let env = assignment_from_index(i, 1);
            assert_eq!(expr.eval(&env), 0);
        }
    }
}
