//! L0–L3 data-plane use cases.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use memory_domain::error::{DomainError, DomainResult};
use memory_domain::memory::{without_nul, AtomicMemory, ConversationMessage, Role};
use memory_domain::profile::{
    compose_persona, parse_scene, render_navigation, strip_scene_navigation,
    validate_relative_path, MemberState, NavigationEntry, NewPersonaVersion, NewSceneVersion,
    PersonaOp, PersonaVersion, ProfileFile, ProfileScope, SceneAtom, SceneMeta, SceneOp,
    SceneVersion, SceneWriteLinks, StaleMark, StaleReason, PERSONA_PATH,
};
use memory_domain::retrieval::SearchMode;
use memory_domain::scope::{DepartmentId, MemoryScope};
use serde::Serialize;
use tracing::warn;

use crate::redaction::{RedactionService, WriteScope};

use super::search::AtomSearcher;
use crate::common::{Page, Scored};
use crate::ports::memory::{
    AtomFilter, AtomRepository, AtomSelector, ConversationFilter, ConversationRepository,
    ProfileVersionStore, TextFilter,
};
use crate::ports::system::{Clock, EmbeddingProvider, IdGenerator};

pub const MAX_MESSAGES_PER_ADD: usize = 100;

/// Notified after L0 writes so the pipeline can schedule L1.
#[async_trait]
pub trait ConversationObserver: Send + Sync {
    async fn on_conversation(
        &self,
        department: &DepartmentId,
        scope: &MemoryScope,
        session_id: &str,
        user_rounds: u32,
    );
    /// Atoms of the profile were rewritten or deleted outside L1 (through
    /// the API): scenes holding them are stale and need an L2 run.
    async fn on_atoms_changed(&self, department: &DepartmentId, profile: &ProfileScope);
}

pub struct NewMessage {
    pub role: Role,
    pub content: String,
    pub timestamp: Option<DateTime<Utc>>,
    pub recorded_at: Option<DateTime<Utc>>,
}

/// A current scene with its file and META views.
/// One of the caller's atoms in a scene (`scenario/atoms`).
#[derive(Debug, Clone, Serialize)]
pub struct SceneAtomView {
    pub atom_id: String,
    pub state: MemberState,
    /// The version the scene was written from.
    pub atom_version: u32,
    /// The version stored now; `None` once deleted.
    pub current_version: Option<u32>,
    /// `None` once deleted.
    pub content: Option<String>,
    #[serde(rename = "type")]
    pub atom_type: Option<String>,
    pub stale_reasons: Vec<StaleReason>,
}

/// The atoms a scene was built from, as one user may see them: their own,
/// and how many belong to others.
#[derive(Debug, Clone, Serialize)]
pub struct SceneAtomsView {
    pub scene_id: String,
    pub stale: bool,
    pub stale_reasons: usize,
    pub atoms: Vec<SceneAtomView>,
    /// Atoms of other users in this scene, counted but not shown.
    pub hidden: usize,
}

pub struct SceneEntry {
    pub scene: SceneVersion,
    pub file: ProfileFile,
    pub relative_path: String,
    pub meta: Option<SceneMeta>,
}

impl From<SceneVersion> for SceneEntry {
    fn from(scene: SceneVersion) -> Self {
        Self {
            file: scene.to_file(),
            relative_path: scene.slug.clone(),
            meta: Some(scene.meta()),
            scene,
        }
    }
}

pub struct MemoryService {
    redaction: Arc<RedactionService>,
    conversations: Arc<dyn ConversationRepository>,
    atoms: Arc<dyn AtomRepository>,
    profiles: Arc<dyn ProfileVersionStore>,
    embedder: Arc<dyn EmbeddingProvider>,
    searcher: Arc<AtomSearcher>,
    observer: Arc<dyn ConversationObserver>,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGenerator>,
}

