//! OS-backed local execution sandboxing.

#[cfg(target_os = "linux")]
pub mod bus_proxy;
pub mod filesystem;
#[cfg(target_os = "linux")]
pub mod helper;
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
    keyring_only_unavailable_note, AllowedSocket, ProxyEndpoint, SandboxEnv, SandboxNetworkMode,
    SandboxPathAccess, SandboxProfile, SandboxSessionBusMode, SessionBusProxy, SocketPolicy,
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
    /// Whether shell commands run sandboxed at all. Without a sandbox the
    /// network mode applies to the web tools only.
    pub available: bool,
    pub backend: Option<SandboxBackend>,
    /// Whether the Allowlist network mode can be enforced for shell commands.
    pub allowlist_available: bool,
    /// Why Allowlist is unavailable, worded for the user.
    pub allowlist_unavailable_reason: Option<String>,
    /// Whether shell commands reach only the Unix sockets CLAI binds on
    /// purpose. Without it a socket under a path grant (a desktop input
    /// method, Docker, an ssh or gpg agent) is a way out of the sandbox, and
    /// CLAI only hides the directories where such sockets usually live.
    pub socket_isolation_available: bool,
    /// Why socket isolation is unavailable, worded for the user.
    pub socket_isolation_unavailable_reason: Option<String>,
    /// Whether the keyring-only session bus is filtered here. Without it an
    /// agent set to keyring only gets no bus, never the whole one.
    pub session_bus_filter_available: bool,
    /// Why the session bus cannot be filtered and how to give an agent the
    /// whole bus instead, worded for the user.
    pub session_bus_filter_unavailable_reason: Option<String>,
}

pub fn status() -> SandboxStatus {
    let backend = if cfg!(target_os = "linux") {
        Some(SandboxBackend::Bubblewrap)
    } else if cfg!(target_os = "macos") {
        Some(SandboxBackend::Seatbelt)
    } else {
        None
    };
    with_host_checks(status_for(backend))
}

/// What this PC lets the Linux sandbox confine, beyond its backend.
#[cfg(target_os = "linux")]
fn with_host_checks(status: SandboxStatus) -> SandboxStatus {
    let sockets = socket_policy();
    let bus_filter = bus_proxy::unavailable_reason();
    SandboxStatus {
        socket_isolation_available: sockets.confined(),
        socket_isolation_unavailable_reason: socket_isolation_reason(&sockets),
        session_bus_filter_available: bus_filter.is_none(),
        session_bus_filter_unavailable_reason: bus_filter
            .map(profile::keyring_only_unavailable_note),
        ..status
    }
}

#[cfg(not(target_os = "linux"))]
fn with_host_checks(status: SandboxStatus) -> SandboxStatus {
    status
}

/// The helper and what the kernel lets it confine, looked up once.
#[cfg(target_os = "linux")]
pub(crate) fn socket_policy() -> SocketPolicy {
    static POLICY: std::sync::OnceLock<SocketPolicy> = std::sync::OnceLock::new();
    POLICY
        .get_or_init(|| SocketPolicy {
            helper: helper::locate().ok(),
            kernel_confines: landlock_abi() >= 9,
            allowed: Vec::new(),
        })
        .clone()
}

/// The agent's allowed sockets that are sockets now: `~/` expanded with
/// `home`, and the host path with symlinks resolved (`/var/run` is `/run`).
/// Any other entry is skipped with a log note: a socket that is missing
/// at launch is a stopped service, not a broken config.
#[cfg(target_os = "linux")]
pub(crate) fn resolve_allowed_sockets(
    entries: &[String],
    home: Option<&std::path::Path>,
) -> Vec<AllowedSocket> {
    let mut sockets = Vec::new();
    for entry in entries {
        match host_socket(entry, home) {
            Ok(socket) if !sockets.contains(&socket) => sockets.push(socket),
            Ok(_) => {}
            Err(reason) => tracing::warn!("Allowed socket {entry} skipped: {reason}"),
        }
    }
    sockets
}

#[cfg(target_os = "linux")]
fn host_socket(entry: &str, home: Option<&std::path::Path>) -> Result<AllowedSocket, String> {
    use std::os::unix::fs::FileTypeExt;
    let path = match (entry.strip_prefix("~/"), home) {
        (Some(rest), Some(home)) => home.join(rest),
        (Some(_), None) => return Err("the home directory is unknown".to_string()),
        (None, _) => std::path::PathBuf::from(entry),
    };
    let host = std::fs::canonicalize(&path).map_err(|error| error.to_string())?;
    let meta = host.metadata().map_err(|error| error.to_string())?;
    if !meta.file_type().is_socket() {
        return Err("it is not a socket".to_string());
    }
    Ok(AllowedSocket { path, host })
}

