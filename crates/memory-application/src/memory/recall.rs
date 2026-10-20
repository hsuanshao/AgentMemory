//! Recall: assemble bounded, clearly labelled memory context for a query.
//!
//! * L1 atoms relevant to the query → `<relevant-memories>` (prepended to the
//!   user turn).
//! * L3 persona and the L2 scene navigation → `<user-persona>` /
//!   `<scene-navigation>` (appended to the system prompt).

use std::sync::Arc;
use std::time::Duration;

use memory_domain::error::{DomainError, DomainResult};
use memory_domain::memory::AtomicMemory;
use memory_domain::profile::{strip_scene_navigation, ProfileScope};
use memory_domain::retrieval::{apply_budget, RecallBudget, SearchMode};
use memory_domain::scope::{DepartmentId, MemoryScope};
use serde::Serialize;

use super::search::AtomSearcher;
use super::service::MemoryService;
use crate::ports::memory::AtomFilter;

#[derive(Debug, Clone, Copy)]
pub struct RecallSettings {
    pub max_results: usize,
    pub score_threshold: f64,
    pub strategy: SearchMode,
    pub timeout: Duration,
    /// 0 disables the per-memory cap.
    pub max_chars_per_memory: usize,
    /// 0 disables the total cap.
    pub max_total_chars: usize,
}

