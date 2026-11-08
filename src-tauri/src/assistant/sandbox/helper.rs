//! The sandbox helper (`src/bin/clai-sandbox-helper.rs`) and the files an
//! Allowlist command binds next to it.
//!
//! The helper is installed next to the CLAI binary (deb, rpm, a development
//! build). Under Flatpak bwrap runs on the host, which sees the app's files
//! under the installation's `app-path`, not under `/app`.

use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};

use super::ProxyEndpoint;

const HELPER_NAME: &str = "clai-sandbox-helper";
const FLATPAK_INFO: &str = "/.flatpak-info";

/// Where the helper, the proxy socket and the helper's proxy port are
/// inside the sandbox.
pub(crate) const BOX_HELPER: &str = "/run/clai/helper";
pub(crate) const BOX_SOCKET: &str = "/run/clai/net.sock";
pub(crate) const BOX_PROXY_PORT: u16 = 3128;

/// Beside the workspace roots, inside the container every sandbox masks, so
/// no other command's sandbox can see, or connect to, a socket here.
const NETWORK_DIR: &str = ".clai-net";

/// The files one Allowlist command binds: a socket path of its own for its
/// proxy session, the helper, and the shared ssh configuration.
pub fn endpoint(workspace_root: &Path) -> Result<ProxyEndpoint, String> {
    let helper = locate()?;
    prepare_endpoint(workspace_root, &process_home(workspace_root), helper)
}

/// A socket path of its own for one command's session bus proxy: beside its
/// network proxy socket, or, when the workspace has no masked container
/// (it is directly in the home directory or outside it), in
/// `$XDG_RUNTIME_DIR/clai`, which no sandbox binds unless a grant names it.
/// `None` when neither directory can be made.
pub(crate) fn bus_socket(workspace_root: &Path) -> Option<PathBuf> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from);
    bus_socket_in(
        workspace_root,
        &process_home(workspace_root),
        runtime.as_deref(),
    )
}

fn bus_socket_in(workspace_root: &Path, home: &Path, runtime: Option<&Path>) -> Option<PathBuf> {
    let dir = match private_dir(workspace_root, home) {
        Some(Ok(dir)) => dir,
        _ => runtime_private_dir(runtime?)?,
    };
    sweep_stale_bus_sockets(&dir);
    let id = uuid::Uuid::new_v4().simple().to_string();
    Some(dir.join(format!("bus-{}.sock", &id[..12])))
}

/// `<runtime>/clai`, made private if missing. The runtime directory is the
/// user's own (mode 0700), so nobody else can plant a link there.
fn runtime_private_dir(runtime: &Path) -> Option<PathBuf> {
    if !runtime.is_absolute() {
        return None;
    }
    let dir = runtime.join("clai");
    match std::fs::DirBuilder::new().mode(0o700).create(&dir) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(_) => return None,
    }
    let metadata = std::fs::symlink_metadata(&dir).ok()?;
    metadata.is_dir().then_some(dir)
}

/// Removes the proxy sockets a CLAI that was killed left in `dir`, the first
/// time this process uses it: the proxies exit with CLAI, their sockets do
/// not.
fn sweep_stale_bus_sockets(dir: &Path) {
    use std::os::unix::fs::FileTypeExt;
    static SWEPT: std::sync::Mutex<Vec<PathBuf>> = std::sync::Mutex::new(Vec::new());
    let mut swept = SWEPT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if swept.iter().any(|known| known == dir) {
        return;
    }
    swept.push(dir.to_path_buf());
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with("bus-")
            && name.ends_with(".sock")
            && entry.file_type().is_ok_and(|kind| kind.is_socket())
        {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

fn process_home(workspace_root: &Path) -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace_root.to_path_buf())
}

/// [`NETWORK_DIR`] for the workspace, made private if missing; `None` when
/// the workspace is outside the home directory.
fn private_dir(workspace_root: &Path, home: &Path) -> Option<Result<PathBuf, String>> {
    let dir = super::profile::workspace_mask(workspace_root, Some(home))?.join(NETWORK_DIR);
    let made = std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir);
    Some(
        made.map(|()| dir.clone())
            .map_err(|error| format!("cannot prepare {}: {error}", dir.display())),
    )
}

fn prepare_endpoint(
    workspace_root: &Path,
    home: &Path,
    helper: PathBuf,
) -> Result<ProxyEndpoint, String> {
    let dir = private_dir(workspace_root, home)
        .ok_or_else(|| {
            "Sandboxed shell is unavailable: the Allowlist network mode needs the workspace inside \
         the home directory; choose Off or Host network in the agent's Local access settings."
                .to_string()
        })?
        .map_err(|error| format!("Sandboxed shell is unavailable: {error}"))?;
    let unavailable = |error: std::io::Error| {
        format!(
            "Sandboxed shell is unavailable: cannot prepare {}: {error}",
            dir.display()
        )
    };
    let ssh_config = dir.join("ssh_config");
    let id = uuid::Uuid::new_v4().simple().to_string();
    write_ssh_config(&ssh_config, &id).map_err(unavailable)?;
    Ok(ProxyEndpoint {
        socket: dir.join(format!("{}.sock", &id[..12])),
        helper,
        ssh_config,
        forward_ports: Vec::new(),
    })
}

