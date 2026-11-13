use std::ffi::{OsStr, OsString};
use std::path::{Component, Path, PathBuf};

use tokio::process::Command;

use super::filesystem::normalize_path;
use super::helper::{BOX_HELPER, BOX_PROXY_PORT, BOX_SOCKET};
use super::profile::SESSION_BUS_OFF_ADDRESS;
use super::runner::{prepare_stdio, run_spawned_child};
use super::{
    SandboxCommand, SandboxCommandOutput, SandboxNetworkMode, SandboxPathAccess,
    SandboxSessionBusMode, SessionBusProxy,
};
use crate::assistant::network::spawn_roots;

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
    // SAFETY: the hook runs in the forked child and makes only
    // async-signal-safe system calls.
    unsafe {
        child_command.pre_exec(|| {
            mark_inherited_fds_cloexec();
            Ok(())
        });
    }

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
    let output = run_recorded(child, command).await?;

    if looks_like_bwrap_setup_failure(&output) {
        return Err(classify_bwrap_failure(&output.stderr));
    }

    Ok(output)
}

/// Runs the started command to its end. An agent can ask for a server the
/// command starts until then.
async fn run_recorded(
    child: tokio::process::Child,
    command: SandboxCommand,
) -> Result<SandboxCommandOutput, String> {
    let _command = spawn_roots::record_agent_command(child.id());
    run_spawned_child(
        child,
        command.cwd,
        command.timeout_ms,
        command.max_output_chars,
        "Sandboxed shell command",
    )
    .await
}

/// Sets close-on-exec on every descriptor above stdio. bwrap hands the
/// command every descriptor it inherits, and this process holds some without
/// the flag (the desktop session that started it leaves /proc and cgroup
/// files open), so a leaked socket or directory would be a way out of the
/// sandbox. Close-on-exec rather than close: the standard library reports a
/// failed exec through a descriptor of its own that must stay open until then.
fn mark_inherited_fds_cloexec() {
    // SAFETY: close_range(2) only changes descriptor flags (Linux 5.11+).
    let marked = unsafe {
        libc::syscall(
            libc::SYS_close_range,
            3_u32,
            u32::MAX,
            libc::CLOSE_RANGE_CLOEXEC,
        )
    };
    if marked == 0 {
        return;
    }
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: getrlimit writes into the struct it is given.
    let highest = if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) } == 0 {
        limit.rlim_cur.min(1 << 20) as libc::c_int
    } else {
        1024
    };
    for fd in 3..highest {
        // SAFETY: fcntl on a descriptor that may not exist only fails.
        unsafe {
            let flags = libc::fcntl(fd, libc::F_GETFD);
            if flags >= 0 && flags & libc::FD_CLOEXEC == 0 {
                libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC);
            }
        }
    }
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
    bwrap_args_with(command, host_var_run_is_run())
}

/// [`bwrap_args`], told whether the host has `/var/run` as a link to `/run`.
fn bwrap_args_with(command: &SandboxCommand, var_run_link: bool) -> Result<Vec<OsString>, String> {
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
        SandboxNetworkMode::Disabled | SandboxNetworkMode::Proxied(_) => {
            args.push(os("--unshare-net"));
        }
    }

    args.extend([
        os("--die-with-parent"),
        os("--new-session"),
        os("--clearenv"),
        os("--proc"),
        os("/proc"),
        os("--dev"),
        os("/dev"),
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

    // Before any mount: once a mount has made `/var/run` a directory, bwrap
    // cannot put the link there. A name only, `/run` holds what the mounts
    // below put there. `../run`, not `/run`: while bwrap sets the sandbox
    // up, an absolute target names its own root, not the sandbox one, so a
    // mount through the link would miss. `/run` exists so such a mount (a
    // bus path under `/var/run`) finds its parent.
    if var_run_link {
        args.extend([
            os("--dir"),
            os("/run"),
            os("--symlink"),
            os("../run"),
            os(VAR_RUN),
        ]);
    }
    append_tmp_mount(&mut args, command);
    append_runtime_file_binds(&mut args, in_flatpak);
    append_workspace_and_grants(&mut args, command);
    append_socket_masks(&mut args, command);
    append_allowed_socket_binds(&mut args, command, var_run_link);

    for (key, value) in command.profile.env.iter() {
        args.push(os("--setenv"));
        args.push(os(key));
        args.push(os(value));
    }

    append_tmp_env(&mut args, command);
    // After the grants, so no grant can cover the proxy's or the helper's
    // files, and after the profile env, which drops the host's own proxy
    // variables.
    let helper = box_helper(command);
    if let Some(helper) = helper {
        args.extend([
            os("--dir"),
            os("/run/clai"),
            os("--ro-bind"),
            os(helper),
            os(BOX_HELPER),
        ]);
    }
    append_proxy_binds_and_env(&mut args, &command.profile.network);

    // Emitted AFTER the profile env loop: inside Flatpak this overrides the
    // passed-through DBUS_SESSION_BUS_ADDRESS (which names the proxy socket we
    // intentionally don't bind) with the host bus path we do bind. --setenv is
    // last-writer-wins, so ordering matters.
    let bus = append_session_bus_bind(&mut args, command, in_flatpak);

    args.push(os("--chdir"));
    args.push(command.cwd.as_os_str().to_os_string());
    args.push(os("--"));
    if helper.is_some() {
        append_helper_entry(&mut args, command, bus.as_deref());
    }
    args.extend(command.argv.iter().cloned());

    Ok(args)
}

/// The helper's mode and the sockets it lets the command reach: the
/// session bus when bound and the agent's socket grants. In Host network
/// host abstract sockets (X11, for one) stay reachable: that mode reaches
/// every host service on localhost, so refusing them guards nothing and
/// leaves the user no way to grant them.
fn append_helper_entry(args: &mut Vec<OsString>, command: &SandboxCommand, bus: Option<&Path>) {
    args.push(os(BOX_HELPER));
    match &command.profile.network {
        SandboxNetworkMode::Proxied(endpoint) => {
            args.extend([
                os("bridge"),
                os("--socket"),
                os(BOX_SOCKET),
                os("--http"),
                os(BOX_PROXY_PORT.to_string()),
            ]);
            for port in &endpoint.forward_ports {
                args.extend([os("--forward"), os(port.to_string())]);
            }
        }
        SandboxNetworkMode::Host => args.extend([os("run"), os("--host-abstract-unix")]),
        SandboxNetworkMode::Disabled => args.push(os("run")),
    }
    // One rule per socket: Landlock allows the file, under either name.
    let allowed = command
        .profile
        .sockets
        .allowed
        .iter()
        .map(|socket| socket.host.as_path());
    for path in bus.into_iter().chain(allowed) {
        args.extend([os("--allow-unix"), os(path)]);
    }
    args.push(os("--"));
}

/// The helper that starts the command: the Allowlist bridge, which cannot
/// run without it, or the one that confines Unix sockets in Off and Host.
fn box_helper(command: &SandboxCommand) -> Option<&Path> {
    match &command.profile.network {
        SandboxNetworkMode::Proxied(endpoint) => Some(&endpoint.helper),
        _ => command.profile.sockets.helper.as_deref(),
    }
}

/// Where host sockets usually live under the home directory: the input
/// method bus, Docker Desktop, 1Password, colima, lima, Rancher Desktop and
/// gpg-agent without a runtime directory.
pub(super) const SOCKET_DIRS: &[&str] = &[
    ".cache/ibus",
    ".docker/desktop",
    ".docker/run",
    ".1password",
    ".colima",
    ".lima",
    ".rd",
    ".gnupg",
];

/// How deep into a [`SOCKET_DIRS`] entry sockets are looked for: lima keeps
/// them in `~/.lima/<instance>/sock/`.
const SOCKET_SCAN_DEPTH: usize = 3;
/// At most this many entries are looked at per directory, so a huge tree
/// cannot slow every command down.
const SOCKET_SCAN_ENTRIES: usize = 512;

/// Without Landlock a socket under a grant stays connectable, read-only or
/// not, so cover each socket a grant exposes in [`SOCKET_DIRS`] with
/// `/dev/null`. Only the sockets: the directories and their other files stay
/// as the grants show them, so `gpg --verify` still reads `~/.gnupg` and
/// `limactl list` still reads `~/.lima`. A socket the agent has a grant on
/// is left as it is, and bound again later. Best effort: sockets elsewhere
/// stay reachable.
fn append_socket_masks(args: &mut Vec<OsString>, command: &SandboxCommand) {
    let profile = &command.profile;
    if profile.sockets.confined() {
        return;
    }
    let Some(home) = &profile.filesystem.home else {
        return;
    };
    let granted = |socket: &Path| {
        profile
            .sockets
            .allowed
            .iter()
            .any(|allowed| allowed.path == socket || allowed.host == socket)
    };
    for dir in SOCKET_DIRS.iter().map(|dir| home.join(dir)) {
        for socket in sockets_under(&dir) {
            if !granted(&socket)
                && profile
                    .filesystem
                    .covers(&socket, crate::config::FilesystemPathAccess::ReadOnly)
            {
                args.extend([os("--ro-bind"), os("/dev/null"), os(&socket)]);
            }
        }
    }
}

/// The sockets in `dir` and its subdirectories, [`SOCKET_SCAN_DEPTH`] deep,
/// without following symlinks.
fn sockets_under(dir: &Path) -> Vec<PathBuf> {
    use std::os::unix::fs::FileTypeExt;
    let mut sockets = Vec::new();
    let mut pending = vec![(dir.to_path_buf(), 1)];
    let mut seen = 0;
    while let Some((dir, depth)) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            seen += 1;
            if seen > SOCKET_SCAN_ENTRIES {
                return sockets;
            }
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_socket() {
                sockets.push(entry.path());
            } else if kind.is_dir() && depth < SOCKET_SCAN_DEPTH {
                pending.push((entry.path(), depth + 1));
            }
        }
    }
    sockets
}

/// The host sockets the agent may connect to, bound at the host path and at
/// the path its grant names, after the grants and the socket masks, so
/// neither hides them. A name under a system bind (`/etc`, `/usr`) is
/// already there and cannot take a mount point. With the `/var/run` link a
/// name under it is bound at `/run`, the same file, so `/var/run/docker.sock`
/// and `/run/docker.sock` take one mount and none goes through the link.
/// `--ro-bind-try`: a socket that went away since it was resolved (a
/// restarted service) leaves the command without it instead of failing it.
fn append_allowed_socket_binds(
    args: &mut Vec<OsString>,
    command: &SandboxCommand,
    var_run_link: bool,
) {
    let in_sandbox = |name: &Path| match name.strip_prefix(VAR_RUN) {
        Ok(rest) if var_run_link => Path::new("/run").join(rest),
        _ => name.to_path_buf(),
    };
    for socket in &command.profile.sockets.allowed {
        let host = in_sandbox(&socket.host);
        let path = in_sandbox(&socket.path);
        let names = std::iter::once(host.clone())
            .chain(Some(path).filter(|path| *path != host))
            .filter(|name| !path_is_covered_by_system_bind(name));
        for name in names {
            for dir in private_parent_dirs_for(&name) {
                args.extend([os("--dir"), dir.into_os_string()]);
            }
            args.extend([os("--ro-bind-try"), os(&socket.host), os(name)]);
        }
    }
}

const VAR_RUN: &str = "/var/run";

/// Whether the host has `/var/run` as a link to `/run`, as current Linux
/// systems do. The sandbox then gets the same link, so the path a tool uses
/// by default (`/var/run/docker.sock`) names what is bound under `/run`.
/// Otherwise `/var/run` is left out as before: a real directory there holds
/// other files, which only a grant may show.
fn host_var_run_is_run() -> bool {
    is_link_to(Path::new(VAR_RUN), Path::new("/run"))
}

fn is_link_to(link: &Path, target: &Path) -> bool {
    std::fs::symlink_metadata(link).is_ok_and(|meta| meta.file_type().is_symlink())
        && matches!(
            (std::fs::canonicalize(link), std::fs::canonicalize(target)),
            (Ok(link), Ok(target)) if link == target
        )
}

/// Allowlist: the proxy socket, the helper and the ssh configuration, and
/// the variables that send HTTP(S) clients to the helper's port. In-box
/// loopback bypasses the proxy, which would refuse it, so an agent's own
/// test server on `localhost` still works.
fn append_proxy_binds_and_env(args: &mut Vec<OsString>, network: &SandboxNetworkMode) {
    let SandboxNetworkMode::Proxied(endpoint) = network else {
        return;
    };
    args.extend([
        os("--bind"),
        os(&endpoint.socket),
        os(BOX_SOCKET),
        os("--ro-bind"),
        os(&endpoint.ssh_config),
        os("/etc/ssh/ssh_config"),
    ]);
    let proxy = format!("http://127.0.0.1:{BOX_PROXY_PORT}");
    let java = format!(
        "-Dhttp.proxyHost=127.0.0.1 -Dhttp.proxyPort={BOX_PROXY_PORT} \
         -Dhttps.proxyHost=127.0.0.1 -Dhttps.proxyPort={BOX_PROXY_PORT} \
         -Dhttp.nonProxyHosts=localhost|127.0.0.1"
    );
    let no_proxy = "localhost,127.0.0.1,::1";
    for (key, value) in [
        ("HTTP_PROXY", proxy.as_str()),
        ("HTTPS_PROXY", &proxy),
        ("http_proxy", &proxy),
        ("https_proxy", &proxy),
        ("NO_PROXY", no_proxy),
        ("no_proxy", no_proxy),
        ("NODE_USE_ENV_PROXY", "1"),
        ("JAVA_TOOL_OPTIONS", &java),
    ] {
        args.extend([os("--setenv"), os(key), os(value)]);
    }
}

