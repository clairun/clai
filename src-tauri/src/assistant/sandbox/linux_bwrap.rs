use std::ffi::{OsStr, OsString};
use std::path::{Component, Path, PathBuf};

use tokio::process::Command;

use super::runner::{prepare_stdio, run_spawned_child};
use super::{
    SandboxCommand, SandboxCommandOutput, SandboxNetworkMode, SandboxPathAccess,
    SandboxSessionBusMode,
};

const BWRAP_BIN: &str = "bwrap";
const FLATPAK_SPAWN_BIN: &str = "flatpak-spawn";

/// Builds the program + argv to launch the sandbox.
///
/// On a normal host we exec `bwrap` directly. Inside Flatpak, however,
/// the app itself already runs in a bubblewrap sandbox whose seccomp
/// filter blocks creating *nested* user namespaces — a bwrap launched
/// from here would fail with "creating new namespace failed" (and the
/// runtime doesn't even ship `bwrap`). So inside Flatpak we run the same
/// bwrap invocation ON THE HOST via `flatpak-spawn --host bwrap …`: the
/// host's bwrap can create namespaces, so the sandbox profile and its
/// security boundary are preserved unchanged. This path requires the
/// Flatpak to hold the `org.freedesktop.Flatpak` talk permission.
fn launch_argv(bwrap_args: Vec<OsString>, in_flatpak: bool) -> (&'static str, Vec<OsString>) {
    if in_flatpak {
        let mut args = Vec::with_capacity(bwrap_args.len() + 2);
        args.push(os("--host"));
        args.push(os(BWRAP_BIN));
        args.extend(bwrap_args);
        (FLATPAK_SPAWN_BIN, args)
    } else {
        (BWRAP_BIN, bwrap_args)
    }
}

pub async fn run(command: SandboxCommand) -> Result<SandboxCommandOutput, String> {
    let args = bwrap_args(&command)?;
    let in_flatpak = crate::providers::is_flatpak();
    let (program, launch_args) = launch_argv(args, in_flatpak);
    let mut child_command = Command::new(program);
    child_command.args(launch_args);
    prepare_stdio(&mut child_command);

    let child = child_command.spawn().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            if in_flatpak {
                "Sandboxed shell is unavailable: `flatpak-spawn` not found — the Flatpak needs the `org.freedesktop.Flatpak` talk permission to run the sandbox on the host.".to_string()
            } else {
                "Sandboxed shell is unavailable: bubblewrap (`bwrap`) is not installed or not on PATH".to_string()
            }
        } else {
            format!("Failed to start sandboxed shell: {}", e)
        }
    })?;

    let output = run_spawned_child(
        child,
        command.cwd,
        command.timeout_ms,
        command.max_output_chars,
        "Sandboxed shell command",
    )
    .await?;

    if looks_like_bwrap_setup_failure(&output) {
        return Err(classify_bwrap_failure(&output.stderr));
    }

    Ok(output)
}

// Bwrap's own diagnostics — emitted only when bwrap itself fails before it can
// exec the inner command — are recognised via three signals together:
//   - exit code non-zero
//   - stdout empty (inner command never produced any output, because it never
//     ran)
//   - some line of stderr begins with the literal `bwrap:` prefix that bwrap
//     uses for every error message it emits via die_with_error()
//
// Requiring all three avoids two failure modes the older single-prefix check
// had: (a) a sandbox setup failure where bwrap's error was preceded on stderr
// by output from another process in the pipeline would slip past the
// trim_start check; (b) an inner command that legitimately printed
// `bwrap: ...` to its own stderr and exited non-zero would be misclassified
// as a sandbox failure.
fn looks_like_bwrap_setup_failure(output: &SandboxCommandOutput) -> bool {
    if output.success {
        return false;
    }
    if !output.stdout.is_empty() {
        return false;
    }
    output
        .stderr
        .lines()
        .any(|line| line.trim_start().starts_with("bwrap:"))
}

