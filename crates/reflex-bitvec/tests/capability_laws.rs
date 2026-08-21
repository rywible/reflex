use std::num::{NonZeroU64, NonZeroUsize};
use std::ops::ControlFlow;
use std::time::Duration;

use reflex::domain::VerificationRequest;
use reflex::{
    BundlePlan, Direction, DomainDefinition, GoalSet, ImprovementRequest, MeasurementEnvironment,
    MeasurementSpace, MeasurementWriter, MetricOrdering, NonEmpty, NonZeroDuration, Objective,
    OptimizationGoal, Preference, ResourceEnvelope, SeedSource, SeedWriter, StructuralProtocol,
    Verdict, VerdictWriter, VerificationBatch, VerificationKernel, VerifiedBatch, improve,
};
use reflex_bitvec::{BitVecDomain, Expression, Metric, SeedScope};

#[test]
fn canonical_structure_interns_equal_subexpressions() {
    let domain = BitVecDomain::unary_u8();
    let expression = Expression::xor(Expression::input(), Expression::input());
    let mut encoded = Vec::new();
    domain
        .structure()
        .encode_canonical(&expression, &mut encoded, &mut ())
        .unwrap();

    assert_eq!(
        encoded,
        [
            2, 0, 0, 0, // two DAG nodes
            0, // input
            2, 0, 0, 0, 0, 0, 0, 0, 0, // xor(node 0, node 0)
        ],
        "canonical construction hash-conses structurally equal children"
    );
}

#[test]
fn measurement_space_orders_and_tolerates_evaluator_work() {
    let domain = BitVecDomain::unary_u8();
    let expression = Expression::xor(Expression::input(), Expression::constant(0));
    let artifacts = [&expression];
    let environment = MeasurementEnvironment::local_process();
    let mut measured = Vec::new();
    let mut scratch = Vec::new();
    domain
        .measurements()
        .measure_batch(
            VerifiedBatch::new(&artifacts),
            &environment,
            &mut MeasurementWriter::new(&mut measured),
            &mut scratch,
        )
        .unwrap();
    let evaluator_work = measured
        .iter()
        .find(|measurement| measurement.metric == Metric::EvaluatorOperations)
        .expect("Evaluator Operations is a declared Measurement");

    assert!(
        evaluator_work.observation == 3
            && domain
                .measurements()
                .compare(Metric::EvaluatorOperations, &2, &3)
                == Ok(MetricOrdering::Less)
            && domain
                .measurements()
                .within_tolerance(Metric::EvaluatorOperations, &2, &4, &2)
                == Ok(true)
            && domain.measurements().environments_compatible(
                Metric::EvaluatorOperations,
                &environment,
                &environment,
            )
    );
}

#[test]
fn decoder_rejects_a_noncanonical_duplicate_dag_node() {
    let domain = BitVecDomain::unary_u8();
    let duplicate_inputs = [
        3, 0, 0, 0, // three nodes
        0, // input 0
        0, // duplicate input 1
        2, 0, 0, 0, 0, 1, 0, 0, 0, // xor(node 0, node 1)
    ];

    assert!(
        domain
            .structure()
            .decode_canonical(&duplicate_inputs, &mut ())
            .is_err(),
        "one semantic DAG has exactly one accepted canonical byte representation"
    );
}

#[test]
fn artifact_key_is_sha256_over_semantics_and_canonical_structure() {
    let bundle_path = std::env::temp_dir().join(format!(
        "reflex-key-{}-{}.bundle",
        std::process::id(),
        std::thread::current().name().unwrap_or("unnamed")
    ));
    let objectives = NonEmpty::one(Objective::new(Metric::NodeCount, Direction::Minimize));
    let preference =
        Preference::tiered(NonEmpty::one(NonEmpty::one(Metric::NodeCount)), []).unwrap();
    let request = ImprovementRequest::new(
        GoalSet::one(OptimizationGoal::new([], objectives, preference, None).unwrap()),
        SeedScope::one(Expression::input()),
        ResourceEnvelope::new(
            NonZeroUsize::new(1).unwrap(),
            NonZeroU64::new(16 * 1024 * 1024).unwrap(),
            NonZeroU64::new(16 * 1024 * 1024).unwrap(),
            NonZeroDuration::new(Duration::from_secs(5)).unwrap(),
            NonZeroDuration::new(Duration::from_secs(5)).unwrap(),
            NonZeroU64::new(10_000).unwrap(),
        ),
        BundlePlan::Fresh {
            target: bundle_path.clone(),
        },
    )
    .unwrap();
    let outcome = improve(BitVecDomain::unary_u8(), request, |_| {
        ControlFlow::Continue(())
    })
    .unwrap();

    assert_eq!(
        outcome.pareto().artifacts()[0].key().as_bytes(),
        &[
            0x98, 0x32, 0x52, 0x12, 0x90, 0x49, 0x45, 0xf9, 0x8d, 0x15, 0x76, 0xcc, 0xd7, 0x39,
            0x98, 0x8b, 0xf2, 0x10, 0x0c, 0x5f, 0xa2, 0x12, 0xef, 0xac, 0xc3, 0xf9, 0xd4, 0x68,
            0xc5, 0x3c, 0x15, 0xad,
        ]
    );
    std::fs::remove_file(bundle_path).ok();
}

#[test]
fn kernel_refuses_a_claim_that_is_not_bound_to_the_seed() {
    let domain = BitVecDomain::unary_u8();
    let seed = Expression::input();
    let candidate = Expression::constant(0);
    let unrelated_claim = domain
        .kernel()
        .claim_for_candidate(&candidate, &candidate)
        .unwrap();
    let requests = [VerificationRequest {
        seed: &seed,
        candidate: &candidate,
        claim: &unrelated_claim,
    }];
    let mut verdicts = Vec::new();
    domain
        .kernel()
        .verify_batch(
            VerificationBatch::new(&requests),
            &mut VerdictWriter::new(&mut verdicts),
            &mut (),
        )
        .unwrap();

    assert!(matches!(verdicts.as_slice(), [Verdict::Refuted]));
}

#[test]
fn encoded_seed_cursor_reproduces_the_exact_remaining_stream() {
    let domain = BitVecDomain::unary_u8();
    let source = domain.seeds();
    let scope = SeedScope::one(Expression::xor(
        Expression::input(),
        Expression::constant(0),
    ));
    let mut encoded_scope = Vec::new();
    source.encode_scope(&scope, &mut encoded_scope).unwrap();
    let decoded_scope = source.decode_scope(&encoded_scope).unwrap();

    let cursor = source.open(&decoded_scope).unwrap();
    let mut encoded_cursor = Vec::new();
    source.encode_cursor(&cursor, &mut encoded_cursor).unwrap();
    let mut left_cursor = source.decode_cursor(&encoded_cursor).unwrap();
    let mut right_cursor = source.decode_cursor(&encoded_cursor).unwrap();
    let mut left = Vec::new();
    let mut right = Vec::new();
    source
        .read_batch(
            &mut left_cursor,
            1,
            &mut SeedWriter::new(&mut left),
            &mut (),
        )
        .unwrap();
    source
        .read_batch(
            &mut right_cursor,
            1,
            &mut SeedWriter::new(&mut right),
            &mut (),
        )
        .unwrap();

    assert!(
        left.len() == 1
            && right.len() == 1
            && left[0].artifact == right[0].artifact
            && left[0].verification.claim == right[0].verification.claim
            && left[0].verification.evidence == right[0].verification.evidence
            && left[0].provenance == right[0].provenance,
        "the encoded Cursor fixes Seed content, Verification, and provenance"
    );
}
