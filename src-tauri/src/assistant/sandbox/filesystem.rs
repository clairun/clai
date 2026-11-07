//! Compile filesystem identity, isolation and additive access once, before execution.
//! Backends lower this plan; they must not infer destinations from host symlinks.
use super::profile::{workspace_mask, SandboxPathAccess, SandboxPathGrant};
use crate::config::{FilesystemPathAccess, FilesystemPathGrant};
use std::{
    fs,
    path::{Component, Path, PathBuf},
};

/// CLAI's own state in a workspace: its policy (`config.json`), the
/// conversation history (`data.sqlite`) and images, all written by the app
/// from outside the sandbox.
const STATE_DIR: &str = ".clai";
/// The part of [`STATE_DIR`] agents write: their memory, which the app
/// creates before each command.
const MEMORY_DIR: &str = "memory";
/// Project configuration the provider CLIs (Claude Code, Codex, OpenCode)
/// read from their working directory, the workspace root. They run outside
/// the sandbox, so the shell may only read these, like [`STATE_DIR`]:
/// otherwise a command could plant a hook that the next CLI run executes on
/// the host.
pub(crate) const CLI_PROJECT_CONFIG: &[&str] = &[
    ".claude",
    ".mcp.json",
    ".codex",
    "opencode.json",
    "opencode.jsonc",
    ".opencode",
];
/// The directories of [`CLI_PROJECT_CONFIG`]. A mount can only cover a path
/// that exists, so they are created before each command; the files cannot be
/// created empty without breaking the CLIs, which are told to ignore them
/// instead.
const CLI_PROJECT_CONFIG_DIRS: &[&str] = &[".claude", ".codex", ".opencode"];

#[derive(Debug, Clone)]
pub struct EffectiveFilesystemPolicy {
    pub workspace_root: PathBuf,
    pub path_grants: Vec<SandboxPathGrant>,
    pub mask: Option<PathBuf>,
    pub home: Option<PathBuf>,
    /// CLAI's own state, reachable by the shell or not: the app directory
    /// (`~/.clai`), where its symlinks lead, and the workspace's `.clai`.
    /// Other workspaces' `.clai` are recognised by their place in a workspace
    /// directory; see [`Self::is_read_only_state`].
    state: Vec<PathBuf>,
    /// The state under a writable root. The shell may only read it, so an
    /// agent cannot rewrite its own policy, its history or the agent
    /// definitions every workspace shares.
    #[cfg_attr(
        not(any(test, target_os = "linux", target_os = "macos")),
        expect(dead_code, reason = "only the Linux and macOS sandboxes read it")
    )]
    pub read_only_state: Vec<PathBuf>,
    /// Each writable root that holds read-only state and the directories
    /// between them. Renaming one would carry the state away and free its path
    /// to be recreated, so they must not move.
    #[cfg_attr(
        not(any(test, target_os = "linux", target_os = "macos")),
        expect(dead_code, reason = "only the Linux and macOS sandboxes read it")
    )]
    pub pinned: Vec<PathBuf>,
    /// The agent's memory, writable inside its read-only `.clai`.
    pub memory: Option<PathBuf>,
}

impl EffectiveFilesystemPolicy {
    pub fn compile(
        workspace: &Path,
        grants: &[SandboxPathGrant],
        home: Option<&Path>,
    ) -> Result<Self, String> {
        Self::compile_in(
            workspace,
            grants,
            home,
            Some(crate::paths::clai_home().as_path()),
        )
    }

