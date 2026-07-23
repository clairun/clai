#![cfg_attr(all(test, not(target_os = "macos")), allow(dead_code))]

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::process::Command;

use super::runner::{prepare_stdio, run_spawned_child};
use super::{
    SandboxCommand, SandboxCommandOutput, SandboxNetworkMode, SandboxPathAccess, SandboxPathGrant,
};

const SANDBOX_EXEC_BIN: &str = "/usr/bin/sandbox-exec";

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum PathFilter {
    Literal(String),
    Subpath(String),
}

pub async fn run(mut command: SandboxCommand) -> Result<SandboxCommandOutput, String> {
    validate_profile_paths(&command)?;
    prune_missing_grants(&mut command);

    let private_tmp = create_private_tmp_dir(&command.profile.workspace_root)?;
    let profile = match seatbelt_profile(&command, &private_tmp) {
        Ok(profile) => profile,
        Err(error) => {
            cleanup_private_tmp_dir(&private_tmp);
            return Err(error);
        }
    };

    let mut argv = command.argv.iter();
    let program = argv
        .next()
        .ok_or_else(|| "Sandbox command argv cannot be empty".to_string())?;
    let mut child_command = Command::new(SANDBOX_EXEC_BIN);
    child_command
        .arg("-p")
        .arg(profile)
        .arg(program)
        .args(argv)
        .current_dir(&command.cwd)
        .env_clear();
    for (key, value) in command.profile.env.iter() {
        child_command.env(key, value);
    }
    child_command
        .env("TMPDIR", &private_tmp)
        .env("TMP", &private_tmp)
        .env("TEMP", &private_tmp);
    prepare_stdio(&mut child_command);

    let child = match child_command.spawn() {
        Ok(child) => child,
        Err(error) => {
            cleanup_private_tmp_dir(&private_tmp);
            if error.kind() == std::io::ErrorKind::NotFound {
                return Err(
                    "Sandboxed shell is unavailable: macOS sandbox-exec is not installed"
                        .to_string(),
                );
            }
            return Err(format!("Failed to start sandboxed shell: {}", error));
        }
    };

    let result = run_spawned_child(
        child,
        command.cwd,
        command.timeout_ms,
        command.max_output_chars,
        "Sandboxed shell command",
    )
    .await;
    cleanup_private_tmp_dir(&private_tmp);

    let output = result?;
    if looks_like_sandbox_exec_setup_failure(&output) {
        return Err(classify_sandbox_exec_failure(&output.stderr));
    }

    Ok(output)
}

fn validate_profile_paths(command: &SandboxCommand) -> Result<(), String> {
    if !command.profile.workspace_root.is_dir() {
        return Err(format!(
            "Sandbox workspace does not exist or is not a directory: {}",
            command.profile.workspace_root.display()
        ));
    }

    Ok(())
}

/// Drop path grants whose host path no longer exists before building the
/// seatbelt profile. A stale grant (e.g. an `always`-scoped grant to a
/// directory the user later deleted) must degrade to "that path is simply
/// absent inside the sandbox" — the same ENOENT an unsandboxed shell would
/// hit — instead of aborting EVERY sandboxed command for the agent. Pruning
/// can only ever REMOVE access, never widen it. Mirrors
/// `linux_bwrap::prune_missing_grants` (which additionally has to probe the
/// host namespace under Flatpak; macOS has no such split, so a plain
/// in-process probe is correct).
fn prune_missing_grants(command: &mut SandboxCommand) {
    command.profile.path_grants.retain(|grant| {
        let exists = grant.host_path.exists();
        if !exists {
            tracing::warn!(
                "Skipping filesystem path grant whose path no longer exists: {} — the \
                 sandbox launches without it. Remove the stale grant from agent \
                 settings to silence this warning.",
                grant.host_path.display()
            );
        }
        exists
    });
}

