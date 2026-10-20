//! The four memory layers.
//!
//! * L0 [`ConversationMessage`] — raw turns, the source of truth.
//! * L1 [`AtomicMemory`] — facts / preferences / instructions distilled from L0.
//! * L2 scene blocks and L3 `persona.md` — see [`crate::profile::ProfileFile`].

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{DomainError, DomainResult};
use crate::scope::MemoryScope;

pub const MAX_MESSAGE_CHARS: usize = 8192;
pub const MAX_ATOM_CHARS: usize = 8192;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConversationMessage {
    pub id: String,
    pub scope: MemoryScope,
    pub session_id: String,
    pub role: Role,
    pub content: String,
    /// When the message happened, as reported by the caller.
    pub timestamp: DateTime<Utc>,
    /// When the message was persisted.
    pub recorded_at: DateTime<Utc>,
    pub version: u32,
}

/// Drops U+0000. PostgreSQL stores it in neither TEXT nor JSONB (SQLite
/// does), and in conversation text it is only ever noise from binary tool
/// output, so it is removed where text enters, whatever the backend.
pub fn without_nul(text: String) -> String {
    if text.contains('\0') {
        text.replace('\0', "")
    } else {
        text
    }
}

impl ConversationMessage {
    pub fn validate_content(content: &str) -> DomainResult<()> {
        let chars = content.chars().count();
        if chars == 0 || chars > MAX_MESSAGE_CHARS {
            return Err(DomainError::invalid(format!(
                "message content must be 1-{MAX_MESSAGE_CHARS} characters"
            )));
        }
        Ok(())
    }
}

/// The L1 taxonomy of the original engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AtomType {
    /// Something that happened ("released v2 on Monday").
    Episodic,
    /// A stable trait or preference of the user.
    Persona,
    /// A standing rule the agent must follow.
    Instruction,
}

impl AtomType {
    pub const ALL: [AtomType; 3] = [Self::Episodic, Self::Persona, Self::Instruction];

    /// Minimum priority worth keeping (instructions with -1 always pass).
    pub fn min_priority(self) -> i32 {
        match self {
            Self::Persona => 50,
            Self::Episodic => 60,
            Self::Instruction => 70,
        }
    }

    /// Accepts the canonical names plus the aliases LLMs commonly emit.
    pub fn parse_lenient(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "persona" | "preference" => Some(Self::Persona),
            "episodic" | "episode" | "event" => Some(Self::Episodic),
            "instruction" | "instruct" | "rule" => Some(Self::Instruction),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Episodic => "episodic",
            Self::Persona => "persona",
            Self::Instruction => "instruction",
        }
    }

    pub fn parse(raw: &str) -> DomainResult<Self> {
        Self::ALL
            .into_iter()
            .find(|t| t.as_str() == raw)
            .ok_or_else(|| DomainError::invalid(format!("unknown atom type: {raw}")))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AtomicMemory {
    pub id: String,
    pub scope: MemoryScope,
    pub atom_type: AtomType,
    pub content: String,
    pub background: Option<String>,
    /// 0-100 importance from the extractor; -1 marks a strict global rule.
    pub priority: i32,
    /// L0 message ids this atom was distilled from.
    pub source_message_ids: Vec<String>,
    pub session_id: Option<String>,
    pub version: u32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl AtomicMemory {
    /// Applies an edit and bumps the version, preserving identity.
    pub fn revise(
        &mut self,
        content: String,
        background: Option<String>,
        now: DateTime<Utc>,
    ) -> DomainResult<()> {
        if content.trim().is_empty() || content.chars().count() > MAX_ATOM_CHARS {
            return Err(DomainError::invalid(format!(
                "atom content must be 1-{MAX_ATOM_CHARS} characters"
            )));
        }
        self.content = content;
        if background.is_some() {
            self.background = background;
        }
        self.version += 1;
        self.updated_at = now;
        Ok(())
    }
}

/// Memory layer identifiers used by prompts, logs and the pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Layer {
    L1,
    L2,
    L3,
}

impl Layer {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::L1 => "l1",
            Self::L2 => "l2",
            Self::L3 => "l3",
        }
    }

    pub fn parse(raw: &str) -> DomainResult<Self> {
        match raw {
            "l1" => Ok(Self::L1),
            "l2" => Ok(Self::L2),
            "l3" => Ok(Self::L3),
            other => Err(DomainError::invalid(format!("unknown layer: {other}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn without_nul_drops_only_nul() {
        assert_eq!(without_nul("a\0b\0".into()), "ab");
        assert_eq!(without_nul("無 NUL".into()), "無 NUL");
    }
}
