//! Persistent per-workspace scratch space for sandboxed commands.
//!
//! # Why
//!
//! Agents treat `/tmp` the way any Unix program does: a place to park build
//! caches, downloaded modules, and intermediate files. Both sandbox backends
//! used to hand every command a *fresh empty* temp dir — bwrap via
//! `--tmpfs /tmp`, seatbelt via a per-command private dir — so anything an
//! agent wrote there vanished before its next command. The visible cost is
//! wasted work: a Go or cargo build re-downloads its whole dependency set on
//! every single invocation.
//!
//! This module gives each workspace one scratch directory that survives
//! *between* commands, which is the only persistence the waste problem needs.
//!
//! # Why not simply grant the host `/tmp`
//!
//! Because host `/tmp` is a shared, world-writable namespace. Binding it into
//! every sandbox would let an agent in one workspace read and clobber another
//! workspace's scratch files, and would expose live host IPC endpoints
//! (session-bus sockets, keyring sockets, X11) to agents that were never
//! granted them. On many distributions `/tmp` is also a RAM-backed tmpfs,
//! which is the wrong place for the multi-gigabyte build caches this exists to
//! hold.
//!
//! # Where it lives, and why that placement *is* the isolation
//!
//! The scratch directory is `<workspace-container>/.scratch/<workspace-id>` —
//! a sibling of the workspace roots, inside the container that
//! [`super::profile::workspace_mask`] already hides.
//!
//! That placement is deliberate and load-bearing. The container is masked on
//! every surface an agent can reach: the bwrap mount set, the seatbelt profile,
//! and the in-process `fs_*` path validator. Putting scratch inside it means
//! one workspace cannot read or write another's scratch through *any* of them,
//! without a single line of new masking logic.
//!
//! The inverse is what makes this worth stating: a location under `$HOME` but
//! outside the container (say the OS cache directory) is **not** safe here,
//! because a new agent's default grants include `$HOME` read-only. A broad home
//! grant re-exposes every other workspace's scratch at its real path. Isolation
//! comes from the placement, not from the directory being private-looking.
//!
//! Consequently, when the container cannot be masked
//! ([`super::profile::workspace_mask`] returns `None` — a workspace outside
//! `$HOME`, or sitting directly at `$HOME`) this module **fails closed** and
//! provides no scratch at all, rather than creating an unprotected directory.
//!
//! # Growth control
//!
//! A directory that persists forever is a disk leak, so it is reclaimed from
//! two directions:
//!
//! 1. **Reset on first use per app session.** The first sandboxed command a
//!    workspace runs after CLAI starts gets a clean directory. This mirrors the
//!    way a real `/tmp` is cleared at boot, and means nothing accumulates
//!    across restarts.
//! 2. **Idle reclaim.** Workspaces that are never reopened would never hit (1),
//!    so the same first-use pass also drops sibling directories idle past
//!    [`MAX_IDLE_AGE`] and drains anything awaiting deletion.
//!
//! Both are bounds on *time*, not on size: nothing caps how large a single
//! workspace's scratch can grow within one session. Note also that this moves
//! temp data that previously lived in a RAM-backed tmpfs (capped, and freed at
//! command exit) onto the disk holding the workspace.
//!
//! Neither runs on the startup path. Reclamation is lazy — it happens on a
//! workspace's first sandboxed command, not at launch — so app start does no
//! filesystem work and there is no background task to wire up, keep alive, or
//! accidentally leave dead. The first-use pass is O(1) in the size of the data
//! it discards, because it *renames* directories aside and unlinks them on a
//! background thread; discarding a multi-gigabyte build cache never blocks an
//! agent's command.
//!
//! # Layout
//!
//! ```text
//! <workspace-container>/.scratch/
//!   <workspace-id>/     # bound at /tmp (Linux) or pointed to by TMPDIR (macOS)
//!   .trash/<nonce>/     # renamed aside, awaiting background unlink
//! ```
//!
//! # Known limitation
//!
//! "First use per app session" is process-local. Two CLAI instances sharing one
//! container will each reset a workspace's scratch on their own first use, so
//! one can discard a directory the other is actively using. Only regenerable
//! scratch is at risk, so this is accepted rather than defended with a
//! cross-process lock.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, SystemTime};

