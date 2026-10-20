//! Client configuration: a JSON file written by `amem login`, overridable
//! per process by environment variables.
//!
//! Precedence (highest first): `AMEM_<FIELD>` → `CLAUDE_PLUGIN_OPTION_<FIELD>`
//! (Claude Code plugin `userConfig`) → the config file.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    pub server_url: String,
    #[serde(default = "default_department_id")]
    pub department_id: String,
    pub user_key: String,
    pub team_id: String,
    pub agent_id: String,
    /// Comma-separated prefixes. A prompt that starts with one of them (after
    /// leading whitespace) is not a person's turn: it is neither recalled for
    /// nor captured. Empty by default.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub skip_prompt_prefixes: String,
}

fn default_department_id() -> String {
    "default".into()
}

const FIELDS: [&str; 5] = [
    "server_url",
    "department_id",
    "user_key",
    "team_id",
    "agent_id",
];

/// Settings that may stay empty, overridable like `FIELDS`.
const OPTIONAL_FIELDS: [&str; 1] = ["skip_prompt_prefixes"];

impl Config {
    /// File (if any) plus environment overrides.
    pub fn load() -> Result<Self, String> {
        let file = read_file(&config_path())?;
        Ok(Self::merge(file, |k| std::env::var(k).ok()))
    }

    pub fn merge(file: Option<Config>, env: impl Fn(&str) -> Option<String>) -> Self {
        let mut c = file.unwrap_or_else(|| Config {
            department_id: default_department_id(),
            ..Default::default()
        });
        for field in FIELDS.into_iter().chain(OPTIONAL_FIELDS) {
            let upper = field.to_uppercase();
            let value = env(&format!("AMEM_{upper}"))
                .or_else(|| env(&format!("CLAUDE_PLUGIN_OPTION_{upper}")))
                .filter(|v| !v.trim().is_empty());
            if let Some(v) = value {
                *c.field_mut(field) = v.trim().to_owned();
            }
        }
        c.server_url = c.server_url.trim_end_matches('/').to_owned();
        c
    }

    fn field_mut(&mut self, field: &str) -> &mut String {
        match field {
            "server_url" => &mut self.server_url,
            "department_id" => &mut self.department_id,
            "user_key" => &mut self.user_key,
            "team_id" => &mut self.team_id,
            "skip_prompt_prefixes" => &mut self.skip_prompt_prefixes,
            _ => &mut self.agent_id,
        }
    }

    /// Whether `prompt` starts with one of `skip_prompt_prefixes`.
    pub fn skips_prompt(&self, prompt: &str) -> bool {
        let prompt = prompt.trim_start();
        self.skip_prompt_prefixes
            .split(',')
            .map(str::trim)
            .any(|prefix| !prefix.is_empty() && prompt.starts_with(prefix))
    }

    /// Names of required fields that are still empty.
    pub fn missing(&self) -> Vec<&'static str> {
        FIELDS
            .into_iter()
            .filter(|f| match *f {
                "server_url" => self.server_url.is_empty(),
                "department_id" => self.department_id.is_empty(),
                "user_key" => self.user_key.is_empty(),
                "team_id" => self.team_id.is_empty(),
                _ => self.agent_id.is_empty(),
            })
            .collect()
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        let json = serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?;
        write_private(path, &json)
    }
}

fn read_file(path: &Path) -> Result<Option<Config>, String> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let mut value: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", path.display()))?;
    let migrated = migrate_file(&mut value);
    let config: Config =
        serde_json::from_value(value).map_err(|e| format!("{}: {e}", path.display()))?;
    if migrated {
        // Best effort: an unwritable file is migrated again on every load.
        let _ = config.save(path);
    }
    Ok(Some(config))
}