/// Writes a new file and renames it over `path`, so a symlink planted at
/// `path` is replaced, never written through. Skipped when `path` is
/// already a regular file with the text.
fn write_ssh_config(path: &Path, id: &str) -> std::io::Result<()> {
    use std::io::Write;
    let content = ssh_config_text();
    let current = std::fs::symlink_metadata(path).ok();
    if current.is_some_and(|metadata| metadata.file_type().is_file())
        && std::fs::read_to_string(path).ok().as_deref() == Some(content.as_str())
    {
        return Ok(());
    }
    let temporary = path.with_file_name(format!("ssh_config.{id}.tmp"));
    let written = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .and_then(|mut file| file.write_all(content.as_bytes()))
        .and_then(|()| std::fs::rename(&temporary, path));
    if written.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    written
}

/// Bound at `/etc/ssh/ssh_config`: ssh, and git, scp and rsync over ssh,
/// reach hosts through the proxy. `~/.ssh/config` is read first, so a
/// `ProxyCommand` or `ProxyJump` there takes precedence.
pub(crate) fn ssh_config_text() -> String {
    format!(
        "# Written by CLAI. In the Allowlist network mode ssh reaches hosts through\n\
         # CLAI's network proxy.\n\
         Host *\n    ProxyCommand {BOX_HELPER} connect --proxy 127.0.0.1:{BOX_PROXY_PORT} %h %p\n"
    )
}

/// The helper as the sandbox's bwrap sees it, once this process has found
/// it runnable. Allowlist cannot run without it, so the error fails the
/// command closed.
pub fn locate() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|error| not_found(&error.to_string()))?;
    let dir = exe.parent().ok_or_else(|| not_found("no app directory"))?;
    let installed = dir.join(HELPER_NAME);
    if !is_runnable(&installed) {
        return Err(not_found(&installed.display().to_string()));
    }
    if !crate::providers::is_flatpak() {
        return Ok(installed);
    }
    let info =
        std::fs::read_to_string(FLATPAK_INFO).map_err(|error| not_found(&error.to_string()))?;
    let app_path =
        flatpak_app_path(&info).ok_or_else(|| not_found("no app-path in /.flatpak-info"))?;
    Ok(app_path.join("bin").join(HELPER_NAME))
}

fn not_found(detail: &str) -> String {
    format!("Sandboxed shell is unavailable: network helper not found ({detail})")
}

pub(super) fn is_runnable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

