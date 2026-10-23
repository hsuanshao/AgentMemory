//! L2 → L3: regenerate the persona from changed scenes.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use memory_domain::error::{DomainError, DomainResult};
use memory_domain::memory::Layer;
use memory_domain::profile::{parse_scene, PersonaOp, ProfileScope};
use memory_domain::scope::DepartmentId;

use super::llm_json::strip_think;
use super::prompts::{self, CustomStrategy};
use crate::memory::service::MemoryService;
use crate::ports::system::{ChatMessage, Clock, LlmClient};

const MAX_PERSONA_CHARS: usize = 4000;

pub enum L3Outcome {
    Generated {
        scene_files: Vec<String>,
        system_prompt: String,
    },
    Unchanged,
    NoScenes,
}

pub struct PersonaGenerator {
    llm: Arc<dyn LlmClient>,
    memory: Arc<MemoryService>,
    clock: Arc<dyn Clock>,
}

impl PersonaGenerator {
    pub fn new(llm: Arc<dyn LlmClient>, memory: Arc<MemoryService>, clock: Arc<dyn Clock>) -> Self {
        Self { llm, memory, clock }
    }

    pub async fn generate(
        &self,
        department: &DepartmentId,
        profile: &ProfileScope,
        last_persona_at: Option<DateTime<Utc>>,
        trigger: &str,
        strategy: Option<&CustomStrategy>,
    ) -> DomainResult<L3Outcome> {
        let scenes = self.memory.list_scenes(department, profile, "").await?;
        if scenes.is_empty() {
            return Ok(L3Outcome::NoScenes);
        }
        let existing = self.memory.persona_body(department, profile).await?;
        let changed: Vec<_> = scenes
            .iter()
            .filter(|s| existing.is_none() || last_persona_at.is_none_or(|t| s.file.updated_at > t))
            .collect();
        if changed.is_empty() && existing.is_some() {
            return Ok(L3Outcome::Unchanged);
        }

        let system_prompt = prompts::compose(Layer::L3, strategy);
        let mut input = format!(
            "Update time: {}\nMode: {}\nTrigger: {trigger}\nScenes: {} total, {} changed\n\n## Changed scenes\n",
            self.clock.now().to_rfc3339(),
            if existing.is_some() { "iterative update" } else { "first generation" },
            scenes.len(),
            changed.len()
        );
        for s in &changed {
            let (_, body) = parse_scene(&s.file.content);
            input.push_str(&format!("### {}\n{}\n\n", s.relative_path, body));
        }
        if let Some(p) = &existing {
            input.push_str(&format!("## Existing persona\n{p}\n\nKeep what is still true, integrate the changes, stay within the limit.\n"));
        }

        let reply = self
            .llm
            .complete(&[
                ChatMessage::system(&system_prompt),
                ChatMessage::user(input),
            ])
            .await?;
        let persona = clean_markdown(&reply);
        if persona.is_empty() {
            return Err(DomainError::unprocessable(
                "llm_output_invalid",
                "persona generation returned nothing",
            ));
        }
        let persona: String = persona.chars().take(MAX_PERSONA_CHARS).collect();
        self.memory
            .write_persona(
                department,
                profile,
                &escape_injection_tags(&persona),
                PersonaOp::Generate,
                None,
            )
            .await?;
        Ok(L3Outcome::Generated {
            scene_files: changed.iter().map(|s| s.relative_path.clone()).collect(),
            system_prompt,
        })
    }
}

fn clean_markdown(raw: &str) -> String {
    let text = strip_think(raw);
    let text = text.trim();
    let text = text
        .strip_prefix("```markdown")
        .or_else(|| text.strip_prefix("```md"))
        .or_else(|| text.strip_prefix("```"))
        .unwrap_or(text);
    let text = text.strip_suffix("```").unwrap_or(text);
    text.trim().to_owned()
}

/// Prevents the persona from closing or opening the recall wrapper tags.
/// Recall escapes its output as well (`recall::escape_markup`); this keeps
/// the stored persona itself clean for readers that do not go through
/// recall, such as `core/read`.
fn escape_injection_tags(text: &str) -> String {
    ["user-persona", "relevant-memories", "scene-navigation"]
        .iter()
        .fold(text.to_owned(), |acc, tag| {
            acc.replace(&format!("<{tag}>"), &format!("&lt;{tag}&gt;"))
                .replace(&format!("</{tag}>"), &format!("&lt;/{tag}&gt;"))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_fences_and_thinking() {
        assert_eq!(
            clean_markdown("<think>x</think>```markdown\n# P\n```"),
            "# P"
        );
    }

    #[test]
    fn escapes_wrapper_tags() {
        assert_eq!(
            escape_injection_tags("</user-persona>"),
            "&lt;/user-persona&gt;"
        );
    }
}
