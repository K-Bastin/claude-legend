use crate::paths;
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalSession {
    pub id: String,
    pub path: PathBuf,
    pub cwd: String,
    pub title: String,
    pub first_prompt: String,
    pub git_branch: Option<String>,
    pub prompt_count: u32,
    pub updated_at: u64,
}

/// Parsed session keyed by file, with the mtime and size it was parsed at.
type ParseCache = HashMap<PathBuf, (u64, u64, Option<LocalSession>)>;

#[derive(Default)]
pub struct SessionIndex {
    cache: Mutex<ParseCache>,
    project_keys: Mutex<HashMap<String, ProjectIdentity>>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectIdentity {
    pub key: String,
    pub name: String,
    pub git_remote: Option<String>,
}

fn user_text(entry: &Value) -> Option<String> {
    if entry.get("isMeta").and_then(Value::as_bool) == Some(true)
        || entry.get("isSidechain").and_then(Value::as_bool) == Some(true)
    {
        return None;
    }
    let content = entry.get("message")?.get("content")?;
    let text = match content {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter(|p| p.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|p| p.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join(" "),
        _ => return None,
    };
    let text = text.trim();
    // Slash commands, hook output and system reminders are wrapped in tags.
    if text.is_empty() || text.starts_with('<') {
        return None;
    }
    Some(text.to_string())
}

fn truncate(s: &str, max: usize) -> String {
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.chars().count() <= max {
        s
    } else {
        format!("{}…", s.chars().take(max).collect::<String>())
    }
}

pub fn parse_session(path: &Path) -> Option<LocalSession> {
    let id = path.file_stem()?.to_str()?.to_string();
    let text = std::fs::read_to_string(path).ok()?;
    let mut cwd = None;
    let mut git_branch = None;
    let mut first_prompt = None;
    let mut custom_title = None;
    let mut ai_title = None;
    let mut summary = None;
    let mut prompt_count = 0;
    for line in text.lines() {
        let Ok(entry) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let str_field = |k: &str| entry.get(k).and_then(Value::as_str).map(str::to_string);
        if cwd.is_none() {
            cwd = str_field("cwd");
        }
        if let Some(branch) = str_field("gitBranch").filter(|b| !b.is_empty()) {
            git_branch = Some(branch);
        }
        match entry.get("type").and_then(Value::as_str) {
            Some("user") => {
                if let Some(text) = user_text(&entry) {
                    prompt_count += 1;
                    first_prompt.get_or_insert(text);
                }
            }
            Some("custom-title") => custom_title = str_field("customTitle").or(custom_title),
            Some("ai-title") => ai_title = str_field("aiTitle").or(ai_title),
            Some("summary") => summary = str_field("summary").or(summary),
            _ => {}
        }
    }
    let cwd = cwd?;
    if prompt_count == 0 {
        return None;
    }
    let first_prompt = first_prompt.unwrap_or_default();
    let title = custom_title
        .or(ai_title)
        .or(summary)
        .unwrap_or_else(|| first_prompt.clone());
    Some(LocalSession {
        id,
        path: path.to_path_buf(),
        cwd,
        title: truncate(&title, 90),
        first_prompt: truncate(&first_prompt, 240),
        git_branch,
        prompt_count,
        updated_at: paths::mtime_ms(path),
    })
}

impl SessionIndex {
    pub fn scan(&self) -> Vec<LocalSession> {
        let mut files = Vec::new();
        if let Ok(projects) = std::fs::read_dir(paths::projects_dir()) {
            for project in projects.flatten() {
                let Ok(entries) = std::fs::read_dir(project.path()) else {
                    continue;
                };
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.extension().and_then(|e| e.to_str()) == Some("jsonl") && path.is_file()
                    {
                        files.push(path);
                    }
                }
            }
        }

        let mut cache = self.cache.lock().unwrap();
        cache.retain(|p, _| files.contains(p));
        let mut sessions = Vec::new();
        for path in files {
            let meta = std::fs::metadata(&path).ok();
            let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
            let mtime = paths::mtime_ms(&path);
            let fresh = matches!(cache.get(&path), Some((m, s, _)) if *m == mtime && *s == size);
            if !fresh {
                cache.insert(path.clone(), (mtime, size, parse_session(&path)));
            }
            if let Some((_, _, Some(session))) = cache.get(&path) {
                sessions.push(session.clone());
            }
        }
        sessions.sort_by_key(|s| std::cmp::Reverse(s.updated_at));
        sessions
    }

    pub fn find(&self, id: &str) -> Option<LocalSession> {
        self.scan().into_iter().find(|s| s.id == id)
    }

    /// Identifies a project independently of where it lives on disk: by its
    /// git remote when there is one, by its folder name otherwise.
    pub fn project_identity(&self, cwd: &str) -> ProjectIdentity {
        if let Some(identity) = self.project_keys.lock().unwrap().get(cwd) {
            return identity.clone();
        }
        let identity = compute_identity(cwd);
        self.project_keys
            .lock()
            .unwrap()
            .insert(cwd.to_string(), identity.clone());
        identity
    }
}

fn git(cwd: &str, args: &[&str]) -> Option<String> {
    if !Path::new(cwd).is_dir() {
        return None;
    }
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(cwd).args(args);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let out = cmd.output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!s.is_empty()).then_some(s)
}

