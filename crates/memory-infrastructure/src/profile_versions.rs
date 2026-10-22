//! Pieces of the scene / persona version stores shared by both backends.

use std::collections::BTreeMap;

use memory_domain::error::{DomainError, DomainResult};
use memory_domain::profile::SceneRef;
use sha2::{Digest, Sha256};

/// Scene id of a legacy file: derived, so re-running the import (or
/// importing the same files into another database) yields the same ids.
pub fn legacy_scene_id(team_id: &str, agent_id: &str, path: &str) -> String {
    let digest = Sha256::digest(format!("{team_id}\0{agent_id}\0{path}").as_bytes());
    let hex: String = digest.iter().take(12).map(|b| format!("{b:02x}")).collect();
    format!("scn-{hex}")
}

pub fn attrs_json(attrs: &BTreeMap<String, String>) -> String {
    serde_json::to_string(attrs).unwrap_or_else(|_| "{}".into())
}

pub fn parents_json(parents: &[SceneRef]) -> String {
    serde_json::to_string(parents).unwrap_or_else(|_| "[]".into())
}

pub fn attrs_from(json: &str) -> DomainResult<BTreeMap<String, String>> {
    serde_json::from_str(json).map_err(|e| DomainError::internal(format!("scene attrs: {e}")))
}

pub fn parents_from(json: &str) -> DomainResult<Vec<SceneRef>> {
    serde_json::from_str(json).map_err(|e| DomainError::internal(format!("scene parents: {e}")))
}

pub fn slug_taken(slug: &str) -> DomainError {
    DomainError::conflict(
        "scene_exists",
        format!("another current scene is already named {slug}"),
    )
}

pub fn scene_changed(slug: &str, expected: u32, found: u32) -> DomainError {
    DomainError::conflict(
        "scene_changed",
        format!("{slug} is at version {found}, not the expected {expected}"),
    )
}

pub fn unknown_scene(scene_id: &str) -> DomainError {
    DomainError::invalid(format!("scene {scene_id} does not belong to this profile"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_ids_are_stable_and_distinct() {
        let a = legacy_scene_id("t", "a", "scene_blocks/x.md");
        assert_eq!(a, legacy_scene_id("t", "a", "scene_blocks/x.md"));
        assert_ne!(a, legacy_scene_id("t", "b", "scene_blocks/x.md"));
        assert!(a.starts_with("scn-") && a.len() == 28);
    }
}