    /// [`Self::compile`] with the app's own directory (`~/.clai`) given.
    pub(crate) fn compile_in(
        workspace: &Path,
        grants: &[SandboxPathGrant],
        home: Option<&Path>,
        clai_home: Option<&Path>,
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
        let own_state = target(workspace_root.join(STATE_DIR));
        let state: Vec<PathBuf> = clai_home
            .filter(|path| path.is_absolute())
            .and_then(|path| resolve(path).ok())
            .into_iter()
            .flat_map(|app| {
                let links: Vec<PathBuf> = entries(&app)
                    .filter(|entry| entry.is_symlink())
                    .filter_map(target)
                    .collect();
                std::iter::once(app).chain(links)
            })
            .chain(state_entries(&workspace_root))
            .collect();
        let memory = own_state
            .map(|state| state.join(MEMORY_DIR))
            .filter(|memory| fs::symlink_metadata(memory).is_ok_and(|m| m.is_dir()));
        let containers = containers(&workspace_root, mask.as_deref());
        // A grant inside the state must not make it writable again.
        for grant in &mut path_grants {
            if is_state(&grant.host_path, &state, &containers, memory.as_deref()) {
                grant.access = SandboxPathAccess::ReadOnly;
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
        let (read_only_state, pinned) = protect(&state, &writes, &containers, mask.as_deref());
        let memory = memory.filter(|memory| read_only_state.iter().any(|s| memory.starts_with(s)));
        Ok(Self {
            workspace_root,
            path_grants,
            mask,
            home,
            state,
            read_only_state,
            pinned,
            memory,
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

    /// Whether writing `path` would change CLAI's own state: the app directory
    /// outside its workspace directories, or any workspace's `.clai` but the
    /// agent's memory, whether it exists yet or not. Grants do not matter, so
    /// a write grant there is lowered to read-only or refused up front even
    /// while the agent cannot reach the path at all.
    pub fn is_read_only_state(&self, path: &Path) -> bool {
        let containers = containers(&self.workspace_root, self.mask.as_deref());
        is_state(path, &self.state, &containers, self.memory.as_deref())
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

    /// The ordered operations a mount-based backend applies. Only the Linux
    /// bwrap backend lowers these; macOS reads the resolved fields directly
    /// and Windows runs commands unsandboxed.
    ///
    /// Mounts are explicit and deeper mounts win: masks come first at equal
    /// depth, then read-only and writable binds, and read-only state last. So
    /// the workspace stays writable inside a read-only `~/.clai`, and the
    /// agent's memory inside its read-only `.clai`.
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
        ops.extend(
            self.read_only_state
                .iter()
                .map(|path| FilesystemOperation::Protect(path)),
        );
        // A mount point cannot be renamed: bind each pinned directory that is
        // not bound yet to itself.
        ops.extend(
            self.pinned
                .iter()
                .filter(|path| {
                    **path != self.workspace_root
                        && !self.path_grants.iter().any(|g| g.host_path == **path)
                })
                .map(|path| FilesystemOperation::Expose {
                    path,
                    access: SandboxPathAccess::ReadWrite,
                    required: false,
                }),
        );
        if let Some(memory) = &self.memory {
            ops.push(FilesystemOperation::Expose {
                path: memory,
                access: SandboxPathAccess::ReadWrite,
                required: false,
            });
        }
        ops.sort_by_key(|op| match op {
            FilesystemOperation::Mask(path) => (path.components().count(), 0),
            FilesystemOperation::Protect(path) => (path.components().count(), 3),
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
    /// Bind read-only over a writable root: CLAI's own state.
    Protect(&'a Path),
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

/// The directories that hold workspaces: the workspace's parent and the
/// masked container, usually the same one.
fn containers(workspace_root: &Path, mask: Option<&Path>) -> Vec<PathBuf> {
    workspace_root
        .parent()
        .into_iter()
        .chain(mask)
        .map(Path::to_path_buf)
        .collect()
}

/// See [`EffectiveFilesystemPolicy::is_read_only_state`].
fn is_state(path: &Path, state: &[PathBuf], containers: &[PathBuf], memory: Option<&Path>) -> bool {
    if memory.is_some_and(|memory| path.starts_with(memory)) {
        return false;
    }
    if containers.iter().any(|container| {
        path.strip_prefix(container).is_ok_and(|rest| {
            rest.components()
                .nth(1)
                .is_some_and(|name| is_state_entry(name.as_os_str()))
        })
    }) {
        return true;
    }
    // The deepest match wins: the workspaces inside the app directory hold
    // the user's files.
    let depth = |roots: &[PathBuf]| {
        roots
            .iter()
            .filter(|root| path.starts_with(root))
            .map(|root| root.components().count())
            .max()
    };
    depth(state) > depth(containers)
}

/// The state a writable root covers, sorted, and what must not move: each
/// such root and the directories below it on the way to the state, up to the
/// first read-only one. A directory bind is used rather than one per entry,
/// so entries created later, and files the app replaces by renaming, stay
/// covered.
///
/// Other workspaces' `.clai` are looked up only when a write grant exists,
/// which spares the common case a directory scan. A write grant on the
/// masked workspace directory itself makes that directory read-only too, so
/// no workspace can be planted in it; its workspaces stay writable, as each is
/// pinned on the way to its `.clai`.
fn protect(
    state: &[PathBuf],
    writes: &[PathBuf],
    containers: &[PathBuf],
    mask: Option<&Path>,
) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let mut candidates = state.to_vec();
    if writes.len() > 1 {
        for container in containers {
            candidates.extend(
                entries(container)
                    .filter_map(target)
                    .flat_map(|workspace| state_entries(&workspace)),
            );
        }
        if let Some(mask) = mask.filter(|mask| writes.iter().any(|root| root == mask)) {
            candidates.push(mask.to_path_buf());
        }
    }
    let mut protected: Vec<PathBuf> = candidates
        .into_iter()
        .filter(|path| writes.iter().any(|root| authorizes(root, path, mask)))
        .collect();
    protected.sort();
    protected.dedup();
    let mut pinned = Vec::new();
    for path in &protected {
        for root in writes
            .iter()
            .filter(|root| *root != path && authorizes(root, path, mask))
        {
            let mut between: Vec<&Path> = path
                .ancestors()
                .skip(1)
                .take_while(|dir| *dir != root.as_path())
                .collect();
            between.reverse();
            pinned.push(root.clone());
            pinned.extend(
                between
                    .into_iter()
                    .take_while(|dir| !protected.iter().any(|p| p == dir))
                    .map(Path::to_path_buf),
            );
        }
    }
    pinned.sort();
    pinned.dedup();
    (protected, pinned)
}

/// Whether a workspace entry is read-only to the shell: CLAI state or CLI
/// project configuration.
fn is_state_entry(name: &std::ffi::OsStr) -> bool {
    name == STATE_DIR || CLI_PROJECT_CONFIG.iter().any(|entry| name == *entry)
}

/// The existing read-only entries of `workspace`, where writes to them land.
fn state_entries(workspace: &Path) -> Vec<PathBuf> {
    std::iter::once(STATE_DIR)
        .chain(CLI_PROJECT_CONFIG.iter().copied())
        .filter_map(|name| target(workspace.join(name)))
        .collect()
}

/// Creates the missing [`CLI_PROJECT_CONFIG_DIRS`] of the workspace, so the
/// sandbox binds them read-only and a command cannot create them.
pub(crate) fn ensure_cli_project_config_dirs(workspace_root: &Path) {
    for name in CLI_PROJECT_CONFIG_DIRS {
        let dir = workspace_root.join(name);
        if fs::symlink_metadata(&dir).is_ok() {
            continue;
        }
        if let Err(error) = fs::create_dir(&dir) {
            tracing::warn!(path = %dir.display(), %error, "Could not create a CLI project configuration directory; the shell can create it");
        }
    }
}

fn entries(dir: &Path) -> impl Iterator<Item = PathBuf> {
    fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
}

/// Where writes to `path` land: `path`, or its target when it is a symlink.
/// `None` when it is missing or dangling.
fn target(path: PathBuf) -> Option<PathBuf> {
    if fs::symlink_metadata(&path).ok()?.file_type().is_symlink() {
        resolve(&path).ok()
    } else {
        Some(path)
    }
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
            FilesystemOperation::Protect(e),
            FilesystemOperation::Expose { path: d, access: ReadWrite, required: true },
        ] if *a == home && *b == container && *c == container && *e == container && *d == workspace));
    }

    /// `<base>/home/.clai/workspaces/{own,other}`, each with its `.clai`, and
    /// the app's config and skills.
    struct ClaiLayout {
        _temp: tempfile::TempDir,
        base: PathBuf,
        home: PathBuf,
        app: PathBuf,
        container: PathBuf,
        workspace: PathBuf,
        sibling: PathBuf,
        state: PathBuf,
    }

    impl ClaiLayout {
        fn new() -> Self {
            let temp = tempfile::tempdir().unwrap();
            let base = temp.path().canonicalize().unwrap();
            let home = base.join("home");
            let app = home.join(".clai");
            let container = app.join("workspaces");
            let workspace = container.join("own");
            let sibling = container.join("other");
            let state = workspace.join(".clai");
            for dir in [
                state.join("memory"),
                sibling.join(".clai"),
                app.join("skills"),
            ] {
                fs::create_dir_all(dir).unwrap();
            }
            fs::write(app.join("config.json"), "{}").unwrap();
            Self {
                _temp: temp,
                base,
                home,
                app,
                container,
                workspace,
                sibling,
                state,
            }
        }

        fn compile(&self, grants: &[SandboxPathGrant]) -> EffectiveFilesystemPolicy {
            EffectiveFilesystemPolicy::compile_in(
                &self.workspace,
                grants,
                Some(&self.home),
                Some(&self.app),
            )
            .unwrap()
        }
    }

    /// E1: the provider CLIs run on the host in the workspace and load project
    /// configuration from it, so the shell may only read that configuration,
    /// in its own workspace and in another one a write grant reaches.
    #[test]
    fn cli_project_config_is_read_only_state() {
        let layout = ClaiLayout::new();
        let ClaiLayout {
            workspace,
            sibling,
            state,
            ..
        } = &layout;
        ensure_cli_project_config_dirs(workspace);
        fs::write(workspace.join(".mcp.json"), "{}").unwrap();
        fs::create_dir(sibling.join(".codex")).unwrap();
        for dir in [".claude", ".codex", ".opencode"] {
            assert!(workspace.join(dir).is_dir(), "{dir}");
        }
        assert!(!workspace.join("opencode.json").exists());

        let policy = layout.compile(&[]);
        let mut expected = vec![
            state.clone(),
            workspace.join(".claude"),
            workspace.join(".codex"),
            workspace.join(".mcp.json"),
            workspace.join(".opencode"),
        ];
        expected.sort();
        assert_eq!(policy.read_only_state, expected);
        for name in CLI_PROJECT_CONFIG {
            assert!(policy.is_read_only_state(&workspace.join(name)), "{name}");
            assert!(
                policy.is_read_only_state(&workspace.join(name).join("x")),
                "{name}"
            );
        }
        assert!(!policy.is_read_only_state(&workspace.join("notes")));
        assert!(!policy.is_read_only_state(&workspace.join("src/.claude.bak")));

        let granted = layout.compile(&[grant(sibling, SandboxPathAccess::ReadWrite)]);
        assert!(granted.read_only_state.contains(&sibling.join(".codex")));
        assert!(granted.is_read_only_state(&sibling.join(".claude/settings.json")));
    }

    #[test]
    fn missing_cli_config_dirs_are_created_and_existing_entries_kept() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path();
        fs::write(workspace.join(".claude"), "a file").unwrap();
        ensure_cli_project_config_dirs(workspace);
        assert_eq!(
            fs::read_to_string(workspace.join(".claude")).unwrap(),
            "a file"
        );
        assert!(workspace.join(".codex").is_dir());
        assert!(workspace.join(".opencode").is_dir());
        for file in [".mcp.json", "opencode.json", "opencode.jsonc"] {
            assert!(!workspace.join(file).exists(), "{file}");
        }
    }

    #[test]
    fn clai_state_refuses_write_grants_whether_the_shell_reaches_it_or_not() {
        let layout = ClaiLayout::new();
        let ClaiLayout {
            home,
            app,
            container,
            workspace,
            sibling,
            state,
            ..
        } = &layout;
        // Read grants leave only the workspace's own state to protect, but
        // every piece of state is refused write grants, existing or not.
        let policy = layout.compile(&[grant(home, SandboxPathAccess::ReadOnly)]);
        assert_eq!(policy.read_only_state, vec![state.clone()]);
        assert_eq!(policy.pinned, vec![workspace.clone()]);
        assert_eq!(policy.memory, Some(state.join("memory")));
        for path in [
            state.join("config.json"),
            app.join("config.json"),
            app.join("skills/new/SKILL.md"),
            app.join("new"),
            sibling.join(".clai/config.json"),
            container.join("planted/.clai"),
        ] {
            assert!(policy.is_read_only_state(&path), "{path:?}");
        }
        for path in [
            state.join("memory/notes.md"),
            workspace.join("notes.md"),
            sibling.join("notes.md"),
            container.clone(),
            home.join("notes.md"),
        ] {
            assert!(!policy.is_read_only_state(&path), "{path:?}");
        }
    }

    #[test]
    fn clai_state_is_read_only_under_every_write_root_except_memory() {
        use SandboxPathAccess::ReadWrite;
        let layout = ClaiLayout::new();
        let ClaiLayout {
            base,
            home,
            app,
            container,
            workspace,
            sibling,
            state,
            ..
        } = &layout;
        let compile = |grants: &[SandboxPathGrant]| layout.compile(grants);

        // A write grant on home makes the whole app directory read-only, not
        // its entries one by one; the masked siblings stay out of reach.
        let policy = compile(&[grant(home, ReadWrite)]);
        assert_eq!(policy.read_only_state, vec![app.clone(), state.clone()]);
        assert_eq!(policy.pinned, vec![home.clone(), workspace.clone()]);

        // A write grant above home pins home itself.
        let policy = compile(&[grant(base, ReadWrite)]);
        assert_eq!(
            policy.pinned,
            vec![base.clone(), home.clone(), workspace.clone()]
        );

        // A write grant on the workspace directory makes it read-only too and
        // pins every workspace in it, and grants inside some state never make
        // it writable. Each read-only bind follows its write root.
        let policy = compile(&[
            grant(home, ReadWrite),
            grant(container, ReadWrite),
            grant(sibling, ReadWrite),
            grant(&state.join("config.json"), ReadWrite),
            grant(&app.join("config.json"), ReadWrite),
        ]);
        assert_eq!(
            policy.read_only_state,
            vec![
                app.clone(),
                container.clone(),
                sibling.join(".clai"),
                state.clone(),
            ]
        );
        assert_eq!(
            policy.pinned,
            vec![
                home.clone(),
                container.clone(),
                sibling.clone(),
                workspace.clone()
            ]
        );
        let operations = policy.operations();
        let protect = |path: &Path| {
            operations
                .iter()
                .position(|op| matches!(op, FilesystemOperation::Protect(p) if *p == path))
                .unwrap()
        };
        let write = |path: &Path| {
            operations
                .iter()
                .position(|op| {
                    matches!(op, FilesystemOperation::Expose { path: p, access: ReadWrite, .. } if *p == path)
                })
                .unwrap()
        };
        assert!(write(home) < protect(app));
        assert!(protect(app) < write(container));
        assert!(write(container) < protect(container));
        assert!(protect(container) < write(sibling));
        assert!(write(sibling) < protect(&sibling.join(".clai")));
        assert!(write(workspace) < protect(state));
        assert!(protect(state) < write(&state.join("memory")));
        for op in &operations {
            if let FilesystemOperation::Expose {
                path,
                access: ReadWrite,
                ..
            } = op
            {
                assert!(!policy.is_read_only_state(path), "{path:?} bound writable");
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn clai_state_is_protected_where_symlinks_lead_and_without_a_home_mask() {
        use SandboxPathAccess::ReadWrite;
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().canonicalize().unwrap();
        let app = home.join(".clai");
        let container = app.join("workspaces");
        let workspace = container.join("own");
        let sibling = container.join("other");
        let state = workspace.join(".clai");
        let skills = home.join("dotfiles/skills");
        let notes = home.join("notes");
        for dir in [&state, &sibling.join(".clai"), &skills, &notes] {
            fs::create_dir_all(dir).unwrap();
        }
        std::os::unix::fs::symlink(&skills, app.join("skills")).unwrap();
        std::os::unix::fs::symlink(&notes, state.join("memory")).unwrap();

        // A symlinked entry is protected where it leads; a symlinked memory is
        // not bound again, and writes through it land wherever it points.
        let policy = EffectiveFilesystemPolicy::compile_in(
            &workspace,
            &[grant(&home, ReadWrite)],
            Some(&home),
            Some(&app),
        )
        .unwrap();
        assert_eq!(
            policy.read_only_state,
            vec![app.clone(), state.clone(), skills.clone()]
        );
        assert!(policy.is_read_only_state(&skills.join("x/SKILL.md")));
        assert_eq!(policy.memory, None);

        // Without a mask (home unknown, or the app directory outside home) the
        // workspace directory is not hidden: it stays read-only under the app
        // directory, and nothing inside that is pinned writable.
        let policy = EffectiveFilesystemPolicy::compile_in(
            &workspace,
            &[grant(&home, ReadWrite)],
            None,
            Some(&app),
        )
        .unwrap();
        assert_eq!(
            policy.read_only_state,
            vec![app.clone(), sibling.join(".clai"), state.clone(), skills]
        );
        assert_eq!(
            policy.pinned,
            vec![home.clone(), workspace.clone(), home.join("dotfiles")]
        );
    }

    #[cfg(unix)]
    #[test]
    fn memory_stays_out_of_reach_when_its_clai_leads_outside_every_write_root() {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        let workspace = base.join("ws");
        let state = base.join("elsewhere");
        fs::create_dir_all(state.join("memory")).unwrap();
        fs::create_dir_all(&workspace).unwrap();
        std::os::unix::fs::symlink(&state, workspace.join(".clai")).unwrap();

        let policy = EffectiveFilesystemPolicy::compile_in(&workspace, &[], None, None).unwrap();
        assert!(policy.read_only_state.is_empty());
        assert_eq!(policy.memory, None);
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
