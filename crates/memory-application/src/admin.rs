//! Operational use cases: chat-memory clearing, knowledge metadata,
//! department statistics and department destruction.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::SecondsFormat;
use memory_domain::error::{DomainError, DomainResult};
use memory_domain::memory::AtomType;
use memory_domain::meta::AssetType;
use memory_domain::profile::ProfileScope;
use memory_domain::scope::{DepartmentId, SegmentId};
use serde::{Deserialize, Serialize};

use crate::common::{BatchDeleteResult, Page};
use crate::meta::{ChatMemoryCleaner, MetaService};
use crate::pipeline::service::PipelineService;
use crate::ports::memory::{
    AtomFilter, AtomRepository, ConversationFilter, ConversationRepository, ProfileVersionStore,
};
use crate::ports::meta::{KnowledgeEntity, KnowledgeRepository};
use crate::ports::pipeline::GenerationLogRepository;
use crate::ports::skill::SkillRepository;
use crate::ports::system::Clock;

#[derive(Debug, Clone, Serialize)]
pub struct ClearItem {
    pub memory_id: String,
    pub cleared: bool,
    pub l0_deleted: usize,
    pub l1_deleted: usize,
    pub profile_deleted: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

pub struct ChatMemoryService {
    conversations: Arc<dyn ConversationRepository>,
    atoms: Arc<dyn AtomRepository>,
    profiles: Arc<dyn ProfileVersionStore>,
    pipeline: Arc<PipelineService>,
    meta: Arc<MetaService>,
}

impl ChatMemoryService {
    pub fn new(
        conversations: Arc<dyn ConversationRepository>,
        atoms: Arc<dyn AtomRepository>,
        profiles: Arc<dyn ProfileVersionStore>,
        pipeline: Arc<PipelineService>,
        meta: Arc<MetaService>,
    ) -> Self {
        Self {
            conversations,
            atoms,
            profiles,
            pipeline,
            meta,
        }
    }

    async fn wipe(
        &self,
        department: &DepartmentId,
        profile: &ProfileScope,
    ) -> DomainResult<(usize, usize, usize)> {
        let l0 = self
            .conversations
            .delete_profile(department, profile)
            .await?;
        let l1 = self.atoms.delete_profile(department, profile).await?;
        let profile_rows = self.profiles.purge(department, profile).await?;
        self.pipeline.reset_profile(department, profile).await?;
        Ok((l0, l1, profile_rows))
    }

    /// Clears content of chat-memory assets; assets, bindings and ACL stay.
    pub async fn clear(
        &self,
        department: &DepartmentId,
        memory_ids: &[String],
    ) -> DomainResult<Vec<ClearItem>> {
        let mut ids: Vec<String> = memory_ids
            .iter()
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
            .collect();
        ids.dedup();
        if ids.is_empty() || ids.len() > 100 {
            return Err(DomainError::invalid("memory_ids must contain 1-100 ids"));
        }
        let mut items = Vec::new();
        for id in ids {
            items.push(match self.resolve_profile(department, &id).await {
                Ok(profile) => match self.wipe(department, &profile).await {
                    Ok((l0, l1, files)) => ClearItem {
                        memory_id: id,
                        cleared: true,
                        l0_deleted: l0,
                        l1_deleted: l1,
                        profile_deleted: files,
                        reason: None,
                    },
                    Err(e) => failed_item(id, e.to_string()),
                },
                Err(e) => failed_item(id, e.to_string()),
            });
        }
        Ok(items)
    }

