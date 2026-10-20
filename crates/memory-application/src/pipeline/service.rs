//! Orchestrates L0 → L1 → L2 → L3. Scheduling decisions come from
//! `memory_domain::pipeline`; state is durable via `PipelineStateRepository`.
//!
//! Concurrency rule: state documents are only modified inside
//! `deps.locks` for their key, and long-running steps (LLM calls) work on a
//! snapshot, then re-read the state and update only the fields they own.

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use chrono::{DateTime, Duration, TimeZone, Utc};
use memory_domain::error::{DomainError, DomainResult};
use memory_domain::memory::Layer;
use memory_domain::pipeline::{
    next_l2_due, persona_trigger, PersonaState, PipelineSettings, Warmup,
};
use memory_domain::profile::{strip_scene_navigation, ProfileScope};
use memory_domain::scope::{InstanceId, MemoryScope};
use serde::Serialize;
use tracing::{debug, info, warn};

use super::generation_logs::{GenerationLogService, GenerationRecord};
use super::l1::L1Extractor;
use super::l2::SceneExtractor;
use super::l3::{L3Outcome, PersonaGenerator};
use super::memory_prompts::MemoryPromptService;
use super::metrics::{MetricsSnapshot, PipelineMetrics};
use super::prompts::{self, CustomStrategy};
use crate::memory::service::{ConversationObserver, MemoryService};
use crate::ports::coordination::KeyLock;
use crate::ports::memory::{AtomRepository, ConversationRepository};
use crate::ports::pipeline::{
    DeadLetter, DeadLetterFilter, GenerationRef, JobQueue, PipelineJob, PipelineStateRepository,
    ProfileState, QueueStats, SessionState,
};
use crate::ports::system::{Clock, LlmClient};

const L1_PROCESS_BATCH: usize = 10;
const L2_ATOM_BATCH: usize = 100;

/// Runs skill-extraction jobs; registered by the skill module.
#[async_trait]
pub trait SkillJobRunner: Send + Sync {
    async fn run_skill_task(&self, instance: &InstanceId, task_id: &str) -> DomainResult<()>;
}

/// What `record_failure` needs about a generation that did not finish.
struct Failure<'a> {
    layer: Layer,
    team_id: &'a str,
    agent_id: &'a str,
    user_id: Option<&'a str>,
    session_id: Option<&'a str>,
    strategy: Option<&'a CustomStrategy>,
    input_refs: Vec<GenerationRef>,
    started_at_ms: i64,
}

fn refs<'a>(layer: &str, ids: impl Iterator<Item = &'a String>) -> Vec<GenerationRef> {
    ids.map(|id| GenerationRef {
        layer: layer.into(),
        record_id: id.clone(),
    })
    .collect()
}

pub struct PipelineDeps {
    pub state: Arc<dyn PipelineStateRepository>,
    pub queue: Arc<dyn JobQueue>,
    pub conversations: Arc<dyn ConversationRepository>,
    pub atoms: Arc<dyn AtomRepository>,
    pub memory: Arc<MemoryService>,
    pub l1: Arc<L1Extractor>,
    pub l2: Arc<SceneExtractor>,
    pub l3: Arc<PersonaGenerator>,
    pub prompts: Arc<MemoryPromptService>,
    pub logs: Arc<GenerationLogService>,
    pub llm: Arc<dyn LlmClient>,
    pub clock: Arc<dyn Clock>,
    pub metrics: Arc<PipelineMetrics>,
    /// Guards read-modify-write of pipeline state documents.
    pub locks: Arc<dyn KeyLock>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PipelineStatus {
    pub worker: MetricsSnapshot,
    pub queue: QueueStats,
    pub sessions: Vec<SessionState>,
    pub profiles: Vec<ProfileState>,
}

pub struct PipelineService {
    deps: PipelineDeps,
    settings: PipelineSettings,
    skill_runner: OnceLock<Arc<dyn SkillJobRunner>>,
}

fn session_key(instance: &InstanceId, scope: &MemoryScope, session_id: &str) -> String {
    format!("s:{instance}/{}/{session_id}", scope.key())
}

fn profile_key(instance: &InstanceId, profile: &ProfileScope) -> String {
    format!("p:{instance}/{}", profile.key())
}

fn max_seq(current: Option<i64>, seen: Option<i64>) -> Option<i64> {
    current.max(seen)
}

impl PipelineService {
    pub fn new(deps: PipelineDeps, settings: PipelineSettings) -> Self {
        Self {
            deps,
            settings,
            skill_runner: OnceLock::new(),
        }
    }