fn create_private_tmp_dir(workspace_root: &Path) -> Result<PathBuf, String> {
    let base = workspace_root.join(".clai").join("tmp");
    fs::create_dir_all(&base).map_err(|error| {
        format!(
            "Failed to create sandbox temp directory {}: {}",
            base.display(),
            error
        )
    })?;

    for attempt in 0..100u32 {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default();
        let dir = base.join(format!(
            "sandbox-{}-{}-{}",
            std::process::id(),
            nanos,
            attempt
        ));
        match fs::create_dir(&dir) {
            Ok(()) => return Ok(dir),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(format!(
                    "Failed to create sandbox temp directory {}: {}",
                    dir.display(),
                    error
                ));
            }
        }
    }

    Err(format!(
        "Failed to create a unique sandbox temp directory under {}",
        base.display()
    ))
}

fn cleanup_private_tmp_dir(path: &Path) {
    if let Err(error) = fs::remove_dir_all(path) {
        tracing::warn!(
            "Failed to remove macOS sandbox temp directory {}: {}",
            path.display(),
            error
        );
    }
}

fn seatbelt_profile(command: &SandboxCommand, private_tmp: &Path) -> Result<String, String> {
    let mut read_filters = BTreeSet::new();
    let mut write_filters = BTreeSet::new();
    let mut metadata_filters = BTreeSet::new();

    add_default_filters(&mut read_filters, &mut write_filters, &mut metadata_filters)?;

    let workspace = canonicalize_existing(&command.profile.workspace_root)?;
    add_dir_filter(&mut read_filters, &mut metadata_filters, &workspace)?;
    add_dir_filter(&mut write_filters, &mut metadata_filters, &workspace)?;

    let tmp = canonicalize_existing(private_tmp)?;
    add_dir_filter(&mut read_filters, &mut metadata_filters, &tmp)?;
    add_dir_filter(&mut write_filters, &mut metadata_filters, &tmp)?;

    for grant in &command.profile.path_grants {
        add_grant_filters(
            &mut read_filters,
            &mut write_filters,
            &mut metadata_filters,
            grant,
        )?;
    }

    let mut profile = String::from(
        "(version 1)\n\
         (deny default)\n\
         (allow process*)\n\
         (allow signal (target same-sandbox))\n\
         (allow sysctl-read)\n",
    );
    append_mach_lookup_rule(&mut profile, command.profile.network);
    if matches!(command.profile.network, SandboxNetworkMode::Host) {
        profile.push_str("(allow network*)\n");
    }
    append_rule(&mut profile, "file-read-metadata", &metadata_filters);
    append_rule(&mut profile, "file-read*", &read_filters);
    append_rule(&mut profile, "file-write*", &write_filters);
    append_workspace_mask(&mut profile, command)?;
    Ok(profile)
}

/// Workspace isolation: deny reading/writing the workspace *container* (e.g.
/// `~/.clai/workspaces`) so a broad `$HOME` allow can't expose sibling
/// workspaces, then re-expose the agent's own workspace (and any explicitly-
/// granted sibling) AFTER the deny. Seatbelt is last-match-wins, so the
/// re-expose rules must be emitted after the deny — hence this runs last.
/// Metadata is intentionally left allowed so path traversal into the
/// re-exposed workspace still works. See `profile::workspace_mask`.
fn append_workspace_mask(profile: &mut String, command: &SandboxCommand) -> Result<(), String> {
    let home = command.profile.env.home().map(Path::new);
    let Some(mask) =
        crate::assistant::sandbox::profile::workspace_mask(&command.profile.workspace_root, home)
    else {
        return Ok(());
    };
    // The container may not exist yet; if we can't canonicalize it there's
    // nothing to hide.
    let Ok(mask) = canonicalize_existing(&mask) else {
        return Ok(());
    };

    profile.push_str("(deny file-read* file-write*\n  (subpath \"");
    profile.push_str(&path_literal(&mask)?);
    profile.push_str("\")\n)\n");

    let mut read = Vec::new();
    let mut write = Vec::new();
    let workspace = canonicalize_existing(&command.profile.workspace_root)?;
    read.push(path_literal(&workspace)?);
    write.push(path_literal(&workspace)?);
    for grant in &command.profile.path_grants {
        let Ok(path) = canonicalize_existing(&grant.host_path) else {
            continue;
        };
        if !path.starts_with(&mask) {
            continue;
        }
        read.push(path_literal(&path)?);
        if grant.access == SandboxPathAccess::ReadWrite {
            write.push(path_literal(&path)?);
        }
    }
    append_subpath_allow(profile, "file-read*", &read);
    append_subpath_allow(profile, "file-write*", &write);
    Ok(())
}

