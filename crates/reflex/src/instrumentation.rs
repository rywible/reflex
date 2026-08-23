use std::fmt::Write as _;
use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

const REPORT_PREFIX_ENV: &str = "REFLEX_INTERNAL_PHASE_REPORT_PREFIX";
static SESSION_ORDINAL: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy)]
pub(crate) enum Phase {
    Setup,
    Generation,
    Selection,
    Verification,
    VerificationKernel,
    MeasurementAdmission,
    Consolidation,
    Training,
    Finalization,
}

#[derive(Clone, Copy)]
pub(crate) enum ResourceRefusal {
    DurablePreVerification,
    ResidentPreVerification,
    ResidentEpoch,
    VerificationBudget,
    Time,
}

impl ResourceRefusal {
    const fn name(self) -> &'static str {
        match self {
            Self::DurablePreVerification => "durable-pre-verification",
            Self::ResidentPreVerification => "resident-pre-verification",
            Self::ResidentEpoch => "resident-epoch",
            Self::VerificationBudget => "verification-budget",
            Self::Time => "time",
        }
    }
}

impl Phase {
    const COUNT: usize = 9;

    const fn index(self) -> usize {
        self as usize
    }
}

pub(crate) struct Recorder {
    report: Option<PathBuf>,
    started: Option<Instant>,
    phase_ns: [u64; Phase::COUNT],
    epochs: u64,
    candidates_generated: u64,
    candidates_selected: u64,
    verification_requests: u64,
    artifacts_admitted: u64,
    resource_refusal: Option<ResourceRefusal>,
}

impl Recorder {
    pub(crate) fn from_environment() -> Self {
        let report = std::env::var_os(REPORT_PREFIX_ENV).map(|prefix| {
            let ordinal = SESSION_ORDINAL.fetch_add(1, Ordering::Relaxed);
            let mut path = PathBuf::from(prefix);
            path.set_extension(format!("{ordinal}.phase"));
            path
        });
        let started = report.as_ref().map(|_| Instant::now());
        Self {
            report,
            started,
            phase_ns: [0; Phase::COUNT],
            epochs: 0,
            candidates_generated: 0,
            candidates_selected: 0,
            verification_requests: 0,
            artifacts_admitted: 0,
            resource_refusal: None,
        }
    }

    #[inline]
    pub(crate) fn start(&self) -> Option<Instant> {
        self.report.as_ref().map(|_| Instant::now())
    }

    #[inline]
    pub(crate) fn finish(&mut self, phase: Phase, started: Option<Instant>) {
        if let Some(started) = started {
            self.phase_ns[phase.index()] =
                self.phase_ns[phase.index()].saturating_add(nanoseconds(started.elapsed()));
        }
    }

    #[inline]
    pub(crate) fn epoch(&mut self) {
        if self.report.is_some() {
            self.epochs = self.epochs.saturating_add(1);
        }
    }

    #[inline]
    pub(crate) fn generated(&mut self, count: usize) {
        if self.report.is_some() {
            self.candidates_generated = self
                .candidates_generated
                .saturating_add(u64::try_from(count).unwrap_or(u64::MAX));
        }
    }

    #[inline]
    pub(crate) fn selected(&mut self, count: usize) {
        if self.report.is_some() {
            self.candidates_selected = self
                .candidates_selected
                .saturating_add(u64::try_from(count).unwrap_or(u64::MAX));
        }
    }

    #[inline]
    pub(crate) fn verified(&mut self, count: usize) {
        if self.report.is_some() {
            self.verification_requests = self
                .verification_requests
                .saturating_add(u64::try_from(count).unwrap_or(u64::MAX));
        }
    }

    #[inline]
    pub(crate) fn admitted(&mut self, count: usize) {
        if self.report.is_some() {
            self.artifacts_admitted = self
                .artifacts_admitted
                .saturating_add(u64::try_from(count).unwrap_or(u64::MAX));
        }
    }

    #[inline]
    pub(crate) fn refused(&mut self, refusal: ResourceRefusal) {
        if self.report.is_some() && self.resource_refusal.is_none() {
            self.resource_refusal = Some(refusal);
        }
    }

    fn write_report(&self, report: &PathBuf) -> std::io::Result<()> {
        let mut body = String::new();
        writeln!(body, "schema=reflex-internal-phase-v2").unwrap();
        let total = self
            .started
            .map_or(0, |started| nanoseconds(started.elapsed()));
        writeln!(body, "total_ns={total}").unwrap();
        for (name, phase) in [
            ("setup_ns", Phase::Setup),
            ("generation_ns", Phase::Generation),
            ("selection_ns", Phase::Selection),
            ("verification_ns", Phase::Verification),
            ("verification_kernel_ns", Phase::VerificationKernel),
            ("measurement_admission_ns", Phase::MeasurementAdmission),
            ("consolidation_ns", Phase::Consolidation),
            ("training_ns", Phase::Training),
            ("finalization_ns", Phase::Finalization),
        ] {
            writeln!(body, "{name}={}", self.phase_ns[phase.index()]).unwrap();
        }
        writeln!(body, "epochs={}", self.epochs).unwrap();
        writeln!(body, "candidates_generated={}", self.candidates_generated).unwrap();
        writeln!(body, "candidates_selected={}", self.candidates_selected).unwrap();
        writeln!(body, "verification_requests={}", self.verification_requests).unwrap();
        writeln!(body, "artifacts_admitted={}", self.artifacts_admitted).unwrap();
        writeln!(
            body,
            "resource_refusal={}",
            self.resource_refusal.map_or("none", ResourceRefusal::name)
        )
        .unwrap();
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(report)?;
        file.write_all(body.as_bytes())?;
        file.sync_all()
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        if let Some(report) = &self.report {
            let _ = self.write_report(report);
        }
    }
}

fn nanoseconds(duration: std::time::Duration) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::{Phase, Recorder};

    #[test]
    fn disabled_recorder_only_retains_zero_aggregates() {
        let mut recorder = Recorder {
            report: None,
            started: None,
            phase_ns: [0; Phase::COUNT],
            epochs: 0,
            candidates_generated: 0,
            candidates_selected: 0,
            verification_requests: 0,
            artifacts_admitted: 0,
            resource_refusal: None,
        };
        let started = recorder.start();
        recorder.finish(Phase::Generation, started);
        recorder.epoch();
        recorder.generated(10);
        recorder.selected(5);
        recorder.verified(4);
        recorder.admitted(3);
        recorder.refused(super::ResourceRefusal::DurablePreVerification);
        assert_eq!(recorder.phase_ns, [0; Phase::COUNT]);
        assert_eq!(recorder.epochs, 0);
        assert_eq!(recorder.candidates_generated, 0);
        assert_eq!(recorder.candidates_selected, 0);
        assert_eq!(recorder.verification_requests, 0);
        assert_eq!(recorder.artifacts_admitted, 0);
        assert!(recorder.resource_refusal.is_none());
    }
}