impl MemoryService {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        redaction: Arc<RedactionService>,
        conversations: Arc<dyn ConversationRepository>,
        atoms: Arc<dyn AtomRepository>,
        profiles: Arc<dyn ProfileVersionStore>,
        embedder: Arc<dyn EmbeddingProvider>,
        searcher: Arc<AtomSearcher>,
        observer: Arc<dyn ConversationObserver>,
        clock: Arc<dyn Clock>,
        ids: Arc<dyn IdGenerator>,
    ) -> Self {
        Self {
            redaction,
            conversations,
            atoms,
            profiles,
            embedder,
            searcher,
            observer,
            clock,
            ids,
        }
    }

    // ── L0 ──────────────────────────────────────────────────────────────

    pub async fn add_conversation(
        &self,
        department: &DepartmentId,
        scope: &MemoryScope,
        session_id: &str,
        messages: Vec<NewMessage>,
    ) -> DomainResult<Vec<ConversationMessage>> {
        let redactor = self
            .redaction
            .redactor(
                department,
                WriteScope::member(scope.team_id.as_str(), scope.user_id.as_str()),
            )
            .await?;
        if messages.is_empty() || messages.len() > MAX_MESSAGES_PER_ADD {
            return Err(DomainError::invalid("messages must contain 1-100 items"));
        }
        let now = self.clock.now();
        let records = messages
            .into_iter()
            .enumerate()
            .map(|(i, mut m)| {
                m.content = redactor.redact(&without_nul(m.content));
                ConversationMessage::validate_content(&m.content)?;
                // Distinct recorded_at per message keeps L1 cursors exact.
                let recorded_at = m
                    .recorded_at
                    .unwrap_or(now + Duration::milliseconds(i as i64));
                Ok(ConversationMessage {
                    id: self.ids.next("msg-"),
                    scope: scope.clone(),
                    session_id: session_id.to_owned(),
                    role: m.role,
                    content: m.content,
                    timestamp: m.timestamp.unwrap_or(recorded_at),
                    recorded_at,
                    version: 1,
                })
            })
            .collect::<DomainResult<Vec<_>>>()?;
        self.conversations.append(department, &records).await?;
        let rounds = records.iter().filter(|m| m.role == Role::User).count() as u32;
        if rounds > 0 {
            self.observer
                .on_conversation(department, scope, session_id, rounds)
                .await;
        }
        Ok(records)
    }

    pub async fn query_conversation(
        &self,
        department: &DepartmentId,
        scope: &MemoryScope,
        filter: &ConversationFilter,
        page: Page,
    ) -> DomainResult<(Vec<ConversationMessage>, usize)> {
        self.conversations
            .query(department, scope, filter, page)
            .await
    }

    pub async fn search_conversation(
        &self,
        department: &DepartmentId,
        scope: &MemoryScope,
        query: &str,
        filter: &ConversationFilter,
        limit: usize,
    ) -> DomainResult<Vec<Scored<ConversationMessage>>> {
        require_query(query)?;
        self.conversations
            .search(department, scope, query, filter, limit)
            .await
    }

    pub async fn delete_conversation(
        &self,
        department: &DepartmentId,
        scope: &MemoryScope,
        message_ids: &[String],
        session_ids: &[String],
    ) -> DomainResult<usize> {
        if message_ids.is_empty() && session_ids.is_empty() {
            return Err(DomainError::invalid(
                "message_ids or session_ids is required",
            ));
        }
        let mut deleted = 0;
        if !message_ids.is_empty() {
            deleted += self
                .conversations
                .delete_by_ids(department, scope, message_ids)
                .await?;
        }
        if !session_ids.is_empty() {
            deleted += self
                .conversations
                .delete_by_sessions(department, scope, session_ids)
                .await?;
        }
        Ok(deleted)
    }

    pub async fn count_conversation(
        &self,
        department: &DepartmentId,
        scope: &MemoryScope,
        filter: &ConversationFilter,
    ) -> DomainResult<usize> {
        self.conversations
            .count(department, Some(scope), filter)
            .await
    }

    // ── L1 ──────────────────────────────────────────────────────────────

    pub async fn update_atom(
        &self,
        department: &DepartmentId,
        scope: &MemoryScope,
        id: &str,
        content: String,
        background: Option<String>,
    ) -> DomainResult<AtomicMemory> {
        let redactor = self
            .redaction
            .redactor(
                department,
                WriteScope::member(scope.team_id.as_str(), scope.user_id.as_str()),
            )
            .await?;
        let mut atom = self
            .atoms
            .get(department, id)
            .await?
            .ok_or_else(|| DomainError::not_found("atomic", id))?;
        if atom.scope.team_id != scope.team_id
            || atom.scope.agent_id != scope.agent_id
            || atom.scope.user_id != scope.user_id
        {
            return Err(DomainError::permission_denied(
                "atom does not belong to the caller's scope",
            ));
        }
        atom.revise(
            redactor.redact(&content),
            background.map(|b| redactor.redact(&b)),
            self.clock.now(),
        )?;
        let embedding = self.embed_one(&atom.content).await;
        // The scene keeps the old text as its "before": redacted by the
        // rules in force now, not those of when it was written.
        let before: TextFilter = Arc::new(move |text: &str| redactor.redact(text));
        self.atoms
            .upsert_filtered(department, &atom, embedding.as_deref(), before)
            .await?;
        self.observer
            .on_atoms_changed(department, &ProfileScope::from(scope))
            .await;
        Ok(atom)
    }

    pub async fn query_atoms(
        &self,
        department: &DepartmentId,
        scope: &MemoryScope,
        filter: &AtomFilter,
        page: Page,
    ) -> DomainResult<(Vec<AtomicMemory>, usize)> {
        self.atoms
            .query(department, AtomSelector::Scope(scope), filter, page)
            .await
    }

    pub async fn search_atoms(
        &self,
        department: &DepartmentId,
        scope: &MemoryScope,
        query: &str,
        filter: &AtomFilter,
        limit: usize,
    ) -> DomainResult<Vec<Scored<AtomicMemory>>> {
        require_query(query)?;
        Ok(self
            .searcher
            .search(department, scope, query, filter, limit, SearchMode::Hybrid)
            .await?
            .hits)
    }

    pub async fn delete_atoms(
        &self,
        department: &DepartmentId,
        scope: &MemoryScope,
        ids: &[String],
    ) -> DomainResult<usize> {
        if ids.is_empty() {
            return Err(DomainError::invalid("ids is required"));
        }
        let before = self
            .redaction
            .member_filter(department, scope.team_id.as_str(), scope.user_id.as_str())
            .await?;
        let deleted = self
            .atoms
            .delete_filtered(department, scope, ids, before)
            .await?;
        if deleted > 0 {
            self.observer
                .on_atoms_changed(department, &ProfileScope::from(scope))
                .await;
        }
        Ok(deleted)
    }

    pub async fn count_atoms(
        &self,
        department: &DepartmentId,
        scope: &MemoryScope,
        filter: &AtomFilter,
    ) -> DomainResult<usize> {
        self.atoms.count(department, Some(scope), filter).await
    }

    async fn embed_one(&self, text: &str) -> Option<Vec<f32>> {
        self.embedder.dimensions()?;
        match self.embedder.embed(&[text.to_owned()]).await {
            Ok(mut v) if !v.is_empty() => Some(v.remove(0)),
            Ok(_) => None,
            Err(err) => {
                warn!(error = %err, "embedding failed; storing atom without vector");
                None
            }
        }
    }

    // ── L2 ──────────────────────────────────────────────────────────────

    /// A fresh scene id for a scene about to be created.
    pub fn new_scene_id(&self) -> String {
        self.ids.next("scn-")
    }

    pub async fn list_scenes(
        &self,
        department: &DepartmentId,
        profile: &ProfileScope,
        prefix: &str,
    ) -> DomainResult<Vec<SceneEntry>> {
        validate_relative_path(prefix)?;
        Ok(self
            .profiles
            .scene_heads(department, profile, prefix)
            .await?
            .into_iter()
            .map(SceneEntry::from)
            .collect())
    }

    /// The scene named `path`. A removed scene is gone for current reads;
    /// its versions stay readable by number.
    pub async fn read_scene(
        &self,
        department: &DepartmentId,
        profile: &ProfileScope,
        path: &str,
        version: Option<u32>,
    ) -> DomainResult<Option<SceneVersion>> {
        validate_scene_path(path)?;
        Ok(self
            .profiles
            .scene(department, profile, path, version)
            .await?
            .filter(|s| version.is_some() || s.is_live()))
    }

    /// Any scene of the profile by id, including one whose name a later
    /// scene took. A retired scene is returned too: callers asking by id
    /// are following its history.
    pub async fn read_scene_by_id(
        &self,
        department: &DepartmentId,
        profile: &ProfileScope,
        scene_id: &str,
        version: Option<u32>,
    ) -> DomainResult<Option<SceneVersion>> {
        self.profiles
            .scene_by_id(department, profile, scene_id, version)
            .await
    }

    /// Appends scene versions in one transaction, secrets redacted. Every
    /// scene write goes through here: the L2 extractor's, the API's and
    /// removals.
    pub async fn append_scenes(
        &self,
        department: &DepartmentId,
        profile: &ProfileScope,
        versions: Vec<NewSceneVersion>,
    ) -> DomainResult<Vec<SceneVersion>> {
        self.append_scenes_linked(department, profile, versions, SceneWriteLinks::default())
            .await
    }

    /// [`append_scenes`], with the atoms an L2 write placed and the stale
    /// marks it dealt with (see `ProfileVersionStore::append_scenes_linked`).
    ///
    /// [`append_scenes`]: MemoryService::append_scenes
    pub async fn append_scenes_linked(
        &self,
        department: &DepartmentId,
        profile: &ProfileScope,
        mut versions: Vec<NewSceneVersion>,
        links: SceneWriteLinks,
    ) -> DomainResult<Vec<SceneVersion>> {
        let redactor = self
            .redaction
            .redactor(department, WriteScope::team(profile.team_id.as_str()))
            .await?;
        for v in &mut versions {
            v.summary = redactor.redact(&v.summary);
            v.body = redactor.redact(&v.body);
        }
        self.profiles
            .append_scenes_linked(department, profile, versions, links)
            .await
    }

    /// A member's redaction, for text of their atoms used outside the
    /// write it came with (a scene's "before" and "after").
    pub async fn member_filter(
        &self,
        department: &DepartmentId,
        profile: &ProfileScope,
        user_id: &str,
    ) -> DomainResult<TextFilter> {
        self.redaction
            .member_filter(department, profile.team_id.as_str(), user_id)
            .await
    }

    /// Atoms placed in a scene, current and moved out, of every user.
    pub async fn scene_atoms(
        &self,
        department: &DepartmentId,
        profile: &ProfileScope,
        scene_id: &str,
    ) -> DomainResult<Vec<SceneAtom>> {
        self.profiles
            .scene_atoms(department, profile, scene_id)
            .await
    }

    /// Open stale marks of the profile's scenes, oldest first.
    pub async fn stale_marks(
        &self,
        department: &DepartmentId,
        profile: &ProfileScope,
        scene_ids: Option<&[String]>,
    ) -> DomainResult<Vec<StaleMark>> {
        self.profiles
            .stale_marks(department, profile, scene_ids)
            .await
    }

    /// Open stale marks per scene, for the scenes listed.
    pub async fn stale_counts(
        &self,
        department: &DepartmentId,
        profile: &ProfileScope,
        scene_ids: &[String],
    ) -> DomainResult<HashMap<String, usize>> {
        let mut counts = HashMap::new();
        for mark in self
            .profiles
            .stale_marks(department, profile, Some(scene_ids))
            .await?
        {
            *counts.entry(mark.scene_id).or_default() += 1;
        }
        Ok(counts)
    }

    /// The atoms of scene `scene_id` that belong to `scope`'s user, and how
    /// many belong to others. A scene is shared by the agent's users, but
    /// an atom is its user's memory: others' are only counted.
    pub async fn scene_atom_view(
        &self,
        department: &DepartmentId,
        scope: &MemoryScope,
        scene_id: &str,
    ) -> DomainResult<SceneAtomsView> {
        let profile = ProfileScope::from(scope);
        let rows = self
            .profiles
            .scene_atoms(department, &profile, scene_id)
            .await?;
        let marks = self
            .profiles
            .stale_marks(department, &profile, Some(&[scene_id.to_owned()]))
            .await?;
        let user = scope.user_id.as_str();
        let hidden = rows.iter().filter(|r| r.user_id != user).count();
        let mut atoms = Vec::new();
        for row in rows.into_iter().filter(|r| r.user_id == user) {
            let stored = self.atoms.get(department, &row.atom_id).await?.filter(|a| {
                ProfileScope::from(&a.scope) == profile && a.scope.user_id.as_str() == user
            });
            atoms.push(SceneAtomView {
                stale_reasons: marks
                    .iter()
                    .filter(|m| m.atom_id == row.atom_id)
                    .map(|m| m.reason)
                    .collect(),
                atom_id: row.atom_id,
                state: row.state,
                atom_version: row.atom_version,
                current_version: stored.as_ref().map(|a| a.version),
                content: stored.as_ref().map(|a| a.content.clone()),
                atom_type: stored.as_ref().map(|a| a.atom_type.as_str().to_owned()),
            });
        }
        Ok(SceneAtomsView {
            scene_id: scene_id.to_owned(),
            stale: !marks.is_empty(),
            stale_reasons: marks.len(),
            atoms,
            hidden,
        })
    }

    pub async fn note_stale_attempt(
        &self,
        department: &DepartmentId,
        profile: &ProfileScope,
        ids: &[i64],
    ) -> DomainResult<()> {
        self.profiles
            .note_stale_attempt(department, profile, ids)
            .await
    }

    /// One atom by id, whatever its scope: for the pipeline, which works
    /// across a profile's users.
    pub async fn atom(
        &self,
        department: &DepartmentId,
        id: &str,
    ) -> DomainResult<Option<AtomicMemory>> {
        self.atoms.get(department, id).await
    }

    /// Writes a scene through the API as a new version: the body comes from
    /// `content` (its META header, if any, is ignored), the summary is
    /// replaced when given, heat and extra attributes carry over.
    pub async fn write_scene(
        &self,
        department: &DepartmentId,
        profile: &ProfileScope,
        path: &str,
        content: &str,
        summary: Option<&str>,
        author: Option<&str>,
    ) -> DomainResult<SceneVersion> {
        validate_scene_path(path)?;
        if let Some(summary) = summary {
            if summary.chars().count() > MAX_SUMMARY_CHARS {
                return Err(DomainError::invalid(format!(
                    "summary is limited to {MAX_SUMMARY_CHARS} characters"
                )));
            }
        }
        let current = self
            .profiles
            .scene(department, profile, path, None)
            .await?
            .filter(SceneVersion::is_live);
        // A new scene gets a plain name; one that exists keeps its name, so
        // scenes created before the rule can still be updated.
        if current.is_none() {
            validate_new_scene_name(path)?;
        }
        let (_, body) = parse_scene(content);
        let version = NewSceneVersion {
            scene_id: current
                .as_ref()
                .map_or_else(|| self.new_scene_id(), |s| s.scene_id.clone()),
            op: SceneOp::Manual,
            slug: path.to_owned(),
            summary: summary
                .map(str::to_owned)
                .or_else(|| current.as_ref().map(|s| s.summary.clone()))
                .unwrap_or_default(),
            heat: current.as_ref().map_or(1, |s| s.heat),
            body,
            attrs: current.map(|s| s.attrs).unwrap_or_default(),
            parents: Vec::new(),
            author: author.map(str::to_owned),
            expected_version: None,
        };
        let mut written = self
            .append_scenes(department, profile, vec![version])
            .await?;
        written
            .pop()
            .ok_or_else(|| DomainError::internal("scene write returned nothing"))
    }

    /// Retires the scene at `path`, or every scene below it when it ends
    /// with `/`. Their versions stay; they just stop being current.
    pub async fn remove_scene(
        &self,
        department: &DepartmentId,
        profile: &ProfileScope,
        path: &str,
        author: Option<&str>,
    ) -> DomainResult<usize> {
        validate_relative_path(path)?;
        if path.is_empty() {
            return Err(DomainError::invalid("path is required"));
        }
        let retired: Vec<NewSceneVersion> = self
            .profiles
            .scene_heads(department, profile, path)
            .await?
            .into_iter()
            .filter(|s| path.ends_with('/') || s.slug == path)
            // The user's removal wins over whatever happened since the read.
            .map(|s| NewSceneVersion {
                expected_version: None,
                ..retirement(s, None, author)
            })
            .collect();
        let removed = retired.len();
        if removed > 0 {
            self.append_scenes(department, profile, retired).await?;
        }
        Ok(removed)
    }

    pub async fn count_scenes(
        &self,
        department: &DepartmentId,
        profile: &ProfileScope,
        prefix: &str,
    ) -> DomainResult<usize> {
        validate_relative_path(prefix)?;
        self.profiles
            .count_scenes(department, Some(profile), prefix)
            .await
    }

    // ── L3 ──────────────────────────────────────────────────────────────

    /// The persona as a file. The latest version comes with the navigation
    /// of the current scenes; an older one is its body alone.
    pub async fn read_persona(
        &self,
        department: &DepartmentId,
        profile: &ProfileScope,
        version: Option<u32>,
    ) -> DomainResult<Option<ProfileFile>> {
        let Some(persona) = self.profiles.persona(department, profile, version).await? else {
            return Ok(None);
        };
        let content = match version {
            Some(_) => persona.body.clone(),
            None => compose_persona(&persona.body, &self.navigation(department, profile).await?),
        };
        Ok(Some(persona_file(&persona, content)))
    }

    /// The latest persona body (no navigation), if there is a non-empty one.
    pub async fn persona_body(
        &self,
        department: &DepartmentId,
        profile: &ProfileScope,
    ) -> DomainResult<Option<String>> {
        Ok(self
            .profiles
            .persona(department, profile, None)
            .await?
            .map(|p| p.body)
            .filter(|b| !b.trim().is_empty()))
    }

    /// Appends a persona version; any navigation in `content` is dropped,
    /// since it is rendered from the scenes when read.
    pub async fn write_persona(
        &self,
        department: &DepartmentId,
        profile: &ProfileScope,
        content: &str,
        op: PersonaOp,
        author: Option<&str>,
    ) -> DomainResult<ProfileFile> {
        let redactor = self
            .redaction
            .redactor(department, WriteScope::team(profile.team_id.as_str()))
            .await?;
        let current = self.profiles.persona(department, profile, None).await?;
        let persona = self
            .profiles
            .append_persona(
                department,
                profile,
                NewPersonaVersion {
                    op,
                    body: redactor.redact(&strip_scene_navigation(content)),
                    attrs: current.map(|p| p.attrs).unwrap_or_default(),
                    author: author.map(str::to_owned),
                },
            )
            .await?;
        let navigation = self.navigation(department, profile).await?;
        let content = compose_persona(&persona.body, &navigation);
        Ok(persona_file(&persona, content))
    }

    pub async fn count_persona(
        &self,
        department: &DepartmentId,
        profile: &ProfileScope,
    ) -> DomainResult<usize> {
        self.profiles
            .count_personas(department, Some(profile))
            .await
    }

    /// Rendered Scene Navigation for the profile ("" when there are no scenes).
    pub async fn navigation(
        &self,
        department: &DepartmentId,
        profile: &ProfileScope,
    ) -> DomainResult<String> {
        let scenes = self.profiles.scene_heads(department, profile, "").await?;
        if scenes.is_empty() {
            return Ok(String::new());
        }
        let updated: Vec<String> = scenes.iter().map(|s| s.created_at.to_rfc3339()).collect();
        let mut entries: Vec<NavigationEntry<'_>> = scenes
            .iter()
            .zip(&updated)
            .map(|(s, updated)| NavigationEntry {
                filename: &s.slug,
                summary: &s.summary,
                heat: s.heat,
                updated,
            })
            .collect();
        Ok(render_navigation(&mut entries))
    }
}

