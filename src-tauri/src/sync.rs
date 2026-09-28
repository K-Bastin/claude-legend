//! Synchronisation of sessions through a [`Remote`] store.
//!
//! Layout of the store root:
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
//!
//! A cycle starts by indexing the store; if that fails nothing is written, so
//! a network hiccup can never make a remote session look missing and get
//! overwritten.

use crate::config::{load_json, save_json, Machine};
use crate::paths::{self, localize, neutralize, write_atomic};
use crate::sessions::{LocalSession, ProjectIdentity, SessionIndex};
use crate::store::{join, Entry, Remote};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::cell::RefCell;
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

impl LockInfo {
    /// Held by another machine that is still alive.
    pub fn is_foreign(&self, machine_id: &str, now: u64) -> bool {
        self.machine_id != machine_id && now.saturating_sub(self.heartbeat) < LOCK_STALE_MS
    }
}

#[derive(Debug, Clone)]
pub struct RemoteProject {
    pub info: ProjectInfo,
    /// Local path of the project on this machine.
    pub mapping: Option<String>,
    pub sessions: Vec<RemoteMeta>,
}

/// Snapshot of the store, refreshed by every sync. The UI reads it instead of
/// hitting the network.
#[derive(Debug, Clone, Default)]
pub struct RemoteIndex {
    pub at: u64,
    pub projects: Vec<RemoteProject>,
    pub locks: Vec<LockInfo>,
    /// Sessions whose meta is listed but could not be read (upload in
    /// progress, file busy…). They are left alone until the next cycle rather
    /// than treated as missing, which would overwrite them.
    pub unreadable: HashSet<String>,
}

impl RemoteIndex {
    fn project(&self, key: &str) -> Option<&RemoteProject> {
        self.projects.iter().find(|p| p.info.key == key)
    }

