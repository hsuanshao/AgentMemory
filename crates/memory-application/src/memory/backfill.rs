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

use memory_domain::error::{DomainResult, ErrorKind};
use memory_domain::memory::AtomicMemory;
use memory_domain::scope::DepartmentId;
use serde::Serialize;
use tracing::warn;

use crate::ports::memory::{AtomRepository, Sequenced};
use crate::ports::system::EmbeddingProvider;

/// Atoms embedded per call to the embedding service.
const BATCH: usize = 32;

/// Single atoms in a row the service fails on (unavailable, not refusing
/// the input) before it counts as down.
const DOWN_AFTER: usize = 3;

/// Atoms in a row whose input the service refuses before the settings, not
/// the atoms, are taken as wrong (a misspelt model, `dimensions` the model
/// does not support): a batch's worth.
const MISCONFIGURED_AFTER: usize = BATCH;

/// Why an embedding call did not give vectors.
enum Failure {
    /// The service refused this input (HTTP 400/413/422): skip it.
    Refused,
    /// The service is unreachable, timing out or failing.
    Unavailable,
}

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
    /// The run stopped because the service refused every input: its
    /// settings (model, dimensions) are likely wrong.
    pub misconfigured: bool,
}

impl BackfillReport {
    fn add(&mut self, other: BackfillReport) {
        self.missing += other.missing;
        self.embedded += other.embedded;
        self.skipped += other.skipped;
        self.service_down |= other.service_down;
        self.misconfigured |= other.misconfigured;
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
    /// atom whose input the service refuses is skipped, however many there
    /// are in a row, up to `MISCONFIGURED_AFTER`: then the settings are taken
    /// as wrong. When the service fails `DOWN_AFTER` atoms in a row for its
    /// own reasons (unreachable, timeouts, 5xx), it is taken as down. Either
    /// way the run stops, and the next one starts again from those atoms.
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
        // The current streaks of refused and failed atoms, by the index of
        // their first atom and their length.
        let (mut refused, mut failed) = ((0, 0), (0, 0));
        let mut stop_at = None;
        'batches: for (b, batch) in atoms.chunks(BATCH).enumerate() {
            if let Ok(vectors) = self.embed(batch).await {
                for (atom, vector) in batch.iter().zip(&vectors) {
                    self.store(department, &atom.item, vector, &mut report)
                        .await?;
                    done = Some(atom.seq);
                }
                (refused, failed) = ((0, 0), (0, 0));
                continue;
            }
            for (j, atom) in batch.iter().enumerate() {
                let index = b * BATCH + j;
                match self.embed(std::slice::from_ref(atom)).await {
                    Ok(vectors) => {
                        self.store(department, &atom.item, &vectors[0], &mut report)
                            .await?;
                        (refused, failed) = ((0, 0), (0, 0));
                    }
                    // The service answered: skip this one.
                    Err(Failure::Refused) => {
                        failed = (0, 0);
                        refused = streak(refused, index);
                        if refused.1 == MISCONFIGURED_AFTER {
                            report.misconfigured = true;
                            stop_at = Some(refused.0);
                            break 'batches;
                        }
                    }
                    Err(Failure::Unavailable) => {
                        refused = (0, 0);
                        failed = streak(failed, index);
                        if failed.1 == DOWN_AFTER {
                            report.service_down = true;
                            stop_at = Some(failed.0);
                            break 'batches;
                        }
                    }
                }
                done = Some(atom.seq);
            }
        }
        if report.misconfigured {
            warn!(
                %department,
                "embedding service refuses every input; check memory.embedding model and dimensions"
            );
        }
        if let Some(first) = stop_at {
            // Resume at the first atom of the streak; atoms skipped before
            // it wait for the next pass.
            done = first
                .checked_sub(1)
                .map(|i| atoms[i].seq)
                .or(self.cursor(department));
        }
        report.complete = stop_at.is_none() && atoms.len() < limit;
        // At the end of the queue, start over next time.
        self.set_cursor(department, if report.complete { None } else { done });
        report.skipped = report.missing - report.embedded;
        Ok(report)
    }

    async fn embed(&self, atoms: &[Sequenced<AtomicMemory>]) -> Result<Vec<Vec<f32>>, Failure> {
        let texts: Vec<String> = atoms.iter().map(|a| a.item.content.clone()).collect();
        match self.embedder.embed(&texts).await {
            Ok(v) if v.len() == atoms.len() => Ok(v),
            Ok(_) => Err(Failure::Unavailable),
            Err(e) if e.kind == ErrorKind::InvalidInput => Err(Failure::Refused),
            Err(_) => Err(Failure::Unavailable),
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

/// A streak `(first index, length)` extended by the atom at `index`.
fn streak((first, len): (usize, usize), index: usize) -> (usize, usize) {
    if len == 0 {
        (index, 1)
    } else {
        (first, len + 1)
    }
}
