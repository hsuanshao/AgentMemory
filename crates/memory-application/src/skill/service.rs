//! Skill management: versioned SKILL.md documents with resource files,
//! optimistic locking and an `<available_skills>` listing.

use std::sync::Arc;

use memory_redact::Redactor;

use base64::Engine;
use memory_domain::error::{DomainError, DomainResult, ErrorKind};
use memory_domain::scope::DepartmentId;
use memory_domain::skill::{
    apply_patch, guess_mime, parse_frontmatter, render_listing, validate_resource_path,
    ListingLine, ResourceEntry, SkillStatus, SkillVersion, DEFAULT_LISTING_BUDGET,
    MAX_RESOURCE_BYTES, MAX_TOTAL_RESOURCE_BYTES,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tracing::warn;

use crate::meta::MetaService;
use crate::ports::skill::{SkillFilter, SkillRepository, SkillResourceStore};
use crate::ports::system::{Clock, IdGenerator};

#[derive(Debug, Clone, Copy)]
pub struct SkillSettings {
    pub version_ttl_days: i64,
    pub max_versions: usize,
    pub listing_top_k: usize,
}

impl Default for SkillSettings {
    fn default() -> Self {
        Self {
            version_ttl_days: 0,
            max_versions: 100,
            listing_top_k: 20,
        }
    }
}

/// Who performs a skill operation.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SkillIdentity {
    pub team_id: Option<String>,
    pub agent_id: Option<String>,
    pub user_id: Option<String>,
    pub task_id: Option<String>,
}

impl SkillIdentity {
    fn require_writer(&self) -> DomainResult<(&str, &str, &str)> {
        match (
            self.team_id.as_deref(),
            self.agent_id.as_deref(),
            self.user_id.as_deref(),
        ) {
            (Some(t), Some(a), Some(u)) if !t.is_empty() && !a.is_empty() && !u.is_empty() => {
                Ok((t, a, u))
            }
            _ => Err(skill_error(
                ErrorKind::InvalidInput,
                "INVALID_FRONTMATTER",
                "team_id, agent_id and user_id are required",
            )),
        }
    }
}

/// What to include when reading a skill.
#[derive(Debug, Clone, Copy)]
pub struct ReadOptions {
    pub version: Option<u32>,
    pub include_content: bool,
    pub include_manifest: bool,
}

