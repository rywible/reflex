use crate::expr::{BvExpr, MAX_EXPR_DEPTH, ShiftKind};
use rand::Rng;
use rand_chacha::ChaCha8Rng;
use rand_chacha::rand_core::SeedableRng;
use reflex_canonical::CanonicalEncode;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BitvecTask {
    pub initial: BvExpr,
    pub target_max_cost: u32,
}

impl CanonicalEncode for BitvecTask {
    fn encode_canonical(
        &self,
        out: &mut reflex_canonical::CanonicalWriter,
    ) -> Result<(), reflex_canonical::CanonicalError> {
        self.initial.encode_canonical(out)?;
        out.write_u32(self.target_max_cost)?;
        Ok(())
    }
}

impl BitvecTask {
    pub fn verify_budget_ok(&self) -> bool {
        self.initial.validate().is_ok()
            && self.initial.arity() <= 2
            && crate::expr::assignment_count(self.initial.arity()) <= 65_536
    }
}

/// Deterministic task generator for benchmarks and corpus expansion.
pub fn generate_tasks(
    count: usize,
    seed: u64,
) -> Result<Vec<BitvecTask>, reflex_canonical::CanonicalError> {
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    let mut tasks = Vec::with_capacity(count);
    let mut seen = std::collections::HashSet::new();

    while tasks.len() < count {
        let depth = rng.gen_range(2..=MAX_EXPR_DEPTH.min(5));
        let expr = random_expr(&mut rng, depth, 0);
        if expr.validate().is_err() {
            continue;
        }
        let digest = reflex_canonical::content_id(
            b"bitvec.task.v1",
            &BitvecTask {
                initial: expr.clone(),
                target_max_cost: 1,
            },
        )?;
        if !seen.insert(digest) {
            continue;
        }
        tasks.push(BitvecTask {
            initial: expr,
            target_max_cost: 1,
        });
    }
    Ok(tasks)
}

fn random_expr(rng: &mut ChaCha8Rng, depth: u32, var_bias: u8) -> BvExpr {
    if depth <= 1 {
        return if rng.gen_bool(0.5) {
            BvExpr::Var(var_bias % 2)
        } else {
            BvExpr::Const(rng.r#gen())
        };
    }
    match rng.gen_range(0..8) {
        0 => BvExpr::Add(
            Box::new(random_expr(rng, depth - 1, var_bias)),
            Box::new(random_expr(rng, depth - 1, var_bias)),
        ),
        1 => BvExpr::Xor(
            Box::new(random_expr(rng, depth - 1, var_bias)),
            Box::new(random_expr(rng, depth - 1, var_bias)),
        ),
        2 => BvExpr::And(
            Box::new(random_expr(rng, depth - 1, var_bias)),
            Box::new(random_expr(rng, depth - 1, var_bias)),
        ),
        3 => BvExpr::Sub(
            Box::new(random_expr(rng, depth - 1, var_bias)),
            Box::new(random_expr(rng, depth - 1, var_bias)),
        ),
        4 => BvExpr::Or(
            Box::new(random_expr(rng, depth - 1, var_bias)),
            Box::new(random_expr(rng, depth - 1, var_bias)),
        ),
        5 => BvExpr::Shift {
            kind: if rng.gen_bool(0.5) {
                ShiftKind::LogicalLeft
            } else {
                ShiftKind::LogicalRight
            },
            value: Box::new(random_expr(rng, depth - 1, var_bias)),
            amount: Box::new(BvExpr::Const(rng.gen_range(0..8))),
        },
        6 => BvExpr::Select(
            Box::new(BvExpr::Var(var_bias % 2)),
            Box::new(random_expr(rng, depth - 1, var_bias)),
            Box::new(random_expr(rng, depth - 1, var_bias)),
        ),
        _ => BvExpr::Xor(
            Box::new(BvExpr::Var(var_bias % 2)),
            Box::new(BvExpr::Var(var_bias % 2)),
        ),
    }
}

/// Frozen train/dev/eval corpus (canonicalized, duplicate-semantics-free).
pub fn frozen_corpus() -> &'static [BitvecTask] {
    FROZEN_CORPUS.as_slice()
}

pub fn frozen_train() -> &'static [BitvecTask] {
    let corpus = frozen_corpus();
    &corpus[..corpus.len().saturating_sub(2)]
}

pub fn frozen_eval() -> &'static [BitvecTask] {
    let corpus = frozen_corpus();
    &corpus[corpus.len().saturating_sub(2)..]
}

fn build_frozen_corpus() -> Vec<BitvecTask> {
    vec![
        BitvecTask {
            initial: BvExpr::Xor(Box::new(BvExpr::Var(0)), Box::new(BvExpr::Var(0))),
            target_max_cost: 1,
        },
        BitvecTask {
            initial: BvExpr::Sub(Box::new(BvExpr::Var(1)), Box::new(BvExpr::Var(1))),
            target_max_cost: 1,
        },
        BitvecTask {
            initial: BvExpr::Add(Box::new(BvExpr::Var(0)), Box::new(BvExpr::Const(0))),
            target_max_cost: 1,
        },
        BitvecTask {
            initial: BvExpr::Add(Box::new(BvExpr::Const(1)), Box::new(BvExpr::Const(2))),
            target_max_cost: 1,
        },
        BitvecTask {
            initial: BvExpr::Xor(Box::new(BvExpr::Var(0)), Box::new(BvExpr::Const(0))),
            target_max_cost: 1,
        },
        BitvecTask {
            initial: BvExpr::Select(
                Box::new(BvExpr::Var(0)),
                Box::new(BvExpr::Const(5)),
                Box::new(BvExpr::Const(9)),
            ),
            target_max_cost: 2,
        },
        BitvecTask {
            initial: BvExpr::Shift {
                kind: ShiftKind::LogicalLeft,
                value: Box::new(BvExpr::Var(0)),
                amount: Box::new(BvExpr::Const(0)),
            },
            target_max_cost: 1,
        },
    ]
}

pub static FROZEN_CORPUS: std::sync::LazyLock<Vec<BitvecTask>> =
    std::sync::LazyLock::new(build_frozen_corpus);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_frozen_corpus_no_duplicate_semantics() {
        let mut seen = std::collections::HashSet::new();
        for task in frozen_corpus() {
            assert!(task.verify_budget_ok());
            let id = reflex_canonical::content_id(b"bitvec.expr.v1", &task.initial).unwrap();
            assert!(seen.insert(id), "duplicate semantics in frozen corpus");
        }
    }

    #[test]
    fn test_generate_100k_tasks_under_budget() {
        let start = std::time::Instant::now();
        let tasks = generate_tasks(100_000, 42).unwrap();
        assert_eq!(tasks.len(), 100_000);
        assert!(start.elapsed().as_secs() < 5);
        for t in tasks.iter().take(100) {
            assert!(t.verify_budget_ok());
        }
    }
}
