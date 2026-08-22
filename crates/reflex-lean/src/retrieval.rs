use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};
use std::io::Write;

use sha2::{Digest, Sha256};

use crate::ast::{LeanArtifact, LeanBinderInfo, LeanExpr, LeanLiteral};

/// Immutable, symbol-independent premise index for deterministic Operator
/// enumeration. Hash collisions can only alter proposal order; every proposal
/// still passes through the Lean kernel.
pub(crate) struct DonorRetrievalIndex {
    exact: Vec<DigestPosting>,
    identical_proofs: Vec<DigestPosting>,
    postings: Vec<Posting>,
    posting_donors: Vec<usize>,
    proposition_nodes: Vec<usize>,
    document_weights: Vec<u64>,
}

struct DigestPosting {
    digest: [u8; 32],
    donors: Vec<usize>,
}

struct Posting {
    token: u64,
    start: usize,
    len: usize,
    weight: u64,
}

#[derive(Default)]
pub(crate) struct DonorRetrievalScratch {
    cursors: BinaryHeap<PostingCursor>,
    heap: BinaryHeap<RankedDonor>,
    ranked: Vec<RetrievedDonor>,
    selected: Vec<usize>,
    sketch: TokenSketch,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RetrievalTier {
    Exact,
    Structural,
    Fallback,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct RetrievedDonor {
    pub(crate) index: usize,
    pub(crate) tier: RetrievalTier,
    score: u64,
    union: u64,
}

impl RetrievedDonor {
    pub(crate) fn relevance(self) -> f32 {
        self.score
            .saturating_mul(u64::from(u16::MAX))
            .checked_div(self.union)
            .map_or(0.0, |scaled| {
                f32::from(u16::try_from(scaled).unwrap_or(u16::MAX)) / f32::from(u16::MAX)
            })
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
struct RankedDonor {
    index: usize,
    score: u64,
    union: u64,
    node_delta: usize,
}

#[derive(Clone, Copy, Eq, PartialEq)]
struct PostingCursor {
    donor: usize,
    posting: usize,
    offset: usize,
}

impl Ord for PostingCursor {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .donor
            .cmp(&self.donor)
            .then_with(|| other.posting.cmp(&self.posting))
    }
}

impl PartialOrd for PostingCursor {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for RankedDonor {
    fn cmp(&self, other: &Self) -> Ordering {
        ranking_cmp(self, other)
    }
}

impl PartialOrd for RankedDonor {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

const MAX_STRUCTURAL_TOKENS: usize = 256;

#[derive(Default)]
struct TokenSketch {
    heap: BinaryHeap<u64>,
    ordered: Vec<u64>,
}

impl DonorRetrievalIndex {
    pub(crate) fn build(artifacts: &[LeanArtifact]) -> Self {
        let mut postings = HashMap::<u64, Vec<usize>>::new();
        let mut proposition_nodes = Vec::with_capacity(artifacts.len());
        let mut sketch = TokenSketch::default();
        for (index, artifact) in artifacts.iter().enumerate() {
            proposition_nodes.push(artifact.proposition.node_count());
            sketch.write(&artifact.proposition);
            for token in &sketch.ordered {
                postings.entry(*token).or_default().push(index);
            }
        }
        let document_count = artifacts.len();
        postings
            .retain(|_, donors| donors.len().saturating_mul(2) <= document_count.saturating_add(1));
        let mut document_weights = vec![0_u64; document_count];
        let mut sparse_postings = postings.into_iter().collect::<Vec<_>>();
        sparse_postings.sort_unstable_by_key(|(token, _)| *token);
        let mut posting_donors = Vec::new();
        let postings = sparse_postings
            .into_iter()
            .map(|(token, donors)| {
                let weight = inverse_frequency_weight(document_count, donors.len());
                for donor in &donors {
                    document_weights[*donor] = document_weights[*donor].saturating_add(weight);
                }
                let start = posting_donors.len();
                let len = donors.len();
                posting_donors.extend(donors);
                Posting {
                    token,
                    start,
                    len,
                    weight,
                }
            })
            .collect();
        Self {
            exact: digest_postings(artifacts, |artifact| &artifact.proposition),
            identical_proofs: digest_postings(artifacts, |artifact| &artifact.proof_term),
            postings,
            posting_donors,
            proposition_nodes,
            document_weights,
        }
    }

    pub(crate) fn rank_prefix<'a>(
        &self,
        proposition: &LeanExpr,
        proof_term: &LeanExpr,
        limit: usize,
        scratch: &'a mut DonorRetrievalScratch,
    ) -> &'a [RetrievedDonor] {
        let limit = limit.min(self.proposition_nodes.len());
        let proof_digest = expression_digest(proof_term);
        let excluded = digest_donors(&self.identical_proofs, &proof_digest);
        scratch.cursors.clear();
        scratch.heap.clear();
        scratch.ranked.clear();
        scratch.selected.clear();
        scratch.sketch.write(proposition);
        scratch.prepare(limit);
        scratch.sketch.ordered.sort_unstable_by(|left, right| {
            self.posting(*right)
                .map_or(0, |(_, posting)| posting.weight)
                .cmp(&self.posting(*left).map_or(0, |(_, posting)| posting.weight))
                .then_with(|| left.cmp(right))
        });

        self.write_exact(proposition, excluded, limit, scratch);
        let remaining = limit.saturating_sub(scratch.ranked.len());
        if remaining == 0 {
            return &scratch.ranked;
        }

        self.write_structural(proposition, excluded, remaining, scratch);
        self.write_fallback(excluded, limit, scratch);
        &scratch.ranked
    }

    fn write_exact(
        &self,
        proposition: &LeanExpr,
        excluded: &[usize],
        limit: usize,
        scratch: &mut DonorRetrievalScratch,
    ) {
        let digest = expression_digest(proposition);
        let exact = digest_donors(&self.exact, &digest);
        if exact.is_empty() {
            return;
        }
        for index in exact
            .iter()
            .copied()
            .filter(|index| excluded.binary_search(index).is_err())
            .take(limit)
        {
            scratch.selected.push(index);
            scratch.ranked.push(RetrievedDonor {
                index,
                tier: RetrievalTier::Exact,
                score: 1,
                union: 1,
            });
        }
    }

    fn write_structural(
        &self,
        proposition: &LeanExpr,
        excluded: &[usize],
        remaining: usize,
        scratch: &mut DonorRetrievalScratch,
    ) {
        let mut query_weight = 0_u64;
        for token in &scratch.sketch.ordered {
            let Some((posting_index, posting)) = self.posting(*token) else {
                continue;
            };
            query_weight = query_weight.saturating_add(posting.weight);
            if posting.len != 0 {
                scratch.cursors.push(PostingCursor {
                    donor: self.posting_donors[posting.start],
                    posting: posting_index,
                    offset: 0,
                });
            }
        }
        let query_nodes = proposition.node_count();
        while let Some(cursor) = scratch.cursors.pop() {
            let index = cursor.donor;
            let mut score = self.postings[cursor.posting].weight;
            self.advance_cursor(cursor, &mut scratch.cursors);
            while scratch
                .cursors
                .peek()
                .is_some_and(|next| next.donor == index)
            {
                let same_donor = scratch.cursors.pop().expect("the cursor heap was nonempty");
                score = score.saturating_add(self.postings[same_donor.posting].weight);
                self.advance_cursor(same_donor, &mut scratch.cursors);
            }
            if scratch.selected.binary_search(&index).is_ok()
                || excluded.binary_search(&index).is_ok()
            {
                continue;
            }
            let candidate = RankedDonor {
                index,
                score,
                union: query_weight
                    .saturating_add(self.document_weights[index])
                    .saturating_sub(score),
                node_delta: self.proposition_nodes[index].abs_diff(query_nodes),
            };
            if scratch.heap.len() < remaining {
                scratch.heap.push(candidate);
            } else if scratch.heap.peek().is_some_and(|worst| candidate < *worst) {
                scratch.heap.pop();
                scratch.heap.push(candidate);
            }
        }
        let structural_start = scratch.ranked.len();
        scratch
            .ranked
            .extend(scratch.heap.drain().map(|candidate| RetrievedDonor {
                index: candidate.index,
                tier: RetrievalTier::Structural,
                score: candidate.score,
                union: candidate.union,
            }));
        scratch.ranked[structural_start..].sort_unstable_by(|left, right| {
            (u128::from(right.score) * u128::from(left.union))
                .cmp(&(u128::from(left.score) * u128::from(right.union)))
                .then_with(|| {
                    self.proposition_nodes[left.index]
                        .abs_diff(query_nodes)
                        .cmp(&self.proposition_nodes[right.index].abs_diff(query_nodes))
                })
                .then_with(|| left.index.cmp(&right.index))
        });
        scratch.selected.extend(
            scratch.ranked[structural_start..]
                .iter()
                .map(|donor| donor.index),
        );
        scratch.selected.sort_unstable();
        scratch.selected.dedup();
    }

    fn write_fallback(
        &self,
        excluded: &[usize],
        limit: usize,
        scratch: &mut DonorRetrievalScratch,
    ) {
        for index in 0..self.proposition_nodes.len() {
            if scratch.ranked.len() == limit {
                break;
            }
            if excluded.binary_search(&index).is_err()
                && scratch.selected.binary_search(&index).is_err()
            {
                scratch.ranked.push(RetrievedDonor {
                    index,
                    tier: RetrievalTier::Fallback,
                    score: 0,
                    union: 1,
                });
            }
        }
    }

    fn posting(&self, token: u64) -> Option<(usize, &Posting)> {
        self.postings
            .binary_search_by_key(&token, |posting| posting.token)
            .ok()
            .map(|index| (index, &self.postings[index]))
    }

    fn advance_cursor(&self, cursor: PostingCursor, cursors: &mut BinaryHeap<PostingCursor>) {
        let posting = &self.postings[cursor.posting];
        let offset = cursor.offset.saturating_add(1);
        if offset < posting.len {
            cursors.push(PostingCursor {
                donor: self.posting_donors[posting.start + offset],
                posting: cursor.posting,
                offset,
            });
        }
    }

    pub(crate) fn resident_bytes(&self) -> u64 {
        vector_bytes::<DigestPosting>(self.exact.capacity())
            .saturating_add(digest_posting_payload_bytes(&self.exact))
            .saturating_add(vector_bytes::<DigestPosting>(
                self.identical_proofs.capacity(),
            ))
            .saturating_add(digest_posting_payload_bytes(&self.identical_proofs))
            .saturating_add(vector_bytes::<Posting>(self.postings.capacity()))
            .saturating_add(vector_bytes::<usize>(self.posting_donors.capacity()))
            .saturating_add(vector_bytes::<usize>(self.proposition_nodes.capacity()))
            .saturating_add(vector_bytes::<u64>(self.document_weights.capacity()))
    }

    pub(crate) fn scratch_resident_bytes(&self, output_capacity: usize) -> u64 {
        let output_capacity = output_capacity.min(self.proposition_nodes.len());
        let output_capacity = vector_capacity_bound(output_capacity);
        let token_capacity = vector_capacity_bound(MAX_STRUCTURAL_TOKENS);
        binary_heap_bytes::<PostingCursor>(token_capacity)
            .saturating_add(binary_heap_bytes::<RankedDonor>(output_capacity))
            .saturating_add(vector_bytes::<RetrievedDonor>(output_capacity))
            .saturating_add(vector_bytes::<usize>(output_capacity))
            .saturating_add(binary_heap_bytes::<u64>(token_capacity))
            .saturating_add(vector_bytes::<u64>(token_capacity))
    }

    #[cfg(test)]
    fn index_shape(&self) -> (usize, usize, usize) {
        (
            self.proposition_nodes.len(),
            self.postings.len(),
            self.posting_donors.len(),
        )
    }
}

impl DonorRetrievalScratch {
    fn prepare(&mut self, output_capacity: usize) {
        reserve_heap_exact(&mut self.cursors, MAX_STRUCTURAL_TOKENS);
        reserve_heap_exact(&mut self.heap, output_capacity);
        reserve_vec_exact(&mut self.ranked, output_capacity);
        reserve_vec_exact(&mut self.selected, output_capacity);
    }

    #[cfg(test)]
    fn resident_bytes(&self) -> u64 {
        binary_heap_bytes::<PostingCursor>(self.cursors.capacity())
            .saturating_add(binary_heap_bytes::<RankedDonor>(self.heap.capacity()))
            .saturating_add(vector_bytes::<RetrievedDonor>(self.ranked.capacity()))
            .saturating_add(vector_bytes::<usize>(self.selected.capacity()))
            .saturating_add(binary_heap_bytes::<u64>(self.sketch.heap.capacity()))
            .saturating_add(vector_bytes::<u64>(self.sketch.ordered.capacity()))
    }
}

fn digest_postings(
    artifacts: &[LeanArtifact],
    expression: fn(&LeanArtifact) -> &LeanExpr,
) -> Vec<DigestPosting> {
    let mut groups = HashMap::<[u8; 32], Vec<usize>>::new();
    for (index, artifact) in artifacts.iter().enumerate() {
        groups
            .entry(expression_digest(expression(artifact)))
            .or_default()
            .push(index);
    }
    let mut postings = groups
        .into_iter()
        .map(|(digest, donors)| DigestPosting { digest, donors })
        .collect::<Vec<_>>();
    postings.sort_unstable_by_key(|posting| posting.digest);
    postings
}

fn digest_donors<'a>(postings: &'a [DigestPosting], digest: &[u8; 32]) -> &'a [usize] {
    postings
        .binary_search_by_key(digest, |posting| posting.digest)
        .ok()
        .map_or(&[], |index| postings[index].donors.as_slice())
}

fn digest_posting_payload_bytes(postings: &[DigestPosting]) -> u64 {
    postings.iter().fold(0_u64, |bytes, posting| {
        bytes.saturating_add(vector_bytes::<usize>(posting.donors.capacity()))
    })
}

struct DigestWriter<'a>(&'a mut Sha256);

