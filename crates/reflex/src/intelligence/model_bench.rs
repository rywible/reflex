use std::hint::black_box;
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

use super::*;
use crate::learning::{FEATURE_COUNT as LEGACY_FEATURE_COUNT, Features, FtrlModel};

const FEATURE_WIDTHS: [usize; 7] = [8, 12, 16, 20, 24, 32, 64];
const HEAD_COUNTS: [usize; 3] = [1, 4, 9];
const ROUTE_FANOUTS: [usize; 3] = [1, 4, 8];
const TARGET_FORECASTS: usize = 1_000_000;
const AXES: [ForecastAxis; 9] = [
    ForecastAxis::KernelAcceptance,
    ForecastAxis::ImmediateImprovement,
    ForecastAxis::UsefulDescendants,
    ForecastAxis::CrossGoalLeverage,
    ForecastAxis::CompressionValue,
    ForecastAxis::InformationValue,
    ForecastAxis::Novelty,
    ForecastAxis::DeadEndRisk,
    ForecastAxis::VerificationCost,
];

#[test]
fn exact_imported_uncertainty_is_validated_and_preserved_bit_exactly() {
    let forecast =
        Forecast::with_uncertainty(ForecastAxis::UsefulDescendants, 0.375, 0.125, 0.625, 41)
            .unwrap();
    let mut encoded = Vec::new();
    forecast.encode_canonical(&mut encoded);
    let restored = Forecast::decode_canonical(&mut Decoder::new(&encoded)).unwrap();

    assert_eq!(restored.estimate().to_bits(), 0.375_f32.to_bits());
    assert_eq!(restored.calibration_error().to_bits(), 0.125_f32.to_bits());
    assert_eq!(restored.uncertainty().to_bits(), 0.625_f32.to_bits());
    for (estimate, calibration, uncertainty) in [
        (f32::NAN, 0.0, 0.0),
        (0.5, f32::INFINITY, 1.0),
        (0.5, 0.75, 0.5),
        (0.5, 0.0, 1.000_001),
    ] {
        assert!(
            Forecast::with_uncertainty(
                ForecastAxis::UsefulDescendants,
                estimate,
                calibration,
                uncertainty,
                0,
            )
            .is_err()
        );
    }
}

#[test]
fn imported_ftrl_forecasts_match_the_legacy_model_bit_exactly() {
    let legacy = legacy_ftrl_fixture();
    let model = CompactModel::import_ftrl(legacy.conversion_view()).unwrap();
    let cases = [0, 1, 2, 3, 4, 7, 11];

    for case in cases {
        let features = differential_features(LEGACY_FEATURE_COUNT, case);
        let expected = legacy.forecast(Features(features.as_slice().try_into().unwrap()));
        let mut actual = Vec::new();
        let (_, count) = model.forecast_into(&features, &mut actual).unwrap();

        assert_eq!(usize::from(count), 7);
        for forecast in actual {
            let expected = expected.0[legacy_head_index(forecast.axis())];
            assert_eq!(forecast.estimate().to_bits(), expected.estimate.to_bits());
            assert_eq!(
                forecast.calibration_error().to_bits(),
                expected.calibration_error.to_bits()
            );
            assert_eq!(
                forecast.uncertainty().to_bits(),
                expected.uncertainty.to_bits()
            );
        }
    }
}

#[test]
fn imported_ftrl_canonical_round_trip_preserves_identity_and_resident_accounting() {
    let legacy = legacy_ftrl_fixture();
    let model = CompactModel::import_ftrl(legacy.conversion_view()).unwrap();
    let mut encoded = Vec::new();

    model.encode_canonical(&mut encoded);
    let mut decoder = Decoder::new(&encoded);
    let restored = CompactModel::decode_canonical(&mut decoder).unwrap();
    let mut restored_encoded = Vec::new();
    restored.encode_canonical(&mut restored_encoded);

    assert!(decoder.is_finished());
    assert_eq!(encoded.len(), 1_439);
    assert_eq!(restored_encoded, encoded);
    assert_eq!(restored.content_identity(), model.content_identity());
    assert_eq!(
        model.content_identity(),
        [
            118, 251, 224, 104, 230, 203, 82, 181, 147, 42, 67, 140, 72, 244, 96, 133, 124, 177,
            19, 6, 10, 56, 173, 223, 157, 107, 16, 192, 2, 241, 176, 103,
        ]
    );
    assert_eq!(
        model.resident_bytes(),
        std::mem::size_of::<CompactModel>() + std::mem::size_of::<ImportedFtrlModel>()
    );
}

