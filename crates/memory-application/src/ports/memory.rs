//! Storage ports for the four memory layers.

use async_trait::async_trait;
use memory_domain::error::DomainResult;
use memory_domain::memory::{AtomType, AtomicMemory, ConversationMessage};
use memory_domain::profile::{ProfileFile, ProfileScope};
use memory_domain::scope::{DepartmentId, MemoryScope};
use serde::Serialize;

use crate::common::{Page, Scored, TimeRange};

#[derive(Debug, Clone, Default)]
pub struct ConversationFilter {
    pub session_id: Option<String>,
    pub time: TimeRange,
}

#[derive(Debug, Clone, Default)]
pub struct AtomFilter {
    pub atom_type: Option<AtomType>,
    pub time: TimeRange,
}

/// A row with its storage sequence number. Sequences are assigned by the
/// store, strictly increasing and never reused (a rewrite gets a new one),
/// so they make exact pipeline cursors regardless of client timestamps.
#[derive(Debug, Clone)]
pub struct Sequenced<T> {
    pub seq: i64,
    pub item: T,
}

/// Which slice of a department an operation addresses. L0/L1 rows carry the
/// full scope; L2 consumes every user's atoms of a team + agent.
#[derive(Debug, Clone)]
pub enum AtomSelector<'a> {
    Scope(&'a MemoryScope),
    Profile(&'a ProfileScope),
}

/// L0 storage; implementations index content for keyword search.
#[async_trait]
pub trait ConversationRepository: Send + Sync {
    async fn append(
        &self,
        department: &DepartmentId,
        messages: &[ConversationMessage],
    ) -> DomainResult<()>;
    async fn query(
        &self,
        department: &DepartmentId,
        scope: &MemoryScope,
        filter: &ConversationFilter,
        page: Page,
    ) -> DomainResult<(Vec<ConversationMessage>, usize)>;
    async fn search(
        &self,
        department: &DepartmentId,
        scope: &MemoryScope,
        query: &str,
        filter: &ConversationFilter,
        limit: usize,
    ) -> DomainResult<Vec<Scored<ConversationMessage>>>;
    async fn count(
        &self,
        department: &DepartmentId,
        scope: Option<&MemoryScope>,
        filter: &ConversationFilter,
    ) -> DomainResult<usize>;
    async fn delete_by_ids(
        &self,
        department: &DepartmentId,
        scope: &MemoryScope,
        ids: &[String],
    ) -> DomainResult<usize>;
    async fn delete_by_sessions(
        &self,
        department: &DepartmentId,
        scope: &MemoryScope,
        sessions: &[String],
    ) -> DomainResult<usize>;
    /// Deletes every message of a team + agent (chat-memory clear).
    async fn delete_profile(
        &self,
        department: &DepartmentId,
        profile: &ProfileScope,
    ) -> DomainResult<usize>;
    /// Messages of one session stored after sequence `after` (exclusive),
    /// in storage order. Drives L1 extraction.
    async fn after_seq(
        &self,
        department: &DepartmentId,
        scope: &MemoryScope,
        session_id: &str,
        after: Option<i64>,
        limit: usize,
    ) -> DomainResult<Vec<Sequenced<ConversationMessage>>>;
}

/// L1 storage with keyword and optional vector search.
#[async_trait]
pub trait AtomRepository: Send + Sync {
    async fn upsert(
        &self,
        department: &DepartmentId,
        atom: &AtomicMemory,
        embedding: Option<&[f32]>,
    ) -> DomainResult<()>;
    async fn get(&self, department: &DepartmentId, id: &str) -> DomainResult<Option<AtomicMemory>>;
    async fn query(
        &self,
        department: &DepartmentId,
        selector: AtomSelector<'_>,
        filter: &AtomFilter,
        page: Page,
    ) -> DomainResult<(Vec<AtomicMemory>, usize)>;
    /// Atoms of a team + agent written after sequence `after` (exclusive),
    /// in write order. Drives L2 consolidation.
    async fn after_seq(
        &self,
        department: &DepartmentId,
        profile: &ProfileScope,
        after: Option<i64>,
        limit: usize,
    ) -> DomainResult<Vec<Sequenced<AtomicMemory>>>;
    async fn keyword_search(
        &self,
        department: &DepartmentId,
        scope: &MemoryScope,
        query: &str,
        filter: &AtomFilter,
        limit: usize,
    ) -> DomainResult<Vec<Scored<AtomicMemory>>>;
    async fn vector_search(
        &self,
        department: &DepartmentId,
        scope: &MemoryScope,
        embedding: &[f32],
        filter: &AtomFilter,
        limit: usize,
    ) -> DomainResult<Vec<Scored<AtomicMemory>>>;
    async fn count(
        &self,
        department: &DepartmentId,
        scope: Option<&MemoryScope>,
        filter: &AtomFilter,
    ) -> DomainResult<usize>;
    async fn delete(
        &self,
        department: &DepartmentId,
        scope: &MemoryScope,
        ids: &[String],
    ) -> DomainResult<usize>;
    async fn delete_profile(
        &self,
        department: &DepartmentId,
        profile: &ProfileScope,
    ) -> DomainResult<usize>;
}

/// Versioned profile files (scenes + persona) of a team + agent.
#[async_trait]
pub trait ProfileFileRepository: Send + Sync {
    /// Head versions of files whose path starts with `prefix`.
    async fn list(
        &self,
        department: &DepartmentId,
        profile: &ProfileScope,
        prefix: &str,
    ) -> DomainResult<Vec<ProfileFile>>;
    async fn read(
        &self,
        department: &DepartmentId,
        profile: &ProfileScope,
        path: &str,
        version: Option<u32>,
    ) -> DomainResult<Option<ProfileFile>>;
    /// Writes a new head version; identical content keeps the version.
    async fn write(
        &self,
        department: &DepartmentId,
        profile: &ProfileScope,
        path: &str,
        content: &str,
    ) -> DomainResult<ProfileFile>;
    /// Removes `path`, or every file below it when it ends with `/`.
    async fn remove(
        &self,
        department: &DepartmentId,
        profile: &ProfileScope,
        path: &str,
    ) -> DomainResult<usize>;
    async fn count(
        &self,
        department: &DepartmentId,
        profile: Option<&ProfileScope>,
        prefix: &str,
    ) -> DomainResult<usize>;
}

/// Rows moved by [`ScopeMigrationStore::reassign_default`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct MovedRows {
    pub conversations: usize,
    pub atoms: usize,
    pub profile_files: usize,
    pub skill_versions: usize,
}

/// Upgrade path for data written before the data plane required a real
/// team and agent: re-homes the `default` buckets in one transaction.
#[async_trait]
pub trait ScopeMigrationStore: Send + Sync {
    /// Moves L0/L1 rows of `from` (team/agent/user), and L2/L3 files and
    /// skills of team+agent `from`, to `to`. Upstream always passes
    /// `default/default/default`. With `dry_run` the transaction is rolled
    /// back and only the counts are reported.
    async fn reassign_default(
        &self,
        department: &DepartmentId,
        from: &MemoryScope,
        to: &MemoryScope,
        dry_run: bool,
    ) -> DomainResult<MovedRows>;
}

/// Raw session-turn objects written by `/v3/session/add`.
#[async_trait]
pub trait SessionObjectStore: Send + Sync {
    /// Stores `bytes` under `key` (relative to the department root).
    async fn put(&self, department: &DepartmentId, key: &str, bytes: &[u8]) -> DomainResult<()>;
}