pub fn normalize_remote(url: &str) -> String {
    let mut s = url.trim().to_lowercase();
    if let Some(idx) = s.find("://") {
        s = s[idx + 3..].to_string();
    }
    if let Some(idx) = s.find('@') {
        s = s[idx + 1..].to_string();
    }
    s = s.replacen(':', "/", 1);
    s.trim_end_matches('/').trim_end_matches(".git").to_string()
}

/// Session ids are the UUIDs Claude Code names its transcripts with. Ids read
/// from the sync target end up in file names and `claude` arguments, so
/// anything else is refused.
pub fn is_session_id(id: &str) -> bool {
    uuid::Uuid::try_parse(id).is_ok_and(|u| u.hyphenated().to_string() == id.to_lowercase())
}

/// Project keys are slugs (see [`slug`]), safe as file and directory names.
pub fn is_project_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 200
        && key
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// The remote URL without credentials (`https://user:token@host/…`).
fn redact_remote(url: &str) -> String {
    match url.find("://") {
        Some(scheme_end) => {
            let rest = &url[scheme_end + 3..];
            let host_start = rest
                .find('/')
                .map_or(rest, |i| &rest[..i])
                .rfind('@')
                .map_or(0, |i| i + 1);
            format!("{}{}", &url[..scheme_end + 3], &rest[host_start..])
        }
        None => url.to_string(),
    }
}

pub fn slug(s: &str) -> String {
    let mut out = String::new();
    for c in s.to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_matches('-').to_string()
}

/// Last folder of the path; a filesystem root (`/`, `C:\\`) is named after itself.
fn folder_name(path: &str) -> String {
    let name = path
        .trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("");
    if name.is_empty() || name.ends_with(':') {
        path.to_string()
    } else {
        name.to_string()
    }
}

fn compute_identity(cwd: &str) -> ProjectIdentity {
    let name = folder_name(cwd);
    let remote = git(cwd, &["config", "--get", "remote.origin.url"]);
    if let Some(remote) = remote {
        let mut key = normalize_remote(&remote);
        if let Some(top) = git(cwd, &["rev-parse", "--show-toplevel"]) {
            let top = std::fs::canonicalize(&top).unwrap_or_else(|_| PathBuf::from(&top));
            let here = std::fs::canonicalize(cwd).unwrap_or_else(|_| PathBuf::from(cwd));
            if let Ok(rel) = here.strip_prefix(&top) {
                let rel = rel.to_string_lossy().replace('\\', "/");
                if !rel.is_empty() {
                    key = format!("{key}/{rel}");
                }
            }
        }
        return ProjectIdentity {
            key: slug(&key),
            name,
            git_remote: Some(redact_remote(&remote)),
        };
    }
    ProjectIdentity {
        key: format!(
            "local-{}",
            Some(slug(&name))
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "root".into())
        ),
        name,
        git_remote: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remotes_normalize_to_same_key() {
        let a = slug(&normalize_remote("git@github.com:KB/Cohabsys.git"));
        let b = slug(&normalize_remote("https://github.com/kb/cohabsys"));
        assert_eq!(a, b);
        assert_eq!(a, "github-com-kb-cohabsys");
    }

    #[test]
    fn only_uuids_and_slugs_are_accepted() {
        assert!(is_session_id("11111111-2222-3333-4444-555555555555"));
        for bad in [
            "--dangerously-skip-permissions",
            "../x",
            "",
            "11111111222233334444555555555555",
        ] {
            assert!(!is_session_id(bad), "{bad}");
        }
        assert!(is_project_key("github-com-kb-app"));
        for bad in ["", "../x", "a/b", "A", r"a\b", "a.b"] {
            assert!(!is_project_key(bad), "{bad}");
        }
        assert!(is_project_key(&compute_identity("/").key));
    }

    #[test]
    fn remote_credentials_are_not_kept() {
        assert_eq!(
            redact_remote("https://kb:ghp_secret@github.com/kb/app.git"),
            "https://github.com/kb/app.git"
        );
        assert_eq!(
            redact_remote("ssh://git@host:22/a@b.git"),
            "ssh://host:22/a@b.git"
        );
        assert_eq!(
            redact_remote("git@github.com:kb/app.git"),
            "git@github.com:kb/app.git"
        );
    }

    #[test]
    fn root_folders_get_a_name() {
        assert_eq!(folder_name("/home/kb/app/"), "app");
        assert_eq!(folder_name(r"C:\Users\kb\app"), "app");
        assert_eq!(folder_name("/"), "/");
        assert_eq!(folder_name(r"C:\"), r"C:\");
        assert_eq!(compute_identity("/").key, "local-root");
    }
}
