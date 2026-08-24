use std::num::{NonZeroU64, NonZeroUsize};
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use reflex::{
    ArtifactKey, BundlePlan, Completion, Direction, GoalId, GoalSet, ImprovementRequest, NonEmpty,
    NonZeroDuration, Objective, OptimizationGoal, Preference, ResourceEnvelope, ResourceUsage,
    improve,
};
use reflex_bitvec::{BitVecDomain, Expression, Metric, SeedScope};

static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

pub struct TestDirectory {
    path: PathBuf,
}

impl TestDirectory {
    pub fn new(label: &str) -> Self {
        let sequence = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "reflex-directional-{label}-{}-{sequence}",
            std::process::id()
        ));
        std::fs::create_dir(&path).expect("the private Directional Harness creates its directory");
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.path).ok();
    }
}

#[derive(Clone, Copy)]
pub struct ResourceLimits {
    worker_threads: usize,
    resident_bytes: u64,
    durable_bytes: u64,
    elapsed_time: Duration,
    cpu_time: Duration,
    verification_requests: u64,
}

impl ResourceLimits {
    pub const fn tiny() -> Self {
        Self {
            worker_threads: 1,
            resident_bytes: 16 * 1024 * 1024,
            durable_bytes: 16 * 1024 * 1024,
            elapsed_time: Duration::from_secs(10),
            cpu_time: Duration::from_secs(10),
            verification_requests: 64,
        }
    }

    pub const fn training() -> Self {
        Self {
            worker_threads: 1,
            resident_bytes: 32 * 1024 * 1024,
            durable_bytes: 32 * 1024 * 1024,
            elapsed_time: Duration::from_secs(10),
            cpu_time: Duration::from_secs(10),
            verification_requests: 512,
        }
    }

    fn envelope(self) -> ResourceEnvelope {
        ResourceEnvelope::new(
            NonZeroUsize::new(self.worker_threads).unwrap(),
            NonZeroU64::new(self.resident_bytes).unwrap(),
            NonZeroU64::new(self.durable_bytes).unwrap(),
            NonZeroDuration::new(self.elapsed_time).unwrap(),
            NonZeroDuration::new(self.cpu_time).unwrap(),
            NonZeroU64::new(self.verification_requests).unwrap(),
        )
    }

    pub fn assert_contains(self, usage: ResourceUsage) {
        assert_eq!(usage.worker_threads, self.worker_threads);
        assert!(usage.resident_bytes > 0 && usage.resident_bytes <= self.resident_bytes);
        assert!(usage.durable_bytes > 0 && usage.durable_bytes <= self.durable_bytes);
        assert!(usage.elapsed_time <= self.elapsed_time);
        assert!(usage.cpu_time <= self.cpu_time);
        assert!(usage.verification_requests <= self.verification_requests);
    }
}

#[derive(Debug, Eq, PartialEq)]
pub struct ArtifactTrace {
    key: [u8; 32],
    origin: [u8; 32],
    parent: Option<[u8; 32]>,
    node_count: usize,
    provenance: Vec<u8>,
    pub(super) truth_table: Vec<u8>,
}

#[derive(Debug, Eq, PartialEq)]
pub struct SemanticTrace {
    pub(super) artifacts: Vec<ArtifactTrace>,
}

impl SemanticTrace {
    pub fn contains_artifact(&self, node_count: usize, truth_table: &[u8]) -> bool {
        self.artifacts.iter().any(|artifact| {
            artifact.node_count == node_count && artifact.truth_table == truth_table
        })
    }

    pub fn contains_artifacts(&self, expected: &[(usize, Vec<u8>)]) -> bool {
        expected
            .iter()
            .all(|(node_count, truth_table)| self.contains_artifact(*node_count, truth_table))
    }
}

#[derive(Debug, Eq, PartialEq)]
pub struct UpdateTrace {
    sequence: u64,
    added: Vec<[u8; 32]>,
    removed: Vec<[u8; 32]>,
    affected_goals: Vec<[u8; 32]>,
}

pub struct TinyRun {
    pub completion: Completion,
    pub semantic: SemanticTrace,
    pub updates: Vec<UpdateTrace>,
    pub usage: ResourceUsage,
    pub bundle_is_file: bool,
}

pub fn run_identity_session(bundle: BundlePlan, limits: ResourceLimits) -> TinyRun {
    run_node_count_session(
        SeedScope::one(Expression::xor(
            Expression::input(),
            Expression::constant(0),
        )),
        bundle,
        limits,
    )
}

pub fn run_node_count_session(
    seeds: SeedScope,
    bundle: BundlePlan,
    limits: ResourceLimits,
) -> TinyRun {
    let target = bundle_target(&bundle).to_path_buf();
    let goal = OptimizationGoal::new(
        [],
        NonEmpty::one(Objective::new(Metric::NodeCount, Direction::Minimize)),
        Preference::tiered(NonEmpty::one(NonEmpty::one(Metric::NodeCount)), [])
            .expect("one priority tier names the only Objective"),
        None,
    )
    .expect("the tiny Optimization Goal is structurally valid");
    let request = ImprovementRequest::new(GoalSet::one(goal), seeds, limits.envelope(), bundle)
        .expect("the tiny Improvement Request is structurally valid");
    let mut updates = Vec::new();
    let outcome = improve(BitVecDomain::unary_u8(), request, |update| {
        updates.push(UpdateTrace {
            sequence: update.sequence(),
            added: update
                .added()
                .iter()
                .map(|artifact| key(artifact.key()))
                .collect(),
            removed: update.removed().iter().copied().map(key).collect(),
            affected_goals: update
                .affected_goals()
                .iter()
                .copied()
                .map(goal_id)
                .collect(),
        });
        ControlFlow::Continue(())
    })
    .expect("the tiny Improvement Session succeeds");
    let semantic = SemanticTrace {
        artifacts: outcome
            .pareto()
            .artifacts()
            .iter()
            .map(|artifact| ArtifactTrace {
                key: key(artifact.key()),
                origin: key(artifact.origin_key()),
                parent: artifact.parent_key().map(key),
                node_count: artifact.artifact().node_count(),
                provenance: artifact.provenance().to_vec(),
                truth_table: (u8::MIN..=u8::MAX)
                    .map(|input| artifact.artifact().evaluate(input))
                    .collect(),
            })
            .collect(),
    };

    TinyRun {
        completion: outcome.completion(),
        semantic,
        updates,
        usage: outcome.usage(),
        bundle_is_file: target.is_file(),
    }
}

fn bundle_target(bundle: &BundlePlan) -> &Path {
    match bundle {
        BundlePlan::Fresh { target }
        | BundlePlan::Resume { target, .. }
        | BundlePlan::Fork { target, .. } => target,
    }
}

fn key(key: ArtifactKey) -> [u8; 32] {
    *key.as_bytes()
}

fn goal_id(goal: GoalId) -> [u8; 32] {
    *goal.as_bytes()
}
