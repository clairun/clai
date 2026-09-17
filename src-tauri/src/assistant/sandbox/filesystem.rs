//! Compile filesystem identity, isolation and additive access once, before execution.
//! Backends lower this plan; they must not infer destinations from host symlinks.
use super::profile::{workspace_mask, SandboxPathAccess, SandboxPathGrant};
use crate::config::{FilesystemPathAccess, FilesystemPathGrant};
use std::{
    fs,
    path::{Component, Path, PathBuf},
};

#[derive(Debug, Clone)]
pub struct EffectiveFilesystemPolicy {
    pub workspace_root: PathBuf,
    pub path_grants: Vec<SandboxPathGrant>,
    pub mask: Option<PathBuf>,
    pub home: Option<PathBuf>,
}

impl EffectiveFilesystemPolicy {
    pub fn compile(
        workspace: &Path,
        grants: &[SandboxPathGrant],
        home: Option<&Path>,
    ) -> Result<Self, String> {
        // Derive isolation from the configured container BEFORE resolving the workspace.
        let mask = workspace_mask(workspace, home)
            .map(|p| resolve(&p))
            .transpose()?;
        let workspace_root = resolve(workspace)?;
        let home = home.map(resolve).transpose()?;
        let mut path_grants = Vec::new();
        for grant in grants {
            // Optional grants may go stale (including dangling symlinks). Never
            // substitute the unresolved spelling: omit access rather than fail
            // unrelated workspace commands or prevent recovery grant requests.
            let host_path = match resolve(&grant.host_path) {
                Ok(path) => path,
                Err(error) => {
                    tracing::warn!(path = %grant.host_path.display(), %error, "Skipping unresolvable optional filesystem grant");
                    continue;
                }
            };
            if host_path == workspace_root {
                continue;
            }
            let resolved = SandboxPathGrant {
                host_path,
                access: grant.access,
            };
            if !path_grants.contains(&resolved) {
                path_grants.push(resolved);
            }
        }
        let mut writes = vec![workspace_root.clone()];
        writes.extend(
            path_grants
                .iter()
                .filter(|g| g.access == SandboxPathAccess::ReadWrite)
                .map(|g| g.host_path.clone()),
        );
        path_grants.retain(|g| {
            g.access == SandboxPathAccess::ReadWrite
                || !writes
                    .iter()
                    .any(|w| authorizes(w, &g.host_path, mask.as_deref()))
        });
        Ok(Self {
            workspace_root,
            path_grants,
            mask,
            home,
        })
    }

