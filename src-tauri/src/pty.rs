use portable_pty::{native_pty_system, ChildKiller, CommandBuilder, MasterPty, PtySize};
use serde::Serialize;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;
use tauri::ipc::Channel;

#[derive(Clone, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum PtyEvent {
    Data { data: String },
    Exit { code: Option<u32> },
}

struct PtyHandle {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    killer: Box<dyn ChildKiller + Send + Sync>,
}

#[derive(Default)]
pub struct PtyManager {
    next_id: AtomicU32,
    handles: Arc<Mutex<HashMap<u32, PtyHandle>>>,
}

fn build_command(program: &Path, args: &[String]) -> CommandBuilder {
    let is_script = program
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("cmd") || e.eq_ignore_ascii_case("bat"));
    let mut cmd = if is_script {
        let mut c = CommandBuilder::new("cmd.exe");
        c.arg("/c");
        c.arg(program);
        c
    } else {
        CommandBuilder::new(program)
    };
    cmd.args(args);
    cmd
}

/// Decodes UTF-8 across read boundaries so multi-byte characters split between
/// two reads are not mangled.
fn decode_chunk(pending: &mut Vec<u8>, chunk: &[u8]) -> String {
    pending.extend_from_slice(chunk);
    match std::str::from_utf8(pending) {
        Ok(s) => {
            let out = s.to_string();
            pending.clear();
            out
        }
        Err(e) if e.error_len().is_none() => {
            let valid = e.valid_up_to();
            let out = String::from_utf8_lossy(&pending[..valid]).into_owned();
            pending.drain(..valid);
            out
        }
        Err(_) => {
            let out = String::from_utf8_lossy(pending).into_owned();
            pending.clear();
            out
        }
    }
}

impl PtyManager {
    pub fn spawn(
        &self,
        program: &Path,
        args: &[String],
        cwd: &Path,
        size: PtySize,
        channel: Channel<PtyEvent>,
        on_exit: impl FnOnce(u32) + Send + 'static,
    ) -> anyhow::Result<u32> {
        let pair = native_pty_system().openpty(size)?;
        let mut cmd = build_command(program, args);
        cmd.cwd(cwd);
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        // Inherited when the app itself is started from a terminal or from a
        // Claude Code session. These describe the parent session and would
        // make `claude` misdetect its environment, e.g. CLAUDE_CODE_CHILD_SESSION
        // turns transcript saving off. User configuration (CLAUDE_CODE_USE_*,
        // …) is left untouched.
        for var in [
            "TERM_PROGRAM",
            "TERM_PROGRAM_VERSION",
            "CLAUDECODE",
            "CLAUDE_CODE_ENTRYPOINT",
            "CLAUDE_CODE_CHILD_SESSION",
            "CLAUDE_CODE_EXECPATH",
            "CLAUDE_CODE_MESSAGING_SOCKET",
            "CLAUDE_CODE_MESSAGING_TOKEN",
            "CLAUDE_CODE_SESSION_ATTENDED",
            "CLAUDE_CODE_SESSION_ID",
            "CLAUDE_EFFORT",
            "CLAUDE_PID",
        ] {
            cmd.env_remove(var);
        }

        let mut child = pair.slave.spawn_command(cmd)?;
        drop(pair.slave);
        let mut reader = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;
        let killer = child.clone_killer();

        let id = self.next_id.fetch_add(1, Ordering::SeqCst) + 1;
        self.handles.lock().unwrap().insert(
            id,
            PtyHandle {
                master: pair.master,
                writer,
                killer,
            },
        );

        let (exit_tx, exit_rx) = mpsc::channel::<Option<u32>>();
        let handles = self.handles.clone();
        std::thread::spawn(move || {
            let code = child.wait().ok().map(|s| s.exit_code());
            let _ = exit_tx.send(code);
            // ConPTY only signals EOF to the reader once the master is closed.
            if cfg!(windows) {
                std::thread::sleep(Duration::from_millis(200));
                handles.lock().unwrap().remove(&id);
            }
        });

        let handles = self.handles.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 16 * 1024];
            let mut pending = Vec::new();
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let data = decode_chunk(&mut pending, &buf[..n]);
                        if !data.is_empty() && channel.send(PtyEvent::Data { data }).is_err() {
                            break;
                        }
                    }
                }
            }
            let code = exit_rx.recv_timeout(Duration::from_secs(3)).ok().flatten();
            handles.lock().unwrap().remove(&id);
            let _ = channel.send(PtyEvent::Exit { code });
            on_exit(id);
        });

        Ok(id)
    }

    pub fn write(&self, id: u32, data: &str) -> anyhow::Result<()> {
        let mut handles = self.handles.lock().unwrap();
        let handle = handles
            .get_mut(&id)
            .ok_or_else(|| anyhow::anyhow!("terminal {id} fermé"))?;
        handle.writer.write_all(data.as_bytes())?;
        handle.writer.flush()?;
        Ok(())
    }

    pub fn resize(&self, id: u32, cols: u16, rows: u16) -> anyhow::Result<()> {
        let handles = self.handles.lock().unwrap();
        if let Some(handle) = handles.get(&id) {
            handle.master.resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })?;
        }
        Ok(())
    }

    pub fn kill(&self, id: u32) {
        if let Some(handle) = self.handles.lock().unwrap().get_mut(&id) {
            let _ = handle.killer.kill();
        }
    }

    pub fn kill_all(&self) {
        for handle in self.handles.lock().unwrap().values_mut() {
            let _ = handle.killer.kill();
        }
    }
}
