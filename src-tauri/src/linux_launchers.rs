//! Native Linux app-list entries for starred workspaces.

use std::collections::HashSet;
use std::env;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use uuid::Uuid;

use crate::AppState;

const PREFIX: &str = "run.clai.CLAI-workspace-";
const SUFFIX: &str = ".desktop";
const OWNER_KEY: &str = "X-CLAI-Workspace-ID=";
static LAUNCHER_LOCK: Mutex<()> = Mutex::new(());

fn applications_dir() -> Result<PathBuf, String> {
    let data_home = match env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
    {
        Some(value) => value,
        None => dirs::home_dir()
            .ok_or_else(|| "Cannot find home directory for workspace launchers".to_string())?
            .join(".local/share"),
    };
    Ok(data_home.join("applications"))
}

fn launcher_path(dir: &Path, id: &str) -> Result<PathBuf, String> {
    let uuid = Uuid::parse_str(id).map_err(|_| format!("Invalid workspace UUID: {id}"))?;
    if uuid.to_string() != id {
        return Err(format!("Workspace UUID is not canonical: {id}"));
    }
    Ok(dir.join(format!("{PREFIX}{id}{SUFFIX}")))
}

fn escaped_name(title: &str) -> String {
    let mut result = String::with_capacity(title.len());
    for ch in title.chars() {
        match ch {
            '\\' => result.push_str("\\\\"),
            '\n' => result.push_str("\\n"),
            '\r' => result.push_str("\\r"),
            '\t' => result.push_str("\\t"),
            ';' => result.push_str("\\;"),
            c if c.is_control() => result.push(' '),
            c => result.push(c),
        }
    }
    result
}

fn entry(id: &str, title: &str) -> String {
    format!(
        "[Desktop Entry]\nType=Application\nName={} — CLAI\nExec=clai open {}\nIcon=run.clai.CLAI\nTerminal=false\nCategories=Development;\n{}{}\n",
        escaped_name(title), id, OWNER_KEY, id
    )
}

fn is_owned(path: &Path, id: &str) -> Result<bool, String> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(format!("Cannot inspect {}: {error}", path.display())),
    };
    if !metadata.file_type().is_file() {
        return Ok(false);
    }
    let content = fs::read_to_string(path)
        .map_err(|error| format!("Cannot read {}: {error}", path.display()))?;
    Ok(content.starts_with("[Desktop Entry]\n")
        && content
            .lines()
            .any(|line| line == format!("{OWNER_KEY}{id}")))
}

fn upsert_in(dir: &Path, id: &str, title: &str) -> Result<(), String> {
    let path = launcher_path(dir, id)?;
    match fs::symlink_metadata(&path) {
        Ok(_) if !is_owned(&path, id)? => {
            return Err(format!(
                "Refusing to replace non-CLAI launcher {}",
                path.display()
            ));
        }
        Err(error) if error.kind() != io::ErrorKind::NotFound => {
            return Err(format!("Cannot inspect {}: {error}", path.display()));
        }
        _ => {}
    }
    fs::create_dir_all(dir).map_err(|error| format!("Cannot create {}: {error}", dir.display()))?;
    let mut temp = tempfile::NamedTempFile::new_in(dir)
        .map_err(|error| format!("Cannot create launcher in {}: {error}", dir.display()))?;
    temp.write_all(entry(id, title).as_bytes())
        .and_then(|()| temp.as_file().sync_all())
        .map_err(|error| format!("Cannot write launcher {}: {error}", path.display()))?;
    temp.persist(&path)
        .map_err(|error| format!("Cannot replace launcher {}: {error}", path.display()))?;
    Ok(())
}

fn remove_in(dir: &Path, id: &str) -> Result<(), String> {
    let path = launcher_path(dir, id)?;
    if is_owned(&path, id)? {
        fs::remove_file(&path)
            .map_err(|error| format!("Cannot remove launcher {}: {error}", path.display()))?;
    }
    Ok(())
}

pub(crate) fn warning(
    workspace_id: &str,
    saved_change: &str,
    result: Result<(), String>,
) -> Option<String> {
    result.err().map(|error| {
        tracing::warn!(%error, %workspace_id, "Failed to sync workspace launcher");
        format!("{saved_change} was saved, but its app launcher could not be updated: {error}")
    })
}

pub(crate) fn sync_workspace(state: &AppState, id: &str) -> Result<(), String> {
    if crate::providers::is_flatpak() {
        return Ok(());
    }
    let _guard = LAUNCHER_LOCK
        .lock()
        .map_err(|error| format!("Launcher lock error: {error}"))?;
    sync_workspace_in(state, id, &applications_dir()?)
}

fn sync_workspace_in(state: &AppState, id: &str, dir: &Path) -> Result<(), String> {
    let index = state
        .workspace_index
        .read()
        .map_err(|error| format!("Workspace index lock error: {error}"))?;
    let starred = index
        .by_id
        .get(id)
        .filter(|locator| locator.starred_at > 0)
        .map(|locator| locator.title.clone());
    drop(index);
    match starred {
        Some(title) => upsert_in(dir, id, &title),
        None => remove_in(dir, id),
    }
}

