use crate::domain::DomainDefinition;
use crate::session::VerifiedArtifact;

use super::experience::{ConsequenceCheckpoint, ExperienceEntry, ExperienceLedger};

/// A staged admission transition for one Improvement Session epoch.
///
/// The transition owns the rollback invariant: until `commit` is called, all
/// artifact, Search Frontier, and consequence additions are discarded together.
/// This makes every early return and resource refusal atomic without duplicating
/// truncation logic in the Runtime Controller.
pub(super) struct EpochTransition<'a, D: DomainDefinition> {
    known: &'a mut Vec<VerifiedArtifact<D>>,
    frontier: &'a mut Vec<(VerifiedArtifact<D>, usize)>,
    ledger: &'a mut ExperienceLedger,
    known_len: usize,
    known_capacity: usize,
    frontier_len: usize,
    frontier_capacity: usize,
    consequence_checkpoint: ConsequenceCheckpoint,
    committed: bool,
}

impl<'a, D: DomainDefinition> EpochTransition<'a, D> {
    pub(super) fn begin(
        known: &'a mut Vec<VerifiedArtifact<D>>,
        frontier: &'a mut Vec<(VerifiedArtifact<D>, usize)>,
        ledger: &'a mut ExperienceLedger,
    ) -> Self {
        let known_len = known.len();
        let known_capacity = known.capacity();
        let frontier_len = frontier.len();
        let frontier_capacity = frontier.capacity();
        let consequence_checkpoint = ledger.checkpoint_consequences();
        Self {
            known,
            frontier,
            ledger,
            known_len,
            known_capacity,
            frontier_len,
            frontier_capacity,
            consequence_checkpoint,
            committed: false,
        }
    }

    pub(super) fn admit(&mut self, artifact: VerifiedArtifact<D>, origin: usize) {
        self.frontier.push((artifact.clone(), origin));
        self.known.push(artifact);
    }

    pub(super) fn known(&self) -> &Vec<VerifiedArtifact<D>> {
        self.known
    }

    pub(super) fn frontier(&self) -> &Vec<(VerifiedArtifact<D>, usize)> {
        self.frontier
    }

    pub(super) fn ledger(&self) -> &ExperienceLedger {
        self.ledger
    }

    pub(super) fn admission_state(
        &mut self,
    ) -> (
        &[VerifiedArtifact<D>],
        &[ExperienceEntry],
        &mut Vec<crate::learning::ConsequenceObservation>,
    ) {
        let (entries, consequences) = self.ledger.admission_observations();
        (self.known.as_slice(), entries, consequences)
    }

    pub(super) fn commit(mut self) {
        self.committed = true;
    }
}

impl<D: DomainDefinition> Drop for EpochTransition<'_, D> {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        self.known.truncate(self.known_len);
        self.known.shrink_to(self.known_capacity);
        self.frontier.truncate(self.frontier_len);
        self.frontier.shrink_to(self.frontier_capacity);
        self.ledger
            .rollback_consequences(self.consequence_checkpoint);
    }
}

#[cfg(test)]
mod tests {
    // The public integration tests exercise rollback through the real Runtime
    // Controller, including process-abort recovery. This module deliberately
    // has no fake DomainDefinition solely to test Vec truncation.
}
