//! Pipeline job queue, worker pool and timer scanner.
//!
//! The worker is backend-neutral: queue storage is a [`QueueBackend`]
//! (in-process here, Redis for multi-node) and per-job exclusivity is a
//! [`KeyLock`] (local or distributed).

use std::collections::{HashSet, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use memory_application::admin::DepartmentDirectory;
use memory_application::audit::CredentialGuard;
use memory_application::memory::backfill::EmbeddingBackfill;
use memory_application::pipeline::service::{JobFailure, PipelineService};
use memory_application::ports::coordination::KeyLock;
use memory_application::ports::memory::AtomRepository;
use memory_application::ports::pipeline::{
    failure_is_deterministic, DeadLetter, DeadLetterAction, DeadLetterFilter, DeadLetterPolicy,
    JobQueue, PipelineJob, QueueStats,
};
use memory_application::skill::extraction::SkillExtractionService;
use memory_domain::error::{DomainError, DomainResult};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, Semaphore};
use tokio::task::JoinHandle;
use tracing::{debug, error, info, warn};

const POP_WAIT: Duration = Duration::from_secs(1);
/// Dead letters the in-process backend keeps (newest win).
const LOCAL_DEAD_LETTER_CAP: usize = 1_000;

#[derive(Debug, Clone, Copy)]
pub struct WorkerSettings {
    pub concurrency: usize,
    pub max_retries: u32,
    pub base_backoff: Duration,
    pub busy_requeue_delay: Duration,
    pub scan_interval: Duration,
    pub dead_letters: DeadLetterPolicy,
    /// How often the dead-letter sweeper runs.
    pub sweep_interval: Duration,
    /// Dead letters the sweeper requeues per run, at most.
    pub redrive_batch: usize,
}

impl Default for WorkerSettings {
    fn default() -> Self {
        Self {
            concurrency: 8,
            max_retries: 3,
            base_backoff: Duration::from_secs(5),
            busy_requeue_delay: Duration::from_millis(200),
            scan_interval: Duration::from_secs(2),
            dead_letters: DeadLetterPolicy {
                ttl_ms: 24 * 3_600_000,
                max_redrives: 3,
                redrive_after_ms: 3_600_000,
            },
            sweep_interval: Duration::from_secs(300),
            redrive_batch: 10,
        }
    }
}

/// A queued job and how many times it has failed. A job requeued from the
/// dead letters carries how often that happened and when it first died, so
/// the next dead letter keeps counting.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Envelope {
    pub job: PipelineJob,
    pub attempt: u32,
    #[serde(default)]
    pub redrives: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_dead_at_ms: Option<i64>,
}

impl Envelope {
    /// A job's first attempt.
    pub fn new(job: PipelineJob) -> Self {
        Self {
            job,
            attempt: 0,
            redrives: 0,
            first_dead_at_ms: None,
        }
    }

    /// A dead letter, requeued as a first attempt.
    pub fn redrive(letter: DeadLetter) -> Self {
        Self {
            first_dead_at_ms: Some(letter.first_dead_at_ms.unwrap_or(letter.dead_at_ms)),
            redrives: letter.redrives + 1,
            ..Self::new(letter.job)
        }
    }
}

/// A popped job plus the receipt that acknowledges it. Until acked, a
/// distributed backend may redeliver the job (at-least-once delivery).
#[derive(Debug, Clone, PartialEq)]
pub struct Delivery {
    pub envelope: Envelope,
    /// Opaque to the worker; empty for backends without redelivery.
    pub receipt: String,
}

/// Identical waiting jobs share this key and are coalesced.
pub fn dedupe_key(job: &PipelineJob) -> String {
    match job.range() {
        // A redo is never the same job as the cursor run of its session.
        Some(r) => format!("{}:{}:{}-{}", job.kind(), job.serial_key(), r.first, r.last),
        None => format!("{}:{}", job.kind(), job.serial_key()),
    }
}