/// Renames fields written by older versions; true when anything changed.
/// Before 0.3.0 the department was called `service_id`.
fn migrate_file(value: &mut serde_json::Value) -> bool {
    let Some(map) = value.as_object_mut() else {
        return false;
    };
    match map.remove("service_id") {
        Some(id) => {
            map.entry("department_id").or_insert(id);
            true
        }
        None => false,
    }
}

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn xdg(var: &str, fallback: &[&str]) -> PathBuf {
    std::env::var_os(var)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| fallback.iter().fold(home(), |p, s| p.join(s)))
}

pub fn config_path() -> PathBuf {
    match std::env::var_os("AMEM_CONFIG") {
        Some(p) => PathBuf::from(p),
        None => xdg("XDG_CONFIG_HOME", &[".config"]).join("agent-memory/client.json"),
    }
}

/// Offline queue and per-session cursors.
pub fn data_dir() -> PathBuf {
    match std::env::var_os("AMEM_DATA_DIR") {
        Some(p) => PathBuf::from(p),
        None => xdg("XDG_DATA_HOME", &[".local", "share"]).join("agent-memory"),
    }
}

/// Writes atomically (temp file + rename), readable by the owner only:
/// the config holds a user key and queued jobs hold conversation text.
pub fn write_private(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
    std::fs::write(&tmp, bytes).map_err(|e| format!("{}: {e}", tmp.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_from_before_departments_is_rewritten_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("client.json");
        std::fs::write(
            &path,
            r#"{"server_url":"http://s","service_id":"sales","user_key":"k","team_id":"t","agent_id":"a"}"#,
        )
        .unwrap();

        let config = read_file(&path).unwrap().unwrap();
        assert_eq!(config.department_id, "sales");
        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(saved.contains(r#""department_id": "sales""#));
        assert!(!saved.contains("service_id"));
        assert_eq!(read_file(&path).unwrap().unwrap(), config);
    }

    #[test]
    fn env_overrides_file_and_amem_beats_plugin_options() {
        let file = Config {
            server_url: "http://file/".into(),
            department_id: "default".into(),
            user_key: "sk-mem-file".into(),
            team_id: "team-file".into(),
            agent_id: "agt-file".into(),
            skip_prompt_prefixes: String::new(),
        };
        let c = Config::merge(Some(file), |k| match k {
            "AMEM_TEAM_ID" => Some("team-env".into()),
            "CLAUDE_PLUGIN_OPTION_TEAM_ID" => Some("team-plugin".into()),
            "CLAUDE_PLUGIN_OPTION_AGENT_ID" => Some("agt-plugin".into()),
            "AMEM_USER_KEY" => Some("  ".into()),
            _ => None,
        });
        assert_eq!(c.server_url, "http://file");
        assert_eq!(c.team_id, "team-env");
        assert_eq!(c.agent_id, "agt-plugin");
        assert_eq!(c.user_key, "sk-mem-file", "blank env values are ignored");
        assert!(c.missing().is_empty());
    }

    #[test]
    fn skip_prompt_prefixes_match_the_start_only() {
        let c = Config::merge(None, |k| {
            (k == "AMEM_SKIP_PROMPT_PREFIXES").then(|| "[AGEND-, <task-notification>,".into())
        });
        assert!(c.skips_prompt("[AGEND-MSG] id=m-1 kind=task"));
        assert!(c.skips_prompt("\n  <task-notification>done</task-notification>"));
        assert!(
            !c.skips_prompt("deploy it [AGEND-MSG] id=m-2"),
            "only a leading prefix counts"
        );
        assert!(!c.skips_prompt("fix the build"));
        assert!(!c.missing().contains(&"skip_prompt_prefixes"));
        assert!(
            !Config::default().skips_prompt("[AGEND-MSG]"),
            "empty skips nothing"
        );
    }

    #[test]
    fn reports_missing_fields() {
        let c = Config::merge(None, |_| None);
        assert_eq!(c.department_id, "default");
        assert_eq!(
            c.missing(),
            ["server_url", "user_key", "team_id", "agent_id"]
        );
    }
}