#[cfg(target_os = "linux")]
fn socket_isolation_reason(sockets: &SocketPolicy) -> Option<String> {
    if sockets.helper.is_none() {
        Some("The sandbox helper is not installed".to_string())
    } else if !sockets.kernel_confines {
        Some("This Linux kernel cannot confine Unix sockets (it needs Landlock ABI 9)".to_string())
    } else {
        None
    }
}

/// The kernel's Landlock ABI version, 0 without Landlock. The helper asks
/// the same in the sandbox; under Flatpak this process may be refused the
/// call while the host is not, so it can only under-report.
#[cfg(target_os = "linux")]
fn landlock_abi() -> i64 {
    // SAFETY: with no attribute and size 0 the call only reports the version
    // (LANDLOCK_CREATE_RULESET_VERSION).
    let version = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            std::ptr::null::<u8>(),
            0_usize,
            1_u32,
        )
    };
    version.max(0)
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
        socket_isolation_available: allowlist_available,
        socket_isolation_unavailable_reason: allowlist_unavailable_reason.map(str::to_string),
        session_bus_filter_available: allowlist_available,
        session_bus_filter_unavailable_reason: allowlist_unavailable_reason.map(str::to_string),
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

/// A test that needs a working bwrap (and the helper `cargo test` builds)
/// calls this where they are missing. It skips, unless `CLAI_REQUIRE_BWRAP`
/// is set, as in CI, where a silent skip would hide the test.
#[cfg(all(test, target_os = "linux"))]
pub(crate) fn skip_without_bwrap(reason: &str) {
    assert!(
        std::env::var_os("CLAI_REQUIRE_BWRAP").is_none(),
        "CLAI_REQUIRE_BWRAP is set, but this test cannot run: {reason}"
    );
    eprintln!("skipped: {reason}");
}

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
        let status = status();
        let sockets = socket_policy();
        assert_eq!(
            SandboxStatus {
                socket_isolation_available: true,
                socket_isolation_unavailable_reason: None,
                session_bus_filter_available: true,
                session_bus_filter_unavailable_reason: None,
                ..status.clone()
            },
            status_for(Some(SandboxBackend::Bubblewrap))
        );
        assert_eq!(status.socket_isolation_available, sockets.confined());
        assert_eq!(
            status.socket_isolation_unavailable_reason.is_none(),
            sockets.confined()
        );
        assert_eq!(
            status.session_bus_filter_unavailable_reason,
            bus_proxy::unavailable_reason().map(keyring_only_unavailable_note)
        );
        assert_eq!(
            status.session_bus_filter_available,
            bus_proxy::unavailable_reason().is_none()
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn allowed_sockets_resolve_to_the_host_socket_and_skip_the_rest() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().canonicalize().unwrap();
        let run = home.join("run");
        std::fs::create_dir(&run).unwrap();
        let socket = run.join("docker.sock");
        let _listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        std::os::unix::fs::symlink(&run, home.join("var-run")).unwrap();
        std::fs::write(home.join("file.sock"), "").unwrap();
        let entries = [
            "~/var-run/docker.sock".to_string(),
            socket.to_string_lossy().into_owned(),
            "~/file.sock".to_string(),
            "~/missing.sock".to_string(),
        ];
        assert_eq!(
            resolve_allowed_sockets(&entries, Some(&home)),
            [
                AllowedSocket {
                    path: home.join("var-run/docker.sock"),
                    host: socket.clone(),
                },
                AllowedSocket {
                    path: socket.clone(),
                    host: socket,
                },
            ]
        );
        assert!(resolve_allowed_sockets(&entries[..1], None).is_empty());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn socket_isolation_names_what_is_missing() {
        let helper = Some(std::path::PathBuf::from("/opt/clai/clai-sandbox-helper"));
        let reason = |helper: Option<std::path::PathBuf>, kernel_confines| {
            socket_isolation_reason(&SocketPolicy {
                helper,
                kernel_confines,
                allowed: Vec::new(),
            })
        };
        assert_eq!(reason(helper.clone(), true), None);
        assert!(reason(helper, false).unwrap().contains("Landlock ABI 9"));
        assert!(reason(None, true)
            .unwrap()
            .contains("helper is not installed"));
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
                "socketIsolationAvailable": false,
                "socketIsolationUnavailableReason": "Not available on macOS yet",
                "sessionBusFilterAvailable": false,
                "sessionBusFilterUnavailableReason": "Not available on macOS yet",
            })
        );
    }
}
