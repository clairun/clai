//! Recursive deletion of trees the app owns but that contain read-only
//! directories. Shared by workspace deletion and the sandbox scratch reaper,
//! which both remove trees agents build caches in.

use std::fs;
use std::path::Path;

/// Give the owner write access to one entry, so the tree it belongs to can
/// be deleted.
///
/// On Unix only directories are touched, set to exactly `0700`: unlinking a
/// file is governed by its *parent* directory's bits, never its own, so
/// relaxing file modes would be collateral damage for nothing. The new mode
/// is a constant rather than the old one plus `u+rwx` so that, if a racing
/// symlink swap redirects the chmod, it can never grant group or other
/// access — the old bits come from the directory that was stat-ed, which
/// whoever wrote the tree chose. On Windows the read-only
/// attribute is cleared on both — std's `remove_dir_all` already deletes
/// read-only entries on NTFS, but its FAT32 fallback does not. Neither
/// platform's ACLs are touched; a `deny write` ACE still blocks deletion.
///
/// Never call this on a symlink: `fs::set_permissions` resolves it and would
/// change the target's permissions, outside the tree being deleted.
fn grant_owner_write(path: &Path, metadata: &fs::Metadata) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if !metadata.is_dir() {
            return Ok(());
        }
        if metadata.permissions().mode() & 0o700 == 0o700 {
            return Ok(());
        }
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
    }
    #[cfg(not(unix))]
    {
        let mut permissions = metadata.permissions();
        if !permissions.readonly() {
            return Ok(());
        }
        // The lint warns that this is world-writable on Unix; this block
        // only compiles off Unix, where clearing the read-only attribute is
        // exactly the operation wanted.
        #[allow(clippy::permissions_set_readonly_false)]
        permissions.set_readonly(false);
        fs::set_permissions(path, permissions)
    }
}

/// Walk `root` granting owner write on the way down, best effort. Each
/// directory is chmod-ed *before* its entries are listed, which is what lets
/// the walk descend into a directory whose read or execute bits were stripped
/// too (`0000`, `0300`): after the chmod it can be listed, before it cannot.
///
/// Symlinks are skipped, not followed: `remove_dir_all` unlinks them without
/// entering, and chmod-ing one would reach outside the tree. The walk is
/// path-based, though, where the removal it precedes is `openat`-hardened: a
/// directory (or one of its ancestors) swapped for a symlink between the stat
/// and the chmod redirects that chmod outside the tree. The redirected chmod
/// can only set a directory the user owns to `0700` — it adds owner bits and
/// may remove group/other access, never grants it. The deletion itself is
/// never redirected.
fn grant_owner_write_recursively(root: &Path) {
    let mut stack = vec![root.to_path_buf()];
    while let Some(path) = stack.pop() {
        let Ok(metadata) = fs::symlink_metadata(&path) else {
            continue;
        };
        if metadata.is_symlink() {
            continue;
        }
        if let Err(error) = grant_owner_write(&path, &metadata) {
            tracing::debug!(path = %path.display(), "Could not grant write access: {}", error);
        }
        if !metadata.is_dir() {
            continue;
        }
        let Ok(entries) = fs::read_dir(&path) else {
            continue;
        };
        for entry in entries.flatten() {
            stack.push(entry.path());
        }
    }
}

/// `fs::remove_dir_all` that also clears the one obstacle the user owns and
/// would otherwise have to clear by hand: directories inside the tree with
/// their write bit stripped. Go's module cache (`pkg/mod`) and Cargo's
/// registry sources publish theirs as `0555`, so a tree that ever held a
/// `go build` cache cannot be deleted — plain `rm -rf` fails on it too, and
/// ownership does not help, because on Unix it is the *directory's* write bit
/// that permits unlinking its children.
///
/// The chmod pass only runs after a `PermissionDenied`, so the common case
/// pays nothing. When the retry fails anyway, the directories it relaxed keep
/// their new modes — acceptable for a tree the caller has decided to delete;
/// the error is returned for it to report.
pub(crate) fn remove_dir_all_forcing_permissions(root: &Path) -> std::io::Result<()> {
    match fs::remove_dir_all(root) {
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            grant_owner_write_recursively(root);
            fs::remove_dir_all(root)
        }
        result => result,
    }
}

/// A `tempfile::TempDir` whose cleanup also clears read-only directories.
///
/// `TempDir` cleans up with a plain `remove_dir_all` and ignores the error, so
/// a test that strips write bits and fails (or returns) before restoring them
/// leaks its whole tree. Tests that create read-only directories use this.
#[cfg(all(test, unix))]
pub(crate) struct ForcedTempDir(tempfile::TempDir);

#[cfg(all(test, unix))]
impl ForcedTempDir {
    pub(crate) fn new() -> Self {
        Self(tempfile::tempdir().unwrap())
    }

    pub(crate) fn path(&self) -> &Path {
        self.0.path()
    }
}

#[cfg(all(test, unix))]
impl Drop for ForcedTempDir {
    fn drop(&mut self) {
        let _ = remove_dir_all_forcing_permissions(self.0.path());
    }
}

