//! Integrated terminal backend.
//!
//! A PTY-backed shell session streamed to the frontend (xterm.js). The
//! renderer-side cost in WebKitGTK (Tauri's Linux webview) is the throughput
//! risk, so the backend is built for throughput from the start.
//!
//! Design notes that matter for perf and correctness:
//!
//! - **Coalescing.** PTY output is read on a dedicated thread into a shared
//!   buffer; a separate flusher thread drains that buffer on a ~16ms cadence
//!   (≈60fps) and sends one `Channel` message per tick. This collapses the
//!   thousands of tiny reads a flood (`yes`, build logs) produces into at most
//!   ~60 IPC messages/sec, which is what keeps the webview from drowning. We
//!   never send per-read.
//! - **Binary safety.** PTY bytes are arbitrary (UTF-8 sequences split across
//!   reads, raw escape codes), so output is base64-encoded over the JSON
//!   `Channel` and decoded to a `Uint8Array` on the JS side. Input from
//!   xterm's `onData` is always a valid UTF-8 string, so it travels as-is.
//! - **Flatpak.** The sandbox image ships almost no host binaries (no `git` —
//!   see PR #60), so a shell run *inside* the sandbox is useless. We host-hop
//!   via `flatpak-spawn --host` and pass the working directory as
//!   `--directory=<dir>` (NOT `CommandBuilder::cwd`, which would only move the
//!   flatpak-spawn wrapper, not the host shell — the exact lesson from PR #60).
//!   Whether a PTY's tty semantics survive the flatpak-spawn portal hop still
//!   needs validation on a real Flatpak build.
//! - **Lifecycle.** The child is reaped by the reader thread on EOF; an
//!   explicit `terminal_close` kills the child (which unblocks the reader).

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use portable_pty::{native_pty_system, ChildKiller, CommandBuilder, MasterPty, PtySize};
use serde::Serialize;
use tauri::ipc::Channel;
use tauri::State;

use crate::AppState;

/// Flush cadence for coalesced PTY output (~60fps).
const FLUSH_INTERVAL: Duration = Duration::from_millis(16);
/// Read buffer size; large enough that a flood is a handful of reads, not many.
const READ_BUF: usize = 64 * 1024;

/// An event streamed from a live terminal session to the frontend.
#[derive(Clone, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum TerminalEvent {
    /// A coalesced chunk of PTY output, base64-encoded (bytes may not be valid
    /// UTF-8, so we don't send a `String`).
    Output {
        #[serde(rename = "dataB64")]
        data_b64: String,
    },
    /// The shell process exited; the session is finished.
    Exit { code: Option<u32> },
}

/// Live handles for one terminal session. Cloneable via `Arc`; every field is
/// behind a `Mutex` so the registry can hand out shared references that the
/// write/resize/close commands serialize against.
struct TerminalHandle {
    /// Master PTY — kept for `resize`.
    master: Mutex<Box<dyn MasterPty + Send>>,
    /// Write half — keystrokes/commands from the frontend.
    writer: Mutex<Box<dyn Write + Send>>,
    /// Kills the child on `terminal_close` (also unblocks the reader thread).
    killer: Mutex<Box<dyn ChildKiller + Send + Sync>>,
    /// Set once the session is torn down, so the flusher thread can stop.
    closed: Arc<AtomicBool>,
}

/// Registry of live terminal sessions, keyed by session id. Stored on
/// `AppState`; cheap to clone (just bumps the `Arc`).
#[derive(Clone, Default)]
pub struct TerminalRegistry {
    inner: Arc<Mutex<HashMap<String, Arc<TerminalHandle>>>>,
}

