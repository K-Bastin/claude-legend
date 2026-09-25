mod config;
mod paths;
mod pty;
mod sessions;
mod sync;

use config::{Machine, Settings};
use pty::{PtyEvent, PtyManager};
use serde::{Deserialize, Serialize};
use sessions::SessionIndex;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use sync::{LockInfo, SyncReport, Syncer};
use tauri::ipc::Channel;
use tauri::{AppHandle, Emitter, Manager, State};

struct AppState {
    config_dir: PathBuf,
    data_dir: PathBuf,
    machine: Machine,
    settings: Mutex<Settings>,
    index: SessionIndex,
    ptys: PtyManager,
    /// pty id -> session id
    open: Mutex<HashMap<u32, String>>,
    sync_guard: Mutex<()>,
    last_report: Mutex<Option<SyncReport>>,
}

type CmdResult<T> = Result<T, String>;

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

impl AppState {
    fn open_session_ids(&self) -> HashSet<String> {
        self.open.lock().unwrap().values().cloned().collect()
    }

    /// Runs `f` with a syncer when a sync folder is configured.
    fn with_syncer<T>(&self, f: impl FnOnce(&Syncer) -> T) -> Option<T> {
        let settings = self.settings.lock().unwrap().clone();
        let dir = settings.sync_dir.filter(|d| !d.trim().is_empty())?;
        let syncer = Syncer {
            root: sync::sync_root(&dir),
            machine: &self.machine,
            machine_name: &settings.machine_name,
            state_path: self.data_dir.join("sync-state.json"),
            conflicts_dir: self.data_dir.join("conflicts"),
            index: &self.index,
            open_sessions: self.open_session_ids(),
        };
        Some(f(&syncer))
    }