#[test]
fn imported_ftrl_decoder_rejects_hostile_shape_state_and_calibration() {
    const SHAPE_BYTES: usize = 4;
    const AXIS_BYTES: usize = 7;
    const MATRIX_BYTES: usize = LEGACY_FEATURE_COUNT * 7 * std::mem::size_of::<f32>();
    const WEIGHT_START: usize = SHAPE_BYTES + AXIS_BYTES;
    const SECOND_MOMENT_START: usize = WEIGHT_START + MATRIX_BYTES;
    const COUNT_START: usize = SECOND_MOMENT_START + MATRIX_BYTES;
    const ERROR_START: usize = COUNT_START + 7 * std::mem::size_of::<u64>();

    let model = CompactModel::import_ftrl(legacy_ftrl_fixture().conversion_view()).unwrap();
    let mut canonical = Vec::new();
    model.encode_canonical(&mut canonical);
    let corruptions = [
        {
            let mut bytes = canonical.clone();
            bytes[1..3].copy_from_slice(&23_u16.to_le_bytes());
            bytes
        },
        {
            let mut bytes = canonical.clone();
            bytes[3] = 6;
            bytes
        },
        {
            let mut bytes = canonical.clone();
            bytes[4] = ForecastAxis::Novelty as u8;
            bytes
        },
        {
            let mut bytes = canonical.clone();
            bytes[WEIGHT_START..WEIGHT_START + 4]
                .copy_from_slice(&f32::NAN.to_bits().to_le_bytes());
            bytes
        },
        {
            let mut bytes = canonical.clone();
            bytes[SECOND_MOMENT_START..SECOND_MOMENT_START + 4]
                .copy_from_slice(&(-1.0_f32).to_bits().to_le_bytes());
            bytes
        },
        {
            let mut bytes = canonical.clone();
            bytes[COUNT_START..COUNT_START + 8].copy_from_slice(&0_u64.to_le_bytes());
            debug_assert_ne!(
                &bytes[ERROR_START..ERROR_START + 4],
                &0.0_f32.to_bits().to_le_bytes()
            );
            bytes
        },
        canonical[..canonical.len() - 1].to_vec(),
    ];

    for hostile in corruptions {
        assert!(CompactModel::decode_canonical(&mut Decoder::new(&hostile)).is_err());
    }
}

fn legacy_head_index(axis: ForecastAxis) -> usize {
    match axis {
        ForecastAxis::ImmediateImprovement => 0,
        ForecastAxis::UsefulDescendants => 1,
        ForecastAxis::CrossGoalLeverage => 2,
        ForecastAxis::CompressionValue => 3,
        ForecastAxis::KernelAcceptance => 4,
        ForecastAxis::VerificationCost => 5,
        ForecastAxis::DeadEndRisk => 6,
        ForecastAxis::InformationValue | ForecastAxis::Novelty => {
            panic!("the legacy FTRL model has no {axis:?} head")
        }
    }
}

fn legacy_ftrl_fixture() -> FtrlModel {
    FtrlModel::deterministic_conversion_fixture()
}

