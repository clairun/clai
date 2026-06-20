//! Filesystem storage for conversation images.
//!
//! Pasted/attached images are stored as files under the workspace root (not as
//! base64 inside `content_json`), so a [`ContentPart::Image`] only carries a
//! lightweight reference. This keeps the DB small, avoids re-sending megabytes
//! of base64 on every history replay, and gives CLI providers a real file path
//! to read (codex `--image`, claude image block, …).
//!
//! [`ContentPart::Image`]: crate::assistant::types::ContentPart::Image

use std::path::{Path, PathBuf};
use uuid::Uuid;

/// Subdirectory (under the workspace root) holding conversation images.
/// Hidden under `.clai/` so it doesn't surface as a user artifact and is
/// removed together with the workspace when the workspace dir is deleted.
pub const IMAGE_STORE_SUBDIR: &str = ".clai/images";

/// Maximum accepted size for a single image (decoded bytes). Bounds DB/token
/// cost and rejects accidental huge pastes.
pub const MAX_IMAGE_BYTES: usize = 10_000_000; // 10 MB

/// A stored image: everything needed to build a `ContentPart::Image`.
pub struct StoredImage {
    /// Stable id (also the on-disk file stem).
    pub id: String,
    /// Path relative to the workspace root (forward-slashed).
    pub path: String,
    /// Canonical MIME type.
    pub media_type: String,
    /// Original filename, if the source had one.
    pub filename: Option<String>,
}

/// Map a supported image MIME type to `(extension, canonical_media_type)`.
///
/// Returns `None` for unsupported types so callers reject them rather than
/// writing arbitrary bytes. The canonical media type matters because the
/// Anthropic API only accepts `image/png|jpeg|gif|webp` — `image/jpg` must be
/// normalized to `image/jpeg`.
pub fn normalize_image_type(media_type: &str) -> Option<(&'static str, &'static str)> {
    match media_type.trim().to_ascii_lowercase().as_str() {
        "image/png" => Some(("png", "image/png")),
        "image/jpeg" | "image/jpg" => Some(("jpg", "image/jpeg")),
        "image/gif" => Some(("gif", "image/gif")),
        "image/webp" => Some(("webp", "image/webp")),
        _ => None,
    }
}

/// Persist `data` as `<root>/.clai/images/<uuid>.<ext>`.
///
/// Pure filesystem logic (no Tauri) so it is unit-testable. Validates size and
/// MIME type before writing; rejects empty, oversized, or unsupported input.
pub fn store_image(
    root: &Path,
    data: &[u8],
    media_type: &str,
    filename: Option<String>,
) -> Result<StoredImage, String> {
    if data.is_empty() {
        return Err("Image data is empty".to_string());
    }
    if data.len() > MAX_IMAGE_BYTES {
        return Err(format!(
            "Image is too large ({} bytes, max {})",
            data.len(),
            MAX_IMAGE_BYTES
        ));
    }
    let (ext, canonical_media_type) = normalize_image_type(media_type)
        .ok_or_else(|| format!("Unsupported image type: {}", media_type))?;

    let dir = root.join(IMAGE_STORE_SUBDIR);
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("Failed to create image store {}: {}", dir.display(), e))?;

    // The filename is a fresh server-generated UUID, so an attacker cannot aim
    // this write at a pre-existing victim file the way a crafted *read* path can
    // (the read sinks defend against symlinked entries via `resolve_store_path`).
    let id = Uuid::new_v4().to_string();
    let rel = format!("{}/{}.{}", IMAGE_STORE_SUBDIR, id, ext);
    let target = root.join(&rel);
    std::fs::write(&target, data)
        .map_err(|e| format!("Failed to write {}: {}", target.display(), e))?;

    Ok(StoredImage {
        id,
        path: rel,
        media_type: canonical_media_type.to_string(),
        filename,
    })
}

