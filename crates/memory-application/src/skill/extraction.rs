//! Conversation-driven skill extraction.
//!
//! Each finished turn is appended to a per-session buffer. When the buffer
//! crosses a threshold (tool calls, bytes, or a very large turn) it is
//! archived and a background job asks the LLM to create or improve skills.

use std::sync::Arc;

use async_trait::async_trait;
use memory_domain::error::{DomainError, DomainResult};
use memory_domain::memory::without_nul;
use memory_domain::scope::DepartmentId;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tracing::{info, warn};

use super::service::{ListQuery, ReadOptions, SkillIdentity, SkillService, StringPatch};
use crate::pipeline::llm_json::parse_llm_json;
use crate::pipeline::service::{PipelineService, SkillJobRunner};
use crate::ports::pipeline::PipelineJob;
use crate::ports::skill::{
    BufferCounters, BufferKey, BufferedMessage, SkillBufferStore, SkillTask,
};
use crate::ports::system::{ChatMessage, Clock, IdGenerator, LlmClient};

const ROLES: [&str; 5] = ["user", "assistant", "tool_call", "tool_result", "system"];
const COMPRESSED_MARKER: &str = "\n\n[middle content too long; only head and tail kept]\n\n";

#[derive(Debug, Clone, Copy)]
pub struct ExtractionSettings {
    pub enabled: bool,
    pub archive_bytes: usize,
    pub tool_call_threshold: u32,
    pub tool_content_threshold: usize,
    pub tool_keep_bytes: usize,
    pub prefix_skills_limit: usize,
}

impl Default for ExtractionSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            archive_bytes: 40 * 1024,
            tool_call_threshold: 10,
            tool_content_threshold: 2048,
            tool_keep_bytes: 1024,
            prefix_skills_limit: 20,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Archived {
    pub task_id: String,
    pub archived_at_ms: i64,
    pub archive_key: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct AddResult {
    pub status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archived: Option<Archived>,
}

const SKILL_REVIEW_PROMPT: &str = r#"You review a finished agent conversation and decide whether it contains reusable,
procedural know-how worth saving as a Skill (a SKILL.md playbook other agents can follow).

Good skills: multi-step procedures that worked, troubleshooting paths, checklists, conventions
learned the hard way. Not skills: one-off facts, chit-chat, user preferences (those are memories).

Prefer improving an existing skill (patch/update) over creating a near-duplicate. The existing
skills are the ones listed in the input; there are no others to look for.
A SKILL.md must start with frontmatter:
---
name: lowercase-kebab-name
description: one line saying when to use it
---
followed by markdown: when to use, steps, validation, pitfalls.

## Output — JSON only
{"actions": [
  {"action": "create", "name": "kebab-name", "content": "<full SKILL.md>"},
  {"action": "update", "skill_id": "skl-…", "content": "<full SKILL.md>"},
  {"action": "patch", "skill_id": "skl-…", "old_string": "…", "new_string": "…"}
], "summary": "one line"}
Return {"actions": [], "summary": "Nothing to save."} when nothing qualifies."#;

#[derive(Debug, Deserialize)]
struct ReviewPlan {
    #[serde(default)]
    actions: Vec<ReviewAction>,
    #[serde(default)]
    summary: String,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "action", rename_all = "lowercase")]
enum ReviewAction {
    Create {
        name: String,
        content: String,
    },
    Update {
        skill_id: String,
        content: String,
    },
    Patch {
        skill_id: String,
        old_string: String,
        new_string: String,
    },
}

pub struct SkillExtractionService {
    buffers: Arc<dyn SkillBufferStore>,
    skills: Arc<SkillService>,
    pipeline: Arc<PipelineService>,
    llm: Arc<dyn LlmClient>,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGenerator>,
    settings: ExtractionSettings,
}

impl SkillExtractionService {
    pub fn new(
        buffers: Arc<dyn SkillBufferStore>,
        skills: Arc<SkillService>,
        pipeline: Arc<PipelineService>,
        llm: Arc<dyn LlmClient>,
        clock: Arc<dyn Clock>,
        ids: Arc<dyn IdGenerator>,
        settings: ExtractionSettings,
    ) -> Self {
        Self {
            buffers,
            skills,
            pipeline,
            llm,
            clock,
            ids,
            settings,
        }
    }

    fn require_enabled(&self) -> DomainResult<()> {
        if self.settings.enabled {
            Ok(())
        } else {
            Err(DomainError::not_found(
                "skill_extraction",
                "module disabled",
            ))
        }
    }

