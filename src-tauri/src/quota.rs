//! Plan usage (5-hour and weekly limits) as reported by Claude Code.
//!
//! Claude Code gives `rate_limits` to its status line command after each
//! response. Sessions started by the app use this binary as status line
//! (`claude-legend --statusline-relay`): it records the limits, then runs the
//! user's own status line, if any, and prints its output unchanged.

use serde_json::Value;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub const RELAY_ARG: &str = "--statusline-relay";
const QUOTA_FILE_ENV: &str = "CLAUDE_LEGEND_QUOTA_FILE";
const NEXT_COMMAND_ENV: &str = "CLAUDE_LEGEND_STATUSLINE_NEXT";

pub fn quota_file(data_dir: &Path) -> PathBuf {
    data_dir.join("quota.json")
}

/// Last recorded limits: `{ "rateLimits": {...}, "updatedAt": ms }`.
pub fn load(data_dir: &Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(quota_file(data_dir)).ok()?).ok()
}

/// The user's own status line command, from the settings Claude Code would
/// read for `cwd` (most specific first).
fn user_status_line(cwd: &Path) -> Option<String> {
    let files = [
        cwd.join(".claude").join("settings.local.json"),
        cwd.join(".claude").join("settings.json"),
        crate::paths::claude_home().join("settings.json"),
    ];
    files.iter().find_map(|file| {
        let settings: Value = serde_json::from_str(&std::fs::read_to_string(file).ok()?).ok()?;
        let line = settings.get("statusLine")?;
        (line.get("type")?.as_str()? == "command")
            .then(|| line.get("command")?.as_str().map(str::to_string))
            .flatten()
    })
}

/// Extra `claude` arguments and environment making the relay the status line.
pub struct LaunchOptions {
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}

pub fn launch_options(data_dir: &Path, cwd: &Path) -> anyhow::Result<LaunchOptions> {
    let exe = std::env::current_exe()?;
    let settings = serde_json::json!({
        "statusLine": {
            "type": "command",
            "command": format!("\"{}\" {RELAY_ARG}", exe.display()),
            "padding": 0,
        }
    });
    let settings_file = data_dir.join("statusline-settings.json");
    crate::paths::write_atomic(
        &settings_file,
        serde_json::to_string_pretty(&settings)?.as_bytes(),
    )?;
    let args = vec![
        "--settings".to_string(),
        settings_file.to_string_lossy().into_owned(),
    ];
    let env = vec![
        (
            QUOTA_FILE_ENV.to_string(),
            quota_file(data_dir).to_string_lossy().into_owned(),
        ),
        (
            NEXT_COMMAND_ENV.to_string(),
            user_status_line(cwd).unwrap_or_default(),
        ),
    ];
    Ok(LaunchOptions { args, env })
}

/// Keeps the limits of the status line input, only rewriting the file when they change.
fn record(file: &Path, input: &str) {
    let Ok(status) = serde_json::from_str::<Value>(input) else {
        return;
    };
    let Some(limits) = status
        .get("rate_limits")
        .filter(|l| l.as_object().is_some_and(|o| !o.is_empty()))
    else {
        return;
    };
    let previous = std::fs::read_to_string(file)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok());
    if previous.as_ref().and_then(|p| p.get("rateLimits")) == Some(limits) {
        return;
    }
    let quota = serde_json::json!({ "rateLimits": limits, "updatedAt": crate::paths::now_ms() });
    let _ = crate::paths::write_atomic(file, quota.to_string().as_bytes());
}

fn shell(command: &str) -> Command {
    if cfg!(windows) {
        let mut cmd = Command::new("cmd");
        cmd.arg("/C").arg(command);
        cmd
    } else {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(command);
        cmd
    }
}

/// Entry point of `claude-legend --statusline-relay`, called by Claude Code.
pub fn relay() {
    let mut input = String::new();
    let _ = std::io::stdin().read_to_string(&mut input);
    if let Some(file) = std::env::var_os(QUOTA_FILE_ENV) {
        record(Path::new(&file), &input);
    }
    let next = std::env::var(NEXT_COMMAND_ENV).unwrap_or_default();
    if next.trim().is_empty() {
        return;
    }
    let Ok(mut child) = shell(&next)
        .stdin(Stdio::piped())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
    else {
        return;
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(input.as_bytes());
    }
    let _ = child.wait();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_rate_limits_only_when_they_change() {
        let dir = std::env::temp_dir().join(format!("cl-quota-{}", uuid::Uuid::new_v4()));
        let file = dir.join("quota.json");
        record(&file, r#"{"model":{}}"#);
        assert!(!file.exists());
        let input =
            r#"{"rate_limits":{"five_hour":{"used_percentage":66,"resets_at":1790598600}}}"#;
        record(&file, input);
        let first = std::fs::read_to_string(&file).unwrap();
        assert!(first.contains("\"used_percentage\":66"));
        std::thread::sleep(std::time::Duration::from_millis(5));
        record(&file, input);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), first);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn finds_the_user_status_line_most_specific_first() {
        let dir = std::env::temp_dir().join(format!("cl-sl-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join(".claude")).unwrap();
        std::fs::write(
            dir.join(".claude/settings.json"),
            r#"{"statusLine":{"type":"command","command":"project-line"}}"#,
        )
        .unwrap();
        assert_eq!(user_status_line(&dir).as_deref(), Some("project-line"));
        std::fs::write(
            dir.join(".claude/settings.local.json"),
            r#"{"statusLine":{"type":"command","command":"local-line"}}"#,
        )
        .unwrap();
        assert_eq!(user_status_line(&dir).as_deref(), Some("local-line"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
