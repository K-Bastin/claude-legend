//! Per-project rules: instructions added to Claude's system prompt for every
//! session of a project, kept outside the project (no CLAUDE.md in the repo)
//! and shared through the sync target.

use crate::paths;
use std::path::{Path, PathBuf};

pub fn dir(data_dir: &Path) -> PathBuf {
    data_dir.join("rules")
}

/// Project keys are slugs (`[a-z0-9-]`), safe as file names.
pub fn path(data_dir: &Path, key: &str) -> PathBuf {
    dir(data_dir).join(format!("{key}.md"))
}

pub fn load(data_dir: &Path, key: &str) -> String {
    std::fs::read_to_string(path(data_dir, key)).unwrap_or_default()
}

/// Empty rules are kept as an empty file so that clearing them syncs too.
pub fn save(data_dir: &Path, key: &str, rules: &str) -> anyhow::Result<()> {
    paths::write_atomic(&path(data_dir, key), rules.as_bytes())?;
    Ok(())
}

/// File to pass to `claude --append-system-prompt-file`, if the project has rules.
pub fn prompt_file(data_dir: &Path, key: &str) -> Option<PathBuf> {
    let file = path(data_dir, key);
    (!load(data_dir, key).trim().is_empty()).then_some(file)
}

pub fn keys_with_rules(data_dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir(data_dir)) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let key = name.strip_suffix(".md")?.to_string();
            (!load(data_dir, &key).trim().is_empty()).then_some(key)
        })
        .collect()
}
