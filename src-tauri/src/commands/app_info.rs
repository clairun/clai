//! App metadata exposed to the frontend (version / build info).

/// Version string to display in the UI.
///
/// In a release build this is the crate version (e.g. `26.6.7`). In a dev
/// build past the last release tag it's the `git describe` string baked in by
/// `build.rs`, with the leading `v` stripped (e.g. `26.6.7-38-g6148106`), so
/// the About page reflects exactly how far ahead of the release the build is.
/// Falls back to the crate version when no git info was baked in, or when no
/// release tag was reachable (e.g. a shallow CI clone, where `git describe`
/// emits a bare commit hash rather than a version-relative string).
#[tauri::command]
pub fn app_version_detail() -> String {
    resolve_version_detail(option_env!("CLAI_GIT_DESCRIBE"), env!("CARGO_PKG_VERSION"))
}

/// Number of agent runs in progress in this process, which quitting or
/// restarting the app interrupts.
#[tauri::command]
pub fn app_live_run_count() -> usize {
    crate::assistant::runtime::live_run_count()
}

/// Pick the display version from the baked `git describe` value, falling back
/// to the crate version. Split out from [`app_version_detail`] so the fallback
/// rules are testable without recompiling to change the baked env var.
fn resolve_version_detail(git_describe: Option<&str>, pkg_version: &str) -> String {
    git_describe
        .map(|describe| describe.trim_start_matches('v').to_string())
        // A version-relative describe starts with the tag's numeric version.
        // Without a reachable tag, `git describe --always` returns a bare
        // hash like `c605bfa`; that isn't a meaningful version, so fall back.
        .filter(|describe| describe.chars().next().is_some_and(|c| c.is_ascii_digit()))
        .unwrap_or_else(|| pkg_version.to_string())
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

    #[test]
    fn resolve_version_detail_strips_leading_v_from_describe() {
        assert_eq!(
            resolve_version_detail(Some("v26.6.7-38-g6148106"), "26.6.7"),
            "26.6.7-38-g6148106"
        );
    }

    #[test]
    fn resolve_version_detail_falls_back_when_no_describe_baked() {
        assert_eq!(resolve_version_detail(None, "26.6.7"), "26.6.7");
    }

    #[test]
    fn resolve_version_detail_falls_back_on_bare_hash() {
        // Shallow CI clone: no reachable tag, so `git describe --always`
        // emits a bare commit hash. That isn't a version, so we fall back.
        assert_eq!(resolve_version_detail(Some("c605bfa"), "26.6.7"), "26.6.7");
    }
}