    pub fn from_config(
        workspace: &Path,
        grants: &[FilesystemPathGrant],
        home: Option<&Path>,
    ) -> Result<Self, String> {
        let grants = grants
            .iter()
            .map(|g| {
                Ok(SandboxPathGrant {
                    host_path: resolve_configured_path(&g.path)?,
                    access: match g.access {
                        FilesystemPathAccess::ReadOnly => SandboxPathAccess::ReadOnly,
                        FilesystemPathAccess::ReadWrite => SandboxPathAccess::ReadWrite,
                    },
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        Self::compile(workspace, &grants, home)
    }

    /// Query an absolute, resolved path (the form returned by grant requests).
    pub fn covers(&self, path: &Path, access: FilesystemPathAccess) -> bool {
        authorizes(&self.workspace_root, path, self.mask.as_deref())
            || self.path_grants.iter().any(|g| {
                authorizes(&g.host_path, path, self.mask.as_deref())
                    && (g.access == SandboxPathAccess::ReadWrite
                        || access == FilesystemPathAccess::ReadOnly)
            })
    }

    pub fn authorize_cwd(&self, requested: &str) -> Result<PathBuf, String> {
        let raw = Path::new(requested);
        let path = if raw.is_absolute() {
            raw.to_path_buf()
        } else {
            self.workspace_root.join(raw)
        };
        let path = resolve(&normalize_path(path))?;
        if self.covers(&path, FilesystemPathAccess::ReadOnly) {
            Ok(path)
        } else {
            Err(format!(
                "Path {} is outside the agent's allowed filesystem grants",
                path.display()
            ))
        }
    }

    /// Mounts are explicit, with masks first at equal depth and writable binds last.
    /// The ordered operations a mount-based backend applies. Only the Linux
    /// bwrap backend lowers these; macOS reads the resolved fields directly
    /// and Windows runs commands unsandboxed.
    #[cfg(any(target_os = "linux", test))]
    pub fn operations(&self) -> Vec<FilesystemOperation<'_>> {
        let mut ops = vec![FilesystemOperation::Expose {
            path: &self.workspace_root,
            access: SandboxPathAccess::ReadWrite,
            required: true,
        }];
        ops.extend(
            self.path_grants
                .iter()
                .map(|g| FilesystemOperation::Expose {
                    path: &g.host_path,
                    access: g.access,
                    required: false,
                }),
        );
        if let Some(mask) = &self.mask {
            ops.push(FilesystemOperation::Mask(mask));
        }
        ops.sort_by_key(|op| match op {
            FilesystemOperation::Mask(path) => (path.components().count(), 0),
            FilesystemOperation::Expose { path, access, .. } => (
                path.components().count(),
                if *access == SandboxPathAccess::ReadWrite {
                    2
                } else {
                    1
                },
            ),
        });
        ops
    }
}

#[derive(Debug)]
#[cfg(any(target_os = "linux", test))]
pub enum FilesystemOperation<'a> {
    Mask(&'a Path),
    Expose {
        path: &'a Path,
        access: SandboxPathAccess,
        required: bool,
    },
}

pub(crate) fn resolve(path: &Path) -> Result<PathBuf, String> {
    resolve_in_execution_namespace(path).map_err(|e| format!("Path {} resolves outside the agent's allowed filesystem grants or cannot be resolved: {}", path.display(), e))
}

fn resolve_in_execution_namespace(path: &Path) -> std::io::Result<PathBuf> {
    #[cfg(target_os = "linux")]
    if crate::providers::is_flatpak() {
        return resolve_on_host(path);
    }
    resolve_symlinks_through_existing_ancestor(path)
}

/// Resolve a path that must already exist, in the namespace the shell sandbox
/// runs in. `fs_request_grant` uses this so the identity it compares against
/// the compiled policy, and later persists, is the same one [`compile`]
/// derives: the host view under Flatpak, the process view elsewhere.
///
/// [`compile`]: EffectiveFilesystemPolicy::compile
pub(crate) fn resolve_existing(path: &Path) -> std::io::Result<PathBuf> {
    #[cfg(target_os = "linux")]
    if crate::providers::is_flatpak() {
        return resolve_on_host(path);
    }
    fs::canonicalize(path)
}

/// Flatpak launches the shell sandbox on the host. Resolving in the app's
/// namespace can either discard a live host grant or bind an unrelated path.
/// Require an existing host target: optional absent grants are skipped, and an
/// absent cwd cannot be entered anyway. Never fall back to the app's spelling.
#[cfg(target_os = "linux")]
fn resolve_on_host(path: &Path) -> std::io::Result<PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    let output = crate::providers::get_host_command("realpath")
        .args(["--canonicalize-existing", "--zero", "--"])
        .arg(path)
        .output()?;
    if !output.status.success() {
        return Err(std::io::Error::other(format!(
            "Host path resolution failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    let Some(bytes) = output.stdout.strip_suffix(&[0]) else {
        return Err(std::io::Error::other(
            "Invalid host path resolution response",
        ));
    };
    let resolved = PathBuf::from(std::ffi::OsString::from_vec(bytes.to_vec()));
    if !resolved.is_absolute() || bytes.contains(&0) {
        return Err(std::io::Error::other(
            "Invalid host path resolution response",
        ));
    }
    Ok(resolved)
}

fn authorizes(root: &Path, candidate: &Path, mask: Option<&Path>) -> bool {
    candidate.starts_with(root) && !grant_masked_for_candidate(root, candidate, mask)
}
pub(crate) fn grant_masked_for_candidate(
    root: &Path,
    candidate: &Path,
    mask: Option<&Path>,
) -> bool {
    mask.is_some_and(|m| candidate.starts_with(m) && m.starts_with(root) && m != root)
}

/// Resolve `path` through symlinks so a requested shell working directory is
/// checked by where it actually lands, not by its spelling.
///
/// The nearest existing ancestor (or the path itself) is canonicalized and the
/// not-yet-existing tail is re-appended unchanged; nothing is created. An
/// existing component that cannot be resolved (for example, a dangling
/// symlink) is refused. If nothing along the path exists, no symlink can be
/// involved and the path is returned as is.
pub(crate) fn resolve_symlinks_through_existing_ancestor(path: &Path) -> std::io::Result<PathBuf> {
    let mut missing: Vec<&std::ffi::OsStr> = Vec::new();
    let mut ancestor = path;
    loop {
        match fs::symlink_metadata(ancestor) {
            Ok(_) => {
                let mut resolved = ancestor.canonicalize()?;
                for name in missing.iter().rev() {
                    resolved.push(name);
                }
                return Ok(resolved);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let (Some(name), Some(parent)) = (ancestor.file_name(), ancestor.parent()) else {
            return Ok(path.to_path_buf());
        };
        missing.push(name);
        ancestor = parent;
    }
}

pub(crate) fn resolve_configured_path(path: &str) -> Result<PathBuf, String> {
    let raw = Path::new(path);
    if raw.is_absolute() {
        Ok(normalize_path(raw.to_path_buf()))
    } else {
        let cwd = std::env::current_dir()
            .map_err(|e| format!("Failed to resolve current directory: {}", e))?;
        Ok(normalize_path(cwd.join(raw)))
    }
}

pub(crate) fn normalize_path(path: PathBuf) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(Path::new(std::path::MAIN_SEPARATOR_STR)),
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(part) => normalized.push(part),
        }
    }
    normalized
}

/// Render a path for agent-facing tool output.
///
/// On Windows the agent's `bash_exec` runs inside Git Bash, where `\` is an
/// escape character, so paths handed to the model must use `/` to round-trip
/// back into shell commands. We also strip the `\\?\` verbatim prefix that
/// `std::fs::canonicalize` adds, leaving clean `C:/Users/...` paths that Git
/// Bash accepts.
///
/// On Unix the path is returned verbatim: `\` is a legal byte in a filename,
/// so rewriting it would corrupt names.
pub(crate) fn agent_path_string(path: &Path) -> String {
    #[cfg(windows)]
    {
        let s = path.to_string_lossy();
        let trimmed = s.strip_prefix(r"\\?\").unwrap_or(s.as_ref());
        trimmed.replace('\\', "/")
    }
    #[cfg(not(windows))]
    {
        path.display().to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grant(path: &Path, access: SandboxPathAccess) -> SandboxPathGrant {
        SandboxPathGrant {
            host_path: path.to_path_buf(),
            access,
        }
    }

    #[test]
    fn additive_grants_fold_reads_and_preserve_write_upgrades() {
        use SandboxPathAccess::{ReadOnly, ReadWrite};
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let workspace = root.join("ws");
        let data = root.join("data");
        let unrelated = root.join("database");
        let policy = EffectiveFilesystemPolicy::compile(
            &workspace,
            &[
                grant(&workspace, ReadOnly),
                grant(&workspace.join("docs"), ReadOnly),
                grant(&data, ReadWrite),
                grant(&data, ReadWrite),
                grant(&data.join("docs"), ReadOnly),
                grant(&unrelated, ReadOnly),
                grant(&unrelated.join("scratch"), ReadWrite),
            ],
            None,
        )
        .unwrap();
        assert_eq!(
            policy.path_grants,
            vec![
                grant(&data, ReadWrite),
                grant(&unrelated, ReadOnly),
                grant(&unrelated.join("scratch"), ReadWrite),
            ]
        );
        assert!(policy.covers(&workspace.join("docs"), FilesystemPathAccess::ReadWrite));
        assert!(policy.covers(&data.join("docs"), FilesystemPathAccess::ReadWrite));
        assert!(policy.covers(&unrelated, FilesystemPathAccess::ReadOnly));
        assert!(policy.covers(&unrelated.join("child"), FilesystemPathAccess::ReadOnly));
        assert!(!policy.covers(&unrelated, FilesystemPathAccess::ReadWrite));
        assert!(policy.covers(&unrelated.join("scratch"), FilesystemPathAccess::ReadWrite));
        assert!(!policy.covers(&root.join("elsewhere"), FilesystemPathAccess::ReadOnly));
    }

    #[test]
    fn mask_blocks_broad_grants_but_preserves_explicit_siblings_and_container() {
        use SandboxPathAccess::{ReadOnly, ReadWrite};
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().canonicalize().unwrap();
        let container = home.join(".clai/workspaces");
        let workspace = container.join("own");
        let sibling = container.join("other");
        for broad_access in [ReadOnly, ReadWrite] {
            let policy = EffectiveFilesystemPolicy::compile(
                &workspace,
                &[grant(&home, broad_access)],
                Some(&home),
            )
            .unwrap();
            assert_eq!(policy.mask.as_ref(), Some(&container));
            assert!(!policy.covers(&sibling, FilesystemPathAccess::ReadOnly));
            for id in ["own", "other"] {
                assert!(!policy.covers(
                    &container.join(".scratch").join(id),
                    FilesystemPathAccess::ReadOnly
                ));
            }
            assert!(policy.covers(&workspace.join("notes"), FilesystemPathAccess::ReadWrite));
            assert!(policy.covers(&home.join(".gitconfig"), FilesystemPathAccess::ReadOnly));
            let explicit = EffectiveFilesystemPolicy::compile(
                &workspace,
                &[grant(&home, broad_access), grant(&sibling, ReadOnly)],
                Some(&home),
            )
            .unwrap();
            assert!(explicit.path_grants.contains(&grant(&sibling, ReadOnly)));
            assert!(explicit.covers(&sibling, FilesystemPathAccess::ReadOnly));
            assert!(!explicit.covers(&sibling, FilesystemPathAccess::ReadWrite));
        }
        for write_root in [&sibling, &container] {
            let policy = EffectiveFilesystemPolicy::compile(
                &workspace,
                &[
                    grant(write_root, ReadWrite),
                    grant(&sibling.join("notes"), ReadOnly),
                ],
                Some(&home),
            )
            .unwrap();
            assert_eq!(policy.path_grants, vec![grant(write_root, ReadWrite)]);
            assert!(policy.covers(&sibling.join("notes"), FilesystemPathAccess::ReadWrite));
        }
    }

    #[test]
    fn operations_mask_before_equal_depth_grants_and_write_after_read() {
        use SandboxPathAccess::{ReadOnly, ReadWrite};
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().canonicalize().unwrap();
        let container = home.join(".clai/workspaces");
        let workspace = container.join("own");
        let policy = EffectiveFilesystemPolicy::compile(
            &workspace,
            &[grant(&home, ReadOnly), grant(&container, ReadWrite)],
            Some(&home),
        )
        .unwrap();
        let operations = policy.operations();
        assert!(matches!(operations.as_slice(), [
            FilesystemOperation::Expose { path: a, access: ReadOnly, required: false },
            FilesystemOperation::Mask(b),
            FilesystemOperation::Expose { path: c, access: ReadWrite, required: false },
            FilesystemOperation::Expose { path: d, access: ReadWrite, required: true },
        ] if *a == home && *b == container && *c == container && *d == workspace));
    }

    #[test]
    fn cwd_allows_read_grants_and_missing_descendants_but_rejects_escape() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let workspace = root.join("workspace");
        let read = root.join("read");
        fs::create_dir(&workspace).unwrap();
        fs::create_dir(&read).unwrap();
        let policy = EffectiveFilesystemPolicy::compile(
            &workspace,
            &[grant(&read, SandboxPathAccess::ReadOnly)],
            None,
        )
        .unwrap();
        assert_eq!(policy.authorize_cwd(".").unwrap(), workspace);
        assert_eq!(
            policy.authorize_cwd("new/child").unwrap(),
            workspace.join("new/child")
        );
        assert_eq!(policy.authorize_cwd(read.to_str().unwrap()).unwrap(), read);
        assert!(policy.authorize_cwd("../outside").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn configured_symlink_grants_resolve_before_folding_and_cwd_checks() {
        use FilesystemPathAccess::{ReadOnly, ReadWrite};
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let workspace = root.join("ws");
        let outside = root.join("outside");
        fs::create_dir_all(workspace.join("docs")).unwrap();
        fs::create_dir_all(outside.join("sub")).unwrap();
        let escape = workspace.join("escape");
        let inside = root.join("inside");
        std::os::unix::fs::symlink(&outside, &escape).unwrap();
        std::os::unix::fs::symlink(workspace.join("docs"), &inside).unwrap();
        let ungranted = EffectiveFilesystemPolicy::compile(&workspace, &[], None).unwrap();
        assert!(ungranted.authorize_cwd("escape").is_err());
        let policy = EffectiveFilesystemPolicy::from_config(
            &workspace,
            &[
                FilesystemPathGrant {
                    path: escape.display().to_string(),
                    access: ReadOnly,
                    origin: None,
                },
                FilesystemPathGrant {
                    path: inside.display().to_string(),
                    access: ReadOnly,
                    origin: None,
                },
            ],
            None,
        )
        .unwrap();
        assert_eq!(
            policy.path_grants,
            vec![grant(&outside, SandboxPathAccess::ReadOnly)]
        );
        assert_eq!(
            policy.authorize_cwd("escape/sub").unwrap(),
            outside.join("sub")
        );
        assert_eq!(
            policy.authorize_cwd(inside.to_str().unwrap()).unwrap(),
            workspace.join("docs")
        );
        assert!(!policy.covers(&outside, ReadWrite));
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_workspace_uses_configured_container_and_resolved_target() {
        use SandboxPathAccess::{ReadOnly, ReadWrite};
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().canonicalize().unwrap();
        let container = home.join(".clai/workspaces");
        let target = home.join("projects/ws");
        let notes = home.join("projects/notes");
        let sibling = container.join("other");
        for path in [&container, &target.join("docs"), &notes, &sibling] {
            fs::create_dir_all(path).unwrap();
        }
        let workspace = container.join("own");
        std::os::unix::fs::symlink(&target, &workspace).unwrap();
        std::os::unix::fs::symlink(&sibling, target.join("escape")).unwrap();
        let policy = EffectiveFilesystemPolicy::compile(
            &workspace,
            &[
                grant(&home, ReadWrite),
                grant(&notes, ReadOnly),
                grant(&target.join("docs"), ReadOnly),
            ],
            Some(&home),
        )
        .unwrap();
        assert_eq!(policy.workspace_root, target);
        assert_eq!(policy.mask, Some(container));
        assert_eq!(policy.path_grants, vec![grant(&home, ReadWrite)]);
        assert_eq!(policy.authorize_cwd(".").unwrap(), target);
        assert!(policy.authorize_cwd("escape").is_err());
        assert_eq!(
            policy.authorize_cwd(notes.to_str().unwrap()).unwrap(),
            notes
        );
        assert!(!policy.covers(&sibling, FilesystemPathAccess::ReadOnly));
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_container_resolves_mask_grants_and_workspace_together() {
        use SandboxPathAccess::{ReadOnly, ReadWrite};
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().canonicalize().unwrap();
        let real = home.join("state/clai/workspaces");
        let sibling = real.join("other");
        fs::create_dir_all(real.join("own/docs")).unwrap();
        fs::create_dir_all(&sibling).unwrap();
        std::os::unix::fs::symlink(home.join("state/clai"), home.join(".clai")).unwrap();
        let alias = home.join("shortcut");
        std::os::unix::fs::symlink(&sibling, &alias).unwrap();
        let workspace = home.join(".clai/workspaces/own");
        let policy = EffectiveFilesystemPolicy::compile(
            &workspace,
            &[
                grant(&home, ReadWrite),
                grant(&real.join("own/docs"), ReadOnly),
            ],
            Some(&home),
        )
        .unwrap();
        assert_eq!(policy.mask, Some(real.clone()));
        assert_eq!(policy.path_grants, vec![grant(&home, ReadWrite)]);
        assert!(!policy.covers(&sibling, FilesystemPathAccess::ReadOnly));
        assert!(policy.covers(&real.join("own/docs"), FilesystemPathAccess::ReadWrite));
        let explicit = EffectiveFilesystemPolicy::compile(
            &workspace,
            &[grant(&home, ReadWrite), grant(&alias, ReadOnly)],
            Some(&home),
        )
        .unwrap();
        assert!(explicit.path_grants.contains(&grant(&sibling, ReadOnly)));
        assert_eq!(
            explicit.authorize_cwd(alias.to_str().unwrap()).unwrap(),
            sibling
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn host_resolution_preserves_path_bytes_and_rejects_unavailable_targets() {
        use std::os::unix::ffi::OsStringExt;
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let target = root.join(std::ffi::OsString::from_vec(b"target-\xff\n".to_vec()));
        fs::create_dir(&target).unwrap();
        let alias = root.join("alias");
        std::os::unix::fs::symlink(&target, &alias).unwrap();
        assert_eq!(resolve_on_host(&alias).unwrap(), target);
        fs::remove_dir(&target).unwrap();
        assert!(resolve_on_host(&alias).is_err());
        assert!(resolve_on_host(&root.join("missing")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn dangling_optional_grant_is_omitted_but_workspace_and_cwd_remain_strict() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let workspace = root.join("workspace");
        fs::create_dir(&workspace).unwrap();
        let link = root.join("dangling");
        std::os::unix::fs::symlink(root.join("missing"), &link).unwrap();
        let grant = SandboxPathGrant {
            host_path: link.clone(),
            access: SandboxPathAccess::ReadWrite,
        };
        let policy = EffectiveFilesystemPolicy::compile(&workspace, &[grant], None).unwrap();
        assert!(policy.path_grants.is_empty());
        assert_eq!(policy.authorize_cwd(".").unwrap(), workspace);
        assert!(policy.authorize_cwd(link.to_str().unwrap()).is_err());
        assert!(!policy.covers(&link, FilesystemPathAccess::ReadWrite));
        assert!(EffectiveFilesystemPolicy::compile(&link, &[], None).is_err());
    }
}