fn append_subpath_allow(profile: &mut String, operation: &str, paths: &[String]) {
    if paths.is_empty() {
        return;
    }
    profile.push_str("(allow ");
    profile.push_str(operation);
    profile.push('\n');
    for path in paths {
        profile.push_str("  (subpath \"");
        profile.push_str(path);
        profile.push_str("\")\n");
    }
    profile.push_str(")\n");
}

fn append_mach_lookup_rule(profile: &mut String, network: SandboxNetworkMode) {
    profile.push_str("(allow mach-lookup\n");
    profile.push_str("  (global-name \"com.apple.cfprefsd.agent\")\n");
    profile.push_str("  (global-name \"com.apple.cfprefsd.daemon\")\n");
    profile.push_str("  (global-name \"com.apple.system.logger\")\n");
    profile.push_str("  (global-name \"com.apple.system.opendirectoryd.libinfo\")\n");
    if matches!(network, SandboxNetworkMode::Host) {
        profile.push_str("  (global-name \"com.apple.mDNSResponder\")\n");
    }
    profile.push_str(")\n");
}

fn add_default_filters(
    read_filters: &mut BTreeSet<PathFilter>,
    write_filters: &mut BTreeSet<PathFilter>,
    metadata_filters: &mut BTreeSet<PathFilter>,
) -> Result<(), String> {
    for path in [
        "/System",
        "/Library",
        "/bin",
        "/sbin",
        "/usr",
        "/etc",
        "/private/etc",
        "/private/var/db",
        "/Applications/Xcode.app",
        "/Library/Developer",
        "/opt/homebrew",
        "/nix/store",
        "/run/current-system",
    ] {
        add_existing_dir_filter(read_filters, metadata_filters, Path::new(path))?;
    }

    for path in ["/dev/null", "/dev/random", "/dev/urandom", "/dev/zero"] {
        add_literal_filter(read_filters, metadata_filters, Path::new(path))?;
        if path == "/dev/null" {
            add_literal_filter(write_filters, metadata_filters, Path::new(path))?;
        }
    }

    Ok(())
}

fn add_grant_filters(
    read_filters: &mut BTreeSet<PathFilter>,
    write_filters: &mut BTreeSet<PathFilter>,
    metadata_filters: &mut BTreeSet<PathFilter>,
    grant: &SandboxPathGrant,
) -> Result<(), String> {
    // Tolerate a grant vanishing between the prune in `run` and profile
    // construction (or a caller that skipped the prune): skipping the filter
    // only removes access, matching the lenient `*-bind-try` semantics of the
    // Linux backend, whereas erroring would abort the whole command.
    let Ok(path) = canonicalize_existing(&grant.host_path) else {
        return Ok(());
    };
    if path.is_dir() {
        add_dir_filter(read_filters, metadata_filters, &path)?;
        if grant.access == SandboxPathAccess::ReadWrite {
            add_dir_filter(write_filters, metadata_filters, &path)?;
        }
    } else {
        add_literal_filter(read_filters, metadata_filters, &path)?;
        if grant.access == SandboxPathAccess::ReadWrite {
            add_literal_filter(write_filters, metadata_filters, &path)?;
        }
    }
    Ok(())
}

fn add_existing_dir_filter(
    filters: &mut BTreeSet<PathFilter>,
    metadata_filters: &mut BTreeSet<PathFilter>,
    path: &Path,
) -> Result<(), String> {
    if path.is_dir() {
        add_dir_filter(filters, metadata_filters, path)?;
    }
    Ok(())
}

fn add_dir_filter(
    filters: &mut BTreeSet<PathFilter>,
    metadata_filters: &mut BTreeSet<PathFilter>,
    path: &Path,
) -> Result<(), String> {
    add_ancestor_metadata_filters(metadata_filters, path)?;
    filters.insert(PathFilter::Literal(path_literal(path)?));
    filters.insert(PathFilter::Subpath(path_literal(path)?));
    Ok(())
}

