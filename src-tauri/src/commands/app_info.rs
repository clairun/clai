//! App metadata exposed to the frontend (version / build info).

/// Version string to display in the UI.
///
/// In a release build this is the crate version (e.g. `26.6.7`). In a dev
/// build past the last release tag it's the `git describe` string baked in by
/// `build.rs`, with the leading `v` stripped (e.g. `26.6.7-38-g6148106`), so
/// the About page reflects exactly how far ahead of the release the build is.
/// Falls back to the crate version when no git info was baked in.
#[tauri::command]
pub fn app_version_detail() -> String {
    option_env!("CLAI_GIT_DESCRIBE")
        .map(|describe| describe.trim_start_matches('v').to_string())
        .filter(|describe| !describe.is_empty())
        .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_version_detail_is_version_like_without_leading_v() {
        let version = app_version_detail();
        assert!(!version.is_empty());
        assert!(
            !version.starts_with('v'),
            "leading `v` should be stripped: {version}"
        );
        assert!(
            version.chars().next().is_some_and(|c| c.is_ascii_digit()),
            "expected a version-like string, got {version}"
        );
    }
}
