//! Isolation scope: every memory record belongs to exactly one
//! `(department, team, agent, user)` bucket. Missing dimensions fall back to
//! the `default` bucket, mirroring the original v3 data plane.

use serde::{Deserialize, Serialize};

use crate::error::DomainError;

pub const DEFAULT_BUCKET: &str = "default";
const MAX_SEGMENT_BYTES: usize = 256;

/// A validated identifier that is safe to use as a storage path segment.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SegmentId(String);

impl SegmentId {
    pub fn parse(field: &str, raw: &str) -> Result<Self, DomainError> {
        let invalid = |why: &str| DomainError::invalid(format!("{field}: {why}"));
        if raw.is_empty() {
            return Err(invalid("must not be empty"));
        }
        if raw.len() > MAX_SEGMENT_BYTES {
            return Err(invalid("exceeds 256 bytes"));
        }
        if raw == "." || raw == ".." {
            return Err(invalid("must not be '.' or '..'"));
        }
        if raw.chars().any(|c| c == '/' || c == '\\' || c.is_control()) {
            return Err(invalid("contains '/', '\\' or control characters"));
        }
        Ok(Self(raw.to_owned()))
    }

    pub fn default_bucket() -> Self {
        Self(DEFAULT_BUCKET.to_owned())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for SegmentId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// The memory department a request targets (`x-memory-department-id`).
pub type DepartmentId = SegmentId;

/// Team + agent + user triple that isolates memory data inside a department.
/// `task_id` is an optional narrowing tag, not an isolation boundary.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MemoryScope {
    pub team_id: SegmentId,
    pub agent_id: SegmentId,
    pub user_id: SegmentId,
    pub task_id: Option<SegmentId>,
}

impl MemoryScope {
    /// Builds a scope from optional raw values; absent or empty values fall
    /// back to the `default` bucket.
    pub fn from_optional(
        team_id: Option<&str>,
        agent_id: Option<&str>,
        user_id: Option<&str>,
        task_id: Option<&str>,
    ) -> Result<Self, DomainError> {
        Ok(Self {
            team_id: bucket_or_default("team_id", team_id)?,
            agent_id: bucket_or_default("agent_id", agent_id)?,
            user_id: bucket_or_default("user_id", user_id)?,
            task_id: non_empty(task_id)
                .map(|t| SegmentId::parse("task_id", t))
                .transpose()?,
        })
    }

    /// Stable key used for per-scope pipeline state and file layout.
    pub fn key(&self) -> String {
        format!("{}/{}/{}", self.team_id, self.agent_id, self.user_id)
    }
}

fn non_empty(raw: Option<&str>) -> Option<&str> {
    raw.filter(|s| !s.trim().is_empty())
}

fn bucket_or_default(field: &str, raw: Option<&str>) -> Result<SegmentId, DomainError> {
    match non_empty(raw) {
        Some(value) => SegmentId::parse(field, value),
        None => Ok(SegmentId::default_bucket()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_dimensions_fall_back_to_default_bucket() {
        let scope = MemoryScope::from_optional(Some("t1"), None, Some(""), None).unwrap();
        assert_eq!(scope.team_id.as_str(), "t1");
        assert_eq!(scope.agent_id.as_str(), DEFAULT_BUCKET);
        assert_eq!(scope.user_id.as_str(), DEFAULT_BUCKET);
        assert_eq!(scope.key(), "t1/default/default");
    }

    #[test]
    fn rejects_path_traversal_segments() {
        for bad in ["..", ".", "a/b", "a\\b", "a\nb"] {
            assert!(
                SegmentId::parse("x", bad).is_err(),
                "{bad:?} must be rejected"
            );
        }
    }
}