fn add_literal_filter(
    filters: &mut BTreeSet<PathFilter>,
    metadata_filters: &mut BTreeSet<PathFilter>,
    path: &Path,
) -> Result<(), String> {
    add_ancestor_metadata_filters(metadata_filters, path)?;
    filters.insert(PathFilter::Literal(path_literal(path)?));
    Ok(())
}

fn add_ancestor_metadata_filters(
    filters: &mut BTreeSet<PathFilter>,
    path: &Path,
) -> Result<(), String> {
    let mut current = PathBuf::new();
    for component in path.components() {
        match component {
            Component::RootDir => current.push(Path::new("/")),
            Component::Normal(part) => {
                filters.insert(PathFilter::Literal(path_literal(&current)?));
                current.push(part);
            }
            Component::CurDir | Component::ParentDir | Component::Prefix(_) => {}
        }
    }
    if let Some(parent) = path.parent() {
        filters.insert(PathFilter::Literal(path_literal(parent)?));
    }
    Ok(())
}

fn canonicalize_existing(path: &Path) -> Result<PathBuf, String> {
    fs::canonicalize(path)
        .map_err(|error| format!("Failed to canonicalize {}: {}", path.display(), error))
}

fn append_rule(profile: &mut String, operation: &str, filters: &BTreeSet<PathFilter>) {
    if filters.is_empty() {
        return;
    }

    profile.push_str("(allow ");
    profile.push_str(operation);
    profile.push('\n');
    for filter in filters {
        match filter {
            PathFilter::Literal(path) => {
                profile.push_str("  (literal \"");
                profile.push_str(path);
                profile.push_str("\")\n");
            }
            PathFilter::Subpath(path) => {
                profile.push_str("  (subpath \"");
                profile.push_str(path);
                profile.push_str("\")\n");
            }
        }
    }
    profile.push_str(")\n");
}

fn path_literal(path: &Path) -> Result<String, String> {
    os_str_literal(path.as_os_str())
}

fn os_str_literal(value: &OsStr) -> Result<String, String> {
    let text = std::str::from_utf8(value.as_bytes())
        .map_err(|_| "Sandbox paths must be valid UTF-8 on macOS".to_string())?;
    Ok(escape_sbpl_string(text))
}

fn escape_sbpl_string(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            _ => escaped.push(ch),
        }
    }
    escaped
}

fn looks_like_sandbox_exec_setup_failure(output: &SandboxCommandOutput) -> bool {
    if output.success {
        return false;
    }
    if !output.stdout.is_empty() {
        return false;
    }
    output
        .stderr
        .lines()
        .any(|line| line.trim_start().starts_with("sandbox-exec:"))
}

fn classify_sandbox_exec_failure(stderr: &str) -> String {
    let detail = stderr.trim();
    if detail.is_empty() {
        "Sandboxed shell failed to start: sandbox-exec exited before running the command"
            .to_string()
    } else {
        format!("Sandboxed shell failed to start: {}", detail)
    }
}

