//! L1 → L2: consolidate new atoms into a bounded set of scene files.

use std::collections::HashMap;
use std::sync::Arc;

use memory_domain::error::DomainResult;
use memory_domain::memory::{AtomicMemory, Layer};
use memory_domain::pipeline::scene_capacity_guidance;
use memory_domain::profile::{
    is_tombstone, normalize_scene_filename, NewSceneVersion, ProfileScope, SceneOp, SceneRef,
    SceneVersion, MAX_SCENE_CHARS,
};
use memory_domain::scope::DepartmentId;
use serde::Deserialize;
use tracing::warn;

use super::llm_json::parse_llm_json;
use super::prompts::{self, CustomStrategy};
use crate::memory::service::{retirement, MemoryService, SceneEntry};
use crate::ports::system::{ChatMessage, Clock, LlmClient};

#[derive(Debug, Deserialize)]
struct ScenePlan {
    #[serde(default)]
    operations: Vec<SceneOperation>,
    #[serde(default)]
    persona_update_request: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SceneOperation {
    action: String,
    file: String,
    #[serde(default)]
    merge_from: Vec<String>,
    #[serde(default)]
    summary: String,
    #[serde(default)]
    content: String,
}

pub struct L2Outcome {
    pub changed_files: Vec<String>,
    pub persona_update_request: Option<String>,
    pub system_prompt: String,
}

pub struct SceneExtractor {
    llm: Arc<dyn LlmClient>,
    memory: Arc<MemoryService>,
    clock: Arc<dyn Clock>,
    max_scenes: usize,
}

impl SceneExtractor {
    pub fn new(
        llm: Arc<dyn LlmClient>,
        memory: Arc<MemoryService>,
        clock: Arc<dyn Clock>,
        max_scenes: usize,
    ) -> Self {
        Self {
            llm,
            memory,
            clock,
            max_scenes,
        }
    }

    pub async fn consolidate(
        &self,
        department: &DepartmentId,
        profile: &ProfileScope,
        atoms: &[AtomicMemory],
        strategy: Option<&CustomStrategy>,
    ) -> DomainResult<L2Outcome> {
        let system_prompt = prompts::compose(Layer::L2, strategy);
        let scenes = self.memory.list_scenes(department, profile, "").await?;
        let input = self.plan_input(atoms, &scenes);
        let reply = self
            .llm
            .complete(&[
                ChatMessage::system(&system_prompt),
                ChatMessage::user(input),
            ])
            .await?;
        let plan: ScenePlan = parse_llm_json(&reply, '{')?;
        let changed = self
            .apply(department, profile, &scenes, plan.operations)
            .await?;
        Ok(L2Outcome {
            changed_files: changed,
            persona_update_request: plan.persona_update_request.filter(|r| !r.trim().is_empty()),
            system_prompt,
        })
    }

    fn plan_input(&self, atoms: &[AtomicMemory], scenes: &[SceneEntry]) -> String {
        let memories: Vec<serde_json::Value> = atoms
            .iter()
            .map(|a| serde_json::json!({"id": a.id, "type": a.atom_type.as_str(), "content": a.content, "created_at": a.created_at.to_rfc3339()}))
            .collect();
        let existing: Vec<serde_json::Value> = scenes
            .iter()
            .map(|s| {
                let meta = s.meta.clone().unwrap_or_default();
                let (_, body) = memory_domain::profile::parse_scene(&s.file.content);
                serde_json::json!({
                    "file": s.relative_path, "heat": meta.heat, "updated": meta.updated,
                    "summary": meta.summary, "body": body,
                })
            })
            .collect();
        prompts::data_input(
            &serde_json::json!({
                "new_memories": memories,
                "existing_scenes": existing,
                "scene_count": scenes.len(),
                "max_scenes": self.max_scenes,
                "current_time": self.clock.now().to_rfc3339(),
            }),
            &format!(
                "Scenes: {} of {}. {}\n\nRespond only per the output contract.",
                scenes.len(),
                self.max_scenes,
                scene_capacity_guidance(scenes.len(), self.max_scenes)
            ),
        )
    }

    async fn apply(
        &self,
        department: &DepartmentId,
        profile: &ProfileScope,
        scenes: &[SceneEntry],
        operations: Vec<SceneOperation>,
    ) -> DomainResult<Vec<String>> {
        let mut current: HashMap<String, SceneVersion> = scenes
            .iter()
            .map(|s| (s.relative_path.clone(), s.scene.clone()))
            .collect();
        let mut created = 0usize;
        let mut changed = Vec::new();
        for op in operations {
            let Ok(file) = normalize_scene_filename(&op.file) else {
                warn!(file = %op.file, "skipping scene operation with invalid filename");
                continue;
            };
            let own = current.get(&file).cloned();
            let is_create = own.is_none() && op.action != "merge";
            if is_create && (created >= 1 || current.len() >= self.max_scenes) {
                warn!(file = %file, "scene cap reached; dropping CREATE");
                continue;
            }
            if is_tombstone(&op.content) {
                continue;
            }
            let merged_away: Vec<SceneVersion> = op
                .merge_from
                .iter()
                .filter_map(|f| normalize_scene_filename(f).ok())
                .filter(|f| f != &file)
                .filter_map(|f| current.get(&f).cloned())
                .collect();
            let heat = match op.action.as_str() {
                "merge" => {
                    merged_away.iter().map(|m| m.heat).sum::<u32>()
                        + own.as_ref().map_or(0, |m| m.heat)
                        + 1
                }
                _ => own.as_ref().map_or(1, |m| m.heat + 1),
            };
            // The plan was made from the snapshot: write only on top of what
            // the LLM saw.
            let own_version = own.as_ref().map(|s| s.version);
            let scene_id = own
                .as_ref()
                .map_or_else(|| self.memory.new_scene_id(), |s| s.scene_id.clone());
            let mut versions = vec![NewSceneVersion {
                scene_id: scene_id.clone(),
                op: match (&own, merged_away.is_empty()) {
                    (_, false) => SceneOp::Merge,
                    (Some(_), true) => SceneOp::Update,
                    (None, true) => SceneOp::Create,
                },
                slug: file.clone(),
                summary: op.summary.trim().to_owned(),
                heat,
                body: op.content.chars().take(MAX_SCENE_CHARS).collect(),
                attrs: own.map(|s| s.attrs).unwrap_or_default(),
                parents: merged_away
                    .iter()
                    .map(|m| SceneRef {
                        scene_id: m.scene_id.clone(),
                        version: m.version,
                    })
                    .collect(),
                author: None,
                expected_version: own_version,
            }];
            versions.extend(
                merged_away
                    .iter()
                    .map(|m| retirement(m.clone(), Some(&scene_id), None)),
            );
            let written = match self
                .memory
                .append_scenes(department, profile, versions)
                .await
            {
                // Only an API write or removal changes scenes while L2 runs:
                // the user's version wins, and this operation is dropped.
                Err(err) if err.code == "scene_exists" || err.code == "scene_changed" => {
                    warn!(file = %file, error = %err, "scene changed by the user since the snapshot; skipping operation");
                    continue;
                }
                other => other?,
            };
            // Counted only once written: a skipped create does not use up
            // the batch's one new scene.
            if is_create {
                created += 1;
            }
            let mut written = written.into_iter();
            if let Some(target) = written.next() {
                current.insert(file.clone(), target);
            }
            for old in &merged_away {
                current.remove(&old.slug);
            }
            changed.push(file);
        }
        Ok(changed)
    }
}
