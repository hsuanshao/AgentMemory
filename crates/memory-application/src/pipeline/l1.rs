//! L0 → L1: extract atoms from new messages, reconcile them with existing
//! atoms (store / update / merge / skip) and persist them.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use memory_redact::Redactor;

use memory_domain::error::DomainResult;
use memory_domain::memory::{AtomType, AtomicMemory, ConversationMessage};
use memory_domain::retrieval::SearchMode;
use memory_domain::scope::{DepartmentId, MemoryScope};
use serde::Deserialize;
use serde_json::Value;
use tracing::warn;

use super::llm_json::parse_llm_json;
use super::prompts::{self, CustomStrategy};
use crate::common::Page;
use crate::memory::search::AtomSearcher;
use crate::ports::memory::{AtomFilter, AtomRepository, AtomSelector};
use crate::ports::system::{ChatMessage, Clock, EmbeddingProvider, IdGenerator, LlmClient};

const DEDUP_CANDIDATES_PER_MEMORY: usize = 5;
/// The session's newest episodic atoms shown to extraction, so a later turn
/// can mark one out of date (#43), and the most of their text it may hold.
const SESSION_ATOMS_SHOWN: usize = 10;
const SESSION_ATOMS_MAX_CHARS: usize = 3000;

#[derive(Debug, Deserialize)]
struct SceneBlock {
    #[serde(default)]
    scene_name: String,
    #[serde(default)]
    memories: Vec<RawMemory>,
}

#[derive(Debug, Deserialize)]
struct RawMemory {
    #[serde(default)]
    content: String,
    #[serde(default, rename = "type")]
    kind: String,
    #[serde(default)]
    priority: Value,
    #[serde(default)]
    source_message_ids: Vec<String>,
    /// Ids of the session's atoms this memory makes out of date.
    #[serde(default)]
    supersedes: Vec<String>,
}

/// A new atom, and the atoms it replaces (only ids extraction was shown).
struct Candidate {
    atom: AtomicMemory,
    supersedes: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct DedupDecision {
    record_id: String,
    action: String,
    #[serde(default)]
    target_ids: Vec<String>,
    merged_content: Option<String>,
    merged_type: Option<String>,
    merged_priority: Option<Value>,
}

pub struct L1Outcome {
    /// Atoms written or revised by this run.
    pub written: Vec<AtomicMemory>,
    pub last_scene: Option<String>,
    pub system_prompt: String,
    /// False when no message was worth sending to the LLM.
    pub llm_called: bool,
}

pub struct L1Extractor {
    redactor: Arc<Redactor>,
    llm: Arc<dyn LlmClient>,
    atoms: Arc<dyn AtomRepository>,
    embedder: Arc<dyn EmbeddingProvider>,
    searcher: Arc<AtomSearcher>,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGenerator>,
    enable_dedup: bool,
    max_memories: usize,
}

impl L1Extractor {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        redactor: Arc<Redactor>,
        llm: Arc<dyn LlmClient>,
        atoms: Arc<dyn AtomRepository>,
        embedder: Arc<dyn EmbeddingProvider>,
        searcher: Arc<AtomSearcher>,
        clock: Arc<dyn Clock>,
        ids: Arc<dyn IdGenerator>,
        enable_dedup: bool,
        max_memories: usize,
    ) -> Self {
        Self {
            redactor,
            llm,
            atoms,
            embedder,
            searcher,
            clock,
            ids,
            enable_dedup,
            max_memories,
        }
    }

    pub async fn extract(
        &self,
        department: &DepartmentId,
        scope: &MemoryScope,
        session_id: &str,
        messages: &[ConversationMessage],
        previous_scene: Option<&str>,
        strategy: Option<&CustomStrategy>,
    ) -> DomainResult<L1Outcome> {
        let system_prompt = prompts::compose(memory_domain::memory::Layer::L1, strategy);
        let worth: Vec<&ConversationMessage> = messages
            .iter()
            .filter(|m| worth_extracting(&m.content))
            .collect();
        if worth.is_empty() {
            return Ok(L1Outcome {
                written: Vec::new(),
                last_scene: previous_scene.map(Into::into),
                system_prompt,
                llm_called: false,
            });
        }
        let shown = self.session_atoms(department, scope, session_id).await?;
        let reply = self
            .llm
            .complete(&[
                ChatMessage::system(&system_prompt),
                ChatMessage::user(extraction_input(&worth, previous_scene, &shown)),
            ])
            .await?;
        let blocks: Vec<SceneBlock> = parse_llm_json(&reply, '[')?;
        let last_scene = blocks
            .iter()
            .rev()
            .map(|b| b.scene_name.trim())
            .find(|s| !s.is_empty())
            .map(Into::into);
        let shown: HashSet<&str> = shown.iter().map(|a| a.id.as_str()).collect();
        let candidates = self.to_atoms(scope, session_id, blocks, &shown);
        let written = self
            .reconcile_and_store(department, scope, candidates)
            .await?;
        Ok(L1Outcome {
            written,
            last_scene: last_scene.or(previous_scene.map(Into::into)),
            system_prompt,
            llm_called: true,
        })
    }