/// `app-path` of the `[Instance]` group: where the host keeps the app's
/// `/app`.
fn flatpak_app_path(info: &str) -> Option<PathBuf> {
    let mut in_instance = false;
    for line in info.lines().map(str::trim) {
        if line.starts_with('[') {
            in_instance = line == "[Instance]";
        } else if let Some(path) = line.strip_prefix("app-path=").filter(|_| in_instance) {
            return Some(PathBuf::from(path)).filter(|path| path.is_absolute());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_host_app_path_comes_from_the_instance_group() {
        let info = "[Application]\nname=run.clai.CLAI\napp-path=/wrong\n\n[Instance]\n\
                    instance-id=1\napp-path=/var/lib/flatpak/app/run.clai.CLAI/x86_64/stable/abc/files\n";
        assert_eq!(
            flatpak_app_path(info),
            Some(PathBuf::from(
                "/var/lib/flatpak/app/run.clai.CLAI/x86_64/stable/abc/files"
            ))
        );
        assert_eq!(flatpak_app_path("[Instance]\napp-path=relative\n"), None);
        assert_eq!(flatpak_app_path("[Application]\napp-path=/x\n"), None);
    }

    #[test]
    fn only_an_executable_file_is_runnable() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let helper = dir.path().join(HELPER_NAME);
        assert!(!is_runnable(&helper), "missing");
        std::fs::write(&helper, "").unwrap();
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(!is_runnable(&helper), "not executable");
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(is_runnable(&helper));
        assert!(!is_runnable(dir.path()), "a directory");
    }

    #[test]
    fn an_endpoint_lives_in_the_masked_container() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().unwrap();
        let workspace = home.path().join(".clai/workspaces/ws");
        std::fs::create_dir_all(&workspace).unwrap();
        let helper = PathBuf::from("/opt/clai/clai-sandbox-helper");

        let first = prepare_endpoint(&workspace, home.path(), helper.clone()).unwrap();
        let second = prepare_endpoint(&workspace, home.path(), helper.clone()).unwrap();
        let dir = home.path().join(".clai/workspaces/.clai-net");
        assert_eq!(first.socket.parent(), Some(dir.as_path()));
        assert_ne!(first.socket, second.socket, "one socket per command");
        assert_eq!(first.helper, helper);
        assert_eq!(first.ssh_config, dir.join("ssh_config"));
        assert_eq!(
            std::fs::read_to_string(&first.ssh_config).unwrap(),
            ssh_config_text()
        );
        assert_eq!(
            std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
            0o700
        );

        let outside = tempfile::tempdir().unwrap();
        let error = prepare_endpoint(outside.path(), home.path(), helper).unwrap_err();
        assert!(
            error.starts_with("Sandboxed shell is unavailable"),
            "{error}"
        );
    }

    #[test]
    fn a_symlink_at_the_ssh_config_is_replaced_not_written_through() {
        let home = tempfile::tempdir().unwrap();
        let workspace = home.path().join(".clai/workspaces/ws");
        let dir = home.path().join(".clai/workspaces/.clai-net");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        let victim = home.path().join("victim");
        std::fs::write(&victim, "keep me").unwrap();
        std::os::unix::fs::symlink(&victim, dir.join("ssh_config")).unwrap();

        let endpoint = prepare_endpoint(&workspace, home.path(), PathBuf::from("/h")).unwrap();
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep me");
        let metadata = std::fs::symlink_metadata(&endpoint.ssh_config).unwrap();
        assert!(metadata.file_type().is_file());
        assert_eq!(
            std::fs::read_to_string(&endpoint.ssh_config).unwrap(),
            ssh_config_text()
        );
        let left: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(left, ["ssh_config"], "no temporary file is left behind");

        // A link to a file with the same text is replaced as well.
        std::fs::write(&victim, ssh_config_text()).unwrap();
        std::fs::remove_file(&endpoint.ssh_config).unwrap();
        std::os::unix::fs::symlink(&victim, &endpoint.ssh_config).unwrap();
        prepare_endpoint(&workspace, home.path(), PathBuf::from("/h")).unwrap();
        let metadata = std::fs::symlink_metadata(&endpoint.ssh_config).unwrap();
        assert!(metadata.file_type().is_file());
    }

    #[test]
    fn ssh_goes_through_the_helper_and_the_proxy_port() {
        assert!(ssh_config_text().contains(
            "Host *\n    ProxyCommand /run/clai/helper connect --proxy 127.0.0.1:3128 %h %p\n"
        ));
    }

    #[test]
    fn a_bus_socket_without_a_masked_container_lives_in_the_runtime_dir() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().unwrap();
        let runtime = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let in_home = home.path().join("ws");
        let contained = home.path().join(".clai/workspaces/ws");
        std::fs::create_dir_all(&contained).unwrap();
        let private = runtime.path().join("clai");

        for workspace in [in_home.as_path(), outside.path()] {
            let socket = bus_socket_in(workspace, home.path(), Some(runtime.path())).unwrap();
            assert_eq!(socket.parent(), Some(private.as_path()), "{workspace:?}");
        }
        assert_eq!(
            std::fs::metadata(&private).unwrap().permissions().mode() & 0o777,
            0o700
        );
        let socket = bus_socket_in(&contained, home.path(), Some(runtime.path())).unwrap();
        assert_eq!(
            socket.parent(),
            Some(home.path().join(".clai/workspaces/.clai-net").as_path())
        );
        assert_eq!(bus_socket_in(outside.path(), home.path(), None), None);
        assert_eq!(
            bus_socket_in(outside.path(), home.path(), Some(Path::new("run"))),
            None
        );
    }

    #[test]
    fn stale_bus_sockets_are_removed_once_and_others_kept() {
        let runtime = tempfile::tempdir().unwrap();
        let dir = runtime.path().join("clai");
        std::fs::create_dir(&dir).unwrap();
        let stale = dir.join("bus-0123456789ab.sock");
        drop(std::os::unix::net::UnixListener::bind(&stale).unwrap());
        let other = dir.join("net.sock");
        drop(std::os::unix::net::UnixListener::bind(&other).unwrap());
        let workspace = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();

        bus_socket_in(workspace.path(), home.path(), Some(runtime.path())).unwrap();
        assert!(!stale.exists(), "a socket left by a killed CLAI is removed");
        assert!(other.exists(), "only bus proxy sockets are swept");

        let live = dir.join("bus-live00000000.sock");
        let _listener = std::os::unix::net::UnixListener::bind(&live).unwrap();
        bus_socket_in(workspace.path(), home.path(), Some(runtime.path())).unwrap();
        assert!(live.exists(), "a running proxy's socket is kept");
    }

    #[test]
    fn a_missing_helper_names_the_shell_unavailable() {
        assert!(
            not_found("x").starts_with("Sandboxed shell is unavailable: network helper not found")
        );
    }
}
