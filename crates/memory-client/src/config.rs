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
    #[serde(default = "default_service_id")]
    pub service_id: String,
    pub user_key: String,
    pub team_id: String,
    pub agent_id: String,
}

fn default_service_id() -> String {
    "default".into()
}

const FIELDS: [&str; 5] = [
    "server_url",
    "service_id",
    "user_key",
    "team_id",
    "agent_id",
];

impl Config {
    /// File (if any) plus environment overrides.
    pub fn load() -> Result<Self, String> {
        let file = read_file(&config_path())?;
        Ok(Self::merge(file, |k| std::env::var(k).ok()))
    }

    pub fn merge(file: Option<Config>, env: impl Fn(&str) -> Option<String>) -> Self {
        let mut c = file.unwrap_or_else(|| Config {
            service_id: default_service_id(),
            ..Default::default()
        });
        for field in FIELDS {
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
            "service_id" => &mut self.service_id,
            "user_key" => &mut self.user_key,
            "team_id" => &mut self.team_id,
            _ => &mut self.agent_id,
        }
    }

    /// Names of required fields that are still empty.
    pub fn missing(&self) -> Vec<&'static str> {
        FIELDS
            .into_iter()
            .filter(|f| match *f {
                "server_url" => self.server_url.is_empty(),
                "service_id" => self.service_id.is_empty(),
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
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|e| format!("{}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("{}: {e}", path.display())),
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
    fn env_overrides_file_and_amem_beats_plugin_options() {
        let file = Config {
            server_url: "http://file/".into(),
            service_id: "default".into(),
            user_key: "sk-mem-file".into(),
            team_id: "team-file".into(),
            agent_id: "agt-file".into(),
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
    fn reports_missing_fields() {
        let c = Config::merge(None, |_| None);
        assert_eq!(c.service_id, "default");
        assert_eq!(
            c.missing(),
            ["server_url", "user_key", "team_id", "agent_id"]
        );
    }
}