// Every test here builds Unix permission fixtures.
#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    // A workspace that ever ran `go build` carries a module cache whose
    // directories are 0555 by design. Deleting it used to fail with EACCES —
    // `rm -rf` fails on it too — even though the user owns every byte.
    #[test]
    fn remove_dir_all_forcing_permissions_deletes_a_read_only_subtree() {
        fn build_module_cache(parent: &Path) -> PathBuf {
            let root = parent.join("ws");
            let cache = root.join(".gopath/pkg/mod/gopkg.in/yaml.v3@v3.0.1");
            fs::create_dir_all(cache.join("nested")).unwrap();
            fs::write(cache.join("yaml.go"), "package yaml").unwrap();
            fs::write(cache.join("nested/decode.go"), "package yaml").unwrap();
            for dir in [cache.join("nested"), cache.clone()] {
                fs::set_permissions(&dir, fs::Permissions::from_mode(0o555)).unwrap();
            }
            root
        }

        let parent = ForcedTempDir::new();
        let root = build_module_cache(parent.path());

        // Guard the fixture: as root the write bits are advisory, and every
        // assertion below would pass without the code under test.
        if fs::write(
            root.join(".gopath/pkg/mod/gopkg.in/yaml.v3@v3.0.1/probe"),
            "x",
        )
        .is_ok()
        {
            return;
        }
        assert!(
            fs::remove_dir_all(&root).is_err(),
            "fixture must be undeletable by a plain remove_dir_all"
        );

        // Same fixture again, deleted the way the app now does it. (The
        // first one, half-deleted by the failed attempt, is cleared too.)
        let second = build_module_cache(&parent.path().join("second"));
        remove_dir_all_forcing_permissions(&second).unwrap();
        assert!(!second.exists());
        remove_dir_all_forcing_permissions(&root).unwrap();
        assert!(!root.exists());
    }

    // A directory that lost read and execute along with write cannot even be
    // listed until it is chmod-ed, which is why the walk relaxes each
    // directory *before* reading its entries. Chmod-ing after the listing
    // leaves the inner 0555 directory untouched and the retry still fails.
    #[test]
    fn remove_dir_all_forcing_permissions_descends_into_an_unreadable_directory() {
        let parent = ForcedTempDir::new();
        let root = parent.path().join("ws");
        let outer = root.join("outer");
        let inner = outer.join("inner");
        fs::create_dir_all(&inner).unwrap();
        fs::write(inner.join("file"), "x").unwrap();
        fs::set_permissions(&inner, fs::Permissions::from_mode(0o555)).unwrap();
        fs::set_permissions(&outer, fs::Permissions::from_mode(0o000)).unwrap();

        if fs::read_dir(&outer).is_ok() {
            return; // running as root: the mode bits are advisory
        }

        remove_dir_all_forcing_permissions(&root).unwrap();
        assert!(!root.exists());
    }

    // The walk must not reach outside the tree: `fs::set_permissions` follows
    // symlinks. On Linux a symlink's own mode is already 0777, so the
    // `is_symlink` guard in the walk is belt-and-braces there and this test
    // passes without it; the guard earns its keep on Windows, where clearing
    // the read-only attribute would land on the target.
    #[test]
    fn remove_dir_all_forcing_permissions_does_not_chmod_through_symlinks() {
        let parent = ForcedTempDir::new();
        let outsider = parent.path().join("outsider");
        fs::create_dir_all(outsider.join("child")).unwrap();
        fs::set_permissions(&outsider, fs::Permissions::from_mode(0o555)).unwrap();

        let root = parent.path().join("ws");
        let locked = root.join("locked");
        fs::create_dir_all(&locked).unwrap();
        std::os::unix::fs::symlink(&outsider, root.join("link")).unwrap();
        fs::write(locked.join("file"), "x").unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o555)).unwrap();

        if fs::write(locked.join("probe"), "x").is_ok() {
            return;
        }

        remove_dir_all_forcing_permissions(&root).unwrap();
        assert!(!root.exists());
        assert!(outsider.exists(), "the symlink target must survive");
        let mode = fs::metadata(&outsider).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o555, "the symlink target must keep its permissions");
    }

    // `TempDir`'s own cleanup would fail silently on this tree and leak it,
    // which is how test runs used to pile read-only trees into the sandbox
    // scratch directory that TMPDIR points at.
    #[test]
    fn forced_temp_dir_cleans_up_a_read_only_tree() {
        let dir = ForcedTempDir::new();
        let path = dir.path().to_path_buf();
        let locked = path.join("locked");
        fs::create_dir_all(&locked).unwrap();
        fs::write(locked.join("file"), "x").unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o555)).unwrap();

        drop(dir);

        assert!(!path.exists(), "the read-only tree leaked");
    }

    // The mode written comes from the stat-ed directory, which an agent
    // controls. If the path is swapped for a symlink before the chmod lands,
    // that mode lands on the symlink's target: copying the old bits plus
    // `u+rwx` would turn a 0577 fixture into a 0777 `~/.ssh`.
    #[test]
    fn grant_owner_write_never_grants_group_or_other_access() {
        let dir = ForcedTempDir::new();
        let stat_ed = dir.path().join("stat-ed");
        fs::create_dir(&stat_ed).unwrap();
        fs::set_permissions(&stat_ed, fs::Permissions::from_mode(0o577)).unwrap();
        let metadata = fs::symlink_metadata(&stat_ed).unwrap();

        // The race, made deterministic: the stat-ed directory is replaced by
        // a symlink to a directory outside the tree.
        let outsider = dir.path().join("outsider");
        fs::create_dir(&outsider).unwrap();
        fs::set_permissions(&outsider, fs::Permissions::from_mode(0o500)).unwrap();
        fs::remove_dir(&stat_ed).unwrap();
        std::os::unix::fs::symlink(&outsider, &stat_ed).unwrap();

        grant_owner_write(&stat_ed, &metadata).unwrap();

        let mode = fs::metadata(&outsider).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode & 0o077, 0, "granted group/other access: {mode:o}");
    }
}