/// Where queued jobs live.
#[async_trait]
pub trait QueueBackend: Send + Sync {
    /// Adds a new job unless an identical one is still waiting.
    async fn push_new(&self, job: PipelineJob) -> DomainResult<()>;
    /// Re-adds a job once `delay` has passed (retries, busy keys).
    async fn push_later(&self, envelope: Envelope, delay: Duration) -> DomainResult<()>;
    /// The next ready job, waiting at most `wait`. Popping a first attempt
    /// ends its coalescing window.
    async fn pop(&self, wait: Duration) -> DomainResult<Option<Delivery>>;
    /// The delivery's outcome is recorded (done, requeued or dead-lettered);
    /// it must not be redelivered.
    async fn ack(&self, receipt: &str) -> DomainResult<()>;
    /// The job is still running: push back its redelivery deadline.
    async fn extend(&self, receipt: &str) -> DomainResult<()>;
    /// How often a running job must call `extend`; `None` if never.
    fn extend_interval(&self) -> Option<Duration> {
        None
    }
    async fn dead_letter(&self, letter: DeadLetter) -> DomainResult<()>;
    /// Stored dead letters, oldest first.
    async fn dead_letters(&self) -> DomainResult<Vec<DeadLetter>>;
    /// Removes and returns up to `limit` dead letters `select` accepts,
    /// oldest first. A letter is returned by at most one caller, even with
    /// several nodes taking at once.
    async fn take_dead_letters(
        &self,
        select: &(dyn for<'a> Fn(&'a DeadLetter) -> bool + Sync),
        limit: usize,
    ) -> DomainResult<Vec<DeadLetter>>;
    /// `(pending, dead_lettered)`.
    async fn counts(&self) -> DomainResult<(usize, usize)>;
}

/// The application's `JobQueue`, over any backend.
pub struct WorkerQueue {
    backend: Arc<dyn QueueBackend>,
    running: AtomicUsize,
}

impl WorkerQueue {
    pub fn new(backend: Arc<dyn QueueBackend>) -> Arc<Self> {
        Arc::new(Self {
            backend,
            running: AtomicUsize::new(0),
        })
    }

    /// Queues taken dead letters again as first attempts, counting the
    /// redrive. When queueing fails, every letter not yet queued (the
    /// failing one included) goes back to the dead letters, so none is lost.
    async fn requeue_letters(&self, letters: Vec<DeadLetter>) -> DomainResult<usize> {
        let mut letters = letters.into_iter();
        let mut requeued = 0;
        while let Some(letter) = letters.next() {
            if let Err(err) = self
                .backend
                .push_later(Envelope::redrive(letter.clone()), Duration::ZERO)
                .await
            {
                // Keep them rather than lose them; the next redrive tries again.
                let rest: Vec<_> = std::iter::once(letter).chain(letters).collect();
                warn!(error = %err, kept = rest.len(), "redrive failed; keeping the dead letters");
                for letter in rest {
                    self.backend.dead_letter(letter).await?;
                }
                return Err(err);
            }
            requeued += 1;
        }
        Ok(requeued)
    }

    /// One sweep: expires dead letters `policy` is done with, then requeues
    /// up to `batch` that are due. Returns `(redriven, expired)`.
    pub async fn sweep_dead_letters(
        &self,
        policy: &DeadLetterPolicy,
        model: &str,
        batch: usize,
    ) -> DomainResult<(usize, usize)> {
        let now = Utc::now().timestamp_millis();
        let decide = |l: &DeadLetter| policy.decide(l, now, model);
        let expired = self
            .backend
            .take_dead_letters(&|l| decide(l) == DeadLetterAction::Expire, usize::MAX)
            .await?;
        for l in &expired {
            info!(
                kind = l.job.kind(),
                redrives = l.redrives,
                error = l.error.as_deref().unwrap_or("-"),
                "dead letter expired"
            );
        }
        let due = self
            .backend
            .take_dead_letters(&|l| decide(l) == DeadLetterAction::Redrive, batch)
            .await?;
        let redriven = self.requeue_letters(due).await?;
        if redriven > 0 {
            info!(redriven, "dead letters redriven by the sweeper");
        }
        Ok((redriven, expired.len()))
    }
}

#[async_trait]
impl JobQueue for WorkerQueue {
    async fn enqueue(&self, job: PipelineJob) -> DomainResult<()> {
        self.backend.push_new(job).await
    }

    async fn stats(&self) -> DomainResult<QueueStats> {
        let (pending, dead_letter) = self.backend.counts().await?;
        Ok(QueueStats {
            pending,
            running: self.running.load(Ordering::Relaxed),
            dead_letter,
        })
    }

    async fn dead_letters(
        &self,
        filter: &DeadLetterFilter,
        limit: usize,
    ) -> DomainResult<Vec<DeadLetter>> {
        let mut letters = self.backend.dead_letters().await?;
        letters.retain(|l| filter.matches(l));
        letters.reverse();
        letters.truncate(limit);
        Ok(letters)
    }