pub(crate) fn bwrap_args(command: &SandboxCommand) -> Result<Vec<OsString>, String> {
    validate_profile_paths(command)?;

    let mut args = vec![
        os("--unshare-user"),
        os("--unshare-ipc"),
        os("--unshare-pid"),
        os("--unshare-uts"),
        os("--unshare-cgroup-try"),
    ];

    match command.profile.network {
        SandboxNetworkMode::Host => args.push(os("--share-net")),
        SandboxNetworkMode::Disabled => args.push(os("--unshare-net")),
    }

    args.extend([
        os("--die-with-parent"),
        os("--new-session"),
        os("--clearenv"),
        os("--proc"),
        os("/proc"),
        os("--dev"),
        os("/dev"),
        os("--tmpfs"),
        os("/tmp"),
        os("--ro-bind"),
        os("/usr"),
        os("/usr"),
        os("--ro-bind-try"),
        os("/bin"),
        os("/bin"),
        os("--ro-bind-try"),
        os("/sbin"),
        os("/sbin"),
        os("--ro-bind-try"),
        os("/lib"),
        os("/lib"),
        os("--ro-bind-try"),
        os("/lib32"),
        os("/lib32"),
        os("--ro-bind-try"),
        os("/lib64"),
        os("/lib64"),
        os("--ro-bind-try"),
        os("/libx32"),
        os("/libx32"),
        os("--ro-bind"),
        os("/etc"),
        os("/etc"),
        // Overlay an empty tmpfs at /etc/ssh on top of the /etc bind.
        // Rationale: with --unshare-user the sandbox's user namespace can
        // only map the caller's UID; every other host UID, including root,
        // appears as `nobody` (65534) inside. OpenSSH then refuses every
        // config file under /etc/ssh/ssh_config.d/ with "Bad owner or
        // permissions" because it expects ownership by root or the caller.
        // Hiding /etc/ssh removes those files from view entirely so ssh
        // falls back to its built-in defaults and the user's
        // ~/.ssh/config (which IS owned by the caller's UID via the
        // workspace/grant binds). No legitimate workflow depends on
        // /etc/ssh inside the sandbox.
        os("--tmpfs"),
        os("/etc/ssh"),
        os("--ro-bind-try"),
        os("/sys"),
        os("/sys"),
    ]);

    append_runtime_file_binds(&mut args);
    append_session_bus_bind(&mut args, command);
    append_workspace_and_grants(&mut args, command);

    for (key, value) in command.profile.env.iter() {
        args.push(os("--setenv"));
        args.push(os(key));
        args.push(os(value));
    }

    args.push(os("--chdir"));
    args.push(command.cwd.as_os_str().to_os_string());
    args.push(os("--"));
    args.extend(command.argv.iter().cloned());

    Ok(args)
}

fn validate_profile_paths(command: &SandboxCommand) -> Result<(), String> {
    if !command.profile.workspace_root.exists() {
        return Err(format!(
            "Sandbox workspace does not exist: {}",
            command.profile.workspace_root.display()
        ));
    }

    for grant in &command.profile.path_grants {
        if !grant.host_path.exists() {
            return Err(format!(
                "Sandbox path grant does not exist: {}",
                grant.host_path.display()
            ));
        }
    }

    Ok(())
}

fn append_runtime_file_binds(args: &mut Vec<OsString>) {
    for bind in runtime_file_binds() {
        for dir in private_parent_dirs_for(&bind.destination) {
            args.push(os("--dir"));
            args.push(dir.into_os_string());
        }
        args.push(os("--ro-bind"));
        args.push(bind.source.into_os_string());
        args.push(bind.destination.into_os_string());
    }
}

/// Bind the user's D-Bus session bus socket into the sandbox when the
/// profile asks for it. Required for libsecret-based auth (gh, secret-tool,
/// git-credential-libsecret) to reach the host's Secret Service
/// implementation (gnome-keyring-daemon, KDE Wallet, etc.).
///
/// Resolution order — authoritative first, conventional fallback second:
///
/// 1. `DBUS_SESSION_BUS_ADDRESS` on the host. If it points at a
///    `unix:path=<file>` socket, bind that file at the same path inside
///    the sandbox.
/// 2. If the address is `unix:abstract=<name>`, no filesystem bind is
///    possible (abstract sockets live in the kernel-managed abstract
///    namespace). They are network-namespace-scoped, so they remain
///    reachable as long as we use `--share-net` (the default). We log
///    that case and continue.
/// 3. As a fallback for cases where the env var is unset, try the
///    modern systemd convention `$XDG_RUNTIME_DIR/bus`.
///
/// If none of those resolve, we log a warning and skip the bind; the
/// agent's libsecret-using tools will then fail with the same "no bus"
/// error they would have without the toggle, and can escalate via
/// `workspace_requestUserInput`. This makes the toggle meaningful only
/// where a session bus actually exists — desktop Linux — and a graceful
/// no-op everywhere else (headless servers, containers, WSL).
fn append_session_bus_bind(args: &mut Vec<OsString>, command: &SandboxCommand) {
    if !matches!(command.profile.session_bus, SandboxSessionBusMode::Allow) {
        return;
    }
    let bus_path = resolve_session_bus_socket();
    let Some(bus_path) = bus_path else {
        tracing::warn!(
            "Sandbox session_bus is Allow but no path-based D-Bus session bus socket \
             was found on the host (neither parsed from DBUS_SESSION_BUS_ADDRESS nor \
             at $XDG_RUNTIME_DIR/bus). libsecret-using tools will fail; the toggle is \
             a no-op here."
        );
        return;
    };
    for dir in private_parent_dirs_for(&bus_path) {
        args.push(os("--dir"));
        args.push(dir.into_os_string());
    }
    args.push(os("--ro-bind"));
    args.push(bus_path.clone().into_os_string());
    args.push(bus_path.into_os_string());
}

