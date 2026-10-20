//! L0 → L1: extract atoms from new messages, reconcile them with existing
//! atoms (store / update / merge / skip) and persist them.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use memory_domain::error::DomainResult;
use memory_domain::memory::{AtomType, AtomicMemory, ConversationMessage};
use memory_domain::retrieval::SearchMode;
use memory_domain::scope::{InstanceId, MemoryScope};
use serde::Deserialize;
use serde_json::Value;
use tracing::warn;

use super::llm_json::parse_llm_json;
use super::prompts::{self, CustomStrategy};
use crate::memory::search::AtomSearcher;
use crate::ports::memory::{AtomFilter, AtomRepository};
use crate::ports::system::{ChatMessage, Clock, EmbeddingProvider, IdGenerator, LlmClient};

const DEDUP_CANDIDATES_PER_MEMORY: usize = 5;

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
        instance: &InstanceId,
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
        let reply = self
            .llm
            .complete(&[
                ChatMessage::system(&system_prompt),
                ChatMessage::user(extraction_input(&worth, previous_scene)),
            ])
            .await?;
        let blocks: Vec<SceneBlock> = parse_llm_json(&reply, '[')?;
        let last_scene = blocks
            .iter()
            .rev()
            .map(|b| b.scene_name.trim())
            .find(|s| !s.is_empty())
            .map(Into::into);
        let candidates = self.to_atoms(scope, session_id, blocks);
        let written = self
            .reconcile_and_store(instance, scope, candidates)
            .await?;
        Ok(L1Outcome {
            written,
            last_scene: last_scene.or(previous_scene.map(Into::into)),
            system_prompt,
            llm_called: true,
        })
    }

    fn to_atoms(
        &self,
        scope: &MemoryScope,
        session_id: &str,
        blocks: Vec<SceneBlock>,
    ) -> Vec<AtomicMemory> {
        let now = self.clock.now();
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
                (keep && !m.content.trim().is_empty()).then(|| AtomicMemory {
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
                })
            })
            .take(self.max_memories)
            .collect()
    }

    async fn reconcile_and_store(
        &self,
        instance: &InstanceId,
        scope: &MemoryScope,
        fresh: Vec<AtomicMemory>,
    ) -> DomainResult<Vec<AtomicMemory>> {
        if fresh.is_empty() {
            return Ok(Vec::new());
        }
        let decisions = if self.enable_dedup {
            self.dedup(instance, scope, &fresh).await
        } else {
            HashMap::new()
        };
        let mut written = Vec::new();
        for atom in fresh {
            match decisions.get(&atom.id) {
                Some(d) if d.action == "skip" => {}
                Some(d)
                    if (d.action == "update" || d.action == "merge")
                        && !d.target_ids.is_empty() =>
                {
                    if let Some(merged) = self.merge_into_targets(instance, scope, &atom, d).await?
                    {
                        written.push(merged);
                    }
                }
                _ => {
                    self.store(instance, &atom).await?;
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
        instance: &InstanceId,
        scope: &MemoryScope,
        fresh: &[AtomicMemory],
    ) -> HashMap<String, DedupDecision> {
        let mut pool: HashMap<String, AtomicMemory> = HashMap::new();
        let mut related: Vec<(String, Vec<String>)> = Vec::new();
        for atom in fresh {
            let hits = match self
                .searcher
                .search(
                    instance,
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
        instance: &InstanceId,
        scope: &MemoryScope,
        fresh: &AtomicMemory,
        d: &DedupDecision,
    ) -> DomainResult<Option<AtomicMemory>> {
        let mut targets = Vec::new();
        for id in &d.target_ids {
            if let Some(t) = self.atoms.get(instance, id).await? {
                targets.push(t);
            }
        }
        let Some(first) = targets.first().cloned() else {
            self.store(instance, fresh).await?;
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
            self.atoms.delete(instance, scope, &others).await?;
        }
        self.store(instance, &merged).await?;
        Ok(Some(merged))
    }

    async fn store(&self, instance: &InstanceId, atom: &AtomicMemory) -> DomainResult<()> {
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
            .upsert(instance, atom, embedding.as_deref())
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

fn extraction_input(messages: &[&ConversationMessage], previous_scene: Option<&str>) -> String {
    let mut out = format!(
        "[Previous scene]: {}\n\n[New messages to extract]\n",
        previous_scene.unwrap_or("none")
    );
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

    #[test]
    fn priorities_are_repaired() {
        assert_eq!(parse_priority(&json!(85)), 85);
        assert_eq!(parse_priority(&json!("-1")), -1);
        assert_eq!(parse_priority(&json!(250)), 50);
        assert_eq!(parse_priority(&json!(null)), 50);
    }
}