    async fn redrive(&self, filter: &DeadLetterFilter, limit: usize) -> DomainResult<usize> {
        let taken = self
            .backend
            .take_dead_letters(&|l| filter.matches(l), limit)
            .await?;
        self.requeue_letters(taken).await
    }
}

/// Single-node backend: an in-memory channel.
pub struct LocalQueue {
    tx: mpsc::UnboundedSender<Envelope>,
    rx: tokio::sync::Mutex<mpsc::UnboundedReceiver<Envelope>>,
    queued: Mutex<HashSet<String>>,
    pending: Arc<AtomicUsize>,
    /// Lost on restart, like everything else in this backend.
    dead: Mutex<VecDeque<DeadLetter>>,
}

impl LocalQueue {
    pub fn new() -> Arc<Self> {
        let (tx, rx) = mpsc::unbounded_channel();
        Arc::new(Self {
            tx,
            rx: tokio::sync::Mutex::new(rx),
            queued: Mutex::new(HashSet::new()),
            pending: Arc::new(AtomicUsize::new(0)),
            dead: Mutex::new(VecDeque::new()),
        })
    }

    fn send(tx: &mpsc::UnboundedSender<Envelope>, envelope: Envelope) -> DomainResult<()> {
        tx.send(envelope)
            .map_err(|_| DomainError::unavailable("pipeline worker stopped"))
    }
}

#[async_trait]
impl QueueBackend for LocalQueue {
    async fn push_new(&self, job: PipelineJob) -> DomainResult<()> {
        if !self
            .queued
            .lock()
            .expect("queue lock")
            .insert(dedupe_key(&job))
        {
            debug!(kind = job.kind(), "job already queued; coalescing");
            return Ok(());
        }
        self.pending.fetch_add(1, Ordering::Relaxed);
        Self::send(&self.tx, Envelope::new(job))
    }

    async fn push_later(&self, envelope: Envelope, delay: Duration) -> DomainResult<()> {
        self.pending.fetch_add(1, Ordering::Relaxed);
        let (tx, pending) = (self.tx.clone(), Arc::clone(&self.pending));
        tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            if Self::send(&tx, envelope).is_err() {
                pending.fetch_sub(1, Ordering::Relaxed);
                warn!("dropping job: worker stopped");
            }
        });
        Ok(())
    }

    async fn pop(&self, wait: Duration) -> DomainResult<Option<Delivery>> {
        let mut rx = self.rx.lock().await;
        let Ok(Some(envelope)) = tokio::time::timeout(wait, rx.recv()).await else {
            return Ok(None);
        };
        self.pending.fetch_sub(1, Ordering::Relaxed);
        if envelope.attempt == 0 {
            self.queued
                .lock()
                .expect("queue lock")
                .remove(&dedupe_key(&envelope.job));
        }
        Ok(Some(Delivery {
            envelope,
            receipt: String::new(),
        }))
    }

    /// In-process jobs die with the process, so there is nothing to redeliver.
    async fn ack(&self, _receipt: &str) -> DomainResult<()> {
        Ok(())
    }

    async fn extend(&self, _receipt: &str) -> DomainResult<()> {
        Ok(())
    }

    async fn dead_letter(&self, letter: DeadLetter) -> DomainResult<()> {
        let mut dead = self.dead.lock().expect("dead letter lock");
        dead.push_back(letter);
        while dead.len() > LOCAL_DEAD_LETTER_CAP {
            dead.pop_front();
        }
        Ok(())
    }

    async fn dead_letters(&self) -> DomainResult<Vec<DeadLetter>> {
        Ok(self
            .dead
            .lock()
            .expect("dead letter lock")
            .iter()
            .cloned()
            .collect())
    }

    async fn take_dead_letters(
        &self,
        select: &(dyn for<'a> Fn(&'a DeadLetter) -> bool + Sync),
        limit: usize,
    ) -> DomainResult<Vec<DeadLetter>> {
        let mut dead = self.dead.lock().expect("dead letter lock");
        let mut taken = Vec::new();
        let mut kept = VecDeque::with_capacity(dead.len());
        for letter in dead.drain(..) {
            if taken.len() < limit && select(&letter) {
                taken.push(letter);
            } else {
                kept.push_back(letter);
            }
        }
        *dead = kept;
        Ok(taken)
    }

    async fn counts(&self) -> DomainResult<(usize, usize)> {
        Ok((
            self.pending.load(Ordering::Relaxed),
            self.dead.lock().expect("dead letter lock").len(),
        ))
    }
}