    /// The session's newest episodic atoms, within `SESSION_ATOMS_MAX_CHARS`.
    async fn session_atoms(
        &self,
        department: &DepartmentId,
        scope: &MemoryScope,
        session_id: &str,
    ) -> DomainResult<Vec<AtomicMemory>> {
        let filter = AtomFilter {
            atom_type: Some(AtomType::Episodic),
            session_id: Some(session_id.to_owned()),
            ..Default::default()
        };
        let page = Page {
            limit: SESSION_ATOMS_SHOWN,
            offset: 0,
        };
        let (newest, _) = self
            .atoms
            .query(department, AtomSelector::Scope(scope), &filter, page)
            .await?;
        Ok(within_budget(newest, SESSION_ATOMS_MAX_CHARS))
    }

    fn to_atoms(
        &self,
        scope: &MemoryScope,
        session_id: &str,
        blocks: Vec<SceneBlock>,
        shown: &HashSet<&str>,
    ) -> Vec<Candidate> {
        let now = self.clock.now();
        // An atom is replaced once; a later memory naming it again is stored
        // as new.
        let mut claimed: HashSet<String> = HashSet::new();
        blocks
            .into_iter()
            .flat_map(|b| {
                let scene = b.scene_name;
                b.memories.into_iter().map(move |m| (scene.clone(), m))
            })
            .filter_map(|(scene, m)| {
                let atom_type = AtomType::parse_lenient(&m.kind)?;
                let priority = parse_priority(&m.priority);
                let keep = priority == -1 || priority >= atom_type.min_priority();
                (keep && !m.content.trim().is_empty()).then_some((scene, m, atom_type, priority))
            })
            .take(self.max_memories)
            .map(|(scene, m, atom_type, priority)| {
                let supersedes = m
                    .supersedes
                    .into_iter()
                    .filter(|id| shown.contains(id.as_str()) && claimed.insert(id.clone()))
                    .collect();
                Candidate {
                    atom: AtomicMemory {
                        id: format!("m_{}_{}", now.timestamp_millis(), self.ids.hex(8)),
                        scope: scope.clone(),
                        atom_type,
                        content: m.content.trim().to_owned(),
                        background: (!scene.trim().is_empty()).then(|| scene.trim().to_owned()),
                        priority,
                        source_message_ids: m.source_message_ids,
                        session_id: Some(session_id.to_owned()),
                        version: 1,
                        created_at: now,
                        updated_at: now,
                    },
                    supersedes,
                }
            })
            .collect()
    }

    /// A memory that supersedes atoms revises them like a dedup `update`;
    /// the rest go through dedup.
    async fn reconcile_and_store(
        &self,
        department: &DepartmentId,
        scope: &MemoryScope,
        candidates: Vec<Candidate>,
    ) -> DomainResult<Vec<AtomicMemory>> {
        let mut written = Vec::new();
        let mut fresh = Vec::new();
        for c in candidates {
            if c.supersedes.is_empty() {
                fresh.push(c.atom);
                continue;
            }
            let update = DedupDecision {
                record_id: c.atom.id.clone(),
                action: "update".into(),
                target_ids: c.supersedes,
                merged_content: None,
                merged_type: None,
                merged_priority: None,
            };
            if let Some(revised) = self
                .merge_into_targets(department, scope, &c.atom, &update)
                .await?
            {
                written.push(revised);
            }
        }
        if fresh.is_empty() {
            return Ok(written);
        }
        let decisions = if self.enable_dedup {
            self.dedup(department, scope, &fresh).await
        } else {
            HashMap::new()
        };
        for atom in fresh {
            match decisions.get(&atom.id) {
                Some(d) if d.action == "skip" => {}
                Some(d)
                    if (d.action == "update" || d.action == "merge")
                        && !d.target_ids.is_empty() =>
                {
                    if let Some(merged) =
                        self.merge_into_targets(department, scope, &atom, d).await?
                    {
                        written.push(merged);
                    }
                }
                _ => {
                    self.store(department, &atom).await?;
                    written.push(atom);
                }
            }
        }
        Ok(written)
    }