fn reconcile_in(
    dir: &Path,
    starred: &[(String, String)],
    remove_orphans: bool,
) -> Result<(), String> {
    let expected: HashSet<_> = starred.iter().map(|(id, _)| id.as_str()).collect();
    let mut errors = Vec::new();
    for (id, title) in starred {
        if let Err(error) = upsert_in(dir, id, title) {
            errors.push(error);
        }
    }
    if remove_orphans {
        let entries = match fs::read_dir(dir) {
            Ok(entries) => Some(entries),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => {
                errors.push(format!("Cannot scan {}: {error}", dir.display()));
                None
            }
        };
        if let Some(entries) = entries {
            for item in entries {
                let item = match item {
                    Ok(item) => item,
                    Err(error) => {
                        errors.push(format!("Cannot read launcher directory entry: {error}"));
                        continue;
                    }
                };
                let name = item.file_name();
                let Some(id) = name
                    .to_str()
                    .and_then(|name| name.strip_prefix(PREFIX))
                    .and_then(|name| name.strip_suffix(SUFFIX))
                else {
                    continue;
                };
                if !expected.contains(id)
                    && Uuid::parse_str(id).is_ok_and(|uuid| uuid.to_string() == id)
                {
                    if let Err(error) = remove_in(dir, id) {
                        errors.push(error);
                    }
                }
            }
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

pub(crate) fn reconcile(state: &AppState) -> Result<(), String> {
    if crate::providers::is_flatpak() {
        return Ok(());
    }
    let _guard = LAUNCHER_LOCK
        .lock()
        .map_err(|error| format!("Launcher lock error: {error}"))?;
    let index = state
        .workspace_index
        .read()
        .map_err(|error| format!("Workspace index lock error: {error}"))?;
    let starred = index
        .by_id
        .values()
        .filter(|locator| locator.starred_at > 0)
        .map(|locator| (locator.id.clone(), locator.title.clone()))
        .collect::<Vec<_>>();
    let remove_orphans = index.load_failures.is_empty() && index.scan_errors.is_empty();
    drop(index);
    reconcile_in(&applications_dir()?, &starred, remove_orphans)
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "11111111-1111-4111-8111-111111111111";
    const B: &str = "22222222-2222-4222-8222-222222222222";

    #[test]
    fn lifecycle_and_startup_reconciliation() {
        let dir = tempfile::tempdir().unwrap();
        let dir = dir.path();
        upsert_in(dir, A, "First").unwrap();
        let path = launcher_path(dir, A).unwrap();
        assert!(fs::read_to_string(&path)
            .unwrap()
            .contains("Name=First — CLAI"));
        upsert_in(dir, A, "Renamed").unwrap();
        assert!(fs::read_to_string(&path)
            .unwrap()
            .contains("Name=Renamed — CLAI"));
        remove_in(dir, A).unwrap();
        assert!(!path.exists());
        upsert_in(dir, A, "Stale").unwrap();
        reconcile_in(dir, &[(B.into(), "Starred".into())], true).unwrap();
        assert!(!path.exists());
        assert!(launcher_path(dir, B).unwrap().exists());
        upsert_in(dir, A, "Stale").unwrap();
        reconcile_in(dir, &[], false).unwrap();
        assert!(path.exists());
    }

    #[test]
    fn escaping_uuid_and_ownership() {
        let dir = tempfile::tempdir().unwrap();
        let dir = dir.path();
        assert!(launcher_path(dir, "../../bad").is_err());
        upsert_in(dir, A, "a\\b\nc\td;\r\0").unwrap();
        let path = launcher_path(dir, A).unwrap();
        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("Name=a\\\\b\\nc\\td\\;\\r  — CLAI"));
        assert!(content.contains(&format!("Exec=clai open {A}\n")));
        fs::write(&path, "[Desktop Entry]\nName=Other\n").unwrap();
        assert!(upsert_in(dir, A, "Mine").is_err());
        remove_in(dir, A).unwrap();
        assert!(path.exists());
        let target = dir.join("other.desktop");
        fs::write(&target, "[Desktop Entry]\nName=Other\n").unwrap();
        fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(&target, &path).unwrap();
        assert!(upsert_in(dir, A, "Mine").is_err());
        remove_in(dir, A).unwrap();
        assert!(path.is_symlink());
        assert!(target.exists());
    }

    #[test]
    fn failed_launcher_write_returns_warning_after_saved_star() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join(A);
        let config = crate::WorkspaceConfig::new(A.into(), "First".into(), 1_000, B.into());
        crate::workspace_config::save(&root, &config).unwrap();
        let mut index = crate::WorkspaceIndex::default();
        index.insert_config(root.clone(), &config);
        let manager = crate::ConfigManager::new_for_tests(
            crate::AppConfig::default(),
            temp.path().join("app-config.json"),
        );
        let state = AppState::new_for_tests(manager, index).unwrap();
        state
            .update_workspace_config(A, |config| {
                config.starred_at = 42;
                Ok(())
            })
            .unwrap();

        let blocker = temp.path().join("not-a-directory");
        fs::write(&blocker, "file").unwrap();
        let message = warning(A, "Workspace star", sync_workspace_in(&state, A, &blocker)).unwrap();
        assert!(message.contains("Workspace star was saved"));
        assert!(message.contains("app launcher could not be updated"));
        assert_eq!(crate::workspace_config::load(&root).unwrap().starred_at, 42);
        assert_eq!(
            state.workspace_index.read().unwrap().by_id[A].starred_at,
            42
        );
    }

    #[test]
    fn atomic_write_failure_preserves_existing_entry() {
        let dir = tempfile::tempdir().unwrap();
        upsert_in(dir.path(), A, "Original").unwrap();
        let path = launcher_path(dir.path(), A).unwrap();
        let original = fs::read_to_string(&path).unwrap();
        assert!(upsert_in(&path, A, "Changed").is_err());
        assert_eq!(fs::read_to_string(path).unwrap(), original);
    }
}
