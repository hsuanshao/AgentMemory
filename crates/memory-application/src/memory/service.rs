//! L0–L3 data-plane use cases.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use memory_domain::error::{DomainError, DomainResult};
use memory_domain::memory::{without_nul, AtomicMemory, ConversationMessage, Role};
use memory_domain::profile::{
    compose_persona, parse_scene, render_navigation, strip_scene_navigation,
    validate_relative_path, NavigationEntry, NewPersonaVersion, NewSceneVersion, PersonaOp,
    PersonaVersion, ProfileFile, ProfileScope, SceneMeta, SceneOp, SceneVersion, PERSONA_PATH,
};
use memory_domain::retrieval::SearchMode;
use memory_domain::scope::{DepartmentId, MemoryScope};
use tracing::warn;

use super::search::AtomSearcher;
use crate::common::{Page, Scored};
use crate::ports::memory::{
    AtomFilter, AtomRepository, AtomSelector, ConversationFilter, ConversationRepository,
    ProfileVersionStore,
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
}

pub struct NewMessage {
    pub role: Role,
    pub content: String,
    pub timestamp: Option<DateTime<Utc>>,
    pub recorded_at: Option<DateTime<Utc>>,
}

/// A current scene with its file and META views.
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
        if messages.is_empty() || messages.len() > MAX_MESSAGES_PER_ADD {
            return Err(DomainError::invalid("messages must contain 1-100 items"));
        }
        let now = self.clock.now();
        let records = messages
            .into_iter()
            .enumerate()
            .map(|(i, mut m)| {
                m.content = without_nul(m.content);
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
        atom.revise(content, background, self.clock.now())?;
        let embedding = self.embed_one(&atom.content).await;
        self.atoms
            .upsert(department, &atom, embedding.as_deref())
            .await?;
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
        self.atoms.delete(department, scope, ids).await
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

    pub async fn read_scene(
        &self,
        department: &DepartmentId,
        profile: &ProfileScope,
        path: &str,
        version: Option<u32>,
    ) -> DomainResult<Option<ProfileFile>> {
        validate_scene_path(path)?;
        // A removed scene is gone for current reads; its versions stay
        // readable by number.
        Ok(self
            .profiles
            .scene(department, profile, path, version)
            .await?
            .filter(|s| version.is_some() || s.is_live())
            .map(|s| s.to_file()))
    }

    /// Appends scene versions in one transaction (the L2 extractor's writes).
    pub async fn append_scenes(
        &self,
        department: &DepartmentId,
        profile: &ProfileScope,
        versions: Vec<NewSceneVersion>,
    ) -> DomainResult<Vec<SceneVersion>> {
        self.profiles
            .append_scenes(department, profile, versions)
            .await
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
    ) -> DomainResult<ProfileFile> {
        validate_scene_path(path)?;
        let current = self
            .profiles
            .scene(department, profile, path, None)
            .await?
            .filter(SceneVersion::is_live);
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
        };
        let mut written = self
            .profiles
            .append_scenes(department, profile, vec![version])
            .await?;
        written
            .pop()
            .map(|s| s.to_file())
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
            .map(|s| retirement(s, None, author))
            .collect();
        let removed = retired.len();
        if removed > 0 {
            self.profiles
                .append_scenes(department, profile, retired)
                .await?;
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
        let current = self.profiles.persona(department, profile, None).await?;
        let persona = self
            .profiles
            .append_persona(
                department,
                profile,
                NewPersonaVersion {
                    op,
                    body: strip_scene_navigation(content),
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

/// The final version of a scene that goes away: same content, `retire`.
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

fn validate_scene_path(path: &str) -> DomainResult<()> {
    validate_relative_path(path)?;
    if path.is_empty() || path.ends_with('/') {
        return Err(DomainError::invalid("path must name a file"));
    }
    Ok(())
}
