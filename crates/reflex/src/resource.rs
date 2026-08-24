use std::cell::Cell;
use std::time::{Duration, Instant};

use cpu_time::ProcessTime;

use crate::{
    ExternalVerificationUsage, ResourceEnvelope, ResourceUsage, VerificationAllowance,
    VerificationWorkerRequirements,
};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct ResidentReservation {
    live: u64,
    transient: u64,
    pending_durability: u64,
}

impl ResidentReservation {
    pub(crate) const fn live(bytes: u64) -> Self {
        Self {
            live: bytes,
            transient: 0,
            pending_durability: 0,
        }
    }

    pub(crate) const fn with_transient(mut self, bytes: u64) -> Self {
        self.transient = bytes;
        self
    }

    pub(crate) const fn with_pending_durability(mut self, bytes: u64) -> Self {
        self.pending_durability = bytes;
        self
    }

    pub(crate) const fn peak_bytes(self) -> u64 {
        self.live
            .saturating_add(self.transient)
            .saturating_add(self.pending_durability)
    }
}

pub(crate) struct ResourceEnvelopeGuard {
    wall_started: Instant,
    cpu_started: ProcessTime,
    elapsed_limit: Duration,
    cpu_limit: Duration,
    worker_limit: usize,
    resident_limit: u64,
    durable_limit: u64,
    verification_limit: u64,
    peak_resident: Cell<u64>,
    peak_durable: Cell<u64>,
    elapsed_before: Cell<Duration>,
    external_elapsed: Cell<Duration>,
    cpu_before: Cell<Duration>,
    external_cpu: Cell<Duration>,
}

impl ResourceEnvelopeGuard {
    pub(crate) fn start(resources: &ResourceEnvelope) -> Result<Self, ()> {
        Ok(Self {
            wall_started: Instant::now(),
            cpu_started: ProcessTime::try_now().map_err(|_| ())?,
            elapsed_limit: resources.elapsed_time().get(),
            cpu_limit: resources.cpu_time().get(),
            worker_limit: resources.worker_threads().get(),
            resident_limit: resources.resident_bytes().get(),
            durable_limit: resources.durable_bytes().get(),
            verification_limit: resources.verification_requests().get(),
            peak_resident: Cell::new(0),
            peak_durable: Cell::new(0),
            elapsed_before: Cell::new(Duration::ZERO),
            external_elapsed: Cell::new(Duration::ZERO),
            cpu_before: Cell::new(Duration::ZERO),
            external_cpu: Cell::new(Duration::ZERO),
        })
    }

    pub(crate) fn resume(&self, usage: ResourceUsage) -> Result<(), ()> {
        if usage.worker_threads != self.worker_limit
            || usage.resident_bytes > self.resident_limit
            || usage.durable_bytes > self.durable_limit
            || usage.verification_requests > self.verification_limit
        {
            return Err(());
        }
        self.peak_resident
            .set(self.peak_resident.get().max(usage.resident_bytes));
        self.peak_durable
            .set(self.peak_durable.get().max(usage.durable_bytes));
        self.elapsed_before.set(usage.elapsed_time);
        self.cpu_before.set(usage.cpu_time);
        Ok(())
    }

    pub(crate) fn reserve(&self, reservation: ResidentReservation) -> bool {
        let bytes = reservation.peak_bytes();
        if bytes > self.resident_limit {
            return false;
        }
        self.peak_resident.set(self.peak_resident.get().max(bytes));
        true
    }

    pub(crate) const fn verification_limit(&self) -> u64 {
        self.verification_limit
    }

    pub(crate) fn available_resident(&self, live_bytes: u64) -> u64 {
        self.resident_limit.saturating_sub(live_bytes)
    }

    pub(crate) const fn checkpoint_fits(&self, bytes: u64) -> bool {
        bytes <= self.durable_limit
    }

    pub(crate) fn verification_allowance(
        &self,
        worker_lanes: usize,
        resident_overlap: u64,
    ) -> Result<VerificationAllowance, ()> {
        let elapsed_spent = self.current_elapsed();
        let cpu_spent = self.current_cpu()?;
        Ok(VerificationAllowance::new(
            worker_lanes,
            self.resident_limit.saturating_sub(resident_overlap),
            self.elapsed_limit.saturating_sub(elapsed_spent),
            self.cpu_limit.saturating_sub(cpu_spent),
        ))
    }

    pub(crate) fn charge_external_verification(
        &self,
        requirements: VerificationWorkerRequirements,
        allowance: VerificationAllowance,
        usage: ExternalVerificationUsage,
        resident_overlap: u64,
    ) -> Result<(), ()> {
        self.external_cpu
            .set(self.external_cpu.get().saturating_add(usage.cpu_time()));
        self.external_elapsed.set(
            self.external_elapsed
                .get()
                .saturating_add(usage.elapsed_time()),
        );
        let observed_resident = resident_overlap
            .saturating_sub(requirements.resident_bytes())
            .saturating_add(usage.peak_resident_bytes());
        self.peak_resident
            .set(self.peak_resident.get().max(observed_resident));
        let elapsed_spent = self.current_elapsed();
        let cpu_spent = self.current_cpu()?;
        if usage.worker_lanes() > requirements.worker_lanes()
            || usage.worker_lanes() > allowance.worker_lanes()
            || usage.peak_resident_bytes() > requirements.resident_bytes()
            || usage.peak_resident_bytes() > allowance.resident_bytes()
            || usage.elapsed_time() > allowance.elapsed_time()
            || usage.cpu_time() > allowance.cpu_time()
            || requirements.worker_lanes() == 0 && usage != ExternalVerificationUsage::default()
            || observed_resident > self.resident_limit
            || requirements.worker_lanes() != 0
                && (elapsed_spent > self.elapsed_limit || cpu_spent > self.cpu_limit)
        {
            return Err(());
        }
        Ok(())
    }

