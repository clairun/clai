use std::path::{Path, PathBuf};

pub fn clai_home() -> PathBuf {
    std::env::var_os("CLAI_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".clai")))
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
        return dirs::home_dir().unwrap_or_else(|| PathBuf::from("~"));
    }
    if let Some(rest) = value.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    path.to_path_buf()
}