    /// Appends a turn; archives the buffer when a threshold is crossed.
    pub async fn conversation_add(
        &self,
        department: &DepartmentId,
        key: BufferKey,
        messages: Vec<BufferedMessage>,
    ) -> DomainResult<AddResult> {
        self.require_enabled()?;
        validate_key(&key)?;
        let messages = without_nul_messages(messages);
        validate_messages(&messages)?;
        let raw_bytes = byte_size(&messages);
        let compress = raw_bytes >= self.settings.archive_bytes;
        let incoming = if compress {
            self.compress_tools(messages)
        } else {
            messages
        };

        let (mut buffer, counters) = self.buffers.load(department, &key).await?;
        buffer.extend(incoming);
        let mut oversize = false;
        if compress && byte_size(&buffer) > self.settings.archive_bytes * 2 {
            buffer = self.trim_middle(buffer);
            oversize = true;
        }
        let next = BufferCounters {
            tool_call_count: buffer.iter().filter(|m| m.role == "tool_call").count() as u32,
            byte_count: counters.byte_count + raw_bytes,
        };
        let reason = if oversize {
            Some("oversize")
        } else if compress {
            Some("compressed")
        } else if next.tool_call_count >= self.settings.tool_call_threshold {
            Some("tool_calls")
        } else if next.byte_count >= self.settings.archive_bytes {
            Some("bytes")
        } else {
            None
        };
        match reason {
            Some(reason) => {
                let archived = self
                    .archive(department, &key, &buffer, reason, None)
                    .await?;
                self.buffers.clear(department, &key).await?;
                Ok(AddResult {
                    status: "archived",
                    archived: Some(archived),
                })
            }
            None => {
                self.buffers.save(department, &key, &buffer, &next).await?;
                Ok(AddResult {
                    status: "ok",
                    archived: None,
                })
            }
        }
    }

    pub async fn force_archive(
        &self,
        department: &DepartmentId,
        key: BufferKey,
        reason: Option<String>,
    ) -> DomainResult<Value> {
        self.require_enabled()?;
        validate_key(&key)?;
        let (buffer, _) = self.buffers.load(department, &key).await?;
        if buffer.is_empty() {
            return Ok(json!({"status": "empty", "message": "buffer is empty"}));
        }
        let archived = self
            .archive(department, &key, &buffer, "force", reason)
            .await?;
        self.buffers.clear(department, &key).await?;
        Ok(
            json!({"status": "archived", "task_id": archived.task_id, "archived_at_ms": archived.archived_at_ms, "archive_key": archived.archive_key}),
        )
    }

    /// Archives a one-off conversation slice directly (no buffer).
    pub async fn extract(
        &self,
        department: &DepartmentId,
        who: &SkillIdentity,
        session_id: Option<String>,
        messages: Vec<BufferedMessage>,
        reason: Option<String>,
    ) -> DomainResult<Archived> {
        self.require_enabled()?;
        let key = BufferKey {
            user_id: who.user_id.clone().unwrap_or_default(),
            team_id: who.team_id.clone().unwrap_or_default(),
            agent_id: who.agent_id.clone().unwrap_or_default(),
            session_id: session_id.unwrap_or_else(|| format!("sx-{}", self.ids.hex(8))),
        };
        validate_key(&key)?;
        let messages = without_nul_messages(messages);
        validate_messages(&messages)?;
        let messages = if byte_size(&messages) >= self.settings.archive_bytes * 2 {
            self.trim_middle(self.compress_tools(messages))
        } else {
            messages
        };
        self.archive(department, &key, &messages, "direct", reason)
            .await
    }

    async fn archive(
        &self,
        department: &DepartmentId,
        key: &BufferKey,
        messages: &[BufferedMessage],
        reason: &str,
        hint: Option<String>,
    ) -> DomainResult<Archived> {
        let at = self.clock.now().timestamp_millis();
        let archive_key = self.buffers.archive(department, key, at, messages).await?;
        let task = SkillTask {
            task_id: format!("skill-extract-task-{}", self.ids.hex(8)),
            key: key.clone(),
            archive_key: archive_key.clone(),
            archived_at_ms: at,
            reason: reason.to_owned(),
            hint,
            status: "queued".into(),
            attempts: 0,
            error: None,
            candidates: json!([]),
        };
        self.buffers.put_task(department, &task).await?;
        self.pipeline
            .enqueue_job(PipelineJob::SkillExtract {
                department: department.clone(),
                task_id: task.task_id.clone(),
            })
            .await?;
        Ok(Archived {
            task_id: task.task_id,
            archived_at_ms: at,
            archive_key,
            reason: reason.to_owned(),
        })
    }