impl Default for ReadOptions {
    fn default() -> Self {
        Self {
            version: None,
            include_content: true,
            include_manifest: true,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ListQuery {
    pub owner_agent_id: Option<String>,
    pub name_prefix: Option<String>,
    #[serde(default, rename = "status")]
    pub statuses: Vec<SkillStatus>,
    #[serde(skip)]
    pub limit: Option<usize>,
    #[serde(skip)]
    pub offset: Option<usize>,
}

/// Replace `old` with `new` (all occurrences when `replace_all`).
#[derive(Debug, Clone)]
pub struct StringPatch {
    pub old: String,
    pub new: String,
    pub replace_all: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ResourceInput {
    pub path: String,
    pub content: String,
    #[serde(default = "utf8")]
    pub encoding: String,
    pub mime_type: Option<String>,
    #[serde(default)]
    pub is_executable: bool,
}

fn utf8() -> String {
    "utf-8".into()
}

#[derive(Debug, Clone, Serialize)]
pub struct SkillSummary {
    pub skill_id: String,
    pub name: String,
    pub description: String,
    pub version: u32,
    pub is_head: bool,
    pub status: SkillStatus,
    pub owner_user_id: String,
    pub owner_agent_id: String,
    pub team_id: String,
    pub task_id: Option<String>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    #[serde(skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
}

impl From<&SkillVersion> for SkillSummary {
    fn from(v: &SkillVersion) -> Self {
        Self {
            skill_id: v.skill_id.clone(),
            name: v.name.clone(),
            description: v.description.clone(),
            version: v.version,
            is_head: v.is_head,
            status: v.status,
            owner_user_id: v.owner_user_id.clone(),
            owner_agent_id: v.owner_agent_id.clone(),
            team_id: v.team_id.clone(),
            task_id: v.task_id.clone(),
            created_at_ms: v.created_at_ms,
            updated_at_ms: v.updated_at_ms,
            metadata: v.metadata.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SkillDetail {
    #[serde(flatten)]
    pub summary: SkillSummary,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manifest: Option<Vec<ResourceEntry>>,
    pub content_hash: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct VersionItem {
    #[serde(flatten)]
    pub summary: SkillSummary,
    pub is_expired: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchHit {
    #[serde(flatten)]
    pub summary: SkillSummary,
    pub score: f64,
    pub snippet: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ListingHit {
    pub skill_id: String,
    pub version: u32,
    pub name: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Listing {
    pub mode: &'static str,
    pub listing: String,
    pub hits: Vec<ListingHit>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FileContent {
    pub path: String,
    pub content: String,
    pub version: u32,
    pub size_bytes: usize,
    pub encoding: String,
    pub mime_type: String,
}

pub(crate) fn skill_error(kind: ErrorKind, code: &str, detail: impl Into<String>) -> DomainError {
    DomainError::new(kind, code, detail)
}

fn not_found(skill_id: &str) -> DomainError {
    skill_error(
        ErrorKind::NotFound,
        "SKILL_NOT_FOUND",
        format!("skill not found: {skill_id}"),
    )
}

/// Raw resource bytes after decoding.
struct Resource {
    path: String,
    bytes: Vec<u8>,
    mime_type: String,
    is_executable: bool,
}

/// Decodes uploaded files. A file that is UTF-8 text (`.sh`, `.env.example`,
/// …), however it was encoded, is redacted like the skill itself; anything
/// else is binary and stored as sent.
fn decode_resources(
    redactor: &Redactor,
    inputs: Vec<ResourceInput>,
) -> DomainResult<Vec<Resource>> {
    if inputs.len() > 100 {
        return Err(skill_error(
            ErrorKind::InvalidInput,
            "INVALID_PATH",
            "at most 100 files per call",
        ));
    }
    inputs
        .into_iter()
        .map(|r| {
            validate_resource_path(&r.path)?;
            let bytes = match r.encoding.as_str() {
                "utf-8" => r.content.into_bytes(),
                "base64" => base64_decode(&r.content).ok_or_else(|| {
                    skill_error(
                        ErrorKind::InvalidInput,
                        "INVALID_PATH",
                        format!("invalid base64 in {}", r.path),
                    )
                })?,
                other => {
                    return Err(DomainError::invalid(format!(
                        "unsupported encoding: {other}"
                    )))
                }
            };
            if bytes.len() > MAX_RESOURCE_BYTES {
                return Err(skill_error(
                    ErrorKind::TooLarge,
                    "RESOURCE_TOO_LARGE",
                    format!("{} exceeds 5MB", r.path),
                ));
            }
            let bytes = match String::from_utf8(bytes) {
                Ok(text) => redactor.redact(&text).into_bytes(),
                Err(binary) => binary.into_bytes(),
            };
            Ok(Resource {
                mime_type: r.mime_type.unwrap_or_else(|| guess_mime(&r.path).into()),
                path: r.path,
                bytes,
                is_executable: r.is_executable,
            })
        })
        .collect()
}

pub struct SkillService {
    redactor: Arc<Redactor>,
    repo: Arc<dyn SkillRepository>,
    resources: Arc<dyn SkillResourceStore>,
    meta: Arc<MetaService>,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGenerator>,
    settings: SkillSettings,
}

impl SkillService {
    pub fn new(
        redactor: Arc<Redactor>,
        repo: Arc<dyn SkillRepository>,
        resources: Arc<dyn SkillResourceStore>,
        meta: Arc<MetaService>,
        clock: Arc<dyn Clock>,
        ids: Arc<dyn IdGenerator>,
        settings: SkillSettings,
    ) -> Self {
        Self {
            redactor,
            repo,
            resources,
            meta,
            clock,
            ids,
            settings,
        }
    }

    fn now_ms(&self) -> i64 {
        self.clock.now().timestamp_millis()
    }

    async fn new_skill_id(&self, department: &DepartmentId) -> DomainResult<String> {
        for _ in 0..3 {
            let id = format!("skl-{}", self.ids.hex(12));
            if !self.repo.skill_id_exists(department, &id).await? {
                return Ok(id);
            }
        }
        Err(skill_error(
            ErrorKind::Unavailable,
            "SKILL_ID_COLLISION",
            "could not allocate a skill id",
        ))
    }

    pub async fn create(
        &self,
        department: &DepartmentId,
        who: &SkillIdentity,
        name: &str,
        content: &str,
        resources: Vec<ResourceInput>,
        metadata: Option<Value>,
    ) -> DomainResult<SkillSummary> {
        let (team, agent, user) = who.require_writer()?;
        let content = &self.redactor.redact(content);
        let fm = parse_frontmatter(content)?;
        if fm.name != name {
            return Err(skill_error(
                ErrorKind::InvalidInput,
                "INVALID_FRONTMATTER",
                "frontmatter name must equal name",
            ));
        }
        if self
            .repo
            .find_head_by_name(department, team, agent, name)
            .await?
            .is_some()
        {
            return Err(skill_error(
                ErrorKind::Unprocessable,
                "SKILL_NAME_DUPLICATE",
                format!("skill name exists: {name}"),
            ));
        }
        let resources = decode_resources(&self.redactor, resources)?;
        let skill_id = self.new_skill_id(department).await?;
        let now = self.now_ms();
        let mut manifest = Vec::new();
        self.write_resources(department, &skill_id, 1, &resources, &mut manifest)
            .await?;
        let version = SkillVersion {
            skill_id: skill_id.clone(),
            version: 1,
            is_head: true,
            name: fm.name,
            description: fm.description,
            content: content.to_owned(),
            content_hash: content_hash(content),
            manifest,
            status: SkillStatus::Active,
            owner_user_id: user.to_owned(),
            owner_agent_id: agent.to_owned(),
            team_id: team.to_owned(),
            task_id: who.task_id.clone(),
            metadata: metadata.unwrap_or(Value::Null),
            created_at_ms: now,
            updated_at_ms: now,
        };
        if let Err(err) = self.repo.append_version(department, &version).await {
            let _ = self.resources.delete_skill(department, &skill_id).await;
            return Err(err);
        }
        if let Err(err) = self
            .meta
            .ensure_skill_asset(department, &skill_id, &version.name, team, agent, user)
            .await
        {
            self.repo.delete_skill(department, &skill_id).await?;
            self.resources.delete_skill(department, &skill_id).await?;
            return Err(err);
        }
        Ok(SkillSummary::from(&version))
    }

    /// Loads the head, hiding skills of other teams and enforcing owner
    /// and optimistic-lock rules for writes.
    async fn head_for_write(
        &self,
        department: &DepartmentId,
        who: &SkillIdentity,
        skill_id: &str,
        expected: u32,
    ) -> DomainResult<SkillVersion> {
        let (_, agent, _) = who.require_writer()?;
        let head = self.visible_head(department, who, skill_id).await?;
        if head.owner_agent_id != agent {
            return Err(skill_error(
                ErrorKind::PermissionDenied,
                "SKILL_NOT_OWNER",
                "only the owner agent can modify this skill",
            ));
        }
        if head.version != expected {
            return Err(skill_error(
                ErrorKind::Conflict,
                "SKILL_VERSION_STALE",
                format!("current_version={}", head.version),
            ));
        }
        Ok(head)
    }

    async fn visible_head(
        &self,
        department: &DepartmentId,
        who: &SkillIdentity,
        skill_id: &str,
    ) -> DomainResult<SkillVersion> {
        let head = self
            .repo
            .head(department, skill_id)
            .await?
            .ok_or_else(|| not_found(skill_id))?;
        // Team members read team skills; without a team only the owner
        // user does. Mismatches look like absence, so ids cannot be probed.
        let visible = match who.team_id.as_deref() {
            Some(team) => team == head.team_id,
            None => who.user_id.as_deref() == Some(head.owner_user_id.as_str()),
        };
        if !visible {
            return Err(not_found(skill_id));
        }
        Ok(head)
    }

    /// Appends v+1 (no-op when nothing changed): copies resources, applies
    /// removals then writes, and moves the head.
    async fn append_next(
        &self,
        department: &DepartmentId,
        who: &SkillIdentity,
        head: SkillVersion,
        content: Option<String>,
        writes: Vec<Resource>,
        removes: Vec<String>,
    ) -> DomainResult<SkillSummary> {
        let content = match content {
            Some(c) => self.redactor.redact(&c),
            None => head.content.clone(),
        };
        let fm = parse_frontmatter(&content)?;
        if fm.name != head.name {
            return Err(skill_error(
                ErrorKind::InvalidInput,
                "INVALID_FRONTMATTER",
                "skill name cannot change",
            ));
        }
        let removes: Vec<String> = removes
            .into_iter()
            .filter(|p| head.manifest.iter().any(|m| &m.path == p))
            .collect();
        if content_hash(&content) == head.content_hash && writes.is_empty() && removes.is_empty() {
            return Ok(SkillSummary::from(&head));
        }
        let next_version = head.version + 1;
        self.resources
            .copy_version(department, &head.skill_id, head.version, next_version)
            .await?;
        let mut manifest: Vec<ResourceEntry> = head
            .manifest
            .iter()
            .filter(|m| !removes.contains(&m.path))
            .cloned()
            .collect();
        for path in &removes {
            self.resources
                .remove(department, &head.skill_id, next_version, path)
                .await?;
        }
        self.write_resources(
            department,
            &head.skill_id,
            next_version,
            &writes,
            &mut manifest,
        )
        .await?;
        let next = SkillVersion {
            version: next_version,
            description: fm.description,
            content_hash: content_hash(&content),
            content,
            manifest,
            owner_user_id: who.user_id.clone().unwrap_or(head.owner_user_id.clone()),
            task_id: who.task_id.clone(),
            updated_at_ms: self.now_ms(),
            created_at_ms: self.now_ms(),
            ..head
        };
        self.repo.append_version(department, &next).await?;
        self.cleanup_versions(department, &next.skill_id).await;
        Ok(SkillSummary::from(&next))
    }

    async fn write_resources(
        &self,
        department: &DepartmentId,
        skill_id: &str,
        version: u32,
        resources: &[Resource],
        manifest: &mut Vec<ResourceEntry>,
    ) -> DomainResult<()> {
        for r in resources {
            self.resources
                .write(department, skill_id, version, &r.path, &r.bytes)
                .await?;
            manifest.retain(|m| m.path != r.path);
            manifest.push(ResourceEntry {
                path: r.path.clone(),
                size_bytes: r.bytes.len() as u64,
                mime_type: r.mime_type.clone(),
                is_executable: r.is_executable,
            });
        }
        let total: u64 = manifest.iter().map(|m| m.size_bytes).sum();
        if total as usize > MAX_TOTAL_RESOURCE_BYTES {
            return Err(skill_error(
                ErrorKind::TooLarge,
                "RESOURCE_TOO_LARGE",
                "skill resources exceed 50MB",
            ));
        }
        Ok(())
    }

    /// Keeps at most `max_versions` non-head versions and drops expired
    /// ones beyond the newest three. Best-effort.
    async fn cleanup_versions(&self, department: &DepartmentId, skill_id: &str) {
        let result: DomainResult<()> = async {
            let (versions, _) = self
                .repo
                .list_versions(department, skill_id, 10_000, 0)
                .await?;
            let now = self.now_ms();
            let ttl = self.settings.version_ttl_days * 86_400;
            let non_head: Vec<&SkillVersion> = versions.iter().filter(|v| !v.is_head).collect();
            let doomed: Vec<u32> = non_head
                .iter()
                .enumerate()
                .filter(|(i, v)| {
                    *i >= self.settings.max_versions || (*i >= 3 && v.is_expired(now, ttl))
                })
                .map(|(_, v)| v.version)
                .collect();
            if !doomed.is_empty() {
                self.repo
                    .delete_versions(department, skill_id, &doomed)
                    .await?;
                for v in doomed {
                    self.resources
                        .delete_version(department, skill_id, v)
                        .await?;
                }
            }
            Ok(())
        }
        .await;
        if let Err(err) = result {
            warn!(error = %err, skill_id, "skill version cleanup failed");
        }
    }

    pub async fn update(
        &self,
        department: &DepartmentId,
        who: &SkillIdentity,
        skill_id: &str,
        expected: u32,
        content: &str,
    ) -> DomainResult<SkillSummary> {
        let head = self
            .head_for_write(department, who, skill_id, expected)
            .await?;
        self.append_next(
            department,
            who,
            head,
            Some(content.to_owned()),
            Vec::new(),
            Vec::new(),
        )
        .await
    }

    pub async fn patch(
        &self,
        department: &DepartmentId,
        who: &SkillIdentity,
        skill_id: &str,
        expected: u32,
        patch: &StringPatch,
    ) -> DomainResult<SkillSummary> {
        let head = self
            .head_for_write(department, who, skill_id, expected)
            .await?;
        let content = apply_patch(&head.content, &patch.old, &patch.new, patch.replace_all)?;
        self.append_next(department, who, head, Some(content), Vec::new(), Vec::new())
            .await
    }

    pub async fn write_files(
        &self,
        department: &DepartmentId,
        who: &SkillIdentity,
        skill_id: &str,
        expected: u32,
        files: Vec<ResourceInput>,
    ) -> DomainResult<SkillSummary> {
        if files.is_empty() {
            return Err(DomainError::invalid("files must not be empty"));
        }
        let head = self
            .head_for_write(department, who, skill_id, expected)
            .await?;
        let writes = decode_resources(&self.redactor, files)?;
        self.append_next(department, who, head, None, writes, Vec::new())
            .await
    }

    pub async fn remove_files(
        &self,
        department: &DepartmentId,
        who: &SkillIdentity,
        skill_id: &str,
        expected: u32,
        paths: Vec<String>,
    ) -> DomainResult<SkillSummary> {
        if paths.is_empty() || paths.len() > 100 {
            return Err(DomainError::invalid("paths must contain 1-100 items"));
        }
        let head = self
            .head_for_write(department, who, skill_id, expected)
            .await?;
        self.append_next(department, who, head, None, Vec::new(), paths)
            .await
    }

    pub async fn delete(
        &self,
        department: &DepartmentId,
        who: &SkillIdentity,
        skill_id: &str,
        expected: u32,
    ) -> DomainResult<Value> {
        self.head_for_write(department, who, skill_id, expected)
            .await?;
        let deleted = self.repo.delete_skill(department, skill_id).await?;
        self.resources.delete_skill(department, skill_id).await?;
        if let Err(err) = self
            .meta
            .remove_assets(department, &[skill_id.to_owned()])
            .await
        {
            warn!(error = %err, skill_id, "failed to remove skill asset");
        }
        Ok(json!({"skill_id": skill_id, "archived": deleted > 0}))
    }

    async fn resolve_version(
        &self,
        department: &DepartmentId,
        head: SkillVersion,
        version: Option<u32>,
    ) -> DomainResult<SkillVersion> {
        let Some(v) = version.filter(|v| *v != head.version) else {
            return Ok(head);
        };
        let found = self
            .repo
            .version(department, &head.skill_id, v)
            .await?
            .ok_or_else(|| not_found(&head.skill_id))?;
        if found.is_expired(self.now_ms(), self.settings.version_ttl_days * 86_400) {
            return Err(skill_error(
                ErrorKind::Conflict,
                "SKILL_VERSION_EXPIRED",
                format!("latest_version={}", head.version),
            ));
        }
        Ok(found)
    }

    fn detail(v: SkillVersion, read: ReadOptions) -> SkillDetail {
        SkillDetail {
            summary: SkillSummary::from(&v),
            content_hash: v.content_hash,
            content: read.include_content.then_some(v.content),
            manifest: read.include_manifest.then_some(v.manifest),
        }
    }

    pub async fn get(
        &self,
        department: &DepartmentId,
        who: &SkillIdentity,
        skill_id: &str,
        read: ReadOptions,
    ) -> DomainResult<SkillDetail> {
        let head = self.visible_head(department, who, skill_id).await?;
        let v = self.resolve_version(department, head, read.version).await?;
        Ok(Self::detail(v, read))
    }

    pub async fn get_by_name(
        &self,
        department: &DepartmentId,
        team_id: &str,
        agent_id: &str,
        name: &str,
        read: ReadOptions,
    ) -> DomainResult<SkillDetail> {
        let head = self
            .repo
            .find_head_by_name(department, team_id, agent_id, name)
            .await?
            .ok_or_else(|| not_found(name))?;
        let v = self.resolve_version(department, head, read.version).await?;
        Ok(Self::detail(v, read))
    }

    fn filter_for(
        who: &SkillIdentity,
        owner_agent: Option<String>,
        name_prefix: Option<String>,
        statuses: Vec<SkillStatus>,
    ) -> SkillFilter {
        SkillFilter {
            team_id: who.team_id.clone(),
            owner_agent_id: owner_agent,
            owner_user_id: if who.team_id.is_none() {
                who.user_id.clone()
            } else {
                None
            },
            name_prefix,
            statuses: if statuses.is_empty() {
                vec![SkillStatus::Active]
            } else {
                statuses
            },
        }
    }

    pub async fn list(
        &self,
        department: &DepartmentId,
        who: &SkillIdentity,
        query: ListQuery,
    ) -> DomainResult<(Vec<SkillSummary>, usize)> {
        let filter = Self::filter_for(who, query.owner_agent_id, query.name_prefix, query.statuses);
        let (heads, total) = self
            .repo
            .list_heads(
                department,
                &filter,
                query.limit.unwrap_or(50).clamp(1, 1000),
                query.offset.unwrap_or(0),
            )
            .await?;
        Ok((heads.iter().map(SkillSummary::from).collect(), total))
    }

    /// BM25 over head versions; `team_scope` searches the whole team
    /// instead of the caller agent's skills.
    pub async fn search(
        &self,
        department: &DepartmentId,
        who: &SkillIdentity,
        query: &str,
        top_k: Option<usize>,
        team_scope: bool,
    ) -> DomainResult<Vec<SearchHit>> {
        if query.trim().is_empty() || query.chars().count() > 2048 {
            return Err(DomainError::invalid("query must be 1-2048 characters"));
        }
        let owner = if team_scope {
            None
        } else {
            who.agent_id.clone()
        };
        let filter = Self::filter_for(who, owner, None, Vec::new());
        let hits = self
            .repo
            .search_heads(department, &filter, query, top_k.unwrap_or(10).clamp(1, 50))
            .await?;
        Ok(hits
            .into_iter()
            .map(|(v, score, snippet)| SearchHit {
                summary: SkillSummary::from(&v),
                score,
                snippet,
            })
            .collect())
    }

    pub async fn versions(
        &self,
        department: &DepartmentId,
        who: &SkillIdentity,
        skill_id: &str,
        limit: Option<usize>,
        offset: Option<usize>,
    ) -> DomainResult<(Vec<VersionItem>, usize)> {
        self.visible_head(department, who, skill_id).await?;
        let (versions, total) = self
            .repo
            .list_versions(
                department,
                skill_id,
                limit.unwrap_or(50).clamp(1, 1000),
                offset.unwrap_or(0),
            )
            .await?;
        let now = self.now_ms();
        let ttl = self.settings.version_ttl_days * 86_400;
        Ok((
            versions
                .iter()
                .map(|v| VersionItem {
                    summary: SkillSummary::from(v),
                    is_expired: v.is_expired(now, ttl),
                })
                .collect(),
            total,
        ))
    }

    pub async fn read_file(
        &self,
        department: &DepartmentId,
        who: &SkillIdentity,
        skill_id: &str,
        path: &str,
        version: Option<u32>,
        encoding: Option<&str>,
    ) -> DomainResult<FileContent> {
        validate_resource_path(path)?;
        let head = self.visible_head(department, who, skill_id).await?;
        let v = self.resolve_version(department, head, version).await?;
        let entry = v
            .manifest
            .iter()
            .find(|m| m.path == path)
            .ok_or_else(|| not_found(path))?;
        let bytes = self
            .resources
            .read(department, skill_id, v.version, path)
            .await?
            .ok_or_else(|| {
                skill_error(
                    ErrorKind::Unavailable,
                    "STORAGE_NOT_FOUND",
                    "resource missing from storage",
                )
            })?;
        let (content, encoding) = match (encoding, String::from_utf8(bytes.clone())) {
            (Some("base64"), _) | (None, Err(_)) => (base64_encode(&bytes), "base64"),
            (_, Ok(text)) => (text, "utf-8"),
            (_, Err(_)) => (base64_encode(&bytes), "base64"),
        };
        Ok(FileContent {
            path: path.to_owned(),
            content,
            version: v.version,
            size_bytes: bytes.len(),
            encoding: encoding.into(),
            mime_type: entry.mime_type.clone(),
        })
    }

    /// The `<available_skills>` block: everything when the agent has fewer
    /// than `top_k` skills, otherwise the best matches for `query`.
    pub async fn listing(
        &self,
        department: &DepartmentId,
        who: &SkillIdentity,
        query: Option<&str>,
        char_budget: Option<usize>,
    ) -> DomainResult<Listing> {
        let budget = char_budget.unwrap_or(DEFAULT_LISTING_BUDGET).min(64_000);
        let top_k = self.settings.listing_top_k;
        let (mode, skills): (&'static str, Vec<SkillVersion>) = match query
            .filter(|q| !q.trim().is_empty())
        {
            Some(q) => {
                let filter = Self::filter_for(who, who.agent_id.clone(), None, Vec::new());
                (
                    "search",
                    self.repo
                        .search_heads(department, &filter, q, top_k)
                        .await?
                        .into_iter()
                        .map(|(v, _, _)| v)
                        .collect(),
                )
            }
            None => {
                let filter = Self::filter_for(who, who.agent_id.clone(), None, Vec::new());
                let (heads, total) = self.repo.list_heads(department, &filter, top_k, 0).await?;
                (if total < top_k { "full" } else { "search" }, heads)
            }
        };
        let lines: Vec<ListingLine<'_>> = skills
            .iter()
            .map(|s| ListingLine {
                skill_id: &s.skill_id,
                name: &s.name,
                description: &s.description,
            })
            .collect();
        let (listing, visible) = render_listing(&lines, budget);
        let hits = skills
            .iter()
            .take(visible)
            .map(|s| ListingHit {
                skill_id: s.skill_id.clone(),
                version: s.version,
                name: s.name.clone(),
            })
            .collect();
        Ok(Listing {
            mode,
            listing,
            hits,
        })
    }
}

fn content_hash(content: &str) -> String {
    crate::pipeline::generation_logs::hex_sha256(content)
}

fn base64_encode(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let clean: String = text.chars().filter(|c| !c.is_ascii_whitespace()).collect();
    base64::engine::general_purpose::STANDARD.decode(clean).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_round_trips() {
        for sample in [&b""[..], b"a", b"ab", b"abc", b"hello world\xff"] {
            assert_eq!(base64_decode(&base64_encode(sample)).unwrap(), sample);
        }
        assert_eq!(base64_encode(b"Man"), "TWFu");
    }
}
