use crate::expr::{BvExpr, assignment_count, assignment_from_index};

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BitvecVerification {
    pub is_equivalent: bool,
    /// Full independent u8 assignment over declared arity (not a correlated stub).
    pub counterexample: Option<Vec<u8>>,
}

impl reflex_canonical::CanonicalEncode for BitvecVerification {
    fn encode_canonical(
        &self,
        out: &mut reflex_canonical::CanonicalWriter,
    ) -> Result<(), reflex_canonical::CanonicalError> {
        out.write_bool(self.is_equivalent)?;
        out.write_option(self.counterexample.as_ref())?;
        Ok(())
    }
}

pub fn verify_equivalent(original: &BvExpr, optimized: &BvExpr) -> BitvecVerification {
    let arity = original.arity().max(optimized.arity());
    let count = assignment_count(arity);
    for i in 0..count {
        let env = assignment_from_index(i, arity);
        let out_opt = optimized.eval(&env);
        let out_orig = original.eval(&env);
        if out_opt != out_orig {
            return BitvecVerification {
                is_equivalent: false,
                counterexample: Some(env),
            };
        }
    }
    BitvecVerification {
        is_equivalent: true,
        counterexample: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_exhaustive_multi_var_not_correlated_env() {
        // x + y != x for most assignments; correlated [v,v+1,v^0x55,v&0x0f] misses many.
        let original = BvExpr::Add(Box::new(BvExpr::Var(0)), Box::new(BvExpr::Var(1)));
        let wrong = BvExpr::Var(0);
        let res = verify_equivalent(&original, &wrong);
        assert!(!res.is_equivalent);
        let cex = res.counterexample.expect("counterexample");
        assert_eq!(cex.len(), 2);
        assert_ne!(
            original.eval(&cex),
            wrong.eval(&cex),
            "counterexample must witness mismatch"
        );
    }

    #[test]
    fn test_shift_select_equivalence() {
        let original = BvExpr::Select(
            Box::new(BvExpr::Var(0)),
            Box::new(BvExpr::Const(9)),
            Box::new(BvExpr::Const(3)),
        );
        let optimized = BvExpr::Select(
            Box::new(BvExpr::Var(0)),
            Box::new(BvExpr::Const(9)),
            Box::new(BvExpr::Const(3)),
        );
        assert!(verify_equivalent(&original, &optimized).is_equivalent);
    }

    #[test]
    fn test_known_bad_rewrite_rejected() {
        let original = BvExpr::Add(Box::new(BvExpr::Var(0)), Box::new(BvExpr::Var(0)));
        let wrong = BvExpr::Const(0);
        let res = verify_equivalent(&original, &wrong);
        assert!(!res.is_equivalent);
        assert!(res.counterexample.is_some());
    }
}
