//! Hybrid L1 retrieval shared by `atomic/search`, recall and L1 dedup.

use std::collections::HashMap;
use std::sync::Arc;

use memory_domain::error::DomainResult;
use memory_domain::memory::AtomicMemory;
use memory_domain::retrieval::{normalise_rrf, rrf_fuse, RankedHit, SearchMode, DEFAULT_RRF_K};
use memory_domain::scope::{DepartmentId, MemoryScope};
use tracing::warn;

use crate::common::Scored;
use crate::ports::memory::{AtomFilter, AtomRepository};
use crate::ports::system::EmbeddingProvider;

pub struct AtomSearcher {
    atoms: Arc<dyn AtomRepository>,
    embedder: Arc<dyn EmbeddingProvider>,
}

pub struct AtomSearchOutcome {
    pub hits: Vec<Scored<AtomicMemory>>,
    /// The mode actually used after degradation (e.g. hybrid → bm25).
    pub mode: SearchMode,
}

impl AtomSearcher {
    pub fn new(atoms: Arc<dyn AtomRepository>, embedder: Arc<dyn EmbeddingProvider>) -> Self {
        Self { atoms, embedder }
    }

    pub fn vector_enabled(&self) -> bool {
        self.embedder.dimensions().is_some()
    }

    /// Searches with `mode`, degrading to BM25 when embeddings are off or
    /// fail. Scores are normalised to 0..=1 in every mode.
    pub async fn search(
        &self,
        department: &DepartmentId,
        scope: &MemoryScope,
        query: &str,
        filter: &AtomFilter,
        limit: usize,
        mode: SearchMode,
    ) -> DomainResult<AtomSearchOutcome> {
        let effective = if self.vector_enabled() {
            mode
        } else {
            SearchMode::Bm25
        };
        let candidates = limit * 3;
        let keyword = match effective {
            SearchMode::Embedding => Vec::new(),
            _ => relative_to_best(
                self.atoms
                    .keyword_search(department, scope, query, filter, candidates)
                    .await?,
            ),
        };
        let vector = match effective {
            SearchMode::Bm25 => Vec::new(),
            _ => {
                self.vector_hits(department, scope, query, filter, candidates)
                    .await
            }
        };
        let mode = match (keyword.is_empty(), vector.is_empty(), effective) {
            (_, true, SearchMode::Hybrid) => SearchMode::Bm25,
            (true, false, SearchMode::Hybrid) => SearchMode::Embedding,
            (_, _, m) => m,
        };
        let hits = match mode {
            SearchMode::Bm25 => keyword,
            SearchMode::Embedding => vector,
            SearchMode::Hybrid => fuse(keyword, vector),
        };
        Ok(AtomSearchOutcome {
            hits: hits.into_iter().take(limit).collect(),
            mode,
        })
    }

    async fn vector_hits(
        &self,
        department: &DepartmentId,
        scope: &MemoryScope,
        query: &str,
        filter: &AtomFilter,
        limit: usize,
    ) -> Vec<Scored<AtomicMemory>> {
        let embedded = match self.embedder.embed(&[query.to_owned()]).await {
            Ok(mut v) if !v.is_empty() => v.remove(0),
            Ok(_) => return Vec::new(),
            Err(err) => {
                warn!(error = %err, "query embedding failed; degrading to keyword search");
                return Vec::new();
            }
        };
        self.atoms
            .vector_search(department, scope, &embedded, filter, limit)
            .await
            .unwrap_or_else(|err| {
                warn!(error = %err, "vector search failed");
                Vec::new()
            })
    }
}

/// BM25 magnitudes depend on corpus size (tiny corpora give near-zero
/// IDF), so keyword scores are expressed relative to the best hit.
fn relative_to_best(mut hits: Vec<Scored<AtomicMemory>>) -> Vec<Scored<AtomicMemory>> {
    let best = hits.iter().map(|h| h.score).fold(0.0_f64, f64::max);
    if best > 0.0 {
        hits.iter_mut().for_each(|h| h.score /= best);
    }
    hits
}

fn fuse(
    keyword: Vec<Scored<AtomicMemory>>,
    vector: Vec<Scored<AtomicMemory>>,
) -> Vec<Scored<AtomicMemory>> {
    let as_ranked = |list: &[Scored<AtomicMemory>]| -> Vec<RankedHit> {
        list.iter()
            .map(|s| RankedHit {
                id: s.item.id.clone(),
                score: s.score,
            })
            .collect()
    };
    let fused = rrf_fuse(&[as_ranked(&keyword), as_ranked(&vector)], DEFAULT_RRF_K);
    let mut by_id: HashMap<String, AtomicMemory> = HashMap::new();
    for s in keyword.into_iter().chain(vector) {
        by_id.entry(s.item.id.clone()).or_insert(s.item);
    }
    fused
        .into_iter()
        .filter_map(|hit| {
            by_id.remove(&hit.id).map(|item| Scored {
                item,
                score: normalise_rrf(hit.score, 2, DEFAULT_RRF_K),
            })
        })
        .collect()
}
