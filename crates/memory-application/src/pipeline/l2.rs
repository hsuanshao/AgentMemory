//! L1 → L2: consolidate new atoms into a bounded set of scene files,
//! record which atoms each scene holds, and bring stale scenes up to date.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use memory_domain::error::DomainResult;
use memory_domain::memory::{AtomicMemory, Layer};
use memory_domain::pipeline::scene_capacity_guidance;
use memory_domain::profile::{
    is_tombstone, normalize_scene_filename, NewSceneVersion, ProfileScope, SceneMember, SceneOp,
    SceneRef, SceneVersion, SceneWriteLinks, StaleMark, StaleReason, MAX_SCENE_CHARS,
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
    /// Stale scenes the model read and found already right.
    #[serde(default)]
    unchanged: Vec<String>,
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
    /// Numbers (`n`) of the new memories this scene now states.
    #[serde(default)]
    atoms: Vec<u32>,
}

/// How many stale scenes one run is given, and after how many runs that
/// left a mark it is no longer given to L2 (it stays for a person).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StaleSettings {
    pub scenes_per_run: usize,
    pub max_attempts: u32,
}

impl Default for StaleSettings {
    fn default() -> Self {
        Self {
            scenes_per_run: 10,
            max_attempts: 3,
        }
    }
}

pub struct L2Outcome {
    pub changed_files: Vec<String>,
    pub persona_update_request: Option<String>,
    pub system_prompt: String,
    /// The stale marks this run was given.
    pub stale_given: Vec<i64>,
}

pub struct SceneExtractor {
    llm: Arc<dyn LlmClient>,
    memory: Arc<MemoryService>,
    clock: Arc<dyn Clock>,
    max_scenes: usize,
    stale: StaleSettings,
}

/// A stale scene as the model sees it, with the marks behind it.
struct StaleScene {
    file: String,
    marks: Vec<StaleMark>,
    reasons: Vec<serde_json::Value>,
}

impl SceneExtractor {
    pub fn new(
        llm: Arc<dyn LlmClient>,
        memory: Arc<MemoryService>,
        clock: Arc<dyn Clock>,
        max_scenes: usize,
        stale: StaleSettings,
    ) -> Self {
        Self {
            llm,
            memory,
            clock,
            max_scenes,
            stale,
        }
    }

    /// Whether the profile has stale scenes an L2 run would be given, even
    /// with no new atoms: the same marks [`Self::consolidate`] picks, so a
    /// mark L2 cannot be given never asks for a run.
    pub async fn has_stale_work(
        &self,
        department: &DepartmentId,
        profile: &ProfileScope,
    ) -> DomainResult<bool> {
        let scenes = self.memory.list_scenes(department, profile, "").await?;
        Ok(!self
            .pick_stale(department, profile, &scenes)
            .await?
            .is_empty())
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
        let stale = self.stale_scenes(department, profile, &scenes).await?;
        let stale_given: Vec<i64> = stale
            .iter()
            .flat_map(|s| s.marks.iter().map(|m| m.id))
            .collect();
        let input = self.plan_input(atoms, &scenes, &stale);
        // Counted before the model is called: a call that fails, times out
        // or answers nonsense uses up a try like one that leaves the scene.
        // A mark this run settles is gone, attempts and all.
        self.memory
            .note_stale_attempt(department, profile, &stale_given)
            .await?;
        let reply = self
            .llm
            .complete(&[
                ChatMessage::system(&system_prompt),
                ChatMessage::user(input),
            ])
            .await?;
        let plan: ScenePlan = parse_llm_json(&reply, '{')?;
        let numbers: Vec<&[u32]> = plan.operations.iter().map(|o| o.atoms.as_slice()).collect();
        let (placed, dropped) = assign_numbers(&numbers, atoms.len());
        if dropped > 0 {
            warn!(
                dropped,
                "L2 returned memory numbers out of range or claimed twice; ignored"
            );
        }
        let members: Vec<Vec<SceneMember>> = placed
            .iter()
            .map(|indices| indices.iter().map(|&i| member(&atoms[i])).collect())
            .collect();
        let mut settled = HashSet::new();
        let changed = self
            .apply(
                department,
                profile,
                &scenes,
                plan.operations,
                members,
                &stale,
                &mut settled,
            )
            .await?;
        self.settle_unchanged(department, profile, &stale, &plan.unchanged, &mut settled)
            .await?;
        Ok(L2Outcome {
            changed_files: changed,
            persona_update_request: plan.persona_update_request.filter(|r| !r.trim().is_empty()),
            system_prompt,
            stale_given,
        })
    }