#[test]
fn linear_forecasts_are_bit_exact_for_adversarial_finite_inputs() {
    const DIFFERENTIAL_WIDTHS: [usize; 10] = [1, 8, 15, 16, 17, 24, 63, 64, 255, 256];
    const DIFFERENTIAL_HEADS: [usize; 6] = [1, 3, 4, 7, 8, 9];

    let mut valid_cases = 0_usize;
    for feature_count in DIFFERENTIAL_WIDTHS {
        for head_count in DIFFERENTIAL_HEADS {
            let mut reference_heads = differential_heads(feature_count, head_count);
            let model = CompactModel::linear(feature_count, reference_heads.clone()).unwrap();
            reference_heads.sort_unstable_by_key(|head| head.axis.default_priority());
            for case in 0..12 {
                let features = differential_features(feature_count, case);
                let expected = reference_forecasts(&reference_heads, &features);
                let mut actual = Vec::new();
                let actual_result = model.forecast_into(&features, &mut actual);
                match (expected, actual_result) {
                    (Ok(expected), Ok(_)) => {
                        valid_cases += 1;
                        assert_eq!(
                            encoded_forecasts(&actual),
                            encoded_forecasts(&expected),
                            "forecast bits changed at width={feature_count}, heads={head_count}, case={case}",
                        );
                    }
                    (Err(expected), Err(actual)) => assert_eq!(actual, expected),
                    (expected, actual) => panic!(
                        "forecast result changed at width={feature_count}, heads={head_count}, case={case}: expected={expected:?}, actual={actual:?}"
                    ),
                }
            }
        }
    }
    assert_eq!(
        valid_cases,
        DIFFERENTIAL_WIDTHS.len() * DIFFERENTIAL_HEADS.len() * 12
    );
}

#[test]
fn linear_model_canonical_identity_is_layout_independent() {
    let model = benchmark_model(24, 9);
    let mut encoded = Vec::new();
    model.encode_canonical(&mut encoded);
    let identity: [u8; 32] = Sha256::digest(&encoded).into();
    assert_eq!(encoded.len(), 992);
    assert_eq!(
        identity,
        [
            0x1a, 0x04, 0x8f, 0x6a, 0x69, 0xe0, 0x67, 0x78, 0x9c, 0x13, 0xc4, 0x7f, 0xc4, 0x16,
            0x3f, 0x62, 0xd4, 0x87, 0x49, 0x94, 0x39, 0x96, 0x4f, 0x7e, 0xd2, 0xc9, 0x56, 0xb3,
            0x78, 0x55, 0x69, 0x3f,
        ],
    );
}

#[test]
fn grouped_linear_model_accounts_for_each_weight_exactly_once() {
    let model = benchmark_model(24, 9);
    let CompactModel::GroupedLinear(grouped) = &model else {
        panic!("nine 24-feature heads must use grouped inference");
    };
    assert_eq!(
        grouped.grouped_weights.len() * GROUPED_HEAD_COUNT + grouped.tail_weights.len(),
        24 * 9,
    );
    let exact_resident_bytes = std::mem::size_of::<CompactModel>()
        + std::mem::size_of::<GroupedLinearModel>()
        + grouped.heads.capacity() * std::mem::size_of::<LinearParameters>()
        + grouped.grouped_weights.capacity() * std::mem::size_of::<[f32; GROUPED_HEAD_COUNT]>()
        + grouped.tail_weights.capacity() * std::mem::size_of::<f32>();
    assert_eq!(model.resident_bytes(), exact_resident_bytes);

    assert!(matches!(
        benchmark_model(MINIMUM_GROUPED_FEATURES - 1, 9),
        CompactModel::Linear(_)
    ));
}