    pub fn register_skill_runner(&self, runner: Arc<dyn SkillJobRunner>) {
        let _ = self.skill_runner.set(runner);
    }

    /// The LLM distillation uses, as generation logs record it.
    pub fn llm_model(&self) -> &str {
        self.deps.llm.model()
    }

    pub fn llm_configured(&self) -> bool {
        self.deps.llm.is_configured()
    }

    /// Jobs that failed every retry, newest first.
    pub async fn dead_letters(
        &self,
        filter: &DeadLetterFilter,
        limit: usize,
    ) -> DomainResult<Vec<DeadLetter>> {
        self.deps.queue.dead_letters(filter, limit).await
    }

    /// Queues dead letters again as first attempts; returns how many.
    pub async fn redrive_dead_letters(
        &self,
        filter: &DeadLetterFilter,
        limit: usize,
    ) -> DomainResult<usize> {
        let requeued = self.deps.queue.redrive(filter, limit).await?;
        if requeued > 0 {
            info!(requeued, kind = ?filter.kind, "dead letters redriven");
        }
        Ok(requeued)
    }

    pub fn metrics(&self) -> &Arc<PipelineMetrics> {
        &self.deps.metrics
    }

    pub fn settings(&self) -> &PipelineSettings {
        &self.settings
    }

    pub async fn enqueue_job(&self, job: PipelineJob) -> DomainResult<()> {
        debug!(kind = job.kind(), key = %job.serial_key(), "enqueue pipeline job");
        self.deps.metrics.enqueued();
        self.deps.queue.enqueue(job).await
    }

    fn now(&self) -> DateTime<Utc> {
        self.deps.clock.now()
    }

    async fn load_session(
        &self,
        instance: &InstanceId,
        scope: &MemoryScope,
        session_id: &str,
    ) -> DomainResult<SessionState> {
        Ok(
            match self
                .deps
                .state
                .get_session(instance, scope, session_id)
                .await?
            {
                Some(s) => s,
                None => SessionState {
                    instance: instance.clone(),
                    scope: scope.clone(),
                    session_id: session_id.to_owned(),
                    conversation_count: 0,
                    warmup: Warmup::initial(&self.settings).0,
                    l1_cursor: None,
                    l1_due_at: None,
                    last_l1_at: None,
                    last_scene_name: None,
                    last_active_at: self.now(),
                },
            },
        )
    }

    async fn load_profile(
        &self,
        instance: &InstanceId,
        profile: &ProfileScope,
    ) -> DomainResult<ProfileState> {
        Ok(self
            .deps
            .state
            .get_profile(instance, profile)
            .await?
            .unwrap_or_else(|| ProfileState::new(instance.clone(), profile.clone())))
    }

    /// Counts user rounds; triggers L1 at the warm-up threshold, otherwise
    /// (re)arms the idle debounce.
    async fn record_rounds(
        &self,
        instance: &InstanceId,
        scope: &MemoryScope,
        session_id: &str,
        rounds: u32,
    ) -> DomainResult<()> {
        let triggered = {
            let _guard = self
                .deps
                .locks
                .lock(&session_key(instance, scope, session_id))
                .await?;
            let mut state = self.load_session(instance, scope, session_id).await?;
            let now = self.now();
            state.conversation_count += rounds;
            state.last_active_at = now;
            let every_n = self.settings.every_n_conversations;
            let triggered = state.conversation_count >= Warmup(state.warmup).threshold(every_n);
            if triggered {
                state.warmup = Warmup(state.warmup).advance(every_n).0;
                state.conversation_count = 0;
                state.l1_due_at = None;
            } else {
                state.l1_due_at =
                    Some(now + Duration::seconds(self.settings.l1_idle_timeout_secs as i64));
            }
            self.deps.state.put_session(&state).await?;
            triggered
        };
        if triggered {
            self.enqueue_job(PipelineJob::L1 {
                instance: instance.clone(),
                scope: scope.clone(),
                session_id: session_id.to_owned(),
            })
            .await?;
        }
        Ok(())
    }