/// Scratch container, as a child of the workspace container. Dot-prefixed so it
/// is visually distinct from the workspace-id directories beside it, and so the
/// workspace index skips it. Shared with the `fs_*` tools via `profile`.
use super::profile::SCRATCH_DIR_NAME as SCRATCH_DIR;
/// Holds directories renamed out of use, awaiting unlink.
const TRASH_DIR: &str = ".trash";

/// A workspace's scratch dir is reclaimed once it has gone unused for this
/// long. Only applies to workspaces not used in the current app session;
/// anything reset this session is skipped outright.
const MAX_IDLE_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// Cap on the human-readable half of a scratch id, so a pathological workspace
/// directory name cannot produce a filename the OS rejects.
const MAX_ID_STEM: usize = 40;

/// Scratch is private to one workspace, so it is created 0700 rather than
/// inheriting the 1777 semantics of a shared `/tmp`.
#[cfg(unix)]
const SCRATCH_MODE: u32 = 0o700;

/// Root of a container's scratch tree, or `None` when the container cannot be
/// masked.
///
/// Fails closed on purpose: without the container mask the scratch directory
/// would be reachable by other workspaces through a broad `$HOME` grant, and no
/// scratch at all is better than unprotected scratch. See the module docs.
fn scratch_root(workspace_root: &Path, home: Option<&Path>) -> Option<PathBuf> {
    let container = super::profile::workspace_mask(workspace_root, home)?;
    Some(container.join(SCRATCH_DIR))
}

/// FNV-1a. Hand-rolled rather than using `DefaultHasher` because the id is a
/// *directory name*: `DefaultHasher`'s output is explicitly not guaranteed
/// stable across Rust releases, so an upgrade would silently orphan every
/// existing scratch dir. FNV-1a is fixed by its specification.
fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Stable directory name for a workspace's scratch space.
///
/// Shaped as `<readable-stem>-<hash>`: the stem makes the directory
/// recognisable when a user inspects the container, and the hash of the *full*
/// path is what actually guarantees uniqueness — two workspaces can share a
/// basename under different parents.
///
/// The trailing `-<hash>` also makes the result structurally incapable of being
/// `.`, `..`, or the reserved `.trash`, so it can never escape or collide with
/// the tree's own bookkeeping.
pub(crate) fn workspace_scratch_id(workspace_root: &Path) -> String {
    let full = workspace_root.to_string_lossy();
    let hash = fnv1a64(full.as_bytes());

    let stem: String = workspace_root
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .take(MAX_ID_STEM)
        .collect();

    let stem = if stem.is_empty() {
        "ws".to_string()
    } else {
        stem
    };
    format!("{stem}-{hash:016x}")
}

/// Scratch ids already reset during this app session.
fn initialized() -> &'static Mutex<HashSet<String>> {
    static SET: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    SET.get_or_init(|| Mutex::new(HashSet::new()))
}

/// Recover a poisoned lock rather than propagating the panic. The set is
/// advisory — worst case after a poisoning we reset a scratch dir one extra
/// time, which is harmless — so refusing to run would be the worse failure.
fn lock_initialized() -> MutexGuard<'static, HashSet<String>> {
    initialized().lock().unwrap_or_else(|e| e.into_inner())
}