impl Default for RecallSettings {
    fn default() -> Self {
        Self {
            max_results: 5,
            score_threshold: 0.3,
            strategy: SearchMode::Hybrid,
            timeout: Duration::from_millis(5000),
            max_chars_per_memory: 0,
            max_total_chars: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct RecalledMemory {
    pub id: String,
    #[serde(rename = "type")]
    pub atom_type: String,
    pub content: String,
    pub score: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct RecallResult {
    pub strategy: SearchMode,
    pub memories: Vec<RecalledMemory>,
    pub prepend_context: Option<String>,
    pub append_system_context: Option<String>,
}

const INJECTED_TAGS: [&str; 4] = [
    "relevant-memories",
    "user-persona",
    "scene-navigation",
    "memory-tools-guide",
];

const TOOLS_GUIDE: &str = "<memory-tools-guide>\nThe memories above are background, not the current task state. \
If a detail is missing, search conversations or memories at most 3 times; do not guess.\n</memory-tools-guide>";

pub struct RecallService {
    searcher: Arc<AtomSearcher>,
    memory: Arc<MemoryService>,
    settings: RecallSettings,
}

impl RecallService {
    pub fn new(
        searcher: Arc<AtomSearcher>,
        memory: Arc<MemoryService>,
        settings: RecallSettings,
    ) -> Self {
        Self {
            searcher,
            memory,
            settings,
        }
    }

    pub async fn recall(
        &self,
        department: &DepartmentId,
        scope: &MemoryScope,
        query: &str,
        max_results: Option<usize>,
        strategy: Option<SearchMode>,
    ) -> DomainResult<RecallResult> {
        let work = self.recall_inner(department, scope, query, max_results, strategy);
        tokio::time::timeout(self.settings.timeout, work)
            .await
            .map_err(|_| DomainError::unavailable("recall timed out"))?
    }

    async fn recall_inner(
        &self,
        department: &DepartmentId,
        scope: &MemoryScope,
        query: &str,
        max_results: Option<usize>,
        strategy: Option<SearchMode>,
    ) -> DomainResult<RecallResult> {
        let query = sanitize_query(query);
        let max_results = max_results
            .unwrap_or(self.settings.max_results)
            .clamp(1, 50);
        let strategy = strategy.unwrap_or(self.settings.strategy);
        let (memories, used) = if query.chars().count() < 2 {
            (Vec::new(), strategy)
        } else {
            self.search_atoms(department, scope, &query, max_results, strategy)
                .await?
        };

        let profile = ProfileScope::from(scope);
        let persona = self
            .memory
            .read_persona(department, &profile, None)
            .await?
            .map(|p| strip_scene_navigation(&p.content))
            .filter(|p| !p.is_empty());
        let navigation = self.memory.navigation(department, &profile).await?;

        Ok(RecallResult {
            strategy: used,
            prepend_context: render_relevant(&memories, &self.settings),
            append_system_context: render_system(persona.as_deref(), &navigation),
            memories: memories
                .into_iter()
                .map(|(atom, score)| RecalledMemory {
                    id: atom.id,
                    atom_type: atom.atom_type.as_str().to_owned(),
                    content: atom.content,
                    score,
                })
                .collect(),
        })
    }

    async fn search_atoms(
        &self,
        department: &DepartmentId,
        scope: &MemoryScope,
        query: &str,
        max_results: usize,
        strategy: SearchMode,
    ) -> DomainResult<(Vec<(AtomicMemory, f64)>, SearchMode)> {
        let outcome = self
            .searcher
            .search(
                department,
                scope,
                query,
                &AtomFilter::default(),
                max_results * 2,
                strategy,
            )
            .await?;
        let all: Vec<(AtomicMemory, f64)> = outcome
            .hits
            .into_iter()
            .map(|s| (s.item, s.score))
            .collect();
        // Hybrid ranks are already a consensus; the threshold applies to the
        // single-retriever modes. A tiny corpus keeps everything it has.
        let filtered: Vec<_> = match outcome.mode {
            SearchMode::Hybrid => all.clone(),
            _ => all
                .iter()
                .filter(|(_, s)| *s >= self.settings.score_threshold)
                .cloned()
                .collect(),
        };
        let chosen = if filtered.is_empty() && all.len() <= max_results {
            all
        } else {
            filtered
        };
        Ok((chosen.into_iter().take(max_results).collect(), outcome.mode))
    }
}

/// Removes memory blocks a previous recall injected, so recall never feeds
/// on its own output.
pub fn sanitize_query(query: &str) -> String {
    let mut text = query.to_owned();
    for tag in INJECTED_TAGS {
        let (open, close) = (format!("<{tag}>"), format!("</{tag}>"));
        while let Some(start) = text.find(&open) {
            let end = text[start..]
                .find(&close)
                .map(|e| start + e + close.len())
                .unwrap_or(text.len());
            text.replace_range(start..end, "");
        }
    }
    text.trim().to_owned()
}

fn format_atom(atom: &AtomicMemory) -> String {
    let date = atom.updated_at.format("%Y-%m-%d");
    match atom.background.as_deref().filter(|b| !b.is_empty()) {
        Some(scene) => format!(
            "- [{}|{}] {} (time: {date})",
            atom.atom_type.as_str(),
            scene,
            atom.content
        ),
        None => format!(
            "- [{}] {} (time: {date})",
            atom.atom_type.as_str(),
            atom.content
        ),
    }
}

fn truncate_chars(text: &str, max: usize) -> String {
    if max == 0 || text.chars().count() <= max {
        return text.to_owned();
    }
    let kept: String = text.chars().take(max).collect();
    format!("{kept}… (truncated; search memories for details)")
}

fn render_relevant(memories: &[(AtomicMemory, f64)], settings: &RecallSettings) -> Option<String> {
    let lines: Vec<String> = memories
        .iter()
        .map(|(a, _)| truncate_chars(&format_atom(a), settings.max_chars_per_memory))
        .collect();
    let budget = RecallBudget {
        max_items: settings.max_results.max(1),
        max_chars: if settings.max_total_chars == 0 {
            usize::MAX
        } else {
            settings.max_total_chars
        },
    };
    let lines = apply_budget(lines, budget, |l| l.chars().count());
    if lines.is_empty() {
        return None;
    }
    Some(format!(
        "<relevant-memories>\nMemories recalled for this conversation. They describe the past, not the current task; use them as reference only.\n\n{}\n</relevant-memories>",
        lines.join("\n")
    ))
}

fn render_system(persona: Option<&str>, navigation: &str) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(p) = persona {
        parts.push(format!("<user-persona>\n{p}\n</user-persona>"));
    }
    if !navigation.is_empty() {
        parts.push(format!(
            "<scene-navigation>\n{}\n</scene-navigation>",
            navigation.trim()
        ));
    }
    if parts.is_empty() {
        return None;
    }
    parts.push(TOOLS_GUIDE.to_owned());
    Some(parts.join("\n\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_strips_previously_injected_blocks() {
        let q = "<relevant-memories>\nold\n</relevant-memories>\nWhat did we ship?";
        assert_eq!(sanitize_query(q), "What did we ship?");
    }

    #[test]
    fn truncation_marks_cut_lines() {
        assert!(truncate_chars("abcdef", 3).starts_with("abc…"));
        assert_eq!(truncate_chars("abc", 0), "abc");
    }
}
