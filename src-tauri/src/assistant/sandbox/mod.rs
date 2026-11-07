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
    keyring_only_unavailable_note, ProxyEndpoint, SandboxEnv, SandboxNetworkMode,
    SandboxPathAccess, SandboxProfile, SandboxSessionBusMode, SessionBusProxy, SocketPolicy,
};
// Only the Linux sandbox binds granted sockets.
#[cfg(target_os = "linux")]
pub use profile::AllowedSocket;
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
            skipped: Vec::new(),
            hint_places: socket_hint_places(crate::paths::real_home().as_deref()),
        })
        .clone()
}

/// Where the hint for unreachable sockets may look on the host: the system
/// and user runtime directories and the usual socket directories under
/// home. Not any path a command prints, which would let it learn what is
/// on the host outside its grants.
#[cfg(target_os = "linux")]
fn socket_hint_places(home: Option<&std::path::Path>) -> Vec<std::path::PathBuf> {
    let runtime = ["/run", "/var/run"].map(std::path::PathBuf::from);
    let home_dirs = home
        .into_iter()
        .flat_map(|home| linux_bwrap::SOCKET_DIRS.iter().map(|dir| home.join(dir)));
    runtime.into_iter().chain(home_dirs).collect()
}

/// The path grants that name a Unix socket now: the sockets a command may
/// connect to. Only a grant on the socket itself counts, read-only as much
/// as read-write, since connecting ignores a read-only mount; a socket under
/// a granted directory stays unreachable. Each is bound at the path the
/// grant names and at its host path, with symlinks resolved (`/var/run` is
/// `/run`). A grant refused because its path passes through a directory the
/// sandbox can write (`writable`) is returned with the reason, for the
/// command's result.
#[cfg(target_os = "linux")]
pub(crate) fn granted_sockets(
    grants: &[std::path::PathBuf],
    writable: &dyn Fn(&std::path::Path) -> bool,
) -> (Vec<AllowedSocket>, Vec<String>) {
    use std::os::unix::fs::FileTypeExt;
    let (mut sockets, mut refused) = (Vec::new(), Vec::new());
    for path in grants {
        if !std::fs::metadata(path).is_ok_and(|meta| meta.file_type().is_socket()) {
            continue;
        }
        match resolve_unwritable(path, writable) {
            Ok(host) => {
                let socket = AllowedSocket {
                    path: path.clone(),
                    host,
                };
                if !sockets.contains(&socket) {
                    sockets.push(socket);
                }
            }
            // Gone since it was looked at: a service that stopped.
            Err(SocketSkip::Absent) => {}
            Err(SocketSkip::Refused(reason)) => {
                let refusal = format!("{}: {reason}", path.display());
                if !refused.contains(&refusal) {
                    tracing::warn!("Socket grant refused: {refusal}");
                    refused.push(refusal);
                }
            }
        }
    }
    (sockets, refused)
}

/// Why a socket grant is left out of a command.
#[cfg(target_os = "linux")]
enum SocketSkip {
    /// Not there now.
    Absent,
    /// Not safe to hand over, whatever is there.
    Refused(String),
}

