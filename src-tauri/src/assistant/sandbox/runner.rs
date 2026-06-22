use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Stdio;

use tokio::io::AsyncReadExt;
use tokio::process::{Child, Command};
use tokio::task::JoinHandle;
use tokio::time::{timeout, Duration};

use super::SandboxProfile;

/// Grace period for reaping a child after it is SIGKILLed on timeout.
///
/// SIGKILL cannot reap a process stuck in uninterruptible sleep (D state,
/// e.g. blocked on a shared `target/` build-dir lock or NFS) until it leaves
/// that state, so an unbounded post-kill `wait()` could hang the executor
/// forever. Bounding it means a wedged child can't take the whole tool call —
/// and, via the shared session, sibling sessions — down with it; the OS still
/// reaps the process once it unblocks. The same bound guards the stdout/stderr
/// reader joins so a grandchild holding a pipe fd open can't hang us either.
const POST_KILL_REAP_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone)]
pub struct SandboxCommand {
    pub argv: Vec<OsString>,
    pub cwd: PathBuf,
    pub timeout_ms: u64,
    pub max_output_chars: usize,
    pub profile: SandboxProfile,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxCommandOutput {
    pub cwd: PathBuf,
    pub exit_code: Option<i32>,
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

pub async fn run_command(command: SandboxCommand) -> Result<SandboxCommandOutput, String> {
    if command.argv.is_empty() {
        return Err("Sandbox command argv cannot be empty".to_string());
    }
    if !command.cwd.is_dir() {
        return Err(format!(
            "Shell cwd does not exist or is not a directory: {}",
            command.cwd.display()
        ));
    }

    #[cfg(target_os = "linux")]
    {
        super::linux_bwrap::run(command).await
    }

    #[cfg(target_os = "macos")]
    {
        super::macos_seatbelt::run(command).await
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        super::unsupported::run(command).await
    }
}

pub(crate) async fn run_spawned_child(
    mut child: Child,
    cwd: PathBuf,
    timeout_ms: u64,
    output_limit: usize,
    timeout_label: &str,
) -> Result<SandboxCommandOutput, String> {
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "Failed to capture command stdout".to_string())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "Failed to capture command stderr".to_string())?;

    let stdout_task = tokio::spawn(async move {
        let mut reader = tokio::io::BufReader::new(stdout);
        let mut buf = Vec::new();
        reader.read_to_end(&mut buf).await.map(|_| buf)
    });
    let stderr_task = tokio::spawn(async move {
        let mut reader = tokio::io::BufReader::new(stderr);
        let mut buf = Vec::new();
        reader.read_to_end(&mut buf).await.map(|_| buf)
    });

    // Capture the pid up front: on timeout we signal the child's whole process
    // group (it was spawned as a group leader, see `prepare_stdio`), and
    // `child.id()` returns `None` once the child has been awaited/reaped.
    let child_pid = child.id();

    let status = match timeout(Duration::from_millis(timeout_ms), child.wait()).await {
        Ok(result) => result.map_err(|e| format!("Shell command failed: {}", e))?,
        Err(_) => {
            // Timed out. SIGKILL the entire process group so descendants
            // (bwrap/sandbox-exec -> sh -> cargo -> rustc, …) die too — a plain
            // `child.kill()` only reaps the direct child and would leave the
            // grandchildren orphaned and alive. Then bound every post-kill
            // await so a D-state child or a grandchild holding a pipe open
            // can't wedge the executor (see POST_KILL_REAP_TIMEOUT).
            kill_process_group(child_pid);
            // Fallback for the unknown-pid / non-Unix case where the group
            // kill is a no-op: at least signal the direct child.
            let _ = child.start_kill();
            let _ = timeout(POST_KILL_REAP_TIMEOUT, child.wait()).await;
            drain_or_abort(stdout_task).await;
            drain_or_abort(stderr_task).await;
            return Err(format!(
                "{} timed out after {} ms",
                timeout_label, timeout_ms
            ));
        }
    };

    let stdout_bytes = stdout_task
        .await
        .map_err(|e| format!("Failed to collect stdout: {}", e))?
        .map_err(|e| format!("Failed to read stdout: {}", e))?;
    let stderr_bytes = stderr_task
        .await
        .map_err(|e| format!("Failed to collect stderr: {}", e))?
        .map_err(|e| format!("Failed to read stderr: {}", e))?;