#[test]
#[ignore = "release-only deterministic CPU microbenchmark; run explicitly with one test thread"]
fn compact_model_inference_release_microbenchmark() {
    assert!(
        !black_box(cfg!(debug_assertions)),
        "the inference microbenchmark must run with --release"
    );
    eprintln!("width heads fanout baseline_ns optimized_ns speedup forecasts checksum");
    for feature_count in FEATURE_WIDTHS {
        let features = benchmark_features(feature_count);
        for head_count in HEAD_COUNTS {
            let reference =
                ReferenceLinear::new(feature_count, benchmark_heads(feature_count, head_count));
            let model = CompactModel::linear(feature_count, reference.heads.clone()).unwrap();
            assert_model_semantics(&reference, &model, &features);
            for fanout in ROUTE_FANOUTS {
                let forecasts_per_iteration = head_count * fanout;
                let iterations = TARGET_FORECASTS
                    .div_ceil(forecasts_per_iteration)
                    .max(10_000);
                let (baseline_elapsed, baseline_checksum) =
                    measure_reference(&reference, &features, head_count, fanout, iterations);
                let (optimized_elapsed, optimized_checksum) =
                    measure_model(&model, &features, head_count, fanout, iterations);
                let forecasts = iterations * forecasts_per_iteration;
                let nanos_per_forecast = per_item_nanos(baseline_elapsed, forecasts);
                let optimized_nanos = per_item_nanos(optimized_elapsed, forecasts);
                eprintln!(
                    "{feature_count:>5} {head_count:>5} {fanout:>6} {nanos_per_forecast:>11.3} {optimized_nanos:>12.3} {:>7.3} {forecasts:>9} {baseline_checksum:.6}",
                    nanos_per_forecast / optimized_nanos,
                );
                assert!(nanos_per_forecast.is_finite());
                assert_eq!(baseline_checksum.to_bits(), optimized_checksum.to_bits());
            }
        }
    }
}

#[test]
#[ignore = "release-only component benchmark for dot product and sigmoid attribution"]
fn compact_model_component_release_microbenchmark() {
    assert!(
        !black_box(cfg!(debug_assertions)),
        "the component microbenchmark must run with --release"
    );
    eprintln!("component width baseline_ns candidate_ns speedup checksum");
    for feature_count in FEATURE_WIDTHS {
        let features = benchmark_features(feature_count);
        let head = benchmark_head(ForecastAxis::KernelAcceptance, feature_count, 0);
        let iterations = TARGET_FORECASTS.max(100_000);
        let (dot_elapsed, dot_checksum) = measure_dot(&head, &features, iterations);
        let (unrolled_elapsed, unrolled_checksum) =
            measure_unrolled_dot(&head, &features, iterations);
        let (sigmoid_elapsed, sigmoid_checksum) = measure_sigmoid(iterations);
        let dot_nanos = per_item_nanos(dot_elapsed, iterations);
        let unrolled_nanos = per_item_nanos(unrolled_elapsed, iterations);
        eprintln!(
            "dot       {feature_count:>5} {dot_nanos:>11.3} {unrolled_nanos:>12.3} {:>7.3} {dot_checksum:.6}",
            dot_nanos / unrolled_nanos,
        );
        eprintln!(
            "sigmoid   {feature_count:>5} {:>11.3} {:>12} {:>7} {sigmoid_checksum:.6}",
            per_item_nanos(sigmoid_elapsed, iterations),
            "-",
            "-",
        );
        assert_eq!(dot_checksum.to_bits(), unrolled_checksum.to_bits());
    }
}

fn measure_model(
    model: &CompactModel,
    features: &[f32],
    head_count: usize,
    fanout: usize,
    iterations: usize,
) -> (Duration, f32) {
    let mut output = Vec::with_capacity(head_count * fanout);
    for _ in 0..1_000 {
        output.clear();
        for _ in 0..fanout {
            model
                .forecast_into(black_box(features), &mut output)
                .unwrap();
        }
        black_box(&output);
    }
    let mut checksum = 0.0_f32;
    let started = Instant::now();
    for _ in 0..iterations {
        output.clear();
        for _ in 0..fanout {
            model
                .forecast_into(black_box(features), &mut output)
                .unwrap();
        }
        checksum += black_box(output[0].conservative_value());
    }
    (started.elapsed(), checksum)
}

fn measure_reference(
    model: &ReferenceLinear,
    features: &[f32],
    head_count: usize,
    fanout: usize,
    iterations: usize,
) -> (Duration, f32) {
    let mut output = Vec::with_capacity(head_count * fanout);
    for _ in 0..1_000 {
        output.clear();
        for _ in 0..fanout {
            model
                .forecast_into(black_box(features), &mut output)
                .unwrap();
        }
        black_box(&output);
    }
    let mut checksum = 0.0_f32;
    let started = Instant::now();
    for _ in 0..iterations {
        output.clear();
        for _ in 0..fanout {
            model
                .forecast_into(black_box(features), &mut output)
                .unwrap();
        }
        checksum += black_box(output[0].conservative_value());
    }
    (started.elapsed(), checksum)
}

