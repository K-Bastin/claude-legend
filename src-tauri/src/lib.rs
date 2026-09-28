mod config;
mod paths;
mod pty;
mod sessions;
mod store;
mod sync;

use config::{Machine, Settings, SyncTarget};
use portable_pty::PtySize;
use pty::{PtyEvent, PtyManager};
use serde::{Deserialize, Serialize};
use sessions::SessionIndex;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use store::{ConnectError, Remote};
use sync::{LockInfo, RemoteIndex, SyncReport, Syncer};
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
    /// Connection to the sync target, kept between operations. Its lock also
    /// serialises every sync operation.
    remote: Mutex<Option<Remote>>,
    /// Snapshot of the sync target used by the UI, refreshed by each full sync.
    remote_index: Mutex<Option<RemoteIndex>>,
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

    fn state_path(&self) -> PathBuf {
        self.data_dir.join("sync-state.json")
    }

    /// Runs `f` with a syncer when synchronisation is configured.
    fn with_syncer<T>(&self, f: impl FnOnce(&Syncer) -> T) -> Option<T> {
        let settings = self.settings.lock().unwrap().clone();
        if !settings.sync.is_enabled() {
            return None;
        }
        let open_sessions = self.open_session_ids();
        let mut guard = self.remote.lock().unwrap();
        if guard.as_ref().is_none_or(|r| r.target() != &settings.sync) {
            let secret = store::secret::load(&settings.sync);
            *guard = Some(Remote::new(settings.sync.clone(), secret));
        }
        let remote = guard.as_mut().unwrap();
        remote.begin();
        let syncer = Syncer {
            remote: RefCell::new(remote),
            machine: &self.machine,
            machine_name: &settings.machine_name,
            state_path: self.state_path(),
            conflicts_dir: self.data_dir.join("conflicts"),
            index: &self.index,
            open_sessions,
            fresh_index: RefCell::new(None),
        };
        let out = f(&syncer);
        if let Some(index) = syncer.fresh_index.take() {
            *self.remote_index.lock().unwrap() = Some(index);
        }
        Some(out)
    }

    fn run_full_sync(&self, app: &AppHandle) -> Option<SyncReport> {
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
    sync_label: String,
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
        sync_enabled: settings.sync.is_enabled(),
        sync_label: settings.sync.label(),
        last_report: state.last_report.lock().unwrap().clone(),
        home: paths::home_dir().display().to_string(),
    }
}

#[tauri::command]
fn get_settings(state: State<AppState>) -> Settings {
    state.settings.lock().unwrap().clone()
}

/// `secret` replaces the stored password when given (empty removes it).
#[tauri::command]
async fn save_settings(
    app: AppHandle,
    settings: Settings,
    secret: Option<String>,
) -> CmdResult<()> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        if let Some(secret) = secret {
            store::secret::save(&settings.sync, &secret).map_err(err)?;
        }
        config::save_json(&state.config_dir.join("settings.json"), &settings).map_err(err)?;
        *state.settings.lock().unwrap() = settings;
        // Waits for a running sync, then reconnects with the new settings.
        *state.remote.lock().unwrap() = None;
        *state.remote_index.lock().unwrap() = None;
        std::thread::spawn(move || {
            let state = app.state::<AppState>();
            state.run_full_sync(&app);
            let _ = app.emit("sessions-changed", ());
        });
        Ok(())
    })
    .await
    .map_err(err)?
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TestOutcome {
    ok: bool,
    message: String,
    /// Key presented by an SFTP server that is not trusted yet.
    unknown_fingerprint: Option<String>,
}

/// Connects to a sync target and checks it is writable. Uses the stored
/// password when `secret` is not given.
#[tauri::command]
async fn test_sync(target: SyncTarget, secret: Option<String>) -> CmdResult<TestOutcome> {
    tauri::async_runtime::spawn_blocking(move || {
        let secret = secret.or_else(|| store::secret::load(&target));
        Ok(match store::test(&target, secret.as_deref()) {
            Ok(()) => TestOutcome {
                ok: true,
                message: "Connexion réussie, lecture et écriture autorisées.".into(),
                unknown_fingerprint: None,
            },
            Err(ConnectError::UnknownHost { fingerprint }) => TestOutcome {
                ok: false,
                message: String::new(),
                unknown_fingerprint: Some(fingerprint),
            },
            Err(e) => TestOutcome {
                ok: false,
                message: e.to_string(),
                unknown_fingerprint: None,
            },
        })
    })
    .await
    .map_err(err)?
}