/// Validate that `path` is a safe, store-owned image reference: exactly
/// `.clai/images/<uuid>.<ext>` with an allowed extension. Rejects absolute
/// paths, `..`/`.` traversal, nested subdirectories, non-UUID names, and
/// disallowed extensions.
///
/// This is the trust boundary for client-supplied [`crate::assistant::types::ContentPart::Image`]
/// parts on a user send: the send path later does `root.join(path)` and base64s
/// the bytes for the model, so an unchecked absolute or `..` path would let a
/// crafted message read arbitrary local files and exfiltrate them to the
/// provider. Every path that legitimately reaches a message is produced by
/// [`store_image`], which always emits exactly this shape.
pub fn is_store_relative_path(path: &str) -> bool {
    use std::path::{Component, Path};

    let p = Path::new(path);
    if p.is_absolute() {
        return false;
    }
    // Every component must be a plain name — no `..`, `.`, root, or prefix.
    if !p.components().all(|c| matches!(c, Component::Normal(_))) {
        return false;
    }
    // Must live directly under the image store: `.clai/images/<file>`.
    let Some(file) = path.strip_prefix(&format!("{}/", IMAGE_STORE_SUBDIR)) else {
        return false;
    };
    if file.is_empty() || file.contains('/') {
        return false;
    }
    // `<uuid>.<ext>` with an allowed extension and a real UUID stem.
    let Some((stem, ext)) = file.rsplit_once('.') else {
        return false;
    };
    if !matches!(ext, "png" | "jpg" | "jpeg" | "gif" | "webp") {
        return false;
    }
    Uuid::parse_str(stem).is_ok()
}

