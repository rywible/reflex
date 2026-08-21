use std::collections::{BTreeMap, BTreeSet};

use sha2::{Digest, Sha256};

const MAX_ACTIVE_ARTIFACTS: usize = 4_096;
const MAX_DERIVED_OPERATORS: usize = 256;
const MAX_DERIVED_STEPS: usize = 8;
const MIN_SEMANTIC_SUPPORT: usize = 8;
const MIN_DEACTIVATION_TRIALS: u64 = 16;

#[derive(Clone, Debug)]
pub(crate) struct DerivationObservation {
    pub(crate) id: [u8; 32],
    pub(crate) artifact: [u8; 32],
    pub(crate) parent: [u8; 32],
    pub(crate) claim: [u8; 32],
    pub(crate) operator_identity: Vec<u8>,
    pub(crate) operator_steps: Vec<Vec<u8>>,
    pub(crate) accepted: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DerivedOperator {
    id: [u8; 32],
    symbol: Vec<u8>,
    steps: Vec<Vec<u8>>,
    support: Vec<[u8; 32]>,
    active: bool,
    trials: u64,
    accepted: u64,
}

impl DerivedOperator {
    pub(crate) fn id(&self) -> [u8; 32] {
        self.id
    }

    pub(crate) fn symbol(&self) -> &[u8] {
        &self.symbol
    }

    pub(crate) fn steps(&self) -> &[Vec<u8>] {
        &self.steps
    }

    pub(crate) fn active(&self) -> bool {
        self.active
    }

    pub(crate) fn protected_exploration(&self) -> bool {
        self.trials < MIN_DEACTIVATION_TRIALS
    }