    /// Fires due timers. Called periodically by the infrastructure scanner.
    pub async fn tick(&self) -> DomainResult<()> {
        let now = self.now();
        for due in self.deps.state.due_sessions(now).await? {
            let fire = {
                let _guard = self
                    .deps
                    .locks
                    .lock(&session_key(&due.instance, &due.scope, &due.session_id))
                    .await?;
                let mut state = self
                    .load_session(&due.instance, &due.scope, &due.session_id)
                    .await?;
                if state.l1_due_at.is_none_or(|t| t > now) {
                    continue;
                }
                let fire = state.conversation_count > 0;
                state.l1_due_at = None;
                state.conversation_count = 0;
                self.deps.state.put_session(&state).await?;
                fire
            };
            if fire {
                self.enqueue_job(PipelineJob::L1 {
                    instance: due.instance,
                    scope: due.scope,
                    session_id: due.session_id,
                })
                .await?;
            }
        }
        for due in self.deps.state.due_profiles(now).await? {
            {
                let _guard = self
                    .deps
                    .locks
                    .lock(&profile_key(&due.instance, &due.profile))
                    .await?;
                let mut state = self.load_profile(&due.instance, &due.profile).await?;
                if state.l2_due_at.is_none_or(|t| t > now) {
                    continue;
                }
                state.l2_due_at = None;
                self.deps.state.put_profile(&state).await?;
            }
            self.enqueue_job(PipelineJob::L2 {
                instance: due.instance,
                profile: due.profile,
            })
            .await?;
        }
        Ok(())
    }

    /// Executes one job. Errors are retried by the worker.
    pub async fn run(&self, job: &PipelineJob) -> DomainResult<()> {
        match job {
            PipelineJob::L1 {
                instance,
                scope,
                session_id,
            } => self.run_l1(instance, scope, session_id).await,
            PipelineJob::L2 { instance, profile } => self.run_l2(instance, profile).await,
            PipelineJob::L3 { instance, profile } => self.run_l3(instance, profile).await,
            PipelineJob::SkillExtract { instance, task_id } => match self.skill_runner.get() {
                Some(runner) => runner.run_skill_task(instance, task_id).await,
                None => Ok(()),
            },
        }
    }

