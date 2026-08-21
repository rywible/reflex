use std::num::{NonZeroU64, NonZeroUsize};
use std::ops::ControlFlow;
use std::time::Duration;

use reflex::{
    BundlePlan, Direction, GoalSet, ImprovementRequest, NonEmpty, NonZeroDuration, Objective,
    OptimizationGoal, Preference, ResourceEnvelope, SessionError, improve,
};
use reflex_bitvec::{BitVecDomain, Expression, Metric, SeedScope};
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
    let segments = read_segments(&bytes);
    let experience = segments
        .iter()
        .find(|(kind, _)| *kind == 4)
        .map(|(_, payload)| *payload)
        .unwrap();
    let count = u64::from_le_bytes(experience[..8].try_into().unwrap());
    let candidate_length =
        usize::try_from(u64::from_le_bytes(experience[104..112].try_into().unwrap())).unwrap();
    let verdict = experience[112 + candidate_length];

    assert!(
        &bytes[..8] == b"REFLEX\0\x03"
            && segments.iter().map(|(kind, _)| *kind).eq(1..=5)
            && count == 1
            && verdict == 2
            && outcome.usage().verification_requests == 2,
        "the canonical Experience segment retains an ordinary Refuted verdict"
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

fn read_segments(mut bytes: &[u8]) -> Vec<(u8, &[u8])> {
    bytes = &bytes[8..];
    let identity_length = usize::try_from(read_u64(&mut bytes)).unwrap();
    bytes = &bytes[identity_length..];
    let count = read_u32(&mut bytes) as usize;
    let mut segments = Vec::with_capacity(count);
    for _ in 0..count {
        let kind = bytes[0];
        bytes = &bytes[1..];
        assert_eq!(read_u32(&mut bytes), 1);
        let length = usize::try_from(read_u64(&mut bytes)).unwrap();
        let (payload, remainder) = bytes.split_at(length);
        segments.push((kind, payload));
        bytes = &remainder[32..];
    }
    segments
}

fn segment_checksum_offset(bytes: &[u8], expected_kind: u8) -> usize {
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
        offset += 1 + 4;
        let length = usize::try_from(u64::from_le_bytes(
            bytes[offset..offset + 8].try_into().unwrap(),
        ))
        .unwrap();
        offset += 8 + length;
        if kind == expected_kind {
            return offset;
        }
        offset += 32;
    }
    panic!("missing segment kind {expected_kind}")
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