    fn compress_tools(&self, messages: Vec<BufferedMessage>) -> Vec<BufferedMessage> {
        let threshold = self.settings.tool_content_threshold;
        let keep = self.settings.tool_keep_bytes;
        messages
            .into_iter()
            .map(|mut m| {
                if (m.role == "tool_call" || m.role == "tool_result") && m.content.len() > threshold
                {
                    m.content = format!(
                        "{}{COMPRESSED_MARKER}{}",
                        head_bytes(&m.content, keep),
                        tail_bytes(&m.content, keep)
                    );
                }
                m
            })
            .collect()
    }

    /// Keeps whole messages from both ends up to `archive_bytes` each and
    /// replaces the middle with a single system note.
    fn trim_middle(&self, messages: Vec<BufferedMessage>) -> Vec<BufferedMessage> {
        let budget = self.settings.archive_bytes;
        let sizes: Vec<usize> = messages.iter().map(message_bytes).collect();
        let mut head = 0;
        let mut used = 0;
        while head < messages.len() && (head == 0 || used + sizes[head] <= budget) {
            used += sizes[head];
            head += 1;
        }
        let mut tail_start = messages.len();
        used = 0;
        while tail_start > head
            && (tail_start == messages.len() || used + sizes[tail_start - 1] <= budget)
        {
            tail_start -= 1;
            used += sizes[tail_start];
        }
        if tail_start <= head {
            return messages;
        }
        let dropped = tail_start - head;
        let dropped_bytes: usize = sizes[head..tail_start].iter().sum();
        let mut out: Vec<BufferedMessage> = messages[..head].to_vec();
        out.push(BufferedMessage {
            role: "system".into(),
            content: format!("[{dropped} middle messages / {dropped_bytes} bytes omitted]"),
            tool_name: None,
            tool_call_id: None,
            timestamp: None,
        });
        out.extend_from_slice(&messages[tail_start..]);
        out
    }

    async fn run(&self, department: &DepartmentId, task: &mut SkillTask) -> DomainResult<()> {
        let Some(messages) = self
            .buffers
            .read_archive(department, &task.archive_key)
            .await?
        else {
            warn!(task = %task.task_id, "archive missing; dropping ghost task");
            task.status = "dropped".into();
            return Ok(());
        };
        let who = SkillIdentity {
            team_id: Some(task.key.team_id.clone()),
            agent_id: Some(task.key.agent_id.clone()),
            user_id: Some(task.key.user_id.clone()),
            task_id: None,
        };
        let query = ListQuery {
            owner_agent_id: who.agent_id.clone(),
            limit: Some(self.settings.prefix_skills_limit),
            ..Default::default()
        };
        let (existing, total) = self.skills.list(department, &who, query).await?;
        let mut prompt = format!(
            "## Skills this agent already owns ({} of {total})\n",
            existing.len()
        );
        for s in &existing {
            let desc: String = s.description.chars().take(100).collect();
            prompt.push_str(&format!(
                "- {} ({}, v{}) — {desc}\n",
                s.name, s.skill_id, s.version
            ));
        }
        if let Some(hint) = &task.hint {
            prompt = format!("## Extraction hint from the main agent\n{hint}\n\n{prompt}");
        }
        prompt.push_str("\n---\n\n");
        prompt.push_str(&transcript(&messages));

        let reply = self
            .llm
            .complete(&[
                ChatMessage::system(SKILL_REVIEW_PROMPT),
                ChatMessage::user(prompt),
            ])
            .await?;
        let plan: ReviewPlan = parse_llm_json(&reply, '{')?;
        let mut applied = Vec::new();
        for action in plan.actions {
            match self.apply(department, &who, action).await {
                Ok(v) => applied.push(v),
                Err(err) => warn!(error = %err, task = %task.task_id, "skill action rejected"),
            }
        }
        info!(task = %task.task_id, applied = applied.len(), summary = %plan.summary, "skill extraction finished");
        task.candidates = Value::Array(applied);
        task.status = "succeeded".into();
        Ok(())
    }

