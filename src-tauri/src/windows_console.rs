//! Suppress the console window Windows attaches to spawned processes.
//!
//! CLAI's desktop binary is a GUI-subsystem app: it owns no console, so every
//! console-subsystem child it spawns (`where` probes, `.cmd`/`.bat` editor
//! shims, `git`, `taskkill`, provider CLIs, stdio MCP servers) makes
//! `CreateProcessW` allocate a brand-new **visible** console — the console
//! window that flashes on startup and on every host-app launch on Windows.
//!
//! [`CREATE_NO_WINDOW`] tells `CreateProcessW` to give the child a console
//! without a window. GUI children (explorer, VS Code, Windows Terminal) are
//! unaffected — the flag only suppresses console-window creation — so it is
//! safe to apply to every spawn EXCEPT ones whose visible console *is* the
//! product: launching `cmd`/`powershell` as the user's terminal (see
//! `system_apps::spawn_host_detached`, which routes that decision through
//! `system_apps::HostWindow`).
//!
//! On non-Windows targets the trait is a no-op, so call sites stay
//! platform-unconditional and compile everywhere.

/// Chainable helper that hides the child's console window on Windows.
///
/// NOTE: `creation_flags` REPLACES the command's stored flags rather than
/// ORing into them. These are currently the only `creation_flags` callers in
/// the repo; if a spawn site ever needs another flag (e.g.
/// `DETACHED_PROCESS`), combine it with `CREATE_NO_WINDOW` in one call
/// instead of calling `creation_flags` twice. (The flags std/tokio need
/// internally, like `CREATE_UNICODE_ENVIRONMENT`, are ORed in at spawn time
/// and cannot be clobbered from here.)
pub(crate) trait HideConsoleWindow {
    /// Apply `CREATE_NO_WINDOW` on Windows; no-op elsewhere.
    fn hide_console_window(&mut self) -> &mut Self;
}

#[cfg(windows)]
use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

impl HideConsoleWindow for std::process::Command {
    #[cfg(windows)]
    fn hide_console_window(&mut self) -> &mut Self {
        use std::os::windows::process::CommandExt;
        self.creation_flags(CREATE_NO_WINDOW)
    }

    #[cfg(not(windows))]
    fn hide_console_window(&mut self) -> &mut Self {
        self
    }
}

impl HideConsoleWindow for tokio::process::Command {
    #[cfg(windows)]
    fn hide_console_window(&mut self) -> &mut Self {
        self.creation_flags(CREATE_NO_WINDOW)
    }

    #[cfg(not(windows))]
    fn hide_console_window(&mut self) -> &mut Self {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::HideConsoleWindow;

    /// The helper must be chainable mid-builder and must not break spawning
    /// for `std::process::Command`. (On Windows this exercises the real
    /// `CREATE_NO_WINDOW` path; elsewhere it verifies the no-op.)
    #[test]
    fn hidden_std_command_still_spawns_and_runs() {
        let program = if cfg!(windows) { "cmd" } else { "sh" };
        let flag = if cfg!(windows) { "/C" } else { "-c" };
        let output = std::process::Command::new(program)
            .arg(flag)
            .arg("exit 0")
            .hide_console_window()
            .output()
            .expect("hidden command should spawn");
        assert!(output.status.success());
    }

    /// Same guarantee for `tokio::process::Command` (the type used by the
    /// MCP stdio, sandbox, and provider-CLI spawn paths).
    #[tokio::test]
    async fn hidden_tokio_command_still_spawns_and_runs() {
        let program = if cfg!(windows) { "cmd" } else { "sh" };
        let flag = if cfg!(windows) { "/C" } else { "-c" };
        let output = tokio::process::Command::new(program)
            .arg(flag)
            .arg("exit 0")
            .hide_console_window()
            .output()
            .await
            .expect("hidden command should spawn");
        assert!(output.status.success());
    }
}
