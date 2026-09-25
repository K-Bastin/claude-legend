//! Folder based synchronisation.
//!
//! Layout of `<syncDir>/claude-legend/`:
//! ```text
//! machines/<machineId>.json
//! projects/<key>/project.json
//! projects/<key>/paths/<machineId>.json        local path of the project on each machine
//! projects/<key>/sessions/<id>.jsonl           session with paths replaced by placeholders
//! projects/<key>/sessions/<id>.meta.json
//! projects/<key>/sessions/<id>/…               subagents, tool results
//! projects/<key>/memory/…
//! file-history/<id>/…                          checkpoints used by /rewind
//! locks/<id>.json
//! ```
//! Each file is only ever written by one machine at a time, except
//! sessions and memory, which are reconciled with a three-way comparison
//! against the hash recorded at the last sync.

use crate::config::{load_json, save_json, Machine};
use crate::paths::{self, localize, neutralize, write_atomic};
use crate::sessions::{LocalSession, ProjectIdentity, SessionIndex};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

const LOCK_STALE_MS: u64 = 120_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MachineInfo {
    pub id: String,
    pub name: String,
    pub os: String,
    pub last_seen: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectInfo {
    pub key: String,
    pub name: String,
    pub git_remote: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PathMapping {
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteMeta {
    pub id: String,
    pub project_key: String,
    pub title: String,
    pub first_prompt: String,
    pub git_branch: Option<String>,
    pub prompt_count: u32,
    pub updated_at: u64,
    pub hash: String,
    pub machine_id: String,
    pub machine_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LockInfo {
    pub session_id: String,
    pub machine_id: String,
    pub machine_name: String,
    pub since: u64,
    pub heartbeat: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct SyncState {
    hashes: HashMap<String, String>,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncReport {
    pub at: u64,
    pub pushed: u32,
    pub pulled: u32,
    pub conflicts: Vec<String>,
    pub errors: Vec<String>,
}

pub struct Syncer<'a> {
    pub root: PathBuf,
    pub machine: &'a Machine,
    pub machine_name: &'a str,
    pub state_path: PathBuf,
    pub conflicts_dir: PathBuf,
    pub index: &'a SessionIndex,
    /// Sessions currently running here: never overwritten from the remote.
    pub open_sessions: HashSet<String>,
}

fn sha(data: &str) -> String {
    hex::encode(Sha256::digest(data.as_bytes()))
}

/// Ignores a trailing line that Claude Code may still be writing.
fn read_complete_lines(path: &Path) -> std::io::Result<String> {
    let bytes = std::fs::read(path)?;
    let end = bytes.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
    Ok(String::from_utf8_lossy(&bytes[..end]).into_owned())
}

fn home() -> String {
    paths::home_dir().to_string_lossy().into_owned()
}

fn walk_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for entry in entries.flatten() {
            let p = entry.path();
            let name = entry.file_name();
            if name.to_string_lossy().ends_with(".cl-tmp") {
                continue;
            }
            if p.is_dir() {
                stack.push(p);
            } else {
                out.push(p);
            }
        }
    }
    out
}

pub fn sync_root(sync_dir: &str) -> PathBuf {
    Path::new(sync_dir).join("claude-legend")
}

impl<'a> Syncer<'a> {
    fn project_dir(&self, key: &str) -> PathBuf {
        self.root.join("projects").join(key)
    }

    fn load_state(&self) -> SyncState {
        load_json(&self.state_path).unwrap_or_default()
    }

    pub fn register_machine(&self) -> anyhow::Result<()> {
        save_json(
            &self.root.join("machines").join(format!("{}.json", self.machine.id)),
            &MachineInfo {
                id: self.machine.id.clone(),
                name: self.machine_name.to_string(),
                os: std::env::consts::OS.to_string(),
                last_seen: paths::now_ms(),
            },
        )
    }

    pub fn mapping(&self, key: &str) -> Option<String> {
        load_json::<PathMapping>(
            &self
                .project_dir(key)
                .join("paths")
                .join(format!("{}.json", self.machine.id)),
        )
        .map(|m| m.path)
    }

    pub fn set_mapping(&self, identity: &ProjectIdentity, path: &str) -> anyhow::Result<()> {
        let dir = self.project_dir(&identity.key);
        let project_file = dir.join("project.json");
        if !project_file.exists() {
            save_json(
                &project_file,
                &ProjectInfo {
                    key: identity.key.clone(),
                    name: identity.name.clone(),
                    git_remote: identity.git_remote.clone(),
                },
            )?;
        }
        let mapping_file = dir.join("paths").join(format!("{}.json", self.machine.id));
        if load_json::<PathMapping>(&mapping_file).map(|m| m.path).as_deref() != Some(path) {
            save_json(&mapping_file, &PathMapping { path: path.to_string() })?;
        }
        Ok(())
    }

    pub fn projects(&self) -> Vec<(ProjectInfo, Option<String>)> {
        let Ok(entries) = std::fs::read_dir(self.root.join("projects")) else {
            return Vec::new();
        };
        entries
            .flatten()
            .filter_map(|e| load_json::<ProjectInfo>(&e.path().join("project.json")))
            .map(|p| {
                let mapping = self.mapping(&p.key);
                (p, mapping)
            })
            .collect()
    }

    pub fn remote_sessions(&self, key: &str) -> Vec<RemoteMeta> {
        let Ok(entries) = std::fs::read_dir(self.project_dir(key).join("sessions")) else {
            return Vec::new();
        };
        entries
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".meta.json"))
            .filter_map(|e| load_json(&e.path()))
            .collect()
    }

    // ---------- locks ----------

    fn lock_path(&self, id: &str) -> PathBuf {
        self.root.join("locks").join(format!("{id}.json"))
    }

    /// Returns the lock held by another machine, if it is still alive.
    pub fn foreign_lock(&self, id: &str) -> Option<LockInfo> {
        let lock: LockInfo = load_json(&self.lock_path(id))?;
        (lock.machine_id != self.machine.id
            && paths::now_ms().saturating_sub(lock.heartbeat) < LOCK_STALE_MS)
            .then_some(lock)
    }

    pub fn acquire_lock(&self, id: &str) -> anyhow::Result<()> {
        let now = paths::now_ms();
        let since = load_json::<LockInfo>(&self.lock_path(id))
            .filter(|l| l.machine_id == self.machine.id)
            .map_or(now, |l| l.since);
        save_json(
            &self.lock_path(id),
            &LockInfo {
                session_id: id.to_string(),
                machine_id: self.machine.id.clone(),
                machine_name: self.machine_name.to_string(),
                since,
                heartbeat: now,
            },
        )
    }

    pub fn release_lock(&self, id: &str) {
        let path = self.lock_path(id);
        if load_json::<LockInfo>(&path).is_some_and(|l| l.machine_id == self.machine.id) {
            let _ = std::fs::remove_file(path);
        }
    }

    // ---------- sessions ----------

    /// Full reconciliation of every local and remote session.
    pub fn sync_all(&self) -> SyncReport {
        let mut report = SyncReport {
            at: paths::now_ms(),
            ..Default::default()
        };
        let mut state = self.load_state();
        if let Err(e) = self.register_machine() {
            report.errors.push(format!("Dossier de synchro inaccessible : {e}"));
            return report;
        }

        let locals = self.index.scan();
        let local_ids: HashSet<String> = locals.iter().map(|s| s.id.clone()).collect();
        let mut keys = HashSet::new();
        for session in &locals {
            let identity = self.index.project_identity(&session.cwd);
            keys.insert(identity.key.clone());
            if let Err(e) = self.sync_local_session(session, &identity, &mut state, &mut report) {
                report.errors.push(format!("{} : {e}", session.title));
            }
        }

        for (project, mapping) in self.projects() {
            let Some(local_root) = mapping else { continue };
            keys.insert(project.key.clone());
            for meta in self.remote_sessions(&project.key) {
                if local_ids.contains(&meta.id) {
                    continue;
                }
                match self.pull(&project.key, &meta, &local_root) {
                    Ok(true) => {
                        state.hashes.insert(format!("s:{}", meta.id), meta.hash.clone());
                        report.pulled += 1;
                    }
                    Ok(false) => {}
                    Err(e) => report.errors.push(format!("{} : {e}", meta.title)),
                }
            }
        }

        for key in keys {
            if let Some(local_root) = self.mapping(&key) {
                self.sync_memory(&key, &local_root, &mut state, &mut report);
            }
        }

        if let Err(e) = save_json(&self.state_path, &state) {
            report.errors.push(format!("État de synchro : {e}"));
        }
        report
    }

    /// Syncs one session before resuming it, so the latest remote turn is used.
    pub fn sync_one(&self, id: &str) -> SyncReport {
        let mut report = SyncReport {
            at: paths::now_ms(),
            ..Default::default()
        };
        let mut state = self.load_state();
        if let Some(session) = self.index.find(id) {
            let identity = self.index.project_identity(&session.cwd);
            if let Err(e) = self.sync_local_session(&session, &identity, &mut state, &mut report) {
                report.errors.push(e.to_string());
            }
        } else {
            for (project, mapping) in self.projects() {
                let Some(local_root) = mapping else { continue };
                if let Some(meta) = self.remote_sessions(&project.key).into_iter().find(|m| m.id == id) {
                    match self.pull(&project.key, &meta, &local_root) {
                        Ok(true) => {
                            state.hashes.insert(format!("s:{id}"), meta.hash.clone());
                            report.pulled += 1;
                        }
                        Ok(false) => report
                            .errors
                            .push("La session est encore en cours de synchronisation, réessaie dans un instant.".into()),
                        Err(e) => report.errors.push(e.to_string()),
                    }
                }
            }
        }
        let _ = save_json(&self.state_path, &state);
        report
    }

    fn sync_local_session(
        &self,
        session: &LocalSession,
        identity: &ProjectIdentity,
        state: &mut SyncState,
        report: &mut SyncReport,
    ) -> anyhow::Result<()> {
        // A project gets mapped to the folder where its sessions are run.
        if self.mapping(&identity.key).is_none() {
            self.set_mapping(identity, &session.cwd)?;
        }
        let key = &identity.key;
        let state_key = format!("s:{}", session.id);
        let home = home();
        let local_neutral = neutralize(&read_complete_lines(&session.path)?, &session.cwd, &home, true);
        let local_hash = sha(&local_neutral);
        let meta_path = self.project_dir(key).join("sessions").join(format!("{}.meta.json", session.id));
        let remote: Option<RemoteMeta> = load_json(&meta_path);
        let base = state.hashes.get(&state_key).cloned();

        let Some(remote) = remote else {
            self.push(key, session, &local_neutral, &local_hash)?;
            state.hashes.insert(state_key, local_hash);
            report.pushed += 1;
            return Ok(());
        };
        if remote.hash == local_hash {
            state.hashes.insert(state_key, local_hash);
            return Ok(());
        }
        let open_here = self.open_sessions.contains(&session.id);
        let local_changed = base.as_deref() != Some(local_hash.as_str());
        let remote_changed = base.as_deref() != Some(remote.hash.as_str());

        let push_wins = if !remote_changed {
            true
        } else if !local_changed {
            false
        } else {
            // Both sides moved. Sessions are append-only logs, so when one
            // contains the other the longer one is simply more recent.
            let remote_text = std::fs::read_to_string(meta_path.with_file_name(format!("{}.jsonl", session.id)))?;
            if remote_text.starts_with(&local_neutral) {
                false
            } else if local_neutral.starts_with(&remote_text) {
                true
            } else {
                let push = open_here || session.updated_at >= remote.updated_at;
                let (loser, side) = if push { (&remote_text, "distant") } else { (&local_neutral, "local") };
                let backup = self
                    .conflicts_dir
                    .join(format!("{}-{}-{side}.jsonl", session.id, paths::now_ms()));
                write_atomic(&backup, loser.as_bytes())?;
                report.conflicts.push(format!(
                    "« {} » modifiée sur deux PC, version {side} sauvegardée dans {}",
                    session.title,
                    backup.display()
                ));
                push
            }
        };

        if push_wins {
            self.push(key, session, &local_neutral, &local_hash)?;
            state.hashes.insert(state_key, local_hash);
            report.pushed += 1;
        } else if !open_here && self.pull(key, &remote, &session.cwd)? {
            state.hashes.insert(state_key, remote.hash.clone());
            report.pulled += 1;
        }
        Ok(())
    }

    fn push(&self, key: &str, session: &LocalSession, neutral: &str, hash: &str) -> anyhow::Result<()> {
        let dir = self.project_dir(key).join("sessions");
        write_atomic(&dir.join(format!("{}.jsonl", session.id)), neutral.as_bytes())?;
        save_json(
            &dir.join(format!("{}.meta.json", session.id)),
            &RemoteMeta {
                id: session.id.clone(),
                project_key: key.to_string(),
                title: session.title.clone(),
                first_prompt: session.first_prompt.clone(),
                git_branch: session.git_branch.clone(),
                prompt_count: session.prompt_count,
                updated_at: session.updated_at,
                hash: hash.to_string(),
                machine_id: self.machine.id.clone(),
                machine_name: self.machine_name.to_string(),
            },
        )?;
        let home = home();
        let local_extra = session.path.with_extension("");
        mirror(&local_extra, &dir.join(&session.id), |text| neutralize(text, &session.cwd, &home, true));
        mirror(
            &paths::file_history_dir().join(&session.id),
            &self.root.join("file-history").join(&session.id),
            str::to_string,
        );
        Ok(())
    }

    /// Returns `false` when the remote copy is not fully synced yet.
    fn pull(&self, key: &str, meta: &RemoteMeta, local_root: &str) -> anyhow::Result<bool> {
        let dir = self.project_dir(key).join("sessions");
        let neutral = std::fs::read_to_string(dir.join(format!("{}.jsonl", meta.id)))?;
        // Syncthing and co. may deliver the meta before the session itself.
        if sha(&neutral) != meta.hash {
            return Ok(false);
        }
        let home = home();
        let project_dir = paths::local_project_dir(local_root);
        let target = project_dir.join(format!("{}.jsonl", meta.id));
        write_atomic(&target, localize(&neutral, local_root, &home, true).as_bytes())?;
        mirror(&dir.join(&meta.id), &project_dir.join(&meta.id), |text| {
            localize(text, local_root, &home, true)
        });
        mirror(
            &self.root.join("file-history").join(&meta.id),
            &paths::file_history_dir().join(&meta.id),
            str::to_string,
        );
        Ok(true)
    }

    // ---------- memory ----------

    fn sync_memory(&self, key: &str, local_root: &str, state: &mut SyncState, report: &mut SyncReport) {
        let home = home();
        let local_dir = paths::local_project_dir(local_root).join("memory");
        let remote_dir = self.project_dir(key).join("memory");
        let mut rels: HashSet<PathBuf> = HashSet::new();
        for dir in [&local_dir, &remote_dir] {
            for f in walk_files(dir) {
                if let Ok(rel) = f.strip_prefix(dir) {
                    rels.insert(rel.to_path_buf());
                }
            }
        }
        for rel in rels {
            let local = local_dir.join(&rel);
            let remote = remote_dir.join(&rel);
            let state_key = format!("m:{key}/{}", rel.to_string_lossy().replace('\\', "/"));
            let local_text = std::fs::read_to_string(&local)
                .ok()
                .map(|t| neutralize(&t, local_root, &home, false));
            let remote_text = std::fs::read_to_string(&remote).ok();
            let local_hash = local_text.as_deref().map(sha);
            let remote_hash = remote_text.as_deref().map(sha);
            if local_hash == remote_hash {
                if let Some(h) = local_hash {
                    state.hashes.insert(state_key, h);
                }
                continue;
            }
            let base = state.hashes.get(&state_key);
            let push = match (&local_hash, &remote_hash) {
                (Some(_), None) => true,
                (None, Some(_)) => false,
                (Some(l), Some(r)) => {
                    if base == Some(r) {
                        true
                    } else if base == Some(l) {
                        false
                    } else {
                        paths::mtime_ms(&local) >= paths::mtime_ms(&remote)
                    }
                }
                (None, None) => continue,
            };
            let result = if push {
                let text = local_text.unwrap_or_default();
                state.hashes.insert(state_key, sha(&text));
                write_atomic(&remote, text.as_bytes())
            } else {
                let text = remote_text.unwrap_or_default();
                state.hashes.insert(state_key, sha(&text));
                write_atomic(&local, localize(&text, local_root, &home, false).as_bytes())
            };
            match result {
                Ok(()) if push => report.pushed += 1,
                Ok(()) => report.pulled += 1,
                Err(e) => report.errors.push(format!("Mémoire {} : {e}", rel.display())),
            }
        }
    }
}

/// Copies files from `src` to `dst` when missing or older on the destination.
/// Text files go through `transform` (path placeholders).
fn mirror(src: &Path, dst: &Path, transform: impl Fn(&str) -> String) {
    for file in walk_files(src) {
        let Ok(rel) = file.strip_prefix(src) else { continue };
        let target = dst.join(rel);
        if target.exists() && paths::mtime_ms(&target) >= paths::mtime_ms(&file) {
            continue;
        }
        let content = if paths::is_text_file(&file) {
            match std::fs::read_to_string(&file) {
                Ok(text) => transform(&text).into_bytes(),
                Err(_) => continue,
            }
        } else {
            match std::fs::read(&file) {
                Ok(bytes) => bytes,
                Err(_) => continue,
            }
        };
        let _ = write_atomic(&target, &content);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sessions::SessionIndex;

    fn syncer<'a>(root: &Path, machine: &'a Machine, name: &'a str, data: &Path, index: &'a SessionIndex) -> Syncer<'a> {
        Syncer {
            root: root.to_path_buf(),
            machine,
            machine_name: name,
            state_path: data.join("state.json"),
            conflicts_dir: data.join("conflicts"),
            index,
            open_sessions: HashSet::new(),
        }
    }

    fn line(cwd: &str, text: &str) -> String {
        format!(
            "{}\n",
            serde_json::json!({"type":"user","cwd":cwd,"sessionId":"s1","message":{"role":"user","content":text}})
        )
    }

    /// Two machines with different project paths hand a session back and forth.
    #[test]
    fn session_roundtrip_between_machines() {
        let tmp = std::env::temp_dir().join(format!("cl-test-{}", uuid::Uuid::new_v4()));
        let shared = tmp.join("shared");
        let (home_a, home_b) = (tmp.join("claude-a"), tmp.join("claude-b"));
        let (proj_a, proj_b) = (tmp.join("pc-a/work/app"), tmp.join("pc-b/dev/app"));
        for d in [&proj_a, &proj_b] {
            std::fs::create_dir_all(d).unwrap();
        }
        let (pa, pb) = (proj_a.to_string_lossy().to_string(), proj_b.to_string_lossy().to_string());
        let id = "11111111-2222-3333-4444-555555555555";

        // Machine A writes a session.
        std::env::set_var("CLAUDE_CONFIG_DIR", &home_a);
        let file_a = paths::local_project_dir(&pa).join(format!("{id}.jsonl"));
        std::fs::create_dir_all(file_a.parent().unwrap()).unwrap();
        std::fs::write(&file_a, line(&pa, &format!("lis {pa}/src/main.rs"))).unwrap();
        std::fs::create_dir_all(paths::local_project_dir(&pa).join("memory")).unwrap();
        std::fs::write(paths::local_project_dir(&pa).join("memory/MEMORY.md"), format!("projet dans {pa}")).unwrap();
        let (ma, mb) = (Machine { id: "A".into() }, Machine { id: "B".into() });
        let index_a = SessionIndex::default();
        let report = syncer(&shared, &ma, "pc-a", &tmp.join("data-a"), &index_a).sync_all();
        assert_eq!(report.errors, Vec::<String>::new());
        assert_eq!(report.pushed, 2);

        // Machine B maps the project to its own folder and imports it.
        std::env::set_var("CLAUDE_CONFIG_DIR", &home_b);
        let index_b = SessionIndex::default();
        let sb = syncer(&shared, &mb, "pc-b", &tmp.join("data-b"), &index_b);
        let identity = index_b.project_identity(&pb);
        assert_eq!(identity.key, "local-app");
        sb.set_mapping(&identity, &pb).unwrap();
        let report = sb.sync_all();
        assert_eq!(report.errors, Vec::<String>::new());
        let file_b = paths::local_project_dir(&pb).join(format!("{id}.jsonl"));
        let imported = std::fs::read_to_string(&file_b).unwrap();
        assert!(imported.contains(&format!("\"cwd\":\"{pb}\"")), "{imported}");
        assert!(imported.contains(&format!("lis {pb}/src/main.rs")));
        let memory = std::fs::read_to_string(paths::local_project_dir(&pb).join("memory/MEMORY.md")).unwrap();
        assert_eq!(memory, format!("projet dans {pb}"));

        // B continues the conversation; a second sync is a no-op.
        std::thread::sleep(std::time::Duration::from_millis(20));
        let mut f = std::fs::OpenOptions::new().append(true).open(&file_b).unwrap();
        std::io::Write::write_all(&mut f, line(&pb, "suite sur B").as_bytes()).unwrap();
        let report = sb.sync_all();
        assert_eq!((report.pushed, report.pulled), (1, 0));
        let report = sb.sync_all();
        assert_eq!((report.pushed, report.pulled), (0, 0));

        // A gets B's turn with its own paths.
        std::env::set_var("CLAUDE_CONFIG_DIR", &home_a);
        let report = syncer(&shared, &ma, "pc-a", &tmp.join("data-a"), &index_a).sync_all();
        assert_eq!((report.pushed, report.pulled), (0, 1));
        let back = std::fs::read_to_string(&file_a).unwrap();
        assert!(back.contains("suite sur B"));
        assert!(!back.contains(&pb));
        assert_eq!(back.matches(&format!("\"cwd\":\"{pa}\"")).count(), 2);

        // Divergent edits on both sides: newest wins, the other is backed up.
        std::fs::write(&file_a, format!("{}{}", back, line(&pa, "A diverge"))).unwrap();
        std::env::set_var("CLAUDE_CONFIG_DIR", &home_b);
        std::fs::write(&file_b, line(&pb, "B réécrit")).unwrap();
        let report = sb.sync_all();
        assert_eq!(report.pushed, 1);
        std::env::set_var("CLAUDE_CONFIG_DIR", &home_a);
        let report = syncer(&shared, &ma, "pc-a", &tmp.join("data-a"), &index_a).sync_all();
        assert_eq!(report.conflicts.len(), 1, "{report:?}");
        assert_eq!(std::fs::read_dir(tmp.join("data-a/conflicts")).unwrap().count(), 1);

        // Locks.
        let sa = syncer(&shared, &ma, "pc-a", &tmp.join("data-a"), &index_a);
        sa.acquire_lock(id).unwrap();
        assert!(sa.foreign_lock(id).is_none());
        assert_eq!(sb.foreign_lock(id).unwrap().machine_name, "pc-a");
        sb.release_lock(id);
        assert!(sb.foreign_lock(id).is_some());
        sa.release_lock(id);
        assert!(sb.foreign_lock(id).is_none());

        std::env::remove_var("CLAUDE_CONFIG_DIR");
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