/// Locate the host's D-Bus session bus socket on the filesystem. Returns
/// None if the bus is abstract-socket-only (no file to bind) or simply
/// absent on this host.
fn resolve_session_bus_socket() -> Option<PathBuf> {
    if let Some(addr) = std::env::var_os("DBUS_SESSION_BUS_ADDRESS") {
        let addr = addr.to_string_lossy();
        // Address format: `transport:key=value,key=value;transport:...`.
        // Each address can be tried in order; we take the first
        // `unix:path=...` entry. `unix:abstract=...` exists too but
        // can't be bind-mounted; abstract sockets reach the sandbox via
        // the shared network namespace already, so no action needed.
        for component in addr.split(';') {
            let component = component.trim();
            if let Some(rest) = component.strip_prefix("unix:") {
                for kv in rest.split(',') {
                    if let Some(path) = kv.trim().strip_prefix("path=") {
                        let path = PathBuf::from(path);
                        if path.exists() {
                            return Some(path);
                        }
                    }
                }
            }
        }
    }
    // Fallback for environments where DBUS_SESSION_BUS_ADDRESS isn't set
    // but the systemd-managed bus still exists at the conventional path.
    if let Some(runtime_dir) = std::env::var_os("XDG_RUNTIME_DIR") {
        let candidate = PathBuf::from(runtime_dir).join("bus");
        if candidate.exists() {
            return Some(candidate);
        }
    }
    None
}

fn append_workspace_and_grants(args: &mut Vec<OsString>, command: &SandboxCommand) {
    // Bind-mounts have last-writer-wins semantics over their subtree: a later
    // shallower bind overlays earlier deeper binds at any nested path. To make
    // the workspace's read-write access survive even when a configured grant is
    // an ancestor of the workspace (e.g. workspace under /home/me with a
    // separate /home/me read-only grant), merge workspace + grants and emit
    // them shallowest-first. The workspace, being deeper than its ancestor
    // grant, ends up bound last and its RW wins.
    //
    // Sort is stable: when two paths have equal depth (siblings), they don't
    // overlap and emit order is irrelevant. We push the workspace first so an
    // exact-duplicate grant gets dropped by the dedup below and the workspace's
    // RW access wins.
    let mut binds: Vec<(PathBuf, SandboxPathAccess)> =
        Vec::with_capacity(command.profile.path_grants.len() + 1);
    binds.push((
        command.profile.workspace_root.clone(),
        SandboxPathAccess::ReadWrite,
    ));
    for grant in &command.profile.path_grants {
        if grant.host_path == command.profile.workspace_root {
            continue;
        }
        binds.push((grant.host_path.clone(), grant.access));
    }

    binds.sort_by_key(|(path, _)| path_depth(path));

    for (path, access) in binds {
        append_bind(args, access, &path, &path);
    }
}