    async fn run_l1(
        &self,
        instance: &InstanceId,
        scope: &MemoryScope,
        session_id: &str,
    ) -> DomainResult<()> {
        if !self.deps.llm.is_configured() {
            warn!("LLM not configured; skipping L1 extraction");
            return Ok(());
        }
        let snapshot = self.load_session(instance, scope, session_id).await?;
        let mut fetched = self
            .deps
            .conversations
            .after_seq(
                instance,
                scope,
                session_id,
                snapshot.l1_cursor,
                L1_PROCESS_BATCH + 1,
            )
            .await?;
        let has_more = fetched.len() > L1_PROCESS_BATCH;
        fetched.truncate(L1_PROCESS_BATCH);
        let Some(last_seq) = fetched.last().map(|m| m.seq) else {
            return Ok(());
        };
        let batch: Vec<_> = fetched.into_iter().map(|m| m.item).collect();

        let started = self.now().timestamp_millis();
        let strategy = self
            .deps
            .prompts
            .resolve(
                instance,
                scope.team_id.as_str(),
                scope.agent_id.as_str(),
                Layer::L1,
            )
            .await?;
        let extracted = self
            .deps
            .l1
            .extract(
                instance,
                scope,
                session_id,
                &batch,
                snapshot.last_scene_name.as_deref(),
                strategy.as_ref(),
            )
            .await;
        let outcome = match extracted {
            Ok(outcome) => outcome,
            Err(err) => {
                self.record_failure(
                    instance,
                    Failure {
                        layer: Layer::L1,
                        team_id: scope.team_id.as_str(),
                        agent_id: scope.agent_id.as_str(),
                        user_id: Some(scope.user_id.as_str()),
                        session_id: Some(session_id),
                        strategy: strategy.as_ref(),
                        input_refs: refs("l0", batch.iter().map(|m| &m.id)),
                        started_at_ms: started,
                    },
                    &err,
                )
                .await;
                return Err(err);
            }
        };

        {
            let _guard = self
                .deps
                .locks
                .lock(&session_key(instance, scope, session_id))
                .await?;
            let mut state = self.load_session(instance, scope, session_id).await?;
            state.l1_cursor = max_seq(state.l1_cursor, Some(last_seq));
            state.last_scene_name = outcome.last_scene.clone();
            state.last_l1_at = Some(self.now());
            self.deps.state.put_session(&state).await?;
        }
        info!(
            session = session_id,
            written = outcome.written.len(),
            "L1 extraction finished"
        );

        if outcome.llm_called {
            self.deps
                .logs
                .record(
                    instance,
                    GenerationRecord {
                        layer: Layer::L1,
                        team_id: scope.team_id.as_str(),
                        agent_id: scope.agent_id.as_str(),
                        user_id: Some(scope.user_id.as_str()),
                        session_id: Some(session_id),
                        strategy: strategy.as_ref(),
                        system_prompt: &outcome.system_prompt,
                        model: self.deps.llm.model(),
                        input_refs: batch
                            .iter()
                            .map(|m| GenerationRef {
                                layer: "l0".into(),
                                record_id: m.id.clone(),
                            })
                            .collect(),
                        output_refs: outcome
                            .written
                            .iter()
                            .map(|a| GenerationRef {
                                layer: "l1".into(),
                                record_id: a.id.clone(),
                            })
                            .collect(),
                        started_at_ms: started,
                        error: None,
                    },
                )
                .await;
        }
        if !outcome.written.is_empty() {
            self.schedule_l2(
                instance,
                &ProfileScope::from(scope),
                outcome.written.len() as u32,
            )
            .await?;
        }
        if has_more {
            self.enqueue_job(PipelineJob::L1 {
                instance: instance.clone(),
                scope: scope.clone(),
                session_id: session_id.to_owned(),
            })
            .await?;
        }
        Ok(())
    }

    /// Moves the L2 deadline earlier (never later) after new atoms.
    async fn schedule_l2(
        &self,
        instance: &InstanceId,
        profile: &ProfileScope,
        new_atoms: u32,
    ) -> DomainResult<()> {
        let _guard = self
            .deps
            .locks
            .lock(&profile_key(instance, profile))
            .await?;
        let mut state = self.load_profile(instance, profile).await?;
        let now = self.now();
        let due_secs = next_l2_due(
            now.timestamp(),
            state.last_l2_at.map(|t| t.timestamp()),
            &self.settings,
        );
        let due = Utc.timestamp_opt(due_secs, 0).single().unwrap_or(now);
        state.l2_pending = true;
        state.memories_since_persona += new_atoms;
        state.l2_due_at = Some(state.l2_due_at.map_or(due, |current| current.min(due)));
        self.deps.state.put_profile(&state).await
    }