/// The final version of a scene that goes away: same content, `retire`,
/// expected to be on top of the version given.
pub fn retirement(
    scene: SceneVersion,
    merged_into: Option<&str>,
    author: Option<&str>,
) -> NewSceneVersion {
    let mut attrs = scene.attrs;
    if let Some(target) = merged_into {
        attrs.insert("merged_into".into(), target.to_owned());
    }
    NewSceneVersion {
        scene_id: scene.scene_id,
        op: SceneOp::Retire,
        slug: scene.slug,
        summary: scene.summary,
        heat: scene.heat,
        body: scene.body,
        attrs,
        parents: Vec::new(),
        author: author.map(str::to_owned),
        expected_version: Some(scene.version),
    }
}

fn persona_file(persona: &PersonaVersion, content: String) -> ProfileFile {
    ProfileFile {
        path: PERSONA_PATH.to_owned(),
        content,
        version: persona.version,
        created_at: persona.created_at,
        updated_at: persona.created_at,
    }
}

fn require_query(query: &str) -> DomainResult<()> {
    let len = query.chars().count();
    if query.trim().is_empty() || len > 2048 {
        return Err(DomainError::invalid("query must be 1-2048 characters"));
    }
    Ok(())
}

/// Characters of a scene summary written through the API, at most.
const MAX_SUMMARY_CHARS: usize = 500;

