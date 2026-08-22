use std::ffi::OsStr;

use rayon::prelude::*;

use crate::domain::{
    ClaimOf, DomainDefinition, EvidenceOf, ReplayVerdictWriter, Verdict, VerdictWriter,
    VerificationAllowance, VerificationBatch, VerificationBatchReport, VerificationKernel,
    VerificationReplayBatch, VerificationReplayRequest, VerificationRequest,
    VerificationWorkerRequirements,
};

type ScheduledVerdicts<D> = Result<
    (
        Vec<(ClaimOf<D>, Verdict<EvidenceOf<D>>)>,
        VerificationBatchReport,
    ),
    ScheduleError<<D as DomainDefinition>::Error>,
>;
type ReplayRequest<'a, D> = VerificationReplayRequest<'a, D, ClaimOf<D>, EvidenceOf<D>>;

pub(super) struct ClaimVerificationRequest<'a, D: DomainDefinition> {
    pub(super) seed: &'a D::Artifact,
    pub(super) candidate: &'a D::Artifact,
}

const POLICY_ENV: &str = "REFLEX_INTERNAL_SCHEDULER";

#[derive(Clone, Copy)]
enum Policy {
    Deterministic,
    Throughput,
}

fn parse_policy(value: Option<&OsStr>) -> Result<Policy, ()> {
    match value {
        None => Ok(Policy::Throughput),
        Some(value) if value == OsStr::new("deterministic") => Ok(Policy::Deterministic),
        Some(value) if value == OsStr::new("throughput") => Ok(Policy::Throughput),
        Some(_) => Err(()),
    }
}

pub(super) enum ScheduleError<E> {
    Domain {
        error: E,
        report: VerificationBatchReport,
    },
    Contract {
        report: VerificationBatchReport,
    },
}

/// The only Runtime seam that turns an ordered capability batch into parallel work.
///
/// Both policies preserve input/result order. Deterministic mode fixes one
/// contiguous partition per Runtime lane; throughput mode exposes more chunks
/// to Rayon's local-deque work stealing.
pub(super) struct Scheduler {
    lanes: usize,
    policy: Policy,
}

impl Scheduler {
    pub(super) fn from_environment(lanes: usize) -> Result<Self, ()> {
        if lanes == 0 {
            return Err(());
        }
        let configured = std::env::var_os(POLICY_ENV);
        let policy = parse_policy(configured.as_deref())?;
        Ok(Self { lanes, policy })
    }

    pub(super) const fn lanes(&self) -> usize {
        self.lanes
    }