/// Spawns the dispatcher. Jobs sharing a serial key never run concurrently
/// (across nodes when `locks` is distributed). `locks` must not be the
/// department the pipeline uses for its state documents: a job holds its
/// serial key while the pipeline takes state locks inside it. A job whose
/// department no longer exists (destroyed after it was queued) is dropped.
pub fn spawn_worker(
    queue: Arc<WorkerQueue>,
    locks: Arc<dyn KeyLock>,
    pipeline: Arc<PipelineService>,
    departments: Arc<dyn DepartmentDirectory>,
    settings: WorkerSettings,
) -> JoinHandle<()> {
    let permits = Arc::new(Semaphore::new(settings.concurrency.max(1)));
    tokio::spawn(async move {
        loop {
            let Ok(permit) = Arc::clone(&permits).acquire_owned().await else {
                break;
            };
            let Delivery { envelope, receipt } = match queue.backend.pop(POP_WAIT).await {
                Ok(Some(delivery)) => delivery,
                Ok(None) => continue,
                Err(err) => {
                    warn!(error = %err, "queue pop failed");
                    tokio::time::sleep(POP_WAIT).await;
                    continue;
                }
            };
            // Asked per job: it also clears what this node still remembers
            // of a department another node destroyed.
            match departments.exists(envelope.job.department()).await {
                Ok(true) => {}
                Ok(false) => {
                    drop_for_unknown_department(&envelope);
                    ack(&queue, &receipt).await;
                    continue;
                }
                Err(err) => {
                    warn!(error = %err, "department check failed; requeueing");
                    requeue_and_ack(&queue, envelope, &receipt, settings.busy_requeue_delay).await;
                    continue;
                }
            }
            let guard = match locks.try_lock(&envelope.job.serial_key()).await {
                Ok(Some(guard)) => guard,
                Ok(None) => {
                    requeue_and_ack(&queue, envelope, &receipt, settings.busy_requeue_delay).await;
                    continue;
                }
                Err(err) => {
                    warn!(error = %err, "job lock failed; requeueing");
                    requeue_and_ack(&queue, envelope, &receipt, settings.busy_requeue_delay).await;
                    continue;
                }
            };
            let (queue, pipeline, departments) = (
                Arc::clone(&queue),
                Arc::clone(&pipeline),
                Arc::clone(&departments),
            );
            tokio::spawn(async move {
                queue.running.fetch_add(1, Ordering::Relaxed);
                // An inner task turns a panic into an error, so the serial
                // key is always released and the job follows retry policy.
                let (runner, job) = (Arc::clone(&pipeline), envelope.job.clone());
                let last_attempt = envelope.attempt >= settings.max_retries;
                let lease = keep_lease(Arc::clone(&queue.backend), receipt.clone());
                let result = tokio::spawn(async move { runner.run(&job, last_attempt).await })
                    .await
                    .unwrap_or_else(|e| {
                        Err(DomainError::internal(format!("job panicked: {e}")).into())
                    });
                queue.running.fetch_sub(1, Ordering::Relaxed);
                drop(guard);
                drop(permit);
                // Ack only once the outcome is recorded; a crash before
                // that lets the backend redeliver the job.
                if handle_result(&queue, &pipeline, &departments, envelope, result, &settings).await
                {
                    ack(&queue, &receipt).await;
                }
                if let Some(lease) = lease {
                    lease.abort();
                }
            });
        }
    })
}

/// `true` once the job is safely queued again.
async fn requeue(queue: &WorkerQueue, envelope: Envelope, delay: Duration) -> bool {
    match queue.backend.push_later(envelope, delay).await {
        Ok(()) => true,
        Err(err) => {
            error!(error = %err, "failed to requeue job; leaving it to redelivery");
            false
        }
    }
}

/// A busy job goes back to the queue; its delivery is then done.
async fn requeue_and_ack(queue: &WorkerQueue, envelope: Envelope, receipt: &str, delay: Duration) {
    if requeue(queue, envelope, delay).await {
        ack(queue, receipt).await;
    }
}

