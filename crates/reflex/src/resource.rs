use std::cell::Cell;
use std::time::{Duration, Instant};

use cpu_time::ProcessTime;

use crate::{ResourceEnvelope, ResourceUsage};

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
    cpu_before: Cell<Duration>,
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
            cpu_before: Cell::new(Duration::ZERO),
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
        Ok(self
            .elapsed_before
            .get()
            .saturating_add(self.wall_started.elapsed())
            >= elapsed_limit
            || self
                .cpu_before
                .get()
                .saturating_add(self.cpu_started.try_elapsed().map_err(|_| ())?)
                >= cpu_limit)
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
            elapsed_time: self
                .elapsed_before
                .get()
                .saturating_add(self.wall_started.elapsed()),
            cpu_time: self
                .cpu_before
                .get()
                .saturating_add(self.cpu_started.try_elapsed().map_err(|_| ())?),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::num::{NonZeroU64, NonZeroUsize};
    use std::time::Duration;

    use crate::{NonZeroDuration, ResourceEnvelope, ResourceUsage};

    use super::{ResidentReservation, ResourceEnvelopeGuard};

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
}
