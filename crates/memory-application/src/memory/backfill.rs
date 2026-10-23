//! Embeddings for atoms stored without one. An atom is stored even when
//! its embedding call fails or times out (it stays findable by keyword);
//! this adds the vector later, without rewriting the atom.
//!
//! Each department is walked in seq order from a cursor, so an atom the
//! service always refuses (too long for the model, say) is skipped and the
//! ones after it still get their vector; the walk starts over once it
//! reaches the end, retrying what was skipped.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use memory_domain::error::DomainResult;
use memory_domain::memory::AtomicMemory;
use memory_domain::scope::DepartmentId;
use serde::Serialize;
use tracing::warn;

use crate::ports::memory::{AtomRepository, Sequenced};
use crate::ports::system::EmbeddingProvider;

/// Atoms embedded per call to the embedding service.
const BATCH: usize = 32;

/// Single atoms in a row the service refuses before it counts as down.
const DOWN_AFTER: usize = 3;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct BackfillReport {
    /// Atoms without an embedding this run looked at.
    pub missing: usize,
    /// Atoms that got one.
    pub embedded: usize,
    /// Atoms left without one: the service refused them, or they changed
    /// meanwhile (their next write embeds them anyway).
    pub skipped: usize,
    /// The run reached the end of the department's queue.
    pub complete: bool,
    /// The run stopped because the embedding service kept failing.
    pub service_down: bool,
}

impl BackfillReport {
    fn add(&mut self, other: BackfillReport) {
        self.missing += other.missing;
        self.embedded += other.embedded;
        self.skipped += other.skipped;
        self.service_down |= other.service_down;
    }
}

pub struct EmbeddingBackfill {
    atoms: Arc<dyn AtomRepository>,
    embedder: Arc<dyn EmbeddingProvider>,
    /// Per department, the seq the next run starts after.
    cursors: Mutex<HashMap<DepartmentId, i64>>,
}

impl EmbeddingBackfill {
    pub fn new(atoms: Arc<dyn AtomRepository>, embedder: Arc<dyn EmbeddingProvider>) -> Self {
        Self {
            atoms,
            embedder,
            cursors: Mutex::new(HashMap::new()),
        }
    }

    pub fn enabled(&self) -> bool {
        self.embedder.dimensions().is_some()
    }

    fn cursor(&self, department: &DepartmentId) -> Option<i64> {
        self.cursors.lock().ok()?.get(department).copied()
    }

    fn set_cursor(&self, department: &DepartmentId, seq: Option<i64>) {
        if let Ok(mut cursors) = self.cursors.lock() {
            match seq {
                Some(seq) => cursors.insert(department.clone(), seq),
                None => cursors.remove(department),
            };
        }
    }

    /// Embeds up to `limit` atoms of one department, continuing where the
    /// last run stopped. A failed batch is retried one atom at a time; an
    /// atom refused on its own is skipped. When `DOWN_AFTER` atoms in a row
    /// are refused, the service is taken as down: the run stops and the
    /// next one starts again from those atoms.
    pub async fn department(
        &self,
        department: &DepartmentId,
        limit: usize,
    ) -> DomainResult<BackfillReport> {
        let mut report = BackfillReport::default();
        if !self.enabled() || limit == 0 {
            return Ok(report);
        }
        let atoms = self
            .atoms
            .missing_embeddings(department, self.cursor(department), limit)
            .await?;
        report.missing = atoms.len();
        // The seq up to which every atom was embedded or skipped.
        let mut done: Option<i64> = self.cursor(department);
        let mut refused_in_a_row = 0;
        'batches: for batch in atoms.chunks(BATCH) {
            if let Some(vectors) = self.embed(batch).await {
                for (atom, vector) in batch.iter().zip(&vectors) {
                    self.store(department, &atom.item, vector, &mut report)
                        .await?;
                    done = Some(atom.seq);
                }
                refused_in_a_row = 0;
                continue;
            }
            for atom in batch {
                match self.embed(std::slice::from_ref(atom)).await {
                    Some(vectors) => {
                        self.store(department, &atom.item, &vectors[0], &mut report)
                            .await?;
                        refused_in_a_row = 0;
                    }
                    None => {
                        refused_in_a_row += 1;
                        if refused_in_a_row == DOWN_AFTER {
                            report.service_down = true;
                            break 'batches;
                        }
                    }
                }
                done = Some(atom.seq);
            }
        }
        if report.service_down {
            // Resume before the atoms refused in a row; skipped ones before
            // them wait for the next pass.
            let resume = atoms
                .iter()
                .take_while(|a| Some(a.seq) <= done)
                .count()
                .saturating_sub(refused_in_a_row - 1);
            done = resume
                .checked_sub(1)
                .map(|i| atoms[i].seq)
                .or(self.cursor(department));
        }
        report.complete = !report.service_down && atoms.len() < limit;
        // At the end of the queue, start over next time.
        self.set_cursor(department, if report.complete { None } else { done });
        report.skipped = report.missing - report.embedded;
        Ok(report)
    }

    async fn embed(&self, atoms: &[Sequenced<AtomicMemory>]) -> Option<Vec<Vec<f32>>> {
        let texts: Vec<String> = atoms.iter().map(|a| a.item.content.clone()).collect();
        match self.embedder.embed(&texts).await {
            Ok(v) if v.len() == atoms.len() => Some(v),
            _ => None,
        }
    }

    async fn store(
        &self,
        department: &DepartmentId,
        atom: &AtomicMemory,
        vector: &[f32],
        report: &mut BackfillReport,
    ) -> DomainResult<()> {
        if self
            .atoms
            .set_embedding(department, &atom.id, atom.updated_at, vector)
            .await?
        {
            report.embedded += 1;
        }
        Ok(())
    }

    /// [`EmbeddingBackfill::department`] for every department. A department
    /// whose storage fails is logged and passed over.
    pub async fn all(&self, limit_per_department: usize) -> DomainResult<BackfillReport> {
        let mut total = BackfillReport::default();
        if !self.enabled() {
            return Ok(total);
        }
        for department in self.atoms.departments().await? {
            match self.department(&department, limit_per_department).await {
                Ok(report) => total.add(report),
                Err(err) => warn!(%department, error = %err, "embedding backfill failed"),
            }
        }
        Ok(total)
    }
}
