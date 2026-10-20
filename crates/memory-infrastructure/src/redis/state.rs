//! `PipelineStateRepository` over Redis: one JSON string per state, plus
//! sorted-set indexes so "what is due" is a range query and per-department
//! listing/deletion never scans the keyspace.

use ::redis::aio::ConnectionManager;
use ::redis::{cmd, pipe, Pipeline};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use memory_application::ports::pipeline::{PipelineStateRepository, ProfileState, SessionState};
use memory_domain::error::DomainResult;
use memory_domain::profile::ProfileScope;
use memory_domain::scope::{DepartmentId, MemoryScope};
use serde::de::DeserializeOwned;

use super::{from_json, to_json, unavailable, KeySpace};

/// Same page size as the SQLite store: a tick handles at most this many.
const DUE_BATCH: isize = 500;

/// The two kinds of state share layout; this names their keys.
#[derive(Clone, Copy)]
enum Kind {
    Session,
    Profile,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Self::Session => "session",
            Self::Profile => "profile",
        }
    }

    fn plural(self) -> &'static str {
        match self {
            Self::Session => "sessions",
            Self::Profile => "profiles",
        }
    }
}

pub struct RedisPipelineState {
    conn: ConnectionManager,
    keys: KeySpace,
}

impl RedisPipelineState {
    pub fn new(conn: ConnectionManager, prefix: &KeySpace) -> Self {
        Self {
            conn,
            keys: prefix.child("pipe"),
        }
    }

    fn doc_key(&self, kind: Kind, id: &str) -> String {
        self.keys.key(&format!("{}:{id}", kind.name()))
    }

    fn due_key(&self, kind: Kind) -> String {
        self.keys.key(&format!("due:{}", kind.plural()))
    }

    fn department_key(&self, kind: Kind, department: &DepartmentId) -> String {
        self.keys
            .key(&format!("department:{department}:{}", kind.plural()))
    }

    /// Writes the document and its index entries in one MULTI, so readers
    /// never see a due entry without its document (or a stale due time).
    async fn put(
        &self,
        kind: Kind,
        department: &DepartmentId,
        id: &str,
        doc: String,
        due_ms: Option<i64>,
        order_ms: i64,
    ) -> DomainResult<()> {
        let mut p = pipe();
        p.atomic();
        p.cmd("SET").arg(self.doc_key(kind, id)).arg(doc).ignore();
        match due_ms {
            Some(ms) => p.cmd("ZADD").arg(self.due_key(kind)).arg(ms).arg(id),
            // A cleared deadline must leave the due index too.
            None => p.cmd("ZREM").arg(self.due_key(kind)).arg(id),
        }
        .ignore();
        p.cmd("ZADD")
            .arg(self.department_key(kind, department))
            .arg(order_ms)
            .arg(id)
            .ignore();
        p.exec_async(&mut self.conn.clone())
            .await
            .map_err(unavailable)
    }

    async fn get<T: DeserializeOwned>(&self, kind: Kind, id: &str) -> DomainResult<Option<T>> {
        let doc: Option<String> = cmd("GET")
            .arg(self.doc_key(kind, id))
            .query_async(&mut self.conn.clone())
            .await
            .map_err(unavailable)?;
        doc.map(|d| from_json(&d)).transpose()
    }

    /// Loads documents by id, skipping ids whose document vanished
    /// (deleted between reading the index and the documents).
    async fn load<T: DeserializeOwned>(&self, kind: Kind, ids: &[String]) -> DomainResult<Vec<T>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let keys: Vec<String> = ids.iter().map(|id| self.doc_key(kind, id)).collect();
        let docs: Vec<Option<String>> = cmd("MGET")
            .arg(keys)
            .query_async(&mut self.conn.clone())
            .await
            .map_err(unavailable)?;
        docs.iter().flatten().map(|d| from_json(d)).collect()
    }

    async fn due<T: DeserializeOwned>(
        &self,
        kind: Kind,
        now: DateTime<Utc>,
    ) -> DomainResult<Vec<T>> {
        let ids: Vec<String> = cmd("ZRANGE")
            .arg(self.due_key(kind))
            .arg("-inf")
            .arg(now.timestamp_millis())
            .arg("BYSCORE")
            .arg("LIMIT")
            .arg(0)
            .arg(DUE_BATCH)
            .query_async(&mut self.conn.clone())
            .await
            .map_err(unavailable)?;
        self.load(kind, &ids).await
    }

    /// Most recent first, like the SQLite store's `ORDER BY … DESC`.
    async fn list<T: DeserializeOwned>(
        &self,
        kind: Kind,
        department: &DepartmentId,
        limit: usize,
    ) -> DomainResult<Vec<T>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let ids = self
            .department_ids(kind, department, limit as isize - 1)
            .await?;
        self.load(kind, &ids).await
    }

    /// Ids of a department, newest first, up to index `stop` (-1 = all).
    async fn department_ids(
        &self,
        kind: Kind,
        department: &DepartmentId,
        stop: isize,
    ) -> DomainResult<Vec<String>> {
        cmd("ZRANGE")
            .arg(self.department_key(kind, department))
            .arg(0)
            .arg(stop)
            .arg("REV")
            .query_async(&mut self.conn.clone())
            .await
            .map_err(unavailable)
    }

    /// Queues removal of the documents and all their index entries.
    fn remove(&self, p: &mut Pipeline, kind: Kind, department: &DepartmentId, ids: &[String]) {
        if ids.is_empty() {
            return;
        }
        let docs: Vec<String> = ids.iter().map(|id| self.doc_key(kind, id)).collect();
        p.cmd("DEL").arg(docs).ignore();
        p.cmd("ZREM").arg(self.due_key(kind)).arg(ids).ignore();
        p.cmd("ZREM")
            .arg(self.department_key(kind, department))
            .arg(ids)
            .ignore();
    }
}