/// Resolve this workspace's scratch directory, resetting it if this is the
/// first sandboxed command the workspace has run in this app session, and
/// reclaiming idle siblings at the same time.
///
/// Returns `None` when scratch is unavailable — the container cannot be masked,
/// or the directory could not be created. Callers must treat that as "fall back
/// to the ephemeral behaviour", never as a command failure: scratch is an
/// optimisation, and losing it must not take the agent's shell down with it.
pub(crate) fn ensure_session_scratch(
    workspace_root: &Path,
    home: Option<&Path>,
) -> Option<PathBuf> {
    let root = scratch_root(workspace_root, home)?;
    let id = workspace_scratch_id(workspace_root);
    let live = root.join(&id);

    // Held across the reset so two commands racing on a workspace's first use
    // cannot both rename the directory aside, and so the reclaim pass cannot
    // observe a half-initialised state.
    let mut guard = lock_initialized();

    if guard.contains(&id) {
        // Already reset this session. Re-create only if it went missing, and
        // insist on a real directory: a symlink here would be followed by the
        // bwrap bind and by seatbelt's canonicalisation, redirecting the
        // agent's /tmp somewhere we never chose.
        if !is_real_dir(&live) {
            if let Err(error) = reset_scratch_dir(&root, &live) {
                tracing::warn!("Falling back to an ephemeral sandbox temp directory: {error}");
                return None;
            }
        }
        return Some(live);
    }

    if let Err(error) = reset_scratch_dir(&root, &live) {
        tracing::warn!("Falling back to an ephemeral sandbox temp directory: {error}");
        return None;
    }

    // Reclaim while we still hold the guard, so a sibling cannot be marked
    // in-use between the decision and the delete.
    reclaim(&root, &guard, &id);

    guard.insert(id);
    Some(live)
}

/// True when `path` is a directory and not a symlink.
///
/// `symlink_metadata` does not follow the final component, which is the point:
/// `create_dir_all` succeeds on a symlink-to-directory, and both sandbox
/// backends would then resolve it — bwrap by binding the target at `/tmp`,
/// seatbelt by canonicalising it into an allow rule. Everything this module
/// binds or deletes must be a real directory it created.
fn is_real_dir(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .map(|meta| meta.is_dir())
        .unwrap_or(false)
}

/// Clear `live` by renaming it aside and recreating it empty.
///
/// The rename is what keeps this O(1): moving a directory within the same
/// filesystem is a single syscall no matter how large the tree is, so the
/// agent's first command never waits on unlinking a build cache.
///
/// Returns the trash path the old tree was moved to, if there was one — which
/// is what makes the rename-aside behaviour observable to tests.
fn reset_scratch_dir(root: &Path, live: &Path) -> Result<Option<PathBuf>, String> {
    // Create the root explicitly before its children: `create_dir_all` only
    // applies the mode to the final component, so building `.scratch/.trash`
    // in one call would leave `.scratch` itself at the umask default.
    create_private_dir_all(root)
        .map_err(|e| format!("could not create {}: {e}", root.display()))?;
    let trash = root.join(TRASH_DIR);
    create_private_dir_all(&trash)
        .map_err(|e| format!("could not create {}: {e}", trash.display()))?;

    let mut moved = None;
    // `symlink_metadata` rather than `exists()`: a dangling or hostile symlink
    // must be cleared too, and `exists()` follows links.
    if fs::symlink_metadata(live).is_ok() {
        let target = trash.join(uuid::Uuid::new_v4().to_string());
        match fs::rename(live, &target) {
            Ok(()) => {
                spawn_delete(root.to_path_buf(), target.clone());
                moved = Some(target);
            }
            Err(error) => {
                // Rename should not fail within one filesystem, but if it does,
                // fall back to deleting in place. Correctness first: a slow
                // reset beats a stale scratch dir.
                tracing::warn!(
                    "Could not rename sandbox scratch directory {} aside ({error}); \
                     removing it in place",
                    live.display()
                );
                remove_entry(root, live);
            }
        }
    }

    create_private_dir_all(live)
        .map_err(|e| format!("could not create {}: {e}", live.display()))?;
    Ok(moved)
}

/// `create_dir_all` with private permissions on Unix.
fn create_private_dir_all(path: &Path) -> std::io::Result<()> {
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // Best-effort: an existing directory with other permissions is not a
        // reason to fail the command.
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(SCRATCH_MODE));
    }
    Ok(())
}

