//! Storage ports for the four memory layers.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use memory_domain::error::DomainResult;
use memory_domain::memory::{AtomType, AtomicMemory, ConversationMessage};
use memory_domain::profile::{
    NewPersonaVersion, NewSceneVersion, PersonaVersion, ProfileScope, SceneAtom, SceneVersion,
    SceneWriteLinks, StaleMark,
};
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
    pub session_id: Option<String>,
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
    /// Departments that have stored data, for sweeps across all of them.
    async fn departments(&self) -> DomainResult<Vec<DepartmentId>>;
    /// Atoms stored without an embedding (the embedding call failed or
    /// timed out) with a seq after `after`, in seq order.
    async fn missing_embeddings(
        &self,
        department: &DepartmentId,
        after: Option<i64>,
        limit: usize,
    ) -> DomainResult<Vec<Sequenced<AtomicMemory>>>;
    /// Adds the embedding of an atom that has none, without rewriting the
    /// atom: its seq stays, so L2 does not see it as new. Does nothing when
    /// the atom changed since `updated_at` or got an embedding meanwhile;
    /// returns whether it was set.
    async fn set_embedding(
        &self,
        department: &DepartmentId,
        id: &str,
        updated_at: DateTime<Utc>,
        embedding: &[f32],
    ) -> DomainResult<bool>;
}

/// Append-only versions of the scenes (L2) and persona (L3) of a team +
/// agent. Writes never change a stored version; only [`purge`] deletes.
///
/// [`purge`]: ProfileVersionStore::purge
#[async_trait]
pub trait ProfileVersionStore: Send + Sync {
    /// Latest version of every live scene whose slug starts with `prefix`,
    /// ordered by slug.
    async fn scene_heads(
        &self,
        department: &DepartmentId,
        profile: &ProfileScope,
        prefix: &str,
    ) -> DomainResult<Vec<SceneVersion>>;
    /// The scene currently named `slug` (the live one, else the one retired
    /// last), at `version` or its latest version.
    async fn scene(
        &self,
        department: &DepartmentId,
        profile: &ProfileScope,
        slug: &str,
        version: Option<u32>,
    ) -> DomainResult<Option<SceneVersion>>;
    /// Scene `scene_id` of the profile, at `version` or its latest version.
    /// Unlike [`scene`], this reaches scenes whose slug another scene took.
    ///
    /// [`scene`]: ProfileVersionStore::scene
    async fn scene_by_id(
        &self,
        department: &DepartmentId,
        profile: &ProfileScope,
        scene_id: &str,
        version: Option<u32>,
    ) -> DomainResult<Option<SceneVersion>>;
    /// Appends the versions in one transaction, each as its scene's next
    /// version. A version identical to its scene's latest one is not
    /// stored; the latest one is returned in its place. Creating a scene
    /// whose slug a live scene already has is a conflict.
    async fn append_scenes(
        &self,
        department: &DepartmentId,
        profile: &ProfileScope,
        versions: Vec<NewSceneVersion>,
    ) -> DomainResult<Vec<SceneVersion>> {
        self.append_scenes_linked(department, profile, versions, SceneWriteLinks::default())
            .await
    }
    /// [`append_scenes`], and in the same transaction what the write says
    /// about atoms:
    /// - a merge (`parents`) moves the absorbed scenes' current atoms and
    ///   open stale marks to the merged scene;
    /// - removing a scene (a `retire` without `merged_into`) moves its atoms
    ///   out;
    /// - a `manual` version settles the scene's stale marks;
    /// - `links.members` become the current atoms of the first version's
    ///   scene: one current elsewhere is moved out (marking that scene
    ///   `moved_out`), one deleted or rewritten since L2 read it marks this
    ///   scene, and the rewrite marks it settled;
    /// - `links.resolved` stale marks are settled.
    ///
    /// [`append_scenes`]: ProfileVersionStore::append_scenes
    async fn append_scenes_linked(
        &self,
        department: &DepartmentId,
        profile: &ProfileScope,
        versions: Vec<NewSceneVersion>,
        links: SceneWriteLinks,
    ) -> DomainResult<Vec<SceneVersion>>;
    /// Atoms placed in scene `scene_id` of the profile, current and moved
    /// out, of every user: callers filter by user.
    async fn scene_atoms(
        &self,
        department: &DepartmentId,
        profile: &ProfileScope,
        scene_id: &str,
    ) -> DomainResult<Vec<SceneAtom>>;
    /// Open stale marks of the profile's scenes (of `scene_ids` only, when
    /// given), oldest first.
    async fn stale_marks(
        &self,
        department: &DepartmentId,
        profile: &ProfileScope,
        scene_ids: Option<&[String]>,
    ) -> DomainResult<Vec<StaleMark>>;
    /// Counts one more L2 run that was given these marks and left them.
    async fn note_stale_attempt(
        &self,
        department: &DepartmentId,
        profile: &ProfileScope,
        ids: &[i64],
    ) -> DomainResult<()>;
    /// Live scenes whose slug starts with `prefix`.
    async fn count_scenes(
        &self,
        department: &DepartmentId,
        profile: Option<&ProfileScope>,
        prefix: &str,
    ) -> DomainResult<usize>;
    async fn persona(
        &self,
        department: &DepartmentId,
        profile: &ProfileScope,
        version: Option<u32>,
    ) -> DomainResult<Option<PersonaVersion>>;
    /// Appends the next version; a body identical to the latest one keeps
    /// the latest version.
    async fn append_persona(
        &self,
        department: &DepartmentId,
        profile: &ProfileScope,
        version: NewPersonaVersion,
    ) -> DomainResult<PersonaVersion>;
    /// Profiles with a persona.
    async fn count_personas(
        &self,
        department: &DepartmentId,
        profile: Option<&ProfileScope>,
    ) -> DomainResult<usize>;
    /// Deletes every scene and persona version of the profile, the one way
    /// history is removed (clearing a chat memory). Returns the number of
    /// scenes and personas that existed.
    async fn purge(&self, department: &DepartmentId, profile: &ProfileScope)
        -> DomainResult<usize>;
}

/// Rows moved by [`ScopeMigrationStore::reassign_default`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct MovedRows {
    pub conversations: usize,
    pub atoms: usize,
    /// Scene and persona rows (L2/L3), legacy files included.
    pub profile_files: usize,
    pub skill_versions: usize,
}

impl MovedRows {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
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
