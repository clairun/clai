//! Where the sandbox helper (`src/bin/clai-sandbox-helper.rs`) is.
//!
//! It is installed next to the CLAI binary (deb, rpm, a development build).
//! Under Flatpak bwrap runs on the host, which sees the app's files under
//! the installation's `app-path`, not under `/app`.

use std::path::{Path, PathBuf};

const HELPER_NAME: &str = "clai-sandbox-helper";
const FLATPAK_INFO: &str = "/.flatpak-info";

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

fn is_runnable(path: &Path) -> bool {
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
    fn a_missing_helper_names_the_shell_unavailable() {
        assert!(
            not_found("x").starts_with("Sandboxed shell is unavailable: network helper not found")
        );
    }
}