impl Write for DigestWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn expression_digest(expression: &LeanExpr) -> [u8; 32] {
    let mut digest = Sha256::new();
    serde_json::to_writer(DigestWriter(&mut digest), expression)
        .expect("serializing a Lean expression into an infallible digest writer cannot fail");
    digest.finalize().into()
}

fn ranking_cmp(left: &RankedDonor, right: &RankedDonor) -> Ordering {
    (u128::from(right.score) * u128::from(left.union))
        .cmp(&(u128::from(left.score) * u128::from(right.union)))
        .then_with(|| left.node_delta.cmp(&right.node_delta))
        .then_with(|| left.index.cmp(&right.index))
}

impl TokenSketch {
    fn write(&mut self, expression: &LeanExpr) {
        self.heap.clear();
        self.ordered.clear();
        reserve_heap_exact(&mut self.heap, MAX_STRUCTURAL_TOKENS);
        reserve_vec_exact(&mut self.ordered, MAX_STRUCTURAL_TOKENS);
        structural_hash(expression, self);
        self.ordered.extend(self.heap.drain());
        self.ordered.sort_unstable();
    }

    fn retain(&mut self, token: u64) {
        if self.heap.iter().any(|retained| *retained == token) {
            return;
        }
        if self.heap.len() < MAX_STRUCTURAL_TOKENS {
            self.heap.push(token);
        } else if self.heap.peek().is_some_and(|largest| token < *largest) {
            self.heap.pop();
            self.heap.push(token);
        }
    }
}