    pub(crate) fn time_exhausted(&self) -> Result<bool, ()> {
        self.time_exhausted_against(self.elapsed_limit, self.cpu_limit)
    }

    pub(crate) fn search_time_exhausted(&self) -> Result<bool, ()> {
        self.time_exhausted_against(
            self.elapsed_limit.saturating_mul(4) / 5,
            self.cpu_limit.saturating_mul(4) / 5,
        )
    }

    fn time_exhausted_against(
        &self,
        elapsed_limit: Duration,
        cpu_limit: Duration,
    ) -> Result<bool, ()> {
        Ok(self.current_elapsed() >= elapsed_limit || self.current_cpu()? >= cpu_limit)
    }

    pub(crate) fn usage(
        &self,
        verification_requests: u64,
        durable_bytes: u64,
    ) -> Result<ResourceUsage, ()> {
        Ok(ResourceUsage {
            worker_threads: self.worker_limit,
            resident_bytes: self.peak_resident.get(),
            verification_requests,
            durable_bytes: self.peak_durable.get().max(durable_bytes),
            elapsed_time: self.current_elapsed(),
            cpu_time: self.current_cpu()?,
        })
    }

    pub(crate) fn current_cpu(&self) -> Result<Duration, ()> {
        Ok(self
            .cpu_before
            .get()
            .saturating_add(self.cpu_started.try_elapsed().map_err(|_| ())?)
            .saturating_add(self.external_cpu.get()))
    }

    fn current_elapsed(&self) -> Duration {
        self.elapsed_before.get().saturating_add(overlapped_elapsed(
            self.wall_started.elapsed(),
            self.external_elapsed.get(),
        ))
    }
}

fn overlapped_elapsed(controller: Duration, external: Duration) -> Duration {
    controller.max(external)
}

#[cfg(test)]
mod tests {
    use std::num::{NonZeroU64, NonZeroUsize};
    use std::time::Duration;

    use crate::{
        ExternalVerificationUsage, NonZeroDuration, ResourceEnvelope, ResourceUsage,
        VerificationWorkerRequirements,
    };

    use super::{ResidentReservation, ResourceEnvelopeGuard, overlapped_elapsed};

    #[test]
    fn reservation_accounts_for_every_memory_category() {
        let reservation = ResidentReservation::live(10)
            .with_transient(20)
            .with_pending_durability(30);

        assert_eq!(reservation.peak_bytes(), 60);
    }

    #[test]
    fn reservation_saturates_on_overflow() {
        let reservation = ResidentReservation::live(u64::MAX).with_transient(1);

        assert_eq!(reservation.peak_bytes(), u64::MAX);
    }

    #[test]
    fn resume_rejects_spent_verifications_beyond_the_envelope() {
        let one_second = NonZeroDuration::new(Duration::from_secs(1)).unwrap();
        let resources = ResourceEnvelope::new(
            NonZeroUsize::new(1).unwrap(),
            NonZeroU64::new(1).unwrap(),
            NonZeroU64::new(1).unwrap(),
            one_second,
            one_second,
            NonZeroU64::new(1).unwrap(),
        );
        let guard = ResourceEnvelopeGuard::start(&resources).unwrap();

        assert!(
            guard
                .resume(ResourceUsage {
                    worker_threads: 1,
                    resident_bytes: 0,
                    verification_requests: 2,
                    durable_bytes: 0,
                    elapsed_time: Duration::ZERO,
                    cpu_time: Duration::ZERO,
                })
                .is_err()
        );
    }

    #[test]
    fn rejected_external_overrun_is_still_charged() {
        let one_second = NonZeroDuration::new(Duration::from_secs(1)).unwrap();
        let resources = ResourceEnvelope::new(
            NonZeroUsize::new(2).unwrap(),
            NonZeroU64::new(1_000).unwrap(),
            NonZeroU64::new(1).unwrap(),
            one_second,
            one_second,
            NonZeroU64::new(1).unwrap(),
        );
        let guard = ResourceEnvelopeGuard::start(&resources).unwrap();
        let requirements = VerificationWorkerRequirements::external(
            NonZeroUsize::new(1).unwrap(),
            NonZeroU64::new(100).unwrap(),
        );
        let allowance = guard.verification_allowance(1, 100).unwrap();

        assert!(
            guard
                .charge_external_verification(
                    requirements,
                    allowance,
                    ExternalVerificationUsage::new(
                        1,
                        200,
                        Duration::from_millis(1),
                        Duration::from_secs(2),
                    ),
                    200,
                )
                .is_err()
        );
        let usage = guard.usage(1, 0).unwrap();
        assert!(usage.cpu_time >= Duration::from_secs(2));
        assert!(usage.elapsed_time >= Duration::from_millis(1));
        assert!(usage.resident_bytes >= 300);
    }

    #[test]
    fn external_elapsed_overlaps_controller_wall_time_instead_of_double_charging_it() {
        assert_eq!(
            overlapped_elapsed(Duration::from_secs(3), Duration::from_secs(5)),
            Duration::from_secs(5)
        );
        assert_eq!(
            overlapped_elapsed(Duration::from_secs(7), Duration::from_secs(2)),
            Duration::from_secs(7)
        );
    }
}
