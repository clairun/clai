//! Workspace-tier shell permission persistence.
//!
//! Each agent workspace can carry its own allow/block prefix lists in
//! `<workspace_root>/.clai/permissions.json`. The file is intentionally
//! plain JSON so it can be committed to a workspace's git repo (a planned
//! future feature: portable, version-controlled workspaces).
//!
//! At policy-check time the lists here are *unioned* with the agent's
//! own per-agent lists (which still live in the workspace_agents SQLite
//! blob). Any block in any scope beats any allow in any scope.
//!
//! Reads are lenient: a missing or malformed file returns the default
//! (empty lists), letting the per-agent lists carry the decision. We never
//! fail-open on the allow side because the worst case here is "the
//! workspace tier adds nothing," which keeps the agent's own gating intact.

#![allow(dead_code)] // wired into enforce_command_policy in commit 5

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// On-disk schema version. Bump when the file shape changes incompatibly.
const CURRENT_VERSION: u32 = 1;

const PERMISSIONS_DIR: &str = ".clai";
const PERMISSIONS_FILE: &str = "permissions.json";
const TEMP_EXTENSION: &str = "json.tmp";

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceShellPermissions {
    #[serde(default)]
    pub allowed_command_prefixes: Vec<String>,
    #[serde(default)]
    pub blocked_command_prefixes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WorkspacePermissions {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub shell: WorkspaceShellPermissions,
}

impl Default for WorkspacePermissions {
    fn default() -> Self {
        Self {
            version: CURRENT_VERSION,
            shell: WorkspaceShellPermissions::default(),
        }
    }
}

fn default_version() -> u32 {
    CURRENT_VERSION
}

/// Computes the `.clai/permissions.json` path inside the given workspace.
pub fn permissions_path(workspace_root: &Path) -> PathBuf {
    workspace_root.join(PERMISSIONS_DIR).join(PERMISSIONS_FILE)
}

/// Loads the workspace's permissions file. Returns the default on any
/// failure (missing file, unreadable, malformed) — callers should not
/// treat this as fatal. A `tracing::warn` is emitted on parse failure
/// so the user can debug a corrupted file.
pub fn load(workspace_root: &Path) -> WorkspacePermissions {
    let path = permissions_path(workspace_root);
    let contents = match fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return WorkspacePermissions::default();
        }
        Err(e) => {
            tracing::warn!(
                "Failed to read workspace permissions at {}: {}",
                path.display(),
                e
            );
            return WorkspacePermissions::default();
        }
    };
    match serde_json::from_str::<WorkspacePermissions>(&contents) {
        Ok(perms) => perms,
        Err(e) => {
            tracing::warn!(
                "Failed to parse workspace permissions at {}: {}",
                path.display(),
                e
            );
            WorkspacePermissions::default()
        }
    }
}