/// `/` never occurs inside a segment, so ids split unambiguously and a
/// team + agent prefix selects exactly that profile's sessions.
fn session_id(department: &DepartmentId, scope: &MemoryScope, session_id: &str) -> String {
    format!("{department}/{}/{session_id}", scope.key())
}

fn profile_id(department: &DepartmentId, profile: &ProfileScope) -> String {
    format!("{department}/{}/{}", profile.team_id, profile.agent_id)
}

#[async_trait]
impl PipelineStateRepository for RedisPipelineState {
    async fn get_session(
        &self,
        department: &DepartmentId,
        scope: &MemoryScope,
        session: &str,
    ) -> DomainResult<Option<SessionState>> {
        self.get(Kind::Session, &session_id(department, scope, session))
            .await
    }

    async fn put_session(&self, state: &SessionState) -> DomainResult<()> {
        self.put(
            Kind::Session,
            &state.department,
            &session_id(&state.department, &state.scope, &state.session_id),
            to_json(state)?,
            state.l1_due_at.map(|t| t.timestamp_millis()),
            state.last_active_at.timestamp_millis(),
        )
        .await
    }

    async fn get_profile(
        &self,
        department: &DepartmentId,
        profile: &ProfileScope,
    ) -> DomainResult<Option<ProfileState>> {
        self.get(Kind::Profile, &profile_id(department, profile))
            .await
    }

    async fn put_profile(&self, state: &ProfileState) -> DomainResult<()> {
        self.put(
            Kind::Profile,
            &state.department,
            &profile_id(&state.department, &state.profile),
            to_json(state)?,
            state.l2_due_at.map(|t| t.timestamp_millis()),
            Utc::now().timestamp_millis(),
        )
        .await
    }

    async fn due_sessions(&self, now: DateTime<Utc>) -> DomainResult<Vec<SessionState>> {
        self.due(Kind::Session, now).await
    }

    async fn due_profiles(&self, now: DateTime<Utc>) -> DomainResult<Vec<ProfileState>> {
        self.due(Kind::Profile, now).await
    }

    async fn list_sessions(
        &self,
        department: &DepartmentId,
        limit: usize,
    ) -> DomainResult<Vec<SessionState>> {
        self.list(Kind::Session, department, limit).await
    }

    async fn list_profiles(
        &self,
        department: &DepartmentId,
        limit: usize,
    ) -> DomainResult<Vec<ProfileState>> {
        self.list(Kind::Profile, department, limit).await
    }

    /// Also drops the sessions of that team + agent (any user), like the
    /// SQLite store.
    async fn delete_profile(
        &self,
        department: &DepartmentId,
        profile: &ProfileScope,
    ) -> DomainResult<()> {
        let id = profile_id(department, profile);
        let session_prefix = format!("{id}/");
        let sessions: Vec<String> = self
            .department_ids(Kind::Session, department, -1)
            .await?
            .into_iter()
            .filter(|s| s.starts_with(&session_prefix))
            .collect();
        let mut p = pipe();
        p.atomic();
        self.remove(&mut p, Kind::Profile, department, &[id]);
        self.remove(&mut p, Kind::Session, department, &sessions);
        p.exec_async(&mut self.conn.clone())
            .await
            .map_err(unavailable)
    }

    async fn delete_department(&self, department: &DepartmentId) -> DomainResult<()> {
        let sessions = self.department_ids(Kind::Session, department, -1).await?;
        let profiles = self.department_ids(Kind::Profile, department, -1).await?;
        let mut p = pipe();
        p.atomic();
        self.remove(&mut p, Kind::Session, department, &sessions);
        self.remove(&mut p, Kind::Profile, department, &profiles);
        p.cmd("DEL")
            .arg(self.department_key(Kind::Session, department))
            .arg(self.department_key(Kind::Profile, department))
            .ignore();
        p.exec_async(&mut self.conn.clone())
            .await
            .map_err(unavailable)
    }
}