/// The host path `path` names, resolved as `canonicalize` does, but refused
/// when a name on the way, a symlink's target included, lies in a directory
/// the sandbox can write: a command could put a link there to the session
/// bus or the systemd manager, and the next command would be handed that.
/// A granted name in a directory the sandbox cannot write is a mount point
/// in the sandbox, so a command cannot remove it or rename over it (EBUSY),
/// even with a read-write grant on the socket itself.
#[cfg(target_os = "linux")]
fn resolve_unwritable(
    path: &std::path::Path,
    writable: &dyn Fn(&std::path::Path) -> bool,
) -> Result<std::path::PathBuf, SocketSkip> {
    use std::ffi::OsString;
    use std::path::{Component, Path, PathBuf};
    const MAX_LINKS: usize = 40;
    fn push_reversed(pending: &mut Vec<OsString>, path: &Path) {
        let parts = path.components().filter_map(|part| match part {
            Component::Normal(name) => Some(name.to_os_string()),
            Component::ParentDir => Some(OsString::from("..")),
            Component::RootDir | Component::CurDir | Component::Prefix(_) => None,
        });
        let parts: Vec<OsString> = parts.collect();
        pending.extend(parts.into_iter().rev());
    }
    if !path.is_absolute() {
        return Err(SocketSkip::Refused(
            "it is not an absolute path".to_string(),
        ));
    }
    let mut resolved = PathBuf::from("/");
    let mut pending = Vec::new();
    push_reversed(&mut pending, path);
    let mut links = 0;
    while let Some(part) = pending.pop() {
        if part == ".." {
            resolved.pop();
            continue;
        }
        let next = resolved.join(&part);
        if writable(&resolved) {
            return Err(SocketSkip::Refused(format!(
                "sandboxed commands can write at {}, so a command could swap {} for a link \
                 to another socket",
                resolved.display(),
                next.display()
            )));
        }
        let absent = |_: std::io::Error| SocketSkip::Absent;
        let metadata = std::fs::symlink_metadata(&next).map_err(absent)?;
        if !metadata.file_type().is_symlink() {
            resolved = next;
            continue;
        }
        links += 1;
        if links > MAX_LINKS {
            return Err(SocketSkip::Absent);
        }
        let target = std::fs::read_link(&next).map_err(absent)?;
        if target.is_absolute() {
            resolved = PathBuf::from("/");
        }
        push_reversed(&mut pending, &target);
    }
    Ok(resolved)
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

/// A test that needs Landlock ABI `needed` calls this, and returns when it
/// says to skip on an older kernel. It never skips when
/// `CLAI_REQUIRE_LANDLOCK_ABI` is `needed` or more: the test fails instead.
#[cfg(all(test, target_os = "linux"))]
pub(crate) fn skip_below_landlock(needed: i64) -> bool {
    let abi = landlock_abi();
    if abi >= needed {
        return false;
    }
    let required = std::env::var("CLAI_REQUIRE_LANDLOCK_ABI")
        .ok()
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(0);
    assert!(
        required < needed,
        "CLAI_REQUIRE_LANDLOCK_ABI is {required}, but this kernel has Landlock ABI {abi}"
    );
    eprintln!("skipped: this test needs Landlock ABI {needed}; the kernel has {abi}");
    true
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
    fn only_grants_on_a_socket_count_resolved_to_the_host_socket() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().canonicalize().unwrap();
        let run = home.join("run");
        std::fs::create_dir(&run).unwrap();
        let socket = run.join("docker.sock");
        let _listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        std::os::unix::fs::symlink(&run, home.join("var-run")).unwrap();
        std::fs::write(home.join("file.sock"), "").unwrap();
        let grants = [
            home.join("var-run/docker.sock"),
            socket.clone(),
            socket.clone(),
            home.join("file.sock"),
            home.join("missing.sock"),
            // A directory grant: the socket in it does not count.
            run.clone(),
        ];
        let nowhere = |_: &std::path::Path| false;
        assert_eq!(
            granted_sockets(&grants, &nowhere),
            (
                vec![
                    AllowedSocket {
                        path: home.join("var-run/docker.sock"),
                        host: socket.clone(),
                    },
                    AllowedSocket {
                        path: socket.clone(),
                        host: socket,
                    },
                ],
                Vec::<String>::new()
            )
        );
    }

    /// M2: a command can write in the workspace, so a socket named there,
    /// or reached through a link there, is refused: the command could make
    /// it a link to the session bus or the systemd manager.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_socket_grant_reached_through_a_writable_directory_is_refused() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let (workspace, outside) = (root.join("ws"), root.join("outside"));
        std::fs::create_dir(&workspace).unwrap();
        std::fs::create_dir(&outside).unwrap();
        // The host socket a planted link would lead to.
        let bus = outside.join("bus");
        let _listener = std::os::unix::net::UnixListener::bind(&bus).unwrap();
        // In the workspace: a link where a dev server socket was.
        std::os::unix::fs::symlink(&bus, workspace.join("dev.sock")).unwrap();
        // Outside: a link that leads into the workspace, then to the bus.
        std::os::unix::fs::symlink(workspace.join("dev.sock"), outside.join("via.sock")).unwrap();
        // Outside: a directory link, as /var/run, straight to the socket.
        std::os::unix::fs::symlink(&outside, root.join("run")).unwrap();
        let writable = |path: &std::path::Path| path.starts_with(&workspace);
        let grants = [
            workspace.join("dev.sock"),
            outside.join("via.sock"),
            root.join("run/bus"),
        ];
        let (allowed, refused) = granted_sockets(&grants, &writable);
        assert_eq!(
            allowed,
            [AllowedSocket {
                path: root.join("run/bus"),
                host: bus.clone(),
            }]
        );
        assert_eq!(refused.len(), 2, "{refused:?}");
        for (grant, refusal) in grants.iter().zip(&refused) {
            assert!(refusal.starts_with(&*grant.to_string_lossy()), "{refusal}");
            assert!(refusal.contains("can write at"), "{refusal}");
        }

        // Without the writable check both links would hand over the bus.
        let (allowed, _) = granted_sockets(&grants[..2], &|_| false);
        assert!(allowed.iter().all(|socket| socket.host == bus));
        assert_eq!(allowed.len(), 2);
    }

    /// The narrowed M2: only a writable directory on the way makes a
    /// redirect possible. A read-write grant on the socket itself does not,
    /// as the granted name is a mount point in the sandbox.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_writable_socket_counts_and_a_writable_directory_on_the_way_does_not() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let dir = root.join("real");
        std::fs::create_dir(&dir).unwrap();
        let socket = dir.join("docker.sock");
        let _listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        std::os::unix::fs::symlink(&dir, root.join("link")).unwrap();
        let only = |writable: &std::path::Path| {
            let writable = writable.to_path_buf();
            move |path: &std::path::Path| path == writable
        };

        let (allowed, refused) = granted_sockets(std::slice::from_ref(&socket), &only(&socket));
        assert_eq!(allowed.len(), 1, "{refused:?}");

        let (allowed, refused) = granted_sockets(std::slice::from_ref(&socket), &only(&dir));
        assert!(allowed.is_empty());
        assert!(
            refused[0].contains(&format!("can write at {}", dir.display())),
            "{refused:?}"
        );
        // The directory that holds a symlink on the way counts too.
        let (allowed, refused) = granted_sockets(&[root.join("link/docker.sock")], &only(&root));
        assert!(allowed.is_empty());
        assert_eq!(refused.len(), 1);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_socket_hint_looks_only_where_sockets_usually_live() {
        use std::path::PathBuf;
        let places = socket_hint_places(Some(std::path::Path::new("/home/u")));
        for place in ["/run", "/var/run", "/home/u/.docker/run", "/home/u/.gnupg"] {
            assert!(
                places.contains(&PathBuf::from(place)),
                "{place}: {places:?}"
            );
        }
        assert!(!places.contains(&PathBuf::from("/home/u")));
        assert_eq!(
            socket_hint_places(None),
            [PathBuf::from("/run"), PathBuf::from("/var/run")]
        );
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
                skipped: Vec::new(),
                hint_places: Vec::new(),
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
