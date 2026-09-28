//! Events of the conversations running in the app, for desktop notifications.
//!
//! Sessions started by the app get `Notification` (Claude needs a permission or
//! an answer) and `Stop` (Claude finished answering) hooks running this binary
//! as `claude-legend --hook-relay`. The relay leaves each event as a small file
//! in the events folder, which the app drains and forwards to the page. Hooks
//! from the user's own settings still run: Claude Code adds up the hooks of
//! every settings file.

use crate::paths;
use crate::sessions::is_session_id;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::Read;
use std::path::{Path, PathBuf};

pub const RELAY_ARG: &str = "--hook-relay";
const DIR_ENV: &str = "CLAUDE_LEGEND_EVENTS_DIR";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeEvent {
    pub session_id: String,
    /// "notification" or "stop".
    pub kind: String,
    /// What Claude is waiting for, for notifications.
    pub message: Option<String>,
    pub at: u64,
}

pub fn dir(data_dir: &Path) -> PathBuf {
    data_dir.join("events")
}

/// `hooks` section of the settings passed to sessions started by the app.
pub fn hooks(exe: &Path) -> Value {
    let hook = json!([{ "hooks": [{
        "type": "command",
        "command": format!("\"{}\" {RELAY_ARG}", exe.display()),
        "timeout": 10,
    }] }]);
    json!({ "Notification": hook, "Stop": hook })
}

/// Environment telling the relay where to leave events.
pub fn launch_env(data_dir: &Path) -> (String, String) {
    (
        DIR_ENV.to_string(),
        dir(data_dir).to_string_lossy().into_owned(),
    )
}

/// The event described by a hook's input, if it is one the app shows.
fn from_hook_input(input: &str) -> Option<ClaudeEvent> {
    let hook: Value = serde_json::from_str(input).ok()?;
    let session_id = hook
        .get("session_id")?
        .as_str()
        .filter(|id| is_session_id(id))?;
    let kind = match hook.get("hook_event_name")?.as_str()? {
        "Notification" => "notification",
        "Stop" => "stop",
        _ => return None,
    };
    Some(ClaudeEvent {
        session_id: session_id.to_string(),
        kind: kind.to_string(),
        message: hook
            .get("message")
            .and_then(Value::as_str)
            .map(str::to_string),
        at: paths::now_ms(),
    })
}

fn record(dir: &Path, event: &ClaudeEvent) -> std::io::Result<()> {
    let file = dir.join(format!("{}-{}.json", event.at, uuid::Uuid::new_v4()));
    paths::write_atomic(&file, serde_json::to_string(event)?.as_bytes())
}

/// Entry point of `claude-legend --hook-relay`, called by Claude Code. Prints
/// nothing and always succeeds, so it never changes what Claude does.
pub fn relay() {
    let mut input = String::new();
    let _ = std::io::stdin().read_to_string(&mut input);
    if let (Some(dir), Some(event)) = (std::env::var_os(DIR_ENV), from_hook_input(&input)) {
        let _ = record(Path::new(&dir), &event);
    }
}

/// Takes the pending events out of the folder, oldest first.
pub fn drain(dir: &Path) -> Vec<ClaudeEvent> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut events: Vec<ClaudeEvent> = entries
        .flatten()
        .filter(|e| {
            let name = e.file_name();
            let name = name.to_string_lossy();
            // Files still being written by write_atomic start with a dot.
            !name.starts_with('.') && name.ends_with(".json")
        })
        .filter_map(|e| {
            let event = crate::config::load_json(&e.path());
            let _ = std::fs::remove_file(e.path());
            event
        })
        .collect();
    events.sort_by_key(|e| e.at);
    events
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "11111111-2222-3333-4444-555555555555";

    #[test]
    fn hook_inputs_become_events() {
        let event = from_hook_input(&format!(
            r#"{{"session_id":"{ID}","hook_event_name":"Notification","message":"Claude needs your permission to use Bash"}}"#
        ))
        .unwrap();
        assert_eq!(event.kind, "notification");
        assert_eq!(
            event.message.as_deref(),
            Some("Claude needs your permission to use Bash")
        );
        assert_eq!(
            from_hook_input(&format!(
                r#"{{"session_id":"{ID}","hook_event_name":"Stop"}}"#
            ))
            .unwrap()
            .kind,
            "stop"
        );
        assert!(from_hook_input(&format!(
            r#"{{"session_id":"{ID}","hook_event_name":"PreToolUse"}}"#
        ))
        .is_none());
        assert!(from_hook_input(r#"{"session_id":"../x","hook_event_name":"Stop"}"#).is_none());
        assert!(from_hook_input("not json").is_none());
    }

    #[test]
    fn recorded_events_are_drained_once_in_order() {
        let dir = std::env::temp_dir().join(format!("cl-events-{}", uuid::Uuid::new_v4()));
        let event = |at| ClaudeEvent {
            session_id: ID.into(),
            kind: "stop".into(),
            message: None,
            at,
        };
        record(&dir, &event(2)).unwrap();
        record(&dir, &event(1)).unwrap();
        let drained = drain(&dir);
        assert_eq!(drained.iter().map(|e| e.at).collect::<Vec<_>>(), [1, 2]);
        assert!(drain(&dir).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn both_hooks_run_the_relay() {
        let hooks = hooks(Path::new("/opt/claude-legend"));
        for event in ["Notification", "Stop"] {
            assert_eq!(
                hooks[event][0]["hooks"][0]["command"],
                "\"/opt/claude-legend\" --hook-relay"
            );
        }
    }
}
