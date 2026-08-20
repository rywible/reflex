//! Generation-stamped episode arena (§10.1, P5.2).
//!
//! The arena owns domain payloads for one episode and stamps every state
//! handle with the generation it was issued in. Resetting the arena
//! (`clear`) bumps the generation, which invalidates every previously issued
//! handle: a stale handle is rejected by every accessor, either as `None`
//! (compatibility accessors) or as a precise [`DomainError::StaleStateHandle`]
//! (resolution accessors).
//!
//! Artifacts and verifications are also arena-resident so the erased-domain
//! facade ([`crate::ErasedDomain`]) can carry typed payloads between
//! `reconstruct_artifact`, `verify`, and `evaluate_utility` without
//! serialization in the native path (P5.2).

use crate::DomainError;
use reflex_types::{CandidateHandle, StateHandle, StateId};
use serde::{Deserialize, Serialize};
use std::any::Any;

/// Handle to an artifact stored in the episode arena (erased-domain path).
/// Carries the issuing arena generation; stale handles are rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ArtifactHandle(pub u32, pub u64);

/// Handle to a verification stored in the episode arena (erased-domain path).
/// Carries the issuing arena generation; stale handles are rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct VerificationHandle(pub u32, pub u64);

/// Per-episode payload arena.
///
/// Ownership rules (P5.2 AC "domains unload only when no episode holds their
/// handles"): the arena owns its payloads; handles borrow the arena on every
/// access, so a payload cannot outlive its arena slot.
#[derive(Default)]
pub struct EpisodeArena {
    generation: u64,
    states: Vec<Box<dyn Any + Send + Sync>>,
    state_ids: Vec<StateId>,
    candidates: Vec<Box<dyn Any + Send + Sync>>,
    artifacts: Vec<Box<dyn Any + Send + Sync>>,
    verifications: Vec<Box<dyn Any + Send + Sync>>,
}

impl EpisodeArena {
    pub fn new() -> Self {
        Self::default()
    }

    /// Current arena generation. Handles issued in an earlier generation are
    /// stale.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    // -- states -----------------------------------------------------------

    /// Inserts a state payload and returns its generation-stamped handle.
    pub fn insert_state<T: Any + Send + Sync>(&mut self, state: T, id: StateId) -> StateHandle {
        let index = self.states.len() as u32;
        self.states.push(Box::new(state));
        self.state_ids.push(id);
        StateHandle::new(index, self.generation)
    }

    /// Typed access. Returns `None` for stale, out-of-range, or
    /// wrong-typed handles.
    pub fn get_state<T: Any>(&self, handle: StateHandle) -> Option<&T> {
        let index = self.state_index(handle)?;
        self.states.get(index)?.downcast_ref::<T>()
    }

    /// State id access. Returns `None` for stale or out-of-range handles.
    pub fn get_state_id(&self, handle: StateHandle) -> Option<StateId> {
        let index = self.state_index(handle)?;
        self.state_ids.get(index).copied()
    }

    /// Erased access that distinguishes staleness from other failures.
    pub fn resolve_state(
        &self,
        handle: StateHandle,
    ) -> Result<&(dyn Any + Send + Sync), DomainError> {
        let index = self.state_index_checked(handle)?;
        self.states
            .get(index)
            .map(|slot| slot.as_ref())
            .ok_or(DomainError::InvalidStateHandle(handle.index()))
    }

    /// Whether `handle` is a live state handle in this arena.
    pub fn contains_state(&self, handle: StateHandle) -> bool {
        self.state_index(handle)
            .map(|i| i < self.states.len())
            .unwrap_or(false)
    }

    /// Handle of the most recently inserted state, if any. Useful for
    /// domains whose artifact reconstruction consumes the terminal state
    /// produced by the final `apply_candidates`.
    pub fn last_state_handle(&self) -> Option<StateHandle> {
        let index = self.states.len().checked_sub(1)? as u32;
        Some(StateHandle::new(index, self.generation))
    }

    // -- candidates ---------------------------------------------------------

    /// Inserts a candidate payload and returns a batch-local cursor.
    ///
    /// Candidate handles are cursor values inside an enumeration batch; they
    /// are not generation-checked (a candidate batch lives and dies with the
    /// state it was enumerated from).
    pub fn insert_candidate<T: Any + Send + Sync>(&mut self, candidate: T) -> CandidateHandle {
        let index = self.candidates.len() as u32;
        self.candidates.push(Box::new(candidate));
        CandidateHandle(index)
    }