fn append_bind(args: &mut Vec<OsString>, access: SandboxPathAccess, source: &Path, dest: &Path) {
    match access {
        SandboxPathAccess::ReadOnly => args.push(os("--ro-bind")),
        SandboxPathAccess::ReadWrite => args.push(os("--bind")),
    }
    args.push(source.as_os_str().to_os_string());
    args.push(dest.as_os_str().to_os_string());
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RuntimeFileBind {
    source: PathBuf,
    destination: PathBuf,
}

fn runtime_file_binds() -> Vec<RuntimeFileBind> {
    [Path::new("/etc/resolv.conf"), Path::new("/etc/localtime")]
        .into_iter()
        .filter_map(resolve_runtime_symlink_bind)
        .filter(|bind| !path_is_covered_by_system_bind(&bind.destination))
        .collect()
}

fn resolve_runtime_symlink_bind(path: &Path) -> Option<RuntimeFileBind> {
    let metadata = std::fs::symlink_metadata(path).ok()?;
    if !metadata.file_type().is_symlink() {
        return None;
    }

    let target = std::fs::read_link(path).ok()?;
    let destination = if target.is_absolute() {
        normalize_path(target)
    } else {
        normalize_path(path.parent()?.join(target))
    };
    let source = std::fs::canonicalize(&destination).ok()?;
    if !std::fs::metadata(&source).ok()?.is_file() {
        return None;
    }

    Some(RuntimeFileBind {
        source,
        destination,
    })
}

fn private_parent_dirs_for(path: &Path) -> Vec<PathBuf> {
    let Some(parent) = path.parent() else {
        return Vec::new();
    };

    let mut dirs = Vec::new();
    let mut current = PathBuf::new();
    for component in parent.components() {
        match component {
            Component::RootDir => current.push(Path::new("/")),
            Component::Normal(part) => {
                current.push(part);
                dirs.push(current.clone());
            }
            Component::CurDir | Component::ParentDir | Component::Prefix(_) => {}
        }
    }
    dirs
}

fn path_is_covered_by_system_bind(path: &Path) -> bool {
    [
        "/usr", "/bin", "/sbin", "/lib", "/lib32", "/lib64", "/libx32", "/etc", "/sys",
    ]
    .iter()
    .map(Path::new)
    .any(|root| path == root || path.starts_with(root))
}

fn path_depth(path: &Path) -> usize {
    path.components()
        .filter(|component| matches!(component, Component::Normal(_)))
        .count()
}

fn normalize_path(path: PathBuf) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(Path::new("/")),
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(part) => normalized.push(part),
        }
    }
    normalized
}

fn classify_bwrap_failure(stderr: &str) -> String {
    let lower = stderr.to_ascii_lowercase();
    if lower.contains("operation not permitted")
        || lower.contains("no permissions")
        || lower.contains("creating new namespace failed")
        || lower.contains("user namespace")
    {
        format!(
            "Sandboxed shell is unavailable: bubblewrap could not create the required Linux namespaces. Enable unprivileged user namespaces for this host. Details: {}",
            stderr.trim()
        )
    } else {
        format!("Sandboxed shell failed to start: {}", stderr.trim())
    }
}