#[cfg(test)]
fn os(value: impl AsRef<OsStr>) -> std::ffi::OsString {
    value.as_ref().to_os_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assistant::sandbox::{
        SandboxEnv, SandboxPathGrant, SandboxProfile, SandboxSessionBusMode,
    };

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

    fn sample_command(workspace: &Path) -> SandboxCommand {
        SandboxCommand {
            argv: vec![os("/bin/sh"), os("-lc"), os("pwd")],
            cwd: workspace.to_path_buf(),
            timeout_ms: 1_000,
            max_output_chars: 1_000,
            profile: SandboxProfile {
                workspace_root: workspace.to_path_buf(),
                path_grants: vec![],
                network: SandboxNetworkMode::Host,
                session_bus: SandboxSessionBusMode::Deny,
                env: SandboxEnv::filtered_from_iter(
                    [("PATH", "/usr/bin:/bin")],
                    workspace,
                    SandboxSessionBusMode::Deny,
                ),
            },
        }
    }

    #[test]
    fn profile_allows_workspace_read_write_and_network_when_enabled() {
        let workspace = tempfile::tempdir().unwrap();
        let private_tmp = tempfile::tempdir_in(workspace.path()).unwrap();
        let command = sample_command(workspace.path());

        let profile = seatbelt_profile(&command, private_tmp.path()).unwrap();
        let workspace_path = escape_sbpl_string(&workspace.path().display().to_string());

        assert!(profile.contains("(deny default)"));
        assert!(profile.contains("(allow network*)"));
        assert!(profile.contains(&format!("(subpath \"{}\")", workspace_path)));
        assert!(
            profile.contains("(allow file-write*"),
            "workspace should be writable; profile: {profile}"
        );
    }

    #[test]
    fn profile_masks_sibling_workspaces_but_reexposes_own() {
        // A broad $HOME grant would expose sibling workspaces under
        // ~/.clai/workspaces. The container is denied, then the agent's own
        // workspace is re-allowed AFTER the deny (seatbelt is last-match-wins).
        let home = tempfile::tempdir().unwrap();
        let container = home.path().join(".clai").join("workspaces");
        let workspace = container.join("ws-abc");
        std::fs::create_dir_all(&workspace).unwrap();
        let private_tmp = tempfile::tempdir_in(&workspace).unwrap();

        let mut command = sample_command(&workspace);
        command.profile.workspace_root = workspace.clone();
        command.cwd = workspace.clone();
        command.profile.env = SandboxEnv::filtered_from_iter(
            [("PATH", "/usr/bin:/bin")],
            home.path(),
            SandboxSessionBusMode::Deny,
        );
        command.profile.path_grants = vec![SandboxPathGrant {
            host_path: home.path().to_path_buf(),
            access: SandboxPathAccess::ReadOnly,
        }];

        let profile = seatbelt_profile(&command, private_tmp.path()).unwrap();

        let container_c =
            escape_sbpl_string(&fs::canonicalize(&container).unwrap().display().to_string());
        let workspace_c =
            escape_sbpl_string(&fs::canonicalize(&workspace).unwrap().display().to_string());

        let deny = format!(
            "(deny file-read* file-write*\n  (subpath \"{}\")",
            container_c
        );
        let deny_idx = profile
            .find(&deny)
            .unwrap_or_else(|| panic!("container should be denied; profile:\n{profile}"));
        // The own-workspace allow must appear AFTER the deny to win.
        let reexpose_idx = profile
            .rfind(&format!("(subpath \"{}\")", workspace_c))
            .unwrap_or_else(|| panic!("workspace should be re-exposed; profile:\n{profile}"));
        assert!(
            reexpose_idx > deny_idx,
            "workspace re-expose must come after the deny; profile:\n{profile}"
        );
    }

    #[test]
    fn profile_omits_network_when_disabled() {
        let workspace = tempfile::tempdir().unwrap();
        let private_tmp = tempfile::tempdir_in(workspace.path()).unwrap();
        let mut command = sample_command(workspace.path());
        command.profile.network = SandboxNetworkMode::Disabled;

        let profile = seatbelt_profile(&command, private_tmp.path()).unwrap();

        assert!(!profile.contains("(allow network*)"));
        assert!(!profile.contains("com.apple.mDNSResponder"));
    }

    #[test]
    fn profile_maps_read_only_and_read_write_grants() {
        let workspace = tempfile::tempdir().unwrap();
        let private_tmp = tempfile::tempdir_in(workspace.path()).unwrap();
        let read_only = tempfile::tempdir().unwrap();
        let read_write = tempfile::tempdir().unwrap();
        let mut command = sample_command(workspace.path());
        command.profile.path_grants = vec![
            SandboxPathGrant {
                host_path: read_only.path().to_path_buf(),
                access: SandboxPathAccess::ReadOnly,
            },
            SandboxPathGrant {
                host_path: read_write.path().to_path_buf(),
                access: SandboxPathAccess::ReadWrite,
            },
        ];

        let profile = seatbelt_profile(&command, private_tmp.path()).unwrap();
        let ro_path = escape_sbpl_string(
            &fs::canonicalize(read_only.path())
                .unwrap()
                .display()
                .to_string(),
        );
        let rw_path = escape_sbpl_string(
            &fs::canonicalize(read_write.path())
                .unwrap()
                .display()
                .to_string(),
        );

        assert!(profile.contains(&format!("(subpath \"{}\")", ro_path)));
        assert!(profile.contains(&format!("(subpath \"{}\")", rw_path)));

        let write_section = profile
            .split("(allow file-write*")
            .nth(1)
            .expect("write section should exist");
        assert!(!write_section.contains(&format!("(subpath \"{}\")", ro_path)));
        assert!(write_section.contains(&format!("(subpath \"{}\")", rw_path)));
    }

    // Regression for "a deleted filesystem-grant path breaks bash_exec
    // entirely": a stale grant is pruned (live ones kept) instead of failing
    // the whole command up front.
    #[test]
    fn prune_missing_grants_drops_stale_grant_and_keeps_live_ones() {
        let workspace = tempfile::tempdir().unwrap();
        let live = tempfile::tempdir().unwrap();
        let live_grant = SandboxPathGrant {
            host_path: live.path().to_path_buf(),
            access: SandboxPathAccess::ReadWrite,
        };
        let mut command = sample_command(workspace.path());
        command.profile.path_grants = vec![
            SandboxPathGrant {
                host_path: vanished_path(),
                access: SandboxPathAccess::ReadOnly,
            },
            live_grant.clone(),
        ];

        prune_missing_grants(&mut command);

        assert_eq!(
            command.profile.path_grants,
            vec![live_grant],
            "stale grant should be pruned, live grant kept"
        );
    }

    // Even if a stale grant survives to profile construction (TOCTOU between
    // the prune and the build), the profile builds without it instead of
    // erroring the whole command.
    #[test]
    fn profile_builds_despite_missing_grant_path() {
        let workspace = tempfile::tempdir().unwrap();
        let private_tmp = tempfile::tempdir_in(workspace.path()).unwrap();
        let mut command = sample_command(workspace.path());
        command.profile.path_grants = vec![SandboxPathGrant {
            host_path: vanished_path(),
            access: SandboxPathAccess::ReadOnly,
        }];

        let profile = seatbelt_profile(&command, private_tmp.path())
            .expect("profile should build despite the stale grant");
        assert!(
            !profile.contains("stale-grant"),
            "stale grant path must not appear in the profile:\n{profile}"
        );
    }

    #[test]
    fn profile_escapes_path_literals() {
        assert_eq!(
            escape_sbpl_string("/tmp/a \"quoted\" path\\name"),
            "/tmp/a \\\"quoted\\\" path\\\\name"
        );
    }

    #[test]
    fn profile_does_not_allow_all_mach_lookups() {
        let workspace = tempfile::tempdir().unwrap();
        let private_tmp = tempfile::tempdir_in(workspace.path()).unwrap();
        let command = sample_command(workspace.path());

        let profile = seatbelt_profile(&command, private_tmp.path()).unwrap();

        assert!(!profile.contains("(allow mach-lookup)\n"));
        assert!(profile.contains("(global-name \"com.apple.system.logger\")"));
        assert!(profile.contains("(global-name \"com.apple.mDNSResponder\")"));
    }

    #[test]
    fn setup_failure_requires_sandbox_exec_stderr_prefix() {
        let output = SandboxCommandOutput {
            cwd: PathBuf::from("/tmp"),
            exit_code: Some(1),
            success: false,
            stdout: String::new(),
            stderr: "sandbox-exec: profile compilation failed\n".to_string(),
        };

        assert!(looks_like_sandbox_exec_setup_failure(&output));

        let inner_command_output = SandboxCommandOutput {
            stderr: "sandbox-exec: printed by the child\n".to_string(),
            stdout: "child ran\n".to_string(),
            ..output
        };
        assert!(!looks_like_sandbox_exec_setup_failure(
            &inner_command_output
        ));
    }
}