impl TerminalRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    fn insert(&self, id: String, handle: Arc<TerminalHandle>) {
        self.inner
            .lock()
            .expect("terminal registry poisoned")
            .insert(id, handle);
    }

    fn get(&self, id: &str) -> Option<Arc<TerminalHandle>> {
        self.inner
            .lock()
            .expect("terminal registry poisoned")
            .get(id)
            .cloned()
    }

    fn remove(&self, id: &str) -> Option<Arc<TerminalHandle>> {
        self.inner
            .lock()
            .expect("terminal registry poisoned")
            .remove(id)
    }

    /// Kill every live session. Called on app teardown (see `lib.rs`'s
    /// `RunEvent::Exit` hook) so PTY children and their reader/flusher threads
    /// don't outlive the app — the frontend's per-component `terminal_close`
    /// only fires on a graceful React unmount, not on window close or quit.
    pub fn close_all(&self) {
        // Drain under the lock, then kill without holding it (kill can block).
        let handles: Vec<Arc<TerminalHandle>> = {
            let mut map = self.inner.lock().expect("terminal registry poisoned");
            map.drain().map(|(_, handle)| handle).collect()
        };
        for handle in handles {
            handle.closed.store(true, Ordering::SeqCst);
            if let Ok(mut killer) = handle.killer.lock() {
                let _ = killer.kill();
            }
        }
    }
}

/// Resolve the shell to launch. Native: `$SHELL` (fallback `/bin/bash`), or
/// `%COMSPEC%`/`cmd.exe` on Windows. Under Flatpak we rely on the host's PATH
/// via `flatpak-spawn --host bash`, since the sandbox's `$SHELL` may point at a
/// path that doesn't exist on the host.
fn native_shell() -> String {
    if cfg!(windows) {
        std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".to_string())
    } else {
        // Trust $SHELL only if it actually exists. A stale value (e.g. left
        // over after switching shells, or inherited from another machine)
        // would spawn a process that exits immediately, giving a
        // flash-and-gone terminal. Fall back to bash, then sh.
        if let Ok(shell) = std::env::var("SHELL") {
            if !shell.is_empty() && Path::new(&shell).exists() {
                return shell;
            }
        }
        for candidate in ["/bin/bash", "/bin/sh"] {
            if Path::new(candidate).exists() {
                return candidate.to_string();
            }
        }
        "/bin/sh".to_string()
    }
}

/// Build the `CommandBuilder` for the shell, host-hopping under Flatpak.
///
/// Separated from `spawn_pty` and parameterized on `in_flatpak` so the argv
/// construction is unit-testable without a real sandbox (mirrors the approach
/// taken for the git host-hop in PR #60).
fn build_shell_command(in_flatpak: bool, dir: &Path) -> CommandBuilder {
    if in_flatpak {
        // Working dir MUST go through `--directory=`; `CommandBuilder::cwd`
        // would only move the flatpak-spawn wrapper, not the host shell.
        let mut cmd = CommandBuilder::new("flatpak-spawn");
        cmd.arg("--host");
        cmd.arg(format!("--directory={}", dir.display()));
        cmd.arg("--env=TERM=xterm-256color");
        cmd.arg("--env=COLORTERM=truecolor");
        cmd.arg("bash");
        cmd
    } else {
        let mut cmd = CommandBuilder::new(native_shell());
        cmd.cwd(dir);
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        cmd
    }
}

/// Decide the working directory: explicit `cwd` wins, else the workspace root,
/// else the user's home, else `/`.
fn resolve_cwd(state: &AppState, workspace_id: Option<&str>, cwd: Option<&str>) -> PathBuf {
    if let Some(cwd) = cwd {
        let trimmed = cwd.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed);
        }
    }
    if let Some(workspace_id) = workspace_id {
        if let Some(root) = state.workspace_root(workspace_id) {
            return root;
        }
    }
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"))
}

