use std::path::{Path, PathBuf};

pub const ROOT_TOKEN: &str = "{{claude-legend:root}}";
pub const HOME_TOKEN: &str = "{{claude-legend:home}}";

pub fn home_dir() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
}

pub fn claude_home() -> PathBuf {
    match std::env::var_os("CLAUDE_CONFIG_DIR") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => home_dir().join(".claude"),
    }
}

pub fn projects_dir() -> PathBuf {
    claude_home().join("projects")
}

pub fn file_history_dir() -> PathBuf {
    claude_home().join("file-history")
}

/// Same encoding Claude Code uses for `~/.claude/projects/<dir>`.
pub fn encode_project_dir(cwd: &str) -> String {
    cwd.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

pub fn local_project_dir(cwd: &str) -> PathBuf {
    projects_dir().join(encode_project_dir(cwd))
}

fn json_escape(s: &str) -> String {
    let quoted = serde_json::to_string(s).unwrap_or_default();
    quoted[1..quoted.len() - 1].to_string()
}

fn variants(path: &str, escape: bool) -> Vec<String> {
    let trimmed = path.trim_end_matches(['/', '\\']);
    if trimmed.len() < 3 {
        return Vec::new();
    }
    let mut out = vec![trimmed.to_string()];
    if trimmed.contains('\\') {
        out.push(trimmed.replace('\\', "/"));
    }
    if escape {
        out = out.iter().map(|v| json_escape(v)).collect();
    }
    out.sort_by_key(|v| std::cmp::Reverse(v.len()));
    out.dedup();
    out
}

/// Replaces `needle` only where it is not followed by a character that would
/// make it part of a longer path segment (`/a/proj` must not match `/a/proj2`).
fn replace_bounded(text: &str, needle: &str, replacement: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(idx) = rest.find(needle) {
        let after = &rest[idx + needle.len()..];
        let bounded = after
            .chars()
            .next()
            .is_none_or(|c| !(c.is_alphanumeric() || c == '-' || c == '_'));
        out.push_str(&rest[..idx]);
        out.push_str(if bounded { replacement } else { needle });
        rest = after;
    }
    out.push_str(rest);
    out
}

/// Makes session content machine independent by swapping the project root and
/// the home directory for placeholders.
pub fn neutralize(text: &str, root: &str, home: &str, escape: bool) -> String {
    let mut out = text.to_string();
    for v in variants(root, escape) {
        out = replace_bounded(&out, &v, ROOT_TOKEN);
    }
    for v in variants(home, escape) {
        out = replace_bounded(&out, &v, HOME_TOKEN);
    }
    out
}

pub fn localize(text: &str, root: &str, home: &str, escape: bool) -> String {
    let root = root.trim_end_matches(['/', '\\']);
    let home = home.trim_end_matches(['/', '\\']);
    let (root, home) = if escape {
        (json_escape(root), json_escape(home))
    } else {
        (root.to_string(), home.to_string())
    };
    text.replace(ROOT_TOKEN, &root).replace(HOME_TOKEN, &home)
}

pub fn is_text_file(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("jsonl" | "json" | "md" | "txt" | "log")
    )
}

/// Writes through a temp file so sync tools never pick up a half written file.
pub fn write_atomic(path: &Path, content: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_file_name(format!(
        ".{}.cl-tmp",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("file")
    ));
    std::fs::write(&tmp, content)?;
    std::fs::rename(&tmp, path)
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub fn mtime_ms(path: &Path) -> u64 {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn neutralize_roundtrip_linux_to_windows() {
        let line = r#"{"cwd":"/home/kb/Workspace/app","file":"/home/kb/Workspace/app/src/a.ts","other":"/home/kb/Workspace/app2","cfg":"/home/kb/.claude/x"}"#;
        let neutral = neutralize(line, "/home/kb/Workspace/app", "/home/kb", true);
        assert_eq!(
            neutral,
            r#"{"cwd":"{{claude-legend:root}}","file":"{{claude-legend:root}}/src/a.ts","other":"{{claude-legend:home}}/Workspace/app2","cfg":"{{claude-legend:home}}/.claude/x"}"#
        );
        let win = localize(&neutral, r"C:\Users\kb\dev\app", r"C:\Users\kb", true);
        assert!(win.contains(r#""cwd":"C:\\Users\\kb\\dev\\app""#));
        assert_eq!(
            neutralize(&win, r"C:\Users\kb\dev\app", r"C:\Users\kb", true),
            neutral
        );
    }

    #[test]
    fn encodes_like_claude_code() {
        assert_eq!(
            encode_project_dir("/home/kb/.local/share/applications"),
            "-home-kb--local-share-applications"
        );
        assert_eq!(encode_project_dir(r"C:\Users\kb\app"), "C--Users-kb-app");
    }
}
