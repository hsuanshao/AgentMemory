//! L1 → L2: consolidate new atoms into a bounded set of scene files.

use std::collections::HashMap;
use std::sync::Arc;

use memory_domain::error::DomainResult;
use memory_domain::memory::{AtomicMemory, Layer};
use memory_domain::pipeline::scene_capacity_guidance;
use memory_domain::profile::{
    is_tombstone, normalize_scene_filename, render_scene, scene_path, ProfileScope, SceneMeta,
    MAX_SCENE_CHARS,
};
use memory_domain::scope::DepartmentId;
use serde::Deserialize;
use tracing::warn;

use super::llm_json::parse_llm_json;
use super::prompts::{self, CustomStrategy};
use crate::memory::service::{MemoryService, SceneEntry};
use crate::ports::memory::ProfileFileRepository;
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
    profiles: Arc<dyn ProfileFileRepository>,
    memory: Arc<MemoryService>,
    clock: Arc<dyn Clock>,
    max_scenes: usize,
}

impl SceneExtractor {
    pub fn new(
        llm: Arc<dyn LlmClient>,
        profiles: Arc<dyn ProfileFileRepository>,
        memory: Arc<MemoryService>,
        clock: Arc<dyn Clock>,
        max_scenes: usize,
    ) -> Self {
        Self {
            llm,
            profiles,
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
        self.memory.refresh_navigation(department, profile).await?;
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
        let mut summary = format!(
            "**Current scene count: {} / {}**\n{}\n\n",
            scenes.len(),
            self.max_scenes,
            scene_capacity_guidance(scenes.len(), self.max_scenes)
        );
        for s in scenes {
            let meta = s.meta.clone().unwrap_or_default();
            let (_, body) = memory_domain::profile::parse_scene(&s.file.content);
            summary.push_str(&format!(
                "### {}\nheat: {} | updated: {}\nsummary: {}\n{}\n\n",
                s.relative_path, meta.heat, meta.updated, meta.summary, body
            ));
        }
        format!(
            "### 1. New memories\n{}\n\n### 2. Existing scenes (full content)\n{}\n### 3. Current time\n{}",
            serde_json::to_string_pretty(&memories).unwrap_or_default(),
            summary,
            self.clock.now().to_rfc3339()
        )
    }

    async fn apply(
        &self,
        department: &DepartmentId,
        profile: &ProfileScope,
        scenes: &[SceneEntry],
        operations: Vec<SceneOperation>,
    ) -> DomainResult<Vec<String>> {
        let mut heats: HashMap<String, SceneMeta> = scenes
            .iter()
            .map(|s| (s.relative_path.clone(), s.meta.clone().unwrap_or_default()))
            .collect();
        let mut created = 0usize;
        let mut changed = Vec::new();
        for op in operations {
            let Ok(file) = normalize_scene_filename(&op.file) else {
                warn!(file = %op.file, "skipping scene operation with invalid filename");
                continue;
            };
            let exists = heats.contains_key(&file);
            let is_create = !exists && op.action != "merge";
            if is_create && (created >= 1 || heats.len() >= self.max_scenes) {
                warn!(file = %file, "scene cap reached; dropping CREATE");
                continue;
            }
            if is_tombstone(&op.content) {
                continue;
            }
            let merged_away: Vec<String> = op
                .merge_from
                .iter()
                .filter_map(|f| normalize_scene_filename(f).ok())
                .filter(|f| f != &file && heats.contains_key(f))
                .collect();
            let heat = match (op.action.as_str(), heats.get(&file)) {
                ("merge", own) => {
                    merged_away
                        .iter()
                        .filter_map(|f| heats.get(f))
                        .map(|m| m.heat)
                        .sum::<u32>()
                        + own.map_or(0, |m| m.heat)
                        + 1
                }
                (_, Some(m)) => m.heat + 1,
                (_, None) => 1,
            };
            let now = self.clock.now().to_rfc3339();
            let meta = SceneMeta {
                created: heats
                    .get(&file)
                    .map(|m| m.created.clone())
                    .filter(|c| !c.is_empty())
                    .unwrap_or_else(|| now.clone()),
                updated: now,
                summary: op.summary.trim().to_owned(),
                heat,
                extra: Default::default(),
            };
            let body: String = op.content.chars().take(MAX_SCENE_CHARS).collect();
            self.profiles
                .write(
                    department,
                    profile,
                    &scene_path(&file),
                    &render_scene(&meta, &body),
                )
                .await?;
            for old in &merged_away {
                self.profiles
                    .remove(department, profile, &scene_path(old))
                    .await?;
                heats.remove(old);
            }
            if is_create {
                created += 1;
            }
            heats.insert(file.clone(), meta);
            changed.push(file);
        }
        Ok(changed)
    }
}