/// Spawn the reader + flusher threads for a session.
///
/// - Reader thread owns the PTY reader and the child: it blocks on reads,
///   appending into the shared `pending` buffer; on EOF it reaps the child
///   (recording the exit code) and flags `eof`.
/// - Flusher thread drains `pending` every `FLUSH_INTERVAL` and emits one
///   `Output` event per non-empty tick; once `eof` is set it does a final
///   drain, emits `Exit`, and stops.
fn spawn_io_threads(
    mut reader: Box<dyn Read + Send>,
    mut child: Box<dyn portable_pty::Child + Send + Sync>,
    channel: Channel<TerminalEvent>,
    closed: Arc<AtomicBool>,
) {
    let pending: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let eof = Arc::new(AtomicBool::new(false));
    let exit_code: Arc<Mutex<Option<u32>>> = Arc::new(Mutex::new(None));

    {
        let pending = Arc::clone(&pending);
        let eof = Arc::clone(&eof);
        let exit_code = Arc::clone(&exit_code);
        thread::spawn(move || {
            let mut buf = vec![0u8; READ_BUF];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        pending
                            .lock()
                            .expect("terminal pending buffer poisoned")
                            .extend_from_slice(&buf[..n]);
                    }
                    Err(_) => break,
                }
            }
            // Reap before flagging EOF so the flusher can report the code.
            if let Ok(status) = child.wait() {
                *exit_code.lock().expect("terminal exit code poisoned") = Some(status.exit_code());
            }
            eof.store(true, Ordering::SeqCst);
        });
    }

    {
        let pending = Arc::clone(&pending);
        thread::spawn(move || {
            loop {
                thread::sleep(FLUSH_INTERVAL);

                let chunk = {
                    let mut guard = pending.lock().expect("terminal pending buffer poisoned");
                    if guard.is_empty() {
                        None
                    } else {
                        Some(std::mem::take(&mut *guard))
                    }
                };
                if let Some(bytes) = chunk {
                    if channel
                        .send(TerminalEvent::Output {
                            data_b64: BASE64.encode(&bytes),
                        })
                        .is_err()
                    {
                        break; // frontend went away
                    }
                }

                // Explicit teardown (terminal_close / app-exit close_all): the
                // frontend initiated it and is gone, so stop promptly without
                // emitting an Exit event.
                if closed.load(Ordering::SeqCst) {
                    break;
                }
                if eof.load(Ordering::SeqCst) {
                    let tail = {
                        let mut guard = pending.lock().expect("terminal pending buffer poisoned");
                        std::mem::take(&mut *guard)
                    };
                    if !tail.is_empty() {
                        let _ = channel.send(TerminalEvent::Output {
                            data_b64: BASE64.encode(&tail),
                        });
                    }
                    let code = *exit_code.lock().expect("terminal exit code poisoned");
                    let _ = channel.send(TerminalEvent::Exit { code });
                    break;
                }
            }
        });
    }
}

/// Open a new terminal session and start streaming its output over `on_event`.
/// Returns the session id used by the write/resize/close commands.
#[tauri::command]
pub async fn terminal_open(
    state: State<'_, AppState>,
    workspace_id: Option<String>,
    cwd: Option<String>,
    cols: u16,
    rows: u16,
    on_event: Channel<TerminalEvent>,
) -> Result<String, String> {
    let dir = resolve_cwd(&state, workspace_id.as_deref(), cwd.as_deref());
    let in_flatpak = crate::providers::is_flatpak();

    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows: rows.max(1),
            cols: cols.max(1),
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| format!("Failed to open pty: {e}"))?;

    let cmd = build_shell_command(in_flatpak, &dir);
    let child = pair
        .slave
        .spawn_command(cmd)
        .map_err(|e| format!("Failed to spawn shell: {e}"))?;
    // Close the parent's handle to the slave so the reader sees EOF when the
    // child exits.
    drop(pair.slave);

    let reader = pair
        .master
        .try_clone_reader()
        .map_err(|e| format!("Failed to clone pty reader: {e}"))?;
    let writer = pair
        .master
        .take_writer()
        .map_err(|e| format!("Failed to take pty writer: {e}"))?;
    let killer = child.clone_killer();

    let closed = Arc::new(AtomicBool::new(false));
    let handle = Arc::new(TerminalHandle {
        master: Mutex::new(pair.master),
        writer: Mutex::new(writer),
        killer: Mutex::new(killer),
        closed: Arc::clone(&closed),
    });

    spawn_io_threads(reader, child, on_event, closed);

    let session_id = uuid::Uuid::new_v4().to_string();
    state.terminals.insert(session_id.clone(), handle);
    Ok(session_id)
}