/// A scene path written through the API: segments of letters, digits (any
/// script), `-`, `_` and `.`, like the names L2 gives scenes
/// (`normalize_scene_filename`). Paths and summaries go into recall output,
/// so no spaces, newlines or markup. At most 200 characters.
fn validate_new_scene_name(path: &str) -> DomainResult<()> {
    let ok = path.chars().count() <= 200
        && path.split('/').all(|seg| {
            !seg.is_empty()
                && seg != "."
                && seg
                    .chars()
                    .all(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.'))
        });
    if ok {
        Ok(())
    } else {
        Err(DomainError::invalid(
            "scene path: letters, digits, -, _ and . in segments separated by /, at most 200 characters",
        ))
    }
}

fn validate_scene_path(path: &str) -> DomainResult<()> {
    validate_relative_path(path)?;
    if path.is_empty() || path.ends_with('/') {
        return Err(DomainError::invalid("path must name a file"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_scene_names_are_plain() {
        for ok in ["deploy.md", "projects/發版-流程.md", "a_b.c.md"] {
            assert!(validate_new_scene_name(ok).is_ok(), "{ok}");
        }
        for bad in [
            "a b.md",
            "x\n### Path: y.md",
            "</scene-navigation>.md",
            "a//b.md",
            "./a.md",
            "a/",
        ] {
            assert!(validate_new_scene_name(bad).is_err(), "{bad:?}");
        }
        assert!(validate_new_scene_name(&"a".repeat(201)).is_err());
    }
}