async fn ack(queue: &WorkerQueue, receipt: &str) {
    if let Err(err) = queue.backend.ack(receipt).await {
        // Harmless beyond a possible duplicate run after the lease ends.
        warn!(error = %err, "failed to ack job; it may be redelivered");
    }
}

/// Keeps a running job's delivery leased so it is not redelivered while
/// it runs (LLM calls take minutes); the caller aborts it when done.
fn keep_lease(backend: Arc<dyn QueueBackend>, receipt: String) -> Option<JoinHandle<()>> {
    let every = backend.extend_interval()?;
    Some(tokio::spawn(async move {
        let mut ticker = tokio::time::interval(every);
        ticker.tick().await; // the first tick is immediate
        loop {
            ticker.tick().await;
            if let Err(err) = backend.extend(&receipt).await {
                warn!(error = %err, "failed to extend job lease");
            }
        }
    }))
}

fn drop_for_unknown_department(envelope: &Envelope) {
    info!(
        kind = envelope.job.kind(),
        department = %envelope.job.department(),
        "job dropped: its department no longer exists"
    );
}

/// Records the outcome; `true` once it is durable, so the delivery may be
/// acked.
async fn handle_result(
    queue: &WorkerQueue,
    pipeline: &PipelineService,
    departments: &Arc<dyn DepartmentDirectory>,
    envelope: Envelope,
    result: Result<(), JobFailure>,
    settings: &WorkerSettings,
) -> bool {
    let metrics = pipeline.metrics();
    let Err(JobFailure { error: err, redo }) = result else {
        metrics.completed(Utc::now().to_rfc3339());
        return true;
    };
    // Destroyed while the job ran: nothing to retry or keep. A replica
    // with the schema cached sees missing tables rather than
    // `unknown_department`, so the registry is asked as well; a destroy
    // removes dead letters only after the registry row, so a letter
    // written before that row went is removed with the rest.
    if err.code == "unknown_department"
        || matches!(
            departments.exists(envelope.job.department()).await,
            Ok(false)
        )
    {
        drop_for_unknown_department(&envelope);
        return true;
    }
    // A failure that would repeat is not retried (L1/L2 dropped its input).
    let retry =
        envelope.attempt < settings.max_retries && !failure_is_deterministic(&err.to_string());
    metrics.failed(format!("{} job failed: {err}", envelope.job.kind()), retry);
    if retry {
        // 5s, 15s, 45s …
        let delay = settings.base_backoff * 3u32.pow(envelope.attempt);
        warn!(kind = envelope.job.kind(), attempt = envelope.attempt + 1, error = %err, "job failed; retrying in {delay:?}");
        requeue(
            queue,
            Envelope {
                attempt: envelope.attempt + 1,
                ..envelope
            },
            delay,
        )
        .await
    } else {
        error!(kind = envelope.job.kind(), error = %err, "job dead-lettered");
        // A dropped input is kept as the job that redoes exactly it.
        let letter = DeadLetter {
            job: redo.map_or(envelope.job, |job| *job),
            attempt: envelope.attempt,
            error: Some(err.to_string()),
            dead_at_ms: Utc::now().timestamp_millis(),
            redrives: envelope.redrives,
            first_dead_at_ms: envelope.first_dead_at_ms,
            model: Some(pipeline.llm_model().to_owned()),
        };
        match queue.backend.dead_letter(letter).await {
            Ok(()) => true,
            Err(e) => {
                error!(error = %e, "failed to record dead letter");
                false
            }
        }
    }
}

/// Periodically expires and requeues dead letters (`DeadLetterPolicy`).
/// Taking a letter is exclusive, so several nodes may sweep at once.
pub fn spawn_dead_letter_sweeper(
    queue: Arc<WorkerQueue>,
    pipeline: Arc<PipelineService>,
    settings: WorkerSettings,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(settings.sweep_interval);
        loop {
            ticker.tick().await;
            match queue
                .sweep_dead_letters(
                    &settings.dead_letters,
                    pipeline.llm_model(),
                    settings.redrive_batch,
                )
                .await
            {
                Ok((redriven, expired)) => pipeline.metrics().swept(redriven, expired),
                Err(err) => warn!(error = %err, "dead letter sweep failed"),
            }
        }
    })
}

