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
    peak_resident: Cell<u64>,
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
            peak_resident: Cell::new(0),
        })
    }

    pub(crate) fn observe_resident(&self, bytes: u64) -> bool {
        if bytes > self.resident_limit {
            return false;
        }
        self.peak_resident.set(self.peak_resident.get().max(bytes));
        true
    }

    pub(crate) fn time_exhausted(&self) -> Result<bool, ()> {
        Ok(self.wall_started.elapsed() >= self.elapsed_limit
            || self.cpu_started.try_elapsed().map_err(|_| ())? >= self.cpu_limit)
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
            durable_bytes,
            elapsed_time: self.wall_started.elapsed(),
            cpu_time: self.cpu_started.try_elapsed().map_err(|_| ())?,
        })
    }
}