    fn meta(&self, key: &str, id: &str) -> Option<&RemoteMeta> {
        self.project(key)?.sessions.iter().find(|m| m.id == id)
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct SyncState {
    pub hashes: HashMap<String, String>,
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
    pub remote: RefCell<&'a mut Remote>,
    pub machine: &'a Machine,
    pub machine_name: &'a str,
    pub state_path: PathBuf,
    pub conflicts_dir: PathBuf,
    pub index: &'a SessionIndex,
    /// Sessions currently running here: never overwritten from the remote.
    pub open_sessions: HashSet<String>,
    /// Index built at the end of the last `sync_all`.
    pub fresh_index: RefCell<Option<RemoteIndex>>,
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
            if entry.file_name().to_string_lossy().ends_with(".cl-tmp") {
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

/// `a/b/c` form of a path relative to `base`.
fn rel_path(file: &Path, base: &Path) -> Option<String> {
    let rel = file.strip_prefix(base).ok()?;
    Some(
        rel.components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/"),
    )
}

fn local_path(base: &Path, rel: &str) -> PathBuf {
    rel.split('/')
        .filter(|s| !s.is_empty())
        .fold(base.to_path_buf(), |p, s| p.join(s))
}

fn project_dir(key: &str) -> String {
    format!("projects/{key}")
}

impl<'a> Syncer<'a> {
    // ---------- store access ----------

    fn read(&self, path: &str) -> anyhow::Result<Option<Vec<u8>>> {
        self.remote.borrow_mut().read(path)
    }

    fn read_text(&self, path: &str) -> anyhow::Result<Option<String>> {
        Ok(self
            .read(path)?
            .map(|d| String::from_utf8_lossy(&d).into_owned()))
    }

    /// Unreadable JSON (partial upload, foreign file) counts as missing.
    fn read_json<T: DeserializeOwned>(&self, path: &str) -> anyhow::Result<Option<T>> {
        Ok(self
            .read(path)?
            .and_then(|d| serde_json::from_slice(&d).ok()))
    }

    fn write(&self, path: &str, data: &[u8]) -> anyhow::Result<()> {
        self.remote.borrow_mut().write(path, data)
    }

    fn write_json<T: Serialize>(&self, path: &str, value: &T) -> anyhow::Result<()> {
        self.write(path, &serde_json::to_vec_pretty(value)?)
    }

    fn list(&self, dir: &str) -> anyhow::Result<Vec<Entry>> {
        self.remote.borrow_mut().list(dir)
    }

    fn walk(&self, dir: &str) -> anyhow::Result<Vec<(String, Entry)>> {
        self.remote.borrow_mut().walk(dir)
    }

    fn load_state(&self) -> SyncState {
        load_json(&self.state_path).unwrap_or_default()
    }

    pub fn register_machine(&self) -> anyhow::Result<()> {
        self.write_json(
            &format!("machines/{}.json", self.machine.id),
            &MachineInfo {
                id: self.machine.id.clone(),
                name: self.machine_name.to_string(),
                os: std::env::consts::OS.to_string(),
                last_seen: paths::now_ms(),
            },
        )
    }

    pub fn build_index(&self) -> anyhow::Result<RemoteIndex> {
        let mapping_name = format!("{}.json", self.machine.id);
        let mut projects = Vec::new();
        let mut unreadable = HashSet::new();
        for dir in self.list("projects")?.into_iter().filter(|e| e.is_dir) {
            let base = project_dir(&dir.name);
            // Listing first lets unchanged files come from the cache.
            self.list(&base)?;
            let Some(info) = self.read_json::<ProjectInfo>(&join(&base, "project.json"))? else {
                continue;
            };
            let mapping = if self
                .list(&join(&base, "paths"))?
                .iter()
                .any(|e| e.name == mapping_name)
            {
                self.read_json::<PathMapping>(&format!("{base}/paths/{mapping_name}"))?
                    .map(|m| m.path)
            } else {
                None
            };
            let mut sessions = Vec::new();
            for entry in self.list(&join(&base, "sessions"))? {
                let Some(id) = entry.name.strip_suffix(".meta.json") else {
                    continue;
                };
                match self.read_json(&format!("{base}/sessions/{}", entry.name))? {
                    Some(meta) => sessions.push(meta),
                    None => {
                        unreadable.insert(id.to_string());
                    }
                }
            }
            projects.push(RemoteProject {
                info,
                mapping,
                sessions,
            });
        }
        let mut locks = Vec::new();
        for entry in self.list("locks")? {
            if entry.name.ends_with(".json") {
                if let Some(lock) = self.read_json(&format!("locks/{}", entry.name))? {
                    locks.push(lock);
                }
            }
        }
        Ok(RemoteIndex {
            at: paths::now_ms(),
            projects,
            locks,
            unreadable,
        })
    }

    pub fn set_mapping(&self, identity: &ProjectIdentity, path: &str) -> anyhow::Result<()> {
        let base = project_dir(&identity.key);
        let project_file = join(&base, "project.json");
        if self.read(&project_file)?.is_none() {
            self.write_json(
                &project_file,
                &ProjectInfo {
                    key: identity.key.clone(),
                    name: identity.name.clone(),
                    git_remote: identity.git_remote.clone(),
                },
            )?;
        }
        self.write_json(
            &format!("{base}/paths/{}.json", self.machine.id),
            &PathMapping {
                path: path.to_string(),
            },
        )
    }

    // ---------- locks ----------

    fn lock_path(id: &str) -> String {
        format!("locks/{id}.json")
    }

    /// Returns the lock held by another machine, if it is still alive.
    pub fn foreign_lock(&self, id: &str) -> anyhow::Result<Option<LockInfo>> {
        let lock: Option<LockInfo> = self.read_json(&Self::lock_path(id))?;
        Ok(lock.filter(|l| l.is_foreign(&self.machine.id, paths::now_ms())))
    }

    pub fn acquire_lock(&self, id: &str) -> anyhow::Result<()> {
        let now = paths::now_ms();
        let since = self
            .read_json::<LockInfo>(&Self::lock_path(id))?
            .filter(|l| l.machine_id == self.machine.id)
            .map_or(now, |l| l.since);
        self.write_json(
            &Self::lock_path(id),
            &LockInfo {
                session_id: id.to_string(),
                machine_id: self.machine.id.clone(),
                machine_name: self.machine_name.to_string(),
                since,
                heartbeat: now,
            },
        )
    }

    pub fn release_lock(&self, id: &str) -> anyhow::Result<()> {
        let path = Self::lock_path(id);
        if self
            .read_json::<LockInfo>(&path)?
            .is_some_and(|l| l.machine_id == self.machine.id)
        {
            self.remote.borrow_mut().delete(&path)?;
        }
        Ok(())
    }

    // ---------- sessions ----------

    /// Full reconciliation of every local and remote session. The refreshed
    /// index is left in `fresh_index`.
    pub fn sync_all(&self) -> SyncReport {
        self.remote.borrow_mut().begin();
        let mut report = SyncReport {
            at: paths::now_ms(),
            ..Default::default()
        };
        let mut index = match self.register_machine().and_then(|_| self.build_index()) {
            Ok(index) => index,
            Err(e) => {
                report
                    .errors
                    .push(format!("Synchronisation impossible : {e:#}"));
                return report;
            }
        };
        let mut state = self.load_state();

        let locals = self.index.scan();
        let local_ids: HashSet<String> = locals.iter().map(|s| s.id.clone()).collect();
        let mut keys = HashSet::new();
        for session in &locals {
            let identity = self.index.project_identity(&session.cwd);
            keys.insert(identity.key.clone());
            if let Err(e) =
                self.sync_local_session(session, &identity, &mut index, &mut state, &mut report)
            {
                report.errors.push(format!("{} : {e:#}", session.title));
            }
        }

        for project in &index.projects {
            let Some(local_root) = &project.mapping else {
                continue;
            };
            keys.insert(project.info.key.clone());
            for meta in &project.sessions {
                if local_ids.contains(&meta.id) {
                    continue;
                }
                match self.pull(&project.info.key, meta, local_root) {
                    Ok(true) => {
                        state
                            .hashes
                            .insert(format!("s:{}", meta.id), meta.hash.clone());
                        report.pulled += 1;
                    }
                    Ok(false) => {}
                    Err(e) => report.errors.push(format!("{} : {e:#}", meta.title)),
                }
            }
        }

        for key in keys {
            if let Some(local_root) = index.project(&key).and_then(|p| p.mapping.clone()) {
                self.sync_memory(&key, &local_root, &mut state, &mut report);
            }
        }

        if let Err(e) = save_json(&self.state_path, &state) {
            report.errors.push(format!("État de synchro : {e}"));
        }
        match self.build_index() {
            Ok(index) => *self.fresh_index.borrow_mut() = Some(index),
            Err(e) => report.errors.push(format!("Index distant : {e:#}")),
        }
        report
    }

    /// Syncs one session before resuming it, so the latest remote turn is used.
    pub fn sync_one(&self, id: &str) -> SyncReport {
        self.remote.borrow_mut().begin();
        let mut report = SyncReport {
            at: paths::now_ms(),
            ..Default::default()
        };
        let mut index = match self.build_index() {
            Ok(index) => index,
            Err(e) => {
                report
                    .errors
                    .push(format!("Synchronisation impossible : {e:#}"));
                return report;
            }
        };
        let mut state = self.load_state();
        if let Some(session) = self.index.find(id) {
            let identity = self.index.project_identity(&session.cwd);
            if let Err(e) =
                self.sync_local_session(&session, &identity, &mut index, &mut state, &mut report)
            {
                report.errors.push(format!("{e:#}"));
            }
        } else if let Some((project, meta)) = index
            .projects
            .iter()
            .find_map(|p| p.sessions.iter().find(|m| m.id == id).map(|m| (p, m)))
        {
            if let Some(local_root) = &project.mapping {
                match self.pull(&project.info.key, meta, local_root) {
                    Ok(true) => {
                        state.hashes.insert(format!("s:{id}"), meta.hash.clone());
                        report.pulled += 1;
                    }
                    Ok(false) => report.errors.push(
                        "La session est encore en cours de synchronisation, réessaie dans un instant."
                            .into(),
                    ),
                    Err(e) => report.errors.push(format!("{e:#}")),
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
        index: &mut RemoteIndex,
        state: &mut SyncState,
        report: &mut SyncReport,
    ) -> anyhow::Result<()> {
        // A project gets mapped to the folder where its sessions are run.
        if index
            .project(&identity.key)
            .and_then(|p| p.mapping.as_ref())
            .is_none()
        {
            self.set_mapping(identity, &session.cwd)?;
            match index
                .projects
                .iter_mut()
                .find(|p| p.info.key == identity.key)
            {
                Some(project) => project.mapping = Some(session.cwd.clone()),
                None => index.projects.push(RemoteProject {
                    info: ProjectInfo {
                        key: identity.key.clone(),
                        name: identity.name.clone(),
                        git_remote: identity.git_remote.clone(),
                    },
                    mapping: Some(session.cwd.clone()),
                    sessions: Vec::new(),
                }),
            }
        }
        let key = &identity.key;
        let state_key = format!("s:{}", session.id);
        let home = home();
        let local_neutral = neutralize(
            &read_complete_lines(&session.path)?,
            &session.cwd,
            &home,
            true,
        );
        let local_hash = sha(&local_neutral);
        let base = state.hashes.get(&state_key).cloned();

        let Some(remote) = index.meta(key, &session.id).cloned() else {
            if index.unreadable.contains(&session.id) {
                return Ok(());
            }
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
            let remote_text = self
                .read_text(&format!(
                    "{}/sessions/{}.jsonl",
                    project_dir(key),
                    session.id
                ))?
                .unwrap_or_default();
            if remote_text.starts_with(&local_neutral) {
                false
            } else if local_neutral.starts_with(&remote_text) {
                true
            } else {
                let push = open_here || session.updated_at >= remote.updated_at;
                let (loser, side) = if push {
                    (&remote_text, "distant")
                } else {
                    (&local_neutral, "local")
                };
                let backup = self.conflicts_dir.join(format!(
                    "{}-{}-{side}.jsonl",
                    session.id,
                    paths::now_ms()
                ));
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

    fn push(
        &self,
        key: &str,
        session: &LocalSession,
        neutral: &str,
        hash: &str,
    ) -> anyhow::Result<()> {
        let dir = format!("{}/sessions", project_dir(key));
        self.write(&format!("{dir}/{}.jsonl", session.id), neutral.as_bytes())?;
        self.write_json(
            &format!("{dir}/{}.meta.json", session.id),
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
        let to_remote = |text: &str| neutralize(text, &session.cwd, &home, true);
        self.push_mirror(
            &session.path.with_extension(""),
            &format!("{dir}/{}", session.id),
            &to_remote,
        )?;
        self.push_mirror(
            &paths::file_history_dir().join(&session.id),
            &format!("file-history/{}", session.id),
            &str::to_string,
        )
    }

    /// Returns `false` when the remote copy is not fully synced yet.
    fn pull(&self, key: &str, meta: &RemoteMeta, local_root: &str) -> anyhow::Result<bool> {
        let dir = format!("{}/sessions", project_dir(key));
        let Some(neutral) = self.read_text(&format!("{dir}/{}.jsonl", meta.id))? else {
            return Ok(false);
        };
        // Syncthing and co. may deliver the meta before the session itself.
        if sha(&neutral) != meta.hash {
            return Ok(false);
        }
        let home = home();
        let project_dir = paths::local_project_dir(local_root);
        write_atomic(
            &project_dir.join(format!("{}.jsonl", meta.id)),
            localize(&neutral, local_root, &home, true).as_bytes(),
        )?;
        self.pull_mirror(
            &format!("{dir}/{}", meta.id),
            &project_dir.join(&meta.id),
            &|text: &str| localize(text, local_root, &home, true),
            &|text: &str| neutralize(text, local_root, &home, true),
        )?;
        self.pull_mirror(
            &format!("file-history/{}", meta.id),
            &paths::file_history_dir().join(&meta.id),
            &str::to_string,
            &str::to_string,
        )?;
        Ok(true)
    }

    /// Uploads local files whose remote copy is missing or differs in size.
    /// Text files go through `to_remote` (path placeholders).
    fn push_mirror(
        &self,
        local_dir: &Path,
        remote_dir: &str,
        to_remote: &dyn Fn(&str) -> String,
    ) -> anyhow::Result<()> {
        if !local_dir.is_dir() {
            return Ok(());
        }
        let remote: HashMap<String, u64> = self
            .walk(remote_dir)?
            .into_iter()
            .map(|(rel, e)| (rel, e.size))
            .collect();
        for file in walk_files(local_dir) {
            let Some(rel) = rel_path(&file, local_dir) else {
                continue;
            };
            let content = if paths::is_text_file(&file) {
                match std::fs::read_to_string(&file) {
                    Ok(text) => to_remote(&text).into_bytes(),
                    Err(_) => continue,
                }
            } else {
                match std::fs::read(&file) {
                    Ok(bytes) => bytes,
                    Err(_) => continue,
                }
            };
            if remote.get(&rel) != Some(&(content.len() as u64)) {
                self.write(&join(remote_dir, &rel), &content)?;
            }
        }
        Ok(())
    }

    /// Downloads remote files missing locally or whose content differs, sizes
    /// being compared in their machine-independent form.
    fn pull_mirror(
        &self,
        remote_dir: &str,
        local_dir: &Path,
        to_local: &dyn Fn(&str) -> String,
        to_remote: &dyn Fn(&str) -> String,
    ) -> anyhow::Result<()> {
        for (rel, entry) in self.walk(remote_dir)? {
            let target = local_path(local_dir, &rel);
            let text = paths::is_text_file(&target);
            let local_size = if text {
                std::fs::read_to_string(&target)
                    .ok()
                    .map(|t| to_remote(&t).len() as u64)
            } else {
                std::fs::metadata(&target).ok().map(|m| m.len())
            };
            if local_size == Some(entry.size) {
                continue;
            }
            let Some(data) = self.read(&join(remote_dir, &rel))? else {
                continue;
            };
            let data = if text {
                to_local(&String::from_utf8_lossy(&data)).into_bytes()
            } else {
                data
            };
            write_atomic(&target, &data)?;
        }
        Ok(())
    }

    // ---------- memory ----------

    fn sync_memory(
        &self,
        key: &str,
        local_root: &str,
        state: &mut SyncState,
        report: &mut SyncReport,
    ) {
        if let Err(e) = self.try_sync_memory(key, local_root, state, report) {
            report.errors.push(format!("Mémoire de {key} : {e:#}"));
        }
    }

    fn try_sync_memory(
        &self,
        key: &str,
        local_root: &str,
        state: &mut SyncState,
        report: &mut SyncReport,
    ) -> anyhow::Result<()> {
        let home = home();
        let local_dir = paths::local_project_dir(local_root).join("memory");
        let remote_dir = format!("{}/memory", project_dir(key));
        let remote_files: HashMap<String, Entry> = self.walk(&remote_dir)?.into_iter().collect();
        let mut rels: HashSet<String> = remote_files.keys().cloned().collect();
        rels.extend(
            walk_files(&local_dir)
                .iter()
                .filter_map(|f| rel_path(f, &local_dir)),
        );

        for rel in rels {
            let local = local_path(&local_dir, &rel);
            let remote = join(&remote_dir, &rel);
            let state_key = format!("m:{key}/{rel}");
            let local_text = std::fs::read_to_string(&local)
                .ok()
                .map(|t| neutralize(&t, local_root, &home, false));
            let remote_text = if remote_files.contains_key(&rel) {
                self.read_text(&remote)?
            } else {
                None
            };
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
                        paths::mtime_ms(&local) >= remote_files.get(&rel).map_or(0, |e| e.mtime)
                    }
                }
                (None, None) => continue,
            };
            if push {
                let text = local_text.unwrap_or_default();
                self.write(&remote, text.as_bytes())?;
                state.hashes.insert(state_key, sha(&text));
                report.pushed += 1;
            } else {
                let text = remote_text.unwrap_or_default();
                write_atomic(&local, localize(&text, local_root, &home, false).as_bytes())?;
                state.hashes.insert(state_key, sha(&text));
                report.pulled += 1;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sessions::SessionIndex;

    /// A shared folder, or a real server when `CL_TEST_SYNC_TARGET` holds a
    /// `SyncTarget` as JSON (password in `CL_TEST_SECRET`); a unique
    /// sub-directory keeps runs apart.
    fn remote(shared: &Path) -> Remote {
        use crate::config::SyncTarget;
        let run = shared
            .parent()
            .unwrap()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let target = match std::env::var("CL_TEST_SYNC_TARGET") {
            Ok(json) => match serde_json::from_str(&json).unwrap() {
                SyncTarget::Webdav { url, user } => SyncTarget::Webdav {
                    url: format!("{url}/{run}"),
                    user,
                },
                SyncTarget::Ftp {
                    host,
                    port,
                    user,
                    secure,
                    path,
                } => SyncTarget::Ftp {
                    host,
                    port,
                    user,
                    secure,
                    path: format!("{path}/{run}"),
                },
                SyncTarget::Sftp {
                    host,
                    port,
                    user,
                    auth,
                    key_path,
                    path,
                    fingerprint,
                } => SyncTarget::Sftp {
                    host,
                    port,
                    user,
                    auth,
                    key_path,
                    path: format!("{path}/{run}"),
                    fingerprint,
                },
                other => other,
            },
            Err(_) => SyncTarget::Folder {
                path: shared.to_string_lossy().into_owned(),
            },
        };
        Remote::new(target, std::env::var("CL_TEST_SECRET").ok())
    }

    fn syncer<'a>(
        remote: &'a mut Remote,
        machine: &'a Machine,
        name: &'a str,
        data: &Path,
        index: &'a SessionIndex,
    ) -> Syncer<'a> {
        remote.begin();
        Syncer {
            remote: RefCell::new(remote),
            machine,
            machine_name: name,
            state_path: data.join("state.json"),
            conflicts_dir: data.join("conflicts"),
            index,
            open_sessions: HashSet::new(),
            fresh_index: RefCell::new(None),
        }
    }

    /// A string as it appears inside JSON (backslashes of Windows paths doubled).
    fn json_str(s: &str) -> String {
        let quoted = serde_json::to_string(s).unwrap();
        quoted[1..quoted.len() - 1].to_string()
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
        let (proj_a, proj_b) = (
            tmp.join("pc-a").join("work").join("app"),
            tmp.join("pc-b").join("dev").join("app"),
        );
        for d in [&proj_a, &proj_b] {
            std::fs::create_dir_all(d).unwrap();
        }
        let (pa, pb) = (
            proj_a.to_string_lossy().to_string(),
            proj_b.to_string_lossy().to_string(),
        );
        let id = "11111111-2222-3333-4444-555555555555";

        // Machine A writes a session.
        std::env::set_var("CLAUDE_CONFIG_DIR", &home_a);
        let file_a = paths::local_project_dir(&pa).join(format!("{id}.jsonl"));
        std::fs::create_dir_all(file_a.parent().unwrap()).unwrap();
        std::fs::write(&file_a, line(&pa, &format!("lis {pa}/src/main.rs"))).unwrap();
        std::fs::create_dir_all(paths::local_project_dir(&pa).join("memory")).unwrap();
        std::fs::write(
            paths::local_project_dir(&pa).join("memory/MEMORY.md"),
            format!("projet dans {pa}"),
        )
        .unwrap();
        let (ma, mb) = (Machine { id: "A".into() }, Machine { id: "B".into() });
        let (mut ra, mut rb) = (remote(&shared), remote(&shared));
        let index_a = SessionIndex::default();
        let report = syncer(&mut ra, &ma, "pc-a", &tmp.join("data-a"), &index_a).sync_all();
        assert_eq!(report.errors, Vec::<String>::new());
        assert_eq!(report.pushed, 2);

        // Machine B maps the project to its own folder and imports it.
        std::env::set_var("CLAUDE_CONFIG_DIR", &home_b);
        let index_b = SessionIndex::default();
        let sb = syncer(&mut rb, &mb, "pc-b", &tmp.join("data-b"), &index_b);
        let identity = index_b.project_identity(&pb);
        assert_eq!(identity.key, "local-app");
        sb.set_mapping(&identity, &pb).unwrap();
        let report = sb.sync_all();
        assert_eq!(report.errors, Vec::<String>::new());
        let fresh = sb.fresh_index.borrow().clone().unwrap();
        assert_eq!(fresh.projects[0].sessions.len(), 1);
        assert_eq!(fresh.projects[0].mapping.as_deref(), Some(pb.as_str()));
        let file_b = paths::local_project_dir(&pb).join(format!("{id}.jsonl"));
        let imported = std::fs::read_to_string(&file_b).unwrap();
        assert!(
            imported.contains(&format!("\"cwd\":\"{}\"", json_str(&pb))),
            "{imported}"
        );
        assert!(imported.contains(&json_str(&format!("lis {pb}/src/main.rs"))));
        let memory =
            std::fs::read_to_string(paths::local_project_dir(&pb).join("memory/MEMORY.md"))
                .unwrap();
        assert_eq!(memory, format!("projet dans {pb}"));

        // B continues the conversation; a second sync is a no-op.
        std::thread::sleep(std::time::Duration::from_millis(20));
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&file_b)
            .unwrap();
        std::io::Write::write_all(&mut f, line(&pb, "suite sur B").as_bytes()).unwrap();
        let report = sb.sync_all();
        assert_eq!((report.pushed, report.pulled), (1, 0));
        sb.remote.borrow_mut().begin();
        let report = sb.sync_all();
        assert_eq!((report.pushed, report.pulled), (0, 0));

        // A gets B's turn with its own paths.
        std::env::set_var("CLAUDE_CONFIG_DIR", &home_a);
        let report = syncer(&mut ra, &ma, "pc-a", &tmp.join("data-a"), &index_a).sync_all();
        assert_eq!((report.pushed, report.pulled), (0, 1));
        let back = std::fs::read_to_string(&file_a).unwrap();
        assert!(back.contains("suite sur B"));
        assert!(!back.contains(&json_str(&pb)));
        assert_eq!(
            back.matches(&format!("\"cwd\":\"{}\"", json_str(&pa)))
                .count(),
            2
        );

        // Divergent edits on both sides: newest wins, the other is backed up.
        std::fs::write(&file_a, format!("{}{}", back, line(&pa, "A diverge"))).unwrap();
        std::env::set_var("CLAUDE_CONFIG_DIR", &home_b);
        std::fs::write(&file_b, line(&pb, "B réécrit")).unwrap();
        sb.remote.borrow_mut().begin();
        let report = sb.sync_all();
        assert_eq!(report.pushed, 1);
        std::env::set_var("CLAUDE_CONFIG_DIR", &home_a);
        let report = syncer(&mut ra, &ma, "pc-a", &tmp.join("data-a"), &index_a).sync_all();
        assert_eq!(report.conflicts.len(), 1, "{report:?}");
        assert_eq!(
            std::fs::read_dir(tmp.join("data-a/conflicts"))
                .unwrap()
                .count(),
            1
        );

        // Locks.
        let sa = syncer(&mut ra, &ma, "pc-a", &tmp.join("data-a"), &index_a);
        sa.acquire_lock(id).unwrap();
        assert!(sa.foreign_lock(id).unwrap().is_none());
        assert_eq!(sb.foreign_lock(id).unwrap().unwrap().machine_name, "pc-a");
        sb.release_lock(id).unwrap();
        assert!(sb.foreign_lock(id).unwrap().is_some());
        sa.release_lock(id).unwrap();
        assert!(sb.foreign_lock(id).unwrap().is_none());

        std::env::remove_var("CLAUDE_CONFIG_DIR");
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