    fn run_full_sync(&self, app: &AppHandle) -> Option<SyncReport> {
        let _guard = self.sync_guard.lock().unwrap();
        let report = self.with_syncer(|s| s.sync_all())?;
        *self.last_report.lock().unwrap() = Some(report.clone());
        let _ = app.emit("sync-report", &report);
        Some(report)
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionEntry {
    id: String,
    title: String,
    first_prompt: String,
    cwd: Option<String>,
    project_key: String,
    project_name: String,
    git_branch: Option<String>,
    prompt_count: u32,
    updated_at: u64,
    /// "local" or "remote" (only in the sync folder, not yet on this machine).
    location: &'static str,
    synced: bool,
    last_machine: Option<String>,
    locked_by: Option<LockInfo>,
    open_here: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AppInfo {
    machine_id: String,
    claude_path: Option<String>,
    claude_error: Option<String>,
    sync_enabled: bool,
    last_report: Option<SyncReport>,
    home: String,
}

#[tauri::command]
fn app_info(state: State<AppState>) -> AppInfo {
    let settings = state.settings.lock().unwrap().clone();
    let claude = config::resolve_claude(&settings);
    AppInfo {
        machine_id: state.machine.id.clone(),
        claude_path: claude.as_ref().ok().map(|p| p.display().to_string()),
        claude_error: claude.err(),
        sync_enabled: settings.sync_dir.is_some_and(|d| !d.trim().is_empty()),
        last_report: state.last_report.lock().unwrap().clone(),
        home: paths::home_dir().display().to_string(),
    }
}

#[tauri::command]
fn get_settings(state: State<AppState>) -> Settings {
    state.settings.lock().unwrap().clone()
}

#[tauri::command]
fn save_settings(app: AppHandle, state: State<AppState>, settings: Settings) -> CmdResult<()> {
    config::save_json(&state.config_dir.join("settings.json"), &settings).map_err(err)?;
    *state.settings.lock().unwrap() = settings;
    std::thread::spawn(move || {
        let state = app.state::<AppState>();
        state.run_full_sync(&app);
    });
    Ok(())
}

#[tauri::command]
fn list_sessions(state: State<AppState>) -> Vec<SessionEntry> {
    let open = state.open_session_ids();
    let locals = state.index.scan();
    let mut entries: Vec<SessionEntry> = Vec::new();
    let remote_info = state.with_syncer(|s| {
        let projects = s.projects();
        let mut remote: HashMap<String, (sync::RemoteMeta, sync::ProjectInfo, Option<String>)> = HashMap::new();
        for (project, mapping) in &projects {
            for meta in s.remote_sessions(&project.key) {
                remote.insert(meta.id.clone(), (meta, project.clone(), mapping.clone()));
            }
        }
        let ids: Vec<String> = locals
            .iter()
            .map(|l| l.id.clone())
            .chain(remote.keys().cloned())
            .collect();
        let locks: HashMap<String, LockInfo> = ids
            .into_iter()
            .filter_map(|id| s.foreign_lock(&id).map(|l| (id, l)))
            .collect();
        let state_hashes: HashMap<String, String> =
            config::load_json::<serde_json::Value>(&s.state_path)
                .and_then(|v| serde_json::from_value(v["hashes"].clone()).ok())
                .unwrap_or_default();
        (remote, locks, state_hashes)
    });
    let (mut remote, locks, hashes) = remote_info.unwrap_or_default();

    for local in locals {
        let identity = state.index.project_identity(&local.cwd);
        let remote_meta = remote.remove(&local.id);
        let synced = remote_meta
            .as_ref()
            .is_some_and(|(m, _, _)| hashes.get(&format!("s:{}", local.id)) == Some(&m.hash));
        entries.push(SessionEntry {
            open_here: open.contains(&local.id),
            locked_by: locks.get(&local.id).cloned(),
            last_machine: remote_meta.map(|(m, _, _)| m.machine_name),
            id: local.id,
            title: local.title,
            first_prompt: local.first_prompt,
            cwd: Some(local.cwd),
            project_key: identity.key,
            project_name: identity.name,
            git_branch: local.git_branch,
            prompt_count: local.prompt_count,
            updated_at: local.updated_at,
            location: "local",
            synced,
        });
    }
    for (id, (meta, project, mapping)) in remote {
        entries.push(SessionEntry {
            open_here: false,
            locked_by: locks.get(&id).cloned(),
            last_machine: Some(meta.machine_name),
            id,
            title: meta.title,
            first_prompt: meta.first_prompt,
            cwd: mapping,
            project_key: project.key,
            project_name: project.name,
            git_branch: meta.git_branch,
            prompt_count: meta.prompt_count,
            updated_at: meta.updated_at,
            location: "remote",
            synced: true,
        });
    }
    entries.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    entries
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct OpenRequest {
    /// Existing session to resume; a new one is started when absent.
    session_id: Option<String>,
    cwd: String,
    cols: u16,
    rows: u16,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct OpenResult {
    pty_id: u32,
    session_id: String,
    warnings: Vec<String>,
}

#[tauri::command]
async fn open_session(
    app: AppHandle,
    request: OpenRequest,
    channel: Channel<PtyEvent>,
) -> CmdResult<OpenResult> {
    tauri::async_runtime::spawn_blocking(move || open_session_blocking(&app, request, channel))
        .await
        .map_err(err)?
}

fn open_session_blocking(app: &AppHandle, request: OpenRequest, channel: Channel<PtyEvent>) -> CmdResult<OpenResult> {
    let state = app.state::<AppState>();
    let settings = state.settings.lock().unwrap().clone();
    let claude = config::resolve_claude(&settings)?;
    let cwd = PathBuf::from(&request.cwd);
    if !cwd.is_dir() {
        return Err(format!("Le dossier {} n'existe pas sur ce PC.", request.cwd));
    }
    let mut warnings = Vec::new();

    let (session_id, mut args) = match &request.session_id {
        Some(id) => {
            if let Some(report) = {
                let _guard = state.sync_guard.lock().unwrap();
                state.with_syncer(|s| s.sync_one(id))
            } {
                warnings.extend(report.errors);
                warnings.extend(report.conflicts);
            }
            if state.index.find(id).is_none() {
                return Err("Session introuvable sur ce PC : associe son projet à un dossier puis réessaie.".into());
            }
            (id.clone(), vec!["--resume".to_string(), id.clone()])
        }
        None => {
            let id = uuid::Uuid::new_v4().to_string();
            (id.clone(), vec!["--session-id".to_string(), id])
        }
    };
    args.extend(settings.extra_args.split_whitespace().map(str::to_string));

    state.with_syncer(|s| s.acquire_lock(&session_id));

    let exit_app = app.clone();
    let pty_id = state
        .ptys
        .spawn(&claude, &args, &cwd, request.cols.max(20), request.rows.max(5), channel, move |pty_id| {
            let state = exit_app.state::<AppState>();
            let session = state.open.lock().unwrap().remove(&pty_id);
            if let Some(session) = session {
                state.with_syncer(|s| {
                    s.release_lock(&session);
                    let _guard = state.sync_guard.lock().unwrap();
                    s.sync_one(&session)
                });
            }
            let _ = exit_app.emit("sessions-changed", ());
        })
        .map_err(|e| format!("Impossible de lancer Claude : {e}"))?;
    state.open.lock().unwrap().insert(pty_id, session_id.clone());
    Ok(OpenResult {
        pty_id,
        session_id,
        warnings,
    })
}

#[tauri::command]
fn pty_write(state: State<AppState>, id: u32, data: String) -> CmdResult<()> {
    state.ptys.write(id, &data).map_err(err)
}

#[tauri::command]
fn pty_resize(state: State<AppState>, id: u32, cols: u16, rows: u16) -> CmdResult<()> {
    state.ptys.resize(id, cols, rows).map_err(err)
}

#[tauri::command]
fn pty_kill(state: State<AppState>, id: u32) {
    state.ptys.kill(id);
}

#[tauri::command]
fn lock_status(state: State<AppState>, session_id: String) -> Option<LockInfo> {
    state.with_syncer(|s| s.foreign_lock(&session_id)).flatten()
}

#[tauri::command]
async fn sync_now(app: AppHandle) -> CmdResult<Option<SyncReport>> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        state.run_full_sync(&app)
    })
    .await
    .map_err(err)
}

#[tauri::command]
async fn map_project(app: AppHandle, project_key: String, path: String) -> CmdResult<()> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let identity = state.index.project_identity(&path);
        let identity = sessions::ProjectIdentity {
            key: project_key,
            ..identity
        };
        state
            .with_syncer(|s| s.set_mapping(&identity, &path))
            .ok_or("Synchronisation non configurée")?
            .map_err(err)?;
        state.run_full_sync(&app);
        Ok(())
    })
    .await
    .map_err(err)?
}