/// Mount the sandbox's `/tmp`.
///
/// With scratch space available this binds the workspace's persistent scratch
/// directory, so files an agent leaves in `/tmp` survive until its next
/// command instead of vanishing with a per-command tmpfs. Without it we keep
/// the original empty tmpfs — a private `/tmp` either way, never the host's.
///
/// Emitted before the grant binds so an explicit user grant for `/tmp` still
/// wins: bwrap mounts are last-writer-wins, and a path the user deliberately
/// granted must override our default.
fn append_tmp_mount(args: &mut Vec<OsString>, command: &SandboxCommand) {
    // Always establish the tmpfs first: it is the guaranteed floor. If the
    // scratch bind below is skipped because its source vanished, the sandbox
    // still has a private, writable /tmp instead of none at all.
    args.push(os("--tmpfs"));
    args.push(os("/tmp"));

    let Some(scratch) = command.profile.scratch_tmp.as_deref() else {
        return;
    };
    // `--bind-try`, not `--bind`: the scratch dir is resolved when the profile
    // is built, and a hard bind would abort the whole command if it disappeared
    // before exec (a second app instance resetting the same workspace, or the
    // user clearing the directory). Every grant bind is lenient for exactly
    // this reason. Emitted after the tmpfs so it wins when present.
    args.push(os("--bind-try"));
    args.push(scratch.as_os_str().to_os_string());
    args.push(os("/tmp"));
}

/// Point the standard temp-dir variables at the sandbox's `/tmp`.
///
/// The env allowlist strips the host's TMPDIR, so without this a tool that
/// honours TMPDIR would fall back to its own default. Set only alongside the
/// scratch bind; with a plain tmpfs the unset-TMPDIR default of `/tmp` is
/// already correct.
fn append_tmp_env(args: &mut Vec<OsString>, command: &SandboxCommand) {
    if command.profile.scratch_tmp.is_none() {
        return;
    }
    for key in ["TMPDIR", "TMP", "TEMP"] {
        args.push(os("--setenv"));
        args.push(os(key));
        args.push(os("/tmp"));
    }
}

