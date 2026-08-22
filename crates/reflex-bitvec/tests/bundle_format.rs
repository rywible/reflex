use std::num::{NonZeroU64, NonZeroUsize};
use std::ops::ControlFlow;
use std::time::Duration;

use reflex::{
    BundlePlan, Direction, GoalSet, ImprovementRequest, NonEmpty, NonZeroDuration, Objective,
    OptimizationGoal, Preference, ResourceEnvelope, SessionError, improve,
};
use reflex_bitvec::{BitVecDomain, Expression, Metric, SeedScope};
use reflex_bundle::{CanonicalBundle, SegmentKind};
use sha2::{Digest, Sha256};

#[test]
fn v3_bundle_segments_preserve_refuted_experience() {
    let bundle_path = std::env::temp_dir().join(format!(
        "reflex-v3-experience-{}-{}.bundle",
        std::process::id(),
        std::thread::current().name().unwrap_or("unnamed")
    ));
    let objectives = NonEmpty::one(Objective::new(Metric::NodeCount, Direction::Minimize));
    let preference =
        Preference::tiered(NonEmpty::one(NonEmpty::one(Metric::NodeCount)), []).unwrap();
    let request = ImprovementRequest::new(
        GoalSet::one(OptimizationGoal::new([], objectives, preference, None).unwrap()),
        SeedScope::one(Expression::xor(
            Expression::input(),
            Expression::constant(1),
        )),
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
    let bytes = std::fs::read(&bundle_path).unwrap();
    let decoded = CanonicalBundle::decode(&bytes, 16 * 1024 * 1024).unwrap();
    let experience = decoded.segment(SegmentKind::Experience);
    let segments = read_segments(&bytes);
    let count = u64::from_le_bytes(experience[..8].try_into().unwrap());
    let candidate_length =
        usize::try_from(u64::from_le_bytes(experience[168..176].try_into().unwrap())).unwrap();
    let verdict = experience[176 + candidate_length];

    assert!(
        &bytes[..8] == b"REFLEX\0\x03"
            && segments.iter().map(|(kind, _, _)| *kind).eq(1..=5)
            && segments
                .iter()
                .map(|(_, version, _)| *version)
                .eq([1, 4, 3, 3, 1])
            && count == 1
            && verdict == 2
            && outcome.usage().verification_requests == 2,
        "the canonical Experience segment retains an ordinary Refuted verdict"
    );
    std::fs::remove_file(bundle_path).ok();
}

#[test]
fn v3_experience_keeps_resource_admission_and_measurement_observations_separate() {
    let bundle_path = std::env::temp_dir().join(format!(
        "reflex-v3-delayed-observations-{}-{}.bundle",
        std::process::id(),
        std::thread::current().name().unwrap_or("unnamed")
    ));
    let objectives = NonEmpty::one(Objective::new(Metric::NodeCount, Direction::Minimize));
    let preference =
        Preference::tiered(NonEmpty::one(NonEmpty::one(Metric::NodeCount)), []).unwrap();
    let request = ImprovementRequest::new(
        GoalSet::one(OptimizationGoal::new([], objectives, preference, None).unwrap()),
        SeedScope::one(Expression::xor(
            Expression::input(),
            Expression::constant(0),
        )),
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
    improve(BitVecDomain::unary_u8(), request, |_| {
        ControlFlow::Continue(())
    })
    .unwrap();

    let bytes = std::fs::read(&bundle_path).unwrap();
    let decoded = CanonicalBundle::decode(&bytes, 16 * 1024 * 1024).unwrap();
    let mut experience = decoded.segment(SegmentKind::Experience);
    assert_eq!(read_u64(&mut experience), 1);
    experience = &experience[160..];
    let candidate_length = usize::try_from(read_u64(&mut experience)).unwrap();
    experience = &experience[candidate_length..];
    assert_eq!(experience[0], 1);
    experience = &experience[1..];
    let operator_length = usize::try_from(read_u64(&mut experience)).unwrap();
    experience = &experience[operator_length + 8 * 4 + 24 * 4..];
    let verification_requests = read_u32(&mut experience);
    let _epoch = read_u64(&mut experience);
    let consequence_count = usize::try_from(read_u64(&mut experience)).unwrap();
    experience = &experience[consequence_count * 33..];
    let measurement_count = read_u64(&mut experience);
    experience = &experience[32..];
    let environment_length = usize::try_from(read_u64(&mut experience)).unwrap();
    experience = &experience[environment_length..];
    let value_count = read_u64(&mut experience);

    assert!(
        verification_requests == 1
            && consequence_count >= 3
            && measurement_count == 1
            && environment_length > 0
            && value_count == 6,
        "the immutable attempt, delayed consequences, and encoded Measurements remain distinct records"
    );
    std::fs::remove_file(bundle_path).ok();
}

#[test]
fn resume_rejects_a_bad_segment_checksum_even_with_a_valid_file_checksum() {
    let bundle_path = std::env::temp_dir().join(format!(
        "reflex-v3-segment-checksum-{}-{}.bundle",
        std::process::id(),
        std::thread::current().name().unwrap_or("unnamed")
    ));
    let make_request = |bundle| {
        let objectives = NonEmpty::one(Objective::new(Metric::NodeCount, Direction::Minimize));
        let preference =
            Preference::tiered(NonEmpty::one(NonEmpty::one(Metric::NodeCount)), []).unwrap();
        ImprovementRequest::new(
            GoalSet::one(OptimizationGoal::new([], objectives, preference, None).unwrap()),
            SeedScope::one(Expression::xor(
                Expression::input(),
                Expression::constant(1),
            )),
            ResourceEnvelope::new(
                NonZeroUsize::new(1).unwrap(),
                NonZeroU64::new(16 * 1024 * 1024).unwrap(),
                NonZeroU64::new(16 * 1024 * 1024).unwrap(),
                NonZeroDuration::new(Duration::from_secs(5)).unwrap(),
                NonZeroDuration::new(Duration::from_secs(5)).unwrap(),
                NonZeroU64::new(10_000).unwrap(),
            ),
            bundle,
        )
        .unwrap()
    };
    improve(
        BitVecDomain::unary_u8(),
        make_request(BundlePlan::Fresh {
            target: bundle_path.clone(),
        }),
        |_| ControlFlow::Continue(()),
    )
    .unwrap();
    let mut bytes = std::fs::read(&bundle_path).unwrap();
    let checksum_offset = segment_checksum_offset(&bytes, 4);
    bytes[checksum_offset] ^= 0xff;
    let content_length = bytes.len() - 32;
    let checksum = Sha256::digest(&bytes[..content_length]);
    bytes[content_length..].copy_from_slice(&checksum);
    std::fs::write(&bundle_path, bytes).unwrap();

    let result = improve(
        BitVecDomain::unary_u8(),
        make_request(BundlePlan::Resume {
            source: bundle_path.clone(),
            target: bundle_path.clone(),
        }),
        |_| ControlFlow::Continue(()),
    );

    assert!(matches!(result, Err(SessionError::CorruptBundle)));
    std::fs::remove_file(bundle_path).ok();
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one canonical bundle fixture is mutated across independent compatibility axes"
)]
fn resume_rejects_runtime_or_segment_revision_drift_and_model_digest_mismatches() {
    let bundle_path = std::env::temp_dir().join(format!(
        "reflex-v3-schema-integrity-{}-{}.bundle",
        std::process::id(),
        std::thread::current().name().unwrap_or("unnamed")
    ));
    let make_request = |bundle| {
        let objectives = NonEmpty::one(Objective::new(Metric::NodeCount, Direction::Minimize));
        let preference =
            Preference::tiered(NonEmpty::one(NonEmpty::one(Metric::NodeCount)), []).unwrap();
        ImprovementRequest::new(
            GoalSet::one(OptimizationGoal::new([], objectives, preference, None).unwrap()),
            SeedScope::one(Expression::xor(
                Expression::input(),
                Expression::constant(1),
            )),
            ResourceEnvelope::new(
                NonZeroUsize::new(1).unwrap(),
                NonZeroU64::new(16 * 1024 * 1024).unwrap(),
                NonZeroU64::new(16 * 1024 * 1024).unwrap(),
                NonZeroDuration::new(Duration::from_secs(5)).unwrap(),
                NonZeroDuration::new(Duration::from_secs(5)).unwrap(),
                NonZeroU64::new(10_000).unwrap(),
            ),
            bundle,
        )
        .unwrap()
    };
    improve(
        BitVecDomain::unary_u8(),
        make_request(BundlePlan::Fresh {
            target: bundle_path.clone(),
        }),
        |_| ControlFlow::Continue(()),
    )
    .unwrap();
    let valid = std::fs::read(&bundle_path).unwrap();

    let mut bad_runtime_revision = valid.clone();
    let (_, payload, length, checksum) = segment_location(&bad_runtime_revision, 1);
    bad_runtime_revision[payload + 1..payload + 9].copy_from_slice(&99_u64.to_le_bytes());
    let segment_digest = Sha256::digest(&bad_runtime_revision[payload..payload + length]);
    bad_runtime_revision[checksum..checksum + 32].copy_from_slice(&segment_digest);
    refresh_file_checksum(&mut bad_runtime_revision);
    std::fs::write(&bundle_path, bad_runtime_revision).unwrap();
    assert!(matches!(
        improve(
            BitVecDomain::unary_u8(),
            make_request(BundlePlan::Resume {
                source: bundle_path.clone(),
                target: bundle_path.clone(),
            }),
            |_| ControlFlow::Continue(())
        ),
        Err(SessionError::IncompatibleBundle)
    ));

    let mut bad_version = valid.clone();
    let (version, _, _, _) = segment_location(&bad_version, 4);
    bad_version[version..version + 4].copy_from_slice(&99_u32.to_le_bytes());
    refresh_file_checksum(&mut bad_version);
    std::fs::write(&bundle_path, bad_version).unwrap();
    assert!(matches!(
        improve(
            BitVecDomain::unary_u8(),
            make_request(BundlePlan::Resume {
                source: bundle_path.clone(),
                target: bundle_path.clone(),
            }),
            |_| ControlFlow::Continue(())
        ),
        Err(SessionError::CorruptBundle)
    ));

    let mut false_verdict = CanonicalBundle::decode(&valid, 16 * 1024 * 1024).unwrap();
    let mut experience = false_verdict.segment(SegmentKind::Experience).to_vec();
    let candidate_length =
        usize::try_from(u64::from_le_bytes(experience[168..176].try_into().unwrap())).unwrap();
    experience[176 + candidate_length] = 1;
    false_verdict.replace_segment(SegmentKind::Experience, experience);
    std::fs::write(&bundle_path, false_verdict.encode()).unwrap();
    assert!(matches!(
        improve(
            BitVecDomain::unary_u8(),
            make_request(BundlePlan::Resume {
                source: bundle_path.clone(),
                target: bundle_path.clone(),
            }),
            |_| ControlFlow::Continue(())
        ),
        Err(SessionError::CorruptBundle)
    ));

    let mut bad_model_digest = valid;
    let (_, payload, length, checksum) = segment_location(&bad_model_digest, 2);
    bad_model_digest[payload + 32] ^= 0xff;
    let segment_digest = Sha256::digest(&bad_model_digest[payload..payload + length]);
    bad_model_digest[checksum..checksum + 32].copy_from_slice(&segment_digest);
    refresh_file_checksum(&mut bad_model_digest);
    std::fs::write(&bundle_path, bad_model_digest).unwrap();
    assert!(matches!(
        improve(
            BitVecDomain::unary_u8(),
            make_request(BundlePlan::Resume {
                source: bundle_path.clone(),
                target: bundle_path.clone(),
            }),
            |_| ControlFlow::Continue(())
        ),
        Err(SessionError::CorruptBundle)
    ));
    std::fs::remove_file(bundle_path).ok();
}

fn read_segments(mut bytes: &[u8]) -> Vec<(u8, u32, &[u8])> {
    bytes = &bytes[8..];
    let identity_length = usize::try_from(read_u64(&mut bytes)).unwrap();
    bytes = &bytes[identity_length..];
    let count = read_u32(&mut bytes) as usize;
    let mut segments = Vec::with_capacity(count);
    for _ in 0..count {
        let kind = bytes[0];
        bytes = &bytes[1..];
        let version = read_u32(&mut bytes);
        let length = usize::try_from(read_u64(&mut bytes)).unwrap();
        let (payload, remainder) = bytes.split_at(length);
        segments.push((kind, version, payload));
        bytes = &remainder[32..];
    }
    segments
}

fn segment_checksum_offset(bytes: &[u8], expected_kind: u8) -> usize {
    segment_location(bytes, expected_kind).3
}

fn segment_location(bytes: &[u8], expected_kind: u8) -> (usize, usize, usize, usize) {
    let mut offset = 8;
    let identity_length = usize::try_from(u64::from_le_bytes(
        bytes[offset..offset + 8].try_into().unwrap(),
    ))
    .unwrap();
    offset += 8 + identity_length;
    let count = u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
    offset += 4;
    for _ in 0..count {
        let kind = bytes[offset];
        offset += 1;
        let version_offset = offset;
        offset += 4;
        let length = usize::try_from(u64::from_le_bytes(
            bytes[offset..offset + 8].try_into().unwrap(),
        ))
        .unwrap();
        offset += 8;
        let payload_offset = offset;
        offset += length;
        if kind == expected_kind {
            return (version_offset, payload_offset, length, offset);
        }
        offset += 32;
    }
    panic!("missing segment kind {expected_kind}")
}

fn refresh_file_checksum(bytes: &mut [u8]) {
    let content_length = bytes.len() - 32;
    let checksum = Sha256::digest(&bytes[..content_length]);
    bytes[content_length..].copy_from_slice(&checksum);
}

fn read_u32(bytes: &mut &[u8]) -> u32 {
    let (value, remainder) = bytes.split_at(4);
    *bytes = remainder;
    u32::from_le_bytes(value.try_into().unwrap())
}

fn read_u64(bytes: &mut &[u8]) -> u64 {
    let (value, remainder) = bytes.split_at(8);
    *bytes = remainder;
    u64::from_le_bytes(value.try_into().unwrap())
}