    async fn resolve_profile(
        &self,
        department: &DepartmentId,
        memory_id: &str,
    ) -> DomainResult<ProfileScope> {
        let asset = self
            .meta
            .get_asset_unchecked(department, memory_id)
            .await?
            .filter(|a| a.asset_type == AssetType::ChatMemory)
            .ok_or_else(|| DomainError::not_found("chat_memory", memory_id))?;
        let agent = memory_id
            .strip_prefix(&format!("chat_memory-{}-", asset.team_id))
            .ok_or_else(|| DomainError::invalid("malformed chat memory id"))?;
        Ok(ProfileScope {
            team_id: SegmentId::parse("team_id", &asset.team_id)?,
            agent_id: SegmentId::parse("agent_id", agent)?,
        })
    }
}

fn failed_item(memory_id: String, reason: String) -> ClearItem {
    ClearItem {
        memory_id,
        cleared: false,
        l0_deleted: 0,
        l1_deleted: 0,
        profile_deleted: 0,
        reason: Some(reason),
    }
}

#[async_trait]
impl ChatMemoryCleaner for ChatMemoryService {
    async fn clear_chat_memory(
        &self,
        department: &DepartmentId,
        team_id: &str,
        agent_id: &str,
    ) -> DomainResult<()> {
        let profile = ProfileScope {
            team_id: SegmentId::parse("team_id", team_id)?,
            agent_id: SegmentId::parse("agent_id", agent_id)?,
        };
        self.wipe(department, &profile).await.map(|_| ())
    }
}

#[derive(Debug, Deserialize)]
pub struct KnowledgeInput {
    pub knowledge_id: String,
    #[serde(rename = "type")]
    pub kind: Option<String>,
    pub service_url: Option<String>,
    pub name: Option<String>,
    pub summary: Option<String>,
    pub team_id: Option<String>,
    pub user_id: Option<String>,
    pub repo_url: Option<String>,
    pub branch: Option<String>,
}

/// Registry of knowledge sources (Wiki, CodeGraph). Content lives in an
/// external knowledge service; only metadata is kept here.
pub struct KnowledgeService {
    repo: Arc<dyn KnowledgeRepository>,
    clock: Arc<dyn Clock>,
}

impl KnowledgeService {
    pub fn new(repo: Arc<dyn KnowledgeRepository>, clock: Arc<dyn Clock>) -> Self {
        Self { repo, clock }
    }

    fn now(&self) -> String {
        self.clock
            .now()
            .to_rfc3339_opts(SecondsFormat::Millis, true)
    }

    pub async fn create(
        &self,
        department: &DepartmentId,
        input: KnowledgeInput,
    ) -> DomainResult<KnowledgeEntity> {
        let kind = input
            .kind
            .ok_or_else(|| DomainError::invalid("type is required"))?;
        if kind != "wiki" && kind != "code-graph" {
            return Err(DomainError::invalid("type must be wiki or code-graph"));
        }
        let required = |v: Option<String>, f: &str| {
            v.filter(|s| !s.is_empty())
                .ok_or_else(|| DomainError::invalid(format!("{f} is required")))
        };
        if input
            .summary
            .as_deref()
            .is_some_and(|s| s.chars().count() > 256)
        {
            return Err(DomainError::invalid(
                "summary must be at most 256 characters",
            ));
        }
        let created_at = self
            .repo
            .get_knowledge(department, &input.knowledge_id)
            .await?
            .map(|e| e.created_at)
            .unwrap_or_else(|| self.now());
        let entity = KnowledgeEntity {
            knowledge_id: input.knowledge_id,
            kind,
            service_url: required(input.service_url, "service_url")?,
            name: required(input.name, "name")?,
            summary: input.summary,
            team_id: required(input.team_id, "team_id")?,
            user_id: input.user_id,
            repo_url: input.repo_url,
            branch: input.branch,
            created_at,
            updated_at: self.now(),
        };
        self.repo.upsert_knowledge(department, &entity).await?;
        Ok(entity)
    }

    pub async fn get(
        &self,
        department: &DepartmentId,
        id: &str,
        team_id: Option<&str>,
    ) -> DomainResult<KnowledgeEntity> {
        let entity = self
            .repo
            .get_knowledge(department, id)
            .await?
            .ok_or_else(|| DomainError::not_found("knowledge", id))?;
        if team_id.is_some_and(|t| t != entity.team_id) {
            return Err(DomainError::permission_denied("Knowledge team_id mismatch"));
        }
        Ok(entity)
    }

    pub async fn update(
        &self,
        department: &DepartmentId,
        input: KnowledgeInput,
    ) -> DomainResult<KnowledgeEntity> {
        let mut entity = self
            .get(department, &input.knowledge_id, input.team_id.as_deref())
            .await?;
        if let Some(v) = input.name {
            entity.name = v;
        }
        if input.summary.is_some() {
            entity.summary = input.summary;
        }
        if let Some(v) = input.service_url {
            entity.service_url = v;
        }
        if input.repo_url.is_some() {
            entity.repo_url = input.repo_url;
        }
        if input.branch.is_some() {
            entity.branch = input.branch;
        }
        entity.updated_at = self.now();
        self.repo.upsert_knowledge(department, &entity).await?;
        Ok(entity)
    }

