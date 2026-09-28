//! Archived conversations: hidden from the list on every machine, their files
//! kept. Each session has at most one marker, `{archived, at}`, kept locally in
//! `<data>/archive/<id>.json` and in the store at `archive/<id>.json`. The most
//! recent marker wins, so restoring a conversation propagates like archiving it.

use crate::paths;
use crate::sessions::is_session_id;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Marker {
    pub archived: bool,
    /// When the marker was set, in milliseconds.
    pub at: u64,
}

pub fn dir(data_dir: &Path) -> PathBuf {
    data_dir.join("archive")
}

pub fn marker_path(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{id}.json"))
}

/// Every local marker, by session id.
pub fn load_all(dir: &Path) -> HashMap<String, Marker> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return HashMap::new();
    };
    entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let id = name.strip_suffix(".json").filter(|id| is_session_id(id))?;
            let marker = crate::config::load_json::<Marker>(&e.path())?;
            Some((id.to_string(), marker))
        })
        .collect()
}

pub fn save(dir: &Path, id: &str, marker: &Marker) -> anyhow::Result<()> {
    crate::config::save_json(&marker_path(dir, id), marker)
}

/// Archives or restores a conversation on this machine; the next sync spreads it.
pub fn set(dir: &Path, id: &str, archived: bool) -> anyhow::Result<()> {
    save(
        dir,
        id,
        &Marker {
            archived,
            at: paths::now_ms(),
        },
    )
}

/// The marker both sides should end up with, `None` when they already agree.
pub fn newest(local: Option<Marker>, remote: Option<Marker>) -> Option<(Marker, bool)> {
    match (local, remote) {
        (Some(l), Some(r)) if l == r => None,
        (Some(l), Some(r)) if l.at >= r.at => Some((l, true)),
        (Some(_), Some(r)) => Some((r, false)),
        (Some(l), None) => Some((l, true)),
        (None, Some(r)) => Some((r, false)),
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newest_marker_wins() {
        let old = Marker {
            archived: true,
            at: 1,
        };
        let new = Marker {
            archived: false,
            at: 2,
        };
        assert_eq!(newest(Some(old), Some(new)), Some((new, false)));
        assert_eq!(newest(Some(new), Some(old)), Some((new, true)));
        assert_eq!(newest(Some(old), None), Some((old, true)));
        assert_eq!(newest(None, Some(old)), Some((old, false)));
        assert_eq!(newest(Some(old), Some(old)), None);
    }

    #[test]
    fn markers_roundtrip_and_skip_foreign_files() {
        let dir = std::env::temp_dir().join(format!("cl-archive-{}", uuid::Uuid::new_v4()));
        let id = "11111111-2222-3333-4444-555555555555";
        set(&dir, id, true).unwrap();
        std::fs::write(dir.join("notes.json"), "{}").unwrap();
        let all = load_all(&dir);
        assert_eq!(all.len(), 1);
        assert!(all[id].archived);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