/// Drain `.trash/` and drop sibling scratch dirs idle past [`MAX_IDLE_AGE`].
///
/// Runs once per workspace per session, under the `initialized` guard. Bounded
/// work: one `read_dir` of each of two small directories, with the actual
/// unlinking handed to a background thread.
fn reclaim(root: &Path, in_use: &HashSet<String>, current: &str) {
    // Trash is unconditionally disposable — entries only land there after being
    // renamed out of use.
    if let Ok(entries) = fs::read_dir(root.join(TRASH_DIR)) {
        for entry in entries.flatten() {
            spawn_delete(root.to_path_buf(), entry.path());
        }
    }

    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == TRASH_DIR || name == current || in_use.contains(&name) {
            continue;
        }
        // A workspace used this session owns its directory regardless of its
        // timestamp, so membership is checked before mtime: a build writing
        // deep inside the tree does not bump the top-level directory's mtime,
        // and reclaiming a live scratch dir would delete work in progress.
        if is_idle_past(&entry.path(), MAX_IDLE_AGE) {
            // Rename aside first so the caller never waits on a large unlink.
            let target = root.join(TRASH_DIR).join(uuid::Uuid::new_v4().to_string());
            match fs::rename(entry.path(), &target) {
                Ok(()) => spawn_delete(root.to_path_buf(), target),
                Err(_) => spawn_delete(root.to_path_buf(), entry.path()),
            }
        }
    }
}

/// Unlink a tree off the caller's critical path when a runtime is available.
///
/// Falls back to a synchronous delete outside a runtime (unit tests) so the
/// tree is never simply leaked. Callers are always inside an async tool task in
/// production, so the background branch is the live one.
fn spawn_delete(root: PathBuf, path: PathBuf) {
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => {
            handle.spawn_blocking(move || remove_entry(&root, &path));
        }
        Err(_) => remove_entry(&root, &path),
    }
}

/// True when `path` is a direct child of the scratch root, or of its `.trash/`.
///
/// Every delete in this module is gated on this. The check is deliberately
/// exact rather than a prefix test: `starts_with` alone would accept the roots
/// themselves and any depth beneath them, and this code's whole job is removing
/// directories recursively. Confining deletion to one known shape keeps the
/// blast radius of a path bug to a regenerable scratch entry.
fn is_deletable_scratch_path(root: &Path, path: &Path) -> bool {
    let Some(parent) = path.parent() else {
        return false;
    };
    if parent != root && parent != root.join(TRASH_DIR) {
        return false;
    }
    // Reject `.`/`..` as the final component so the target cannot resolve back
    // up to the root itself, and never delete the trash directory.
    match path.file_name().and_then(|n| n.to_str()) {
        Some(name) => name != "." && name != ".." && !(parent == root && name == TRASH_DIR),
        None => false,
    }
}

/// Remove a scratch entry, refusing anything outside the tree or reached
/// through a symlink.
fn remove_entry(root: &Path, path: &Path) {
    if !is_deletable_scratch_path(root, path) {
        tracing::error!(
            "Refusing to remove {} — outside the sandbox scratch tree",
            path.display()
        );
        return;
    }

    // Structural containment is not enough on its own: if the entry (or the
    // root it hangs off) were a symlink, a lexically in-shape path would still
    // resolve elsewhere. Unlink the link itself instead of recursing through
    // it.
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_symlink() => {
            let _ = fs::remove_file(path);
            return;
        }
        Ok(meta) if !meta.is_dir() => {
            let _ = fs::remove_file(path);
            return;
        }
        Ok(_) => {}
        Err(_) => return,
    }

    if let Err(error) = fs::remove_dir_all(path) {
        if error.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!(
                "Failed to remove sandbox scratch directory {}: {error}",
                path.display()
            );
        }
    }
}