    /// Asks the LLM to reconcile the batch with similar existing atoms.
    /// Any failure degrades to "store everything".
    async fn dedup(
        &self,
        department: &DepartmentId,
        scope: &MemoryScope,
        fresh: &[AtomicMemory],
    ) -> HashMap<String, DedupDecision> {
        let mut pool: HashMap<String, AtomicMemory> = HashMap::new();
        let mut related: Vec<(String, Vec<String>)> = Vec::new();
        for atom in fresh {
            let hits = match self
                .searcher
                .search(
                    department,
                    scope,
                    &atom.content,
                    &AtomFilter::default(),
                    DEDUP_CANDIDATES_PER_MEMORY,
                    SearchMode::Hybrid,
                )
                .await
            {
                Ok(outcome) => outcome.hits,
                Err(err) => {
                    warn!(error = %err, "dedup candidate recall failed");
                    Vec::new()
                }
            };
            let ids: Vec<String> = hits.iter().map(|h| h.item.id.clone()).collect();
            for h in hits {
                pool.entry(h.item.id.clone()).or_insert(h.item);
            }
            related.push((atom.id.clone(), ids));
        }
        if pool.is_empty() {
            return HashMap::new();
        }
        let reply = self
            .llm
            .complete(&[
                ChatMessage::system(prompts::L1_DEDUP),
                ChatMessage::user(dedup_input(&pool, fresh, &related)),
            ])
            .await;
        let decisions: Vec<DedupDecision> = match reply.and_then(|r| parse_llm_json(&r, '[')) {
            Ok(d) => d,
            Err(err) => {
                warn!(error = %err, "dedup failed; storing all new memories");
                return HashMap::new();
            }
        };
        let known: HashSet<&str> = pool.keys().map(String::as_str).collect();
        decisions
            .into_iter()
            .map(|mut d| {
                d.target_ids.retain(|t| known.contains(t.as_str()));
                (d.record_id.clone(), d)
            })
            .collect()
    }

    /// Revises the first target in place (keeping its id stable for API
    /// clients) and deletes the remaining targets.
    async fn merge_into_targets(
        &self,
        department: &DepartmentId,
        scope: &MemoryScope,
        fresh: &AtomicMemory,
        d: &DedupDecision,
    ) -> DomainResult<Option<AtomicMemory>> {
        let mut targets = Vec::new();
        for id in &d.target_ids {
            if let Some(t) = self.atoms.get(department, id).await? {
                targets.push(t);
            }
        }
        let Some(first) = targets.first().cloned() else {
            self.store(department, fresh).await?;
            return Ok(Some(fresh.clone()));
        };
        let mut merged = first;
        merged.content = d
            .merged_content
            .clone()
            .filter(|c| !c.trim().is_empty())
            .unwrap_or_else(|| fresh.content.clone());
        merged.atom_type = d
            .merged_type
            .as_deref()
            .and_then(AtomType::parse_lenient)
            .unwrap_or(fresh.atom_type);
        merged.priority = d
            .merged_priority
            .as_ref()
            .map(parse_priority)
            .unwrap_or(fresh.priority);
        merged.version = targets.iter().map(|t| t.version).max().unwrap_or(1) + 1;
        merged.updated_at = self.clock.now();
        merged
            .source_message_ids
            .extend(fresh.source_message_ids.iter().cloned());
        let others: Vec<String> = targets.iter().skip(1).map(|t| t.id.clone()).collect();
        if !others.is_empty() {
            self.atoms.delete(department, scope, &others).await?;
        }
        self.store(department, &merged).await?;
        Ok(Some(merged))
    }

    /// Stores an atom, secrets redacted: the input was redacted when it was
    /// captured, but the model may still spell one out.
    async fn store(&self, department: &DepartmentId, atom: &AtomicMemory) -> DomainResult<()> {
        let mut redacted = atom.clone();
        redacted.content = self.redactor.redact(&atom.content);
        redacted.background = atom.background.as_deref().map(|b| self.redactor.redact(b));
        let atom = &redacted;
        let embedding = match self.embedder.dimensions() {
            Some(_) => match self
                .embedder
                .embed(std::slice::from_ref(&atom.content))
                .await
            {
                Ok(mut v) if !v.is_empty() => Some(v.remove(0)),
                Ok(_) => None,
                Err(err) => {
                    warn!(error = %err, "embedding failed; storing atom without vector");
                    None
                }
            },
            None => None,
        };
        self.atoms
            .upsert(department, atom, embedding.as_deref())
            .await
    }
}

/// Drops messages made only of a few symbols or question marks.
fn worth_extracting(content: &str) -> bool {
    let trimmed = content.trim();
    let only_symbols = trimmed.chars().count() <= 5 && !trimmed.chars().any(char::is_alphanumeric);
    !(trimmed.is_empty() || only_symbols || trimmed.chars().all(|c| c == '?' || c == '？'))
}