/// Resolve a store-relative image ref to its real absolute path, refusing
/// symlink escapes.
///
/// [`is_store_relative_path`] only checks the *string* shape — it cannot tell
/// whether `.clai/images/<uuid>.png` is a real file or a symlink someone
/// pre-planted to point at `~/.ssh/id_rsa`. The send path reads these bytes and
/// base64-encodes them to the model, so a symlinked store entry would exfiltrate
/// an arbitrary local file through the provider. This canonicalizes both the
/// store directory and the target (following every symlink) and returns the path
/// only when the resolved file still lives inside the store. Returns `None` for a
/// non-store ref, a missing file, or a path that escapes the store.
pub fn resolve_store_path(root: &Path, rel: &str) -> Option<PathBuf> {
    if !is_store_relative_path(rel) {
        return None;
    }
    let store_dir = root.join(IMAGE_STORE_SUBDIR).canonicalize().ok()?;
    let full = root.join(rel).canonicalize().ok()?;
    full.starts_with(&store_dir).then_some(full)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_store_relative_path_accepts_only_store_owned_refs() {
        let uuid = Uuid::new_v4().to_string();
        // Shapes store_image actually produces.
        assert!(is_store_relative_path(&format!(
            ".clai/images/{}.png",
            uuid
        )));
        assert!(is_store_relative_path(&format!(
            ".clai/images/{}.jpg",
            uuid
        )));
        assert!(is_store_relative_path(&format!(
            ".clai/images/{}.gif",
            uuid
        )));
        assert!(is_store_relative_path(&format!(
            ".clai/images/{}.webp",
            uuid
        )));

        // The exfiltration vectors the gate must reject.
        assert!(!is_store_relative_path("/etc/passwd"));
        assert!(!is_store_relative_path("/home/user/.ssh/id_rsa"));
        assert!(!is_store_relative_path("../../../etc/passwd"));
        assert!(!is_store_relative_path(".clai/images/../../secret.png"));

        // Wrong location / nesting / shape.
        assert!(!is_store_relative_path(&format!("images/{}.png", uuid)));
        assert!(!is_store_relative_path(&format!(
            ".clai/other/{}.png",
            uuid
        )));
        assert!(!is_store_relative_path(&format!(
            ".clai/images/sub/{}.png",
            uuid
        )));
        assert!(!is_store_relative_path(".clai/images/passwd.png"));
        assert!(!is_store_relative_path(&format!(
            ".clai/images/{}.svg",
            uuid
        )));
        assert!(!is_store_relative_path(&format!(
            ".clai/images/{}.exe",
            uuid
        )));
        assert!(!is_store_relative_path(""));
    }

    #[test]
    fn normalize_maps_supported_types_and_rejects_others() {
        assert_eq!(
            normalize_image_type("image/png"),
            Some(("png", "image/png"))
        );
        assert_eq!(
            normalize_image_type("IMAGE/JPG"),
            Some(("jpg", "image/jpeg"))
        );
        assert_eq!(
            normalize_image_type(" image/webp "),
            Some(("webp", "image/webp"))
        );
        assert_eq!(normalize_image_type("image/svg+xml"), None);
        assert_eq!(normalize_image_type("text/plain"), None);
    }

    #[test]
    fn store_writes_file_under_subdir_and_returns_reference() {
        let dir = tempfile::tempdir().unwrap();
        let data = b"\x89PNG\r\n\x1a\nfake-png-bytes";
        let stored = store_image(dir.path(), data, "image/png", Some("shot.png".into())).unwrap();

        assert!(stored.path.starts_with(IMAGE_STORE_SUBDIR));
        assert!(stored.path.ends_with(".png"));
        assert_eq!(stored.media_type, "image/png");
        assert_eq!(stored.filename.as_deref(), Some("shot.png"));

        let written = std::fs::read(dir.path().join(&stored.path)).unwrap();
        assert_eq!(written, data);
    }

    #[test]
    fn store_rejects_empty_oversized_and_unsupported() {
        let dir = tempfile::tempdir().unwrap();
        assert!(store_image(dir.path(), b"", "image/png", None).is_err());
        assert!(store_image(dir.path(), b"x", "image/svg+xml", None).is_err());

        let huge = vec![0u8; MAX_IMAGE_BYTES + 1];
        assert!(store_image(dir.path(), &huge, "image/png", None).is_err());
    }

    #[test]
    fn resolve_store_path_returns_real_file_for_store_owned_ref() {
        let dir = tempfile::tempdir().unwrap();
        let stored = store_image(dir.path(), b"\x89PNG\r\n", "image/png", None).unwrap();

        let resolved = resolve_store_path(dir.path(), &stored.path).expect("store file resolves");
        assert!(resolved.ends_with(format!("{}.png", stored.id)));
        assert_eq!(std::fs::read(&resolved).unwrap(), b"\x89PNG\r\n");
    }

    #[test]
    fn resolve_store_path_rejects_non_store_and_missing() {
        let dir = tempfile::tempdir().unwrap();
        // String-shape failures are rejected before touching the FS.
        assert!(resolve_store_path(dir.path(), "/etc/passwd").is_none());
        assert!(resolve_store_path(dir.path(), "../secret.png").is_none());
        // Well-shaped but non-existent ref: canonicalize fails -> None.
        let uuid = Uuid::new_v4();
        assert!(resolve_store_path(dir.path(), &format!(".clai/images/{uuid}.png")).is_none());
    }

    // The whole point of resolve_store_path: a path that passes the lexical
    // `is_store_relative_path` gate but is a symlink escaping the store must be
    // refused, so a crafted message can't exfiltrate an arbitrary local file.
    #[cfg(unix)]
    #[test]
    fn resolve_store_path_rejects_symlink_escaping_the_store() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        // A "secret" file outside the store the attacker wants to read.
        let secret = root.join("secret.txt");
        std::fs::write(&secret, b"top secret").unwrap();

        // Pre-plant a store-shaped symlink pointing at the secret.
        let store_dir = root.join(IMAGE_STORE_SUBDIR);
        std::fs::create_dir_all(&store_dir).unwrap();
        let uuid = Uuid::new_v4();
        let rel = format!(".clai/images/{uuid}.png");
        std::os::unix::fs::symlink(&secret, root.join(&rel)).unwrap();

        // The lexical gate is fooled...
        assert!(is_store_relative_path(&rel));
        // ...but resolve_store_path follows the link and refuses the escape.
        assert!(resolve_store_path(root, &rel).is_none());
    }
}
