#![forbid(unsafe_code)]

//! Synchronous, single-owner control kernel for local Reflex execution.
//!
//! The engine deliberately has no database-shaped abstraction. One coordinator
//! owns this state, hands immutable jobs to bounded workers, and accepts a
//! result only while its attempt epoch is current. Scientific completion also
//! requires a capability minted by the durable evidence store.

use reflex_cas::CommittedEvidence;
use reflex_types::{CellId, Digest, ExperimentId, GenerationId};
use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};
use std::num::{NonZeroU32, NonZeroU64};
use thiserror::Error;

/// Hard bounds for all coordinator-owned collections.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EngineLimits {
    pub max_experiments: usize,
    pub max_generations: usize,
    pub max_cells: usize,
}

impl EngineLimits {
    pub fn new(
        max_experiments: usize,
        max_generations: usize,
        max_cells: usize,
    ) -> Result<Self, EngineError> {
        let limits = Self {
            max_experiments,
            max_generations,
            max_cells,
        };
        if [max_experiments, max_generations, max_cells].contains(&0) {
            return Err(EngineError::InvalidLimits);
        }
        Ok(limits)
    }
}

impl Default for EngineLimits {
    fn default() -> Self {
        Self {
            max_experiments: 64,
            max_generations: 4_096,
            max_cells: 1_000_000,
        }
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum EngineError {
    #[error("engine limits must all be non-zero")]
    InvalidLimits,
    #[error("{kind} capacity {capacity} is exhausted")]
    CapacityExhausted { kind: &'static str, capacity: usize },
    #[error("{kind} identity must be non-zero")]
    ZeroIdentity { kind: &'static str },
    #[error("experiment {0} is not registered")]
    ExperimentNotFound(ExperimentId),
    #[error("experiment {0} was registered with different immutable content")]
    ExperimentConflict(ExperimentId),
    #[error("generation {0} is not registered")]
    GenerationNotFound(GenerationId),
    #[error("generation {0} was registered with different immutable content")]
    GenerationConflict(GenerationId),
    #[error("experiment {experiment_id} already has generation ordinal {ordinal}")]
    GenerationOrdinalConflict {
        experiment_id: ExperimentId,
        ordinal: u32,
    },
    #[error("cell {0} is not registered")]
    CellNotFound(CellId),
    #[error("cell {0} was registered with different immutable content")]
    CellConflict(CellId),
    #[error("cell {cell_id} belongs to a different experiment or generation")]
    CellParentMismatch { cell_id: CellId },
    #[error("attempt counter overflow for cell {0}")]
    AttemptOverflow(CellId),
    #[error("fence epoch overflow for cell {0}")]
    EpochOverflow(CellId),
    #[error("attempt ticket for cell {0} is stale or forged")]
    StaleAttempt(CellId),
    #[error("cell {cell_id} is not running (state {state:?})")]
    CellNotRunning { cell_id: CellId, state: CellState },
    #[error("cell {cell_id} is terminal (state {state:?})")]
    CellTerminal { cell_id: CellId, state: CellState },
    #[error("generation transition {from:?} -> {to:?} is not permitted")]
    InvalidGenerationTransition {
        from: GenerationPhase,
        to: GenerationPhase,
    },
    #[error("committed evidence identity must be non-zero")]
    InvalidCommittedEvidence,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExperimentRegistration {
    pub id: ExperimentId,
    pub manifest: Digest,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GenerationRegistration {
    pub id: GenerationId,
    pub experiment_id: ExperimentId,
    pub ordinal: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CellRegistration {
    pub id: CellId,
    pub experiment_id: ExperimentId,
    pub generation_id: GenerationId,
    pub manifest: Digest,
    pub priority: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GenerationPhase {
    Bootstrap,
    Collecting,
    Verifying,
    CompilingDataset,
    Training,
    Evaluating,
    PromotionPending,
    Promoted,
    Rejected,
    Stopped,
    Failed,
}

impl GenerationPhase {
    pub const fn permits(self, next: Self) -> bool {
        matches!(
            (self, next),
            (Self::Bootstrap, Self::Collecting)
                | (Self::Collecting, Self::Verifying)
                | (Self::Verifying, Self::CompilingDataset)
                | (Self::CompilingDataset, Self::Training)
                | (Self::Training, Self::Evaluating)
                | (
                    Self::Evaluating,
                    Self::PromotionPending | Self::Promoted | Self::Rejected
                )
                | (Self::PromotionPending, Self::Promoted | Self::Rejected)
                | (
                    Self::Bootstrap
                        | Self::Collecting
                        | Self::Verifying
                        | Self::CompilingDataset
                        | Self::Training
                        | Self::Evaluating
                        | Self::PromotionPending,
                    Self::Stopped | Self::Failed
                )
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EvidenceCommitRef {
    digest: Digest,
    archive_generation: NonZeroU64,
}

impl EvidenceCommitRef {
    fn from_committed(evidence: &CommittedEvidence) -> Result<Self, EngineError> {
        if evidence.digest() == Digest::ZERO {
            return Err(EngineError::InvalidCommittedEvidence);
        }
        Ok(Self {
            digest: evidence.digest(),
            archive_generation: evidence.archive_generation(),
        })
    }

    #[cfg(test)]
    fn for_test(name: &[u8], archive_generation: u64) -> Self {
        Self {
            digest: Digest::hash_blake3(name),
            archive_generation: NonZeroU64::new(archive_generation).expect("non-zero generation"),
        }
    }

    pub fn digest(self) -> Digest {
        self.digest
    }

    pub fn archive_generation(self) -> NonZeroU64 {
        self.archive_generation
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GenerationTransition {
    pub revision: u32,
    pub phase: GenerationPhase,
    pub evidence: EvidenceCommitRef,
}

/// One generation's bounded, synchronous state machine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GenerationMachine {
    registration: GenerationRegistration,
    phase: GenerationPhase,
    revision: u32,
    transitions: Vec<GenerationTransition>,
}

impl GenerationMachine {
    const MAX_TRANSITIONS: usize = 9;

    fn new(registration: GenerationRegistration) -> Result<Self, EngineError> {
        validate_generation(&registration)?;
        Ok(Self {
            registration,
            phase: GenerationPhase::Bootstrap,
            revision: 0,
            transitions: Vec::with_capacity(Self::MAX_TRANSITIONS),
        })
    }

    pub fn registration(&self) -> GenerationRegistration {
        self.registration
    }

    pub fn phase(&self) -> GenerationPhase {
        self.phase
    }

    pub fn revision(&self) -> u32 {
        self.revision
    }

    pub fn transitions(&self) -> &[GenerationTransition] {
        &self.transitions
    }

    pub fn advance(
        &mut self,
        expected: GenerationPhase,
        next: GenerationPhase,
        evidence: &CommittedEvidence,
    ) -> Result<GenerationTransition, EngineError> {
        let evidence = EvidenceCommitRef::from_committed(evidence)?;
        self.advance_ref(expected, next, evidence)
    }

    fn advance_ref(
        &mut self,
        expected: GenerationPhase,
        next: GenerationPhase,
        evidence: EvidenceCommitRef,
    ) -> Result<GenerationTransition, EngineError> {
        if self.phase != expected || !self.phase.permits(next) {
            return Err(EngineError::InvalidGenerationTransition {
                from: self.phase,
                to: next,
            });
        }
        if self.transitions.len() >= Self::MAX_TRANSITIONS {
            return Err(EngineError::CapacityExhausted {
                kind: "generation transition",
                capacity: Self::MAX_TRANSITIONS,
            });
        }
        let revision = self
            .revision
            .checked_add(1)
            .ok_or(EngineError::CapacityExhausted {
                kind: "generation revision",
                capacity: u32::MAX as usize,
            })?;
        let transition = GenerationTransition {
            revision,
            phase: next,
            evidence,
        };
        self.phase = next;
        self.revision = revision;
        self.transitions.push(transition);
        Ok(transition)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CellState {
    Ready,
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

impl CellState {
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Cancelled)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttemptOutcome {
    Accepted,
    Rejected,
}

/// Unforgeable ownership token for one local cell attempt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AttemptTicket {
    cell: CellId,
    attempt_no: NonZeroU32,
    epoch: NonZeroU64,
    manifest: Digest,
}

impl AttemptTicket {
    pub fn cell(self) -> CellId {
        self.cell
    }

    pub fn attempt_no(self) -> NonZeroU32 {
        self.attempt_no
    }

    pub fn epoch(self) -> NonZeroU64 {
        self.epoch
    }

    pub fn manifest(self) -> Digest {
        self.manifest
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CellJob {
    pub registration: CellRegistration,
    pub ticket: AttemptTicket,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CellCompletion {
    pub cell: CellId,
    pub attempt_no: NonZeroU32,
    pub outcome: AttemptOutcome,
    pub evidence: EvidenceCommitRef,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CellSnapshot {
    pub registration: CellRegistration,
    pub state: CellState,
    pub attempt_no: u32,
    pub epoch: u64,
    pub completion: Option<CellCompletion>,
}

#[derive(Clone, Debug)]
struct CellRecord {
    registration: CellRegistration,
    state: CellState,
    attempt_no: u32,
    epoch: u64,
    completion: Option<CellCompletion>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ReadyCell {
    priority: i32,
    id: CellId,
    slot: usize,
}

impl Ord for ReadyCell {
    fn cmp(&self, other: &Self) -> Ordering {
        self.priority
            .cmp(&other.priority)
            // BinaryHeap is a max-heap; reverse IDs so the canonical minimum
            // wins ties independent of registration order.
            .then_with(|| other.id.cmp(&self.id))
    }
}

impl PartialOrd for ReadyCell {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// All mutable local control state. This value has one owner and contains no
/// synchronization primitives, lease clocks, or persistence backend handles.
pub struct LocalRunState {
    limits: EngineLimits,
    experiments: Vec<ExperimentRegistration>,
    experiment_index: HashMap<ExperimentId, usize>,
    generations: Vec<GenerationMachine>,
    generation_index: HashMap<GenerationId, usize>,
    generation_ordinals: HashMap<(ExperimentId, u32), GenerationId>,
    cells: Vec<CellRecord>,
    cell_index: HashMap<CellId, usize>,
    ready: BinaryHeap<ReadyCell>,
}

impl LocalRunState {
    pub fn new(limits: EngineLimits) -> Result<Self, EngineError> {
        let limits = EngineLimits::new(
            limits.max_experiments,
            limits.max_generations,
            limits.max_cells,
        )?;
        Ok(Self {
            limits,
            experiments: Vec::with_capacity(limits.max_experiments.min(1_024)),
            experiment_index: HashMap::with_capacity(limits.max_experiments.min(1_024)),
            generations: Vec::with_capacity(limits.max_generations.min(16_384)),
            generation_index: HashMap::with_capacity(limits.max_generations.min(16_384)),
            generation_ordinals: HashMap::with_capacity(limits.max_generations.min(16_384)),
            cells: Vec::with_capacity(limits.max_cells.min(65_536)),
            cell_index: HashMap::with_capacity(limits.max_cells.min(65_536)),
            ready: BinaryHeap::with_capacity(limits.max_cells.min(65_536)),
        })
    }

    pub fn limits(&self) -> EngineLimits {
        self.limits
    }

    pub fn experiment_count(&self) -> usize {
        self.experiments.len()
    }

    pub fn generation_count(&self) -> usize {
        self.generations.len()
    }

    pub fn cell_count(&self) -> usize {
        self.cells.len()
    }

    pub fn register_experiment(
        &mut self,
        registration: ExperimentRegistration,
    ) -> Result<(), EngineError> {
        validate_experiment(&registration)?;
        if let Some(&slot) = self.experiment_index.get(&registration.id) {
            return if self.experiments[slot] == registration {
                Ok(())
            } else {
                Err(EngineError::ExperimentConflict(registration.id))
            };
        }
        ensure_capacity(
            self.experiments.len(),
            self.limits.max_experiments,
            "experiment",
        )?;
        let slot = self.experiments.len();
        self.experiments.push(registration);
        self.experiment_index.insert(registration.id, slot);
        Ok(())
    }

    pub fn register_generation(
        &mut self,
        registration: GenerationRegistration,
    ) -> Result<(), EngineError> {
        validate_generation(&registration)?;
        if !self
            .experiment_index
            .contains_key(&registration.experiment_id)
        {
            return Err(EngineError::ExperimentNotFound(registration.experiment_id));
        }
        if let Some(&slot) = self.generation_index.get(&registration.id) {
            return if self.generations[slot].registration() == registration {
                Ok(())
            } else {
                Err(EngineError::GenerationConflict(registration.id))
            };
        }
        if self
            .generation_ordinals
            .contains_key(&(registration.experiment_id, registration.ordinal))
        {
            return Err(EngineError::GenerationOrdinalConflict {
                experiment_id: registration.experiment_id,
                ordinal: registration.ordinal,
            });
        }
        ensure_capacity(
            self.generations.len(),
            self.limits.max_generations,
            "generation",
        )?;
        let machine = GenerationMachine::new(registration)?;
        let slot = self.generations.len();
        self.generations.push(machine);
        self.generation_index.insert(registration.id, slot);
        self.generation_ordinals.insert(
            (registration.experiment_id, registration.ordinal),
            registration.id,
        );
        Ok(())
    }

    pub fn generation(
        &self,
        generation_id: GenerationId,
    ) -> Result<&GenerationMachine, EngineError> {
        let slot = self
            .generation_index
            .get(&generation_id)
            .copied()
            .ok_or(EngineError::GenerationNotFound(generation_id))?;
        Ok(&self.generations[slot])
    }

    pub fn generation_mut(
        &mut self,
        generation_id: GenerationId,
    ) -> Result<&mut GenerationMachine, EngineError> {
        let slot = self
            .generation_index
            .get(&generation_id)
            .copied()
            .ok_or(EngineError::GenerationNotFound(generation_id))?;
        Ok(&mut self.generations[slot])
    }

    pub fn register_cell(&mut self, registration: CellRegistration) -> Result<(), EngineError> {
        validate_cell(&registration)?;
        if let Some(&slot) = self.cell_index.get(&registration.id) {
            return if self.cells[slot].registration == registration {
                Ok(())
            } else {
                Err(EngineError::CellConflict(registration.id))
            };
        }
        if !self
            .experiment_index
            .contains_key(&registration.experiment_id)
        {
            return Err(EngineError::ExperimentNotFound(registration.experiment_id));
        }
        let generation_slot = self
            .generation_index
            .get(&registration.generation_id)
            .copied()
            .ok_or(EngineError::GenerationNotFound(registration.generation_id))?;
        if self.generations[generation_slot]
            .registration()
            .experiment_id
            != registration.experiment_id
        {
            return Err(EngineError::CellParentMismatch {
                cell_id: registration.id,
            });
        }
        ensure_capacity(self.cells.len(), self.limits.max_cells, "cell")?;
        let slot = self.cells.len();
        self.cells.push(CellRecord {
            registration,
            state: CellState::Ready,
            attempt_no: 0,
            epoch: 0,
            completion: None,
        });
        self.cell_index.insert(registration.id, slot);
        self.ready.push(ReadyCell {
            priority: registration.priority,
            id: registration.id,
            slot,
        });
        Ok(())
    }

    /// Claim the highest-priority ready cell. Equal priorities use ascending
    /// canonical cell identity, never registration or hash-table order.
    pub fn claim_next(&mut self) -> Result<Option<CellJob>, EngineError> {
        while let Some(ready) = self.ready.pop() {
            let record = &mut self.cells[ready.slot];
            if record.state != CellState::Ready {
                continue;
            }
            let attempt_no = record
                .attempt_no
                .checked_add(1)
                .ok_or(EngineError::AttemptOverflow(record.registration.id))?;
            let epoch = record
                .epoch
                .checked_add(1)
                .ok_or(EngineError::EpochOverflow(record.registration.id))?;
            record.attempt_no = attempt_no;
            record.epoch = epoch;
            record.state = CellState::Running;
            let ticket = AttemptTicket {
                cell: record.registration.id,
                attempt_no: NonZeroU32::new(attempt_no).expect("checked increment is non-zero"),
                epoch: NonZeroU64::new(epoch).expect("checked increment is non-zero"),
                manifest: record.registration.manifest,
            };
            return Ok(Some(CellJob {
                registration: record.registration,
                ticket,
            }));
        }
        Ok(None)
    }

    /// Fence a running infrastructure attempt and place the immutable cell
    /// back on the deterministic ready queue.
    pub fn retry(&mut self, ticket: AttemptTicket) -> Result<(), EngineError> {
        let slot = self.validate_ticket(ticket)?;
        let record = &mut self.cells[slot];
        if record.state != CellState::Running {
            return Err(EngineError::CellNotRunning {
                cell_id: ticket.cell,
                state: record.state,
            });
        }
        record.epoch = record
            .epoch
            .checked_add(1)
            .ok_or(EngineError::EpochOverflow(ticket.cell))?;
        record.state = CellState::Ready;
        record.completion = None;
        self.ready.push(ReadyCell {
            priority: record.registration.priority,
            id: record.registration.id,
            slot,
        });
        Ok(())
    }

    /// Cancel a queued or running cell and advance its epoch so every issued
    /// attempt becomes stale. Repeated cancellation is idempotent.
    pub fn cancel(&mut self, cell_id: CellId) -> Result<(), EngineError> {
        let slot = self
            .cell_index
            .get(&cell_id)
            .copied()
            .ok_or(EngineError::CellNotFound(cell_id))?;
        let record = &mut self.cells[slot];
        if record.state == CellState::Cancelled {
            return Ok(());
        }
        if matches!(record.state, CellState::Succeeded | CellState::Failed) {
            return Err(EngineError::CellTerminal {
                cell_id,
                state: record.state,
            });
        }
        record.epoch = record
            .epoch
            .checked_add(1)
            .ok_or(EngineError::EpochOverflow(cell_id))?;
        record.state = CellState::Cancelled;
        Ok(())
    }

    pub fn finalize(
        &mut self,
        ticket: AttemptTicket,
        outcome: AttemptOutcome,
        evidence: &CommittedEvidence,
    ) -> Result<CellCompletion, EngineError> {
        let evidence = EvidenceCommitRef::from_committed(evidence)?;
        self.finalize_ref(ticket, outcome, evidence)
    }

    fn finalize_ref(
        &mut self,
        ticket: AttemptTicket,
        outcome: AttemptOutcome,
        evidence: EvidenceCommitRef,
    ) -> Result<CellCompletion, EngineError> {
        let slot = self.validate_ticket(ticket)?;
        let record = &mut self.cells[slot];
        if record.state != CellState::Running {
            return Err(EngineError::CellNotRunning {
                cell_id: ticket.cell,
                state: record.state,
            });
        }
        let completion = CellCompletion {
            cell: ticket.cell,
            attempt_no: ticket.attempt_no,
            outcome,
            evidence,
        };
        record.state = match outcome {
            AttemptOutcome::Accepted => CellState::Succeeded,
            AttemptOutcome::Rejected => CellState::Failed,
        };
        record.completion = Some(completion);
        Ok(completion)
    }

    pub fn cell(&self, cell_id: CellId) -> Result<CellSnapshot, EngineError> {
        let slot = self
            .cell_index
            .get(&cell_id)
            .copied()
            .ok_or(EngineError::CellNotFound(cell_id))?;
        let record = &self.cells[slot];
        Ok(CellSnapshot {
            registration: record.registration,
            state: record.state,
            attempt_no: record.attempt_no,
            epoch: record.epoch,
            completion: record.completion,
        })
    }

    fn validate_ticket(&self, ticket: AttemptTicket) -> Result<usize, EngineError> {
        let slot = self
            .cell_index
            .get(&ticket.cell)
            .copied()
            .ok_or(EngineError::CellNotFound(ticket.cell))?;
        let record = &self.cells[slot];
        if record.attempt_no != ticket.attempt_no.get()
            || record.epoch != ticket.epoch.get()
            || record.registration.manifest != ticket.manifest
        {
            return Err(EngineError::StaleAttempt(ticket.cell));
        }
        Ok(slot)
    }
}

fn validate_experiment(registration: &ExperimentRegistration) -> Result<(), EngineError> {
    if registration.id == ExperimentId::from_digest(Digest::ZERO) {
        return Err(EngineError::ZeroIdentity { kind: "experiment" });
    }
    if registration.manifest == Digest::ZERO {
        return Err(EngineError::ZeroIdentity {
            kind: "experiment manifest",
        });
    }
    Ok(())
}

fn validate_generation(registration: &GenerationRegistration) -> Result<(), EngineError> {
    if registration.id == GenerationId::from_digest(Digest::ZERO) {
        return Err(EngineError::ZeroIdentity { kind: "generation" });
    }
    if registration.experiment_id == ExperimentId::from_digest(Digest::ZERO) {
        return Err(EngineError::ZeroIdentity { kind: "experiment" });
    }
    if registration.ordinal == 0 {
        return Err(EngineError::ZeroIdentity {
            kind: "generation ordinal",
        });
    }
    Ok(())
}

fn validate_cell(registration: &CellRegistration) -> Result<(), EngineError> {
    if registration.id == CellId::from_digest(Digest::ZERO) {
        return Err(EngineError::ZeroIdentity { kind: "cell" });
    }
    if registration.experiment_id == ExperimentId::from_digest(Digest::ZERO) {
        return Err(EngineError::ZeroIdentity { kind: "experiment" });
    }
    if registration.generation_id == GenerationId::from_digest(Digest::ZERO) {
        return Err(EngineError::ZeroIdentity { kind: "generation" });
    }
    if registration.manifest == Digest::ZERO {
        return Err(EngineError::ZeroIdentity {
            kind: "cell manifest",
        });
    }
    Ok(())
}

fn ensure_capacity(current: usize, capacity: usize, kind: &'static str) -> Result<(), EngineError> {
    if current >= capacity {
        Err(EngineError::CapacityExhausted { kind, capacity })
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reflex_cas::{EvidenceArtifact, LocalEvidenceBundleStore};
    use std::sync::Arc;

    fn digest(name: &[u8]) -> Digest {
        Digest::hash_blake3(name)
    }

    fn experiment(name: &[u8]) -> ExperimentRegistration {
        ExperimentRegistration {
            id: ExperimentId::from_digest(digest(name)),
            manifest: digest(&[name, b"-manifest"].concat()),
        }
    }

    fn generation(experiment: ExperimentRegistration, ordinal: u32) -> GenerationRegistration {
        let mut identity = Vec::with_capacity(36);
        identity.extend_from_slice(experiment.id.digest().as_bytes());
        identity.extend_from_slice(&ordinal.to_le_bytes());
        GenerationRegistration {
            id: GenerationId::from_digest(digest(&identity)),
            experiment_id: experiment.id,
            ordinal,
        }
    }

    fn cell(generation: GenerationRegistration, name: &[u8], priority: i32) -> CellRegistration {
        CellRegistration {
            id: CellId::from_digest(digest(name)),
            experiment_id: generation.experiment_id,
            generation_id: generation.id,
            manifest: digest(&[name, b"-manifest"].concat()),
            priority,
        }
    }

    fn state_with_cells(cells: &[CellRegistration]) -> LocalRunState {
        let experiment = experiment(b"experiment");
        let generation = generation(experiment, 1);
        let mut state = LocalRunState::new(EngineLimits::new(2, 2, 16).unwrap()).unwrap();
        state.register_experiment(experiment).unwrap();
        state.register_generation(generation).unwrap();
        for cell in cells {
            state.register_cell(*cell).unwrap();
        }
        state
    }

    #[test]
    fn immutable_registration_is_idempotent_and_conflicts_fail() {
        let experiment = experiment(b"experiment");
        let generation = generation(experiment, 1);
        let cell = cell(generation, b"cell", 1);
        let mut state = LocalRunState::new(EngineLimits::new(2, 2, 2).unwrap()).unwrap();

        state.register_experiment(experiment).unwrap();
        state.register_experiment(experiment).unwrap();
        let mut changed_experiment = experiment;
        changed_experiment.manifest = digest(b"different");
        assert_eq!(
            state.register_experiment(changed_experiment),
            Err(EngineError::ExperimentConflict(experiment.id))
        );

        state.register_generation(generation).unwrap();
        state.register_generation(generation).unwrap();
        let mut changed_generation = generation;
        changed_generation.ordinal = 2;
        assert_eq!(
            state.register_generation(changed_generation),
            Err(EngineError::GenerationConflict(generation.id))
        );

        state.register_cell(cell).unwrap();
        state.register_cell(cell).unwrap();
        let mut changed_cell = cell;
        changed_cell.priority = 2;
        assert_eq!(
            state.register_cell(changed_cell),
            Err(EngineError::CellConflict(cell.id))
        );
        assert_eq!(state.experiment_count(), 1);
        assert_eq!(state.generation_count(), 1);
        assert_eq!(state.cell_count(), 1);
    }

    #[test]
    fn deterministic_claim_order_uses_priority_then_canonical_id() {
        let experiment = experiment(b"experiment");
        let generation = generation(experiment, 1);
        let low = cell(generation, b"low", 1);
        let tie_a = cell(generation, b"tie-a", 5);
        let tie_b = cell(generation, b"tie-b", 5);
        let mut state = state_with_cells(&[low, tie_b, tie_a]);

        let expected_first = tie_a.id.min(tie_b.id);
        let expected_second = tie_a.id.max(tie_b.id);
        assert_eq!(
            state.claim_next().unwrap().unwrap().ticket.cell(),
            expected_first
        );
        assert_eq!(
            state.claim_next().unwrap().unwrap().ticket.cell(),
            expected_second
        );
        assert_eq!(state.claim_next().unwrap().unwrap().ticket.cell(), low.id);
        assert!(state.claim_next().unwrap().is_none());
    }

    #[test]
    fn retry_fences_late_result_and_increments_attempt() {
        let experiment = experiment(b"experiment");
        let generation = generation(experiment, 1);
        let cell = cell(generation, b"cell", 0);
        let mut state = state_with_cells(&[cell]);
        let first = state.claim_next().unwrap().unwrap().ticket;
        state.retry(first).unwrap();
        let second = state.claim_next().unwrap().unwrap().ticket;

        assert_eq!(second.attempt_no().get(), 2);
        assert!(second.epoch() > first.epoch());
        let evidence = EvidenceCommitRef::for_test(b"accepted", 1);
        assert_eq!(
            state.finalize_ref(first, AttemptOutcome::Accepted, evidence),
            Err(EngineError::StaleAttempt(cell.id))
        );
        state
            .finalize_ref(second, AttemptOutcome::Accepted, evidence)
            .unwrap();
        assert_eq!(state.cell(cell.id).unwrap().state, CellState::Succeeded);
    }

    #[test]
    fn cancellation_fences_running_attempt_and_is_idempotent() {
        let experiment = experiment(b"experiment");
        let generation = generation(experiment, 1);
        let cell = cell(generation, b"cell", 0);
        let mut state = state_with_cells(&[cell]);
        let ticket = state.claim_next().unwrap().unwrap().ticket;
        state.cancel(cell.id).unwrap();
        state.cancel(cell.id).unwrap();

        assert_eq!(state.cell(cell.id).unwrap().state, CellState::Cancelled);
        assert_eq!(
            state.finalize_ref(
                ticket,
                AttemptOutcome::Accepted,
                EvidenceCommitRef::for_test(b"late", 1),
            ),
            Err(EngineError::StaleAttempt(cell.id))
        );
    }

    #[test]
    fn completion_requires_committed_evidence_reference() {
        let experiment = experiment(b"experiment");
        let generation = generation(experiment, 1);
        let cell = cell(generation, b"cell", 0);
        let mut state = state_with_cells(&[cell]);
        let ticket = state.claim_next().unwrap().unwrap().ticket;
        let evidence = EvidenceCommitRef::for_test(b"completion", 7);
        let completion = state
            .finalize_ref(ticket, AttemptOutcome::Accepted, evidence)
            .unwrap();

        assert_eq!(completion.evidence, evidence);
        assert_eq!(completion.evidence.archive_generation().get(), 7);
        assert_eq!(state.cell(cell.id).unwrap().completion, Some(completion));
    }

    #[test]
    fn public_finalization_accepts_only_archive_minted_capability() {
        let experiment = experiment(b"experiment");
        let generation = generation(experiment, 1);
        let cell = cell(generation, b"cell", 0);
        let mut state = state_with_cells(&[cell]);
        let ticket = state.claim_next().unwrap().unwrap().ticket;
        let directory = tempfile::tempdir().unwrap();
        let store = LocalEvidenceBundleStore::new(directory.path().into(), 1024, 1024, 2).unwrap();
        let (_, committed) = store
            .commit(
                "reflex.engine-test-evidence.v1",
                vec![EvidenceArtifact {
                    name: "completion.bin".into(),
                    bytes: Arc::from(&b"verified completion"[..]),
                }],
            )
            .unwrap();

        let completion = state
            .finalize(ticket, AttemptOutcome::Accepted, &committed)
            .unwrap();
        assert_eq!(completion.evidence.digest(), committed.digest());
        assert_eq!(
            completion.evidence.archive_generation(),
            committed.archive_generation()
        );
    }

    #[test]
    fn generation_machine_requires_evidence_and_enforces_transitions() {
        let experiment = experiment(b"experiment");
        let registration = generation(experiment, 1);
        let mut machine = GenerationMachine::new(registration).unwrap();
        let plan = EvidenceCommitRef::for_test(b"plan", 1);
        let transition = machine
            .advance_ref(
                GenerationPhase::Bootstrap,
                GenerationPhase::Collecting,
                plan,
            )
            .unwrap();
        assert_eq!(transition.revision, 1);
        assert_eq!(machine.phase(), GenerationPhase::Collecting);
        assert_eq!(
            machine.advance_ref(
                GenerationPhase::Collecting,
                GenerationPhase::Training,
                EvidenceCommitRef::for_test(b"bad", 2),
            ),
            Err(EngineError::InvalidGenerationTransition {
                from: GenerationPhase::Collecting,
                to: GenerationPhase::Training,
            })
        );
    }

    #[test]
    fn capacities_are_hard_and_idempotent_registration_still_works() {
        let experiment = experiment(b"experiment");
        let generation = generation(experiment, 1);
        let first = cell(generation, b"first", 0);
        let second = cell(generation, b"second", 0);
        let mut state = LocalRunState::new(EngineLimits::new(1, 1, 1).unwrap()).unwrap();
        state.register_experiment(experiment).unwrap();
        state.register_generation(generation).unwrap();
        state.register_cell(first).unwrap();
        state.register_cell(first).unwrap();
        assert_eq!(
            state.register_cell(second),
            Err(EngineError::CapacityExhausted {
                kind: "cell",
                capacity: 1,
            })
        );
    }
}
