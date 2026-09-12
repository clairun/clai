//! Temporary-file helpers for agent runtime setup.

/// Gets the CLAI temp directory for ephemeral files.
fn get_clai_tmp_dir() -> Option<std::path::PathBuf> {
    dirs::config_dir().map(|d| d.join("clai").join("tmp"))
}

/// Clears the CLAI temp directory.
///
/// Safe to call on startup even if the directory does not exist.
pub fn clear_tmp_dir() {
    if let Some(tmp_dir) = get_clai_tmp_dir() {
        if tmp_dir.exists() {
            match std::fs::remove_dir_all(&tmp_dir) {
                Ok(_) => tracing::debug!(path = %tmp_dir.display(), "Cleared clai tmp directory"),
                Err(e) => {
                    tracing::warn!(error = %e, path = %tmp_dir.display(), "Failed to clear clai tmp directory");
                }
            }
        }
    }
}