    async fn run_l2(&self, instance: &InstanceId, profile: &ProfileScope) -> DomainResult<()> {
        if !self.deps.llm.is_configured() {
            return Ok(());
        }
        let snapshot = self.load_profile(instance, profile).await?;
        let mut fetched = self
            .deps
            .atoms
            .after_seq(instance, profile, snapshot.l2_cursor, L2_ATOM_BATCH + 1)
            .await?;
        let has_more = fetched.len() > L2_ATOM_BATCH;
        fetched.truncate(L2_ATOM_BATCH);
        let Some(last_seq) = fetched.last().map(|a| a.seq) else {
            let _guard = self
                .deps
                .locks
                .lock(&profile_key(instance, profile))
                .await?;
            let mut state = self.load_profile(instance, profile).await?;
            state.l2_pending = false;
            return self.deps.state.put_profile(&state).await;
        };
        let atoms: Vec<_> = fetched.into_iter().map(|a| a.item).collect();

        let started = self.now().timestamp_millis();
        let strategy = self
            .deps
            .prompts
            .resolve(
                instance,
                profile.team_id.as_str(),
                profile.agent_id.as_str(),
                Layer::L2,
            )
            .await?;
        let consolidated = self
            .deps
            .l2
            .consolidate(instance, profile, &atoms, strategy.as_ref())
            .await;
        let outcome = match consolidated {
            Ok(outcome) => outcome,
            Err(err) => {
                self.record_failure(
                    instance,
                    Failure {
                        layer: Layer::L2,
                        team_id: profile.team_id.as_str(),
                        agent_id: profile.agent_id.as_str(),
                        user_id: None,
                        session_id: None,
                        strategy: strategy.as_ref(),
                        input_refs: refs("l1", atoms.iter().map(|a| &a.id)),
                        started_at_ms: started,
                    },
                    &err,
                )
                .await;
                return Err(err);
            }
        };

        let state = {
            let _guard = self
                .deps
                .locks
                .lock(&profile_key(instance, profile))
                .await?;
            let mut state = self.load_profile(instance, profile).await?;
            let now = self.now();
            state.l2_cursor = max_seq(state.l2_cursor, Some(last_seq));
            state.last_l2_at = Some(now);
            state.l2_pending = has_more;
            state.scenes_processed += 1;
            if outcome.persona_update_request.is_some() {
                state.persona_update_request = outcome.persona_update_request.clone();
            }
            // Periodic sweep at the max interval; sooner for a backlog or if
            // new atoms scheduled an earlier run meanwhile.
            let sweep = if has_more {
                now
            } else {
                now + Duration::seconds(self.settings.l2_max_interval_secs as i64)
            };
            state.l2_due_at = Some(state.l2_due_at.map_or(sweep, |t| t.min(sweep)));
            self.deps.state.put_profile(&state).await?;
            state
        };

        {
            self.deps
                .logs
                .record(
                    instance,
                    GenerationRecord {
                        layer: Layer::L2,
                        team_id: profile.team_id.as_str(),
                        agent_id: profile.agent_id.as_str(),
                        user_id: None,
                        session_id: None,
                        strategy: strategy.as_ref(),
                        system_prompt: &outcome.system_prompt,
                        model: self.deps.llm.model(),
                        input_refs: atoms
                            .iter()
                            .map(|a| GenerationRef {
                                layer: "l1".into(),
                                record_id: a.id.clone(),
                            })
                            .collect(),
                        output_refs: outcome
                            .changed_files
                            .iter()
                            .map(|f| GenerationRef {
                                layer: "l2".into(),
                                record_id: f.clone(),
                            })
                            .collect(),
                        started_at_ms: started,
                        error: None,
                    },
                )
                .await;
        }
        if self.persona_due(instance, profile, &state).await?.is_some() {
            self.enqueue_job(PipelineJob::L3 {
                instance: instance.clone(),
                profile: profile.clone(),
            })
            .await?;
        }
        Ok(())
    }

    async fn persona_due(
        &self,
        instance: &InstanceId,
        profile: &ProfileScope,
        state: &ProfileState,
    ) -> DomainResult<Option<String>> {
        let has_persona = self
            .deps
            .memory
            .read_persona(instance, profile, None)
            .await?
            .is_some_and(|p| !strip_scene_navigation(&p.content).is_empty());
        let persona_state = PersonaState {
            has_persona,
            scene_count: self.deps.memory.count_scenes(instance, profile, "").await?,
            scenes_processed: state.scenes_processed,
            memories_since_last_persona: state.memories_since_persona,
            update_requested: state.persona_update_request.clone(),
        };
        Ok(persona_trigger(
            &persona_state,
            self.settings.persona_trigger_every_n,
        ))
    }