    pub fn get_candidate<T: Any>(&self, handle: CandidateHandle) -> Option<&T> {
        self.candidates.get(handle.0 as usize)?.downcast_ref::<T>()
    }

    // -- artifacts -----------------------------------------------------------

    /// Inserts an artifact payload (erased-domain path).
    pub fn insert_artifact<T: Any + Send + Sync>(&mut self, artifact: T) -> ArtifactHandle {
        let index = self.artifacts.len() as u32;
        self.artifacts.push(Box::new(artifact));
        ArtifactHandle::new(index, self.generation)
    }

    pub fn get_artifact<T: Any>(&self, handle: ArtifactHandle) -> Option<&T> {
        let index = self.artifact_index(handle)?;
        self.artifacts.get(index)?.downcast_ref::<T>()
    }

    /// Erased access distinguishing staleness from other failures.
    pub fn resolve_artifact(
        &self,
        handle: ArtifactHandle,
    ) -> Result<&(dyn Any + Send + Sync), DomainError> {
        let index = self.artifact_index_checked(handle)?;
        self.artifacts
            .get(index)
            .map(|slot| slot.as_ref())
            .ok_or(DomainError::InvalidArtifactHandle(index))
    }

    // -- verifications --------------------------------------------------------

    /// Inserts a verification payload (erased-domain path).
    pub fn insert_verification<T: Any + Send + Sync>(
        &mut self,
        verification: T,
    ) -> VerificationHandle {
        let index = self.verifications.len() as u32;
        self.verifications.push(Box::new(verification));
        VerificationHandle::new(index, self.generation)
    }

    pub fn get_verification<T: Any>(&self, handle: VerificationHandle) -> Option<&T> {
        let index = self.verification_index(handle)?;
        self.verifications.get(index)?.downcast_ref::<T>()
    }

    /// Erased access distinguishing staleness from other failures.
    pub fn resolve_verification(
        &self,
        handle: VerificationHandle,
    ) -> Result<&(dyn Any + Send + Sync), DomainError> {
        let index = self.verification_index_checked(handle)?;
        self.verifications
            .get(index)
            .map(|slot| slot.as_ref())
            .ok_or(DomainError::InvalidVerificationHandle(index))
    }

    // -- lifecycle -------------------------------------------------------------

    /// Resets the arena: drops every payload and bumps the generation, so all
    /// previously issued handles become stale (P5.2 AC).
    pub fn clear(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.states.clear();
        self.state_ids.clear();
        self.candidates.clear();
        self.artifacts.clear();
        self.verifications.clear();
    }

    // -- counts ------------------------------------------------------------------

    pub fn state_count(&self) -> usize {
        self.states.len()
    }

    pub fn candidate_count(&self) -> usize {
        self.candidates.len()
    }

    pub fn artifact_count(&self) -> usize {
        self.artifacts.len()
    }

    pub fn verification_count(&self) -> usize {
        self.verifications.len()
    }

    // -- internal helpers ----------------------------------------------------------

    fn state_index(&self, handle: StateHandle) -> Option<usize> {
        if handle.generation() != self.generation {
            return None;
        }
        self.state_ids.get(handle.index() as usize)?;
        Some(handle.index() as usize)
    }

    fn state_index_checked(&self, handle: StateHandle) -> Result<usize, DomainError> {
        if handle.generation() != self.generation {
            return Err(DomainError::StaleStateHandle {
                index: handle.index(),
                issued_generation: handle.generation(),
                current_generation: self.generation,
            });
        }
        if handle.index() as usize >= self.states.len() {
            return Err(DomainError::InvalidStateHandle(handle.index()));
        }
        Ok(handle.index() as usize)
    }

    fn artifact_index(&self, handle: ArtifactHandle) -> Option<usize> {
        if handle.1 != self.generation {
            return None;
        }
        ((handle.0 as usize) < self.artifacts.len()).then_some(handle.0 as usize)
    }

    fn artifact_index_checked(&self, handle: ArtifactHandle) -> Result<usize, DomainError> {
        if handle.1 != self.generation {
            return Err(DomainError::StaleArtifactHandle {
                index: handle.0,
                issued_generation: handle.1,
                current_generation: self.generation,
            });
        }
        if handle.0 as usize >= self.artifacts.len() {
            return Err(DomainError::InvalidArtifactHandle(handle.0 as usize));
        }
        Ok(handle.0 as usize)
    }

