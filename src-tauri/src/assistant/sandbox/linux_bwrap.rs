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

pub async fn run(mut command: SandboxCommand) -> Result<SandboxCommandOutput, String> {
    prune_missing_grants(&mut command).await;
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
    let in_flatpak = crate::providers::is_flatpak();

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

    append_runtime_file_binds(&mut args, in_flatpak);
    append_workspace_and_grants(&mut args, command);

    for (key, value) in command.profile.env.iter() {
        args.push(os("--setenv"));
        args.push(os(key));
        args.push(os(value));
    }

    // Emitted AFTER the profile env loop: inside Flatpak this overrides the
    // passed-through DBUS_SESSION_BUS_ADDRESS (which names the proxy socket we
    // intentionally don't bind) with the host bus path we do bind. --setenv is
    // last-writer-wins, so ordering matters.
    append_session_bus_bind(&mut args, command, in_flatpak);

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

    Ok(())
}

/// Drop path grants whose host path no longer exists before launching.
///
/// A grant can outlive its target — e.g. an `always`-scoped grant pointing at
/// a directory the user later deleted. Failing hard here would kill EVERY
/// sandboxed command for that agent with no way to recover (observed
/// 2026-07-05: one stale grant made all `bash_exec` calls abort with
/// "Sandbox path grant does not exist"). Pruning instead makes the stale path
/// simply absent inside the sandbox, which matches what an unsandboxed shell
/// would see (plain ENOENT at use time) — grants already bind with
/// `*-bind-try`, so bwrap itself never aborts on them. Pruning can only ever
/// REMOVE access, never widen it. Each pruned grant is logged so the stale
/// entry can be cleaned from agent settings.
///
/// Existence MUST be probed in the same mount namespace the bind resolves in.
/// In a Flatpak build the inner `bwrap` runs on the host via
/// `flatpak-spawn --host bwrap …`, so its grant binds resolve against the
/// *host* filesystem — but this process runs in the Flatpak namespace, where
/// `/tmp` is a private tmpfs and `/opt`, `/mnt`, … aren't mapped at all. A
/// plain in-process `Path::exists()` would then report a perfectly valid host
/// path (e.g. `/tmp/foo`) as missing and wrongly prune a live grant. So inside
/// Flatpak we probe on the host with `flatpak-spawn --host test -e`.
async fn prune_missing_grants(command: &mut SandboxCommand) {
    let in_flatpak = crate::providers::is_flatpak();
    let mut kept = Vec::with_capacity(command.profile.path_grants.len());
    for grant in command.profile.path_grants.drain(..) {
        let exists = if in_flatpak {
            host_path_exists(&grant.host_path).await
        } else {
            grant.host_path.exists()
        };
        if exists {
            kept.push(grant);
        } else {
            tracing::warn!(
                "Skipping filesystem path grant whose path no longer exists: {} — the \
                 sandbox launches without it. Remove the stale grant from agent \
                 settings to silence this warning.",
                grant.host_path.display()
            );
        }
    }
    command.profile.path_grants = kept;
}

/// Probe path existence on the *host* via `flatpak-spawn --host test -e`.
///
/// Fails open (returns `true`) when the probe itself can't be spawned — e.g.
/// `flatpak-spawn` is missing because the `org.freedesktop.Flatpak` talk
/// permission isn't granted. Failing open keeps the grant, whose `*-bind-try`
/// bind then degrades gracefully on its own; the real sandbox launch surfaces
/// the dedicated "flatpak-spawn not found" message for the spawn problem.
async fn host_path_exists(path: &Path) -> bool {
    let status = Command::new(FLATPAK_SPAWN_BIN)
        .arg("--host")
        .arg("test")
        .arg("-e")
        .arg(path.as_os_str())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await;
    match status {
        Ok(status) => status.success(),
        // Path grants are always absolute, so `test -e <path>` can't be
        // confused by a leading-dash argument; only a spawn failure lands here.
        Err(_) => true,
    }
}

