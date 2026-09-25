use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    /// Folder shared between machines (Syncthing, Nextcloud, OneDrive…).
    pub sync_dir: Option<String>,
    /// Explicit path to the `claude` executable, resolved from PATH otherwise.
    pub claude_path: Option<String>,
    /// Extra arguments passed to every `claude` launch.
    pub extra_args: String,
    pub machine_name: String,
    pub font_size: u16,
    pub sync_interval_secs: u64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            sync_dir: None,
            claude_path: None,
            extra_args: String::new(),
            machine_name: gethostname::gethostname().to_string_lossy().into_owned(),
            font_size: 14,
            sync_interval_secs: 60,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Machine {
    pub id: String,
}

pub fn load_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Option<T> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

pub fn save_json<T: Serialize>(path: &Path, value: &T) -> anyhow::Result<()> {
    let text = serde_json::to_string_pretty(value)?;
    crate::paths::write_atomic(path, text.as_bytes())?;
    Ok(())
}

pub fn load_settings(config_dir: &Path) -> Settings {
    load_json(&config_dir.join("settings.json")).unwrap_or_default()
}

pub fn load_or_create_machine(config_dir: &Path) -> Machine {
    let path = config_dir.join("machine.json");
    if let Some(machine) = load_json(&path) {
        return machine;
    }
    let machine = Machine {
        id: uuid::Uuid::new_v4().to_string(),
    };
    let _ = save_json(&path, &machine);
    machine
}

/// Desktop launchers often start apps with a minimal PATH, so common install
/// locations are checked too.
pub fn resolve_claude(settings: &Settings) -> Result<PathBuf, String> {
    if let Some(path) = settings.claude_path.as_deref().filter(|p| !p.trim().is_empty()) {
        let path = PathBuf::from(path.trim());
        return if path.exists() {
            Ok(path)
        } else {
            Err(format!("Exécutable introuvable : {}", path.display()))
        };
    }
    if let Ok(path) = which::which("claude") {
        return Ok(path);
    }
    let home = crate::paths::home_dir();
    let candidates: &[&str] = if cfg!(windows) {
        &[
            ".local/bin/claude.exe",
            "AppData/Roaming/npm/claude.cmd",
            ".claude/local/claude.exe",
        ]
    } else {
        &[
            ".local/bin/claude",
            ".claude/local/claude",
            ".npm-global/bin/claude",
            ".bun/bin/claude",
        ]
    };
    candidates
        .iter()
        .map(|c| home.join(c))
        .find(|p| p.exists())
        .ok_or_else(|| {
            "Claude Code est introuvable. Installe-le ou indique son chemin dans les réglages."
                .to_string()
        })
}