    pub(crate) fn support(&self) -> &[[u8; 32]] {
        &self.support
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct KnowledgeRevision {
    active_artifacts: Vec<[u8; 32]>,
    operators: Vec<DerivedOperator>,
}

impl KnowledgeRevision {
    pub(crate) fn schedules(&self, artifact: [u8; 32]) -> bool {
        self.active_artifacts.is_empty() || self.active_artifacts.binary_search(&artifact).is_ok()
    }

    pub(crate) fn operators(&self) -> &[DerivedOperator] {
        &self.operators
    }

    pub(crate) fn resolve_operator(&self, symbol: &[u8]) -> Option<&DerivedOperator> {
        self.operators
            .iter()
            .find(|operator| operator.symbol == symbol)
    }

    fn encode(&self) -> Vec<u8> {
        let mut output = Vec::new();
        output.extend_from_slice(b"RFKR\x01");
        push_u64(&mut output, self.active_artifacts.len() as u64);
        for key in &self.active_artifacts {
            output.extend_from_slice(key);
        }
        push_u64(&mut output, self.operators.len() as u64);
        for operator in &self.operators {
            output.extend_from_slice(&operator.id);
            push_bytes(&mut output, &operator.symbol);
            output.push(u8::from(operator.active));
            push_u64(&mut output, operator.trials);
            push_u64(&mut output, operator.accepted);
            push_u64(&mut output, operator.steps.len() as u64);
            for step in &operator.steps {
                push_bytes(&mut output, step);
            }
            push_u64(&mut output, operator.support.len() as u64);
            for attempt in &operator.support {
                output.extend_from_slice(attempt);
            }
        }
        output
    }

    fn decode(bytes: &[u8]) -> Result<Self, ()> {
        let mut input = bytes;
        if take(&mut input, 5)? != b"RFKR\x01" {
            return Err(());
        }
        let active_count = usize::try_from(read_u64(&mut input)?).map_err(|_| ())?;
        if active_count > input.len().saturating_div(32) || active_count > MAX_ACTIVE_ARTIFACTS {
            return Err(());
        }
        let mut active_artifacts = Vec::with_capacity(active_count);
        for _ in 0..active_count {
            let key = take(&mut input, 32)?.try_into().unwrap();
            if active_artifacts.last().is_some_and(|prior| prior >= &key) {
                return Err(());
            }
            active_artifacts.push(key);
        }
        let operator_count = usize::try_from(read_u64(&mut input)?).map_err(|_| ())?;
        if operator_count > MAX_DERIVED_OPERATORS || operator_count > input.len() / 100 {
            return Err(());
        }
        let mut operators = Vec::with_capacity(operator_count);
        for _ in 0..operator_count {
            let id: [u8; 32] = take(&mut input, 32)?.try_into().unwrap();
            let symbol = take_sized(&mut input)?.to_vec();
            let active = match take(&mut input, 1)?[0] {
                0 => false,
                1 => true,
                _ => return Err(()),
            };
            let trials = read_u64(&mut input)?;
            let accepted = read_u64(&mut input)?;
            let step_count = usize::try_from(read_u64(&mut input)?).map_err(|_| ())?;
            if !(2..=MAX_DERIVED_STEPS).contains(&step_count) {
                return Err(());
            }
            let mut steps = Vec::with_capacity(step_count);
            for _ in 0..step_count {
                let step = take_sized(&mut input)?.to_vec();
                if step.is_empty() || std::str::from_utf8(&step).is_err() {
                    return Err(());
                }
                steps.push(step);
            }
            let support_count = usize::try_from(read_u64(&mut input)?).map_err(|_| ())?;
            if support_count < MIN_SEMANTIC_SUPPORT
                || support_count > input.len().saturating_div(32)
            {
                return Err(());
            }
            let mut support = Vec::with_capacity(support_count);
            for _ in 0..support_count {
                let attempt = take(&mut input, 32)?.try_into().unwrap();
                if support.last().is_some_and(|prior| prior >= &attempt) {
                    return Err(());
                }
                support.push(attempt);
            }
            if accepted > trials
                || derived_id(&steps) != id
                || derived_symbol(id) != symbol
                || operators
                    .last()
                    .is_some_and(|prior: &DerivedOperator| prior.id >= id)
            {
                return Err(());
            }
            operators.push(DerivedOperator {
                id,
                symbol,
                steps,
                support,
                active,
                trials,
                accepted,
            });
        }
        if !input.is_empty() {
            return Err(());
        }
        Ok(Self {
            active_artifacts,
            operators,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ConsolidationDecision {
    Promote,
    NoChange,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct KnowledgeState {
    generation: u64,
    champion: KnowledgeRevision,
    predecessor: Option<KnowledgeRevision>,
}

impl KnowledgeState {
    pub(crate) fn pinned_revision(&self) -> &KnowledgeRevision {
        &self.champion
    }

    pub(crate) fn consolidate(
        &mut self,
        observations: &[DerivationObservation],
        roots: impl IntoIterator<Item = [u8; 32]>,
        pareto: impl IntoIterator<Item = [u8; 32]>,
    ) -> ConsolidationDecision {
        let Some(challenger) = build_revision(&self.champion, observations, roots, pareto) else {
            return ConsolidationDecision::NoChange;
        };
        if challenger == self.champion {
            return ConsolidationDecision::NoChange;
        }
        self.predecessor = Some(std::mem::replace(&mut self.champion, challenger));
        self.generation = self.generation.saturating_add(1);
        ConsolidationDecision::Promote
    }

    pub(crate) fn revision_digest(&self, semantic_identity: &str) -> [u8; 32] {
        let mut digest = Sha256::new();
        digest.update(b"reflex-knowledge-revision-v1\0");
        digest.update((semantic_identity.len() as u64).to_le_bytes());
        digest.update(semantic_identity.as_bytes());
        digest.update(self.champion.encode());
        digest.finalize().into()
    }

    pub(crate) fn resident_bytes(&self) -> u64 {
        (std::mem::size_of_val(self) as u64)
            .saturating_add(revision_resident_bytes(&self.champion))
            .saturating_add(self.predecessor.as_ref().map_or(0, revision_resident_bytes))
    }

    pub(crate) fn encode(&self) -> Vec<u8> {
        let mut output = Vec::new();
        output.extend_from_slice(b"RFKS\x01");
        push_u64(&mut output, self.generation);
        push_bytes(&mut output, &self.champion.encode());
        match &self.predecessor {
            Some(predecessor) => {
                output.push(1);
                push_bytes(&mut output, &predecessor.encode());
            }
            None => output.push(0),
        }
        output
    }

    pub(crate) fn decode(bytes: &[u8]) -> Result<Self, ()> {
        let mut input = bytes;
        if take(&mut input, 5)? != b"RFKS\x01" {
            return Err(());
        }
        let generation = read_u64(&mut input)?;
        let champion = KnowledgeRevision::decode(take_sized(&mut input)?)?;
        let predecessor = match take(&mut input, 1)?[0] {
            0 => None,
            1 => Some(KnowledgeRevision::decode(take_sized(&mut input)?)?),
            _ => return Err(()),
        };
        if !input.is_empty()
            || generation == 0
                && (champion != KnowledgeRevision::default() || predecessor.is_some())
            || generation > 0 && predecessor.is_none()
            || predecessor.as_ref().is_some_and(|prior| prior == &champion)
        {
            return Err(());
        }
        Ok(Self {
            generation,
            champion,
            predecessor,
        })
    }

    pub(crate) fn validate(
        &self,
        artifact_keys: &BTreeSet<[u8; 32]>,
        observations: &[DerivationObservation],
        primitive_symbols: &BTreeSet<Vec<u8>>,
    ) -> bool {
        let accepted = observations
            .iter()
            .filter(|observation| observation.accepted)
            .map(|observation| ((observation.artifact, observation.claim), observation))
            .collect::<BTreeMap<_, _>>();
        [&self.champion]
            .into_iter()
            .chain(self.predecessor.iter())
            .all(|revision| {
                revision
                    .active_artifacts
                    .iter()
                    .all(|key| artifact_keys.contains(key))
                    && revision.operators.iter().all(|operator| {
                        operator
                            .steps
                            .iter()
                            .all(|step| primitive_symbols.contains(step))
                            && operator.support.iter().all(|attempt| {
                                let Some(child) = observations
                                    .iter()
                                    .find(|item| item.id == *attempt && item.accepted)
                                else {
                                    return false;
                                };
                                let Some(parent) = accepted.get(&(child.parent, child.claim))
                                else {
                                    return false;
                                };
                                let mut steps = parent.operator_steps.clone();
                                steps.extend(child.operator_steps.clone());
                                steps == operator.steps
                            })
                            && operator
                                .support
                                .iter()
                                .filter_map(|attempt| {
                                    observations
                                        .iter()
                                        .find(|item| item.id == *attempt)
                                        .map(|item| item.claim)
                                })
                                .collect::<BTreeSet<_>>()
                                .len()
                                >= MIN_SEMANTIC_SUPPORT
                            && operator.trials
                                <= observations
                                    .iter()
                                    .filter(|item| item.operator_identity == operator.symbol)
                                    .count() as u64
                            && operator.accepted
                                <= observations
                                    .iter()
                                    .filter(|item| {
                                        item.operator_identity == operator.symbol && item.accepted
                                    })
                                    .count() as u64
                            && (operator.trials < MIN_DEACTIVATION_TRIALS
                                || operator.active
                                    == (operator.accepted.saturating_mul(4) >= operator.trials))
                    })
            })
    }
}

fn build_revision(
    prior: &KnowledgeRevision,
    observations: &[DerivationObservation],
    roots: impl IntoIterator<Item = [u8; 32]>,
    pareto: impl IntoIterator<Item = [u8; 32]>,
) -> Option<KnowledgeRevision> {
    let accepted = observations
        .iter()
        .filter(|observation| observation.accepted)
        .map(|observation| ((observation.artifact, observation.claim), observation))
        .collect::<BTreeMap<_, _>>();
    let mut discovered = BTreeMap::<Vec<Vec<u8>>, (BTreeSet<[u8; 32]>, BTreeSet<[u8; 32]>)>::new();
    for child in observations
        .iter()
        .filter(|observation| observation.accepted)
    {
        let Some(parent) = accepted.get(&(child.parent, child.claim)) else {
            continue;
        };
        let mut steps = parent.operator_steps.clone();
        steps.extend(child.operator_steps.clone());
        if !(2..=MAX_DERIVED_STEPS).contains(&steps.len()) {
            continue;
        }
        let evidence = discovered.entry(steps).or_default();
        evidence.0.insert(child.claim);
        evidence.1.insert(child.id);
    }
    let mut operators = prior.operators.clone();
    let mut new_operators = Vec::new();
    for (steps, (claims, attempts)) in discovered {
        if claims.len() < MIN_SEMANTIC_SUPPORT {
            continue;
        }
        let id = derived_id(&steps);
        if let Some(operator) = operators.iter_mut().find(|operator| operator.id == id) {
            operator.support.extend(attempts);
            operator.support.sort_unstable();
            operator.support.dedup();
            continue;
        }
        new_operators.push(DerivedOperator {
            id,
            symbol: derived_symbol(id),
            steps,
            support: attempts.into_iter().collect(),
            active: true,
            trials: 0,
            accepted: 0,
        });
    }
    new_operators.sort_unstable_by_key(|operator| operator.id);
    new_operators.truncate(MAX_DERIVED_OPERATORS.saturating_sub(operators.len()));
    operators.extend(new_operators);
    for operator in &mut operators {
        operator.trials = observations
            .iter()
            .filter(|observation| observation.operator_identity == operator.symbol)
            .count() as u64;
        operator.accepted = observations
            .iter()
            .filter(|observation| {
                observation.operator_identity == operator.symbol && observation.accepted
            })
            .count() as u64;
        if operator.trials >= MIN_DEACTIVATION_TRIALS {
            operator.active = operator.accepted.saturating_mul(4) >= operator.trials;
        }
    }
    operators.sort_unstable_by_key(|operator| operator.id);

    let mut active = roots.into_iter().chain(pareto).collect::<BTreeSet<_>>();
    if active.len() > MAX_ACTIVE_ARTIFACTS {
        return None;
    }
    let mut scores = BTreeMap::<[u8; 32], u64>::new();
    for observation in observations
        .iter()
        .filter(|observation| observation.accepted)
    {
        *scores.entry(observation.artifact).or_default() += 1;
        *scores.entry(observation.parent).or_default() += 2;
    }
    let mut candidates = scores.into_iter().collect::<Vec<_>>();
    candidates.sort_unstable_by(|(left_key, left_score), (right_key, right_score)| {
        right_score
            .cmp(left_score)
            .then_with(|| left_key.cmp(right_key))
    });
    for (key, _) in candidates {
        if active.len() >= MAX_ACTIVE_ARTIFACTS {
            break;
        }
        active.insert(key);
    }
    Some(KnowledgeRevision {
        active_artifacts: active.into_iter().collect(),
        operators,
    })
}

fn derived_id(steps: &[Vec<u8>]) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"reflex-derived-operator-v1\0");
    push_u64_digest(&mut digest, steps.len() as u64);
    for step in steps {
        push_u64_digest(&mut digest, step.len() as u64);
        digest.update(step);
    }
    digest.finalize().into()
}

fn derived_symbol(id: [u8; 32]) -> Vec<u8> {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut symbol = b"derived:".to_vec();
    for byte in id {
        symbol.push(DIGITS[usize::from(byte >> 4)]);
        symbol.push(DIGITS[usize::from(byte & 0x0f)]);
    }
    symbol
}

fn revision_resident_bytes(revision: &KnowledgeRevision) -> u64 {
    let operators = revision.operators.iter().fold(0_u64, |bytes, operator| {
        operator.steps.iter().fold(
            bytes
                .saturating_add(operator.symbol.capacity() as u64)
                .saturating_add((operator.support.capacity() * 32) as u64),
            |bytes, step| bytes.saturating_add(step.capacity() as u64),
        )
    });
    (revision.active_artifacts.capacity() as u64)
        .saturating_mul(32)
        .saturating_add(
            (revision.operators.capacity() as u64)
                .saturating_mul(std::mem::size_of::<DerivedOperator>() as u64),
        )
        .saturating_add(operators)
}

fn push_u64(output: &mut Vec<u8>, value: u64) {
    output.extend_from_slice(&value.to_le_bytes());
}

fn push_u64_digest(output: &mut Sha256, value: u64) {
    output.update(value.to_le_bytes());
}

fn push_bytes(output: &mut Vec<u8>, bytes: &[u8]) {
    push_u64(output, bytes.len() as u64);
    output.extend_from_slice(bytes);
}

fn read_u64(input: &mut &[u8]) -> Result<u64, ()> {
    Ok(u64::from_le_bytes(take(input, 8)?.try_into().unwrap()))
}

fn take_sized<'a>(input: &mut &'a [u8]) -> Result<&'a [u8], ()> {
    let length = usize::try_from(read_u64(input)?).map_err(|_| ())?;
    take(input, length)
}

fn take<'a>(input: &mut &'a [u8], count: usize) -> Result<&'a [u8], ()> {
    if input.len() < count {
        return Err(());
    }
    let (value, remainder) = input.split_at(count);
    *input = remainder;
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chain(case: u8, accepted: bool) -> [DerivationObservation; 2] {
        [
            DerivationObservation {
                id: [case; 32],
                artifact: [case.saturating_add(1); 32],
                parent: [case.saturating_add(10); 32],
                claim: [case; 32],
                operator_identity: b"simplify".to_vec(),
                operator_steps: vec![b"simplify".to_vec()],
                accepted: true,
            },
            DerivationObservation {
                id: [case.saturating_add(100); 32],
                artifact: [case.saturating_add(2); 32],
                parent: [case.saturating_add(1); 32],
                claim: [case; 32],
                operator_identity: b"simplify".to_vec(),
                operator_steps: vec![b"simplify".to_vec()],
                accepted,
            },
        ]
    }

    #[test]
    fn extracts_only_cross_case_supported_verified_macros() {
        let seven = (1..=7)
            .flat_map(|case| chain(case, true))
            .collect::<Vec<_>>();
        let eight = (1..=8)
            .flat_map(|case| chain(case, true))
            .collect::<Vec<_>>();
        let mut state = KnowledgeState::default();
        assert_eq!(
            state.consolidate(&seven, [], []),
            ConsolidationDecision::Promote
        );
        assert!(state.pinned_revision().operators().is_empty());
        assert_eq!(
            state.consolidate(&eight, [], []),
            ConsolidationDecision::Promote
        );
        let operator = &state.pinned_revision().operators()[0];
        assert!(
            operator.active()
                && operator.steps() == [b"simplify".to_vec(), b"simplify".to_vec()]
                && operator.support.len() == 8
        );
    }

    #[test]
    fn canonical_state_round_trips_and_rejects_malformed_programs_and_ancestry() {
        let observations = (1..=8)
            .flat_map(|case| chain(case, true))
            .collect::<Vec<_>>();
        let mut state = KnowledgeState::default();
        state.consolidate(&observations, [[20; 32]], [[21; 32]]);
        let encoded = state.encode();
        let decoded = KnowledgeState::decode(&encoded).unwrap();
        assert!(
            decoded.encode() == encoded
                && decoded.revision_digest("domain") == state.revision_digest("domain")
        );

        let mut bad_ancestry = KnowledgeState::default().encode();
        bad_ancestry[5..13].copy_from_slice(&1_u64.to_le_bytes());
        assert!(KnowledgeState::decode(&bad_ancestry).is_err());
        let mut malformed = state.pinned_revision().encode();
        let step = malformed
            .windows(8)
            .position(|window| window == b"simplify")
            .unwrap();
        malformed[step] = 0xff;
        assert!(KnowledgeRevision::decode(&malformed).is_err());
    }

    #[test]
    fn evidence_deactivates_but_does_not_delete_a_failed_operator() {
        let support = (1..=8)
            .flat_map(|case| chain(case, true))
            .collect::<Vec<_>>();
        let mut state = KnowledgeState::default();
        state.consolidate(&support, [], []);
        let operator = state.pinned_revision().operators()[0].clone();
        let failures = (0..16)
            .map(|case| DerivationObservation {
                id: [case; 32],
                artifact: [case.saturating_add(1); 32],
                parent: [case.saturating_add(2); 32],
                claim: [case; 32],
                operator_identity: operator.symbol.clone(),
                operator_steps: operator.steps.clone(),
                accepted: false,
            })
            .collect::<Vec<_>>();
        state.consolidate(&failures, [], []);
        let retained = &state.pinned_revision().operators()[0];
        assert!(!retained.active() && retained.trials == 16 && retained.accepted == 0);
    }

    #[test]
    fn active_index_is_canonical_and_bounded_without_dropping_roots_or_pareto() {
        let observations = (0_u16..5_000)
            .map(|index| DerivationObservation {
                id: [u8::try_from(index % 251).unwrap(); 32],
                artifact: Sha256::digest(index.to_le_bytes()).into(),
                parent: Sha256::digest(index.saturating_add(1).to_le_bytes()).into(),
                claim: [0; 32],
                operator_identity: b"primitive".to_vec(),
                operator_steps: vec![b"primitive".to_vec()],
                accepted: true,
            })
            .collect::<Vec<_>>();
        let roots = [[250; 32]];
        let pareto = [[251; 32]];
        let mut state = KnowledgeState::default();
        state.consolidate(&observations, roots, pareto);
        let active = &state.pinned_revision().active_artifacts;
        assert!(
            active.len() == MAX_ACTIVE_ARTIFACTS
                && active.binary_search(&roots[0]).is_ok()
                && active.binary_search(&pareto[0]).is_ok()
                && active.windows(2).all(|pair| pair[0] < pair[1])
        );
    }
}