    /// The live scenes with open marks L2 may still try, oldest mark first,
    /// at most `scenes_per_run` of them. A rewrite replaces its mark with a
    /// newer one, so a scene that keeps changing goes behind quiet ones.
    async fn pick_stale(
        &self,
        department: &DepartmentId,
        profile: &ProfileScope,
        scenes: &[SceneEntry],
    ) -> DomainResult<Vec<(String, Vec<StaleMark>)>> {
        let files: HashMap<&str, &str> = scenes
            .iter()
            .map(|s| (s.scene.scene_id.as_str(), s.relative_path.as_str()))
            .collect();
        let mut by_scene: Vec<(String, Vec<StaleMark>)> = Vec::new();
        for mark in self.memory.stale_marks(department, profile, None).await? {
            if mark.attempts >= self.stale.max_attempts {
                continue;
            }
            let Some(file) = files.get(mark.scene_id.as_str()) else {
                continue;
            };
            if let Some((_, marks)) = by_scene.iter_mut().find(|(f, _)| f == file) {
                marks.push(mark);
            } else if by_scene.len() < self.stale.scenes_per_run {
                by_scene.push(((*file).to_owned(), vec![mark]));
            }
        }
        Ok(by_scene)
    }

    /// The scenes [`Self::pick_stale`] picks, with what changed in each.
    async fn stale_scenes(
        &self,
        department: &DepartmentId,
        profile: &ProfileScope,
        scenes: &[SceneEntry],
    ) -> DomainResult<Vec<StaleScene>> {
        let by_scene = self.pick_stale(department, profile, scenes).await?;
        let mut out = Vec::with_capacity(by_scene.len());
        for (file, marks) in by_scene {
            let mut reasons = Vec::with_capacity(marks.len());
            for mark in &marks {
                reasons.push(self.reason(department, profile, mark).await?);
            }
            out.push(StaleScene {
                file,
                marks,
                reasons,
            });
        }
        Ok(out)
    }

    /// One mark as the model reads it: what the scene may still say, and
    /// what is true now. Both texts pass the atom owner's redaction as it
    /// is now, so a rule added after they were stored still applies to
    /// what reaches the model (and the shared scene it writes). When the
    /// owner cannot be found, the old text is left out.
    async fn reason(
        &self,
        department: &DepartmentId,
        profile: &ProfileScope,
        mark: &StaleMark,
    ) -> DomainResult<serde_json::Value> {
        let now = match mark.reason {
            StaleReason::Deleted => None,
            _ => self.memory.atom(department, &mark.atom_id).await?,
        };
        let kind = match (&now, mark.reason) {
            // Deleted since it was marked.
            (None, _) => StaleReason::Deleted,
            (Some(_), reason) => reason,
        };
        let owner = match &now {
            Some(atom) => Some(atom.scope.user_id.to_string()),
            None => self
                .memory
                .scene_atoms(department, profile, &mark.scene_id)
                .await?
                .into_iter()
                .find(|a| a.atom_id == mark.atom_id)
                .map(|a| a.user_id),
        };
        let filter = match &owner {
            Some(user) => Some(self.memory.member_filter(department, profile, user).await?),
            None => None,
        };
        let before = match (&filter, &mark.before_content) {
            (Some(f), Some(text)) => serde_json::Value::String(f(text)),
            _ => serde_json::Value::Null,
        };
        let mut reason = serde_json::json!({"kind": kind.as_str(), "before": before});
        if let (Some(atom), Some(f), StaleReason::Rewritten | StaleReason::ChangedDuringRun) =
            (&now, &filter, kind)
        {
            reason["after"] = serde_json::Value::String(f(&atom.content));
        }
        Ok(reason)
    }