/// Periodically gives atoms stored without an embedding one. The first run
/// waits a full interval, so a restart does not hit a recovering embedding
/// service at once. Replicas may run it at the same time; an atom is only
/// given an embedding it does not have, so the worst case is a repeated
/// embedding call.
pub fn spawn_embedding_backfill(
    backfill: Arc<EmbeddingBackfill>,
    interval: Duration,
    batch: usize,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let start = tokio::time::Instant::now() + interval;
        let mut ticker = tokio::time::interval_at(start, interval);
        loop {
            ticker.tick().await;
            match backfill.all(batch).await {
                Ok(r) if r.missing > 0 => info!(
                    missing = r.missing,
                    embedded = r.embedded,
                    skipped = r.skipped,
                    "embedding backfill"
                ),
                Ok(_) => {}
                Err(err) => warn!(error = %err, "embedding backfill failed"),
            }
        }
    })
}

/// Hourly, closes skill-extraction tasks whose proposals all expired.
pub fn spawn_proposal_sweeper(
    extraction: Arc<SkillExtractionService>,
    departments: Arc<dyn AtomRepository>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(3600));
        loop {
            ticker.tick().await;
            let list = match departments.departments().await {
                Ok(list) => list,
                Err(err) => {
                    warn!(error = %err, "proposal sweep: cannot list departments");
                    continue;
                }
            };
            for department in list {
                match extraction.sweep_expired(&department).await {
                    Ok(0) => {}
                    Ok(n) => info!(%department, closed = n, "expired skill proposals closed"),
                    Err(err) => warn!(%department, error = %err, "proposal sweep failed"),
                }
            }
        }
    })
}

/// Hourly, drops audit events past their retention period.
pub fn spawn_audit_pruner(guard: Arc<CredentialGuard>) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(3600));
        loop {
            ticker.tick().await;
            match guard.prune().await {
                Ok(0) => {}
                Ok(n) => info!(removed = n, "audit log pruned"),
                Err(err) => warn!(error = %err, "audit log pruning failed"),
            }
        }
    })
}

