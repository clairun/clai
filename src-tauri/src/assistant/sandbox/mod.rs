//! OS-backed local execution sandboxing.

pub mod filesystem;
pub mod profile;
pub mod runner;
// Only the sandboxed backends consume scratch space; compiling it on a
// platform that runs commands unsandboxed would be dead code and trips
// `cargo clippy -- -D warnings` in CI.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub mod scratch;

#[cfg(target_os = "linux")]
mod linux_bwrap;
#[cfg(any(target_os = "macos", all(test, target_family = "unix")))]
mod macos_seatbelt;
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod unsupported;

pub use profile::{
    SandboxEnv, SandboxNetworkMode, SandboxPathAccess, SandboxProfile, SandboxSessionBusMode,
};
pub use runner::{run_command, SandboxCommand, SandboxCommandOutput};

use serde::Serialize;

/// The OS mechanism that sandboxes shell commands on this PC.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "bindings.ts")]
pub enum SandboxBackend {
    Bubblewrap,
    Seatbelt,
}

/// What the sandbox can enforce on this PC; the settings UI reads it to
/// enable or disable controls instead of naming platforms.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "bindings.ts")]
pub struct SandboxStatus {
    /// Whether shell commands run sandboxed at all. Without a sandbox there
    /// is no network control, web tools included.
    pub available: bool,
    pub backend: Option<SandboxBackend>,
    /// Whether the Allowlist network mode can be enforced for shell commands.
    pub allowlist_available: bool,
    /// Why Allowlist is unavailable, worded for the user.
    pub allowlist_unavailable_reason: Option<String>,
}

pub fn status() -> SandboxStatus {
    let backend = if cfg!(target_os = "linux") {
        Some(SandboxBackend::Bubblewrap)
    } else if cfg!(target_os = "macos") {
        Some(SandboxBackend::Seatbelt)
    } else {
        None
    };
    status_for(backend)
}

pub(crate) fn status_for(backend: Option<SandboxBackend>) -> SandboxStatus {
    let (allowlist_available, allowlist_unavailable_reason) = match backend {
        Some(SandboxBackend::Bubblewrap) => (true, None),
        Some(SandboxBackend::Seatbelt) => (false, Some("Not available on macOS yet")),
        None => (false, Some("Not available on this PC")),
    };
    SandboxStatus {
        available: backend.is_some(),
        backend,
        allowlist_available,
        allowlist_unavailable_reason: allowlist_unavailable_reason.map(str::to_string),
    }
}

/// Persistent per-workspace temp space for the sandboxed backends, reset on
/// the workspace's first sandboxed command of the app session.
///
/// Keyed off the process `$HOME` (falling back to the workspace when it is
/// unset) because scratch lives in the app's own cache directory, not in the
/// policy's view of the user's home. `None` — the workspace container cannot
/// be masked, or the directory could not be created — degrades to the old
/// ephemeral behaviour rather than failing the command; scratch is an
/// optimisation. Platforms that run commands unsandboxed have nothing to bind
/// it into, so they never create a directory that could not be used.
pub(crate) fn session_scratch(workspace_root: &std::path::Path) -> Option<std::path::PathBuf> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        let home = std::env::var_os("HOME")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| workspace_root.to_path_buf());
        scratch::ensure_session_scratch(workspace_root, Some(&home))
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = workspace_root;
        None
    }
}

// Only the sandboxed backends and their tests name this type; Windows runs
// commands unsandboxed, so gate it the same way `macos_seatbelt` is gated.
#[cfg(any(target_os = "macos", all(test, target_family = "unix")))]
pub use profile::SandboxPathGrant;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_per_backend() {
        let linux = status_for(Some(SandboxBackend::Bubblewrap));
        assert!(linux.available && linux.allowlist_available);
        assert_eq!(linux.allowlist_unavailable_reason, None);

        let macos = status_for(Some(SandboxBackend::Seatbelt));
        assert!(macos.available && !macos.allowlist_available);
        assert_eq!(
            macos.allowlist_unavailable_reason.as_deref(),
            Some("Not available on macOS yet")
        );

        let none = status_for(None);
        assert!(!none.available && !none.allowlist_available);
        assert_eq!(none.backend, None);
        assert_eq!(
            none.allowlist_unavailable_reason.as_deref(),
            Some("Not available on this PC")
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn status_on_linux_is_bubblewrap_with_allowlist() {
        assert_eq!(status(), status_for(Some(SandboxBackend::Bubblewrap)));
    }

    #[test]
    fn status_serializes_in_camel_case() {
        let json = serde_json::to_value(status_for(Some(SandboxBackend::Seatbelt))).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "available": true,
                "backend": "seatbelt",
                "allowlistAvailable": false,
                "allowlistUnavailableReason": "Not available on macOS yet",
            })
        );
    }
}
