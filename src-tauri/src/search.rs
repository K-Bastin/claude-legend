//! Full-text search in the conversations of this machine: what the user and
//! Claude wrote, case and accent insensitive. The text of each transcript is
//! kept in memory until the file changes.

use crate::paths;
use crate::sessions::{user_text, LocalSession};
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

const MAX_HITS: usize = 200;
const BEFORE: usize = 50;
const AFTER: usize = 90;

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Hit {
    pub id: String,
    /// Text around the first match.
    pub snippet: String,
}

/// Transcript text, and the same text folded for matching.
struct Text {
    original: String,
    folded: String,
}

/// Transcript text keyed by file, with the mtime and size it was read at.
type TextCache = HashMap<PathBuf, (u64, u64, Arc<Text>)>;

#[derive(Default)]
pub struct SearchIndex {
    cache: Mutex<TextCache>,
}

/// Lower case without accents, one character for one, so positions in the
/// folded text are positions in the original text.
fn fold_char(c: char) -> char {
    let c = c.to_lowercase().next().unwrap_or(c);
    match c {
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' => 'a',
        'ç' => 'c',
        'è' | 'é' | 'ê' | 'ë' => 'e',
        'ì' | 'í' | 'î' | 'ï' => 'i',
        'ñ' => 'n',
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' => 'o',
        'ù' | 'ú' | 'û' | 'ü' => 'u',
        'ý' | 'ÿ' => 'y',
        'œ' => 'o',
        'æ' => 'a',
        _ => c,
    }
}

pub fn fold(s: &str) -> String {
    s.chars().map(fold_char).collect()
}

/// Text of a user prompt or of Claude's answer; tool calls and results,
/// meta entries and subagent turns are left out.
fn message_text(entry: &Value) -> Option<String> {
    match entry.get("type").and_then(Value::as_str)? {
        "user" => user_text(entry),
        "assistant" => {
            if entry.get("isSidechain").and_then(Value::as_bool) == Some(true) {
                return None;
            }
            let parts = entry.get("message")?.get("content")?.as_array()?;
            let text = parts
                .iter()
                .filter(|p| p.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|p| p.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join(" ");
            (!text.trim().is_empty()).then_some(text)
        }
        _ => None,
    }
}

fn transcript_text(path: &std::path::Path) -> String {
    let Ok(content) = std::fs::read_to_string(path) else {
        return String::new();
    };
    content
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter_map(|entry| message_text(&entry))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Around `start` (a character position), whitespace collapsed.
fn snippet(original: &str, start: usize, len: usize) -> String {
    let chars: Vec<char> = original.chars().collect();
    let from = start.saturating_sub(BEFORE);
    let to = (start + len + AFTER).min(chars.len());
    let text: String = chars[from..to].iter().collect();
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    format!(
        "{}{text}{}",
        if from > 0 { "…" } else { "" },
        if to < chars.len() { "…" } else { "" }
    )
}

impl SearchIndex {
    fn text(&self, session: &LocalSession) -> Arc<Text> {
        let size = std::fs::metadata(&session.path).map_or(0, |m| m.len());
        let mtime = paths::mtime_ms(&session.path);
        if let Some((m, s, text)) = self.cache.lock().unwrap().get(&session.path) {
            if *m == mtime && *s == size {
                return text.clone();
            }
        }
        let original = transcript_text(&session.path);
        let text = Arc::new(Text {
            folded: fold(&original),
            original,
        });
        self.cache
            .lock()
            .unwrap()
            .insert(session.path.clone(), (mtime, size, text.clone()));
        text
    }

    /// Sessions whose messages contain `query`, in the order given.
    pub fn search(&self, sessions: &[LocalSession], query: &str) -> Vec<Hit> {
        let needle = fold(query.trim());
        if needle.chars().count() < 2 {
            return Vec::new();
        }
        {
            let live: Vec<&PathBuf> = sessions.iter().map(|s| &s.path).collect();
            self.cache.lock().unwrap().retain(|p, _| live.contains(&p));
        }
        let mut hits = Vec::new();
        for session in sessions {
            let text = self.text(session);
            if let Some(byte) = text.folded.find(&needle) {
                let start = text.folded[..byte].chars().count();
                hits.push(Hit {
                    id: session.id.clone(),
                    snippet: snippet(&text.original, start, needle.chars().count()),
                });
                if hits.len() == MAX_HITS {
                    break;
                }
            }
        }
        hits
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn only_prompts_and_answers_are_searched() {
        let lines = [
            json!({"type": "user", "message": {"content": "Corrige le calcul des adresses"}}),
            json!({"type": "user", "isMeta": true, "message": {"content": "caché"}}),
            json!({"type": "assistant", "message": {"content": [
                {"type": "text", "text": "Voici la réponse"},
                {"type": "tool_use", "name": "Bash", "input": {"command": "rm secret"}}
            ]}}),
            json!({"type": "assistant", "isSidechain": true, "message": {"content": [{"type": "text", "text": "agent"}]}}),
            json!({"type": "summary", "summary": "résumé"}),
        ];
        let texts: Vec<String> = lines.iter().filter_map(message_text).collect();
        assert_eq!(
            texts,
            ["Corrige le calcul des adresses", "Voici la réponse"]
        );
    }

    #[test]
    fn matching_ignores_case_and_accents() {
        assert_eq!(fold("Réponse ÉTÉ Œuvre"), "reponse ete ouvre");
        assert_eq!(fold("Réponse").chars().count(), "Réponse".chars().count());
    }

    #[test]
    fn snippet_is_centred_on_the_match() {
        let text = format!("{}cible trouvée{}", "a ".repeat(60), " b".repeat(60));
        let start = text.find("cible").unwrap();
        let s = snippet(&text, start, 5);
        assert!(s.starts_with('…') && s.ends_with('…'), "{s}");
        assert!(s.contains("cible trouvée"));
        assert_eq!(snippet("court", 0, 5), "court");
    }

    #[test]
    fn search_finds_sessions_by_content() {
        let dir = std::env::temp_dir().join(format!("cl-search-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("s.jsonl");
        std::fs::write(
            &path,
            format!(
                "{}\n{}\n",
                json!({"type": "user", "cwd": "/p", "message": {"content": "Premier message"}}),
                json!({"type": "assistant", "message": {"content": [{"type": "text", "text": "La migration est terminée"}]}})
            ),
        )
        .unwrap();
        let session = crate::sessions::parse_session(&path).unwrap();
        let index = SearchIndex::default();
        let hits = index.search(std::slice::from_ref(&session), "MIGRATION est termine");
        assert_eq!(hits.len(), 1);
        assert!(hits[0].snippet.contains("La migration est terminée"));
        assert!(index.search(&[session], "absent").is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