    fn verification_index(&self, handle: VerificationHandle) -> Option<usize> {
        if handle.1 != self.generation {
            return None;
        }
        ((handle.0 as usize) < self.verifications.len()).then_some(handle.0 as usize)
    }

    fn verification_index_checked(&self, handle: VerificationHandle) -> Result<usize, DomainError> {
        if handle.1 != self.generation {
            return Err(DomainError::StaleVerificationHandle {
                index: handle.0,
                issued_generation: handle.1,
                current_generation: self.generation,
            });
        }
        if handle.0 as usize >= self.verifications.len() {
            return Err(DomainError::InvalidVerificationHandle(handle.0 as usize));
        }
        Ok(handle.0 as usize)
    }
}

impl ArtifactHandle {
    /// Builds a handle for slot `index` issued in generation `generation`.
    pub const fn new(index: u32, generation: u64) -> Self {
        Self(index, generation)
    }
}

impl VerificationHandle {
    /// Builds a handle for slot `index` issued in generation `generation`.
    pub const fn new(index: u32, generation: u64) -> Self {
        Self(index, generation)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reflex_types::StateId;

    #[test]
    fn test_generation_stamp_and_stale_detection() {
        let mut arena = EpisodeArena::new();
        let h0 = arena.insert_state(1u32, StateId::from_digest(reflex_types::Digest::ZERO));
        assert_eq!(h0.generation(), 0);
        assert_eq!(arena.generation(), 0);
        assert_eq!(arena.get_state::<u32>(h0), Some(&1));

        arena.clear();
        assert_eq!(arena.generation(), 1);

        // Stale: same slot index, older generation.
        assert_eq!(arena.get_state::<u32>(h0), None);
        assert_eq!(arena.get_state_id(h0), None);
        match arena.resolve_state(h0) {
            Err(DomainError::StaleStateHandle {
                index,
                issued_generation,
                current_generation,
            }) => {
                assert_eq!(index, 0);
                assert_eq!(issued_generation, 0);
                assert_eq!(current_generation, 1);
            }
            other => panic!("expected stale error, got {other:?}"),
        }

        // A fresh insert reuses slot 0 under the new generation; the old
        // handle must still be rejected.
        let h1 = arena.insert_state(2u32, StateId::from_digest(reflex_types::Digest::ZERO));
        assert_eq!(h1.generation(), 1);
        assert_eq!(h1.0, 0);
        assert_eq!(arena.get_state::<u32>(h0), None);
        assert_eq!(arena.get_state::<u32>(h1), Some(&2));
        assert!(!arena.contains_state(h0));
        assert!(arena.contains_state(h1));
    }

    #[test]
    fn test_artifact_and_verification_staleness() {
        let mut arena = EpisodeArena::new();
        let ah = arena.insert_artifact("artifact".to_string());
        let vh = arena.insert_verification(7u8);
        assert_eq!(
            arena.get_artifact::<String>(ah).map(String::as_str),
            Some("artifact")
        );
        assert_eq!(arena.get_verification::<u8>(vh), Some(&7));

        arena.clear();
        assert!(matches!(
            arena.resolve_artifact(ah),
            Err(DomainError::StaleArtifactHandle { .. })
        ));
        assert!(matches!(
            arena.resolve_verification(vh),
            Err(DomainError::StaleVerificationHandle { .. })
        ));
    }

    #[test]
    fn test_out_of_range_and_type_mismatch() {
        let mut arena = EpisodeArena::new();
        let h = arena.insert_state(3u32, StateId::from_digest(reflex_types::Digest::ZERO));
        assert_eq!(arena.get_state::<f32>(h), None); // wrong type
        let bogus = StateHandle::new(99, arena.generation());
        assert_eq!(arena.get_state::<u32>(bogus), None);
        assert!(matches!(
            arena.resolve_state(bogus),
            Err(DomainError::InvalidStateHandle(99))
        ));
    }

    #[test]
    fn test_handle_generation_roundtrip_via_serialization() {
        let h = StateHandle::new(4, 3);
        let json = serde_json::to_string(&h).unwrap();
        let decoded: StateHandle = serde_json::from_str(&json).unwrap();
        assert_eq!(h, decoded);
    }
}
