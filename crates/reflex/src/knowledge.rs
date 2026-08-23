use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use sha2::{Digest, Sha256};

const MAX_ACTIVE_ARTIFACTS: usize = 4_096;
const MAX_DERIVED_OPERATORS: usize = 256;
const MAX_DERIVED_STEPS: usize = 8;
const MIN_SEMANTIC_SUPPORT: usize = 8;
const MIN_DEACTIVATION_TRIALS: u64 = 16;
const MAX_KNOWLEDGE_LINEAGE: usize = 256;

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
    summarized_attempts: u64,
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
        output.extend_from_slice(b"RFKR\x02");
        push_u64(&mut output, self.summarized_attempts);
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
        if take(&mut input, 5)? != b"RFKR\x02" {
            return Err(());
        }
        let summarized_attempts = read_u64(&mut input)?;
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
            summarized_attempts,
            active_artifacts,
            operators,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ConsolidationDecision {
    Promote,
    #[cfg(test)]
    NoChange,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct ConsolidationProduct([u8; 32]);

impl ConsolidationProduct {
    #[cfg(test)]
    pub(crate) const fn from_identity(identity: [u8; 32]) -> Self {
        Self(identity)
    }

    pub(crate) const fn identity(self) -> [u8; 32] {
        self.0
    }

    pub(crate) fn obligation_identity(self, operator: [u8; 32]) -> [u8; 32] {
        obligation_identity(self, operator)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ConsolidationSource {
    digest: [u8; 32],
    observation_count: u64,
    root_count: u64,
    pareto_count: u64,
}

impl ConsolidationSource {
    pub(crate) const fn digest(self) -> [u8; 32] {
        self.digest
    }

    #[cfg(test)]
    pub(crate) const fn observation_count(self) -> u64 {
        self.observation_count
    }

    #[cfg(test)]
    pub(crate) const fn root_count(self) -> u64 {
        self.root_count
    }

    #[cfg(test)]
    pub(crate) const fn pareto_count(self) -> u64 {
        self.pareto_count
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ConsolidationSupport {
    digest: [u8; 32],
    attempt_count: u64,
}

impl ConsolidationSupport {
    pub(crate) const fn digest(self) -> [u8; 32] {
        self.digest
    }

    #[cfg(test)]
    pub(crate) const fn attempt_count(self) -> u64 {
        self.attempt_count
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ConsolidationObligation {
    id: [u8; 32],
    operator: [u8; 32],
}

impl ConsolidationObligation {
    pub(crate) const fn id(self) -> [u8; 32] {
        self.id
    }

    #[cfg(test)]
    pub(crate) const fn operator(self) -> [u8; 32] {
        self.operator
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ConsolidationChallenger {
    id: [u8; 32],
    base_generation: u64,
    base_product: ConsolidationProduct,
    product: ConsolidationProduct,
    source: ConsolidationSource,
    support: ConsolidationSupport,
    obligations: Vec<ConsolidationObligation>,
    revision: Arc<KnowledgeRevision>,
}

impl ConsolidationChallenger {
    fn new(
        base_generation: u64,
        base: &KnowledgeRevision,
        revision: KnowledgeRevision,
        source: ConsolidationSource,
    ) -> Self {
        Self::from_pinned_revision(base_generation, base, Arc::new(revision), source)
    }

    fn from_pinned_revision(
        base_generation: u64,
        base: &KnowledgeRevision,
        revision: Arc<KnowledgeRevision>,
        source: ConsolidationSource,
    ) -> Self {
        let base_product = revision_product(base);
        let product = revision_product(revision.as_ref());
        let support = consolidation_support(base, revision.as_ref());
        let obligations = consolidation_obligations(base, revision.as_ref(), product);
        let mut challenger = Self {
            id: [0; 32],
            base_generation,
            base_product,
            product,
            source,
            support,
            obligations,
            revision,
        };
        challenger.id = challenger.identity();
        challenger
    }

    #[cfg(test)]
    pub(crate) const fn id(&self) -> [u8; 32] {
        self.id
    }

    #[cfg(test)]
    pub(crate) const fn base_generation(&self) -> u64 {
        self.base_generation
    }

    pub(crate) const fn base_product(&self) -> ConsolidationProduct {
        self.base_product
    }

    pub(crate) const fn product(&self) -> ConsolidationProduct {
        self.product
    }

    pub(crate) const fn source(&self) -> ConsolidationSource {
        self.source
    }

    pub(crate) const fn support(&self) -> ConsolidationSupport {
        self.support
    }

    pub(crate) fn obligations(&self) -> &[ConsolidationObligation] {
        &self.obligations
    }

    pub(crate) fn pinned_revision(&self) -> Arc<KnowledgeRevision> {
        Arc::clone(&self.revision)
    }

    pub(crate) fn operator_for(
        &self,
        obligation: ConsolidationObligation,
    ) -> Option<&DerivedOperator> {
        self.supporting_attempts(obligation)?;
        self.revision
            .operators
            .binary_search_by_key(&obligation.operator, |operator| operator.id)
            .ok()
            .map(|index| &self.revision.operators[index])
    }

    pub(crate) fn supporting_attempts(
        &self,
        obligation: ConsolidationObligation,
    ) -> Option<&[[u8; 32]]> {
        let manifest_index = self
            .obligations
            .binary_search_by_key(&obligation.operator, |entry| entry.operator)
            .ok()?;
        if self.obligations[manifest_index] != obligation
            || obligation_identity(self.product, obligation.operator) != obligation.id
        {
            return None;
        }
        let operator_index = self
            .revision
            .operators
            .binary_search_by_key(&obligation.operator, |operator| operator.id)
            .ok()?;
        Some(self.revision.operators[operator_index].support())
    }

    pub(crate) fn resident_bytes(&self) -> u64 {
        (std::mem::size_of_val(self) as u64)
            .saturating_add(revision_resident_bytes(&self.revision))
            .saturating_add(
                (self.obligations.capacity() as u64)
                    .saturating_mul(std::mem::size_of::<ConsolidationObligation>() as u64),
            )
    }

    pub(crate) fn encode(&self) -> Vec<u8> {
        let mut output = Vec::new();
        output.extend_from_slice(b"RFKC\x01");
        output.extend_from_slice(&self.id);
        self.encode_content(&mut output);
        output
    }

    pub(crate) fn decode(bytes: &[u8]) -> Result<Self, ()> {
        let mut input = bytes;
        if take(&mut input, 5)? != b"RFKC\x01" {
            return Err(());
        }
        let expected_id = take(&mut input, 32)?.try_into().unwrap();
        let base_generation = read_u64(&mut input)?;
        let base_product = ConsolidationProduct(take(&mut input, 32)?.try_into().unwrap());
        let product = ConsolidationProduct(take(&mut input, 32)?.try_into().unwrap());
        let source = ConsolidationSource {
            digest: take(&mut input, 32)?.try_into().unwrap(),
            observation_count: read_u64(&mut input)?,
            root_count: read_u64(&mut input)?,
            pareto_count: read_u64(&mut input)?,
        };
        let support = ConsolidationSupport {
            digest: take(&mut input, 32)?.try_into().unwrap(),
            attempt_count: read_u64(&mut input)?,
        };
        let obligation_count = usize::try_from(read_u64(&mut input)?).map_err(|_| ())?;
        if obligation_count > MAX_DERIVED_OPERATORS || obligation_count > input.len() / 64 {
            return Err(());
        }
        let mut obligations = Vec::with_capacity(obligation_count);
        for _ in 0..obligation_count {
            let obligation = ConsolidationObligation {
                id: take(&mut input, 32)?.try_into().unwrap(),
                operator: take(&mut input, 32)?.try_into().unwrap(),
            };
            if obligations
                .last()
                .is_some_and(|prior: &ConsolidationObligation| {
                    prior.operator >= obligation.operator
                })
                || obligation_identity(product, obligation.operator) != obligation.id
            {
                return Err(());
            }
            obligations.push(obligation);
        }
        let revision = Arc::new(KnowledgeRevision::decode(take_sized(&mut input)?)?);
        if !input.is_empty()
            || revision_product(&revision) != product
            || source.observation_count != revision.summarized_attempts
        {
            return Err(());
        }
        let challenger = Self {
            id: expected_id,
            base_generation,
            base_product,
            product,
            source,
            support,
            obligations,
            revision,
        };
        if challenger.identity() != expected_id {
            return Err(());
        }
        Ok(challenger)
    }

    fn identity(&self) -> [u8; 32] {
        let mut content = Vec::new();
        self.encode_content(&mut content);
        let mut digest = Sha256::new();
        digest.update(b"reflex-consolidation-challenger-v1\0");
        digest.update(content);
        digest.finalize().into()
    }

    fn encode_content(&self, output: &mut Vec<u8>) {
        push_u64(output, self.base_generation);
        output.extend_from_slice(&self.base_product.0);
        output.extend_from_slice(&self.product.0);
        output.extend_from_slice(&self.source.digest);
        push_u64(output, self.source.observation_count);
        push_u64(output, self.source.root_count);
        push_u64(output, self.source.pareto_count);
        output.extend_from_slice(&self.support.digest);
        push_u64(output, self.support.attempt_count);
        push_u64(output, self.obligations.len() as u64);
        for obligation in &self.obligations {
            output.extend_from_slice(&obligation.id);
            output.extend_from_slice(&obligation.operator);
        }
        push_bytes(output, &self.revision.encode());
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ConsolidationActivationError {
    #[cfg(test)]
    StaleBase,
    #[cfg(test)]
    WrongProduct,
    InvalidChallenger,
    Unchanged,
    GenerationOverflow,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ConsolidationRollbackError {
    StaleProduct,
    MissingPredecessor,
    GenerationOverflow,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct KnowledgeState {
    generation: u64,
    champion: Arc<KnowledgeRevision>,
    lineage: Vec<Arc<KnowledgeRevision>>,
}

impl KnowledgeState {
    #[cfg(any(test, feature = "internal-experiments"))]
    pub(crate) const fn generation(&self) -> u64 {
        self.generation
    }

    pub(crate) fn pinned_revision(&self) -> &KnowledgeRevision {
        self.champion.as_ref()
    }

    pub(crate) fn pinned_revision_arc(&self) -> Arc<KnowledgeRevision> {
        Arc::clone(&self.champion)
    }

    pub(crate) fn retained_revisions(
        &self,
    ) -> impl Iterator<Item = (ConsolidationProduct, &KnowledgeRevision)> {
        self.lineage
            .iter()
            .chain(std::iter::once(&self.champion))
            .map(|revision| (revision_product(revision.as_ref()), revision.as_ref()))
    }

    pub(crate) fn product(&self) -> ConsolidationProduct {
        revision_product(self.champion.as_ref())
    }

    /// Repository experiment adapter for the frozen no-Derived-Operator
    /// treatment. It preserves the active Artifact index and summarized
    /// Experience boundary while removing executable Derived Operators and
    /// their promotion lineage.
    #[cfg(feature = "internal-experiments")]
    pub(crate) fn without_derived_operators_for_experiment(&self) -> Self {
        let champion = KnowledgeRevision {
            summarized_attempts: self.champion.summarized_attempts,
            active_artifacts: self.champion.active_artifacts.clone(),
            operators: Vec::new(),
        };
        if champion == KnowledgeRevision::default() {
            return Self::default();
        }
        Self {
            // Match the frozen standalone treatment: one synthetic promotion
            // from empty Knowledge to the retained Artifact index with no
            // Derived Operators. Generation zero is reserved for the exact
            // default revision by the production decoder.
            generation: 1,
            champion: Arc::new(champion),
            lineage: vec![Arc::new(KnowledgeRevision::default())],
        }
    }

    pub(crate) fn propose_consolidation(
        &self,
        observations: &[DerivationObservation],
        roots: impl IntoIterator<Item = [u8; 32]>,
        pareto: impl IntoIterator<Item = [u8; 32]>,
    ) -> Option<ConsolidationChallenger> {
        let roots = roots.into_iter().collect::<BTreeSet<_>>();
        let pareto = pareto.into_iter().collect::<BTreeSet<_>>();
        let source = consolidation_source(
            revision_product(self.champion.as_ref()),
            observations,
            &roots,
            &pareto,
        );
        let revision = build_revision(
            self.champion.as_ref(),
            observations,
            roots.iter().copied(),
            pareto.iter().copied(),
        )?;
        if revision == *self.champion {
            return None;
        }
        Some(ConsolidationChallenger::new(
            self.generation,
            self.champion.as_ref(),
            revision,
            source,
        ))
    }

    #[cfg(test)]
    pub(crate) fn activate_verified_challenger(
        &mut self,
        challenger: ConsolidationChallenger,
        verified_product: ConsolidationProduct,
    ) -> Result<ConsolidationDecision, ConsolidationActivationError> {
        if challenger.base_generation != self.generation
            || challenger.base_product != revision_product(self.champion.as_ref())
        {
            return Err(ConsolidationActivationError::StaleBase);
        }
        if challenger.product != verified_product
            || challenger.product != revision_product(challenger.revision.as_ref())
        {
            return Err(ConsolidationActivationError::WrongProduct);
        }
        if challenger.identity() != challenger.id
            || challenger.source.observation_count != challenger.revision.summarized_attempts
            || challenger.support
                != consolidation_support(self.champion.as_ref(), challenger.revision.as_ref())
            || challenger.obligations
                != consolidation_obligations(
                    self.champion.as_ref(),
                    challenger.revision.as_ref(),
                    challenger.product,
                )
        {
            return Err(ConsolidationActivationError::InvalidChallenger);
        }
        if challenger.revision == self.champion {
            return Err(ConsolidationActivationError::Unchanged);
        }
        let generation = self
            .generation
            .checked_add(1)
            .ok_or(ConsolidationActivationError::GenerationOverflow)?;
        if self.lineage.len() == MAX_KNOWLEDGE_LINEAGE {
            return Err(ConsolidationActivationError::GenerationOverflow);
        }
        self.lineage.push(Arc::clone(&self.champion));
        self.champion = challenger.revision;
        self.generation = generation;
        Ok(ConsolidationDecision::Promote)
    }

    pub(crate) fn activate_reverified_challenger(
        &mut self,
        challenger: &ConsolidationChallenger,
        verified_product: ConsolidationProduct,
    ) -> Result<ConsolidationDecision, ConsolidationActivationError> {
        if challenger.base_product != self.product()
            || challenger.product != verified_product
            || challenger.product != revision_product(challenger.revision.as_ref())
            || challenger.source.observation_count != challenger.revision.summarized_attempts
            || challenger.support
                != consolidation_support(self.champion.as_ref(), challenger.revision.as_ref())
            || !consolidation_obligations_match(
                self.champion.as_ref(),
                challenger.revision.as_ref(),
                challenger.product,
                &challenger.obligations,
            )
        {
            return Err(ConsolidationActivationError::InvalidChallenger);
        }
        if challenger.revision == self.champion {
            return Err(ConsolidationActivationError::Unchanged);
        }
        let generation = self
            .generation
            .checked_add(1)
            .ok_or(ConsolidationActivationError::GenerationOverflow)?;
        if self.lineage.len() == MAX_KNOWLEDGE_LINEAGE {
            return Err(ConsolidationActivationError::GenerationOverflow);
        }
        self.lineage.push(Arc::clone(&self.champion));
        self.champion = Arc::clone(&challenger.revision);
        self.generation = generation;
        Ok(ConsolidationDecision::Promote)
    }

    pub(crate) fn rollback_promoted_product(
        &mut self,
        promoted: ConsolidationProduct,
    ) -> Result<(), ConsolidationRollbackError> {
        if self.product() != promoted {
            return Err(ConsolidationRollbackError::StaleProduct);
        }
        let predecessor = self
            .lineage
            .pop()
            .ok_or(ConsolidationRollbackError::MissingPredecessor)?;
        let generation = self
            .generation
            .checked_add(1)
            .ok_or(ConsolidationRollbackError::GenerationOverflow)?;
        self.champion = predecessor;
        self.generation = generation;
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn consolidate(
        &mut self,
        observations: &[DerivationObservation],
        roots: impl IntoIterator<Item = [u8; 32]>,
        pareto: impl IntoIterator<Item = [u8; 32]>,
    ) -> ConsolidationDecision {
        let Some(challenger) = self.propose_consolidation(observations, roots, pareto) else {
            return ConsolidationDecision::NoChange;
        };
        let product = challenger.product();
        self.activate_verified_challenger(challenger, product)
            .expect("a fresh legacy consolidation proposal has exact base and product lineage")
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
            .saturating_add(revision_resident_bytes(self.champion.as_ref()))
            .saturating_add(self.lineage.iter().fold(0, |bytes, revision| {
                bytes.saturating_add(revision_resident_bytes(revision.as_ref()))
            }))
    }

    pub(crate) fn encode(&self) -> Vec<u8> {
        let mut output = Vec::new();
        output.extend_from_slice(b"RFKS\x03");
        push_u64(&mut output, self.generation);
        push_bytes(&mut output, &self.champion.encode());
        push_u64(&mut output, self.lineage.len() as u64);
        for predecessor in &self.lineage {
            push_bytes(&mut output, &predecessor.encode());
        }
        output
    }

    pub(crate) fn decode(bytes: &[u8]) -> Result<Self, ()> {
        let mut input = bytes;
        let magic = take(&mut input, 5)?;
        let generation = read_u64(&mut input)?;
        let champion = Arc::new(KnowledgeRevision::decode(take_sized(&mut input)?)?);
        let lineage = match magic {
            b"RFKS\x02" => match take(&mut input, 1)?[0] {
                0 => Vec::new(),
                1 => vec![Arc::new(KnowledgeRevision::decode(take_sized(
                    &mut input,
                )?)?)],
                _ => return Err(()),
            },
            b"RFKS\x03" => {
                let count = usize::try_from(read_u64(&mut input)?).map_err(|_| ())?;
                if count > MAX_KNOWLEDGE_LINEAGE || count > input.len() / 8 {
                    return Err(());
                }
                let mut lineage = Vec::with_capacity(count);
                for _ in 0..count {
                    lineage.push(Arc::new(KnowledgeRevision::decode(take_sized(
                        &mut input,
                    )?)?));
                }
                lineage
            }
            _ => return Err(()),
        };
        if !input.is_empty()
            || generation == 0 && (*champion != KnowledgeRevision::default() || !lineage.is_empty())
            || lineage.last().is_some_and(|prior| prior == &champion)
            || lineage
                .windows(2)
                .any(|pair| pair[0].summarized_attempts > pair[1].summarized_attempts)
            || lineage
                .last()
                .is_some_and(|prior| prior.summarized_attempts > champion.summarized_attempts)
            || usize::try_from(generation).is_ok_and(|generation| lineage.len() > generation)
        {
            return Err(());
        }
        Ok(Self {
            generation,
            champion,
            lineage,
        })
    }

    pub(crate) fn validate(
        &self,
        artifact_keys: &BTreeSet<[u8; 32]>,
        observations: &[DerivationObservation],
        primitive_symbols: &BTreeSet<Vec<u8>>,
    ) -> bool {
        for revision in std::iter::once(&self.champion).chain(self.lineage.iter()) {
            let revision = revision.as_ref();
            let Ok(summarized_count) = usize::try_from(revision.summarized_attempts) else {
                return false;
            };
            let Some(summarized) = observations.get(..summarized_count) else {
                return false;
            };
            let mut accepted = BTreeMap::<_, Vec<_>>::new();
            for observation in summarized.iter().filter(|observation| observation.accepted) {
                accepted
                    .entry((observation.artifact, observation.claim))
                    .or_default()
                    .push(observation);
            }
            if revision
                .active_artifacts
                .iter()
                .any(|key| !artifact_keys.contains(key))
            {
                return false;
            }
            for operator in &revision.operators {
                if operator
                    .steps
                    .iter()
                    .any(|step| !primitive_symbols.contains(step))
                {
                    return false;
                }
                let mut support_claims = BTreeSet::new();
                for attempt in &operator.support {
                    let Some(child) = summarized
                        .iter()
                        .find(|item| item.id == *attempt && item.accepted)
                    else {
                        return false;
                    };
                    support_claims.insert(child.claim);
                    if !accepted
                        .get(&(child.parent, child.claim))
                        .is_some_and(|parents| {
                            parents.iter().any(|parent| {
                                let mut steps = parent.operator_steps.clone();
                                steps.extend(child.operator_steps.clone());
                                steps == operator.steps
                            })
                        })
                    {
                        return false;
                    }
                }
                if support_claims.len() < MIN_SEMANTIC_SUPPORT {
                    return false;
                }
                let observed_trials = summarized
                    .iter()
                    .filter(|item| item.operator_identity == operator.symbol)
                    .count() as u64;
                let observed_accepted = summarized
                    .iter()
                    .filter(|item| item.operator_identity == operator.symbol && item.accepted)
                    .count() as u64;
                if operator.trials != observed_trials || operator.accepted != observed_accepted {
                    return false;
                }
                if operator.trials >= MIN_DEACTIVATION_TRIALS
                    && operator.active != (operator.accepted.saturating_mul(4) >= operator.trials)
                {
                    return false;
                }
            }
        }
        true
    }

    #[cfg(feature = "internal-experiments")]
    pub(crate) fn poison_predecessor_artifact_for_test(&mut self) -> bool {
        let Some(predecessor) = self.lineage.first_mut() else {
            return false;
        };
        let revision = Arc::make_mut(predecessor);
        let foreign = [0xff; 32];
        if revision.active_artifacts.binary_search(&foreign).is_ok()
            || revision.active_artifacts.len() == MAX_ACTIVE_ARTIFACTS
        {
            return false;
        }
        revision.active_artifacts.push(foreign);
        true
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
        summarized_attempts: u64::try_from(observations.len()).unwrap_or(u64::MAX),
        active_artifacts: active.into_iter().collect(),
        operators,
    })
}

fn revision_product(revision: &KnowledgeRevision) -> ConsolidationProduct {
    let mut digest = Sha256::new();
    digest.update(b"reflex-consolidation-product-v1\0");
    digest.update(revision.encode());
    ConsolidationProduct(digest.finalize().into())
}

fn consolidation_source(
    base: ConsolidationProduct,
    observations: &[DerivationObservation],
    roots: &BTreeSet<[u8; 32]>,
    pareto: &BTreeSet<[u8; 32]>,
) -> ConsolidationSource {
    let mut digest = Sha256::new();
    digest.update(b"reflex-consolidation-source-v1\0");
    digest.update(base.0);
    push_u64_digest(&mut digest, observations.len() as u64);
    for observation in observations {
        digest.update(observation.id);
        digest.update(observation.artifact);
        digest.update(observation.parent);
        digest.update(observation.claim);
        push_u64_digest(&mut digest, observation.operator_identity.len() as u64);
        digest.update(&observation.operator_identity);
        push_u64_digest(&mut digest, observation.operator_steps.len() as u64);
        for step in &observation.operator_steps {
            push_u64_digest(&mut digest, step.len() as u64);
            digest.update(step);
        }
        digest.update([u8::from(observation.accepted)]);
    }
    push_u64_digest(&mut digest, roots.len() as u64);
    for root in roots {
        digest.update(root);
    }
    push_u64_digest(&mut digest, pareto.len() as u64);
    for artifact in pareto {
        digest.update(artifact);
    }
    ConsolidationSource {
        digest: digest.finalize().into(),
        observation_count: u64::try_from(observations.len()).unwrap_or(u64::MAX),
        root_count: u64::try_from(roots.len()).unwrap_or(u64::MAX),
        pareto_count: u64::try_from(pareto.len()).unwrap_or(u64::MAX),
    }
}

fn consolidation_obligations(
    base: &KnowledgeRevision,
    revision: &KnowledgeRevision,
    product: ConsolidationProduct,
) -> Vec<ConsolidationObligation> {
    revision
        .operators
        .iter()
        .filter(|operator| {
            base.operators
                .binary_search_by_key(&operator.id, |existing| existing.id)
                .is_err()
        })
        .map(|operator| ConsolidationObligation {
            id: obligation_identity(product, operator.id),
            operator: operator.id,
        })
        .collect()
}

fn consolidation_obligations_match(
    base: &KnowledgeRevision,
    revision: &KnowledgeRevision,
    product: ConsolidationProduct,
    expected: &[ConsolidationObligation],
) -> bool {
    let mut expected = expected.iter();
    for operator in revision.operators.iter().filter(|operator| {
        base.operators
            .binary_search_by_key(&operator.id, |existing| existing.id)
            .is_err()
    }) {
        if expected.next().is_none_or(|obligation| {
            obligation.operator != operator.id
                || obligation.id != obligation_identity(product, operator.id)
        }) {
            return false;
        }
    }
    expected.next().is_none()
}

fn consolidation_support(
    base: &KnowledgeRevision,
    revision: &KnowledgeRevision,
) -> ConsolidationSupport {
    let attempts = revision
        .operators
        .iter()
        .filter(|operator| {
            base.operators
                .binary_search_by_key(&operator.id, |existing| existing.id)
                .is_err()
        })
        .flat_map(|operator| operator.support.iter().copied())
        .collect::<BTreeSet<_>>();
    let mut digest = Sha256::new();
    digest.update(b"reflex-consolidation-support-v1\0");
    push_u64_digest(&mut digest, attempts.len() as u64);
    for attempt in &attempts {
        digest.update(attempt);
    }
    ConsolidationSupport {
        digest: digest.finalize().into(),
        attempt_count: u64::try_from(attempts.len()).unwrap_or(u64::MAX),
    }
}

fn obligation_identity(product: ConsolidationProduct, operator: [u8; 32]) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"reflex-consolidation-obligation-v1\0");
    digest.update(product.0);
    digest.update(operator);
    digest.finalize().into()
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
                .saturating_add(
                    (operator.steps.capacity() as u64)
                        .saturating_mul(std::mem::size_of::<Vec<u8>>() as u64),
                )
                .saturating_add(
                    (operator.support.capacity() as u64)
                        .saturating_mul(std::mem::size_of::<[u8; 32]>() as u64),
                ),
            |bytes, step| bytes.saturating_add(step.capacity() as u64),
        )
    });
    (revision.active_artifacts.capacity() as u64)
        .saturating_mul(std::mem::size_of::<[u8; 32]>() as u64)
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
    fn consolidation_proposal_does_not_mutate_until_verified_activation() {
        let observations = (1..=8)
            .flat_map(|case| chain(case, true))
            .collect::<Vec<_>>();
        let mut state = KnowledgeState::default();
        let before = state.encode();

        let challenger = state
            .propose_consolidation(&observations, [[20; 32]], [[21; 32]])
            .unwrap();

        assert_eq!(state.encode(), before);
        assert_eq!(challenger.source().observation_count(), 16);
        assert_eq!(challenger.source().root_count(), 1);
        assert_eq!(challenger.source().pareto_count(), 1);
        assert_eq!(challenger.support().attempt_count(), 8);
        assert_eq!(challenger.obligations().len(), 1);
        let product = challenger.product();
        assert_eq!(
            state.activate_verified_challenger(challenger, product),
            Ok(ConsolidationDecision::Promote)
        );
        assert_eq!(state.pinned_revision().operators().len(), 1);
    }

    #[test]
    fn rollback_is_exact_one_step_monotonic_and_stale_safe() {
        let observations = (1..=8)
            .flat_map(|case| chain(case, true))
            .collect::<Vec<_>>();
        let mut state = KnowledgeState::default();
        let original = state.product();
        let challenger = state
            .propose_consolidation(&observations, [[20; 32]], [[21; 32]])
            .unwrap();
        let promoted = challenger.product();
        state
            .activate_verified_challenger(challenger, promoted)
            .unwrap();
        assert_eq!(state.generation(), 1);

        let before_stale = state.encode();
        assert_eq!(
            state.rollback_promoted_product(ConsolidationProduct::from_identity([0x5a; 32])),
            Err(ConsolidationRollbackError::StaleProduct)
        );
        assert_eq!(state.encode(), before_stale);

        state.rollback_promoted_product(promoted).unwrap();
        assert_eq!(state.product(), original);
        assert_eq!(state.generation(), 2);
        assert_eq!(
            KnowledgeState::decode(&state.encode()).unwrap().encode(),
            state.encode()
        );

        let rolled_back = state.encode();
        assert_eq!(
            state.rollback_promoted_product(promoted),
            Err(ConsolidationRollbackError::StaleProduct)
        );
        assert_eq!(state.encode(), rolled_back);
    }

    #[test]
    fn consolidation_obligation_exposes_its_exact_sorted_support_attempts() {
        let observations = (1..=8)
            .flat_map(|case| chain(case, true))
            .collect::<Vec<_>>();
        let challenger = KnowledgeState::default()
            .propose_consolidation(&observations, [], [])
            .unwrap();
        let decoded = ConsolidationChallenger::decode(&challenger.encode()).unwrap();
        let obligation = decoded.obligations()[0];
        let expected = (101..=108).map(|case| [case; 32]).collect::<Vec<_>>();

        assert_eq!(
            decoded.supporting_attempts(obligation),
            Some(expected.as_slice())
        );
    }

    #[test]
    fn consolidation_obligation_support_rejects_a_forged_identity() {
        let observations = (1..=8)
            .flat_map(|case| chain(case, true))
            .collect::<Vec<_>>();
        let challenger = KnowledgeState::default()
            .propose_consolidation(&observations, [], [])
            .unwrap();
        let mut obligation = challenger.obligations()[0];
        obligation.id[0] ^= 0xff;

        assert_eq!(challenger.supporting_attempts(obligation), None);
    }

    #[test]
    fn consolidation_obligation_support_rejects_an_unknown_operator() {
        let observations = (1..=8)
            .flat_map(|case| chain(case, true))
            .collect::<Vec<_>>();
        let challenger = KnowledgeState::default()
            .propose_consolidation(&observations, [], [])
            .unwrap();
        let operator = [0xee; 32];
        let obligation = ConsolidationObligation {
            id: obligation_identity(challenger.product(), operator),
            operator,
        };

        assert_eq!(challenger.supporting_attempts(obligation), None);
    }

    #[test]
    fn consolidation_obligation_support_is_bound_to_one_product() {
        let observations = (1..=8)
            .flat_map(|case| chain(case, true))
            .collect::<Vec<_>>();
        let state = KnowledgeState::default();
        let first = state
            .propose_consolidation(&observations, [[20; 32]], [])
            .unwrap();
        let second = state
            .propose_consolidation(&observations, [[21; 32]], [])
            .unwrap();
        let first_obligation = first.obligations()[0];
        let second_obligation = second.obligations()[0];

        assert_ne!(first.product(), second.product());
        assert_eq!(first_obligation.operator(), second_obligation.operator());
        assert_eq!(first.supporting_attempts(second_obligation), None);
        assert_eq!(second.supporting_attempts(first_obligation), None);
    }

    #[test]
    fn stale_consolidation_challenger_cannot_replace_a_newer_champion() {
        let observations = (1..=8)
            .flat_map(|case| chain(case, true))
            .collect::<Vec<_>>();
        let mut state = KnowledgeState::default();
        let promoted_challenger = state
            .propose_consolidation(&observations, [[20; 32]], [])
            .unwrap();
        let stale_challenger = state
            .propose_consolidation(&observations, [[22; 32]], [])
            .unwrap();
        let promoted_product = promoted_challenger.product();
        state
            .activate_verified_challenger(promoted_challenger, promoted_product)
            .unwrap();
        let before = state.encode();
        let stale_product = stale_challenger.product();

        assert_eq!(
            state.activate_verified_challenger(stale_challenger, stale_product),
            Err(ConsolidationActivationError::StaleBase)
        );
        assert_eq!(state.encode(), before);
    }

    #[test]
    fn wrong_verified_product_cannot_mutate_the_champion() {
        let observations = (1..=8)
            .flat_map(|case| chain(case, true))
            .collect::<Vec<_>>();
        let mut state = KnowledgeState::default();
        let challenger = state.propose_consolidation(&observations, [], []).unwrap();
        let before = state.encode();
        let mut wrong = challenger.product().identity();
        wrong[0] ^= 0xff;

        assert_eq!(
            state.activate_verified_challenger(
                challenger,
                ConsolidationProduct::from_identity(wrong),
            ),
            Err(ConsolidationActivationError::WrongProduct)
        );
        assert_eq!(state.encode(), before);
    }

    #[test]
    fn unchanged_consolidation_has_no_challenger() {
        let mut state = KnowledgeState::default();
        assert!(state.propose_consolidation(&[], [], []).is_none());

        let observations = (1..=8)
            .flat_map(|case| chain(case, true))
            .collect::<Vec<_>>();
        let challenger = state
            .propose_consolidation(&observations, [[20; 32]], [[21; 32]])
            .unwrap();
        let product = challenger.product();
        state
            .activate_verified_challenger(challenger, product)
            .unwrap();

        assert!(
            state
                .propose_consolidation(&observations, [[20; 32]], [[21; 32]])
                .is_none()
        );
    }

    #[test]
    fn consolidation_challenger_round_trip_preserves_canonical_manifest() {
        let observations = (1..=8)
            .flat_map(|case| chain(case, true))
            .collect::<Vec<_>>();
        let state = KnowledgeState::default();
        let challenger = state
            .propose_consolidation(&observations, [[20; 32]], [[21; 32]])
            .unwrap();
        let encoded = challenger.encode();
        let decoded = ConsolidationChallenger::decode(&encoded).unwrap();

        assert_eq!(decoded.encode(), encoded);
        assert_eq!(decoded.id(), challenger.id());
        assert_eq!(decoded.base_generation(), 0);
        assert_eq!(decoded.base_product(), challenger.base_product());
        assert_eq!(decoded.product(), challenger.product());
        assert_eq!(decoded.source(), challenger.source());
        assert_eq!(decoded.source().digest(), challenger.source().digest());
        assert_eq!(decoded.support(), challenger.support());
        assert_eq!(decoded.support().digest(), challenger.support().digest());
        assert_eq!(decoded.obligations(), challenger.obligations());
        assert_eq!(
            decoded.obligations()[0].operator(),
            derived_id(&[b"simplify".to_vec(), b"simplify".to_vec(),])
        );
        assert_eq!(
            decoded.obligations()[0].id(),
            obligation_identity(decoded.product(), decoded.obligations()[0].operator())
        );
    }

    #[test]
    fn consolidation_challenger_resident_accounting_includes_every_capacity() {
        let observations = (1..=8)
            .flat_map(|case| chain(case, true))
            .collect::<Vec<_>>();
        let state = KnowledgeState::default();
        let challenger = state.propose_consolidation(&observations, [], []).unwrap();
        let revision = &challenger.revision;
        let exact_revision_heap = (revision.active_artifacts.capacity() as u64)
            .saturating_mul(std::mem::size_of::<[u8; 32]>() as u64)
            .saturating_add(
                (revision.operators.capacity() as u64)
                    .saturating_mul(std::mem::size_of::<DerivedOperator>() as u64),
            )
            .saturating_add(revision.operators.iter().fold(0_u64, |bytes, operator| {
                bytes
                    .saturating_add(operator.symbol.capacity() as u64)
                    .saturating_add(
                        (operator.steps.capacity() as u64)
                            .saturating_mul(std::mem::size_of::<Vec<u8>>() as u64),
                    )
                    .saturating_add(operator.steps.iter().fold(0_u64, |bytes, step| {
                        bytes.saturating_add(step.capacity() as u64)
                    }))
                    .saturating_add(
                        (operator.support.capacity() as u64).saturating_mul(std::mem::size_of::<
                            [u8; 32],
                        >(
                        )
                            as u64),
                    )
            }));
        let exact = (std::mem::size_of_val(&challenger) as u64)
            .saturating_add(exact_revision_heap)
            .saturating_add(
                (challenger.obligations.capacity() as u64)
                    .saturating_mul(std::mem::size_of::<ConsolidationObligation>() as u64),
            );

        assert_eq!(challenger.resident_bytes(), exact);
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
        let lineage_count = bad_ancestry.len() - 8;
        bad_ancestry[lineage_count..].copy_from_slice(&1_u64.to_le_bytes());
        assert!(KnowledgeState::decode(&bad_ancestry).is_err());
        let mut bad_watermark = state.clone();
        let invalid_watermark = bad_watermark.champion.summarized_attempts.saturating_add(1);
        Arc::make_mut(bad_watermark.lineage.last_mut().unwrap()).summarized_attempts =
            invalid_watermark;
        assert!(KnowledgeState::decode(&bad_watermark.encode()).is_err());
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
        let mut complete_ledger = support;
        complete_ledger.extend(failures);
        state.consolidate(&complete_ledger, [], []);
        let retained = &state.pinned_revision().operators()[0];
        let primitive_symbols = BTreeSet::from([b"simplify".to_vec()]);
        let artifact_keys = complete_ledger
            .iter()
            .flat_map(|observation| [observation.artifact, observation.parent])
            .collect();
        assert!(
            !retained.active()
                && retained.trials == 16
                && retained.accepted == 0
                && state.validate(&artifact_keys, &complete_ledger, &primitive_symbols)
        );
    }

    #[test]
    fn historical_support_survives_an_alternative_parent_derivation() {
        let support = (1..=8)
            .flat_map(|case| chain(case, true))
            .collect::<Vec<_>>();
        let mut state = KnowledgeState::default();
        state.consolidate(&support, [], []);
        let mut complete_ledger = support;
        complete_ledger.push(DerivationObservation {
            id: [240; 32],
            artifact: [2; 32],
            parent: [241; 32],
            claim: [1; 32],
            operator_identity: b"alternate".to_vec(),
            operator_steps: vec![b"alternate".to_vec()],
            accepted: true,
        });
        let artifact_keys = complete_ledger
            .iter()
            .flat_map(|observation| [observation.artifact, observation.parent])
            .collect();
        let primitive_symbols = BTreeSet::from([b"simplify".to_vec(), b"alternate".to_vec()]);
        assert!(state.validate(&artifact_keys, &complete_ledger, &primitive_symbols));
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