fn append_runtime_file_binds(args: &mut Vec<OsString>, in_flatpak: bool) {
    for bind in runtime_file_binds(in_flatpak) {
        for dir in private_parent_dirs_for(&bind.destination) {
            args.push(os("--dir"));
            args.push(dir.into_os_string());
        }
        // Use --ro-bind-try (not --ro-bind) so a runtime file that has
        // vanished between resolve-time and bwrap-launch-time degrades
        // gracefully (bind skipped) instead of aborting the whole sandbox
        // before any command runs.
        args.push(os("--ro-bind-try"));
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
fn append_session_bus_bind(args: &mut Vec<OsString>, command: &SandboxCommand, in_flatpak: bool) {
    if !matches!(command.profile.session_bus, SandboxSessionBusMode::Allow) {
        return;
    }
    let bus_path = resolve_session_bus_socket(in_flatpak);
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
    // Inside Flatpak the bus path is the host's well-known socket, derived
    // from $XDG_RUNTIME_DIR without probing the (invisible) host filesystem —
    // so bind leniently with --ro-bind-try and let a host that genuinely has
    // no session bus degrade gracefully instead of aborting the sandbox. On a
    // normal host the path was resolved via an existence check, so a hard
    // --ro-bind is appropriate.
    args.push(os(if in_flatpak {
        "--ro-bind-try"
    } else {
        "--ro-bind"
    }));
    args.push(bus_path.clone().into_os_string());
    args.push(bus_path.clone().into_os_string());

    // Inside Flatpak the app's DBUS_SESSION_BUS_ADDRESS names the xdg-dbus-proxy
    // socket (e.g. unix:path=/run/flatpak/bus) that we deliberately did NOT
    // bind — it exists only inside the Flatpak mount namespace, not on the host
    // where bwrap actually runs. Point the sandboxed shell at the host bus we
    // just bound so libsecret connects to a socket that exists in the sandbox.
    if in_flatpak {
        let mut addr = OsString::from("unix:path=");
        addr.push(bus_path.as_os_str());
        args.push(os("--setenv"));
        args.push(os("DBUS_SESSION_BUS_ADDRESS"));
        args.push(addr);
    }
}

/// Locate the host's D-Bus session bus socket on the filesystem. Returns
/// None if the bus is abstract-socket-only (no file to bind) or simply
/// absent on this host.
fn resolve_session_bus_socket(in_flatpak: bool) -> Option<PathBuf> {
    if in_flatpak {
        // Inside Flatpak, DBUS_SESSION_BUS_ADDRESS points at the
        // xdg-dbus-proxy socket (`unix:path=/run/flatpak/bus`) and
        // `$XDG_RUNTIME_DIR/bus` is itself a symlink to that same proxy —
        // both resolve to paths that exist ONLY inside the Flatpak mount
        // namespace. But bwrap runs on the HOST via `flatpak-spawn --host
        // bwrap …`, where the proxy path is absent; binding it aborts setup
        // with `bwrap: Can't find source path /run/flatpak/bus`.
        //
        // The host's real session bus lives at the conventional
        // `$XDG_RUNTIME_DIR/bus`. XDG_RUNTIME_DIR is identical inside and
        // outside the sandbox (e.g. `/run/user/<uid>`), so we build that
        // literal path WITHOUT following symlinks (which would chase back to
        // the proxy) and WITHOUT probing existence (the host fs isn't visible
        // from in here — caller binds it with --ro-bind-try). This derivation
        // is host-agnostic: it never hardcodes a UID or path.
        let runtime_dir = std::env::var_os("XDG_RUNTIME_DIR")?;
        let runtime_dir = PathBuf::from(runtime_dir);
        if !runtime_dir.is_absolute() {
            return None;
        }
        return Some(runtime_dir.join("bus"));
    }
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
    // The bool is `lenient`: false for the workspace root (a missing workspace
    // is fatal), true for grants (a missing/invisible grant is skipped via
    // *-bind-try rather than aborting the whole sandbox).
    let mut ops: Vec<(PathBuf, MountOp)> =
        Vec::with_capacity(command.profile.path_grants.len() + 2);
    ops.push((
        command.profile.workspace_root.clone(),
        MountOp::Bind(SandboxPathAccess::ReadWrite, false),
    ));
    for grant in &command.profile.path_grants {
        if grant.host_path == command.profile.workspace_root {
            continue;
        }
        ops.push((grant.host_path.clone(), MountOp::Bind(grant.access, true)));
    }

    // Workspace isolation: overlay an empty tmpfs on the workspace *container*
    // (e.g. `~/.clai/workspaces`) so a broad `$HOME` bind can't expose sibling
    // workspaces. The container is shallower than the workspace root, so the
    // depth-sort below emits the tmpfs first and the workspace bind (and any
    // explicitly-granted sibling, which is also deeper) lands on top of it —
    // re-exposing exactly what's allowed. See `profile::workspace_mask`.
    let home = command.profile.env.home().map(Path::new);
    if let Some(mask) =
        crate::assistant::sandbox::profile::workspace_mask(&command.profile.workspace_root, home)
    {
        ops.push((mask, MountOp::Tmpfs));
    }

    ops.sort_by_key(|(path, _)| path_depth(path));

    for (path, op) in ops {
        match op {
            MountOp::Bind(access, lenient) => append_bind(args, access, &path, &path, lenient),
            MountOp::Tmpfs => {
                args.push(os("--tmpfs"));
                args.push(path.into_os_string());
            }
        }
    }
}

enum MountOp {
    /// `--bind`/`--ro-bind` (or the `*-try` lenient variant).
    Bind(SandboxPathAccess, bool),
    /// `--tmpfs`: overlay an empty tmpfs to hide a subtree.
    Tmpfs,
}

/// Emit a single bind. `lenient` selects bwrap's `*-bind-try` variant, which
/// skips the bind (instead of aborting the entire sandbox) when the source
/// path is absent at launch time. Used for path grants so one stale or
/// namespace-invisible grant can't disable every `bash_exec`; the workspace
/// root binds non-leniently because its absence is a genuine, fatal error.
fn append_bind(
    args: &mut Vec<OsString>,
    access: SandboxPathAccess,
    source: &Path,
    dest: &Path,
    lenient: bool,
) {
    let flag = match (access, lenient) {
        (SandboxPathAccess::ReadOnly, false) => "--ro-bind",
        (SandboxPathAccess::ReadOnly, true) => "--ro-bind-try",
        (SandboxPathAccess::ReadWrite, false) => "--bind",
        (SandboxPathAccess::ReadWrite, true) => "--bind-try",
    };
    args.push(os(flag));
    args.push(source.as_os_str().to_os_string());
    args.push(dest.as_os_str().to_os_string());
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RuntimeFileBind {
    source: PathBuf,
    destination: PathBuf,
}

fn runtime_file_binds(in_flatpak: bool) -> Vec<RuntimeFileBind> {
    if in_flatpak {
        // Inside Flatpak, `/etc/resolv.conf` and `/etc/localtime` are symlinks
        // into the Flatpak runtime's own filesystem view — typically targeting
        // paths under `/run/host/...` that the Flatpak fabricates for the
        // sandboxed app (e.g. `/run/host/monitor/resolv.conf`). Resolving the
        // symlink here therefore yields a bind whose source/destination only
        // exist inside the Flatpak's mount namespace.
        //
        // bwrap, however, runs on the *host* via `flatpak-spawn --host bwrap …`
        // (see `launch_argv`). The host has no such path, so feeding the
        // resolved source to a bind aborts sandbox setup with
        //   `bwrap: Can't find source path …: No such file or directory`
        // before any inner command can run — leaving the sandboxed shell
        // permanently unavailable in a Flatpak build.
        //
        // We can't safely probe the host's filesystem from inside Flatpak (it
        // has its own view), so we don't chase the symlink chain. The wholesale
        // `--ro-bind /etc /etc` line in `bwrap_args` is executed by the host's
        // bwrap and already exposes the host's `/etc/resolv.conf` /
        // `/etc/localtime` via the host's view. On systemd-resolved hosts where
        // `/etc/resolv.conf` is a symlink to `/run/systemd/resolve/...` the
        // target lives outside `/etc` and won't be bound — DNS via resolv.conf
        // may degrade inside the sandbox, but the sandbox itself stays usable,
        // which is the necessary condition for the agent to function at all in
        // a Flatpak install.
        return Vec::new();
    }
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
    fn workspace_container_is_masked_with_tmpfs_before_workspace_bind() {
        // A broad $HOME grant would otherwise expose every sibling workspace
        // under ~/.clai/workspaces. The container gets a tmpfs overlay, and the
        // depth-sort must place that tmpfs BEFORE the workspace bind so the
        // agent's own workspace is re-exposed on top of the empty overlay.
        let home = tempfile::tempdir().unwrap();
        let container = home.path().join(".clai").join("workspaces");
        let workspace = container.join("ws-abc");
        std::fs::create_dir_all(&workspace).unwrap();

        let mut command = sample_command();
        command.cwd = workspace.clone();
        command.profile.workspace_root = workspace.clone();
        command.profile.env = SandboxEnv::filtered_from_iter(
            [("PATH", "/usr/bin:/bin")],
            home.path(),
            SandboxSessionBusMode::Deny,
        );
        command.profile.path_grants = vec![SandboxPathGrant {
            host_path: home.path().to_path_buf(),
            access: SandboxPathAccess::ReadOnly,
        }];

        let args = bwrap_args(&command).unwrap();
        let rendered: Vec<String> = args
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();

        let container_str = container.to_string_lossy().into_owned();
        let workspace_str = workspace.to_string_lossy().into_owned();
        let tmpfs_idx = rendered
            .windows(2)
            .position(|w| w[0] == "--tmpfs" && w[1] == container_str)
            .unwrap_or_else(|| panic!("container should be masked with --tmpfs; got {rendered:?}"));
        let ws_bind_idx = rendered
            .windows(3)
            .position(|w| w[0] == "--bind" && w[1] == workspace_str && w[2] == workspace_str)
            .unwrap_or_else(|| panic!("workspace should be bound; got {rendered:?}"));
        assert!(
            tmpfs_idx < ws_bind_idx,
            "tmpfs over the container must precede the workspace bind; got {rendered:?}"
        );
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
            || resolve_session_bus_socket(false),
        );
        assert_eq!(resolved.as_deref(), Some(custom_socket.as_path()));
    }

    #[test]
    fn session_bus_falls_back_to_xdg_runtime_dir_when_address_unset() {
        let runtime = tempfile::tempdir().unwrap();
        let bus_path = runtime.path().join("bus");
        std::fs::write(&bus_path, "").unwrap();

        let resolved = with_dbus_env(None, Some(runtime.path()), || {
            resolve_session_bus_socket(false)
        });
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
            || resolve_session_bus_socket(false),
        );
        assert!(resolved.is_none());
    }

    #[test]
    fn session_bus_returns_none_when_nothing_is_set() {
        // Headless / containerized / pre-session contexts: no bus exists.
        // The toggle becomes a no-op, no panic.
        let resolved = with_dbus_env(None, None, || resolve_session_bus_socket(false));
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
            || resolve_session_bus_socket(false),
        );
        assert_eq!(resolved.as_deref(), Some(real_socket.as_path()));
    }

    // Regression: inside Flatpak, DBUS_SESSION_BUS_ADDRESS names the
    // xdg-dbus-proxy socket (`unix:path=/run/flatpak/bus`) and
    // `$XDG_RUNTIME_DIR/bus` is a symlink to it — both exist only inside the
    // Flatpak namespace. bwrap runs on the host (`flatpak-spawn --host bwrap`)
    // where that path is absent, so binding it aborts the sandbox with
    // `bwrap: Can't find source path /run/flatpak/bus`. Inside Flatpak we must
    // instead derive the host's well-known bus path from $XDG_RUNTIME_DIR,
    // ignoring the proxy address entirely and without probing the (invisible)
    // host filesystem.
    #[test]
    fn session_bus_uses_host_runtime_dir_path_inside_flatpak() {
        // The proxy address must NOT win, and the runtime dir's bus need not
        // exist (host fs isn't visible from in here) — so use a path that
        // does not exist on the test machine.
        let resolved = with_dbus_env(
            Some("unix:path=/run/flatpak/bus"),
            Some(Path::new("/run/user/4242")),
            || resolve_session_bus_socket(true),
        );
        assert_eq!(resolved.as_deref(), Some(Path::new("/run/user/4242/bus")));
    }

    #[test]
    fn session_bus_none_inside_flatpak_when_runtime_dir_unset() {
        let resolved = with_dbus_env(Some("unix:path=/run/flatpak/bus"), None, || {
            resolve_session_bus_socket(true)
        });
        assert!(resolved.is_none());
    }

    // Inside Flatpak the bus bind must be lenient (--ro-bind-try, since we
    // can't verify the host socket exists) and the sandboxed shell's
    // DBUS_SESSION_BUS_ADDRESS must be rewritten to the bound host path so
    // libsecret connects to a socket that actually exists in the sandbox.
    #[test]
    fn session_bus_allow_inside_flatpak_binds_host_path_and_overrides_address() {
        let mut command = sample_command();
        command.profile.session_bus = SandboxSessionBusMode::Allow;

        let args = with_dbus_env(
            Some("unix:path=/run/flatpak/bus"),
            Some(Path::new("/run/user/4242")),
            || {
                let mut args: Vec<OsString> = Vec::new();
                append_session_bus_bind(&mut args, &command, true);
                args
            },
        );

        let rendered: Vec<String> = args
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();

        // Lenient bind of the host bus path (never the proxy path).
        let host_bus = "/run/user/4242/bus".to_string();
        assert!(
            rendered
                .windows(3)
                .any(|w| w == ["--ro-bind-try", &host_bus, &host_bus]),
            "expected lenient --ro-bind-try of the host bus path; rendered: {rendered:?}"
        );
        assert!(
            !rendered.iter().any(|arg| arg.contains("/run/flatpak/bus")),
            "must never reference the in-namespace proxy path; rendered: {rendered:?}"
        );

        // Address override points the inner shell at the bound host socket.
        let addr_idx = rendered
            .windows(3)
            .position(|w| w[0] == "--setenv" && w[1] == "DBUS_SESSION_BUS_ADDRESS")
            .expect("DBUS_SESSION_BUS_ADDRESS should be overridden inside Flatpak");
        assert_eq!(rendered[addr_idx + 2], "unix:path=/run/user/4242/bus");
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

    // Regression: inside Flatpak the runtime symlinks resolve to paths that
    // exist only in the Flatpak's mount namespace (e.g.
    // `/run/host/monitor/resolv.conf`), but bwrap runs on the host via
    // `flatpak-spawn --host bwrap …` and has no such path. Feeding those
    // sources to a bind aborts sandbox setup before any command runs, so we
    // must emit no runtime-file binds at all inside Flatpak and rely on the
    // wholesale `--ro-bind /etc /etc` line instead.
    #[test]
    fn runtime_file_binds_returns_empty_inside_flatpak() {
        assert!(runtime_file_binds(true).is_empty());
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
                assert_eq!(
                    flag, "--ro-bind-try",
                    "ancestor grant should be a lenient ro-bind"
                );
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

    // A grant whose host path is gone must bind via `--ro-bind-try` (skipped by
    // bwrap) rather than `--ro-bind` (which aborts the whole sandbox). This is
    // what stops one stale grant from disabling every `bash_exec` for a session.
    #[test]
    fn missing_grant_binds_leniently_so_sandbox_still_launches() {
        let workspace = tempfile::tempdir().unwrap();
        let stale = vanished_path();
        let command = SandboxCommand {
            argv: vec![os("/bin/sh"), os("-lc"), os("pwd")],
            cwd: workspace.path().to_path_buf(),
            timeout_ms: 1_000,
            max_output_chars: 1_000,
            profile: SandboxProfile {
                workspace_root: workspace.path().to_path_buf(),
                path_grants: vec![SandboxPathGrant {
                    host_path: stale.clone(),
                    access: SandboxPathAccess::ReadOnly,
                }],
                network: SandboxNetworkMode::Host,
                session_bus: SandboxSessionBusMode::Deny,
                env: SandboxEnv::filtered_from_iter(
                    [("PATH", "/usr/bin:/bin")],
                    workspace.path(),
                    SandboxSessionBusMode::Deny,
                ),
            },
        };

        let mut args: Vec<OsString> = Vec::new();
        append_workspace_and_grants(&mut args, &command);
        let rendered: Vec<String> = args
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();

        let grant_str = stale.display().to_string();
        let mut found = false;
        for chunk_start in (0..rendered.len().saturating_sub(2)).step_by(3) {
            if rendered[chunk_start + 2] == grant_str {
                assert_eq!(
                    rendered[chunk_start], "--ro-bind-try",
                    "a grant must bind leniently; rendered: {rendered:?}"
                );
                found = true;
            }
        }
        assert!(found, "grant bind not emitted; rendered: {rendered:?}");
    }

    /// A path that is guaranteed absent: a tempdir created and immediately
    /// dropped (deleted). Safer than hardcoding a literal that could one day
    /// exist and silently un-pin the regression.
    fn vanished_path() -> PathBuf {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stale-grant");
        drop(dir);
        assert!(!path.exists());
        path
    }

    // A grant whose path has vanished (e.g. an `always` grant to a workspace
    // the user deleted) is PRUNED so the command still runs; live grants are
    // kept. Previously this aborted every bash_exec up front with
    // "Sandbox path grant does not exist".
    #[tokio::test]
    async fn prune_missing_grants_drops_stale_grant_and_keeps_live_ones() {
        if crate::providers::is_flatpak() {
            return; // off-Flatpak assertion only (in-process existence probe)
        }
        let workspace = tempfile::tempdir().unwrap();
        let live = tempfile::tempdir().unwrap();
        let live_grant = SandboxPathGrant {
            host_path: live.path().to_path_buf(),
            access: SandboxPathAccess::ReadWrite,
        };
        let mut command = SandboxCommand {
            argv: vec![os("/bin/sh"), os("-lc"), os("pwd")],
            cwd: workspace.path().to_path_buf(),
            timeout_ms: 1_000,
            max_output_chars: 1_000,
            profile: SandboxProfile {
                workspace_root: workspace.path().to_path_buf(),
                path_grants: vec![
                    SandboxPathGrant {
                        host_path: vanished_path(),
                        access: SandboxPathAccess::ReadOnly,
                    },
                    live_grant.clone(),
                ],
                network: SandboxNetworkMode::Host,
                session_bus: SandboxSessionBusMode::Deny,
                env: SandboxEnv::filtered_from_iter(
                    [("PATH", "/usr/bin:/bin")],
                    workspace.path(),
                    SandboxSessionBusMode::Deny,
                ),
            },
        };

        prune_missing_grants(&mut command).await;

        assert_eq!(
            command.profile.path_grants,
            vec![live_grant],
            "stale grant should be pruned, live grant kept"
        );
    }

    // End-to-end regression for the "deleted grant path breaks bash_exec
    // entirely" bug: launch a real sandbox with a grant whose path doesn't
    // exist and assert the command still runs.
    #[tokio::test]
    async fn run_succeeds_despite_missing_grant_path() {
        let workspace = tempfile::tempdir().unwrap();
        let command = SandboxCommand {
            argv: vec![os("/bin/sh"), os("-lc"), os("pwd")],
            cwd: workspace.path().to_path_buf(),
            timeout_ms: 5_000,
            max_output_chars: 1_000,
            profile: SandboxProfile {
                workspace_root: workspace.path().to_path_buf(),
                path_grants: vec![SandboxPathGrant {
                    host_path: vanished_path(),
                    access: SandboxPathAccess::ReadOnly,
                }],
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

        assert!(
            output.success,
            "command should succeed despite the stale grant; stderr: {}",
            output.stderr
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