fn vector_bytes<T>(capacity: usize) -> u64 {
    (capacity as u64).saturating_mul(std::mem::size_of::<T>() as u64)
}

fn binary_heap_bytes<T>(capacity: usize) -> u64 {
    vector_bytes::<T>(capacity)
}

fn vector_capacity_bound(entries: usize) -> usize {
    if entries == 0 {
        0
    } else {
        entries.saturating_mul(2).max(4)
    }
}

fn reserve_heap_exact<T>(values: &mut BinaryHeap<T>, entries: usize) {
    if values.capacity() < entries {
        values.reserve_exact(entries.saturating_sub(values.len()));
    }
}

fn reserve_vec_exact<T>(values: &mut Vec<T>, entries: usize) {
    if values.capacity() < entries {
        values.reserve_exact(entries.saturating_sub(values.len()));
    }
}

fn inverse_frequency_weight(document_count: usize, posting_count: usize) -> u64 {
    u64::try_from(document_count)
        .unwrap_or(u64::MAX)
        .saturating_add(1)
        .saturating_mul(1_024)
        / u64::try_from(posting_count)
            .unwrap_or(u64::MAX)
            .saturating_add(1)
}

fn structural_hash(expression: &LeanExpr, sketch: &mut TokenSketch) -> u64 {
    let (tag, auxiliary) = match expression {
        LeanExpr::Bvar { index } => (1, u64::try_from(*index).unwrap_or(u64::MAX)),
        LeanExpr::Sort { .. } => (2, 0),
        LeanExpr::Const { .. } => (3, 0),
        LeanExpr::App { .. } => (4, 0),
        LeanExpr::Lam { binder_info, .. } => (5, binder_tag(*binder_info)),
        LeanExpr::ForallE { binder_info, .. } => (6, binder_tag(*binder_info)),
        LeanExpr::LetE { non_dep, .. } => (7, u64::from(*non_dep)),
        LeanExpr::Lit {
            literal: LeanLiteral::NatVal { .. },
        } => (8, 0),
        LeanExpr::Lit {
            literal: LeanLiteral::StrVal { .. },
        } => (8, 1),
        LeanExpr::Proj { index, .. } => (9, u64::try_from(*index).unwrap_or(u64::MAX)),
    };
    let mut hash = mix(0xcbf2_9ce4_8422_2325, tag);
    hash = mix(hash, auxiliary);
    expression.for_each_child(|child| hash = mix(hash, structural_hash(child, sketch)));
    sketch.retain(hash);
    hash
}

