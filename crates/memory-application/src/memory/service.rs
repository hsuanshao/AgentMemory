//! L0–L3 data-plane use cases.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use memory_domain::error::{DomainError, DomainResult};
use memory_domain::memory::{without_nul, AtomicMemory, ConversationMessage, Role};
use memory_domain::profile::{
    compose_persona, parse_scene, render_navigation, render_scene, scene_path,
    strip_scene_navigation, validate_relative_path, NavigationEntry, ProfileFile, ProfileScope,
    SceneMeta, PERSONA_PATH, SCENE_DIR,
};
use memory_domain::retrieval::SearchMode;
use memory_domain::scope::{InstanceId, MemoryScope};
use tracing::warn;

use super::search::AtomSearcher;
use crate::common::{Page, Scored};
use crate::ports::memory::{
    AtomFilter, AtomRepository, AtomSelector, ConversationFilter, ConversationRepository,
    ProfileFileRepository,
};
use crate::ports::system::{Clock, EmbeddingProvider, IdGenerator};

pub const MAX_MESSAGES_PER_ADD: usize = 100;

/// Notified after L0 writes so the pipeline can schedule L1.
#[async_trait]
pub trait ConversationObserver: Send + Sync {
    async fn on_conversation(
        &self,
        instance: &InstanceId,
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

/// A scene listing entry with its parsed META header.
pub struct SceneEntry {
    pub file: ProfileFile,
    pub relative_path: String,
    pub meta: Option<SceneMeta>,
}

pub struct MemoryService {
    conversations: Arc<dyn ConversationRepository>,
    atoms: Arc<dyn AtomRepository>,
    profiles: Arc<dyn ProfileFileRepository>,
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
        profiles: Arc<dyn ProfileFileRepository>,
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
        instance: &InstanceId,
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
        self.conversations.append(instance, &records).await?;
        let rounds = records.iter().filter(|m| m.role == Role::User).count() as u32;
        if rounds > 0 {
            self.observer
                .on_conversation(instance, scope, session_id, rounds)
                .await;
        }
        Ok(records)
    }

    pub async fn query_conversation(
        &self,
        instance: &InstanceId,
        scope: &MemoryScope,
        filter: &ConversationFilter,
        page: Page,
    ) -> DomainResult<(Vec<ConversationMessage>, usize)> {
        self.conversations
            .query(instance, scope, filter, page)
            .await
    }

    pub async fn search_conversation(
        &self,
        instance: &InstanceId,
        scope: &MemoryScope,
        query: &str,
        filter: &ConversationFilter,
        limit: usize,
    ) -> DomainResult<Vec<Scored<ConversationMessage>>> {
        require_query(query)?;
        self.conversations
            .search(instance, scope, query, filter, limit)
            .await
    }

    pub async fn delete_conversation(
        &self,
        instance: &InstanceId,
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
                .delete_by_ids(instance, scope, message_ids)
                .await?;
        }
        if !session_ids.is_empty() {
            deleted += self
                .conversations
                .delete_by_sessions(instance, scope, session_ids)
                .await?;
        }
        Ok(deleted)
    }

    pub async fn count_conversation(
        &self,
        instance: &InstanceId,
        scope: &MemoryScope,
        filter: &ConversationFilter,
    ) -> DomainResult<usize> {
        self.conversations
            .count(instance, Some(scope), filter)
            .await
    }

    // ── L1 ──────────────────────────────────────────────────────────────

    pub async fn update_atom(
        &self,
        instance: &InstanceId,
        scope: &MemoryScope,
        id: &str,
        content: String,
        background: Option<String>,
    ) -> DomainResult<AtomicMemory> {
        let mut atom = self
            .atoms
            .get(instance, id)
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
            .upsert(instance, &atom, embedding.as_deref())
            .await?;
        Ok(atom)
    }

    pub async fn query_atoms(
        &self,
        instance: &InstanceId,
        scope: &MemoryScope,
        filter: &AtomFilter,
        page: Page,
    ) -> DomainResult<(Vec<AtomicMemory>, usize)> {
        self.atoms
            .query(instance, AtomSelector::Scope(scope), filter, page)
            .await
    }

    pub async fn search_atoms(
        &self,
        instance: &InstanceId,
        scope: &MemoryScope,
        query: &str,
        filter: &AtomFilter,
        limit: usize,
    ) -> DomainResult<Vec<Scored<AtomicMemory>>> {
        require_query(query)?;
        Ok(self
            .searcher
            .search(instance, scope, query, filter, limit, SearchMode::Hybrid)
            .await?
            .hits)
    }

    pub async fn delete_atoms(
        &self,
        instance: &InstanceId,
        scope: &MemoryScope,
        ids: &[String],
    ) -> DomainResult<usize> {
        if ids.is_empty() {
            return Err(DomainError::invalid("ids is required"));
        }
        self.atoms.delete(instance, scope, ids).await
    }

