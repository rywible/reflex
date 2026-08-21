use std::cell::Cell;
use std::time::{Duration, Instant};

use cpu_time::ProcessTime;

use crate::{ResourceEnvelope, ResourceUsage};

pub(crate) struct ProductionResourceMeter {
    wall_started: Instant,
    cpu_started: ProcessTime,
    elapsed_limit: Duration,
    cpu_limit: Duration,
    worker_limit: usize,
    resident_limit: u64,
    durable_limit: u64,
    peak_resident: Cell<u64>,
    peak_durable: Cell<u64>,
    elapsed_before: Cell<Duration>,
    cpu_before: Cell<Duration>,
}

impl ProductionResourceMeter {
    pub(crate) fn start(resources: &ResourceEnvelope) -> Result<Self, ()> {
        Ok(Self {
            wall_started: Instant::now(),
            cpu_started: ProcessTime::try_now().map_err(|_| ())?,
            elapsed_limit: resources.elapsed_time().get(),
            cpu_limit: resources.cpu_time().get(),
            worker_limit: resources.worker_threads().get(),
            resident_limit: resources.resident_bytes().get(),
            durable_limit: resources.durable_bytes().get(),
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

    pub(crate) fn observe_resident(&self, bytes: u64) -> bool {
        if bytes > self.resident_limit {
            return false;
        }
        self.peak_resident.set(self.peak_resident.get().max(bytes));
        true
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
