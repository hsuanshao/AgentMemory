//! Embeddings for atoms stored without one. An atom is stored even when
//! its embedding call fails or times out (it stays findable by keyword);
//! this adds the vector later, without rewriting the atom.

use std::sync::Arc;

use memory_domain::error::DomainResult;
use memory_domain::scope::DepartmentId;
use serde::Serialize;

use crate::ports::memory::AtomRepository;
use crate::ports::system::EmbeddingProvider;

/// Atoms embedded per call to the embedding service.
const BATCH: usize = 32;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct BackfillReport {
    /// Atoms found without an embedding.
    pub missing: usize,
    /// Atoms that got one.
    pub embedded: usize,
    /// Atoms left without one: the embedding service failed, or the atom
    /// changed meanwhile (the next write embeds it anyway).
    pub skipped: usize,
}

impl BackfillReport {
    fn add(&mut self, other: BackfillReport) {
        self.missing += other.missing;
        self.embedded += other.embedded;
        self.skipped += other.skipped;
    }
}

pub struct EmbeddingBackfill {
    atoms: Arc<dyn AtomRepository>,
    embedder: Arc<dyn EmbeddingProvider>,
}

impl EmbeddingBackfill {
    pub fn new(atoms: Arc<dyn AtomRepository>, embedder: Arc<dyn EmbeddingProvider>) -> Self {
        Self { atoms, embedder }
    }

    pub fn enabled(&self) -> bool {
        self.embedder.dimensions().is_some()
    }

    /// Embeds up to `limit` atoms of one department, oldest first. Stops at
    /// the first failed embedding call: the service is down or timing out,
    /// and the rest would fail the same way.
    pub async fn department(
        &self,
        department: &DepartmentId,
        limit: usize,
    ) -> DomainResult<BackfillReport> {
        let mut report = BackfillReport::default();
        if !self.enabled() || limit == 0 {
            return Ok(report);
        }
        let atoms = self.atoms.missing_embeddings(department, limit).await?;
        report.missing = atoms.len();
        for batch in atoms.chunks(BATCH) {
            let texts: Vec<String> = batch.iter().map(|a| a.content.clone()).collect();
            let vectors = match self.embedder.embed(&texts).await {
                Ok(v) if v.len() == batch.len() => v,
                Ok(_) | Err(_) => break,
            };
            for (atom, vector) in batch.iter().zip(&vectors) {
                if self
                    .atoms
                    .set_embedding(department, &atom.id, atom.updated_at, vector)
                    .await?
                {
                    report.embedded += 1;
                }
            }
        }
        report.skipped = report.missing - report.embedded;
        Ok(report)
    }

    /// [`EmbeddingBackfill::department`] for every department.
    pub async fn all(&self, limit_per_department: usize) -> DomainResult<BackfillReport> {
        let mut total = BackfillReport::default();
        if !self.enabled() {
            return Ok(total);
        }
        for department in self.atoms.departments().await? {
            total.add(self.department(&department, limit_per_department).await?);
        }
        Ok(total)
    }
}