/// Write frontend input (keystrokes, or a full command line) to the PTY.
#[tauri::command]
pub async fn terminal_write(
    state: State<'_, AppState>,
    session_id: String,
    data: String,
) -> Result<(), String> {
    let handle = state
        .terminals
        .get(&session_id)
        .ok_or("No such terminal session")?;
    let mut writer = handle
        .writer
        .lock()
        .map_err(|_| "terminal writer poisoned")?;
    writer
        .write_all(data.as_bytes())
        .map_err(|e| format!("Failed to write to terminal: {e}"))?;
    writer.flush().ok();
    Ok(())
}

/// Resize the PTY (xterm `onResize` → cols/rows in cells).
#[tauri::command]
pub async fn terminal_resize(
    state: State<'_, AppState>,
    session_id: String,
    cols: u16,
    rows: u16,
) -> Result<(), String> {
    let handle = state
        .terminals
        .get(&session_id)
        .ok_or("No such terminal session")?;
    handle
        .master
        .lock()
        .map_err(|_| "terminal master poisoned")?
        .resize(PtySize {
            rows: rows.max(1),
            cols: cols.max(1),
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| format!("Failed to resize terminal: {e}"))?;
    Ok(())
}

/// Close a terminal session: kill the child (which unblocks the reader thread)
/// and drop it from the registry.
#[tauri::command]
pub async fn terminal_close(state: State<'_, AppState>, session_id: String) -> Result<(), String> {
    if let Some(handle) = state.terminals.remove(&session_id) {
        handle.closed.store(true, Ordering::SeqCst);
        if let Ok(mut killer) = handle.killer.lock() {
            let _ = killer.kill();
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(cmd: &CommandBuilder) -> Vec<String> {
        cmd.get_argv()
            .iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn flatpak_command_host_hops_with_directory_flag() {
        let dir = Path::new("/home/u/.clai/workspaces/abc");
        let cmd = build_shell_command(true, dir);
        let argv = argv(&cmd);
        // Program + args; cwd goes via --directory=, not CommandBuilder::cwd.
        assert_eq!(argv[0], "flatpak-spawn");
        assert!(argv.contains(&"--host".to_string()));
        assert!(argv.contains(&"--directory=/home/u/.clai/workspaces/abc".to_string()));
        assert_eq!(argv.last().unwrap(), "bash");
        // The --directory flag must precede `--host bash` (flatpak-spawn opts
        // before the host command).
        let dir_idx = argv
            .iter()
            .position(|a| a.starts_with("--directory="))
            .unwrap();
        let host_idx = argv.iter().position(|a| a == "--host").unwrap();
        let bash_idx = argv.iter().position(|a| a == "bash").unwrap();
        assert!(dir_idx < bash_idx && host_idx < bash_idx);
    }

    #[test]
    fn native_command_uses_a_real_shell_not_flatpak_spawn() {
        let cmd = build_shell_command(false, Path::new("/tmp"));
        let argv = argv(&cmd);
        assert_ne!(argv[0], "flatpak-spawn");
        assert!(!argv.iter().any(|a| a.starts_with("--directory=")));
    }

    #[test]
    fn output_event_serializes_to_tagged_camelcase() {
        let json = serde_json::to_string(&TerminalEvent::Output {
            data_b64: "aGk=".to_string(),
        })
        .unwrap();
        assert_eq!(json, r#"{"type":"output","dataB64":"aGk="}"#);
    }

    #[test]
    fn exit_event_serializes_with_optional_code() {
        let json = serde_json::to_string(&TerminalEvent::Exit { code: Some(0) }).unwrap();
        assert_eq!(json, r#"{"type":"exit","code":0}"#);
        let json = serde_json::to_string(&TerminalEvent::Exit { code: None }).unwrap();
        assert_eq!(json, r#"{"type":"exit","code":null}"#);
    }
}