fn measure_dot(head: &LinearHead, features: &[f32], iterations: usize) -> (Duration, f32) {
    let mut checksum = 0.0_f32;
    let started = Instant::now();
    for _ in 0..iterations {
        let linear = head
            .weights
            .iter()
            .zip(black_box(features))
            .fold(head.bias, |sum, (weight, feature)| {
                weight.mul_add(*feature, sum)
            });
        checksum += black_box(linear);
    }
    (started.elapsed(), checksum)
}

fn measure_unrolled_dot(head: &LinearHead, features: &[f32], iterations: usize) -> (Duration, f32) {
    let mut checksum = 0.0_f32;
    let started = Instant::now();
    for _ in 0..iterations {
        checksum += black_box(ordered_dot_product(
            head.bias,
            &head.weights,
            black_box(features),
        ));
    }
    (started.elapsed(), checksum)
}

fn measure_sigmoid(iterations: usize) -> (Duration, f32) {
    let mut checksum = 0.0_f32;
    let started = Instant::now();
    for index in 0..iterations {
        let input = black_box(f32::from(u16::try_from(index % 257).unwrap()) * (1.0 / 32.0) - 4.0);
        let estimate = if input >= 0.0 {
            1.0 / (1.0 + (-input).exp())
        } else {
            let exponential = input.exp();
            exponential / (1.0 + exponential)
        };
        checksum += black_box(estimate);
    }
    (started.elapsed(), checksum)
}

fn benchmark_model(feature_count: usize, head_count: usize) -> CompactModel {
    CompactModel::linear(feature_count, benchmark_heads(feature_count, head_count)).unwrap()
}

fn benchmark_heads(feature_count: usize, head_count: usize) -> Vec<LinearHead> {
    AXES.into_iter()
        .take(head_count)
        .enumerate()
        .map(|(head, axis)| benchmark_head(axis, feature_count, head))
        .collect()
}

fn benchmark_head(axis: ForecastAxis, feature_count: usize, head: usize) -> LinearHead {
    let weights = (0..feature_count).map(|index| {
        let magnitude = f32::from(u16::try_from((index + head) % 17).unwrap()) * (1.0 / 32.0);
        if (index + head).is_multiple_of(2) {
            magnitude
        } else {
            -magnitude
        }
    });
    LinearHead::new(axis, 0.125, weights, 0.05, 128).unwrap()
}

fn benchmark_features(feature_count: usize) -> Vec<f32> {
    (0..feature_count)
        .map(|index| {
            let magnitude = f32::from(u16::try_from(index % 23).unwrap()) * (1.0 / 16.0);
            if index.is_multiple_of(3) {
                -magnitude
            } else {
                magnitude
            }
        })
        .collect()
}

fn differential_heads(feature_count: usize, head_count: usize) -> Vec<LinearHead> {
    AXES.into_iter()
        .take(head_count)
        .rev()
        .enumerate()
        .map(|(head, axis)| {
            LinearHead::new(
                axis,
                signed_bounded(head.wrapping_mul(17), 0),
                (0..feature_count).map(|feature| {
                    if feature.is_multiple_of(31) {
                        let subnormal = f32::from_bits(1 + u32::try_from(head % 7).unwrap());
                        if (feature + head).is_multiple_of(2) {
                            subnormal
                        } else {
                            -subnormal
                        }
                    } else {
                        signed_bounded(feature, head)
                    }
                }),
                f32::from(u16::try_from(head + 1).unwrap()) * 0.01,
                u32::try_from(16 + head).unwrap(),
            )
            .unwrap()
        })
        .collect()
}