/// Saves the workspace's permissions file atomically (write to `.tmp`,
/// fsync, rename). Creates `.clai/` if needed.
pub fn save(workspace_root: &Path, perms: &WorkspacePermissions) -> io::Result<()> {
    let path = permissions_path(workspace_root);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(perms)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let temp_path = path.with_extension(TEMP_EXTENSION);
    let mut file = fs::File::create(&temp_path)?;
    file.write_all(json.as_bytes())?;
    file.sync_all()?;
    fs::rename(&temp_path, &path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_perms(allowed: &[&str], blocked: &[&str]) -> WorkspacePermissions {
        WorkspacePermissions {
            version: CURRENT_VERSION,
            shell: WorkspaceShellPermissions {
                allowed_command_prefixes: allowed.iter().map(|s| s.to_string()).collect(),
                blocked_command_prefixes: blocked.iter().map(|s| s.to_string()).collect(),
            },
        }
    }

    #[test]
    fn load_returns_default_when_file_absent() {
        let tmp = TempDir::new().unwrap();
        let perms = load(tmp.path());
        assert_eq!(perms, WorkspacePermissions::default());
    }

    #[test]
    fn save_then_load_round_trip() {
        let tmp = TempDir::new().unwrap();
        let original = make_perms(&["git status", "kubectl logs"], &["rm", "sudo"]);
        save(tmp.path(), &original).unwrap();
        let loaded = load(tmp.path());
        assert_eq!(loaded, original);
    }

    #[test]
    fn save_creates_dot_clai_directory() {
        let tmp = TempDir::new().unwrap();
        let perms = make_perms(&["echo"], &[]);
        save(tmp.path(), &perms).unwrap();
        let dir = tmp.path().join(PERMISSIONS_DIR);
        assert!(dir.is_dir(), ".clai/ should have been created");
        let file = dir.join(PERMISSIONS_FILE);
        assert!(file.is_file(), "permissions.json should exist");
    }

    #[test]
    fn save_uses_atomic_rename() {
        // After a successful save, the temp file should not exist.
        let tmp = TempDir::new().unwrap();
        save(tmp.path(), &make_perms(&["a"], &[])).unwrap();
        let temp_path = permissions_path(tmp.path()).with_extension(TEMP_EXTENSION);
        assert!(!temp_path.exists(), "temp file should be renamed away");
    }

    #[test]
    fn load_falls_back_on_malformed_file() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join(PERMISSIONS_DIR);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(PERMISSIONS_FILE), "{ not valid json").unwrap();
        let perms = load(tmp.path());
        assert_eq!(perms, WorkspacePermissions::default());
    }

    #[test]
    fn load_falls_back_on_unreadable_path() {
        // Passing a workspace_root that doesn't exist should yield defaults.
        let perms = load(Path::new("/nonexistent/path/that/does/not/exist"));
        assert_eq!(perms, WorkspacePermissions::default());
    }

    #[test]
    fn missing_fields_use_defaults() {
        // Forward-compat: old file without `shell.blockedCommandPrefixes`
        // should still parse with blocked defaulting to empty.
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join(PERMISSIONS_DIR);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join(PERMISSIONS_FILE),
            r#"{"version": 1, "shell": {"allowedCommandPrefixes": ["git"]}}"#,
        )
        .unwrap();
        let perms = load(tmp.path());
        assert_eq!(perms.shell.allowed_command_prefixes, vec!["git"]);
        assert!(perms.shell.blocked_command_prefixes.is_empty());
    }

    #[test]
    fn missing_version_defaults_to_current() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join(PERMISSIONS_DIR);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(PERMISSIONS_FILE), r#"{"shell": {}}"#).unwrap();
        let perms = load(tmp.path());
        assert_eq!(perms.version, CURRENT_VERSION);
    }

    #[test]
    fn json_is_pretty_printed() {
        let tmp = TempDir::new().unwrap();
        save(tmp.path(), &make_perms(&["git status"], &["rm"])).unwrap();
        let contents = fs::read_to_string(permissions_path(tmp.path())).unwrap();
        // Pretty-printed JSON has newlines and indentation.
        assert!(
            contents.contains('\n'),
            "expected pretty-printed JSON with newlines, got: {contents}"
        );
        assert!(contents.contains("\"version\""));
        assert!(contents.contains("\"allowedCommandPrefixes\""));
    }

    #[test]
    fn permissions_path_layout() {
        let p = permissions_path(Path::new("/ws/abc"));
        assert_eq!(p, PathBuf::from("/ws/abc/.clai/permissions.json"));
    }

    #[test]
    fn overwrite_existing_file() {
        let tmp = TempDir::new().unwrap();
        save(tmp.path(), &make_perms(&["a"], &[])).unwrap();
        save(tmp.path(), &make_perms(&["b", "c"], &["x"])).unwrap();
        let loaded = load(tmp.path());
        assert_eq!(loaded.shell.allowed_command_prefixes, vec!["b", "c"]);
        assert_eq!(loaded.shell.blocked_command_prefixes, vec!["x"]);
    }
}