    pub async fn count_atoms(
        &self,
        instance: &InstanceId,
        scope: &MemoryScope,
        filter: &AtomFilter,
    ) -> DomainResult<usize> {
        self.atoms.count(instance, Some(scope), filter).await
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

    pub async fn list_scenes(
        &self,
        instance: &InstanceId,
        profile: &ProfileScope,
        prefix: &str,
    ) -> DomainResult<Vec<SceneEntry>> {
        validate_relative_path(prefix)?;
        let files = self
            .profiles
            .list(instance, profile, &scene_path(prefix))
            .await?;
        Ok(files
            .into_iter()
            .map(|file| {
                let relative_path = file.path.trim_start_matches(SCENE_DIR).to_owned();
                let (meta, _) = parse_scene(&file.content);
                SceneEntry {
                    file,
                    relative_path,
                    meta,
                }
            })
            .collect())
    }

    pub async fn read_scene(
        &self,
        instance: &InstanceId,
        profile: &ProfileScope,
        path: &str,
        version: Option<u32>,
    ) -> DomainResult<Option<ProfileFile>> {
        validate_scene_path(path)?;
        self.profiles
            .read(instance, profile, &scene_path(path), version)
            .await
    }

    /// Writes a scene, preserving (or creating) its META header: `created`
    /// and `heat` survive, `updated` is refreshed, `summary` replaced when
    /// given.
    pub async fn write_scene(
        &self,
        instance: &InstanceId,
        profile: &ProfileScope,
        path: &str,
        content: &str,
        summary: Option<&str>,
    ) -> DomainResult<ProfileFile> {
        validate_scene_path(path)?;
        let now = self.clock.now().to_rfc3339();
        let existing = self
            .profiles
            .read(instance, profile, &scene_path(path), None)
            .await?;
        let mut meta = existing
            .as_ref()
            .and_then(|f| parse_scene(&f.content).0)
            .unwrap_or_else(|| SceneMeta {
                created: now.clone(),
                heat: 1,
                ..Default::default()
            });
        meta.updated = now;
        if let Some(s) = summary {
            meta.summary = s.to_owned();
        }
        let (_, body) = parse_scene(content);
        let file = self
            .profiles
            .write(
                instance,
                profile,
                &scene_path(path),
                &render_scene(&meta, &body),
            )
            .await?;
        self.refresh_navigation(instance, profile).await?;
        Ok(file)
    }

    pub async fn remove_scene(
        &self,
        instance: &InstanceId,
        profile: &ProfileScope,
        path: &str,
    ) -> DomainResult<usize> {
        validate_relative_path(path)?;
        if path.is_empty() {
            return Err(DomainError::invalid("path is required"));
        }
        let removed = self
            .profiles
            .remove(instance, profile, &scene_path(path))
            .await?;
        self.refresh_navigation(instance, profile).await?;
        Ok(removed)
    }

    pub async fn count_scenes(
        &self,
        instance: &InstanceId,
        profile: &ProfileScope,
        prefix: &str,
    ) -> DomainResult<usize> {
        validate_relative_path(prefix)?;
        self.profiles
            .count(instance, Some(profile), &scene_path(prefix))
            .await
    }

    // ── L3 ──────────────────────────────────────────────────────────────

    pub async fn read_persona(
        &self,
        instance: &InstanceId,
        profile: &ProfileScope,
        version: Option<u32>,
    ) -> DomainResult<Option<ProfileFile>> {
        self.profiles
            .read(instance, profile, PERSONA_PATH, version)
            .await
    }

    /// Stores the persona body followed by a freshly rendered navigation.
    pub async fn write_persona(
        &self,
        instance: &InstanceId,
        profile: &ProfileScope,
        content: &str,
    ) -> DomainResult<ProfileFile> {
        let body = strip_scene_navigation(content);
        let navigation = self.navigation(instance, profile).await?;
        self.profiles
            .write(
                instance,
                profile,
                PERSONA_PATH,
                &compose_persona(&body, &navigation),
            )
            .await
    }

    pub async fn count_persona(
        &self,
        instance: &InstanceId,
        profile: &ProfileScope,
    ) -> DomainResult<usize> {
        self.profiles
            .count(instance, Some(profile), PERSONA_PATH)
            .await
    }

    /// Rendered Scene Navigation for the profile ("" when there are no scenes).
    pub async fn navigation(
        &self,
        instance: &InstanceId,
        profile: &ProfileScope,
    ) -> DomainResult<String> {
        let scenes = self.list_scenes(instance, profile, "").await?;
        if scenes.is_empty() {
            return Ok(String::new());
        }
        let metas: Vec<(String, SceneMeta)> = scenes
            .into_iter()
            .map(|s| (s.relative_path, s.meta.unwrap_or_default()))
            .collect();
        let mut entries: Vec<NavigationEntry<'_>> = metas
            .iter()
            .map(|(path, m)| NavigationEntry {
                filename: path,
                summary: &m.summary,
                heat: m.heat,
                updated: &m.updated,
            })
            .collect();
        Ok(render_navigation(&mut entries))
    }

    /// Rewrites the navigation of an existing persona after scene changes.
    pub async fn refresh_navigation(
        &self,
        instance: &InstanceId,
        profile: &ProfileScope,
    ) -> DomainResult<()> {
        let Some(persona) = self.read_persona(instance, profile, None).await? else {
            return Ok(());
        };
        let body = strip_scene_navigation(&persona.content);
        if body.is_empty() {
            return Ok(());
        }
        let navigation = self.navigation(instance, profile).await?;
        self.profiles
            .write(
                instance,
                profile,
                PERSONA_PATH,
                &compose_persona(&body, &navigation),
            )
            .await?;
        Ok(())
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
