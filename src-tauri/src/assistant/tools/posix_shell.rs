//! POSIX shell resolution for `bash_exec`.
//!
//! On Unix the shell is always `/bin/sh`. On Windows there is no guaranteed
//! POSIX shell, so we locate one: Git for Windows' **Git Bash** (the common
//! case — CLAI already needs Git on Windows for skills/agents, and the Git
//! installer bundles Git Bash), then **MSYS2**, then a bare `bash` resolved by
//! the OS PATH/PATHEXT.
//!
//! Why not `cmd.exe`/PowerShell: the integrated *terminal* uses `cmd.exe`, which
//! is correct there because the human types the commands. The agent, however,
//! emits POSIX shell (`ls`, `grep`, `cat`, pipes, `&&`, `/bin/sh -lc`), which
//! `cmd.exe` would spawn but then choke on. Only a real POSIX shell makes
//! `bash_exec` useful on Windows.
//!
//! When no POSIX shell exists at all, [`shell_argv`] returns an actionable
//! message ([`MISSING_SHELL_NOTICE`]) instead of an argv, so the caller can
//! guide the user ("install Git for Windows") rather than surfacing a generic
//! "program not found" spawn failure.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::providers::command_exists;

/// Environment variable a user (or the future Settings entry) can set to point
/// `bash_exec` at a specific POSIX shell, overriding auto-detection.
const SHELL_OVERRIDE_ENV: &str = "CLAI_POSIX_SHELL";

/// Guidance shown when no POSIX shell can be found on Windows. Surfaced both as
/// the tool error and a run notice so the run record explains the failure.
pub const MISSING_SHELL_NOTICE: &str = "No POSIX shell found, so shell commands cannot run. \
Install Git for Windows (https://git-scm.com/download/win) — it includes Git Bash — then restart CLAI. \
Advanced: set the CLAI_POSIX_SHELL environment variable to a bash.exe path.";

/// Build the argv that runs `command` through a login POSIX shell.
///
/// - Unix: `Ok(["/bin/sh", "-lc", <command>])`.
/// - Windows: `Ok([<bash>, "-lc", <command>])`, where `<bash>` is the first
///   POSIX shell found by [`find_windows_posix_shell`], or a bare `bash` when
///   one is resolvable on `PATH`. When none exists, `Err(MISSING_SHELL_NOTICE)`.
pub fn shell_argv(command: String) -> Result<Vec<OsString>, String> {
    if cfg!(windows) {
        let bash = find_windows_posix_shell()
            .map(PathBuf::into_os_string)
            .or_else(|| command_exists("bash").then(|| OsString::from("bash")));
        match bash {
            Some(bash) => Ok(vec![bash, OsString::from("-lc"), OsString::from(command)]),
            None => Err(MISSING_SHELL_NOTICE.to_string()),
        }
    } else {
        Ok(vec![
            OsString::from("/bin/sh"),
            OsString::from("-lc"),
            OsString::from(command),
        ])
    }
}

/// Locate a POSIX `bash` on Windows: the `CLAI_POSIX_SHELL` override first, then
/// the first existing path among the known Git for Windows / MSYS2 locations.
/// Returns `None` when none is found (the caller falls back to a bare `bash` on
/// PATH, then to [`MISSING_SHELL_NOTICE`]).
///
/// Compiled on all platforms (so non-Windows CI type-checks it); only invoked
/// from the `cfg!(windows)` branch of [`shell_argv`].
pub fn find_windows_posix_shell() -> Option<PathBuf> {
    if let Some(raw) = std::env::var_os(SHELL_OVERRIDE_ENV) {
        let path = PathBuf::from(raw);
        if !path.as_os_str().is_empty() && path.is_file() {
            return Some(path);
        }
    }
    select_existing_shell(&windows_shell_candidates(), |p| p.is_file())
}

