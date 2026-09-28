//! Versions set aside when a conversation changed on two machines at once, see
//! `Syncer::sync_local_session`. Each is saved as `<id>-<ms>-<side>.jsonl`, in
//! its machine-independent form, and can be brought back as a new
//! conversation next to the original one.

use crate::paths;
use crate::sessions::{is_session_id, parse_session, SessionIndex};
use serde::Serialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Conflict {
    /// File name inside the conflicts folder.
    pub file: String,
    pub session_id: String,
    pub title: String,
    /// When the version was set aside, in milliseconds.
    pub saved_at: u64,
    /// "local" or "distant": which side lost.
    pub side: String,
    pub prompt_count: u32,
}

pub fn dir(data_dir: &Path) -> PathBuf {
    data_dir.join("conflicts")
}

/// `(session id, saved at, side)` from a conflict file name.
fn parse_name(name: &str) -> Option<(&str, u64, &str)> {
    let stem = name.strip_suffix(".jsonl")?;
    let (id, rest) = stem.split_at_checked(36)?;
    let (saved_at, side) = rest.strip_prefix('-')?.split_once('-')?;
    (is_session_id(id) && matches!(side, "local" | "distant")).then_some((
        id,
        saved_at.parse().ok()?,
        side,
    ))
}

/// A conflict file of the folder, refusing anything but a plain conflict name.
fn file_path(dir: &Path, file: &str) -> anyhow::Result<PathBuf> {
    parse_name(file).ok_or_else(|| anyhow::anyhow!("fichier de conflit invalide : {file}"))?;
    Ok(dir.join(file))
}

/// Newest first.
pub fn list(dir: &Path) -> Vec<Conflict> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<Conflict> = entries
        .flatten()
        .filter_map(|e| {
            let file = e.file_name().to_string_lossy().into_owned();
            let (id, saved_at, side) = parse_name(&file)?;
            let parsed = parse_session(&e.path());
            Some(Conflict {
                session_id: id.to_string(),
                title: parsed
                    .as_ref()
                    .map_or_else(|| "(conversation vide)".into(), |s| s.title.clone()),
                prompt_count: parsed.map_or(0, |s| s.prompt_count),
                saved_at,
                side: side.to_string(),
                file,
            })
        })
        .collect();
    out.sort_by_key(|c| std::cmp::Reverse(c.saved_at));
    out
}

pub fn delete(dir: &Path, file: &str) -> anyhow::Result<()> {
    std::fs::remove_file(file_path(dir, file)?)?;
    Ok(())
}

/// Brings a set-aside version back as a new conversation in the project of the
/// original, which must be on this machine. Returns the new session id.
pub fn restore(dir: &Path, file: &str, index: &SessionIndex) -> anyhow::Result<String> {
    let path = file_path(dir, file)?;
    let (id, saved_at, side) = parse_name(file).unwrap();
    let original = index.find(id).ok_or_else(|| {
        anyhow::anyhow!(
            "la conversation d'origine n'est pas sur ce PC : ouvre son projet ici d'abord"
        )
    })?;
    let neutral = std::fs::read_to_string(&path)?;
    let new_id = uuid::Uuid::new_v4().to_string();
    let home = paths::home_dir().to_string_lossy().into_owned();
    let mut text = paths::localize(&neutral, &original.cwd, &home, true).replace(id, &new_id);
    if !text.ends_with('\n') && !text.is_empty() {
        text.push('\n');
    }
    let date = chrono_like_date(saved_at);
    let from = if side == "local" {
        "de ce PC"
    } else {
        "d'un autre PC"
    };
    let title = serde_json::json!({
        "type": "custom-title",
        "customTitle": format!("{} (version {from} du {date})", original.title),
        "sessionId": new_id,
    });
    text.push_str(&format!("{title}\n"));
    paths::write_atomic(
        &paths::local_project_dir(&original.cwd).join(format!("{new_id}.jsonl")),
        text.as_bytes(),
    )?;
    std::fs::remove_file(&path)?;
    Ok(new_id)
}

/// `28/09/2026` in UTC, enough to tell versions apart in a title.
fn chrono_like_date(ms: u64) -> String {
    let days = (ms / 86_400_000) as i64;
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{day:02}/{month:02}/{year}")
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "11111111-2222-3333-4444-555555555555";

    #[test]
    fn names_are_parsed_strictly() {
        assert_eq!(
            parse_name(&format!("{ID}-1790598600000-distant.jsonl")),
            Some((ID, 1_790_598_600_000, "distant"))
        );
        for bad in [
            "../x.jsonl".to_string(),
            format!("{ID}-12-other.jsonl"),
            format!("{ID}-x-local.jsonl"),
            format!("{ID}-12-local.json"),
        ] {
            assert_eq!(parse_name(&bad), None, "{bad}");
        }
        assert!(file_path(Path::new("/c"), "../../etc/passwd").is_err());
    }

    #[test]
    fn dates_are_civil() {
        assert_eq!(chrono_like_date(0), "01/01/1970");
        assert_eq!(chrono_like_date(1_790_598_600_000), "28/09/2026");
        assert_eq!(chrono_like_date(951_782_400_000), "29/02/2000");
    }
}