    async fn run_l3(&self, instance: &InstanceId, profile: &ProfileScope) -> DomainResult<()> {
        if !self.deps.llm.is_configured() {
            return Ok(());
        }
        let snapshot = self.load_profile(instance, profile).await?;
        let Some(trigger) = self.persona_due(instance, profile, &snapshot).await? else {
            return Ok(());
        };
        let started = self.now().timestamp_millis();
        let strategy = self
            .deps
            .prompts
            .resolve(
                instance,
                profile.team_id.as_str(),
                profile.agent_id.as_str(),
                Layer::L3,
            )
            .await?;
        let generated = self
            .deps
            .l3
            .generate(
                instance,
                profile,
                snapshot.last_l3_at,
                &trigger,
                strategy.as_ref(),
            )
            .await;
        let outcome = match generated {
            Ok(outcome) => outcome,
            Err(err) => {
                self.record_failure(
                    instance,
                    Failure {
                        layer: Layer::L3,
                        team_id: profile.team_id.as_str(),
                        agent_id: profile.agent_id.as_str(),
                        user_id: None,
                        session_id: None,
                        strategy: strategy.as_ref(),
                        input_refs: Vec::new(),
                        started_at_ms: started,
                    },
                    &err,
                )
                .await;
                return Err(err);
            }
        };
        let L3Outcome::Generated {
            scene_files,
            system_prompt,
        } = outcome
        else {
            return Ok(());
        };
        {
            let _guard = self
                .deps
                .locks
                .lock(&profile_key(instance, profile))
                .await?;
            let mut state = self.load_profile(instance, profile).await?;
            // Memories that arrived during generation still count.
            state.memories_since_persona = state
                .memories_since_persona
                .saturating_sub(snapshot.memories_since_persona);
            if state.persona_update_request == snapshot.persona_update_request {
                state.persona_update_request = None;
            }
            state.last_l3_at = Some(self.now());
            self.deps.state.put_profile(&state).await?;
        }
        info!(profile = %profile.key(), %trigger, "persona regenerated");
        self.deps
            .logs
            .record(
                instance,
                GenerationRecord {
                    layer: Layer::L3,
                    team_id: profile.team_id.as_str(),
                    agent_id: profile.agent_id.as_str(),
                    user_id: None,
                    session_id: None,
                    strategy: strategy.as_ref(),
                    system_prompt: &system_prompt,
                    model: self.deps.llm.model(),
                    input_refs: scene_files
                        .iter()
                        .map(|f| GenerationRef {
                            layer: "l2".into(),
                            record_id: f.clone(),
                        })
                        .collect(),
                    output_refs: vec![GenerationRef {
                        layer: "l3".into(),
                        record_id: "persona.md".into(),
                    }],
                    started_at_ms: started,
                    error: None,
                },
            )
            .await;
        Ok(())
    }

    /// Records a generation whose LLM call (or parsing its answer) failed,
    /// so failures show up next to successes in the generation logs.
    async fn record_failure(&self, instance: &InstanceId, f: Failure<'_>, err: &DomainError) {
        let system_prompt = prompts::compose(f.layer, f.strategy);
        self.deps
            .logs
            .record(
                instance,
                GenerationRecord {
                    layer: f.layer,
                    team_id: f.team_id,
                    agent_id: f.agent_id,
                    user_id: f.user_id,
                    session_id: f.session_id,
                    strategy: f.strategy,
                    system_prompt: &system_prompt,
                    model: self.deps.llm.model(),
                    input_refs: f.input_refs,
                    output_refs: Vec::new(),
                    started_at_ms: f.started_at_ms,
                    error: Some(err.to_string()),
                },
            )
            .await;
    }

    pub async fn status(
        &self,
        instance: &InstanceId,
        limit: usize,
    ) -> DomainResult<PipelineStatus> {
        Ok(PipelineStatus {
            worker: self.deps.metrics.snapshot(),
            queue: self.deps.queue.stats().await?,
            sessions: self.deps.state.list_sessions(instance, limit).await?,
            profiles: self.deps.state.list_profiles(instance, limit).await?,
        })
    }

    /// Forgets L2/L3 progress of a profile (used when its memory is cleared).
    pub async fn reset_profile(
        &self,
        instance: &InstanceId,
        profile: &ProfileScope,
    ) -> DomainResult<()> {
        let _guard = self
            .deps
            .locks
            .lock(&profile_key(instance, profile))
            .await?;
        self.deps.state.delete_profile(instance, profile).await
    }
}

#[async_trait]
impl ConversationObserver for PipelineService {
    async fn on_conversation(
        &self,
        instance: &InstanceId,
        scope: &MemoryScope,
        session_id: &str,
        user_rounds: u32,
    ) {
        if let Err(err) = self
            .record_rounds(instance, scope, session_id, user_rounds)
            .await
        {
            warn!(error = %err, "failed to update pipeline state");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursors_only_move_forward() {
        assert_eq!(max_seq(None, Some(3)), Some(3));
        assert_eq!(max_seq(Some(5), Some(3)), Some(5));
    }
}