/// Whether `path`'s last modification is older than `max_age`.
///
/// Unreadable metadata and clock skew both yield `false`: reclamation must
/// never delete on missing evidence, and leaving a stale directory one more
/// cycle is strictly cheaper than deleting a live one.
fn is_idle_past(path: &Path, max_age: Duration) -> bool {
    let Ok(modified) = fs::metadata(path).and_then(|m| m.modified()) else {
        return false;
    };
    SystemTime::now()
        .duration_since(modified)
        .map(|age| age > max_age)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a container layout: `<home>/.clai/workspaces/<name>`, which is
    /// what `workspace_mask` recognises.
    fn workspace_in(home: &Path, name: &str) -> PathBuf {
        let ws = home.join(".clai").join("workspaces").join(name);
        fs::create_dir_all(&ws).unwrap();
        ws
    }

    #[test]
    fn scratch_id_is_stable_for_the_same_path() {
        let a = workspace_scratch_id(Path::new("/home/u/.clai/workspaces/abc"));
        let b = workspace_scratch_id(Path::new("/home/u/.clai/workspaces/abc"));
        assert_eq!(a, b);
    }

    /// The whole point of hashing the full path rather than the basename: same
    /// leaf name, different parents, must not collide.
    #[test]
    fn scratch_id_differs_for_same_basename_under_different_parents() {
        let a = workspace_scratch_id(Path::new("/home/u/projects/ws"));
        let b = workspace_scratch_id(Path::new("/srv/other/ws"));
        assert_ne!(a, b);
    }

    /// FNV-1a is pinned by spec; assert known vectors so a refactor that swaps
    /// the hash (and would orphan every existing scratch dir) fails loudly.
    #[test]
    fn fnv1a_matches_the_reference_vector() {
        assert_eq!(fnv1a64(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a64(b"a"), 0xaf63_dc4c_8601_ec8c);
    }

    /// A workspace directory named with separators or other hostile characters
    /// must not steer the scratch path elsewhere, and must never collide with
    /// the tree's own `.trash` bookkeeping.
    #[test]
    fn scratch_id_sanitizes_hostile_names() {
        let id = workspace_scratch_id(Path::new("/home/u/..")); // file_name() == None
        assert!(!id.contains('/') && !id.contains('\\'));
        assert_ne!(id, "..");
        assert_ne!(id, ".");

        let weird = workspace_scratch_id(Path::new("/home/u/a b:c*d"));
        assert!(!weird.contains(' ') && !weird.contains(':') && !weird.contains('*'));

        assert_ne!(workspace_scratch_id(Path::new("/home/u/.trash")), TRASH_DIR);
    }

    #[test]
    fn scratch_id_stem_is_length_capped() {
        let long = "x".repeat(500);
        let id = workspace_scratch_id(Path::new(&format!("/home/u/{long}")));
        assert!(
            id.len() <= MAX_ID_STEM + 1 + 16,
            "unexpected length: {}",
            id.len()
        );
    }

    /// Isolation comes from placement: scratch must sit inside the container
    /// that `workspace_mask` already hides from every access surface.
    #[test]
    fn scratch_root_is_inside_the_masked_workspace_container() {
        let home = Path::new("/home/u");
        let ws = Path::new("/home/u/.clai/workspaces/abc");
        assert_eq!(
            scratch_root(ws, Some(home)),
            Some(PathBuf::from("/home/u/.clai/workspaces/.scratch"))
        );
    }

    /// Fail closed: with no container mask there is nothing hiding the scratch
    /// dir from a broad `$HOME` grant, so we must provide none at all.
    #[test]
    fn scratch_root_fails_closed_when_the_container_cannot_be_masked() {
        // Workspace outside home.
        assert_eq!(
            scratch_root(Path::new("/srv/ws/abc"), Some(Path::new("/home/u"))),
            None
        );
        // Workspace directly at home: masking its parent would hide all of home.
        assert_eq!(
            scratch_root(Path::new("/home/u/abc"), Some(Path::new("/home/u"))),
            None
        );
        // Home unknown.
        assert_eq!(
            scratch_root(Path::new("/home/u/.clai/workspaces/abc"), None),
            None
        );
    }

    #[test]
    fn ensure_session_scratch_is_none_when_failing_closed() {
        let dir = tempfile::tempdir().unwrap();
        let outside = dir.path().join("ws");
        fs::create_dir_all(&outside).unwrap();
        assert_eq!(
            ensure_session_scratch(&outside, Some(Path::new("/nonexistent-home"))),
            None
        );
    }

    /// First use of a workspace in an app session must hand it a CLEAN
    /// directory — that reset is what bounds growth across restarts.
    #[test]
    fn first_use_clears_leftover_contents() {
        let home = tempfile::tempdir().unwrap();
        let ws = workspace_in(home.path(), "ws-first-use");

        let root = home
            .path()
            .join(".clai")
            .join("workspaces")
            .join(SCRATCH_DIR);
        let live = root.join(workspace_scratch_id(&ws));
        fs::create_dir_all(&live).unwrap();
        fs::write(live.join("stale.txt"), "old").unwrap();

        let resolved = ensure_session_scratch(&ws, Some(home.path())).expect("scratch");

        assert_eq!(resolved, live);
        assert!(resolved.is_dir());
        assert!(
            !resolved.join("stale.txt").exists(),
            "first use did not clear the previous session's contents"
        );
    }

    /// ...but every later command in the same session must reuse it, or files
    /// would still vanish between commands and the whole fix would be moot.
    #[test]
    fn later_uses_in_the_same_session_preserve_contents() {
        let home = tempfile::tempdir().unwrap();
        let ws = workspace_in(home.path(), "ws-same-session");

        let first = ensure_session_scratch(&ws, Some(home.path())).expect("scratch");
        fs::write(first.join("build-cache"), "expensive").unwrap();

        let second = ensure_session_scratch(&ws, Some(home.path())).expect("scratch");

        assert_eq!(first, second);
        assert_eq!(
            fs::read_to_string(second.join("build-cache")).unwrap(),
            "expensive",
            "a later command in the same session lost the scratch contents"
        );
    }

    /// Two workspaces must never share scratch space.
    #[test]
    fn separate_workspaces_get_separate_scratch_dirs() {
        let home = tempfile::tempdir().unwrap();
        let a = ensure_session_scratch(&workspace_in(home.path(), "ws-a"), Some(home.path()));
        let b = ensure_session_scratch(&workspace_in(home.path(), "ws-b"), Some(home.path()));
        assert!(a.is_some() && b.is_some());
        assert_ne!(a, b);
    }

    /// Scratch is private to one workspace.
    #[cfg(unix)]
    #[test]
    fn scratch_dir_is_created_private() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().unwrap();
        let ws = workspace_in(home.path(), "ws-perms");
        let live = ensure_session_scratch(&ws, Some(home.path())).expect("scratch");
        let mode = fs::metadata(&live).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, SCRATCH_MODE,
            "scratch dir should be 0700, got {mode:o}"
        );
    }

    /// The reset must not block on unlinking: it renames the old tree into
    /// `.trash/` and defers the delete. Asserting the returned trash path is
    /// what makes that observable — the previous version of this test passed
    /// equally for an in-place delete.
    #[test]
    fn reset_renames_the_old_tree_into_trash_rather_than_deleting_inline() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(SCRATCH_DIR);
        let live = root.join("ws-deadbeefdeadbeef");
        fs::create_dir_all(&live).unwrap();
        fs::write(live.join("big"), "payload").unwrap();

        let moved = reset_scratch_dir(&root, &live).expect("reset");

        let moved = moved.expect("old tree should have been renamed aside, not deleted in place");
        assert_eq!(moved.parent(), Some(root.join(TRASH_DIR).as_path()));
        assert!(live.is_dir(), "live dir was not recreated");
        assert!(!live.join("big").exists(), "live dir was not cleared");
    }

    /// The root holds every workspace's scratch, so it must be private too —
    /// `create_dir_all` only chmods its final component.
    #[cfg(unix)]
    #[test]
    fn scratch_root_and_trash_are_created_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(SCRATCH_DIR);
        let live = root.join("ws-0000000000000000");

        reset_scratch_dir(&root, &live).expect("reset");

        for path in [&root, &root.join(TRASH_DIR), &live] {
            let mode = fs::metadata(path).unwrap().permissions().mode() & 0o777;
            assert_eq!(
                mode,
                SCRATCH_MODE,
                "{} should be 0700, got {mode:o}",
                path.display()
            );
        }
    }

    /// Nothing to move on a first-ever use.
    #[test]
    fn reset_reports_no_move_when_there_was_nothing_to_clear() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(SCRATCH_DIR);
        let live = root.join("ws-0000000000000000");

        assert_eq!(reset_scratch_dir(&root, &live).expect("reset"), None);
        assert!(live.is_dir());
    }

    /// A symlink left where the scratch dir belongs must be cleared, not
    /// followed — otherwise bwrap would bind its target at /tmp.
    #[cfg(unix)]
    #[test]
    fn reset_replaces_a_symlink_with_a_real_directory() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(SCRATCH_DIR);
        fs::create_dir_all(&root).unwrap();
        let elsewhere = dir.path().join("elsewhere");
        fs::create_dir_all(&elsewhere).unwrap();
        fs::write(elsewhere.join("victim"), "x").unwrap();

        let live = root.join("ws-1234567890abcdef");
        std::os::unix::fs::symlink(&elsewhere, &live).unwrap();

        reset_scratch_dir(&root, &live).expect("reset");

        assert!(
            is_real_dir(&live),
            "symlink was not replaced by a real directory"
        );
        assert!(
            elsewhere.join("victim").exists(),
            "clearing the symlink must not delete through it"
        );
    }

    #[test]
    fn is_real_dir_rejects_symlinks_and_files() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        fs::create_dir(&real).unwrap();
        assert!(is_real_dir(&real));

        let file = dir.path().join("file");
        fs::write(&file, "x").unwrap();
        assert!(!is_real_dir(&file));
        assert!(!is_real_dir(&dir.path().join("missing")));

        #[cfg(unix)]
        {
            let link = dir.path().join("link");
            std::os::unix::fs::symlink(&real, &link).unwrap();
            assert!(
                !is_real_dir(&link),
                "a symlink to a dir must not count as a real dir"
            );
        }
    }

    /// The delete guard is the last line of defence for a recursive remove, so
    /// pin the exact shape it accepts.
    #[test]
    fn delete_guard_accepts_only_direct_children_of_the_root_and_trash() {
        let root = Path::new("/c/.clai/workspaces/.scratch");

        assert!(is_deletable_scratch_path(
            root,
            &root.join("ws-0123456789abcdef")
        ));
        assert!(is_deletable_scratch_path(
            root,
            &root.join(TRASH_DIR).join("nonce")
        ));

        // The roots themselves are never removable.
        assert!(!is_deletable_scratch_path(root, root));
        assert!(!is_deletable_scratch_path(root, &root.join(TRASH_DIR)));

        // Only whole scratch entries, never paths inside one.
        assert!(!is_deletable_scratch_path(
            root,
            &root.join("ws").join("nested")
        ));

        // Anything outside the tree is rejected outright.
        assert!(!is_deletable_scratch_path(root, Path::new("/")));
        assert!(!is_deletable_scratch_path(root, Path::new("/home/u")));
        assert!(!is_deletable_scratch_path(root, &root.join("..")));
        // A sibling sharing the root's prefix must not match.
        assert!(!is_deletable_scratch_path(
            root,
            Path::new("/c/.clai/workspaces/.scratch-x/ws")
        ));
    }

    /// `remove_entry` must be inert on paths outside the tree even when they
    /// exist and are writable — the guard, not the caller, is what makes
    /// recursive deletion safe here.
    #[test]
    fn remove_entry_refuses_paths_outside_the_scratch_tree() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(SCRATCH_DIR);
        let victim = dir.path().join("precious");
        fs::create_dir_all(&victim).unwrap();

        remove_entry(&root, &victim);

        assert!(
            victim.exists(),
            "guard failed to protect a path outside the scratch tree"
        );
    }

    /// Even a lexically in-shape entry must not be recursed through when it is
    /// a symlink: the link is unlinked, its target left alone.
    #[cfg(unix)]
    #[test]
    fn remove_entry_unlinks_a_symlink_instead_of_following_it() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(SCRATCH_DIR);
        fs::create_dir_all(&root).unwrap();
        let elsewhere = dir.path().join("elsewhere");
        fs::create_dir_all(&elsewhere).unwrap();
        fs::write(elsewhere.join("victim"), "x").unwrap();

        let link = root.join("ws-aaaaaaaaaaaaaaaa");
        std::os::unix::fs::symlink(&elsewhere, &link).unwrap();

        remove_entry(&root, &link);

        assert!(!link.exists(), "symlink should have been unlinked");
        assert!(elsewhere.join("victim").exists(), "target must survive");
    }

    #[test]
    fn idle_check_is_false_for_a_fresh_directory() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!is_idle_past(dir.path(), MAX_IDLE_AGE));
    }

    /// Missing metadata must not be read as "old enough to delete".
    #[test]
    fn idle_check_is_false_when_metadata_is_unreadable() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!is_idle_past(
            &dir.path().join("does-not-exist"),
            MAX_IDLE_AGE
        ));
    }

    #[test]
    fn idle_check_is_true_past_the_age_bound() {
        let dir = tempfile::tempdir().unwrap();
        assert!(is_idle_past(dir.path(), Duration::from_secs(0)));
    }

    #[test]
    fn reclaim_drains_the_trash() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(SCRATCH_DIR);
        let doomed = root.join(TRASH_DIR).join("nonce-1");
        fs::create_dir_all(doomed.join("nested")).unwrap();
        fs::write(doomed.join("nested").join("f"), "x").unwrap();

        reclaim(&root, &HashSet::new(), "ws-current");

        assert!(!doomed.exists(), "reclaim left trash behind");
    }

    /// Reclaim must not touch a workspace in use this session, nor the one
    /// currently being initialised, even if their mtimes look old.
    #[test]
    fn reclaim_keeps_in_use_and_current_scratch_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(SCRATCH_DIR);
        let in_use = root.join("ws-inuse00000000");
        let current = root.join("ws-current000000");
        fs::create_dir_all(&in_use).unwrap();
        fs::create_dir_all(&current).unwrap();

        let mut set = HashSet::new();
        set.insert("ws-inuse00000000".to_string());
        reclaim(&root, &set, "ws-current000000");

        assert!(
            in_use.exists(),
            "reclaim deleted a scratch dir in use this session"
        );
        assert!(
            current.exists(),
            "reclaim deleted the scratch dir being initialised"
        );
    }

    /// A fresh, unused directory is inside the age bound and must survive.
    #[test]
    fn reclaim_keeps_recent_unused_scratch_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(SCRATCH_DIR);
        let stranger = root.join("ws-1111111111111111");
        fs::create_dir_all(&stranger).unwrap();

        reclaim(&root, &HashSet::new(), "ws-current");

        assert!(
            stranger.exists(),
            "reclaim took a directory inside the age bound"
        );
    }

    /// Reclaim must never remove its own bookkeeping directory.
    #[test]
    fn reclaim_never_removes_the_trash_directory_itself() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(SCRATCH_DIR);
        let trash = root.join(TRASH_DIR);
        fs::create_dir_all(&trash).unwrap();

        reclaim(&root, &HashSet::new(), "ws-current");

        assert!(trash.exists(), "reclaim removed the trash directory");
    }
}
