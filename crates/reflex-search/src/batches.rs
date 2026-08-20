//! Structure-of-arrays batch pools for the search warm path (P6.2).
//!
//! Columns use the workspace [`AlignedVec`] contract from `reflex-types`: a
//! documented `Vec` wrapper pending an aligned-allocator ADR (no `unsafe`).
//! Buffers are preallocated once and reused across expansions so the hot
//! enumerate → feature → score path does not allocate framework heap objects.

use reflex_domain::{BatchCapacityError, CandidateBatch, CandidateBatchBuilder, FeatureBatch};
use reflex_types::FeatureSchemaId;

/// Named overflow for feature/score buffers on the registered hot path.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SearchBatchPoolError {
    #[error("candidate batch capacity exceeded: limit {limit}, requested {requested}")]
    CandidateCapacity { limit: usize, requested: usize },
    #[error(
        "feature batch capacity exceeded: limit {limit} rows x {cols} cols, requested {rows} x {req_cols}"
    )]
    FeatureCapacity {
        limit: usize,
        cols: usize,
        rows: usize,
        req_cols: usize,
    },
}

impl From<BatchCapacityError> for SearchBatchPoolError {
    fn from(value: BatchCapacityError) -> Self {
        match value {
            BatchCapacityError::CandidateCapacity { limit, requested } => {
                SearchBatchPoolError::CandidateCapacity { limit, requested }
            }
        }
    }
}

/// Reusable SoA buffers for one search kernel (§12.1, P6.2).
pub struct SearchBatchPool {
    candidate_builder: CandidateBatchBuilder,
    candidate_limit: usize,
    stored_batch: Option<CandidateBatch>,
    free_batches: Vec<CandidateBatch>,
    features: FeatureBatch,
    feature_row_limit: usize,
    feature_col_limit: usize,
}

impl SearchBatchPool {
    pub fn new(
        candidate_limit: usize,
        feature_rows: usize,
        feature_cols: usize,
        schema: FeatureSchemaId,
    ) -> Self {
        Self {
            candidate_builder: CandidateBatchBuilder::with_limit(candidate_limit),
            candidate_limit,
            stored_batch: Some(Self::empty_candidate_batch(candidate_limit)),
            free_batches: Vec::new(),
            features: FeatureBatch::with_capacity(feature_rows, feature_cols, schema),
            feature_row_limit: feature_rows,
            feature_col_limit: feature_cols,
        }
    }

    fn empty_candidate_batch(candidate_limit: usize) -> CandidateBatch {
        CandidateBatch {
            group_offsets: Vec::with_capacity(2),
            ids: Vec::with_capacity(candidate_limit),
            classes: Vec::with_capacity(candidate_limit),
            tie_breaks: Vec::with_capacity(candidate_limit),
            payload_handles: Vec::with_capacity(candidate_limit),
            flags: Vec::with_capacity(candidate_limit),
        }
    }

    pub fn candidate_limit(&self) -> usize {
        self.candidate_limit
    }

    pub fn begin_candidates(&mut self) -> Result<&mut CandidateBatchBuilder, SearchBatchPoolError> {
        self.candidate_builder.clear();
        if self.stored_batch.is_none() {
            self.stored_batch = Some(
                self.free_batches
                    .pop()
                    .unwrap_or_else(|| Self::empty_candidate_batch(self.candidate_limit)),
            );
        }
        Ok(&mut self.candidate_builder)
    }

    pub fn finish_candidates(&mut self) -> Result<&CandidateBatch, SearchBatchPoolError> {
        if let Some(error) = self.candidate_builder.capacity_error() {
            return Err(error.into());
        }
        let stored = self
            .stored_batch
            .as_mut()
            .expect("begin_candidates provisions the destination batch");
        self.candidate_builder.build_into(stored);
        Ok(stored)
    }

    /// Transfer the finished batch into the search arena without cloning its
    /// SoA columns. A later expansion provisions a recycled or fresh buffer.
    pub fn take_finished_candidates(&mut self) -> CandidateBatch {
        self.stored_batch
            .take()
            .expect("finish_candidates precedes take_finished_candidates")
    }

    pub fn recycle_candidates(&mut self, mut batch: CandidateBatch) {
        batch.group_offsets.clear();
        batch.ids.clear();
        batch.classes.clear();
        batch.tie_breaks.clear();
        batch.payload_handles.clear();
        batch.flags.clear();
        self.free_batches.push(batch);
    }

    pub fn prepare_features(
        &mut self,
        rows: usize,
        cols: usize,
        schema: FeatureSchemaId,
    ) -> Result<(), SearchBatchPoolError> {
        if rows > self.feature_row_limit || cols > self.feature_col_limit {
            return Err(SearchBatchPoolError::FeatureCapacity {
                limit: self.feature_row_limit,
                cols: self.feature_col_limit,
                rows,
                req_cols: cols,
            });
        }
        self.features.prepare(rows, cols, schema);
        Ok(())
    }

    pub fn features_mut(&mut self) -> &mut FeatureBatch {
        &mut self.features
    }

    pub fn finished_candidates_and_features_mut(&mut self) -> (&CandidateBatch, &mut FeatureBatch) {
        (
            self.stored_batch
                .as_ref()
                .expect("finish_candidates precedes feature extraction"),
            &mut self.features,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reflex_types::Digest;

    #[test]
    fn test_search_batch_pool_reuses_buffers() {
        let schema = FeatureSchemaId::from_digest(Digest::hash_blake3(b"schema"));
        let mut pool = SearchBatchPool::new(4, 4, 8, schema);
        let builder_ptr = pool.begin_candidates().unwrap() as *const CandidateBatchBuilder;
        pool.begin_candidates().unwrap();
        assert_eq!(
            pool.begin_candidates().unwrap() as *const CandidateBatchBuilder,
            builder_ptr
        );

        let batch = pool.finish_candidates().unwrap();
        assert!(batch.is_empty());
        let ids_ptr = batch.ids.as_ptr();
        let owned = pool.take_finished_candidates();
        assert_eq!(owned.ids.as_ptr(), ids_ptr);
        pool.recycle_candidates(owned);
        pool.begin_candidates().unwrap();

        pool.prepare_features(2, 3, schema).unwrap();
        pool.prepare_features(2, 3, schema).unwrap();
        assert_eq!(pool.features_mut().rows, 2);
    }

    #[test]
    fn candidate_limit_is_enforced_during_enumeration() {
        let schema = FeatureSchemaId::from_digest(Digest::hash_blake3(b"bounded-schema"));
        let mut pool = SearchBatchPool::new(2, 2, 1, schema);
        let builder = pool.begin_candidates().unwrap();
        for index in 0..3_u8 {
            builder.add(
                reflex_types::CandidateId::from_digest(Digest::hash_blake3(&[index])),
                0,
                u64::from(index),
                reflex_domain::CandidateHandle(u32::from(index)),
                0,
            );
        }
        assert_eq!(builder.len(), 2);
        assert!(matches!(
            pool.finish_candidates(),
            Err(SearchBatchPoolError::CandidateCapacity {
                limit: 2,
                requested: 3
            })
        ));
    }

    #[test]
    fn test_search_batch_pool_capacity_overflow() {
        let schema = FeatureSchemaId::from_digest(Digest::hash_blake3(b"schema"));
        let mut pool = SearchBatchPool::new(1, 2, 2, schema);
        assert!(matches!(
            pool.prepare_features(3, 2, schema),
            Err(SearchBatchPoolError::FeatureCapacity { .. })
        ));
    }
}