    Ok(SandboxCommandOutput {
        cwd,
        exit_code: status.code(),
        success: status.success(),
        stdout: truncate_string(
            String::from_utf8_lossy(&stdout_bytes).into_owned(),
            output_limit,
        ),
        stderr: truncate_string(
            String::from_utf8_lossy(&stderr_bytes).into_owned(),
            output_limit,
        ),
    })
}

/// SIGKILL the child's entire process group.
///
/// The child is spawned as a process-group leader (`prepare_stdio` calls
/// `process_group(0)`), so its pgid equals its pid; signalling the negated pid
/// reaches every descendant that has not started its own group. A no-op when
/// the pid is unknown (already reaped) — the caller's `start_kill()` is then
/// the only available reap — and off Unix, where process groups aren't used.
#[cfg(unix)]
fn kill_process_group(pid: Option<u32>) {
    let Some(pid) = pid else { return };
    // SAFETY: `kill(2)` with a negative pid targets a process group and has no
    // memory effects. A stale/already-dead group yields ESRCH, which we ignore.
    unsafe {
        libc::kill(-(pid as i32), libc::SIGKILL);
    }
}

#[cfg(not(unix))]
fn kill_process_group(_pid: Option<u32>) {}

/// Await a reader task, but give up and abort it after `POST_KILL_REAP_TIMEOUT`
/// so a pipe fd held open by a surviving grandchild can't hang the timeout
/// path forever.
async fn drain_or_abort(mut task: JoinHandle<std::io::Result<Vec<u8>>>) {
    if timeout(POST_KILL_REAP_TIMEOUT, &mut task).await.is_err() {
        task.abort();
    }
}

pub(crate) fn prepare_stdio(command: &mut Command) {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Run the child as its own process-group leader (pgid == child pid) so a
    // timeout can SIGKILL the whole tree — sandbox launcher, shell, and any
    // build/test grandchildren — rather than only the direct child. See
    // `kill_process_group`.
    #[cfg(unix)]
    command.process_group(0);
}

fn truncate_string(text: String, limit: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= limit {
        return text;
    }
    chars[..limit].iter().collect::<String>() + "\n...[truncated]"
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use tokio::process::Command;

    /// Regression guard for the orphaned-process-tree bug: on timeout we must
    /// kill the child's whole process group, not just the direct child. The
    /// script backgrounds a grandchild that touches a marker after a delay,
    /// then blocks. With only a direct-child kill the backgrounded grandchild
    /// survives and creates the marker; with the process-group kill it dies
    /// with its parent and the marker never appears.
    #[tokio::test]
    async fn timeout_kills_whole_process_group_not_just_direct_child() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("grandchild-ran");
        let script = format!(
            "( sleep 3; touch '{}' ) & sleep 30",
            marker.to_str().unwrap()
        );

        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c").arg(&script);
        prepare_stdio(&mut cmd);
        let child = cmd.spawn().expect("spawn /bin/sh");

        let result =
            run_spawned_child(child, dir.path().to_path_buf(), 300, 1_000, "Test command").await;

        let err = result.expect_err("command should time out");
        assert!(err.contains("timed out"), "unexpected error: {err}");

        // Wait past the grandchild's 3s delay; if the group kill worked it was
        // reaped before it could run.
        tokio::time::sleep(Duration::from_secs(5)).await;
        assert!(
            !marker.exists(),
            "grandchild survived the timeout — its process group was not reaped"
        );
    }

    /// The success path still returns the child's output unchanged.
    #[tokio::test]
    async fn returns_output_when_command_completes_within_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c").arg("printf out; printf err 1>&2; exit 7");
        prepare_stdio(&mut cmd);
        let child = cmd.spawn().expect("spawn /bin/sh");

        let out = run_spawned_child(
            child,
            dir.path().to_path_buf(),
            5_000,
            1_000,
            "Test command",
        )
        .await
        .expect("command should complete");

        assert_eq!(out.stdout, "out");
        assert_eq!(out.stderr, "err");
        assert_eq!(out.exit_code, Some(7));
        assert!(!out.success);
    }
}