    async fn apply(
        &self,
        department: &DepartmentId,
        who: &SkillIdentity,
        action: ReviewAction,
    ) -> DomainResult<Value> {
        let current = |id: &str| {
            let id = id.to_owned();
            let read = ReadOptions {
                include_content: false,
                include_manifest: false,
                ..Default::default()
            };
            async move {
                self.skills
                    .get(department, who, &id, read)
                    .await
                    .map(|d| d.summary.version)
            }
        };
        let summary = match action {
            ReviewAction::Create { name, content } => {
                let s = self
                    .skills
                    .create(department, who, &name, &content, Vec::new(), None)
                    .await?;
                return Ok(
                    json!({"action": "create", "skill_id": s.skill_id, "version": s.version}),
                );
            }
            ReviewAction::Update { skill_id, content } => {
                let v = current(&skill_id).await?;
                self.skills
                    .update(department, who, &skill_id, v, &content)
                    .await?
            }
            ReviewAction::Patch {
                skill_id,
                old_string,
                new_string,
            } => {
                let v = current(&skill_id).await?;
                self.skills
                    .patch(
                        department,
                        who,
                        &skill_id,
                        v,
                        &StringPatch {
                            old: old_string,
                            new: new_string,
                            replace_all: false,
                        },
                    )
                    .await?
            }
        };
        Ok(json!({"action": "update", "skill_id": summary.skill_id, "version": summary.version}))
    }
}

#[async_trait]
impl SkillJobRunner for SkillExtractionService {
    async fn run_skill_task(&self, department: &DepartmentId, task_id: &str) -> DomainResult<()> {
        if !self.llm.is_configured() {
            warn!("LLM not configured; skipping skill extraction");
            return Ok(());
        }
        let Some(mut task) = self.buffers.get_task(department, task_id).await? else {
            return Ok(());
        };
        task.attempts += 1;
        let result = self.run(department, &mut task).await;
        if let Err(err) = &result {
            task.status = "failed".into();
            task.error = Some(err.to_string());
        }
        self.buffers.put_task(department, &task).await?;
        result
    }
}

fn validate_key(key: &BufferKey) -> DomainResult<()> {
    for (field, value) in [
        ("user_id", &key.user_id),
        ("team_id", &key.team_id),
        ("agent_id", &key.agent_id),
        ("session_id", &key.session_id),
    ] {
        if value.is_empty() || value.contains('|') {
            return Err(DomainError::new(
                memory_domain::error::ErrorKind::InvalidInput,
                "INVALID_FRONTMATTER",
                format!("{field} must be non-empty and must not contain '|'"),
            ));
        }
    }
    Ok(())
}

/// See `memory_domain::memory::without_nul`: tool output is where U+0000
/// shows up in practice (a command printing binary data).
fn without_nul_messages(messages: Vec<BufferedMessage>) -> Vec<BufferedMessage> {
    messages
        .into_iter()
        .map(|mut m| {
            m.content = without_nul(m.content);
            m.tool_name = m.tool_name.map(without_nul);
            m
        })
        .collect()
}

fn validate_messages(messages: &[BufferedMessage]) -> DomainResult<()> {
    if messages.is_empty() || messages.len() > 500 {
        return Err(DomainError::invalid("messages must contain 1-500 items"));
    }
    for m in messages {
        if !ROLES.contains(&m.role.as_str()) {
            return Err(DomainError::invalid(format!("invalid role: {}", m.role)));
        }
        if (m.role == "tool_call" || m.role == "tool_result")
            && m.tool_call_id.as_deref().is_none_or(str::is_empty)
        {
            return Err(DomainError::invalid("tool messages require tool_call_id"));
        }
    }
    Ok(())
}

fn message_bytes(m: &BufferedMessage) -> usize {
    serde_json::to_vec(m)
        .map(|v| v.len())
        .unwrap_or(m.content.len())
}

fn byte_size(messages: &[BufferedMessage]) -> usize {
    messages.iter().map(message_bytes).sum()
}

fn head_bytes(s: &str, n: usize) -> &str {
    let mut end = n.min(s.len());
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

fn tail_bytes(s: &str, n: usize) -> &str {
    let mut start = s.len().saturating_sub(n);
    while !s.is_char_boundary(start) {
        start += 1;
    }
    &s[start..]
}

fn transcript(messages: &[BufferedMessage]) -> String {
    let body: Vec<String> = messages
        .iter()
        .map(|m| format!("<<past-{}>>\n{}", m.role, m.content))
        .collect();
    format!(
        "{}\n\n<<end-of-transcript>>\nAbove is the past conversation to review. Now decide, and respond only per the output contract.",
        body.join("\n\n")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_slicing_respects_char_boundaries() {
        let s = "héllo wörld";
        assert!(head_bytes(s, 2).len() <= 2);
        assert!(tail_bytes(s, 4).len() <= 4);
    }

    #[test]
    fn rejects_pipes_in_identity() {
        let key = BufferKey {
            user_id: "a|b".into(),
            team_id: "t".into(),
            agent_id: "a".into(),
            session_id: "s".into(),
        };
        assert!(validate_key(&key).is_err());
    }
}