/// Periodically fires due L1 idle timers and L2 schedules.
pub fn spawn_scanner(pipeline: Arc<PipelineService>, interval: Duration) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        loop {
            ticker.tick().await;
            if let Err(err) = pipeline.tick().await {
                warn!(error = %err, "pipeline timer scan failed");
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use memory_domain::scope::SegmentId;

    fn job() -> PipelineJob {
        PipelineJob::SkillExtract {
            department: SegmentId::default_bucket(),
            task_id: "t".into(),
        }
    }

    #[tokio::test]
    async fn the_sweeper_expires_and_redrives_dead_letters() {
        const HOUR: i64 = 3_600_000;
        let policy = DeadLetterPolicy {
            ttl_ms: 24 * HOUR,
            max_redrives: 3,
            redrive_after_ms: HOUR,
        };
        let now = Utc::now().timestamp_millis();
        let letter = |task: &str, error: &str, age: i64, redrives: u32, model: &str| DeadLetter {
            job: PipelineJob::SkillExtract {
                department: SegmentId::default_bucket(),
                task_id: task.into(),
            },
            attempt: 3,
            error: Some(error.into()),
            dead_at_ms: now - age,
            redrives,
            first_dead_at_ms: Some(now - age),
            model: Some(model.into()),
        };
        let backend = LocalQueue::new();
        for l in [
            letter("old", "LLM returned 503", 25 * HOUR, 0, "m"),
            letter("worn", "LLM returned 503", 2 * HOUR, 3, "m"),
            letter("due", "LLM returned 503", 2 * HOUR, 0, "m"),
            letter("fresh", "LLM returned 503", 10 * 60_000, 0, "m"),
            letter(
                "refused",
                "LLM_TOOL_REQUEST: `agy` produced no output",
                2 * HOUR,
                0,
                "m",
            ),
            letter(
                "other-model",
                "LLM_TOOL_REQUEST: `agy` produced no output",
                2 * HOUR,
                0,
                "old-model",
            ),
        ] {
            backend.dead_letter(l).await.unwrap();
        }
        let queue = WorkerQueue::new(backend.clone());

        let (redriven, expired) = queue.sweep_dead_letters(&policy, "m", 10).await.unwrap();
        assert_eq!((redriven, expired), (2, 2));
        let kept: Vec<_> = backend
            .dead_letters()
            .await
            .unwrap()
            .into_iter()
            .map(|l| match l.job {
                PipelineJob::SkillExtract { task_id, .. } => task_id,
                _ => unreachable!(),
            })
            .collect();
        assert_eq!(kept, ["fresh", "refused"]);

        // Requeued as first attempts that remember their history.
        for _ in 0..2 {
            let d = backend
                .pop(Duration::from_millis(200))
                .await
                .unwrap()
                .unwrap();
            assert_eq!((d.envelope.attempt, d.envelope.redrives), (0, 1));
            assert_eq!(d.envelope.first_dead_at_ms, Some(now - 2 * HOUR));
        }
    }

    /// A local queue whose `push_later` fails once `pushes` are used up.
    struct FailingPushes {
        inner: Arc<LocalQueue>,
        pushes: AtomicUsize,
    }

    #[async_trait]
    impl QueueBackend for FailingPushes {
        async fn push_new(&self, job: PipelineJob) -> DomainResult<()> {
            self.inner.push_new(job).await
        }
        async fn push_later(&self, envelope: Envelope, delay: Duration) -> DomainResult<()> {
            // Single-threaded test: load, then take one.
            if self.pushes.load(Ordering::SeqCst) == 0 {
                return Err(DomainError::unavailable("queue unreachable"));
            }
            self.pushes.fetch_sub(1, Ordering::SeqCst);
            self.inner.push_later(envelope, delay).await
        }
        async fn pop(&self, wait: Duration) -> DomainResult<Option<Delivery>> {
            self.inner.pop(wait).await
        }
        async fn ack(&self, receipt: &str) -> DomainResult<()> {
            self.inner.ack(receipt).await
        }
        async fn extend(&self, receipt: &str) -> DomainResult<()> {
            self.inner.extend(receipt).await
        }
        async fn dead_letter(&self, letter: DeadLetter) -> DomainResult<()> {
            self.inner.dead_letter(letter).await
        }
        async fn dead_letters(&self) -> DomainResult<Vec<DeadLetter>> {
            self.inner.dead_letters().await
        }
        async fn take_dead_letters(
            &self,
            select: &(dyn for<'a> Fn(&'a DeadLetter) -> bool + Sync),
            limit: usize,
        ) -> DomainResult<Vec<DeadLetter>> {
            self.inner.take_dead_letters(select, limit).await
        }
        async fn counts(&self) -> DomainResult<(usize, usize)> {
            self.inner.counts().await
        }
    }

    #[tokio::test]
    async fn a_failed_redrive_keeps_every_letter_not_yet_queued() {
        let inner = LocalQueue::new();
        for task in ["a", "b", "c"] {
            inner
                .dead_letter(DeadLetter {
                    job: PipelineJob::SkillExtract {
                        department: SegmentId::default_bucket(),
                        task_id: task.into(),
                    },
                    attempt: 3,
                    error: Some("LLM returned 503".into()),
                    dead_at_ms: 1,
                    redrives: 0,
                    first_dead_at_ms: None,
                    model: None,
                })
                .await
                .unwrap();
        }
        let queue = WorkerQueue::new(Arc::new(FailingPushes {
            inner: inner.clone(),
            pushes: AtomicUsize::new(1),
        }));
        let all = DeadLetterFilter::default();
        assert!(queue.redrive(&all, 10).await.is_err());
        // "a" was queued; "b" (the failing push) and "c" (never tried) are kept.
        assert_eq!(inner.dead_letters().await.unwrap().len(), 2);
        assert!(inner
            .pop(Duration::from_millis(200))
            .await
            .unwrap()
            .is_some());
    }

    #[tokio::test]
    async fn local_queue_coalesces_waiting_duplicates() {
        let q = LocalQueue::new();
        q.push_new(job()).await.unwrap();
        q.push_new(job()).await.unwrap();
        assert_eq!(q.counts().await.unwrap(), (1, 0));
        assert!(q.pop(POP_WAIT).await.unwrap().is_some());
        q.push_new(job()).await.unwrap();
        assert_eq!(
            q.counts().await.unwrap(),
            (1, 0),
            "a popped job no longer coalesces"
        );
    }

    #[tokio::test]
    async fn delayed_push_arrives_after_delay() {
        let q = LocalQueue::new();
        q.push_later(
            Envelope {
                attempt: 2,
                ..Envelope::new(job())
            },
            Duration::from_millis(30),
        )
        .await
        .unwrap();
        assert!(q.pop(Duration::from_millis(5)).await.unwrap().is_none());
        let d = q.pop(Duration::from_millis(200)).await.unwrap().unwrap();
        assert_eq!(d.envelope.attempt, 2);
    }
}