fn differential_features(feature_count: usize, case: usize) -> Vec<f32> {
    (0..feature_count)
        .map(|feature| match case {
            0 => signed_bounded(feature, case),
            1 => {
                let bits = 1 + u32::try_from(feature % 127).unwrap();
                if feature.is_multiple_of(2) {
                    f32::from_bits(bits)
                } else {
                    f32::from_bits(bits | (1 << 31))
                }
            }
            2 => {
                let magnitude = f32::MAX / 2048.0;
                if feature.is_multiple_of(2) {
                    magnitude
                } else {
                    -magnitude
                }
            }
            3 => {
                if feature.is_multiple_of(2) {
                    f32::MIN_POSITIVE
                } else {
                    -f32::MIN_POSITIVE
                }
            }
            4 => {
                if feature.is_multiple_of(2) {
                    0.0
                } else {
                    -0.0
                }
            }
            _ => signed_bounded(feature.wrapping_mul(case + 1), case),
        })
        .collect()
}

fn signed_bounded(left: usize, right: usize) -> f32 {
    let mixed = left
        .wrapping_mul(1_664_525)
        .wrapping_add(right.wrapping_mul(1_013_904_223))
        .wrapping_add(12_345);
    let magnitude = f32::from(u16::try_from(mixed % 65_521).unwrap()) * (1.0 / 16_384.0);
    if mixed.is_multiple_of(2) {
        magnitude
    } else {
        -magnitude
    }
}

fn reference_forecasts(
    heads: &[LinearHead],
    features: &[f32],
) -> Result<Vec<Forecast>, IntelligenceError> {
    heads
        .iter()
        .map(|head| {
            let linear = head
                .weights
                .iter()
                .zip(features)
                .fold(head.bias, |sum, (weight, feature)| {
                    weight.mul_add(*feature, sum)
                });
            Forecast::calibrated(
                head.axis,
                sigmoid(linear),
                head.calibration_error,
                head.support,
            )
        })
        .collect()
}

fn encoded_forecasts(forecasts: &[Forecast]) -> Vec<u8> {
    let mut encoded = Vec::with_capacity(forecasts.len() * 17);
    for forecast in forecasts {
        forecast.encode_canonical(&mut encoded);
    }
    encoded
}

#[expect(
    clippy::cast_precision_loss,
    reason = "the release-only microbenchmark reports a duration per completed item"
)]
fn per_item_nanos(duration: Duration, items: usize) -> f64 {
    duration.as_secs_f64() * 1_000_000_000.0 / items as f64
}

struct ReferenceLinear {
    feature_count: usize,
    heads: Vec<LinearHead>,
}

impl ReferenceLinear {
    fn new(feature_count: usize, mut heads: Vec<LinearHead>) -> Self {
        heads.sort_unstable_by_key(|head| head.axis.default_priority());
        Self {
            feature_count,
            heads,
        }
    }

    fn forecast_into(
        &self,
        features: &[f32],
        output: &mut Vec<Forecast>,
    ) -> Result<(u32, u16), IntelligenceError> {
        let start = u32::try_from(output.len()).map_err(|_| IntelligenceError::CapacityExceeded)?;
        if features.len() != self.feature_count {
            return Err(IntelligenceError::InvalidFeature);
        }
        for head in &self.heads {
            let linear = head
                .weights
                .iter()
                .zip(features)
                .fold(head.bias, |sum, (weight, feature)| {
                    weight.mul_add(*feature, sum)
                });
            output.push(Forecast::calibrated(
                head.axis,
                sigmoid(linear),
                head.calibration_error,
                head.support,
            )?);
        }
        Ok((
            start,
            u16::try_from(self.heads.len()).map_err(|_| IntelligenceError::CapacityExceeded)?,
        ))
    }
}

fn assert_model_semantics(reference: &ReferenceLinear, model: &CompactModel, features: &[f32]) {
    let mut reference_output = Vec::new();
    let mut model_output = Vec::new();
    reference
        .forecast_into(features, &mut reference_output)
        .unwrap();
    model.forecast_into(features, &mut model_output).unwrap();
    assert_eq!(
        encoded_forecasts(&reference_output),
        encoded_forecasts(&model_output)
    );
}