    fn plan_input(
        &self,
        atoms: &[AtomicMemory],
        scenes: &[SceneEntry],
        stale: &[StaleScene],
    ) -> String {
        let memories: Vec<serde_json::Value> = atoms
            .iter()
            .enumerate()
            .map(|(i, a)| serde_json::json!({"n": i + 1, "type": a.atom_type.as_str(), "content": a.content, "created_at": a.created_at.to_rfc3339()}))
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
        let stale_scenes: Vec<serde_json::Value> = stale
            .iter()
            .map(|s| serde_json::json!({"file": s.file, "reasons": s.reasons}))
            .collect();
        prompts::data_input(
            &serde_json::json!({
                "new_memories": memories,
                "existing_scenes": existing,
                "stale_scenes": stale_scenes,
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

    #[allow(clippy::too_many_arguments)]
    async fn apply(
        &self,
        department: &DepartmentId,
        profile: &ProfileScope,
        scenes: &[SceneEntry],
        operations: Vec<SceneOperation>,
        members: Vec<Vec<SceneMember>>,
        stale: &[StaleScene],
        settled: &mut HashSet<i64>,
    ) -> DomainResult<Vec<String>> {
        let mut current: HashMap<String, SceneVersion> = scenes
            .iter()
            .map(|s| (s.relative_path.clone(), s.scene.clone()))
            .collect();
        let marks_of = |file: &str| -> Vec<i64> {
            stale
                .iter()
                .filter(|s| s.file == file)
                .flat_map(|s| s.marks.iter().map(|m| m.id))
                .collect()
        };
        let mut created = 0usize;
        let mut changed = Vec::new();
        for (op, members) in operations.into_iter().zip(members) {
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
            // Rewriting a stale scene, or folding stale scenes into it, deals
            // with the marks it was given for them.
            let resolved: Vec<i64> = std::iter::once(file.as_str())
                .chain(merged_away.iter().map(|m| m.slug.as_str()))
                .flat_map(marks_of)
                .collect();
            let links = SceneWriteLinks {
                members,
                resolved: resolved.clone(),
            };
            let written = match self
                .memory
                .append_scenes_linked(department, profile, versions, links)
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
            settled.extend(resolved);
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

    /// Settles the marks of stale scenes the model read and left as they
    /// are, unless an operation of this run already wrote them.
    async fn settle_unchanged(
        &self,
        department: &DepartmentId,
        profile: &ProfileScope,
        stale: &[StaleScene],
        unchanged: &[String],
        settled: &mut HashSet<i64>,
    ) -> DomainResult<()> {
        let files: HashSet<String> = unchanged
            .iter()
            .filter_map(|f| normalize_scene_filename(f).ok())
            .collect();
        let resolved: Vec<i64> = stale
            .iter()
            .filter(|s| files.contains(&s.file))
            .flat_map(|s| s.marks.iter().map(|m| m.id))
            .filter(|id| !settled.contains(id))
            .collect();
        if resolved.is_empty() {
            return Ok(());
        }
        self.memory
            .append_scenes_linked(
                department,
                profile,
                Vec::new(),
                SceneWriteLinks {
                    members: Vec::new(),
                    resolved: resolved.clone(),
                },
            )
            .await?;
        settled.extend(resolved);
        Ok(())
    }
}

fn member(atom: &AtomicMemory) -> SceneMember {
    SceneMember {
        atom_id: atom.id.clone(),
        atom_version: atom.version,
        user_id: atom.scope.user_id.to_string(),
        content: atom.content.clone(),
    }
}

/// Maps each operation's memory numbers (1-based `n`) to indices into the
/// batch of `batch` atoms. The model's numbers are not trusted: one out of
/// range is dropped, and one claimed by two operations is dropped from
/// both, since a memory belongs to one scene at most. Returns the indices
/// per operation and how many numbers were dropped.
fn assign_numbers(operations: &[&[u32]], batch: usize) -> (Vec<Vec<usize>>, usize) {
    let mut claims: BTreeMap<u32, usize> = BTreeMap::new();
    let mut dropped = 0;
    for numbers in operations {
        let unique: HashSet<u32> = numbers.iter().copied().collect();
        for n in unique {
            if n == 0 || n as usize > batch {
                dropped += 1;
            } else {
                *claims.entry(n).or_default() += 1;
            }
        }
    }
    let placed = operations
        .iter()
        .map(|numbers| {
            let mut seen = HashSet::new();
            let mut indices: Vec<usize> = numbers
                .iter()
                .copied()
                .filter(|n| claims.get(n) == Some(&1) && seen.insert(*n))
                .map(|n| n as usize - 1)
                .collect();
            indices.sort_unstable();
            indices
        })
        .collect();
    dropped += claims.values().filter(|&&c| c > 1).sum::<usize>();
    (placed, dropped)
}

#[cfg(test)]
mod tests {
    use super::assign_numbers;

    #[test]
    fn numbers_out_of_range_or_claimed_twice_are_dropped() {
        let (placed, dropped) = assign_numbers(&[&[1, 3, 3, 9], &[2, 3], &[0, 4]], 4);
        // 3 is claimed by two operations, 9 and 0 are out of range; the
        // repeat of 3 inside one operation counts once.
        assert_eq!(placed, vec![vec![0], vec![1], vec![3]]);
        assert_eq!(dropped, 4);
    }

    #[test]
    fn no_numbers_place_nothing() {
        let (placed, dropped) = assign_numbers(&[&[], &[]], 5);
        assert_eq!(placed, vec![Vec::<usize>::new(), Vec::new()]);
        assert_eq!(dropped, 0);
    }
}