fn parse_priority(v: &Value) -> i32 {
    let n = match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    };
    match n {
        Some(-1.0) => -1,
        Some(p) if (0.0..=100.0).contains(&p) => p.round() as i32,
        _ => 50,
    }
}

/// The leading atoms (newest first) whose text fits in `max_chars`.
fn within_budget(newest: Vec<AtomicMemory>, max_chars: usize) -> Vec<AtomicMemory> {
    let mut used = 0;
    newest
        .into_iter()
        .take_while(|a| {
            used += a.content.chars().count();
            used <= max_chars
        })
        .collect()
}

fn extraction_input(
    messages: &[&ConversationMessage],
    previous_scene: Option<&str>,
    session_atoms: &[AtomicMemory],
) -> String {
    let mut out = format!("[Previous scene]: {}\n\n", previous_scene.unwrap_or("none"));
    if !session_atoms.is_empty() {
        out.push_str("[Existing memories from this session]\n");
        for a in session_atoms {
            out.push_str(&format!(
                "- record_id={} updated_at={}: {}\n",
                a.id,
                a.updated_at.to_rfc3339(),
                a.content
            ));
        }
        out.push('\n');
    }
    out.push_str("[New messages to extract]\n");
    for m in messages {
        let role = match m.role {
            memory_domain::memory::Role::User => "user",
            memory_domain::memory::Role::Assistant => "assistant",
        };
        out.push_str(&format!(
            "[{}] [{role}] [{}]: {}\n",
            m.id,
            m.timestamp.to_rfc3339(),
            m.content
        ));
    }
    out
}

fn dedup_input(
    pool: &HashMap<String, AtomicMemory>,
    fresh: &[AtomicMemory],
    related: &[(String, Vec<String>)],
) -> String {
    let pool_json: Vec<Value> = pool
        .values()
        .map(|a| {
            serde_json::json!({
                "record_id": a.id, "content": a.content, "type": a.atom_type.as_str(),
                "priority": a.priority, "scene_name": a.background, "updated_at": a.updated_at.to_rfc3339(),
            })
        })
        .collect();
    let mut out = format!(
        "[Candidate pool]\n{}\n\n[New memories]\n",
        serde_json::to_string_pretty(&pool_json).unwrap_or_default()
    );
    for atom in fresh {
        let ids = related
            .iter()
            .find(|(id, _)| id == &atom.id)
            .map(|(_, c)| c.join(", "))
            .unwrap_or_default();
        out.push_str(&format!(
            "- record_id={} type={} priority={}: {}\n  [Related candidate ids]: {}\n",
            atom.id,
            atom.atom_type.as_str(),
            atom.priority,
            atom.content,
            if ids.is_empty() { "none".into() } else { ids }
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn quality_gate_drops_noise() {
        assert!(!worth_extracting("??"));
        assert!(!worth_extracting("..."));
        assert!(worth_extracting("ok"));
        assert!(worth_extracting("我喜歡用 Rust"));
    }

    fn episodic(id: &str, content: &str) -> AtomicMemory {
        let now = chrono::Utc::now();
        AtomicMemory {
            id: id.into(),
            scope: MemoryScope::from_optional(None, None, None, None).unwrap(),
            atom_type: AtomType::Episodic,
            content: content.into(),
            background: None,
            priority: 80,
            source_message_ids: vec![],
            session_id: Some("s1".into()),
            version: 1,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn session_atoms_stop_at_the_character_budget() {
        let newest = vec![
            episodic("a", "12345"),
            episodic("b", "67890"),
            episodic("c", "x"),
        ];
        let ids = |v: Vec<AtomicMemory>| v.into_iter().map(|a| a.id).collect::<Vec<_>>();
        assert_eq!(ids(within_budget(newest.clone(), 10)), ["a", "b"]);
        assert_eq!(ids(within_budget(newest.clone(), 4)), Vec::<String>::new());
        assert_eq!(ids(within_budget(newest, 100)), ["a", "b", "c"]);
    }

    #[test]
    fn extraction_input_lists_session_atoms_only_when_there_are_some() {
        assert_eq!(
            extraction_input(&[], Some("scene"), &[]),
            "[Previous scene]: scene\n\n[New messages to extract]\n"
        );
        let input = extraction_input(&[], None, &[episodic("m_1", "The pilot is in progress")]);
        assert!(input.contains("[Existing memories from this session]\n- record_id=m_1 "));
        assert!(input.ends_with(": The pilot is in progress\n\n[New messages to extract]\n"));
    }

    #[test]
    fn priorities_are_repaired() {
        assert_eq!(parse_priority(&json!(85)), 85);
        assert_eq!(parse_priority(&json!("-1")), -1);
        assert_eq!(parse_priority(&json!(250)), 50);
        assert_eq!(parse_priority(&json!(null)), 50);
    }
}
