use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// The user's real home directory. On a native install this is
/// `dirs::home_dir()`. Inside Flatpak `$HOME` is the sandboxed per-app
/// home, so we resolve the *real* host home (via `flatpak-spawn`) and
/// cache it — this is what makes `~/.clai` (and any `~/...` path) point at
/// the same location the native `.deb` install uses, rather than an
/// isolated copy under `~/.var/app/run.clai.CLAI/`. The host
/// home is reachable inside the sandbox because the Flatpak is granted
/// `--filesystem=home`. Falls back to `dirs::home_dir()` if resolution
/// fails, so a missing host-spawn permission degrades to isolated-but-
/// working rather than broken.
fn real_home() -> Option<PathBuf> {
    if crate::providers::is_flatpak() {
        static REAL_HOME: OnceLock<Option<PathBuf>> = OnceLock::new();
        return REAL_HOME
            .get_or_init(|| {
                crate::providers::get_home_dir()
                    .map(PathBuf::from)
                    .or_else(dirs::home_dir)
            })
            .clone();
    }
    dirs::home_dir()
}

pub fn clai_home() -> PathBuf {
    std::env::var_os("CLAI_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| real_home().map(|home| home.join(".clai")))
        .unwrap_or_else(|| PathBuf::from(".clai"))
}

pub fn clai_skills_root() -> PathBuf {
    clai_home().join("skills")
}

pub fn clai_cache_root() -> PathBuf {
    clai_home().join("cache")
}

pub fn clai_cache_bundled_root() -> PathBuf {
    clai_cache_root().join("bundled")
}

pub fn clai_cache_skill_sources_root() -> PathBuf {
    clai_cache_root().join("skill-sources")
}

pub fn expand_tilde(path: &Path) -> PathBuf {
    let value = path.to_string_lossy();
    if value == "~" {
        return real_home().unwrap_or_else(|| PathBuf::from("~"));
    }
    if let Some(rest) = value.strip_prefix("~/") {
        if let Some(home) = real_home() {
            return home.join(rest);
        }
    }
    path.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expand_tilde_passes_absolute_and_relative_through() {
        // Non-tilde paths are returned unchanged regardless of home.
        assert_eq!(expand_tilde(Path::new("/abs/x")), PathBuf::from("/abs/x"));
        assert_eq!(expand_tilde(Path::new("rel/x")), PathBuf::from("rel/x"));
    }

    #[test]
    fn expand_tilde_joins_under_home_when_resolvable() {
        // Tests run on the host (not Flatpak), so real_home() == home_dir().
        if let Some(home) = dirs::home_dir() {
            assert_eq!(expand_tilde(Path::new("~/.clai")), home.join(".clai"));
            assert_eq!(expand_tilde(Path::new("~")), home);
        }
    }
}