const fn binder_tag(binder: LeanBinderInfo) -> u64 {
    match binder {
        LeanBinderInfo::Default => 0,
        LeanBinderInfo::Implicit => 1,
        LeanBinderInfo::StrictImplicit => 2,
        LeanBinderInfo::InstImplicit => 3,
    }
}

const fn mix(hash: u64, value: u64) -> u64 {
    (hash ^ value).wrapping_mul(0x0000_0100_0000_01b3)
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::hint::black_box;
    use std::sync::Arc;
    use std::time::Instant;

    use super::{DonorRetrievalIndex, DonorRetrievalScratch, TokenSketch};
    use crate::ast::{
        LeanArtifact, LeanDeclarationIdentity, LeanEnvironmentIdentity, LeanExpr, LeanName,
    };

    #[test]
    fn structural_tokens_ignore_constant_names_but_retain_constructor_shape() {
        let left = LeanExpr::constant(LeanName::from_dotted("Nat"), vec![]);
        let right = LeanExpr::constant(LeanName::from_dotted("Bool"), vec![]);
        let application = LeanExpr::App {
            function: std::sync::Arc::new(left.clone()),
            argument: std::sync::Arc::new(right),
        };
        let mut left_sketch = TokenSketch::default();
        let mut application_sketch = TokenSketch::default();

        left_sketch.write(&left);
        application_sketch.write(&application);

        assert_eq!(left_sketch.ordered.len(), 1);
        assert!(application_sketch.ordered.contains(&left_sketch.ordered[0]));
        assert_eq!(application_sketch.ordered.len(), 2);
    }

    #[test]
    fn bounded_prefix_is_deterministic_unique_and_uses_corpus_independent_scratch() {
        let small_artifacts = (0..128)
            .map(|index| benchmark_artifact(index, 1 + index % 64))
            .collect::<Vec<_>>();
        let large_artifacts = (0..4_096)
            .map(|index| benchmark_artifact(index, 1 + index % 64))
            .collect::<Vec<_>>();
        let small_index = DonorRetrievalIndex::build(&small_artifacts);
        let large_index = DonorRetrievalIndex::build(&large_artifacts);
        let query = benchmark_artifact(10_000, 31);
        let mut scratch = DonorRetrievalScratch::default();

        let first = large_index
            .rank_prefix(&query.proposition, &query.proof_term, 8, &mut scratch)
            .iter()
            .map(|donor| donor.index)
            .collect::<Vec<_>>();
        let actual_scratch_bytes = scratch.resident_bytes();
        let declared_scratch_bytes = large_index.scratch_resident_bytes(8);
        let second = large_index
            .rank_prefix(&query.proposition, &query.proof_term, 8, &mut scratch)
            .iter()
            .map(|donor| donor.index)
            .collect::<Vec<_>>();

        assert_eq!(first, second);
        assert_eq!(first.len(), 8);
        assert_eq!(first.iter().copied().collect::<HashSet<_>>().len(), 8);
        assert!(actual_scratch_bytes <= declared_scratch_bytes);
        assert_eq!(
            small_index.scratch_resident_bytes(8),
            declared_scratch_bytes,
            "scratch accounting must depend on the bounded output, not corpus size"
        );
    }

    #[test]
    fn retrieval_never_returns_the_sources_identical_proof() {
        let artifacts = (0..8)
            .map(|index| benchmark_artifact(index, index + 1))
            .collect::<Vec<_>>();
        let index = DonorRetrievalIndex::build(&artifacts);
        let mut scratch = DonorRetrievalScratch::default();

        let ranked = index.rank_prefix(
            &artifacts[3].proposition,
            &artifacts[3].proof_term,
            artifacts.len(),
            &mut scratch,
        );

        assert_eq!(ranked.len(), artifacts.len() - 1);
        assert!(ranked.iter().all(|donor| donor.index != 3));
    }

    #[test]
    fn structural_top_k_scores_late_donors_before_applying_corpus_ties() {
        let constant = |name| LeanExpr::constant(LeanName::from_dotted(name), vec![]);
        let query_proposition = LeanExpr::App {
            function: Arc::new(constant("Query.Left")),
            argument: Arc::new(constant("Query.Right")),
        };
        let worse_early = LeanExpr::App {
            function: Arc::new(LeanExpr::App {
                function: Arc::new(constant("Early.Left")),
                argument: Arc::new(constant("Early.Right")),
            }),
            argument: Arc::new(constant("Early.Extra")),
        };
        let best_late = LeanExpr::App {
            function: Arc::new(constant("Late.Left")),
            argument: Arc::new(constant("Late.Right")),
        };
        let mut artifacts = vec![benchmark_artifact(0, 1), benchmark_artifact(1, 1)];
        artifacts[0].proposition = worse_early;
        artifacts[1].proposition = best_late;
        artifacts.extend((2..8).map(|index| {
            let mut artifact = benchmark_artifact(index, 1);
            artifact.proposition = LeanExpr::Bvar { index };
            artifact
        }));
        let index = DonorRetrievalIndex::build(&artifacts);
        let mut scratch = DonorRetrievalScratch::default();
        let query_proof = constant("Query.proof");

        let ranked = index.rank_prefix(&query_proposition, &query_proof, 1, &mut scratch);

        assert_eq!(ranked[0].index, 1);
    }

    #[test]
    #[ignore = "release-mode development microbenchmark"]
    fn indexed_retrieval_is_four_times_faster_than_rescanning_donor_structure() {
        const DONORS: usize = 4_096;
        const QUERIES: usize = 64;
        let artifacts = (0..DONORS)
            .map(|index| benchmark_artifact(index, 1 + index % 64))
            .collect::<Vec<_>>();
        let build_started = Instant::now();
        let index = DonorRetrievalIndex::build(&artifacts);
        let build_elapsed = build_started.elapsed();
        let mut scratch = DonorRetrievalScratch::default();
        let queries = (0..QUERIES)
            .map(|query| {
                let artifact = &artifacts[query * (DONORS / QUERIES)];
                (&artifact.proposition, &artifact.proof_term)
            })
            .collect::<Vec<_>>();

        let indexed_started = Instant::now();
        for (query, proof) in &queries {
            let ranked = index.rank_prefix(black_box(query), proof, 16, &mut scratch);
            assert_eq!(ranked.len(), 16);
            black_box(ranked.first());
        }
        let indexed_elapsed = indexed_started.elapsed();
        let direct_started = Instant::now();
        let mut query_sketch = TokenSketch::default();
        let mut donor_sketch = TokenSketch::default();
        let mut direct_scores = Vec::with_capacity(DONORS);
        for (query, _) in &queries {
            query_sketch.write(black_box(query));
            direct_scores.clear();
            for (donor, artifact) in artifacts.iter().enumerate() {
                donor_sketch.write(&artifact.proposition);
                let overlap = query_sketch
                    .ordered
                    .iter()
                    .filter(|token| donor_sketch.ordered.binary_search(token).is_ok())
                    .count();
                direct_scores.push((std::cmp::Reverse(overlap), donor));
            }
            direct_scores.sort_unstable();
            black_box(direct_scores.first());
        }
        let direct_elapsed = direct_started.elapsed();
        let (documents, postings, posting_entries) = index.index_shape();
        let index_bytes = index.resident_bytes();
        let scratch_bytes = index.scratch_resident_bytes(16);
        eprintln!(
            "retrieval build={build_elapsed:?} indexed={indexed_elapsed:?} direct={direct_elapsed:?} documents={documents} postings={postings} posting_entries={posting_entries} index_bytes={index_bytes} scratch_bytes={scratch_bytes}"
        );
        assert!(
            indexed_elapsed.saturating_mul(4) < direct_elapsed,
            "the in-memory index must handily beat recomputing every donor sketch"
        );
    }

    fn benchmark_artifact(index: usize, nodes: usize) -> LeanArtifact {
        let mut proposition = LeanExpr::constant(
            LeanName::from_dotted(&format!("Reflex.Type{index}")),
            vec![],
        );
        for depth in 1..nodes {
            proposition = LeanExpr::App {
                function: Arc::new(LeanExpr::constant(
                    LeanName::from_dotted(&format!("Reflex.Function{}", depth % 7)),
                    vec![],
                )),
                argument: Arc::new(proposition),
            };
        }
        LeanArtifact {
            environment: LeanEnvironmentIdentity {
                mathlib_commit: "benchmark".into(),
                lean_toolchain: "benchmark".into(),
                lean_commit: "benchmark".into(),
                artifact_format: 0,
                kernel_contract: 0,
                worker_source_sha256: "benchmark".into(),
            },
            declaration: LeanDeclarationIdentity {
                name: LeanName::from_dotted(&format!("Reflex.donor{index}")),
                level_params: vec![],
            },
            proposition,
            proof_term: LeanExpr::constant(
                LeanName::from_dotted(&format!("Reflex.proof{index}")),
                vec![],
            ),
            dependencies: vec![],
            allowed_axioms: vec![],
        }
    }
}