    pub(super) fn claim_and_verify<D: DomainDefinition>(
        &self,
        domain: &D,
        requests: &[ClaimVerificationRequest<'_, D>],
        allowance: VerificationAllowance,
        requirements: VerificationWorkerRequirements,
    ) -> ScheduledVerdicts<D> {
        if requirements.worker_lanes() != 0 {
            let claims = requests
                .iter()
                .map(|request| {
                    domain
                        .kernel()
                        .claim_for_candidate(request.seed, request.candidate)
                })
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| ScheduleError::Domain {
                    error,
                    report: VerificationBatchReport::in_process(),
                })?;
            let verification = requests
                .iter()
                .zip(&claims)
                .map(|(request, claim)| VerificationRequest {
                    seed: request.seed,
                    candidate: request.candidate,
                    claim,
                })
                .collect::<Vec<_>>();
            let mut verdicts = Vec::with_capacity(requests.len());
            let mut writer = VerdictWriter::with_limit(&mut verdicts, requests.len());
            let mut scratch = <D::Kernel as VerificationKernel<D>>::Scratch::default();
            let outcome = domain.kernel().verify_batch(
                VerificationBatch::with_allowance(&verification, allowance),
                &mut writer,
                &mut scratch,
            );
            let (report, error) = outcome.into_parts();
            if let Some(error) = error {
                return Err(ScheduleError::Domain { error, report });
            }
            if writer.overflowed() {
                return Err(ScheduleError::Contract { report });
            }
            if verdicts.len() != claims.len() {
                return Err(ScheduleError::Contract { report });
            }
            return Ok((claims.into_iter().zip(verdicts).collect(), report));
        }
        let chunks = self.run_ordered(requests, |chunk| {
            let chunk_allowance = VerificationAllowance::new(
                1,
                allowance.resident_bytes(),
                allowance.elapsed_time(),
                allowance.cpu_time(),
            );
            let claims = chunk
                .iter()
                .map(|request| {
                    domain
                        .kernel()
                        .claim_for_candidate(request.seed, request.candidate)
                })
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| ScheduleError::Domain {
                    error,
                    report: VerificationBatchReport::in_process(),
                })?;
            let verification = chunk
                .iter()
                .zip(&claims)
                .map(|(request, claim)| VerificationRequest {
                    seed: request.seed,
                    candidate: request.candidate,
                    claim,
                })
                .collect::<Vec<_>>();
            let mut verdicts = Vec::with_capacity(chunk.len());
            let mut writer = VerdictWriter::with_limit(&mut verdicts, chunk.len());
            let mut scratch = <D::Kernel as VerificationKernel<D>>::Scratch::default();
            let outcome = domain.kernel().verify_batch(
                VerificationBatch::with_allowance(&verification, chunk_allowance),
                &mut writer,
                &mut scratch,
            );
            let (report, error) = outcome.into_parts();
            if let Some(error) = error {
                return Err(ScheduleError::Domain { error, report });
            }
            if writer.overflowed()
                || verdicts.len() != chunk.len()
                || report != VerificationBatchReport::in_process()
            {
                return Err(ScheduleError::Contract { report });
            }
            Ok(claims.into_iter().zip(verdicts).collect())
        })?;
        Ok((
            chunks.into_iter().flatten().collect(),
            VerificationBatchReport::in_process(),
        ))
    }

    pub(super) fn replay<D: DomainDefinition>(
        &self,
        domain: &D,
        requests: &[ReplayRequest<'_, D>],
        allowance: VerificationAllowance,
        requirements: VerificationWorkerRequirements,
    ) -> Result<(Vec<bool>, VerificationBatchReport), ScheduleError<D::Error>> {
        if requirements.worker_lanes() != 0 {
            let mut replayed = Vec::with_capacity(requests.len());
            let mut writer = ReplayVerdictWriter::with_limit(&mut replayed, requests.len());
            let mut scratch = <D::Kernel as VerificationKernel<D>>::Scratch::default();
            let outcome = domain.kernel().replay_batch(
                VerificationReplayBatch::with_allowance(requests, allowance),
                &mut writer,
                &mut scratch,
            );
            let (report, error) = outcome.into_parts();
            if let Some(error) = error {
                return Err(ScheduleError::Domain { error, report });
            }
            if writer.overflowed() {
                return Err(ScheduleError::Contract { report });
            }
            return Ok((replayed, report));
        }
        let chunks = self.run_ordered(requests, |chunk| {
            let chunk_allowance = VerificationAllowance::new(
                1,
                allowance.resident_bytes(),
                allowance.elapsed_time(),
                allowance.cpu_time(),
            );
            let mut replayed = Vec::with_capacity(chunk.len());
            let mut writer = ReplayVerdictWriter::with_limit(&mut replayed, chunk.len());
            let mut scratch = <D::Kernel as VerificationKernel<D>>::Scratch::default();
            let outcome = domain.kernel().replay_batch(
                VerificationReplayBatch::with_allowance(chunk, chunk_allowance),
                &mut writer,
                &mut scratch,
            );
            let (report, error) = outcome.into_parts();
            if let Some(error) = error {
                return Err(ScheduleError::Domain { error, report });
            }
            if writer.overflowed()
                || replayed.len() != chunk.len()
                || report != VerificationBatchReport::in_process()
            {
                return Err(ScheduleError::Contract { report });
            }
            Ok(replayed)
        })?;
        Ok((
            chunks.into_iter().flatten().collect(),
            VerificationBatchReport::in_process(),
        ))
    }

    fn run_ordered<T: Sync, R: Send, E: Send>(
        &self,
        input: &[T],
        work: impl Fn(&[T]) -> Result<Vec<R>, E> + Send + Sync,
    ) -> Result<Vec<Vec<R>>, E> {
        if input.is_empty() {
            return Ok(Vec::new());
        }
        let partition_count = match self.policy {
            Policy::Deterministic => self.lanes,
            Policy::Throughput => self.lanes.saturating_mul(8),
        };
        let chunk_size = input.len().div_ceil(partition_count).max(1);
        let results = input.par_chunks(chunk_size).map(work).collect::<Vec<_>>();
        results.into_iter().collect()
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use super::{Policy, Scheduler, parse_policy};

    #[test]
    fn both_schedulers_preserve_order_with_different_partition_granularity() {
        let input = (0..257_u32).collect::<Vec<_>>();
        for policy in [Policy::Deterministic, Policy::Throughput] {
            let scheduler = Scheduler { lanes: 8, policy };
            let chunks = scheduler
                .run_ordered(&input, |chunk| Ok::<_, ()>(chunk.to_vec()))
                .unwrap();
            assert_eq!(chunks.into_iter().flatten().collect::<Vec<_>>(), input);
        }
    }

    #[test]
    fn invalid_internal_policy_crashes_scheduler_construction() {
        assert!(parse_policy(Some(OsStr::new("invalid"))).is_err());
    }

    #[test]
    fn throughput_scheduler_executes_chunks_on_the_owned_pool() {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(8)
            .build()
            .unwrap();
        let scheduler = Scheduler {
            lanes: 8,
            policy: Policy::Throughput,
        };
        let active = AtomicUsize::new(0);
        let peak = AtomicUsize::new(0);
        let input = [0_u8; 64];
        pool.install(|| {
            scheduler
                .run_ordered(&input, |chunk| {
                    let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(now, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(2));
                    active.fetch_sub(1, Ordering::SeqCst);
                    Ok::<_, ()>(chunk.to_vec())
                })
                .unwrap();
        });
        assert!(peak.load(Ordering::SeqCst) >= 4);
    }
}