/// Candidate POSIX-shell paths on Windows, in priority order: Git for Windows
/// (system + 32-bit + native + per-user installs), then MSYS2. Env-derived
/// bases come first so a relocated install is honored; hardcoded defaults catch
/// the case where the env vars are absent.
fn windows_shell_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    for base_env in ["ProgramFiles", "ProgramW6432", "ProgramFiles(x86)"] {
        if let Some(base) = std::env::var_os(base_env) {
            let base = PathBuf::from(base);
            out.push(base.join(r"Git\bin\bash.exe"));
            out.push(base.join(r"Git\usr\bin\bash.exe"));
        }
    }
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        let local = PathBuf::from(local);
        out.push(local.join(r"Programs\Git\bin\bash.exe"));
        out.push(local.join(r"Programs\Git\usr\bin\bash.exe"));
    }
    out.push(PathBuf::from(r"C:\Program Files\Git\bin\bash.exe"));
    out.push(PathBuf::from(r"C:\Program Files\Git\usr\bin\bash.exe"));
    out.push(PathBuf::from(r"C:\msys64\usr\bin\bash.exe"));
    out
}

/// Return the first candidate for which `exists` is true. Pure (the existence
/// check is injected) so the selection order is unit-testable off-Windows.
fn select_existing_shell(
    candidates: &[PathBuf],
    exists: impl Fn(&Path) -> bool,
) -> Option<PathBuf> {
    candidates
        .iter()
        .find(|candidate| exists(candidate.as_path()))
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unix_shell_argv_uses_bin_sh() {
        // The non-Windows branch is what this CI exercises.
        if !cfg!(windows) {
            let argv = shell_argv("echo hi".to_string()).expect("unix always has /bin/sh");
            assert_eq!(
                argv,
                vec![
                    OsString::from("/bin/sh"),
                    OsString::from("-lc"),
                    OsString::from("echo hi"),
                ]
            );
        }
    }

    #[test]
    fn select_existing_shell_picks_first_present_in_order() {
        let candidates = vec![
            PathBuf::from(r"C:\Program Files\Git\bin\bash.exe"),
            PathBuf::from(r"C:\msys64\usr\bin\bash.exe"),
        ];
        // Only the second exists -> it is chosen.
        let chosen = select_existing_shell(&candidates, |p| {
            p == Path::new(r"C:\msys64\usr\bin\bash.exe")
        });
        assert_eq!(chosen, Some(PathBuf::from(r"C:\msys64\usr\bin\bash.exe")));
    }

    #[test]
    fn select_existing_shell_prefers_earlier_candidate() {
        let candidates = vec![
            PathBuf::from(r"C:\Program Files\Git\bin\bash.exe"),
            PathBuf::from(r"C:\msys64\usr\bin\bash.exe"),
        ];
        // Both exist -> the earlier (Git Bash) wins over MSYS2.
        let chosen = select_existing_shell(&candidates, |_| true);
        assert_eq!(
            chosen,
            Some(PathBuf::from(r"C:\Program Files\Git\bin\bash.exe"))
        );
    }

    #[test]
    fn select_existing_shell_returns_none_when_absent() {
        let candidates = vec![PathBuf::from(r"C:\Program Files\Git\bin\bash.exe")];
        assert_eq!(select_existing_shell(&candidates, |_| false), None);
    }

    #[test]
    fn windows_candidates_are_nonempty_and_end_in_bash_exe() {
        let candidates = windows_shell_candidates();
        assert!(!candidates.is_empty());
        assert!(candidates
            .iter()
            .all(|p| p.to_string_lossy().ends_with("bash.exe")));
    }

    #[test]
    fn missing_shell_notice_is_actionable() {
        // Must name the concrete remedy so the run record is self-explanatory.
        assert!(MISSING_SHELL_NOTICE.contains("Git for Windows"));
        assert!(MISSING_SHELL_NOTICE.contains(SHELL_OVERRIDE_ENV));
    }
}
