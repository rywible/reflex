//! Internal temporal-learning support for the repository experiment harness.
//!
//! This module is deliberately hidden from generated consumer documentation.
//! It contains no orchestration authority: it derives replayable training data
//! from pinned catalogs and asks pinned workers to certify every relationship.

use std::collections::{BTreeMap, HashMap, HashSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::ast::{LeanExpr, LeanName};
use crate::catalog::{CatalogEntry, DeclarationKind, LeanCatalog};
use crate::worker::{
    IndexedTheorem, LeanWorker, VerificationItem, VerificationResult, WorkerError,
};

pub const TASTE_FEATURES: usize = 64;
pub const POTENTIAL_HEADS: usize = 7;
const SCREEN_FEATURES: usize = 16;
const TRAINING_EPOCHS: usize = 8;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PotentialHead {
    Anticipation,
    Descendants,
    Reuse,
    Compression,
    MigrationSurvival,
    VerificationCost,
    DeadEnd,
}

impl PotentialHead {
    pub const ALL: [Self; POTENTIAL_HEADS] = [
        Self::Anticipation,
        Self::Descendants,
        Self::Reuse,
        Self::Compression,
        Self::MigrationSurvival,
        Self::VerificationCost,
        Self::DeadEnd,
    ];

    const fn index(self) -> usize {
        match self {
            Self::Anticipation => 0,
            Self::Descendants => 1,
            Self::Reuse => 2,
            Self::Compression => 3,
            Self::MigrationSurvival => 4,
            Self::VerificationCost => 5,
            Self::DeadEnd => 6,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TemporalExample {
    pub declaration: LeanName,
    pub semantic_group: [u8; 32],
    pub features: Vec<f32>,
    pub targets: [f32; POTENTIAL_HEADS],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RelationshipKind {
    Exact,
    Definitional,
    Specialization,
    Derivation,
    FamilyCollapse,
    CorpusCompression,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RelationshipCandidate {
    pub earlier: LeanName,
    pub later: LeanName,
    pub expected: RelationshipKind,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CertifiedRelationship {
    pub kind: RelationshipKind,
    pub earlier: IndexedTheorem,
    pub later: IndexedTheorem,
    pub verification: VerificationResult,
    pub proof_nodes_removed: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ConsolidatedRelationship {
    pub kind: RelationshipKind,
    pub source: LeanName,
    pub members: Vec<CertifiedRelationship>,
    pub proof_nodes_removed: usize,
}

#[derive(Clone, Debug)]
struct GraphEntry {
    name: LeanName,
    statement_hash: u64,
    dependencies: Vec<LeanName>,
    kind: DeclarationKind,
    inbound: usize,
}

#[derive(Clone, Debug)]
pub struct TemporalSnapshot {
    pub environment_sha256: String,
    entries: Vec<GraphEntry>,
}

impl TemporalSnapshot {
    #[must_use]
    pub fn from_catalog(catalog: &LeanCatalog) -> Self {
        let eligible = catalog
            .eligible_entries()
            .filter_map(|entry| graph_entry(catalog, entry))
            .collect::<Vec<_>>();
        let names = eligible
            .iter()
            .map(|entry| entry.name.clone())
            .collect::<HashSet<_>>();
        let mut inbound = HashMap::<LeanName, usize>::new();
        for entry in &eligible {
            for dependency in &entry.dependencies {
                if names.contains(dependency) {
                    *inbound.entry(dependency.clone()).or_default() += 1;
                }
            }
        }
        let entries = eligible
            .into_iter()
            .map(|mut entry| {
                entry.inbound = inbound.get(&entry.name).copied().unwrap_or(0);
                entry
            })
            .collect();
        Self {
            environment_sha256: environment_digest(catalog),
            entries,
        }
    }

    #[must_use]
    pub fn eligible_declarations(&self) -> usize {
        self.entries.len()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TemporalPair {
    pub earlier_environment_sha256: String,
    pub later_environment_sha256: String,
    pub examples: Vec<TemporalExample>,
    pub relationship_candidates: Vec<RelationshipCandidate>,
    pub later_new_declarations: usize,
}

impl TemporalPair {
    #[expect(
        clippy::too_many_lines,
        reason = "pair derivation keeps every delayed target tied to the same auditable graph pass"
    )]
    pub fn derive(earlier: &TemporalSnapshot, later: &TemporalSnapshot) -> Result<Self, String> {
        if earlier.environment_sha256 == later.environment_sha256 {
            return Err("a Temporal Snapshot Pair requires distinct Semantic Identities".into());
        }
        let earlier_by_name = earlier
            .entries
            .iter()
            .map(|entry| (entry.name.clone(), entry))
            .collect::<HashMap<_, _>>();
        let later_by_name = later
            .entries
            .iter()
            .map(|entry| (entry.name.clone(), entry))
            .collect::<HashMap<_, _>>();
        let mut earlier_by_statement = HashMap::<u64, Vec<&GraphEntry>>::new();
        for entry in &earlier.entries {
            earlier_by_statement
                .entry(entry.statement_hash)
                .or_default()
                .push(entry);
        }
        let new_later = later
            .entries
            .iter()
            .filter(|entry| {
                matches!(entry.kind, DeclarationKind::Theorem)
                    && !earlier_by_name.contains_key(&entry.name)
            })
            .collect::<Vec<_>>();
        let mut descendants = HashMap::<LeanName, usize>::new();
        let mut reuse = HashMap::<LeanName, usize>::new();
        let mut compression = HashMap::<LeanName, usize>::new();
        let mut candidates = Vec::new();
        for entry in &new_later {
            for dependency in &entry.dependencies {
                if let Some(source) = earlier_by_name
                    .get(dependency)
                    .filter(|source| matches!(source.kind, DeclarationKind::Theorem))
                {
                    *descendants.entry(source.name.clone()).or_default() += 1;
                    *reuse.entry(source.name.clone()).or_default() += 1;
                    candidates.push(RelationshipCandidate {
                        earlier: source.name.clone(),
                        later: entry.name.clone(),
                        expected: if matches!(source.kind, DeclarationKind::Theorem)
                            && entry_looks_general(&source.dependencies)
                        {
                            RelationshipKind::Specialization
                        } else {
                            RelationshipKind::Derivation
                        },
                    });
                }
            }
            if let Some(sources) = earlier_by_statement.get(&entry.statement_hash) {
                for source in sources
                    .iter()
                    .filter(|source| matches!(source.kind, DeclarationKind::Theorem))
                {
                    *compression.entry(source.name.clone()).or_default() += 1;
                    candidates.push(RelationshipCandidate {
                        earlier: source.name.clone(),
                        later: entry.name.clone(),
                        expected: RelationshipKind::Exact,
                    });
                }
            }
        }
        candidates.sort_by(|left, right| {
            (&left.earlier, &left.later, left.expected as u8).cmp(&(
                &right.earlier,
                &right.later,
                right.expected as u8,
            ))
        });
        candidates.dedup();
        let examples = earlier
            .entries
            .iter()
            .filter(|entry| matches!(entry.kind, DeclarationKind::Theorem))
            .map(|entry| {
                let descendant_count = descendants.get(&entry.name).copied().unwrap_or(0);
                let reuse_count = reuse.get(&entry.name).copied().unwrap_or(0);
                let compression_count = compression.get(&entry.name).copied().unwrap_or(0);
                let survived = later_by_name.contains_key(&entry.name);
                let anticipated = descendant_count != 0 || compression_count != 0;
                TemporalExample {
                    declaration: entry.name.clone(),
                    semantic_group: semantic_group(entry),
                    features: features(entry),
                    targets: [
                        f32::from(anticipated),
                        saturating_count(descendant_count),
                        saturating_count(reuse_count),
                        saturating_count(compression_count),
                        f32::from(survived),
                        ((bounded_f32(entry.dependencies.len()) + 1.0).ln_1p() / 8.0).min(1.0),
                        f32::from(!anticipated && !survived),
                    ],
                }
            })
            .collect();
        Ok(Self {
            earlier_environment_sha256: earlier.environment_sha256.clone(),
            later_environment_sha256: later.environment_sha256.clone(),
            examples,
            relationship_candidates: candidates,
            later_new_declarations: new_later.len(),
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MigrationFailure {
    pub declaration: LeanName,
    pub diagnostic: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MigrationReport {
    pub migrated: Vec<IndexedTheorem>,
    pub failures: Vec<MigrationFailure>,
}

pub fn migrate_theorems(
    source: &[IndexedTheorem],
    target: &LeanWorker,
) -> Result<MigrationReport, WorkerError> {
    let items = source
        .iter()
        .map(|theorem| VerificationItem {
            level_params: theorem.level_params.clone(),
            claim_proposition: theorem.proposition.clone(),
            candidate_proposition: theorem.proposition.clone(),
            proof_term: theorem.proof_term.clone(),
            allowed_axioms: theorem.axioms.clone(),
        })
        .collect::<Vec<_>>();
    let (results, _) = target.verify(&items)?;
    let mut report = MigrationReport {
        migrated: Vec::new(),
        failures: Vec::new(),
    };
    for (theorem, result) in source.iter().zip(results) {
        if result.accepted {
            let mut migrated = theorem.clone();
            migrated.dependencies = result.dependencies;
            migrated.axioms = result.axioms;
            report.migrated.push(migrated);
        } else {
            report.failures.push(MigrationFailure {
                declaration: theorem.name.clone(),
                diagnostic: result.diagnostic,
            });
        }
    }
    Ok(report)
}

pub fn certify_relationship(
    earlier: &LeanWorker,
    later: &LeanWorker,
    candidate: &RelationshipCandidate,
) -> Result<Option<CertifiedRelationship>, WorkerError> {
    let source = earlier.fetch(std::slice::from_ref(&candidate.earlier))?;
    let target = later.fetch(std::slice::from_ref(&candidate.later))?;
    let (Some(source), Some(target)) = (source.into_iter().next(), target.into_iter().next())
    else {
        return Ok(None);
    };
    let direct_derivation = target.dependencies.contains(&source.name);
    let replacement = matches!(
        candidate.expected,
        RelationshipKind::Exact | RelationshipKind::Definitional
    );
    let item = if replacement {
        VerificationItem {
            level_params: target.level_params.clone(),
            claim_proposition: target.proposition.clone(),
            candidate_proposition: source.proposition.clone(),
            proof_term: source.proof_term.clone(),
            allowed_axioms: target.axioms.clone(),
        }
    } else {
        if !direct_derivation {
            return Ok(None);
        }
        VerificationItem {
            level_params: target.level_params.clone(),
            claim_proposition: target.proposition.clone(),
            candidate_proposition: target.proposition.clone(),
            proof_term: target.proof_term.clone(),
            allowed_axioms: target.axioms.clone(),
        }
    };
    let (mut results, _) = later.verify(&[item])?;
    let verification = results.pop().expect("one verification item has one result");
    if !verification.accepted {
        return Ok(None);
    }
    let kind = if replacement {
        if source.proposition == target.proposition {
            RelationshipKind::Exact
        } else {
            RelationshipKind::Definitional
        }
    } else if matches!(source.proposition, LeanExpr::ForallE { .. }) {
        RelationshipKind::Specialization
    } else {
        RelationshipKind::Derivation
    };
    let proof_nodes_removed = if replacement {
        target
            .proof_term
            .node_count()
            .saturating_sub(source.proof_term.node_count())
    } else {
        0
    };
    Ok(Some(CertifiedRelationship {
        kind,
        earlier: source,
        later: target,
        verification,
        proof_nodes_removed,
    }))
}

#[must_use]
pub fn consolidate_certificates(
    certificates: &[CertifiedRelationship],
) -> Vec<ConsolidatedRelationship> {
    let mut by_source = BTreeMap::<LeanName, Vec<CertifiedRelationship>>::new();
    for certificate in certificates.iter().cloned() {
        by_source
            .entry(certificate.earlier.name.clone())
            .or_default()
            .push(certificate);
    }
    let mut consolidated = Vec::new();
    for (source, members) in by_source {
        let removed = members
            .iter()
            .map(|member| member.proof_nodes_removed)
            .sum();
        if members.len() >= 2 {
            consolidated.push(ConsolidatedRelationship {
                kind: RelationshipKind::FamilyCollapse,
                source: source.clone(),
                members: members.clone(),
                proof_nodes_removed: removed,
            });
        }
        if removed != 0 {
            consolidated.push(ConsolidatedRelationship {
                kind: RelationshipKind::CorpusCompression,
                source,
                members,
                proof_nodes_removed: removed,
            });
        }
    }
    consolidated
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct PotentialForecast {
    pub estimate: f32,
    pub uncertainty: f32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Treatment {
    Full,
    Bootstrap,
    NoModel,
    NoConsolidation,
    ImmediateOnly,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TasteModel {
    screen: LinearModel,
    ranker: LinearModel,
    retrieval: Vec<RetrievalCell>,
    examples: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct LinearModel {
    z: Vec<[f32; POTENTIAL_HEADS]>,
    n: Vec<[f32; POTENTIAL_HEADS]>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct RetrievalCell {
    count: u32,
    targets: [f32; POTENTIAL_HEADS],
}

impl TasteModel {
    #[must_use]
    pub fn train(examples: &[TemporalExample], treatment: Treatment) -> Self {
        let mut model = Self {
            screen: LinearModel::new(SCREEN_FEATURES),
            ranker: LinearModel::new(TASTE_FEATURES),
            retrieval: vec![
                RetrievalCell {
                    count: 0,
                    targets: [0.0; POTENTIAL_HEADS],
                };
                256
            ],
            examples: 0,
        };
        if matches!(treatment, Treatment::Bootstrap | Treatment::NoModel) {
            return model;
        }
        for _ in 0..TRAINING_EPOCHS {
            for example in examples {
                let mut targets = example.targets;
                if treatment == Treatment::ImmediateOnly {
                    for (index, target) in targets.iter_mut().enumerate().skip(1) {
                        if index != PotentialHead::VerificationCost.index() {
                            *target = 0.0;
                        }
                    }
                }
                if treatment == Treatment::NoConsolidation {
                    targets[PotentialHead::Compression.index()] = 0.0;
                }
                model
                    .screen
                    .update(&example.features[..SCREEN_FEATURES], targets);
                model.ranker.update(&example.features, targets);
            }
        }
        for example in examples {
            let cell = &mut model.retrieval[retrieval_bucket(&example.features)];
            cell.count = cell.count.saturating_add(1);
            let count = f32::from(u16::try_from(cell.count).unwrap_or(u16::MAX));
            for (mean, target) in cell.targets.iter_mut().zip(example.targets) {
                *mean += (target - *mean) / count;
            }
        }
        model.examples = examples.len() as u64;
        model
    }

    #[must_use]
    pub fn forecast(&self, features: &[f32]) -> [PotentialForecast; POTENTIAL_HEADS] {
        let screen = self.screen.predict(&features[..SCREEN_FEATURES]);
        let ranker = self.ranker.predict(features);
        let cell = &self.retrieval[retrieval_bucket(features)];
        std::array::from_fn(|head| {
            let support = self.ranker.support(features, head);
            let retrieved = if cell.count == 0 {
                ranker[head]
            } else {
                cell.targets[head]
            };
            PotentialForecast {
                estimate: (screen[head] * 0.15 + ranker[head] * 0.65 + retrieved * 0.20)
                    .clamp(0.0, 1.0),
                uncertainty: (1.0
                    / (1.0 + support + f32::from(u16::try_from(cell.count).unwrap_or(u16::MAX))))
                .sqrt(),
            }
        })
    }

    #[must_use]
    pub fn rank(&self, examples: &[TemporalExample], limit: usize) -> Vec<usize> {
        if limit == 0 {
            return Vec::new();
        }
        let mut screened = examples
            .iter()
            .enumerate()
            .map(|(index, example)| {
                let forecast = self.screen.predict(&example.features[..SCREEN_FEATURES]);
                (index, economic_key(forecast, [0.0; POTENTIAL_HEADS]))
            })
            .collect::<Vec<_>>();
        let screen_limit = limit.saturating_mul(4).max(limit).min(screened.len());
        select_prefix(&mut screened, screen_limit, |left, right| {
            right.1.total_cmp(&left.1)
        });
        let mut ranked = screened
            .into_iter()
            .map(|(index, _)| {
                let forecast = self.forecast(&examples[index].features);
                let estimates = forecast.map(|head| head.estimate);
                let uncertainty = forecast.map(|head| head.uncertainty);
                (index, economic_key(estimates, uncertainty), uncertainty[0])
            })
            .collect::<Vec<_>>();
        ranked.sort_unstable_by(|left, right| {
            right
                .1
                .total_cmp(&left.1)
                .then_with(|| left.0.cmp(&right.0))
        });
        let exploration = limit.div_ceil(8).min(ranked.len());
        let mut selected = ranked
            .iter()
            .take(limit.saturating_sub(exploration))
            .map(|entry| entry.0)
            .collect::<Vec<_>>();
        let mut uncertain = ranked
            .iter()
            .skip(limit.saturating_sub(exploration))
            .copied()
            .collect::<Vec<_>>();
        uncertain.sort_unstable_by(|left, right| right.2.total_cmp(&left.2));
        selected.extend(uncertain.into_iter().take(exploration).map(|entry| entry.0));
        selected
    }

    pub fn encode(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec(self)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        let model: Self = serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
        if model.screen.z.len() != SCREEN_FEATURES
            || model.ranker.z.len() != TASTE_FEATURES
            || model.screen.n.len() != SCREEN_FEATURES
            || model.ranker.n.len() != TASTE_FEATURES
            || model.retrieval.len() != 256
            || model
                .screen
                .z
                .iter()
                .chain(&model.screen.n)
                .chain(&model.ranker.z)
                .chain(&model.ranker.n)
                .flatten()
                .any(|value| !value.is_finite())
        {
            return Err("taste model dimensions or numeric state differ".into());
        }
        Ok(model)
    }

    #[must_use]
    pub fn content_sha256(&self) -> String {
        hex(&Sha256::digest(
            self.encode()
                .expect("serializing finite taste state cannot fail"),
        ))
    }
}

impl LinearModel {
    fn new(features: usize) -> Self {
        Self {
            z: vec![[0.0; POTENTIAL_HEADS]; features],
            n: vec![[0.0; POTENTIAL_HEADS]; features],
        }
    }

    fn predict(&self, features: &[f32]) -> [f32; POTENTIAL_HEADS] {
        let mut linear = [0.0; POTENTIAL_HEADS];
        for (index, feature) in features.iter().copied().enumerate() {
            if feature == 0.0 {
                continue;
            }
            for (head, value) in linear.iter_mut().enumerate() {
                *value += weight(self.z[index][head], self.n[index][head]) * feature;
            }
        }
        linear.map(sigmoid)
    }

    fn support(&self, features: &[f32], head: usize) -> f32 {
        features
            .iter()
            .zip(&self.n)
            .map(|(feature, support)| feature * feature * support[head])
            .sum()
    }

    fn update(&mut self, features: &[f32], targets: [f32; POTENTIAL_HEADS]) {
        const ALPHA: f32 = 0.1;
        let prediction = self.predict(features);
        for (index, feature) in features.iter().copied().enumerate() {
            if feature == 0.0 {
                continue;
            }
            for head in 0..POTENTIAL_HEADS {
                let gradient = (prediction[head] - targets[head]) * feature;
                let old_n = self.n[index][head];
                let old_weight = weight(self.z[index][head], old_n);
                let new_n = old_n + gradient * gradient;
                let sigma = (new_n.sqrt() - old_n.sqrt()) / ALPHA;
                self.z[index][head] += gradient - sigma * old_weight;
                self.n[index][head] = new_n;
            }
        }
    }
}

fn weight(z: f32, n: f32) -> f32 {
    const ALPHA: f32 = 0.1;
    -z / ((1.0 + n.sqrt()) / ALPHA + 1.0)
}

fn sigmoid(value: f32) -> f32 {
    if value >= 0.0 {
        1.0 / (1.0 + (-value).exp())
    } else {
        let exp = value.exp();
        exp / (1.0 + exp)
    }
}

fn economic_key(estimates: [f32; POTENTIAL_HEADS], uncertainty: [f32; POTENTIAL_HEADS]) -> f32 {
    let positive = estimates[0] + estimates[1] + estimates[2] + estimates[3] + estimates[4];
    let cost = estimates[5] + estimates[6];
    let confidence_penalty = uncertainty.iter().sum::<f32>() / 7.0;
    positive - cost - confidence_penalty
}

fn select_prefix<T>(
    values: &mut Vec<T>,
    limit: usize,
    mut compare: impl FnMut(&T, &T) -> std::cmp::Ordering,
) {
    if values.len() <= limit {
        values.sort_unstable_by(compare);
    } else {
        let (prefix, _, _) = values.select_nth_unstable_by(limit, &mut compare);
        prefix.sort_unstable_by(compare);
        values.truncate(limit);
    }
}

fn graph_entry(catalog: &LeanCatalog, entry: &CatalogEntry) -> Option<GraphEntry> {
    Some(GraphEntry {
        name: catalog.name(entry.name)?,
        statement_hash: entry.statement_hash,
        dependencies: entry
            .dependencies
            .iter()
            .filter_map(|dependency| catalog.name(*dependency))
            .collect(),
        kind: entry.kind.clone(),
        inbound: 0,
    })
}

fn environment_digest(catalog: &LeanCatalog) -> String {
    let mut digest = Sha256::new();
    digest.update(b"reflex-lean-temporal-snapshot-v1\0");
    digest.update(catalog.content_sha256());
    digest.update(catalog.environment().mathlib_commit.as_bytes());
    digest.update(catalog.environment().lean_commit.as_bytes());
    digest.update(catalog.environment().worker_source_sha256.as_bytes());
    hex(&digest.finalize())
}

fn semantic_group(entry: &GraphEntry) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"reflex-lean-semantic-group-v1\0");
    digest.update(entry.statement_hash.to_le_bytes());
    for dependency in &entry.dependencies {
        digest.update(dependency.to_string().as_bytes());
        digest.update([0]);
    }
    digest.finalize().into()
}

fn features(entry: &GraphEntry) -> Vec<f32> {
    let mut features = vec![0.0; TASTE_FEATURES];
    features[0] = 1.0;
    features[1] = (bounded_f32(entry.dependencies.len()) + 1.0).ln_1p() / 8.0;
    features[2] = (bounded_f32(entry.inbound) + 1.0).ln_1p() / 8.0;
    features[3 + usize::from(entry.kind.code())] = 1.0;
    for bit in 0..32 {
        let pair = u8::try_from((entry.statement_hash >> (bit * 2)) & 3)
            .expect("two statement-hash bits fit in u8");
        features[16 + bit] = f32::from(pair) / 3.0;
    }
    for dependency in &entry.dependencies {
        let digest = Sha256::digest(dependency.to_string().as_bytes());
        let bucket = 48 + usize::from(digest[0] & 15);
        features[bucket] = (features[bucket] + 0.25_f32).min(1.0_f32);
    }
    features
}

fn retrieval_bucket(features: &[f32]) -> usize {
    let mut digest = Sha256::new();
    for (index, feature) in features.iter().enumerate().skip(1) {
        if *feature > 0.25 {
            digest.update((index as u64).to_le_bytes());
            digest.update(feature.to_bits().to_le_bytes());
        }
    }
    usize::from(digest.finalize()[0])
}

fn entry_looks_general(dependencies: &[LeanName]) -> bool {
    dependencies.len() <= 8
}

fn saturating_count(count: usize) -> f32 {
    ((bounded_f32(count) + 1.0).ln() / 5.0_f32.ln()).min(1.0)
}

fn bounded_f32(value: usize) -> f32 {
    f32::from(u16::try_from(value).unwrap_or(u16::MAX))
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut output, byte| {
        write!(output, "{byte:02x}").expect("writing to a String cannot fail");
        output
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn example(index: u8, useful: bool) -> TemporalExample {
        let mut features = vec![0.0; TASTE_FEATURES];
        features[0] = 1.0;
        features[1] = f32::from(index) / 32.0;
        features[16 + usize::from(index % 32)] = 1.0;
        TemporalExample {
            declaration: LeanName::from_dotted(&format!("T{index}")),
            semantic_group: [index; 32],
            features,
            targets: [
                f32::from(useful),
                f32::from(useful),
                f32::from(useful),
                0.0,
                1.0,
                0.1,
                f32::from(!useful),
            ],
        }
    }

    #[test]
    fn full_taste_model_learns_multiple_heads_and_round_trips() {
        let examples = (0..32)
            .map(|index| example(index, index >= 16))
            .collect::<Vec<_>>();
        let model = TasteModel::train(&examples, Treatment::Full);
        let useful = model.forecast(&examples[31].features);
        let dead = model.forecast(&examples[0].features);
        assert!(useful[0].estimate > dead[0].estimate);
        assert!(useful[6].estimate < dead[6].estimate);
        let encoded = model.encode().unwrap();
        let decoded = TasteModel::decode(&encoded).unwrap();
        assert_eq!(model.content_sha256(), decoded.content_sha256());
    }

    #[test]
    fn ranking_retains_protected_uncertain_exploration() {
        let examples = (0..32)
            .map(|index| example(index, index.is_multiple_of(3)))
            .collect::<Vec<_>>();
        let model = TasteModel::train(&examples[..16], Treatment::Full);
        let ranked = model.rank(&examples, 8);
        assert_eq!(ranked.len(), 8);
        assert_eq!(ranked.iter().collect::<HashSet<_>>().len(), 8);
    }
}