    pub async fn delete(
        &self,
        department: &DepartmentId,
        ids: &[String],
        team_id: Option<&str>,
    ) -> DomainResult<BatchDeleteResult> {
        if ids.is_empty() || ids.len() > 100 {
            return Err(DomainError::invalid("knowledge_ids must contain 1-100 ids"));
        }
        let mut allowed = Vec::new();
        let mut result = BatchDeleteResult::default();
        for id in ids {
            match self.get(department, id, team_id).await {
                Ok(_) => allowed.push(id.clone()),
                Err(e) => result.failed.push(crate::common::FailedId {
                    id: id.clone(),
                    reason: e.code,
                }),
            }
        }
        let deleted = self.repo.delete_knowledge(department, &allowed).await?;
        result.deleted_ids = deleted.deleted_ids;
        Ok(result)
    }

    pub async fn list(
        &self,
        department: &DepartmentId,
        team_id: &str,
        kind: Option<&str>,
        ids: Option<&[String]>,
        page: Page,
    ) -> DomainResult<(Vec<KnowledgeEntity>, usize)> {
        self.repo
            .list_knowledge(department, team_id, kind, ids, page)
            .await
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct AtomCounts {
    pub total: usize,
    pub episodic: usize,
    pub persona: usize,
    pub instruction: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct StatsOverview {
    pub l0: usize,
    pub l1: AtomCounts,
    pub l2: usize,
    pub l3: usize,
    pub skills: usize,
    pub generation_logs: usize,
}

/// Which departments have storage. Storage opens only departments created
/// here (`init-admin`); anything else gets `unknown_department`.
#[async_trait]
pub trait DepartmentDirectory: Send + Sync {
    /// Never creates anything. Also forgets a department another replica
    /// destroyed, so a stale view does not outlive the answer.
    async fn exists(&self, department: &DepartmentId) -> DomainResult<bool>;
    /// Creates the department's storage; a no-op when it exists.
    async fn create(&self, department: &DepartmentId) -> DomainResult<()>;
}

/// Wipes every trace of a department (database + files).
#[async_trait]
pub trait DepartmentLifecycle: Send + Sync {
    async fn destroy(&self, department: &DepartmentId) -> DomainResult<serde_json::Value>;
}

pub struct StatsService {
    conversations: Arc<dyn ConversationRepository>,
    atoms: Arc<dyn AtomRepository>,
    profiles: Arc<dyn ProfileVersionStore>,
    skills: Arc<dyn SkillRepository>,
    logs: Arc<dyn GenerationLogRepository>,
}

impl StatsService {
    pub fn new(
        conversations: Arc<dyn ConversationRepository>,
        atoms: Arc<dyn AtomRepository>,
        profiles: Arc<dyn ProfileVersionStore>,
        skills: Arc<dyn SkillRepository>,
        logs: Arc<dyn GenerationLogRepository>,
    ) -> Self {
        Self {
            conversations,
            atoms,
            profiles,
            skills,
            logs,
        }
    }

    pub async fn overview(&self, department: &DepartmentId) -> DomainResult<StatsOverview> {
        let atoms_of = |t: Option<AtomType>| AtomFilter {
            atom_type: t,
            ..Default::default()
        };
        Ok(StatsOverview {
            l0: self
                .conversations
                .count(department, None, &ConversationFilter::default())
                .await?,
            l1: AtomCounts {
                total: self.atoms.count(department, None, &atoms_of(None)).await?,
                episodic: self
                    .atoms
                    .count(department, None, &atoms_of(Some(AtomType::Episodic)))
                    .await?,
                persona: self
                    .atoms
                    .count(department, None, &atoms_of(Some(AtomType::Persona)))
                    .await?,
                instruction: self
                    .atoms
                    .count(department, None, &atoms_of(Some(AtomType::Instruction)))
                    .await?,
            },
            l2: self.profiles.count_scenes(department, None, "").await?,
            l3: self.profiles.count_personas(department, None).await?,
            skills: self.skills.count_heads(department).await?,
            generation_logs: self.logs.count(department).await?,
        })
    }
}