fn validate_profile_paths(command: &SandboxCommand) -> Result<(), String> {
    if !command.profile.filesystem.workspace_root.exists() {
        return Err(format!(
            "Sandbox workspace does not exist: {}",
            command.profile.filesystem.workspace_root.display()
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
    let mut kept = Vec::with_capacity(command.profile.filesystem.path_grants.len());
    for grant in command.profile.filesystem.path_grants.drain(..) {
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
    command.profile.filesystem.path_grants = kept;
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
/// If none of those resolve, we log a warning and skip the bind, and a
/// command whose env names no bus gets [`SESSION_BUS_OFF_ADDRESS`]; the
/// agent's libsecret-using tools will then fail with the same "no bus"
/// error they would have without the toggle, and can escalate via
/// `workspace_requestUserInput`. This makes the toggle meaningful only
/// where a session bus actually exists — desktop Linux — and a graceful
/// no-op everywhere else (headless servers, containers, WSL).
fn append_session_bus_bind(
    args: &mut Vec<OsString>,
    command: &SandboxCommand,
    in_flatpak: bool,
) -> Option<PathBuf> {
    if !matches!(command.profile.session_bus, SandboxSessionBusMode::Allow) {
        return None;
    }
    if let Some(proxy) = &command.profile.session_bus_proxy {
        return Some(append_bus_proxy_bind(args, proxy));
    }
    let bus_path = resolve_session_bus_socket(in_flatpak);
    if bus_path.is_none()
        && !matches!(command.profile.network, SandboxNetworkMode::Host)
        && std::env::var("DBUS_SESSION_BUS_ADDRESS")
            .is_ok_and(|addr| addr.contains("unix:abstract="))
    {
        tracing::warn!(
            "Sandbox session_bus is Allow, but the host session bus is an abstract socket, \
             which only a sandbox on the host network can reach; with network Off or \
             Allowlist, libsecret-using tools will fail."
        );
    }
    let Some(bus_path) = bus_path else {
        tracing::warn!(
            "Sandbox session_bus is Allow but no path-based D-Bus session bus socket \
             was found on the host (neither parsed from DBUS_SESSION_BUS_ADDRESS nor \
             at $XDG_RUNTIME_DIR/bus). libsecret-using tools will fail; the toggle is \
             a no-op here."
        );
        if !command.profile.env.names_session_bus() {
            args.extend([
                os("--setenv"),
                os("DBUS_SESSION_BUS_ADDRESS"),
                os(SESSION_BUS_OFF_ADDRESS),
            ]);
        }
        return None;
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
    Some(bus_path)
}

/// Keyring only: the proxy's socket where the session bus is, and the
/// address that names it, whatever the host's address says besides.
fn append_bus_proxy_bind(args: &mut Vec<OsString>, proxy: &SessionBusProxy) -> PathBuf {
    for dir in private_parent_dirs_for(&proxy.bus) {
        args.extend([os("--dir"), dir.into_os_string()]);
    }
    let mut address = OsString::from("unix:path=");
    address.push(&proxy.bus);
    args.extend([
        os("--ro-bind"),
        os(&proxy.socket),
        os(&proxy.bus),
        os("--setenv"),
        os("DBUS_SESSION_BUS_ADDRESS"),
        address,
    ]);
    proxy.bus.clone()
}

/// Locate the host's D-Bus session bus socket on the filesystem. Returns
/// None if the bus is abstract-socket-only (no file to bind) or simply
/// absent on this host.
pub(super) fn resolve_session_bus_socket(in_flatpak: bool) -> Option<PathBuf> {
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
    use super::filesystem::FilesystemOperation;
    for operation in command.profile.filesystem.operations() {
        match operation {
            FilesystemOperation::Expose {
                path,
                access,
                required,
            } => append_bind(args, access, path, path, !required),
            FilesystemOperation::Mask(path) => {
                args.push(os("--tmpfs"));
                args.push(path.as_os_str().to_owned());
            }
            FilesystemOperation::Protect(path) => {
                append_bind(args, SandboxPathAccess::ReadOnly, path, path, false);
            }
        }
    }
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
    use crate::assistant::sandbox::{
        built_helper, AllowedSocket, SandboxEnv, SandboxPathGrant, SandboxProfile, SocketPolicy,
    };

    fn sample_command() -> SandboxCommand {
        let workspace = std::env::temp_dir();
        SandboxCommand {
            argv: vec![os("/bin/sh"), os("-lc"), os("pwd")],
            cwd: workspace.clone(),
            timeout_ms: 1_000,
            max_output_chars: 1_000,
            profile: SandboxProfile::for_test(
                workspace.clone(),
                vec![],
                SandboxNetworkMode::Host,
                SandboxSessionBusMode::Deny,
                SandboxEnv::filtered_from_iter(
                    [("PATH", "/usr/bin:/bin")],
                    &workspace,
                    SandboxSessionBusMode::Deny,
                ),
                None,
            ),
        }
    }

    #[tokio::test]
    async fn an_agent_command_is_recorded_until_it_is_reaped() {
        use crate::assistant::network::spawn_roots::StartedBy;
        let ana = || Some("Ana, in linux_bwrap".to_string());
        let by = StartedBy::Agent(ana());
        let mut sleep = Command::new("sleep");
        sleep.arg("3");
        prepare_stdio(&mut sleep);
        let child = sleep.spawn().unwrap();
        let pid = child.id().unwrap();
        let command = SandboxCommand {
            timeout_ms: 10_000,
            ..sample_command()
        };
        let run = spawn_roots::as_agent(ana(), run_recorded(child, command));
        let running = async {
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            spawn_roots::started_by(&by)
        };
        let (output, running) = tokio::join!(run, running);
        assert_eq!(output.unwrap().exit_code, Some(0));
        assert_eq!(running, [pid], "while the command runs");
        assert!(spawn_roots::started_by(&by).is_empty(), "gone once reaped");
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
        let (program, args) = launch_argv(bwrap_args, true);
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
        command.profile.filesystem.workspace_root = workspace.clone();
        command.profile.env = SandboxEnv::filtered_from_iter(
            [("PATH", "/usr/bin:/bin")],
            home.path(),
            SandboxSessionBusMode::Deny,
        );
        command.profile.filesystem.path_grants = vec![SandboxPathGrant {
            host_path: home.path().to_path_buf(),
            access: SandboxPathAccess::ReadOnly,
        }];

        command.profile.filesystem = super::super::filesystem::EffectiveFilesystemPolicy::compile(
            &command.profile.filesystem.workspace_root,
            &command.profile.filesystem.path_grants,
            command.profile.env.home().map(Path::new),
        )
        .unwrap();
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
    fn clai_state_is_bound_read_only_after_its_write_roots_and_memory_after_it() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().canonicalize().unwrap();
        let app = home.join(".clai");
        let workspace = app.join("workspaces").join("ws");
        let state = workspace.join(".clai");
        std::fs::create_dir_all(state.join("memory")).unwrap();
        std::fs::write(app.join("config.json"), "{}").unwrap();
        let mut command = sample_command();
        command.cwd = workspace.clone();
        command.profile.filesystem =
            super::super::filesystem::EffectiveFilesystemPolicy::compile_in(
                &workspace,
                &[SandboxPathGrant {
                    host_path: home.clone(),
                    access: SandboxPathAccess::ReadWrite,
                }],
                Some(&home),
                Some(&app),
            )
            .unwrap();
        let rendered: Vec<String> = bwrap_args(&command)
            .unwrap()
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        let bind = |flag: &str, path: &Path| {
            let path = path.to_string_lossy();
            rendered
                .windows(3)
                .position(|w| w[0] == flag && w[1] == path && w[2] == path)
                .unwrap_or_else(|| panic!("expected {flag} {path}; got {rendered:?}"))
        };
        assert!(bind("--bind-try", &home) < bind("--ro-bind", &app));
        assert!(bind("--ro-bind", &app) < bind("--bind", &workspace));
        assert!(bind("--bind", &workspace) < bind("--ro-bind", &state));
        assert!(bind("--ro-bind", &state) < bind("--bind-try", &state.join("memory")));
    }

    #[test]
    fn agents_are_masked_and_only_the_members_craft_bound_inside() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().canonicalize().unwrap();
        let app = home.join(".clai");
        let workspace = app.join("workspaces").join("ws");
        let agents = app.join("agents");
        let own = agents.join("me/craft");
        for dir in [
            workspace.join(".clai/memory"),
            own.clone(),
            agents.join("other/craft"),
        ] {
            std::fs::create_dir_all(dir).unwrap();
        }
        let mut command = sample_command();
        command.cwd = workspace.clone();
        command.profile.filesystem =
            super::super::filesystem::EffectiveFilesystemPolicy::compile_with(
                &workspace,
                &[SandboxPathGrant {
                    host_path: home.clone(),
                    access: SandboxPathAccess::ReadWrite,
                }],
                Some(&home),
                Some(&app),
                Some(&own),
            )
            .unwrap();
        let rendered: Vec<String> = bwrap_args(&command)
            .unwrap()
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        let at = |args: &[&str]| {
            rendered
                .windows(args.len())
                .position(|w| w == args)
                .unwrap_or_else(|| panic!("expected {args:?}; got {rendered:?}"))
        };
        let (app, agents, own, state) = (
            app.display().to_string(),
            agents.display().to_string(),
            own.display().to_string(),
            workspace.join(".clai").display().to_string(),
        );
        assert!(at(&["--ro-bind", &app, &app]) < at(&["--tmpfs", &agents]));
        assert!(at(&["--tmpfs", &agents]) < at(&["--bind-try", &own, &own]));
        let memory = format!("{state}/memory");
        assert!(!rendered.contains(&memory), "{rendered:?}");
        assert_eq!(
            rendered
                .iter()
                .filter(|arg| arg.starts_with(&agents))
                .count(),
            3,
            "{rendered:?}"
        );
    }

    #[test]
    fn an_explicit_container_grant_is_re_exposed_over_the_mask() {
        // A grant on the workspace container itself ties with the container
        // tmpfs on depth. The grant must be emitted last, so everything the
        // user explicitly granted inside the container stays reachable.
        //
        // This is what makes `tools::local`'s grant fold safe: given
        // `~/.clai/workspaces` RW plus a sibling workspace RO, the fold drops
        // the sibling as already-covered. If the tmpfs erased the container
        // bind, that sibling would be silently revoked — and session grants go
        // through the same fold, so re-approving it could not win it back.
        let home = tempfile::tempdir().unwrap();
        let container = home.path().join(".clai").join("workspaces");
        let workspace = container.join("ws-abc");
        let sibling = container.join("ws-other");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&sibling).unwrap();

        let mut command = sample_command();
        command.cwd = workspace.clone();
        command.profile.filesystem.workspace_root = workspace.clone();
        command.profile.env = SandboxEnv::filtered_from_iter(
            [("PATH", "/usr/bin:/bin")],
            home.path(),
            SandboxSessionBusMode::Deny,
        );
        let home_str = home.path().to_string_lossy().into_owned();
        let container_str = container.to_string_lossy().into_owned();

        // Both access levels: a read-only container grant is just as explicit,
        // and just as revoked if the mask lands on top of it.
        for (access, flag) in [
            (SandboxPathAccess::ReadWrite, "--bind-try"),
            (SandboxPathAccess::ReadOnly, "--ro-bind-try"),
        ] {
            command.profile.filesystem.path_grants = vec![
                SandboxPathGrant {
                    host_path: home.path().to_path_buf(),
                    access: SandboxPathAccess::ReadOnly,
                },
                SandboxPathGrant {
                    host_path: container.clone(),
                    access,
                },
            ];

            command.profile.filesystem =
                super::super::filesystem::EffectiveFilesystemPolicy::compile(
                    &command.profile.filesystem.workspace_root,
                    &command.profile.filesystem.path_grants,
                    command.profile.env.home().map(Path::new),
                )
                .unwrap();
            let args = bwrap_args(&command).unwrap();
            let rendered: Vec<String> = args
                .iter()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect();

            let home_bind_idx = rendered
                .windows(3)
                .position(|w| w[0] == "--ro-bind-try" && w[1] == home_str && w[2] == home_str)
                .unwrap_or_else(|| panic!("home should be bound read-only; got {rendered:?}"));
            let tmpfs_idx = rendered
                .windows(2)
                .position(|w| w[0] == "--tmpfs" && w[1] == container_str)
                .unwrap_or_else(|| {
                    panic!("container should be masked with --tmpfs; got {rendered:?}")
                });
            let container_bind_idx = rendered
                .windows(3)
                .position(|w| w[0] == flag && w[1] == container_str && w[2] == container_str)
                .unwrap_or_else(|| panic!("granted container should be bound; got {rendered:?}"));

            assert!(
                home_bind_idx < tmpfs_idx,
                "the broad home bind must stay under the mask; got {rendered:?}"
            );
            assert!(
                tmpfs_idx < container_bind_idx,
                "the {flag} container grant must be bound on top of the mask; got {rendered:?}"
            );
        }
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

    /// Full on a host with no path-based bus: nothing is bound, and a
    /// command whose env names no bus gets the off address instead of an
    /// empty one, from which godbus would autolaunch a bus.
    #[test]
    fn full_without_a_host_bus_gets_the_off_address() {
        let runtime = tempfile::tempdir().unwrap();
        let mut command = sample_command();
        command.profile.session_bus = SandboxSessionBusMode::Allow;
        command.profile.env = SandboxEnv::filtered_from_iter(
            [("PATH", "/usr/bin")],
            &command.cwd,
            SandboxSessionBusMode::Allow,
        );
        let args = rendered(&with_dbus_env(None, Some(runtime.path()), || {
            bwrap_args(&command).unwrap()
        }));
        let setenvs: Vec<&[String]> = args
            .windows(3)
            .filter(|w| w[0] == "--setenv" && w[1] == "DBUS_SESSION_BUS_ADDRESS")
            .collect();
        assert_eq!(
            setenvs,
            [&[
                "--setenv",
                "DBUS_SESSION_BUS_ADDRESS",
                SESSION_BUS_OFF_ADDRESS
            ][..]],
            "{args:?}"
        );
        let off_path = SESSION_BUS_OFF_ADDRESS.strip_prefix("unix:path=").unwrap();
        assert!(!args.iter().any(|arg| arg == off_path), "{args:?}");

        // An empty or `autolaunch:` host address names no bus either; the
        // off address comes after it and replaces it.
        for host in ["", "autolaunch:"] {
            command.profile.env = SandboxEnv::filtered_from_iter(
                [("DBUS_SESSION_BUS_ADDRESS", host)],
                &command.cwd,
                SandboxSessionBusMode::Allow,
            );
            let args = rendered(&with_dbus_env(Some(host), Some(runtime.path()), || {
                bwrap_args(&command).unwrap()
            }));
            let last = args
                .windows(3)
                .rfind(|w| w[0] == "--setenv" && w[1] == "DBUS_SESSION_BUS_ADDRESS")
                .map(|w| w[2].as_str());
            assert_eq!(last, Some(SESSION_BUS_OFF_ADDRESS), "{host:?}: {args:?}");
        }

        // An address the host does give, such as an abstract bus that a
        // sandbox on the host network reaches, is left alone.
        for host in [
            "unix:abstract=/tmp/dbus-x",
            "autolaunch:;unix:abstract=/tmp/dbus-x",
        ] {
            command.profile.env = SandboxEnv::filtered_from_iter(
                [("DBUS_SESSION_BUS_ADDRESS", host)],
                &command.cwd,
                SandboxSessionBusMode::Allow,
            );
            let args = rendered(&with_dbus_env(Some(host), Some(runtime.path()), || {
                bwrap_args(&command).unwrap()
            }));
            assert!(
                !args.iter().any(|arg| arg == SESSION_BUS_OFF_ADDRESS),
                "{host:?}: {args:?}"
            );
        }
    }

    #[test]
    fn keyring_only_binds_the_proxy_at_the_bus_path_and_not_the_bus() {
        let temp = tempfile::tempdir().unwrap();
        let bus = temp.path().join("bus");
        let host_bus = temp.path().join("host-bus");
        std::fs::write(&host_bus, "").unwrap();
        let mut command = sample_command();
        command.profile.session_bus = SandboxSessionBusMode::Allow;
        command.profile.sockets.helper = Some(PathBuf::from("/opt/h"));
        command.profile.session_bus_proxy = Some(SessionBusProxy {
            socket: PathBuf::from("/c/.clai-net/bus-1.sock"),
            bus: bus.clone(),
        });
        let args = rendered(&with_dbus_env(
            Some(&format!("unix:path={}", host_bus.display())),
            None,
            || bwrap_args(&command).unwrap(),
        ));
        let bus = bus.to_string_lossy().into_owned();
        assert!(
            args.windows(3)
                .any(|w| w == ["--ro-bind", "/c/.clai-net/bus-1.sock", bus.as_str()]),
            "{args:?}"
        );
        let address = format!("unix:path={bus}");
        assert!(
            args.windows(3)
                .any(|w| w == ["--setenv", "DBUS_SESSION_BUS_ADDRESS", address.as_str()]),
            "{args:?}"
        );
        assert!(!args.iter().any(|arg| arg.contains("host-bus")), "{args:?}");
        let start = args
            .windows(2)
            .position(|w| w == ["--", BOX_HELPER])
            .unwrap_or_else(|| panic!("{args:?}"));
        assert_eq!(
            args[start + 2..start + 6],
            ["run", "--host-abstract-unix", "--allow-unix", bus.as_str()]
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

    fn proxied(dir: &Path) -> SandboxNetworkMode {
        SandboxNetworkMode::Proxied(crate::assistant::sandbox::ProxyEndpoint {
            socket: dir.join("net.sock"),
            helper: dir.join("clai-sandbox-helper"),
            ssh_config: dir.join("ssh_config"),
            forward_ports: Vec::new(),
        })
    }

    fn forwarding(dir: &Path, ports: &[u16]) -> SandboxNetworkMode {
        let mut network = proxied(dir);
        if let SandboxNetworkMode::Proxied(endpoint) = &mut network {
            endpoint.forward_ports = ports.to_vec();
        }
        network
    }

    #[test]
    fn only_allowlist_binds_the_proxy_and_wraps_the_command() {
        let dir = PathBuf::from("/c/.clai-net");
        for network in [
            SandboxNetworkMode::Disabled,
            SandboxNetworkMode::Host,
            proxied(&dir),
        ] {
            let mut command = sample_command();
            command.profile.network = network.clone();
            let args = rendered(&bwrap_args(&command).unwrap());
            let is_proxied = matches!(network, SandboxNetworkMode::Proxied(_));
            assert_eq!(
                args.contains(&"--share-net".to_string()),
                network == SandboxNetworkMode::Host,
                "{network:?}"
            );
            assert_eq!(
                args.contains(&"--unshare-net".to_string()),
                network != SandboxNetworkMode::Host,
                "{network:?}"
            );
            // A daemonised command stays under bwrap, so local_listener
            // still finds CLAI among its ancestors.
            assert!(args.contains(&"--unshare-pid".to_string()), "{network:?}");
            let has = |needle: &[&str]| args.windows(needle.len()).any(|window| window == needle);
            let binds: [&[&str]; 3] = [
                &["--bind", "/c/.clai-net/net.sock", "/run/clai/net.sock"],
                &[
                    "--ro-bind",
                    "/c/.clai-net/clai-sandbox-helper",
                    "/run/clai/helper",
                ],
                &[
                    "--ro-bind",
                    "/c/.clai-net/ssh_config",
                    "/etc/ssh/ssh_config",
                ],
            ];
            let env: [&[&str]; 8] = [
                &["--setenv", "HTTP_PROXY", "http://127.0.0.1:3128"],
                &["--setenv", "HTTPS_PROXY", "http://127.0.0.1:3128"],
                &["--setenv", "http_proxy", "http://127.0.0.1:3128"],
                &["--setenv", "https_proxy", "http://127.0.0.1:3128"],
                &["--setenv", "NO_PROXY", "localhost,127.0.0.1,::1"],
                &["--setenv", "no_proxy", "localhost,127.0.0.1,::1"],
                &["--setenv", "NODE_USE_ENV_PROXY", "1"],
                &[
                    "--setenv",
                    "JAVA_TOOL_OPTIONS",
                    "-Dhttp.proxyHost=127.0.0.1 -Dhttp.proxyPort=3128 \
                     -Dhttps.proxyHost=127.0.0.1 -Dhttps.proxyPort=3128 \
                     -Dhttp.nonProxyHosts=localhost|127.0.0.1",
                ],
            ];
            for needle in binds.iter().chain(env.iter()) {
                assert_eq!(has(needle), is_proxied, "{network:?} {needle:?}");
            }
            let wrapped = [
                "--",
                "/run/clai/helper",
                "bridge",
                "--socket",
                "/run/clai/net.sock",
                "--http",
                "3128",
                "--",
                "/bin/sh",
                "-lc",
                "pwd",
            ];
            assert_eq!(
                args.ends_with(&wrapped.map(String::from)),
                is_proxied,
                "{network:?}"
            );
            if is_proxied {
                let position = |needle: &str| args.iter().rposition(|arg| arg == needle).unwrap();
                assert!(
                    position("/etc/ssh/ssh_config") > position("/etc/ssh"),
                    "the ssh_config bind lands on the /etc/ssh tmpfs"
                );
            }
        }
    }

    #[test]
    fn every_mode_starts_the_command_through_the_helper_which_allows_the_bus() {
        let temp = tempfile::tempdir().unwrap();
        let bus = temp.path().join("bus");
        std::fs::write(&bus, "").unwrap();
        let bus = bus.to_string_lossy().into_owned();
        for network in [
            SandboxNetworkMode::Disabled,
            SandboxNetworkMode::Host,
            forwarding(Path::new("/c/.clai-net"), &[5432, 6379]),
        ] {
            let mut command = sample_command();
            command.profile.network = network.clone();
            command.profile.session_bus = SandboxSessionBusMode::Allow;
            command.profile.sockets.helper = Some(PathBuf::from("/opt/clai/clai-sandbox-helper"));
            let args = rendered(&with_dbus_env(
                Some(&format!("unix:path={bus}")),
                None,
                || bwrap_args(&command).unwrap(),
            ));
            let start = args
                .windows(2)
                .position(|w| w == ["--", "/run/clai/helper"])
                .unwrap_or_else(|| panic!("{network:?}: {args:?}"));
            let entry: Vec<&str> = args[start + 2..].iter().map(String::as_str).collect();
            let expected: Vec<&str> = match network {
                SandboxNetworkMode::Proxied(_) => vec![
                    "bridge",
                    "--socket",
                    "/run/clai/net.sock",
                    "--http",
                    "3128",
                    "--forward",
                    "5432",
                    "--forward",
                    "6379",
                    "--allow-unix",
                    &bus,
                    "--",
                    "/bin/sh",
                    "-lc",
                    "pwd",
                ],
                SandboxNetworkMode::Host => vec![
                    "run",
                    "--host-abstract-unix",
                    "--allow-unix",
                    &bus,
                    "--",
                    "/bin/sh",
                    "-lc",
                    "pwd",
                ],
                SandboxNetworkMode::Disabled => {
                    vec!["run", "--allow-unix", &bus, "--", "/bin/sh", "-lc", "pwd"]
                }
            };
            assert_eq!(entry, expected, "{network:?}");
            assert_eq!(
                entry.contains(&"--forward"),
                matches!(network, SandboxNetworkMode::Proxied(_)),
                "only Allowlist forwards local ports: {network:?}"
            );
            let helper_binds: Vec<&[String]> = args
                .windows(3)
                .filter(|w| w[0] == "--ro-bind" && w[2] == "/run/clai/helper")
                .collect();
            assert_eq!(helper_binds.len(), 1, "{network:?}: {args:?}");
        }
    }

    /// A home with gpg and lima data next to their sockets, as on a desktop.
    fn home_with_socket_dirs() -> (
        tempfile::TempDir,
        PathBuf,
        Vec<std::os::unix::net::UnixListener>,
    ) {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().canonicalize().unwrap();
        for dir in [
            ".gnupg",
            ".lima/default/sock",
            ".config",
            ".clai/workspaces/ws",
        ] {
            std::fs::create_dir_all(home.join(dir)).unwrap();
        }
        std::fs::write(home.join(".gnupg/pubring.kbx"), "keys").unwrap();
        let listeners = [
            ".gnupg/S.gpg-agent",
            ".lima/default/sock/ssh.sock",
            ".config/app.sock",
        ]
        .map(|socket| std::os::unix::net::UnixListener::bind(home.join(socket)).unwrap());
        (temp, home, listeners.into())
    }

    #[test]
    fn without_landlock_only_the_sockets_a_grant_exposes_are_hidden() {
        let (_temp, home, _listeners) = home_with_socket_dirs();
        let workspace = home.join(".clai/workspaces/ws");
        let mut command = sample_command();
        command.cwd = workspace.clone();
        let grant = |path: &Path| {
            super::super::filesystem::EffectiveFilesystemPolicy::compile(
                &workspace,
                &[SandboxPathGrant {
                    host_path: path.to_path_buf(),
                    access: SandboxPathAccess::ReadOnly,
                }],
                Some(&home),
            )
            .unwrap()
        };
        let masks = |command: &SandboxCommand, granted: &Path| {
            let args = rendered(&bwrap_args(command).unwrap());
            let bind = args
                .windows(2)
                .position(|w| w[0] == "--ro-bind-try" && Path::new(&w[1]) == granted)
                .unwrap();
            assert!(
                !args.windows(2).any(|w| w[0] == "--tmpfs"
                    && Path::new(&w[1]).starts_with(&home)
                    && !w[1].ends_with("workspaces")),
                "no directory is hidden: {args:?}"
            );
            let mut masks: Vec<String> = args
                .windows(3)
                .enumerate()
                .filter(|(_, w)| w[0] == "--ro-bind" && w[1] == "/dev/null")
                .inspect(|(index, _)| assert!(*index > bind, "masks follow the grant"))
                .map(|(_, w)| w[2].clone())
                .collect();
            masks.sort();
            masks
        };
        let socket = |path: &str| home.join(path).to_string_lossy().into_owned();
        for helper in [None, Some(PathBuf::from("/opt/h"))] {
            command.profile.sockets = SocketPolicy {
                helper,
                kernel_confines: false,
                allowed: Vec::new(),
                skipped: Vec::new(),
                hint_places: Vec::new(),
            };
            command.profile.filesystem = grant(&home);
            assert_eq!(
                masks(&command, &home),
                [
                    socket(".gnupg/S.gpg-agent"),
                    socket(".lima/default/sock/ssh.sock")
                ]
            );
            // A grant on the directory itself keeps its files, not its socket.
            command.profile.filesystem = grant(&home.join(".gnupg"));
            assert_eq!(
                masks(&command, &home.join(".gnupg")),
                [socket(".gnupg/S.gpg-agent")]
            );
        }
        command.profile.sockets.kernel_confines = true;
        assert_eq!(masks(&command, &home.join(".gnupg")), Vec::<String>::new());
    }

    /// M3: on a kernel without Landlock ABI 9 a granted `~/.gnupg` stays
    /// readable, so `gpg --verify` works, while its agent socket does not
    /// connect.
    #[tokio::test]
    async fn without_landlock_a_granted_socket_dir_stays_readable() {
        let (_temp, home, _listeners) = home_with_socket_dirs();
        let workspace = home.join(".clai/workspaces/ws");
        let gnupg = home.join(".gnupg");
        let mut profile = SandboxProfile::for_test(
            workspace.clone(),
            vec![SandboxPathGrant {
                host_path: gnupg.clone(),
                access: SandboxPathAccess::ReadOnly,
            }],
            SandboxNetworkMode::Disabled,
            SandboxSessionBusMode::Deny,
            SandboxEnv::filtered_from_iter(
                [("PATH", "/usr/bin:/bin")],
                &home,
                SandboxSessionBusMode::Deny,
            ),
            None,
        );
        profile.sockets.kernel_confines = false;
        let script = r#"import socket, sys
def reach(address):
    try:
        socket.socket(socket.AF_UNIX).connect(address)
        return "ok"
    except OSError as error:
        return type(error).__name__
print(open(sys.argv[1] + "/pubring.kbx").read(), reach(sys.argv[1] + "/S.gpg-agent"))
"#;
        let command = SandboxCommand {
            argv: vec![os("python3"), os("-c"), os(script), os(&gnupg)],
            cwd: workspace,
            timeout_ms: 20_000,
            max_output_chars: 2_000,
            profile,
        };
        let Some(output) = run_or_skip(command).await else {
            return;
        };
        assert_eq!(
            output.stdout.trim(),
            "keys ConnectionRefusedError",
            "{output:?}"
        );
    }

    /// E2/E3: in Host network, with a read grant on a directory that holds a
    /// host socket, the confined command does not reach that socket, but its
    /// own sockets, host abstract sockets (Host network reaches the host's
    /// services anyway) and cross-directory renames work. Without the helper
    /// the host socket is reachable.
    #[tokio::test]
    async fn the_helper_confines_unix_sockets_to_those_made_in_the_sandbox() {
        let Some(helper) = built_helper() else {
            return;
        };
        if super::super::skip_below_landlock(9) {
            return;
        }
        let workspace = tempfile::tempdir().unwrap();
        let host_dir = tempfile::tempdir().unwrap();
        let host_socket = host_dir.path().join("host.sock");
        let _host = std::os::unix::net::UnixListener::bind(&host_socket).unwrap();
        let name = format!("clai-test-{}", uuid::Uuid::new_v4().simple());
        let address = <std::os::unix::net::SocketAddr as std::os::linux::net::SocketAddrExt>::from_abstract_name(name.as_bytes()).unwrap();
        let _abstract = std::os::unix::net::UnixListener::bind_addr(&address).unwrap();
        let script = r#"import os, socket, sys
def reach(address):
    try:
        socket.socket(socket.AF_UNIX).connect(address)
        return "ok"
    except OSError as error:
        return type(error).__name__
own = socket.socket(socket.AF_UNIX)
own.bind("/tmp/own.sock")
own.listen(1)
os.makedirs("a")
os.makedirs("b")
open("a/f", "w").close()
os.rename("a/f", "b/f")
print(reach(sys.argv[1]), reach(chr(0) + sys.argv[2]), reach("/tmp/own.sock"))
"#;
        let run_with = |helper: Option<PathBuf>, workspace: &Path| {
            let mut profile = SandboxProfile::for_test(
                workspace.to_path_buf(),
                vec![SandboxPathGrant {
                    host_path: host_dir.path().to_path_buf(),
                    access: SandboxPathAccess::ReadOnly,
                }],
                SandboxNetworkMode::Host,
                SandboxSessionBusMode::Deny,
                SandboxEnv::filtered_from_iter(
                    [("PATH", "/usr/bin:/bin")],
                    workspace,
                    SandboxSessionBusMode::Deny,
                ),
                None,
            );
            profile.sockets = SocketPolicy {
                helper,
                kernel_confines: true,
                allowed: Vec::new(),
                skipped: Vec::new(),
                hint_places: Vec::new(),
            };
            SandboxCommand {
                argv: vec![
                    os("python3"),
                    os("-c"),
                    os(script),
                    os(&host_socket),
                    os(&name),
                ],
                cwd: workspace.to_path_buf(),
                timeout_ms: 20_000,
                max_output_chars: 2_000,
                profile,
            }
        };
        let Some(confined) = run_or_skip(run_with(Some(helper), workspace.path())).await else {
            return;
        };
        assert_eq!(
            confined.stdout.trim(),
            "PermissionError ok ok",
            "{confined:?}"
        );
        let open = tempfile::tempdir().unwrap();
        let unconfined = run_or_skip(run_with(None, open.path())).await.unwrap();
        assert_eq!(unconfined.stdout.trim(), "ok ok ok", "{unconfined:?}");
    }

    /// Without Landlock an exact grant on a socket in a usual socket place
    /// is not covered with `/dev/null`, unlike a socket next to it that only
    /// a directory grant shows, and the helper allows it.
    #[test]
    fn without_landlock_an_exact_socket_grant_is_not_masked() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().canonicalize().unwrap();
        let workspace = home.join(".clai/workspaces/ws");
        std::fs::create_dir_all(home.join(".docker/run")).unwrap();
        std::fs::create_dir_all(&workspace).unwrap();
        let socket = home.join(".docker/run/docker.sock");
        let other = home.join(".docker/run/other.sock");
        let _listeners =
            [&socket, &other].map(|path| std::os::unix::net::UnixListener::bind(path).unwrap());
        let mut command = sample_command();
        command.cwd = workspace.clone();
        let grants = [
            (&home, SandboxPathAccess::ReadOnly),
            (&socket, SandboxPathAccess::ReadOnly),
        ]
        .map(|(path, access)| SandboxPathGrant {
            host_path: path.clone(),
            access,
        });
        command.profile.filesystem = super::super::filesystem::EffectiveFilesystemPolicy::compile(
            &workspace,
            &grants,
            Some(&home),
        )
        .unwrap();
        let filesystem = &command.profile.filesystem;
        let (mut allowed, skipped) =
            super::super::granted_sockets(&[home.clone(), socket.clone()], &|path| {
                filesystem.covers(path, crate::config::FilesystemPathAccess::ReadWrite)
            });
        assert_eq!(
            allowed,
            [AllowedSocket {
                path: socket.clone(),
                host: socket.clone(),
            }]
        );
        // m4: a name under a system bind gets no bind of its own.
        allowed.push(AllowedSocket {
            path: PathBuf::from("/etc/app/app.sock"),
            host: PathBuf::from("/etc/app/app.sock"),
        });
        command.profile.sockets = SocketPolicy {
            helper: Some(PathBuf::from("/opt/h")),
            kernel_confines: false,
            allowed,
            skipped,
            hint_places: Vec::new(),
        };
        let args = rendered(&bwrap_args(&command).unwrap());
        assert!(
            !args.iter().any(|arg| arg.starts_with("/etc/app")
                && !args.windows(2).any(|w| w == ["--allow-unix", arg.as_str()])),
            "{args:?}"
        );
        let (socket, other) = (socket.to_string_lossy(), other.to_string_lossy());
        let masked = |path: &str| {
            args.windows(3)
                .any(|w| w == ["--ro-bind", "/dev/null", path])
        };
        assert!(masked(&other), "the ungranted socket is masked: {args:?}");
        assert!(!masked(&socket), "the granted socket is not: {args:?}");
        assert!(
            args.windows(3)
                .any(|w| w == ["--ro-bind-try", &*socket, &*socket]),
            "{args:?}"
        );
        let start = args
            .windows(2)
            .position(|w| w == ["--", BOX_HELPER])
            .unwrap_or_else(|| panic!("{args:?}"));
        assert_eq!(
            args[start + 2..start + 9],
            [
                "run",
                "--host-abstract-unix",
                "--allow-unix",
                &*socket,
                "--allow-unix",
                "/etc/app/app.sock",
                "--"
            ],
            "{args:?}"
        );
    }

    /// In every network mode `/var/run` is made a link to `/run` before
    /// anything is mounted under `/run` or `/var`, so nothing can make it a
    /// directory first (bwrap would refuse the link), and a socket granted
    /// by a name under it is bound once, at `/run`: no mount goes through the
    /// link. Without the link on the host, `/var/run` is bound as before.
    #[test]
    fn var_run_is_a_link_to_run_made_before_anything_under_it() {
        let modes = [
            SandboxNetworkMode::Disabled,
            SandboxNetworkMode::Host,
            proxied(Path::new("/c/.clai-net")),
        ];
        for network in modes {
            let mut command = sample_command();
            command.profile.network = network;
            command.profile.sockets.allowed = vec![AllowedSocket {
                path: PathBuf::from("/var/run/docker.sock"),
                host: PathBuf::from("/run/docker.sock"),
            }];
            let args = rendered(&bwrap_args_with(&command, true).unwrap());
            let link = args
                .windows(5)
                .position(|w| w == ["--dir", "/run", "--symlink", "../run", "/var/run"])
                .unwrap_or_else(|| panic!("{args:?}"));
            let early = args[..link]
                .iter()
                .find(|arg| arg.starts_with("/run") || arg.starts_with("/var"));
            assert_eq!(early, None, "{args:?}");
            assert!(
                !args.iter().any(|arg| arg.starts_with("/var/run/")),
                "{args:?}"
            );
            let bind = ["--ro-bind-try", "/run/docker.sock", "/run/docker.sock"];
            assert_eq!(
                args.windows(3).filter(|w| *w == bind).count(),
                1,
                "{args:?}"
            );

            let args = rendered(&bwrap_args_with(&command, false).unwrap());
            assert!(!args.iter().any(|arg| arg == "--symlink"), "{args:?}");
            for name in ["/run/docker.sock", "/var/run/docker.sock"] {
                assert!(
                    args.windows(3)
                        .any(|w| w == ["--ro-bind-try", "/run/docker.sock", name]),
                    "{args:?}"
                );
            }
        }
    }

    #[test]
    fn only_a_symlink_that_leads_to_run_counts_as_the_var_run_link() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let (run, var, other) = (root.join("run"), root.join("var"), root.join("other"));
        for dir in [&run, &var, &other] {
            std::fs::create_dir(dir).unwrap();
        }
        let link = |name: &str, target: &Path| {
            let path = var.join(name);
            std::os::unix::fs::symlink(target, &path).unwrap();
            path
        };
        assert!(is_link_to(&link("relative", Path::new("../run")), &run));
        assert!(is_link_to(&link("absolute", &run), &run));
        assert!(!is_link_to(&link("elsewhere", &other), &run));
        assert!(!is_link_to(&link("dangling", &root.join("gone")), &run));
        assert!(!is_link_to(&run, &run), "a directory is not the link");
        assert!(!is_link_to(&var.join("missing"), &run));
    }

    /// Connects to each socket it is given, then tries to remove, move and
    /// replace the first one, and connects to it again.
    const SWAP_SCRIPT: &str = r#"import socket, subprocess, sys
def reach(address):
    try:
        socket.socket(socket.AF_UNIX).connect(address)
        return "ok"
    except OSError as error:
        return type(error).__name__
granted, other, named, lone = sys.argv[1:]
out = [reach(granted), reach(other), reach(named), reach(lone)]
for argv in (["rm", "-f", granted], ["mv", granted, granted + ".moved"], ["ln", "-sf", "/dev/null", granted]):
    run = subprocess.run(argv, capture_output=True, text=True)
    busy = run.returncode != 0 and "busy" in run.stderr.lower()
    out.append(argv[0] + (":busy" if busy else f":{run.returncode}:{run.stderr.strip()}"))
out.append(reach(granted))
print(*out)
"#;

    /// Through the grants alone: a socket granted exactly connects, also by
    /// a name through a symlinked directory (as /var/run names /run), while
    /// a socket under a granted directory does not, and one outside every
    /// grant is not there. The granted name is a mount point in the sandbox,
    /// in a directory the sandbox can write (its private /tmp), so `rm`, `mv`
    /// and `ln -sf` on it fail with EBUSY and the socket stays reachable:
    /// what lets a read-write grant on a socket pass the redirect check.
    #[tokio::test]
    async fn an_exact_socket_grant_connects_and_cannot_be_swapped() {
        let Some(helper) = built_helper() else {
            return;
        };
        if super::super::skip_below_landlock(9) {
            return;
        }
        let workspace = tempfile::tempdir().unwrap();
        let shared = tempfile::tempdir().unwrap();
        let alone = tempfile::tempdir().unwrap();
        let hidden = tempfile::tempdir().unwrap();
        let path =
            |dir: &tempfile::TempDir, name: &str| dir.path().canonicalize().unwrap().join(name);
        let (granted, other, via, lone) = (
            path(&alone, "granted.sock"),
            path(&shared, "other.sock"),
            path(&hidden, "via.sock"),
            path(&hidden, "lone.sock"),
        );
        let _listeners = [&granted, &other, &via, &lone]
            .map(|socket| std::os::unix::net::UnixListener::bind(socket).unwrap());
        let links = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(hidden.path(), links.path().join("run")).unwrap();
        let named = links.path().join("run/via.sock");
        let grants = [
            (
                shared.path().canonicalize().unwrap(),
                SandboxPathAccess::ReadOnly,
            ),
            (granted.clone(), SandboxPathAccess::ReadWrite),
            (named.clone(), SandboxPathAccess::ReadOnly),
        ];
        let mut profile = SandboxProfile::for_test(
            workspace.path().to_path_buf(),
            grants
                .iter()
                .map(|(path, access)| SandboxPathGrant {
                    host_path: path.clone(),
                    access: *access,
                })
                .collect(),
            SandboxNetworkMode::Disabled,
            SandboxSessionBusMode::Deny,
            SandboxEnv::filtered_from_iter(
                [("PATH", "/usr/bin:/bin")],
                workspace.path(),
                SandboxSessionBusMode::Deny,
            ),
            None,
        );
        let names: Vec<PathBuf> = grants.iter().map(|(path, _)| path.clone()).collect();
        let (allowed, skipped) = super::super::granted_sockets(&names, &|path| {
            profile
                .filesystem
                .covers(path, crate::config::FilesystemPathAccess::ReadWrite)
        });
        assert_eq!(allowed.len(), 2, "{allowed:?} {skipped:?}");
        profile.sockets = SocketPolicy {
            helper: Some(helper),
            kernel_confines: true,
            allowed,
            skipped,
            hint_places: Vec::new(),
        };
        let command = SandboxCommand {
            argv: vec![
                os("python3"),
                os("-c"),
                os(SWAP_SCRIPT),
                os(&granted),
                os(&other),
                os(&named),
                os(&lone),
            ],
            cwd: workspace.path().to_path_buf(),
            timeout_ms: 20_000,
            max_output_chars: 2_000,
            profile,
        };
        let Some(output) = run_or_skip(command).await else {
            return;
        };
        assert_eq!(
            output.stdout.trim(),
            "ok PermissionError ok FileNotFoundError rm:busy mv:busy ln:busy ok",
            "{output:?}"
        );
        use std::os::unix::fs::FileTypeExt;
        assert!(std::fs::symlink_metadata(&granted)
            .unwrap()
            .file_type()
            .is_socket());
        assert!(!alone.path().join("granted.sock.moved").exists());
    }

    /// Connects to the socket it is given, then prints where `/var/run`
    /// leads and what it lists.
    const VAR_RUN_SCRIPT: &str = r#"import os, socket, sys
socket.socket(socket.AF_UNIX).connect(sys.argv[1])
print(os.readlink("/var/run"), *sorted(os.listdir("/var/run")))
"#;

    /// On a host with `/var/run` a link to `/run`: in the sandbox
    /// `/var/run` is the same link, it lists only what the mounts put under
    /// `/run`, and a socket granted by a name under it connects. bwrap
    /// refuses the link if a mount made `/var/run` a directory first.
    #[tokio::test]
    async fn var_run_names_run_and_reaches_a_socket_granted_there() {
        if !host_var_run_is_run() {
            crate::assistant::sandbox::skip_without_bwrap("the host has no /var/run link to /run");
            return;
        }
        let workspace = tempfile::tempdir().unwrap();
        let sockets = tempfile::tempdir().unwrap();
        let host = sockets.path().canonicalize().unwrap().join("s.sock");
        let _listener = std::os::unix::net::UnixListener::bind(&host).unwrap();
        let file = format!("clai-test-{}.sock", std::process::id());
        let name = Path::new(VAR_RUN).join(&file);
        let mut command = network_command(workspace.path(), SandboxNetworkMode::Disabled, "");
        command.argv = vec![os("python3"), os("-c"), os(VAR_RUN_SCRIPT), os(&name)];
        command.profile.sockets.allowed = vec![AllowedSocket { path: name, host }];
        let args = rendered(&bwrap_args(&command).unwrap());
        let end = args.iter().position(|arg| arg == "--").unwrap();
        let under_run: std::collections::BTreeSet<String> = args[..end]
            .iter()
            .filter_map(|arg| {
                let rest = Path::new(arg).strip_prefix("/run").ok()?;
                Some(
                    rest.components()
                        .next()?
                        .as_os_str()
                        .to_string_lossy()
                        .into_owned(),
                )
            })
            .collect();
        assert!(under_run.contains(&file), "{args:?}");
        let Some(output) = run_or_skip(command).await else {
            return;
        };
        let expected = std::iter::once("../run".to_string())
            .chain(under_run)
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(output.stdout.trim(), expected, "{output:?}");
    }

    /// Keyring only, end to end: through the proxy CLAI starts, a command in
    /// the sandbox reaches the keyring name and not another name on the bus.
    #[tokio::test]
    async fn keyring_only_reaches_the_keyring_and_no_other_bus_name() {
        use super::super::bus_proxy::{test_bus::TestBus, BusProxy};
        let Some(helper) = built_helper() else {
            return;
        };
        let Some(bus) = TestBus::start() else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let plan = SessionBusProxy {
            socket: dir.path().join("proxy.sock"),
            bus: bus.socket.clone(),
        };
        let _proxy = BusProxy::start(&plan).unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let mut profile = SandboxProfile::for_test(
            workspace.path().to_path_buf(),
            Vec::new(),
            SandboxNetworkMode::Disabled,
            SandboxSessionBusMode::Allow,
            SandboxEnv::filtered_from_iter(
                [("PATH", "/usr/bin:/bin")],
                workspace.path(),
                SandboxSessionBusMode::Allow,
            ),
            None,
        );
        profile.sockets = SocketPolicy {
            helper: Some(helper),
            kernel_confines: super::super::landlock_abi() >= 9,
            allowed: Vec::new(),
            skipped: Vec::new(),
            hint_places: Vec::new(),
        };
        profile.session_bus_proxy = Some(plan);
        let script = "for name in org.freedesktop.secrets org.example.Spawner; do \
                      gdbus call --session --dest \"$name\" --object-path /t \
                      --method org.example.T.Ping 2>&1 | head -c 200; echo; done";
        let command = SandboxCommand {
            argv: vec![os("/bin/sh"), os("-c"), os(script)],
            cwd: workspace.path().to_path_buf(),
            timeout_ms: 20_000,
            max_output_chars: 2_000,
            profile,
        };
        let Some(output) = run_or_skip(command).await else {
            return;
        };
        let lines: Vec<&str> = output
            .stdout
            .lines()
            .filter(|line| !line.is_empty())
            .collect();
        assert_eq!(lines.first(), Some(&"('pong',)"), "{output:?}");
        assert!(
            lines
                .get(1)
                .is_some_and(|line| line.contains("ServiceUnknown")),
            "{output:?}"
        );
    }

    fn network_command(
        workspace: &Path,
        network: SandboxNetworkMode,
        script: &str,
    ) -> SandboxCommand {
        SandboxCommand {
            argv: vec![os("/bin/sh"), os("-c"), os(script)],
            cwd: workspace.to_path_buf(),
            timeout_ms: 20_000,
            max_output_chars: 10_000,
            profile: SandboxProfile::for_test(
                workspace.to_path_buf(),
                vec![],
                network,
                SandboxSessionBusMode::Deny,
                SandboxEnv::filtered_from_iter(
                    [("PATH", "/usr/bin:/bin")],
                    workspace,
                    SandboxSessionBusMode::Deny,
                ),
                None,
            ),
        }
    }

    /// `None` when this host cannot run bwrap.
    async fn run_or_skip(command: SandboxCommand) -> Option<SandboxCommandOutput> {
        match run(command).await {
            Ok(output) => Some(output),
            Err(error) if error.contains("Sandboxed shell is unavailable") => {
                crate::assistant::sandbox::skip_without_bwrap(&error);
                None
            }
            Err(error) => panic!("sandbox command failed unexpectedly: {error}"),
        }
    }

    /// Sends every host the proxy allows to one loopback echo server.
    struct Echo(std::net::SocketAddr);

    #[async_trait::async_trait]
    impl crate::assistant::network::connect::Connector for Echo {
        async fn connect(
            &self,
            _host: &str,
            _port: u16,
        ) -> Result<tokio::net::TcpStream, crate::assistant::network::connect::ConnectError>
        {
            tokio::net::TcpStream::connect(self.0)
                .await
                .map_err(|error| {
                    crate::assistant::network::connect::ConnectError::Failed(error.to_string())
                })
        }
    }

    async fn echo_server() -> std::net::SocketAddr {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let mut buffer = [0_u8; 64];
                    let read = stream.read(&mut buffer).await.unwrap_or(0);
                    let _ = stream.write_all(&buffer[..read]).await;
                });
            }
        });
        addr
    }

    /// An Allowlist command with a running proxy session that allows
    /// `allowed.test` and sends it to an echo server.
    struct Allowlisted {
        network: SandboxNetworkMode,
        session: crate::assistant::network::proxy::ProxySession,
        _files: tempfile::TempDir,
    }

    async fn allowlisted() -> Option<Allowlisted> {
        allowlisted_with(
            Vec::new(),
            crate::assistant::network::local_listener::host_inspector(),
        )
        .await
    }

    /// [`allowlisted`], with the local ports of `grants` forwarded as a
    /// command run then would, and `inspector` saying what listens there.
    async fn allowlisted_with(
        grants: Vec<crate::config::types::LocalPortGrant>,
        inspector: std::sync::Arc<dyn crate::assistant::network::local_listener::ListenerInspector>,
    ) -> Option<Allowlisted> {
        let helper = built_helper()?;
        let files = tempfile::tempdir().unwrap();
        let mut endpoint = crate::assistant::sandbox::ProxyEndpoint {
            socket: files.path().join("net.sock"),
            helper,
            ssh_config: files.path().join("ssh_config"),
            forward_ports: Vec::new(),
        };
        std::fs::write(
            &endpoint.ssh_config,
            crate::assistant::sandbox::helper::ssh_config_text(),
        )
        .unwrap();
        let run_network =
            crate::assistant::network::run_state::RunNetwork::new(&crate::config::NetworkPolicy {
                mode: crate::config::NetworkMode::Allowlist,
                allow_domains: vec!["allowed.test".to_string()],
                deny_domains: Vec::new(),
                local_ports: grants,
            });
        endpoint.forward_ports = run_network.local_ports();
        let session = crate::assistant::network::proxy::ProxySession::start(
            &endpoint.socket,
            std::sync::Arc::new(run_network),
            std::sync::Arc::new(Echo(echo_server().await)),
            inspector,
        )
        .unwrap();
        Some(Allowlisted {
            network: SandboxNetworkMode::Proxied(endpoint),
            session,
            _files: files,
        })
    }

    #[tokio::test]
    async fn allowlist_reaches_only_listed_hosts_through_the_proxy() {
        let Some(box_network) = allowlisted().await else {
            return;
        };
        let workspace = tempfile::tempdir().unwrap();
        let script = "printf ping | /run/clai/helper connect --proxy 127.0.0.1:3128 allowed.test 443; echo; \
                      /run/clai/helper connect --proxy 127.0.0.1:3128 example.invalid 443 </dev/null; \
                      echo \"refused=$?\"; grep ProxyCommand /etc/ssh/ssh_config; \
                      echo \"$HTTPS_PROXY $NO_PROXY\"; getent hosts github.com || echo no-dns";
        let command = network_command(workspace.path(), box_network.network.clone(), script);
        let Some(output) = run_or_skip(command).await else {
            return;
        };
        assert!(output.stdout.starts_with("ping\n"), "{output:?}");
        assert!(output.stdout.contains("refused=255"), "{output:?}");
        assert!(
            output
                .stdout
                .contains("ProxyCommand /run/clai/helper connect"),
            "{output:?}"
        );
        assert!(
            output
                .stdout
                .contains("http://127.0.0.1:3128 localhost,127.0.0.1,::1"),
            "{output:?}"
        );
        assert!(output.stdout.contains("no-dns"), "{output:?}");
        assert!(output.stderr.contains("403 Forbidden"), "{output:?}");
        assert_eq!(
            box_network.session.refused().targets,
            ["example.invalid:443"]
        );
    }

    /// Says `SERVED` listens on every port of this computer.
    struct Served;

    const SERVED: &str = "/usr/bin/served";

    impl crate::assistant::network::local_listener::ListenerInspector for Served {
        fn inspect(
            &self,
            _port: u16,
            _dial: crate::assistant::network::local_listener::Loopback,
        ) -> std::io::Result<crate::assistant::network::local_listener::Listener> {
            Ok(
                crate::assistant::network::local_listener::Listener::Program(
                    crate::assistant::network::local_listener::Program {
                        exe: SERVED.to_string(),
                        pid: 1,
                        cmdline: SERVED.to_string(),
                        interpreter: false,
                        started_by: None,
                    },
                ),
            )
        }

        fn still_serves(
            &self,
            _port: u16,
            _dial: crate::assistant::network::local_listener::Loopback,
            _programs: &[String],
        ) -> bool {
            false
        }
    }

    fn port_grant(port: u16, program: &str) -> crate::config::types::LocalPortGrant {
        crate::config::types::LocalPortGrant::Program {
            port,
            program: program.to_string(),
        }
    }

    /// Connects to each port given on 127.0.0.1 and on ::1, sends `ping` and
    /// prints what comes back: the echo, `closed`, or `refused` when nothing
    /// listens.
    const LOOPBACK_CLIENT: &str = "import socket, sys
for port in map(int, sys.argv[1:]):
    for host in ('127.0.0.1', '::1'):
        try:
            with socket.create_connection((host, port), 5) as s:
                s.sendall(b'ping')
                print(port, host, s.recv(16).decode() or 'closed')
        except ConnectionRefusedError:
            print(port, host, 'refused')
        except ConnectionResetError:
            print(port, host, 'closed')
";

    /// A granted port answers on the sandbox's own 127.0.0.1 and [::1]
    /// through the proxy, which reaches the host listener; a port granted to
    /// another program is refused and named; nothing listens on the rest.
    #[tokio::test]
    async fn allowlist_forwards_the_granted_local_ports_into_the_box() {
        let granted = echo_server().await.port();
        let changed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let changed = changed.local_addr().unwrap().port();
        let ungranted = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let ungranted = ungranted.local_addr().unwrap().port();
        let grants = vec![
            port_grant(granted, SERVED),
            port_grant(changed, "/usr/bin/other"),
        ];
        let Some(box_network) = allowlisted_with(grants, std::sync::Arc::new(Served)).await else {
            return;
        };
        let workspace = tempfile::tempdir().unwrap();
        std::fs::write(workspace.path().join("client.py"), LOOPBACK_CLIENT).unwrap();
        let script = format!("python3 client.py {granted} {changed} {ungranted}");
        let command = network_command(workspace.path(), box_network.network.clone(), &script);
        let Some(output) = run_or_skip(command).await else {
            return;
        };
        let lines: Vec<&str> = output.stdout.lines().collect();
        assert_eq!(
            lines,
            [
                format!("{granted} 127.0.0.1 ping"),
                format!("{granted} ::1 ping"),
                format!("{changed} 127.0.0.1 closed"),
                format!("{changed} ::1 closed"),
                format!("{ungranted} 127.0.0.1 refused"),
                format!("{ungranted} ::1 refused"),
            ],
            "{output:?}"
        );
        assert_eq!(
            box_network.session.refused().targets,
            [format!("localhost:{changed}")]
        );
        assert!(
            output.stderr.contains("now served by `/usr/bin/served`"),
            "{output:?}"
        );
    }

    /// `NO_PROXY` keeps an agent's own test server reachable: the proxy
    /// would refuse `localhost`.
    #[tokio::test]
    async fn allowlist_lets_in_box_localhost_bypass_the_proxy() {
        let Some(box_network) = allowlisted().await else {
            return;
        };
        let workspace = tempfile::tempdir().unwrap();
        let script = "python3 -m http.server 8765 --bind 127.0.0.1 >/dev/null 2>&1 & \
            python3 -c \"
import time, urllib.request
for _ in range(100):
    try:
        print(urllib.request.urlopen('http://localhost:8765/', timeout=2).status)
        break
    except Exception as error:
        last = error
        time.sleep(0.05)
else:
    print('failed', last)
\"; kill %1";
        let command = network_command(workspace.path(), box_network.network.clone(), script);
        let Some(output) = run_or_skip(command).await else {
            return;
        };
        assert_eq!(output.stdout.trim(), "200", "{output:?}");
        assert!(box_network.session.refused().is_empty());
    }

    #[tokio::test]
    async fn off_has_only_loopback_and_host_network_keeps_the_host_loopback() {
        let workspace = tempfile::tempdir().unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let script = format!(
            "grep -c : /proc/net/dev; grep -q \"^ *lo:\" /proc/net/dev && echo lo; \
             python3 -c \"import socket; socket.create_connection(('127.0.0.1', {port}), 2); print('reached')\" 2>/dev/null || echo unreachable"
        );
        let Some(off) = run_or_skip(network_command(
            workspace.path(),
            SandboxNetworkMode::Disabled,
            &script,
        ))
        .await
        else {
            return;
        };
        assert!(off.stdout.starts_with("1\nlo\n"), "{off:?}");
        assert!(off.stdout.contains("unreachable"), "{off:?}");

        let host = run_or_skip(network_command(
            workspace.path(),
            SandboxNetworkMode::Host,
            &script,
        ))
        .await
        .unwrap();
        assert!(host.stdout.contains("reached"), "{host:?}");
    }

    /// The runner's timeout kills bwrap; the helper and the command die with
    /// its namespace.
    #[tokio::test]
    async fn a_timed_out_allowlist_command_leaves_no_helper_behind() {
        let Some(box_network) = allowlisted().await else {
            return;
        };
        let workspace = tempfile::tempdir().unwrap();
        if run_or_skip(network_command(
            workspace.path(),
            SandboxNetworkMode::Disabled,
            "true",
        ))
        .await
        .is_none()
        {
            return;
        }
        let marker = format!("clai-timeout-{}", uuid::Uuid::new_v4().simple());
        let mut command = network_command(
            workspace.path(),
            box_network.network.clone(),
            &format!("sleep 60; echo {marker}"),
        );
        command.timeout_ms = 1_000;
        let error = run(command).await.unwrap_err();
        assert!(error.contains("timed out"), "{error}");
        let survivors = || {
            std::fs::read_dir("/proc")
                .unwrap()
                .flatten()
                .filter(|entry| {
                    std::fs::read(entry.path().join("cmdline"))
                        .is_ok_and(|cmdline| String::from_utf8_lossy(&cmdline).contains(&marker))
                })
                .count()
        };
        let mut left = survivors();
        for _ in 0..50 {
            if left == 0 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            left = survivors();
        }
        assert_eq!(left, 0, "processes outlived the timed-out command");
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
            profile: SandboxProfile::for_test(
                workspace.path().to_path_buf(),
                vec![],
                SandboxNetworkMode::Disabled,
                SandboxSessionBusMode::Deny,
                SandboxEnv::filtered_from_iter(
                    [("PATH", "/usr/bin:/bin")],
                    workspace.path(),
                    SandboxSessionBusMode::Deny,
                ),
                None,
            ),
        };

        let output = match run(command).await {
            Ok(output) => output,
            Err(error) if error.contains("Sandboxed shell is unavailable") => {
                crate::assistant::sandbox::skip_without_bwrap(&error);
                return;
            }
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
            profile: SandboxProfile::for_test(
                workspace.clone(),
                vec![SandboxPathGrant {
                    host_path: ancestor,
                    access: SandboxPathAccess::ReadOnly,
                }],
                SandboxNetworkMode::Host,
                SandboxSessionBusMode::Deny,
                SandboxEnv::filtered_from_iter(
                    [("PATH", "/usr/bin:/bin")],
                    &workspace,
                    SandboxSessionBusMode::Deny,
                ),
                None,
            ),
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
            profile: SandboxProfile::for_test(
                workspace.path().to_path_buf(),
                vec![SandboxPathGrant {
                    host_path: stale.clone(),
                    access: SandboxPathAccess::ReadOnly,
                }],
                SandboxNetworkMode::Host,
                SandboxSessionBusMode::Deny,
                SandboxEnv::filtered_from_iter(
                    [("PATH", "/usr/bin:/bin")],
                    workspace.path(),
                    SandboxSessionBusMode::Deny,
                ),
                None,
            ),
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
            profile: SandboxProfile::for_test(
                workspace.path().to_path_buf(),
                vec![
                    SandboxPathGrant {
                        host_path: vanished_path(),
                        access: SandboxPathAccess::ReadOnly,
                    },
                    live_grant.clone(),
                ],
                SandboxNetworkMode::Host,
                SandboxSessionBusMode::Deny,
                SandboxEnv::filtered_from_iter(
                    [("PATH", "/usr/bin:/bin")],
                    workspace.path(),
                    SandboxSessionBusMode::Deny,
                ),
                None,
            ),
        };

        prune_missing_grants(&mut command).await;

        assert_eq!(
            command.profile.filesystem.path_grants,
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
            profile: SandboxProfile::for_test(
                workspace.path().to_path_buf(),
                vec![SandboxPathGrant {
                    host_path: vanished_path(),
                    access: SandboxPathAccess::ReadOnly,
                }],
                SandboxNetworkMode::Disabled,
                SandboxSessionBusMode::Deny,
                SandboxEnv::filtered_from_iter(
                    [("PATH", "/usr/bin:/bin")],
                    workspace.path(),
                    SandboxSessionBusMode::Deny,
                ),
                None,
            ),
        };

        let output = match run(command).await {
            Ok(output) => output,
            Err(error) if error.contains("Sandboxed shell is unavailable") => {
                crate::assistant::sandbox::skip_without_bwrap(&error);
                return;
            }
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
            profile: SandboxProfile::for_test(
                workspace.clone(),
                vec![SandboxPathGrant {
                    host_path: ancestor.path().to_path_buf(),
                    access: SandboxPathAccess::ReadOnly,
                }],
                SandboxNetworkMode::Disabled,
                SandboxSessionBusMode::Deny,
                SandboxEnv::filtered_from_iter(
                    [("PATH", "/usr/bin:/bin")],
                    &workspace,
                    SandboxSessionBusMode::Deny,
                ),
                None,
            ),
        };

        let output = match run(command).await {
            Ok(output) => output,
            Err(error) if error.contains("Sandboxed shell is unavailable") => {
                crate::assistant::sandbox::skip_without_bwrap(&error);
                return;
            }
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

    fn tmp_profile(workspace: &Path, scratch: Option<PathBuf>) -> SandboxCommand {
        tmp_profile_with_grants(workspace, scratch, vec![])
    }

    fn tmp_profile_with_grants(
        workspace: &Path,
        scratch: Option<PathBuf>,
        path_grants: Vec<SandboxPathGrant>,
    ) -> SandboxCommand {
        SandboxCommand {
            argv: vec![os("/bin/sh"), os("-lc"), os("pwd")],
            cwd: workspace.to_path_buf(),
            timeout_ms: 1_000,
            max_output_chars: 1_000,
            profile: SandboxProfile::for_test(
                workspace.to_path_buf(),
                path_grants,
                SandboxNetworkMode::Disabled,
                SandboxSessionBusMode::Deny,
                SandboxEnv::filtered_from_iter(
                    [("PATH", "/usr/bin:/bin")],
                    workspace,
                    SandboxSessionBusMode::Deny,
                ),
                scratch,
            ),
        }
    }

    fn rendered(args: &[OsString]) -> Vec<String> {
        args.iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    /// The persistence fix: with scratch space, `/tmp` is a bind of the
    /// workspace's scratch dir, so files survive between commands. A tmpfs
    /// here would silently restore the per-command wipe.
    #[test]
    fn tmp_is_bound_to_the_scratch_dir_when_available() {
        let workspace = PathBuf::from("/home/me/.clai/workspaces/ws");
        let scratch = PathBuf::from("/home/me/.cache/clai/sandbox-tmp/live/ws-0123456789abcdef");
        let command = tmp_profile(&workspace, Some(scratch.clone()));

        let mut args: Vec<OsString> = Vec::new();
        append_tmp_mount(&mut args, &command);

        // A tmpfs floor first so a vanished scratch dir degrades to an empty
        // private /tmp instead of aborting the command, then the lenient bind
        // on top (last-writer-wins).
        assert_eq!(
            rendered(&args),
            vec![
                "--tmpfs".to_string(),
                "/tmp".to_string(),
                "--bind-try".to_string(),
                scratch.display().to_string(),
                "/tmp".to_string()
            ]
        );
    }

    /// Without scratch space we must keep the original private tmpfs. Falling
    /// back to the *host* /tmp would leak sibling workspaces' scratch files
    /// and host IPC sockets into the sandbox.
    #[test]
    fn tmp_falls_back_to_a_private_tmpfs_without_scratch() {
        let workspace = PathBuf::from("/home/me/.clai/workspaces/ws");
        let command = tmp_profile(&workspace, None);

        let mut args: Vec<OsString> = Vec::new();
        append_tmp_mount(&mut args, &command);

        assert_eq!(
            rendered(&args),
            vec!["--tmpfs".to_string(), "/tmp".to_string()]
        );
    }

    #[test]
    fn tmp_env_points_at_the_sandbox_tmp_when_scratch_is_bound() {
        let workspace = PathBuf::from("/home/me/.clai/workspaces/ws");
        let command = tmp_profile(
            &workspace,
            Some(PathBuf::from("/home/me/.cache/clai/sandbox-tmp/live/ws-0")),
        );

        let mut args: Vec<OsString> = Vec::new();
        append_tmp_env(&mut args, &command);

        let rendered = rendered(&args);
        for key in ["TMPDIR", "TMP", "TEMP"] {
            let idx = rendered
                .iter()
                .position(|arg| arg == key)
                .unwrap_or_else(|| panic!("{key} not set"));
            assert_eq!(rendered[idx - 1], "--setenv");
            // The guest path, not the host scratch path: inside the sandbox
            // the scratch dir *is* /tmp.
            assert_eq!(rendered[idx + 1], "/tmp");
        }
    }

    #[test]
    fn tmp_env_is_not_set_without_scratch() {
        let workspace = PathBuf::from("/home/me/.clai/workspaces/ws");
        let command = tmp_profile(&workspace, None);

        let mut args: Vec<OsString> = Vec::new();
        append_tmp_env(&mut args, &command);

        assert!(
            args.is_empty(),
            "unexpected env args: {:?}",
            rendered(&args)
        );
    }

    /// The scratch bind is a default, not a policy: bwrap mounts are
    /// last-writer-wins, and grants are emitted after the base block, so a
    /// user who explicitly grants `/tmp` still gets the host's.
    #[test]
    fn an_explicit_tmp_grant_is_emitted_after_the_scratch_bind() {
        let workspace = tempfile::tempdir().unwrap();
        let command = tmp_profile_with_grants(
            workspace.path(),
            Some(PathBuf::from("/home/me/.cache/clai/sandbox-tmp/live/ws-0")),
            vec![SandboxPathGrant {
                host_path: PathBuf::from("/tmp"),
                access: SandboxPathAccess::ReadWrite,
            }],
        );

        let args = bwrap_args(&command).expect("args");
        let rendered = rendered(&args);

        let scratch_bind = rendered
            .iter()
            .position(|arg| arg.contains("sandbox-tmp/live/ws-0"))
            .expect("scratch bind missing");
        let grant_bind = rendered
            .iter()
            .enumerate()
            .position(|(i, arg)| arg == "/tmp" && i > scratch_bind && rendered[i - 1] == "/tmp")
            .expect("explicit /tmp grant bind missing");

        assert!(
            grant_bind > scratch_bind,
            "explicit /tmp grant must override the scratch bind: {rendered:?}"
        );
    }

    /// End-to-end proof of the user-visible fix: a file written to `/tmp` by
    /// one sandboxed command is still there for the next one. This is the
    /// behaviour the whole change exists to deliver (build caches surviving
    /// between `bash_exec` calls), so it is pinned against a real bwrap run
    /// rather than only against generated argv.
    #[tokio::test]
    async fn files_in_tmp_survive_between_commands_with_scratch() {
        let workspace = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();

        let write = tmp_e2e_command(
            workspace.path(),
            Some(scratch.path().to_path_buf()),
            "echo persisted > /tmp/canary",
        );
        match run(write).await {
            Ok(output) => assert!(output.success, "stderr: {}", output.stderr),
            Err(error) if error.contains("Sandboxed shell is unavailable") => {
                crate::assistant::sandbox::skip_without_bwrap(&error);
                return;
            }
            Err(error) => panic!("sandbox command failed unexpectedly: {error}"),
        }

        let read = tmp_e2e_command(
            workspace.path(),
            Some(scratch.path().to_path_buf()),
            "cat /tmp/canary",
        );
        let output = run(read).await.expect("second run");

        assert!(output.success, "stderr: {}", output.stderr);
        assert!(
            output.stdout.contains("persisted"),
            "/tmp did not persist between commands; stdout: {:?}",
            output.stdout
        );
    }

    /// Negative control for the test above: without scratch space `/tmp` is a
    /// per-command tmpfs, so the canary must NOT survive. Guards against the
    /// persistence test passing for the wrong reason (e.g. leaking the host's
    /// real /tmp into the sandbox).
    #[tokio::test]
    async fn files_in_tmp_do_not_survive_without_scratch() {
        let workspace = tempfile::tempdir().unwrap();

        let write = tmp_e2e_command(workspace.path(), None, "echo persisted > /tmp/canary");
        match run(write).await {
            Ok(output) => assert!(output.success, "stderr: {}", output.stderr),
            Err(error) if error.contains("Sandboxed shell is unavailable") => {
                crate::assistant::sandbox::skip_without_bwrap(&error);
                return;
            }
            Err(error) => panic!("sandbox command failed unexpectedly: {error}"),
        }

        let read = tmp_e2e_command(workspace.path(), None, "cat /tmp/canary 2>/dev/null; true");
        let output = run(read).await.expect("second run");

        assert!(
            !output.stdout.contains("persisted"),
            "a tmpfs /tmp unexpectedly persisted between commands; stdout: {:?}",
            output.stdout
        );
    }

    fn tmp_e2e_command(workspace: &Path, scratch: Option<PathBuf>, script: &str) -> SandboxCommand {
        SandboxCommand {
            argv: vec![os("/bin/sh"), os("-lc"), os(script)],
            cwd: workspace.to_path_buf(),
            timeout_ms: 10_000,
            max_output_chars: 1_000,
            profile: SandboxProfile::for_test(
                workspace.to_path_buf(),
                vec![],
                SandboxNetworkMode::Disabled,
                SandboxSessionBusMode::Deny,
                SandboxEnv::filtered_from_iter(
                    [("PATH", "/usr/bin:/bin")],
                    workspace,
                    SandboxSessionBusMode::Deny,
                ),
                scratch,
            ),
        }
    }
    /// H1: from inside the sandbox the agent cannot rewrite its workspace's
    /// policy or the app's config, with or without a write grant covering
    /// them, while the workspace and its memory stay writable.
    #[tokio::test]
    async fn clai_state_is_read_only_inside_the_sandbox_except_memory() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().canonicalize().unwrap();
        let app = home.join(".clai");
        let workspace = app.join("workspaces").join("ws");
        std::fs::create_dir_all(workspace.join(".clai/memory")).unwrap();
        std::fs::write(workspace.join(".clai/config.json"), "{}").unwrap();
        std::fs::write(app.join("config.json"), "{}").unwrap();
        let script = format!(
            "w() {{ if (echo pwned > \"$1\") 2>/dev/null; then echo \"wrote $1\"; \\
             else echo \"refused $1\"; fi; }}; \\
             w .clai/config.json; w .clai/new; w .clai/memory/x; w notes; \\
             w '{}'; mv .clai .clai-moved 2>/dev/null && echo moved",
            app.join("config.json").display()
        );
        let home_grant = SandboxPathGrant {
            host_path: home.clone(),
            access: SandboxPathAccess::ReadWrite,
        };
        for grants in [vec![], vec![home_grant]] {
            let mut command = SandboxCommand {
                argv: vec![os("/bin/sh"), os("-c"), os(&script)],
                cwd: workspace.clone(),
                timeout_ms: 10_000,
                max_output_chars: 2_000,
                profile: SandboxProfile::for_test(
                    workspace.clone(),
                    vec![],
                    SandboxNetworkMode::Disabled,
                    SandboxSessionBusMode::Deny,
                    SandboxEnv::filtered_from_iter(
                        [("PATH", "/usr/bin:/bin")],
                        &home,
                        SandboxSessionBusMode::Deny,
                    ),
                    None,
                ),
            };
            command.profile.filesystem =
                super::super::filesystem::EffectiveFilesystemPolicy::compile_in(
                    &workspace,
                    &grants,
                    Some(&home),
                    Some(&app),
                )
                .unwrap();
            let output = match run(command).await {
                Ok(output) => output,
                Err(e) if e.contains("Sandboxed shell is unavailable") => {
                    crate::assistant::sandbox::skip_without_bwrap(&e);
                    return;
                }
                Err(e) => panic!("sandbox command failed unexpectedly: {e}"),
            };
            let mut lines = output.stdout.lines();
            for expected in [
                "refused .clai/config.json",
                "refused .clai/new",
                "wrote .clai/memory/x",
                "wrote notes",
            ] {
                assert_eq!(lines.next(), Some(expected), "grants: {grants:?}");
            }
            // Without the grant the app config is not mounted: the write lands
            // in the sandbox's private /tmp, and the host file is checked below.
            if !grants.is_empty() {
                let refused = format!("refused {}", app.join("config.json").display());
                assert_eq!(lines.next(), Some(refused.as_str()));
            }
            assert!(!output.stdout.contains("moved"), "{}", output.stdout);
        }
        for config in [workspace.join(".clai/config.json"), app.join("config.json")] {
            assert_eq!(std::fs::read_to_string(config).unwrap(), "{}");
        }
        assert!(workspace.join(".clai").is_dir());
    }

    /// A member writes its own craft and nothing else of the agents', reads
    /// the workspace memory but cannot write it, and cannot move its craft
    /// aside; under any grant.
    #[tokio::test]
    async fn only_the_members_own_craft_is_reachable_inside_the_sandbox() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().canonicalize().unwrap();
        let app = home.join(".clai");
        let workspace = app.join("workspaces").join("ws");
        let own = app.join("agents/me/craft");
        let other = app.join("agents/other/craft");
        for dir in [workspace.join(".clai/memory"), own.clone(), other.clone()] {
            std::fs::create_dir_all(dir).unwrap();
        }
        std::fs::write(other.join("secret.md"), "secret").unwrap();
        std::fs::write(workspace.join(".clai/memory/state.md"), "state").unwrap();
        let script = format!(
            "w() {{ if (echo pwned > \"$1\") 2>/dev/null; then echo \"wrote $1\"; \\
             else echo \"refused $1\"; fi; }}; \\
             r() {{ if cat \"$1\" >/dev/null 2>&1; then echo \"read $1\"; \\
             else echo \"hidden $1\"; fi; }}; \\
             w '{own}/lesson.md'; r '{other}/secret.md'; w '{other}/secret.md'; \\
             r .clai/memory/state.md; w .clai/memory/state.md; \\
             mv '{own}' '{own}-moved' 2>/dev/null && echo moved",
            own = own.display(),
            other = other.display(),
        );
        let grant = |path: &Path, access| SandboxPathGrant {
            host_path: path.to_path_buf(),
            access,
        };
        for grants in [
            vec![],
            vec![grant(&home, SandboxPathAccess::ReadOnly)],
            vec![grant(&home, SandboxPathAccess::ReadWrite)],
        ] {
            let mut command = SandboxCommand {
                argv: vec![os("/bin/sh"), os("-c"), os(&script)],
                cwd: workspace.clone(),
                timeout_ms: 10_000,
                max_output_chars: 2_000,
                profile: SandboxProfile::for_test(
                    workspace.clone(),
                    vec![],
                    SandboxNetworkMode::Disabled,
                    SandboxSessionBusMode::Deny,
                    SandboxEnv::filtered_from_iter(
                        [("PATH", "/usr/bin:/bin")],
                        &home,
                        SandboxSessionBusMode::Deny,
                    ),
                    None,
                ),
            };
            command.profile.filesystem =
                super::super::filesystem::EffectiveFilesystemPolicy::compile_with(
                    &workspace,
                    &grants,
                    Some(&home),
                    Some(&app),
                    Some(&own),
                )
                .unwrap();
            let output = match run(command).await {
                Ok(output) => output,
                Err(e) if e.contains("Sandboxed shell is unavailable") => {
                    crate::assistant::sandbox::skip_without_bwrap(&e);
                    return;
                }
                Err(e) => panic!("sandbox command failed unexpectedly: {e}"),
            };
            let expected = format!(
                "wrote {own}/lesson.md\nhidden {other}/secret.md\nrefused {other}/secret.md\n\
                 read .clai/memory/state.md\nrefused .clai/memory/state.md",
                own = own.display(),
                other = other.display(),
            );
            assert_eq!(output.stdout.trim_end(), expected, "grants: {grants:?}");
        }
        assert_eq!(
            std::fs::read_to_string(own.join("lesson.md")).unwrap(),
            "pwned\n"
        );
        assert_eq!(
            std::fs::read_to_string(other.join("secret.md")).unwrap(),
            "secret"
        );
        assert_eq!(
            std::fs::read_to_string(workspace.join(".clai/memory/state.md")).unwrap(),
            "state"
        );
    }

    /// E1: a command cannot plant project configuration for the provider
    /// CLIs, which run on the host in the workspace, nor move it aside.
    #[tokio::test]
    async fn cli_project_config_is_read_only_inside_the_sandbox() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().canonicalize().unwrap();
        let app = home.join(".clai");
        let workspace = app.join("workspaces").join("ws");
        std::fs::create_dir_all(workspace.join(".clai/memory")).unwrap();
        super::super::filesystem::ensure_cli_project_config_dirs(&workspace);
        std::fs::write(workspace.join(".mcp.json"), "{}").unwrap();
        let script = "w() { if (echo pwned > \"$1\") 2>/dev/null; then echo \"wrote $1\"; \
                      else echo \"refused $1\"; fi; }; \
                      w .claude/settings.json; w .codex/config.toml; w .opencode/plugin.ts; \
                      w .mcp.json; w notes; \
                      mv .claude .claude-moved 2>/dev/null && echo moved; \
                      rmdir .codex 2>/dev/null && echo removed";
        let mut command = tmp_e2e_command(&workspace, None, script);
        command.profile.filesystem =
            super::super::filesystem::EffectiveFilesystemPolicy::compile_in(
                &workspace,
                &[],
                Some(&home),
                Some(&app),
            )
            .unwrap();
        let Some(output) = run_or_skip(command).await else {
            return;
        };
        assert_eq!(
            output.stdout.lines().collect::<Vec<_>>(),
            [
                "refused .claude/settings.json",
                "refused .codex/config.toml",
                "refused .opencode/plugin.ts",
                "refused .mcp.json",
                "wrote notes",
            ],
            "{output:?}"
        );
        assert_eq!(
            std::fs::read_to_string(workspace.join(".mcp.json")).unwrap(),
            "{}"
        );
        assert!(!workspace.join(".claude/settings.json").exists());
    }

    /// The command sees only stdin, stdout and stderr, even when this process
    /// holds a descriptor without close-on-exec, and in Allowlist the
    /// helper's proxy and forwarded port listeners stay in the helper.
    #[tokio::test]
    async fn the_command_inherits_only_stdio() {
        const SCRIPT: &str = "ls /proc/$$/fd; :";
        let workspace = tempfile::tempdir().unwrap();
        let file = std::fs::File::open(workspace.path()).unwrap();
        // SAFETY: dup returns a new descriptor without close-on-exec.
        let leaked = unsafe { libc::dup(std::os::fd::AsRawFd::as_raw_fd(&file)) };
        assert!(leaked > 2);
        let off = run_or_skip(tmp_e2e_command(workspace.path(), None, SCRIPT)).await;
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = port.local_addr().unwrap().port();
        let grants = vec![port_grant(port, SERVED)];
        let box_network = allowlisted_with(grants, std::sync::Arc::new(Served)).await;
        let allowlist = match &box_network {
            Some(box_network) => {
                let network = box_network.network.clone();
                assert!(
                    matches!(&network, SandboxNetworkMode::Proxied(e) if e.forward_ports == [port]),
                    "{network:?}"
                );
                run_or_skip(network_command(workspace.path(), network, SCRIPT)).await
            }
            None => None,
        };
        // SAFETY: closes the descriptor dup returned above.
        unsafe { libc::close(leaked) };
        for output in [off, allowlist].into_iter().flatten() {
            let fds: Vec<&str> = output.stdout.split_whitespace().collect();
            assert_eq!(fds, ["0", "1", "2"], "{output:?}");
        }
    }

    /// Runs `script` in `workspace` with `grants`, the app directory at `app`.
    /// `None` when bubblewrap is unavailable and the test may skip.
    async fn run_with_clai_home(
        workspace: &Path,
        home: &Path,
        app: &Path,
        grants: &[SandboxPathGrant],
        script: &str,
    ) -> Option<SandboxCommandOutput> {
        let mut command = SandboxCommand {
            argv: vec![os("/bin/sh"), os("-c"), os(script)],
            cwd: workspace.to_path_buf(),
            timeout_ms: 10_000,
            max_output_chars: 2_000,
            profile: SandboxProfile::for_test(
                workspace.to_path_buf(),
                vec![],
                SandboxNetworkMode::Disabled,
                SandboxSessionBusMode::Deny,
                SandboxEnv::filtered_from_iter(
                    [("PATH", "/usr/bin:/bin")],
                    home,
                    SandboxSessionBusMode::Deny,
                ),
                None,
            ),
        };
        command.profile.filesystem =
            super::super::filesystem::EffectiveFilesystemPolicy::compile_in(
                workspace,
                grants,
                Some(home),
                Some(app),
            )
            .unwrap();
        match run(command).await {
            Ok(output) => Some(output),
            Err(e) if e.contains("Sandboxed shell is unavailable") => {
                crate::assistant::sandbox::skip_without_bwrap(&e);
                None
            }
            Err(e) => panic!("sandbox command failed unexpectedly: {e}"),
        }
    }

    const WRITE_PROBE: &str =
        "w() { if (echo pwned > \"$1\") 2>/dev/null; then echo \"wrote $1\"; \
                               else echo \"refused $1\"; fi; }; ";

    /// A read-write grant on home: the app directory cannot be renamed away,
    /// gains no new entries, stays read-only while the app saves its config
    /// by renaming a new file over it, and state reached through a symlink in
    /// it is read-only where it leads.
    #[tokio::test]
    async fn clai_home_holds_under_a_home_grant_through_renames_new_entries_and_saves() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().canonicalize().unwrap();
        let app = home.join(".clai");
        let workspace = app.join("workspaces").join("ws");
        let linked = home.join("dotcache");
        std::fs::create_dir_all(workspace.join(".clai/memory")).unwrap();
        std::fs::create_dir_all(&linked).unwrap();
        std::os::unix::fs::symlink(&linked, app.join("cache")).unwrap();
        let config = app.join("config.json");
        std::fs::write(&config, "{}").unwrap();
        let script = format!(
            "{WRITE_PROBE} mv {app} {app}-moved 2>/dev/null && echo moved; \
             mkdir {app}/skills 2>/dev/null && echo created; touch started; sleep 1; \
             w {app}/config.json; w {linked}/x; w notes",
            app = app.display(),
            linked = linked.display(),
        );
        // The app saves its config as it does in `ConfigManager::save`, while
        // the command runs.
        let started = workspace.join("started");
        let saver = {
            let config = config.clone();
            std::thread::spawn(move || {
                for _ in 0..100 {
                    if started.exists() {
                        let saved = config.with_extension("json.tmp");
                        std::fs::write(&saved, "{\"saved\":1}").unwrap();
                        std::fs::rename(&saved, &config).unwrap();
                        return;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
            })
        };
        let grants = [SandboxPathGrant {
            host_path: home.clone(),
            access: SandboxPathAccess::ReadWrite,
        }];
        let Some(output) = run_with_clai_home(&workspace, &home, &app, &grants, &script).await
        else {
            return;
        };
        saver.join().unwrap();
        let expected = format!(
            "refused {}\nrefused {}\nwrote notes",
            config.display(),
            linked.join("x").display()
        );
        assert_eq!(output.stdout.trim(), expected, "stderr: {}", output.stderr);
        assert_eq!(std::fs::read_to_string(&config).unwrap(), "{\"saved\":1}");
        assert!(!app.join("skills").exists());
        assert!(!home.join(".clai-moved").exists());
        assert!(!linked.join("x").exists());
    }

    /// A read-write grant on the workspace directory: a sibling workspace can
    /// be written but not renamed away, its `.clai` stays read-only, and no
    /// new workspace can be planted next to it.
    #[tokio::test]
    async fn sibling_state_holds_under_a_workspace_directory_grant() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().canonicalize().unwrap();
        let app = home.join(".clai");
        let container = app.join("workspaces");
        let workspace = container.join("ws");
        let sibling = container.join("other");
        std::fs::create_dir_all(workspace.join(".clai/memory")).unwrap();
        std::fs::create_dir_all(sibling.join(".clai")).unwrap();
        let config = sibling.join(".clai/config.json");
        std::fs::write(&config, "{}").unwrap();
        let script = format!(
            "{WRITE_PROBE} mv {sibling} {sibling}-moved 2>/dev/null && echo moved; \
             mkdir {container}/planted 2>/dev/null && echo planted; \
             w {sibling}/notes; w {config}",
            sibling = sibling.display(),
            container = container.display(),
            config = config.display(),
        );
        let grants = [SandboxPathGrant {
            host_path: container.clone(),
            access: SandboxPathAccess::ReadWrite,
        }];
        let Some(output) = run_with_clai_home(&workspace, &home, &app, &grants, &script).await
        else {
            return;
        };
        let expected = format!(
            "wrote {}\nrefused {}",
            sibling.join("notes").display(),
            config.display()
        );
        assert_eq!(output.stdout.trim(), expected, "stderr: {}", output.stderr);
        assert_eq!(std::fs::read_to_string(&config).unwrap(), "{}");
        assert!(!container.join("planted").exists());
        assert!(!container.join("other-moved").exists());
    }

    /// A symlinked `.clai/memory` must not stop the command from starting;
    /// writes through it land where it points, with the access there.
    #[tokio::test]
    async fn a_symlinked_memory_still_launches() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().canonicalize().unwrap();
        let app = home.join(".clai");
        let workspace = app.join("workspaces").join("ws");
        std::fs::create_dir_all(workspace.join(".clai")).unwrap();
        std::fs::create_dir_all(workspace.join("notes")).unwrap();
        std::os::unix::fs::symlink("../notes", workspace.join(".clai/memory")).unwrap();
        let script = format!("{WRITE_PROBE} w .clai/config.json; w .clai/memory/x");
        let Some(output) = run_with_clai_home(&workspace, &home, &app, &[], &script).await else {
            return;
        };
        assert_eq!(
            output.stdout.trim(),
            "refused .clai/config.json\nwrote .clai/memory/x",
            "stderr: {}",
            output.stderr
        );
        assert!(workspace.join("notes/x").exists());
    }

    /// Isolation regression. Scratch lives at `<container>/.scratch/<id>`,
    /// inside the workspace container that `workspace_mask` already hides, so
    /// a broad `$HOME` read-only grant must NOT expose another workspace's
    /// scratch. An earlier revision placed scratch under the OS cache dir,
    /// where the default `$HOME` grant re-exposed every workspace's temp files
    /// at their real path; this pins that it cannot come back.
    #[tokio::test]
    async fn scratch_of_other_workspaces_is_not_readable_via_home_grant() {
        let home = tempfile::tempdir().unwrap();
        let container = home.path().join(".clai").join("workspaces");
        let workspace = container.join("ws-mine");
        std::fs::create_dir_all(&workspace).unwrap();

        let scratch = container.join(".scratch");
        let mine = scratch.join("mine-0000000000000000");
        let theirs = scratch.join("theirs-1111111111111111");
        std::fs::create_dir_all(&mine).unwrap();
        std::fs::create_dir_all(&theirs).unwrap();
        let secret = theirs.join("secret.txt");
        std::fs::write(&secret, "OTHER-WORKSPACE-SECRET").unwrap();

        let script = format!("cat '{}' 2>/dev/null || echo BLOCKED", secret.display());
        let command = SandboxCommand {
            argv: vec![os("/bin/sh"), os("-lc"), os(script)],
            cwd: workspace.clone(),
            timeout_ms: 10_000,
            max_output_chars: 1_000,
            profile: SandboxProfile::for_test(
                workspace.clone(),
                vec![SandboxPathGrant {
                    host_path: home.path().to_path_buf(),
                    access: SandboxPathAccess::ReadOnly,
                }],
                SandboxNetworkMode::Disabled,
                SandboxSessionBusMode::Deny,
                SandboxEnv::filtered_from_iter(
                    [("PATH", "/usr/bin:/bin")],
                    home.path(),
                    SandboxSessionBusMode::Deny,
                ),
                Some(mine.clone()),
            ),
        };

        let output = match run(command).await {
            Ok(o) => o,
            Err(e) if e.contains("Sandboxed shell is unavailable") => {
                crate::assistant::sandbox::skip_without_bwrap(&e);
                return;
            }
            Err(e) => panic!("sandbox command failed unexpectedly: {e}"),
        };
        assert!(
            !output.stdout.contains("OTHER-WORKSPACE-SECRET"),
            "another workspace's scratch was readable via the $HOME grant; stdout: {:?}",
            output.stdout
        );
        assert!(
            output.stdout.contains("BLOCKED"),
            "expected the read to be denied; stdout: {:?}",
            output.stdout
        );
    }

    /// The mask must not cost the agent its OWN scratch: it is reached at
    /// /tmp, a separate mount point, even though its host path sits inside the
    /// hidden container.
    #[tokio::test]
    async fn own_scratch_is_still_usable_at_tmp_under_a_home_grant() {
        let home = tempfile::tempdir().unwrap();
        let container = home.path().join(".clai").join("workspaces");
        let workspace = container.join("ws-mine");
        std::fs::create_dir_all(&workspace).unwrap();
        let mine = container.join(".scratch").join("mine-0000000000000000");
        std::fs::create_dir_all(&mine).unwrap();

        let command = SandboxCommand {
            argv: vec![
                os("/bin/sh"),
                os("-lc"),
                os("echo works > /tmp/canary && cat /tmp/canary"),
            ],
            cwd: workspace.clone(),
            timeout_ms: 10_000,
            max_output_chars: 1_000,
            profile: SandboxProfile::for_test(
                workspace.clone(),
                vec![SandboxPathGrant {
                    host_path: home.path().to_path_buf(),
                    access: SandboxPathAccess::ReadOnly,
                }],
                SandboxNetworkMode::Disabled,
                SandboxSessionBusMode::Deny,
                SandboxEnv::filtered_from_iter(
                    [("PATH", "/usr/bin:/bin")],
                    home.path(),
                    SandboxSessionBusMode::Deny,
                ),
                Some(mine.clone()),
            ),
        };

        let output = match run(command).await {
            Ok(o) => o,
            Err(e) if e.contains("Sandboxed shell is unavailable") => {
                crate::assistant::sandbox::skip_without_bwrap(&e);
                return;
            }
            Err(e) => panic!("sandbox command failed unexpectedly: {e}"),
        };
        assert!(output.success, "stderr: {}", output.stderr);
        assert!(
            output.stdout.contains("works"),
            "stdout: {:?}",
            output.stdout
        );
        // And it really landed in the workspace's own scratch dir on the host.
        assert!(
            mine.join("canary").exists(),
            "write did not reach the host scratch dir"
        );
    }

    struct SymlinkFixture {
        _temp: tempfile::TempDir,
        home: PathBuf,
        workspace: PathBuf,
        sibling: PathBuf,
        readonly: PathBuf,
    }
    fn symlink_fixture(layout: &str) -> SymlinkFixture {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        let real_home = base.join("real-home");
        std::fs::create_dir_all(&real_home).unwrap();
        let home = if layout == "home" {
            let alias = base.join("home");
            symlink(&real_home, &alias).unwrap();
            alias
        } else {
            real_home.clone()
        };
        if layout == "ancestor" {
            std::fs::create_dir_all(real_home.join("state/clai")).unwrap();
            symlink(real_home.join("state/clai"), home.join(".clai")).unwrap();
        }
        if layout == "container" {
            std::fs::create_dir_all(home.join(".clai")).unwrap();
            std::fs::create_dir_all(real_home.join("store")).unwrap();
            symlink(real_home.join("store"), home.join(".clai/workspaces")).unwrap();
        }
        let container = home.join(".clai/workspaces");
        std::fs::create_dir_all(&container).unwrap();
        let workspace = container.join("own");
        if layout == "workspace" {
            std::fs::create_dir_all(real_home.join("projects/own")).unwrap();
            symlink(real_home.join("projects/own"), &workspace).unwrap();
        }
        std::fs::create_dir_all(workspace.join("docs")).unwrap();
        let sibling = container.join("other");
        std::fs::create_dir_all(&sibling).unwrap();
        std::fs::write(sibling.join("data.sqlite"), "SIBLING").unwrap();
        std::fs::write(real_home.join("marker"), "HOME").unwrap();
        let readonly = real_home.join("readonly");
        std::fs::create_dir_all(&readonly).unwrap();
        SymlinkFixture {
            _temp: temp,
            home,
            workspace,
            sibling,
            readonly,
        }
    }

    // Exercise the production compiler AND argument builder, not a handwritten
    // imitation of bwrap's mounts. Each layout failed a different old heuristic.
    #[tokio::test]
    async fn compiled_filesystem_contract_holds_across_symlink_layouts() {
        for layout in ["home", "ancestor", "container", "workspace"] {
            for broad_home in [false, true] {
                for expose_sibling in [false, true] {
                    check_symlink_contract(layout, broad_home, expose_sibling).await;
                }
            }
        }
    }

    const CONTRACT_SCRIPT: &str = r#"
                set -eu
                printf writable > probe
                if [ "$3" = yes ]; then test "$(cat "$1/data.sqlite")" = SIBLING; else test ! -r "$1/data.sqlite"; fi
                if [ "$4" = yes ]; then test "$(cat "$HOME/marker")" = HOME; fi
                if (printf forbidden > "$2/probe") 2>/dev/null; then exit 41; fi
                echo contract-ok
            "#;

    async fn check_symlink_contract(layout: &str, broad_home: bool, expose_sibling: bool) {
        use super::super::filesystem::EffectiveFilesystemPolicy;
        use crate::config::FilesystemPathAccess;
        let fixture = symlink_fixture(layout);
        let SymlinkFixture {
            ref home,
            ref workspace,
            ref sibling,
            ref readonly,
            ..
        } = fixture;
        let mut grants = vec![
            SandboxPathGrant {
                host_path: workspace.join("docs"),
                access: SandboxPathAccess::ReadOnly,
            },
            SandboxPathGrant {
                host_path: readonly.clone(),
                access: SandboxPathAccess::ReadOnly,
            },
        ];
        if broad_home {
            grants.push(SandboxPathGrant {
                host_path: home.clone(),
                access: SandboxPathAccess::ReadOnly,
            });
        }
        if expose_sibling {
            grants.push(SandboxPathGrant {
                host_path: sibling.clone(),
                access: SandboxPathAccess::ReadOnly,
            });
        }
        let filesystem =
            EffectiveFilesystemPolicy::compile(workspace, &grants, Some(home)).unwrap();
        let resolved_sibling = sibling.canonicalize().unwrap();
        assert!(filesystem.covers(
            &workspace.canonicalize().unwrap().join("docs"),
            FilesystemPathAccess::ReadWrite
        ));
        assert_eq!(
            filesystem.covers(&resolved_sibling, FilesystemPathAccess::ReadOnly),
            expose_sibling
        );
        assert!(!filesystem.covers(readonly, FilesystemPathAccess::ReadWrite));
        let cwd = filesystem
            .authorize_cwd(workspace.join("docs").to_str().unwrap())
            .unwrap();
        let env = SandboxEnv::filtered_from_iter(
            [("PATH", "/usr/bin:/bin")],
            filesystem.home.as_deref().unwrap(),
            SandboxSessionBusMode::Deny,
        );
        let command = SandboxCommand {
            argv: vec![
                os("/bin/sh"),
                os("-c"),
                os(CONTRACT_SCRIPT),
                os("contract"),
                resolved_sibling.into_os_string(),
                readonly.clone().into_os_string(),
                os(if expose_sibling { "yes" } else { "no" }),
                os(if broad_home { "yes" } else { "no" }),
            ],
            cwd,
            timeout_ms: 5_000,
            max_output_chars: 2_000,
            profile: SandboxProfile {
                filesystem,
                env,
                network: SandboxNetworkMode::Disabled,
                session_bus: SandboxSessionBusMode::Deny,
                scratch_tmp: None,
                sockets: Default::default(),
                session_bus_proxy: None,
                session_bus_unavailable: None,
            },
        };
        let output = match run(command).await {
            Ok(output) => output,
            Err(error) if error.contains("Sandboxed shell is unavailable") => {
                crate::assistant::sandbox::skip_without_bwrap(&error);
                return;
            }
            Err(error) => {
                panic!("{layout}, home={broad_home}, sibling={expose_sibling}: {error}")
            }
        };
        assert!(
            output.success,
            "{layout}, home={broad_home}, sibling={expose_sibling}: {}",
            output.stderr
        );
        assert!(output.stdout.contains("contract-ok"));
        assert_eq!(
            std::fs::read_to_string(workspace.join("docs/probe")).unwrap(),
            "writable"
        );
    }
}