#[tauri::command]
fn has_sync_secret(target: SyncTarget) -> bool {
    store::secret::has(&target)
}

#[tauri::command]
fn list_sessions(state: State<AppState>) -> Vec<SessionEntry> {
    let open = state.open_session_ids();
    let locals = state.index.scan();
    let mut entries: Vec<SessionEntry> = Vec::new();
    let index = state
        .remote_index
        .lock()
        .unwrap()
        .clone()
        .unwrap_or_default();
    let mut remote: HashMap<String, (sync::RemoteMeta, sync::ProjectInfo, Option<String>)> =
        HashMap::new();
    for project in index.projects {
        for meta in project.sessions {
            remote.insert(
                meta.id.clone(),
                (meta, project.info.clone(), project.mapping.clone()),
            );
        }
    }
    // Liveness is judged at index time, so a long sync interval doesn't make
    // every lock look stale.
    let locks: HashMap<String, LockInfo> = index
        .locks
        .into_iter()
        .filter(|l| l.is_foreign(&state.machine.id, index.at))
        .map(|l| (l.session_id.clone(), l))
        .collect();
    let hashes = config::load_json::<sync::SyncState>(&state.state_path())
        .unwrap_or_default()
        .hashes;

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
    entries.sort_by_key(|e| std::cmp::Reverse(e.updated_at));
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

fn open_session_blocking(
    app: &AppHandle,
    request: OpenRequest,
    channel: Channel<PtyEvent>,
) -> CmdResult<OpenResult> {
    let state = app.state::<AppState>();
    let settings = state.settings.lock().unwrap().clone();
    let claude = config::resolve_claude(&settings)?;
    let cwd = PathBuf::from(&request.cwd);
    if !cwd.is_dir() {
        return Err(format!(
            "Le dossier {} n'existe pas sur ce PC.",
            request.cwd
        ));
    }
    let mut warnings = Vec::new();

    let (session_id, mut args) = match &request.session_id {
        Some(id) => {
            if let Some(report) = state.with_syncer(|s| s.sync_one(id)) {
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

    if let Some(Err(e)) = state.with_syncer(|s| s.acquire_lock(&session_id)) {
        warnings.push(format!("Verrou de session non posé : {e:#}"));
    }

    let exit_app = app.clone();
    let pty_id = state
        .ptys
        .spawn(
            &claude,
            &args,
            &cwd,
            PtySize {
                cols: request.cols.max(20),
                rows: request.rows.max(5),
                pixel_width: 0,
                pixel_height: 0,
            },
            channel,
            move |pty_id| {
                let state = exit_app.state::<AppState>();
                let session = state.open.lock().unwrap().remove(&pty_id);
                if let Some(session) = session {
                    state.with_syncer(|s| {
                        let _ = s.release_lock(&session);
                        s.sync_one(&session)
                    });
                }
                let _ = exit_app.emit("sessions-changed", ());
            },
        )
        .map_err(|e| format!("Impossible de lancer Claude : {e}"))?;
    state
        .open
        .lock()
        .unwrap()
        .insert(pty_id, session_id.clone());
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
async fn lock_status(app: AppHandle, session_id: String) -> CmdResult<Option<LockInfo>> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        state
            .with_syncer(|s| s.foreign_lock(&session_id))
            .transpose()
            .map(Option::flatten)
            .map_err(|e| format!("{e:#}"))
    })
    .await
    .map_err(err)?
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
                // Errors surface in the next sync report.
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
                remote: Mutex::new(None),
                remote_index: Mutex::new(None),
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
            test_sync,
            has_sync_secret,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app, event| {
            if let tauri::RunEvent::Exit = event {
                let state = app.state::<AppState>();
                let open = state.open_session_ids();
                state.ptys.kill_all();
                state.with_syncer(|s| {
                    for id in &open {
                        let _ = s.release_lock(id);
                    }
                });
            }
        });
}