fn background_loop(app: AppHandle) {
    let mut last_sync: Option<Instant> = None;
    let mut last_heartbeat = Instant::now();
    loop {
        std::thread::sleep(Duration::from_secs(5));
        let state = app.state::<AppState>();
        let interval = state.settings.lock().unwrap().sync_interval_secs.max(15);
        if last_heartbeat.elapsed() >= Duration::from_secs(30) {
            last_heartbeat = Instant::now();
            for id in state.open_session_ids() {
                state.with_syncer(|s| s.acquire_lock(&id));
            }
        }
        if last_sync.is_none_or(|t| t.elapsed() >= Duration::from_secs(interval)) {
            last_sync = Some(Instant::now());
            state.run_full_sync(&app);
            let _ = app.emit("sessions-changed", ());
        }
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .setup(|app| {
            let config_dir = app.path().app_config_dir()?;
            let data_dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&config_dir)?;
            std::fs::create_dir_all(&data_dir)?;
            app.manage(AppState {
                machine: config::load_or_create_machine(&config_dir),
                settings: Mutex::new(config::load_settings(&config_dir)),
                config_dir,
                data_dir,
                index: SessionIndex::default(),
                ptys: PtyManager::default(),
                open: Mutex::new(HashMap::new()),
                sync_guard: Mutex::new(()),
                last_report: Mutex::new(None),
            });
            let handle = app.handle().clone();
            std::thread::spawn(move || background_loop(handle));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            app_info,
            get_settings,
            save_settings,
            list_sessions,
            open_session,
            pty_write,
            pty_resize,
            pty_kill,
            lock_status,
            sync_now,
            map_project,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app, event| {
            if let tauri::RunEvent::Exit = event {
                let state = app.state::<AppState>();
                let open = state.open_session_ids();
                state.ptys.kill_all();
                state.with_syncer(|s| open.iter().for_each(|id| s.release_lock(id)));
            }
        });
}