fn os(value: impl AsRef<OsStr>) -> OsString {
    value.as_ref().to_os_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assistant::sandbox::{SandboxEnv, SandboxPathGrant, SandboxProfile};

    fn sample_command() -> SandboxCommand {
        let workspace = std::env::temp_dir();
        SandboxCommand {
            argv: vec![os("/bin/sh"), os("-lc"), os("pwd")],
            cwd: workspace.clone(),
            timeout_ms: 1_000,
            max_output_chars: 1_000,
            profile: SandboxProfile {
                workspace_root: workspace.clone(),
                path_grants: vec![],
                network: SandboxNetworkMode::Host,
                session_bus: SandboxSessionBusMode::Deny,
                env: SandboxEnv::filtered_from_iter(
                    [("PATH", "/usr/bin:/bin")],
                    &workspace,
                    SandboxSessionBusMode::Deny,
                ),
            },
        }
    }

    #[test]
    fn launch_argv_runs_bwrap_directly_on_host() {
        let bwrap_args = vec![os("--die-with-parent"), os("--"), os("/bin/sh")];
        let (program, args) = launch_argv(bwrap_args.clone(), false);
        assert_eq!(program, "bwrap");
        assert_eq!(args, bwrap_args);
    }

    #[test]
    fn launch_argv_wraps_with_flatpak_spawn_in_flatpak() {
        let bwrap_args = vec![os("--die-with-parent"), os("--"), os("/bin/sh")];
        let (program, args) = launch_argv(bwrap_args.clone(), true);
        assert_eq!(program, "flatpak-spawn");
        // flatpak-spawn --host bwrap <original bwrap args...>
        let rendered: Vec<String> = args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(rendered[0], "--host");
        assert_eq!(rendered[1], "bwrap");
        assert_eq!(&rendered[2..], &["--die-with-parent", "--", "/bin/sh"]);
    }

    #[test]
    fn bwrap_args_do_not_bind_run_wholesale() {
        let args = bwrap_args(&sample_command()).unwrap();
        let rendered: Vec<String> = args
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();

        for window in rendered.windows(3) {
            assert_ne!(window, ["--ro-bind", "/run", "/run"]);
            assert_ne!(window, ["--bind", "/run", "/run"]);
        }
    }

    #[test]
    fn etc_ssh_is_overlaid_with_tmpfs_after_etc_bind() {
        // Defends against the OpenSSH "Bad owner or permissions" failure
        // mode: --unshare-user maps host root to nobody inside the namespace,
        // and ssh refuses /etc/ssh/ssh_config.d/* on that basis. The fix is
        // to overlay an empty tmpfs at /etc/ssh, which must come AFTER the
        // /etc ro-bind so it actually overrides.
        let args = bwrap_args(&sample_command()).unwrap();
        let rendered: Vec<String> = args
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();

        let etc_bind_idx = rendered
            .windows(3)
            .position(|w| w == ["--ro-bind", "/etc", "/etc"])
            .expect("/etc should be ro-bound");
        let etc_ssh_tmpfs_idx = rendered
            .windows(2)
            .position(|w| w == ["--tmpfs", "/etc/ssh"])
            .expect("/etc/ssh should be overlaid with a tmpfs");
        assert!(
            etc_ssh_tmpfs_idx > etc_bind_idx,
            "--tmpfs /etc/ssh must come after --ro-bind /etc /etc so it overrides; rendered: {rendered:?}"
        );
    }

    // Process env is shared across the test runner's thread pool, so any
    // test that mutates DBUS_SESSION_BUS_ADDRESS / XDG_RUNTIME_DIR for the
    // duration of its body would otherwise race with parallel tests that
    // read the same vars (this includes resolve_session_bus_socket's
    // env-var reads inside bwrap_args). Serialize them on this mutex so
    // each env-mutating test sees a stable snapshot for its full body.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    // Helper: set DBUS_SESSION_BUS_ADDRESS + XDG_RUNTIME_DIR around a
    // closure, restoring previous values regardless of panic.
    fn with_dbus_env<F: FnOnce() -> R, R>(
        bus_address: Option<&str>,
        runtime_dir: Option<&std::path::Path>,
        body: F,
    ) -> R {
        // Poison recovery: if a previous test panicked while holding the
        // lock, we still want to run — the env may be slightly weird but
        // we'll re-overwrite it below anyway.
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev_addr = std::env::var_os("DBUS_SESSION_BUS_ADDRESS");
        let prev_runtime = std::env::var_os("XDG_RUNTIME_DIR");
        unsafe {
            match bus_address {
                Some(v) => std::env::set_var("DBUS_SESSION_BUS_ADDRESS", v),
                None => std::env::remove_var("DBUS_SESSION_BUS_ADDRESS"),
            }
            match runtime_dir {
                Some(v) => std::env::set_var("XDG_RUNTIME_DIR", v),
                None => std::env::remove_var("XDG_RUNTIME_DIR"),
            }
        }
        let result = body();
        unsafe {
            match prev_addr {
                Some(v) => std::env::set_var("DBUS_SESSION_BUS_ADDRESS", v),
                None => std::env::remove_var("DBUS_SESSION_BUS_ADDRESS"),
            }
            match prev_runtime {
                Some(v) => std::env::set_var("XDG_RUNTIME_DIR", v),
                None => std::env::remove_var("XDG_RUNTIME_DIR"),
            }
        }
        result
    }

    #[test]
    fn session_bus_resolves_unix_path_from_dbus_address_first() {
        // Authoritative source: parse DBUS_SESSION_BUS_ADDRESS even if it
        // points outside the conventional XDG_RUNTIME_DIR location. This
        // catches custom D-Bus setups and older distros that don't follow
        // the modern systemd convention.
        let temp = tempfile::tempdir().unwrap();
        let custom_socket = temp.path().join("custom-bus");
        std::fs::write(&custom_socket, "").unwrap();
        // Set XDG_RUNTIME_DIR to a different empty dir so the fallback
        // path doesn't exist — proves we used the addr, not the fallback.
        let other_runtime = tempfile::tempdir().unwrap();

        let resolved = with_dbus_env(
            Some(&format!(
                "unix:path={},guid=abc123",
                custom_socket.display()
            )),
            Some(other_runtime.path()),
            resolve_session_bus_socket,
        );
        assert_eq!(resolved.as_deref(), Some(custom_socket.as_path()));
    }

    #[test]
    fn session_bus_falls_back_to_xdg_runtime_dir_when_address_unset() {
        let runtime = tempfile::tempdir().unwrap();
        let bus_path = runtime.path().join("bus");
        std::fs::write(&bus_path, "").unwrap();

        let resolved = with_dbus_env(None, Some(runtime.path()), resolve_session_bus_socket);
        assert_eq!(resolved.as_deref(), Some(bus_path.as_path()));
    }

    #[test]
    fn session_bus_returns_none_for_abstract_socket_address() {
        // unix:abstract=... has no filesystem path to bind. Reachable via
        // shared net namespace (the default), so the bus still works
        // without any bind — but resolve_session_bus_socket reports None
        // and we skip the bind step.
        let resolved = with_dbus_env(
            Some("unix:abstract=/tmp/dbus-XYZ123,guid=abc"),
            None,
            resolve_session_bus_socket,
        );
        assert!(resolved.is_none());
    }

    #[test]
    fn session_bus_returns_none_when_nothing_is_set() {
        // Headless / containerized / pre-session contexts: no bus exists.
        // The toggle becomes a no-op, no panic.
        let resolved = with_dbus_env(None, None, resolve_session_bus_socket);
        assert!(resolved.is_none());
    }

    #[test]
    fn session_bus_tries_multiple_components_in_compound_address() {
        // Per the D-Bus spec, DBUS_SESSION_BUS_ADDRESS may carry multiple
        // semicolon-separated addresses. We should accept the first
        // unix:path= that points at an existing file.
        let temp = tempfile::tempdir().unwrap();
        let real_socket = temp.path().join("real-bus");
        std::fs::write(&real_socket, "").unwrap();

        let resolved = with_dbus_env(
            Some(&format!(
                "unix:abstract=does-not-exist;unix:path={}",
                real_socket.display()
            )),
            None,
            resolve_session_bus_socket,
        );
        assert_eq!(resolved.as_deref(), Some(real_socket.as_path()));
    }

    #[test]
    fn session_bus_allow_emits_bus_socket_ro_bind_at_resolved_path() {
        let temp = tempfile::tempdir().unwrap();
        let bus_path = temp.path().join("bus");
        std::fs::write(&bus_path, "").unwrap();

        let mut command = sample_command();
        command.profile.session_bus = SandboxSessionBusMode::Allow;

        let args = with_dbus_env(
            Some(&format!("unix:path={}", bus_path.display())),
            None,
            || bwrap_args(&command).unwrap(),
        );

        let rendered: Vec<String> = args
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        let bus_str = bus_path.to_string_lossy().into_owned();
        let triple_idx = rendered
            .windows(3)
            .position(|w| w[0] == "--ro-bind" && w[1] == bus_str && w[2] == bus_str);
        assert!(
            triple_idx.is_some(),
            "session_bus Allow should emit --ro-bind <path> <path> at the resolved socket; rendered: {rendered:?}"
        );
    }

    #[test]
    fn session_bus_deny_does_not_bind_bus_socket() {
        let temp = tempfile::tempdir().unwrap();
        let bus_path = temp.path().join("bus");
        std::fs::write(&bus_path, "").unwrap();

        // sample_command()'s session_bus is Deny.
        let args = with_dbus_env(
            Some(&format!("unix:path={}", bus_path.display())),
            None,
            || bwrap_args(&sample_command()).unwrap(),
        );
        let rendered: Vec<String> = args
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        let bus_str = bus_path.to_string_lossy().into_owned();
        assert!(
            !rendered.iter().any(|arg| arg == &bus_str),
            "session_bus Deny should never include the bus path in args; rendered: {rendered:?}"
        );
    }

    #[test]
    fn disabled_network_uses_unshare_net_instead_of_share_net() {
        let mut command = sample_command();
        command.profile.network = SandboxNetworkMode::Disabled;
        let args = bwrap_args(&command).unwrap();
        let rendered: Vec<String> = args
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();

        assert!(rendered.contains(&"--unshare-net".to_string()));
        assert!(!rendered.contains(&"--share-net".to_string()));
    }

    #[test]
    fn private_parent_dirs_excludes_root_and_includes_nested_dirs() {
        assert_eq!(
            private_parent_dirs_for(Path::new("/run/systemd/resolve/stub-resolv.conf")),
            vec![
                PathBuf::from("/run"),
                PathBuf::from("/run/systemd"),
                PathBuf::from("/run/systemd/resolve"),
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn resolves_relative_runtime_symlink_destination_without_binding_parent_dir() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let etc_dir = temp.path().join("etc");
        let run_dir = temp.path().join("run/systemd/resolve");
        std::fs::create_dir_all(&etc_dir).unwrap();
        std::fs::create_dir_all(&run_dir).unwrap();
        let target = run_dir.join("stub-resolv.conf");
        std::fs::write(&target, "nameserver 127.0.0.53\n").unwrap();
        let link = etc_dir.join("resolv.conf");
        symlink("../run/systemd/resolve/stub-resolv.conf", &link).unwrap();

        let bind = resolve_runtime_symlink_bind(&link).unwrap();

        assert_eq!(bind.source, target);
        assert_eq!(
            bind.destination,
            temp.path().join("run/systemd/resolve/stub-resolv.conf")
        );
    }

    #[tokio::test]
    async fn run_executes_inside_workspace_and_hides_ungranted_tmp_path() {
        let workspace = tempfile::tempdir().unwrap();
        let secret_dir = tempfile::tempdir().unwrap();
        let secret = secret_dir.path().join("secret.txt");
        std::fs::write(&secret, "secret").unwrap();
        let command_text = format!(
            "pwd; if [ -e '{}' ]; then echo leak; exit 42; else echo denied; fi",
            secret.display()
        );
        let command = SandboxCommand {
            argv: vec![os("/bin/sh"), os("-lc"), os(command_text)],
            cwd: workspace.path().to_path_buf(),
            timeout_ms: 5_000,
            max_output_chars: 1_000,
            profile: SandboxProfile {
                workspace_root: workspace.path().to_path_buf(),
                path_grants: vec![],
                network: SandboxNetworkMode::Disabled,
                session_bus: SandboxSessionBusMode::Deny,
                env: SandboxEnv::filtered_from_iter(
                    [("PATH", "/usr/bin:/bin")],
                    workspace.path(),
                    SandboxSessionBusMode::Deny,
                ),
            },
        };

        let output = match run(command).await {
            Ok(output) => output,
            Err(error) if error.contains("Sandboxed shell is unavailable") => return,
            Err(error) => panic!("sandbox command failed unexpectedly: {error}"),
        };

        assert!(output.success, "stderr: {}", output.stderr);
        assert!(output
            .stdout
            .contains(&workspace.path().display().to_string()));
        assert!(output.stdout.contains("denied"));
        assert!(!output.stdout.contains("leak"));
    }

    // Regression: when a configured grant is an ancestor of the workspace
    // (e.g. user grants `/home/me` RO while workspace lives somewhere under
    // it), naïve emit-order would bind the workspace first and then the
    // shallower grant on top, hiding the workspace's RW under the ancestor's
    // RO. The fix sorts shallowest-first so the deeper workspace bind
    // overlays the ancestor.
    #[test]
    fn workspace_bind_emitted_after_ancestor_grant_bind() {
        let workspace = PathBuf::from("/home/me/.local/share/clai/agent-workspaces/xxx");
        let ancestor = PathBuf::from("/home/me");
        let command = SandboxCommand {
            argv: vec![os("/bin/sh"), os("-lc"), os("pwd")],
            cwd: workspace.clone(),
            timeout_ms: 1_000,
            max_output_chars: 1_000,
            profile: SandboxProfile {
                workspace_root: workspace.clone(),
                path_grants: vec![SandboxPathGrant {
                    host_path: ancestor.clone(),
                    access: SandboxPathAccess::ReadOnly,
                }],
                network: SandboxNetworkMode::Host,
                session_bus: SandboxSessionBusMode::Deny,
                env: SandboxEnv::filtered_from_iter(
                    [("PATH", "/usr/bin:/bin")],
                    &workspace,
                    SandboxSessionBusMode::Deny,
                ),
            },
        };

        // Build args under a workspace_root that doesn't exist on the host:
        // skip validate_profile_paths and just inspect emit order.
        let mut args: Vec<OsString> = Vec::new();
        append_workspace_and_grants(&mut args, &command);

        let rendered: Vec<String> = args
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();

        // Each bind is emitted as [flag, source, dest] triples. Walk those
        // triples and record where each path's flag appears.
        let ancestor_str = "/home/me".to_string();
        let workspace_str = workspace.display().to_string();
        let mut ancestor_flag_idx: Option<usize> = None;
        let mut workspace_flag_idx: Option<usize> = None;
        for chunk_start in (0..rendered.len().saturating_sub(2)).step_by(3) {
            let flag = &rendered[chunk_start];
            let dest = &rendered[chunk_start + 2];
            if dest == &ancestor_str {
                assert_eq!(flag, "--ro-bind", "ancestor grant should be ro-bind");
                ancestor_flag_idx = Some(chunk_start);
            } else if dest == &workspace_str {
                assert_eq!(flag, "--bind", "workspace should be a writable bind");
                workspace_flag_idx = Some(chunk_start);
            }
        }

        let ancestor_flag_idx = ancestor_flag_idx.expect("ancestor grant should be bound");
        let workspace_flag_idx = workspace_flag_idx.expect("workspace should be bound");
        assert!(
            ancestor_flag_idx < workspace_flag_idx,
            "workspace bind ({workspace_flag_idx}) must come after ancestor grant ({ancestor_flag_idx}) so RW overlays RO; rendered: {rendered:?}"
        );
    }

    // End-to-end variant of the regression test: build a real workspace under
    // a real ancestor directory, grant the ancestor RO, run a write through
    // bwrap, and assert the write succeeded. Pre-fix, the ancestor's RO bind
    // would overlay the workspace bind and the write would fail with EROFS.
    #[tokio::test]
    async fn workspace_remains_writable_under_read_only_ancestor_grant() {
        let ancestor = tempfile::tempdir().unwrap();
        let workspace = ancestor.path().join("nested/workspace");
        std::fs::create_dir_all(&workspace).unwrap();

        let probe = workspace.join("probe.txt");
        let probe_in_sandbox = probe.display().to_string();

        let command = SandboxCommand {
            argv: vec![
                os("/bin/sh"),
                os("-lc"),
                os(format!("echo wrote > '{}'", probe_in_sandbox)),
            ],
            cwd: workspace.clone(),
            timeout_ms: 5_000,
            max_output_chars: 1_000,
            profile: SandboxProfile {
                workspace_root: workspace.clone(),
                path_grants: vec![SandboxPathGrant {
                    host_path: ancestor.path().to_path_buf(),
                    access: SandboxPathAccess::ReadOnly,
                }],
                network: SandboxNetworkMode::Disabled,
                session_bus: SandboxSessionBusMode::Deny,
                env: SandboxEnv::filtered_from_iter(
                    [("PATH", "/usr/bin:/bin")],
                    &workspace,
                    SandboxSessionBusMode::Deny,
                ),
            },
        };

        let output = match run(command).await {
            Ok(output) => output,
            Err(error) if error.contains("Sandboxed shell is unavailable") => return,
            Err(error) => panic!("sandbox command failed unexpectedly: {error}"),
        };

        assert!(
            output.success,
            "expected workspace write to succeed; stderr: {}",
            output.stderr
        );
        assert_eq!(
            std::fs::read_to_string(&probe).unwrap().trim(),
            "wrote",
            "workspace file should have been written through the sandbox"
        );
    }

    fn failure_output(stdout: &str, stderr: &str, success: bool) -> SandboxCommandOutput {
        SandboxCommandOutput {
            cwd: PathBuf::from("/tmp"),
            exit_code: if success { Some(0) } else { Some(1) },
            success,
            stdout: stdout.to_string(),
            stderr: stderr.to_string(),
        }
    }

    #[test]
    fn looks_like_bwrap_setup_failure_classifies_real_bwrap_error() {
        let out = failure_output("", "bwrap: setting up uid map: Permission denied\n", false);
        assert!(looks_like_bwrap_setup_failure(&out));
    }

    #[test]
    fn looks_like_bwrap_setup_failure_ignores_user_stderr_that_quotes_bwrap_prefix() {
        // Inner command ran (produced stdout) and exited non-zero, with a
        // stderr line that happens to start with `bwrap:`. The setup
        // classifier must NOT claim sandbox unavailable here.
        let out = failure_output(
            "did some work\n",
            "bwrap: this is the inner command's complaint\n",
            false,
        );
        assert!(!looks_like_bwrap_setup_failure(&out));
    }

    #[test]
    fn looks_like_bwrap_setup_failure_ignores_successful_runs() {
        let out = failure_output("ok\n", "bwrap: ignored\n", true);
        assert!(!looks_like_bwrap_setup_failure(&out));
    }

    #[test]
    fn looks_like_bwrap_setup_failure_ignores_inner_failure_without_bwrap_prefix() {
        let out = failure_output("", "command not found: foo\n", false);
        assert!(!looks_like_bwrap_setup_failure(&out));
    }

    #[test]
    fn looks_like_bwrap_setup_failure_matches_bwrap_line_not_at_stderr_start() {
        // Bwrap can emit several lines; the prefix-at-start check missed
        // failures where the first stderr line was a warning. Accept any line.
        let out = failure_output(
            "",
            "warning: something\nbwrap: creating new namespace failed: Operation not permitted\n",
            false,
        );
        assert!(looks_like_bwrap_setup_failure(&out));
    }
}
