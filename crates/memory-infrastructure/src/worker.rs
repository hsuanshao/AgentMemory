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
use memory_application::pipeline::service::{JobFailure, PipelineService};
use memory_application::ports::coordination::KeyLock;
use memory_application::ports::pipeline::{
    DeadLetter, DeadLetterFilter, JobQueue, PipelineJob, QueueStats,
};
use memory_domain::error::{DomainError, DomainResult};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, Semaphore};
use tokio::task::JoinHandle;
use tracing::{debug, error, warn};

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
}

impl Default for WorkerSettings {
    fn default() -> Self {
        Self {
            concurrency: 8,
            max_retries: 3,
            base_backoff: Duration::from_secs(5),
            busy_requeue_delay: Duration::from_millis(200),
            scan_interval: Duration::from_secs(2),
        }
    }
}

/// A queued job and how many times it has failed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Envelope {
    pub job: PipelineJob,
    pub attempt: u32,
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
    /// Removes and returns up to `limit` dead letters matching `filter`,
    /// oldest first. A letter is returned by at most one caller, even with
    /// several nodes redriving at once.
    async fn take_dead_letters(
        &self,
        filter: &DeadLetterFilter,
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
        let taken = self.backend.take_dead_letters(filter, limit).await?;
        let mut requeued = 0;
        for letter in taken {
            if let Err(err) = self.backend.push_new(letter.job.clone()).await {
                // Keep it rather than lose it; the next redrive tries again.
                warn!(error = %err, "redrive failed; keeping the dead letter");
                self.backend.dead_letter(letter).await?;
                return Err(err);
            }
            requeued += 1;
        }
        Ok(requeued)
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
        Self::send(&self.tx, Envelope { job, attempt: 0 })
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
        filter: &DeadLetterFilter,
        limit: usize,
    ) -> DomainResult<Vec<DeadLetter>> {
        let mut dead = self.dead.lock().expect("dead letter lock");
        let mut taken = Vec::new();
        let mut kept = VecDeque::with_capacity(dead.len());
        for letter in dead.drain(..) {
            if taken.len() < limit && filter.matches(&letter) {
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
/// serial key while the pipeline takes state locks inside it.
pub fn spawn_worker(
    queue: Arc<WorkerQueue>,
    locks: Arc<dyn KeyLock>,
    pipeline: Arc<PipelineService>,
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
            let (queue, pipeline) = (Arc::clone(&queue), Arc::clone(&pipeline));
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
                if handle_result(&queue, &pipeline, envelope, result, &settings).await {
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

/// Records the outcome; `true` once it is durable, so the delivery may be
/// acked.
async fn handle_result(
    queue: &WorkerQueue,
    pipeline: &PipelineService,
    envelope: Envelope,
    result: Result<(), JobFailure>,
    settings: &WorkerSettings,
) -> bool {
    let metrics = pipeline.metrics();
    let Err(JobFailure { error: err, redo }) = result else {
        metrics.completed(Utc::now().to_rfc3339());
        return true;
    };
    let retry = envelope.attempt < settings.max_retries;
    metrics.failed(format!("{} job failed: {err}", envelope.job.kind()), retry);
    if retry {
        // 5s, 15s, 45s …
        let delay = settings.base_backoff * 3u32.pow(envelope.attempt);
        warn!(kind = envelope.job.kind(), attempt = envelope.attempt + 1, error = %err, "job failed; retrying in {delay:?}");
        requeue(
            queue,
            Envelope {
                job: envelope.job,
                attempt: envelope.attempt + 1,
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
                job: job(),
                attempt: 2,
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
