//! App-managed building blocks enabled by CLAI.
//!
//! The default skills are loaded from the CLAI skills repository, not embedded
//! in the app binary. The `bundled:<slug>` workspace ref name is retained as a
//! compatibility contract for existing workspace configs and agent templates.
//!
//! Bundled agent-template ids and default skill slugs are stable public
//! references. Do not rename them across releases without adding an alias or
//! explicit migration for existing configs.

use std::path::{Path, PathBuf};

use include_dir::{include_dir, Dir, DirEntry};
use serde::{Deserialize, Serialize};

use super::{ClaiConfig, ExecutionCapabilityConfig, SkillSourceConfig, SkillSourceKind};

static BUNDLED_AGENT_TEMPLATES: Dir<'_> =
    include_dir!("$CARGO_MANIFEST_DIR/embedded/agent-templates");

pub const DEFAULT_SKILL_SOURCE_NAME: &str = "CLAI Skills";
pub const DEFAULT_SKILL_SOURCE_URI: &str = "https://github.com/clairun/clai-skills.git";
const DEFAULT_SKILL_SOURCE_CACHE_DIR: &str = "clai-skills";

pub fn bundled_root() -> PathBuf {
    crate::paths::clai_cache_bundled_root()
}

/// Legacy embedded-skill cache path. Kept only to identify and migrate
/// pre-repository configs that still reference this local source.
pub fn bundled_skills_root() -> PathBuf {
    bundled_root().join("skills")
}

pub fn default_skill_source_cache_root() -> PathBuf {
    crate::paths::clai_cache_skill_sources_root().join(DEFAULT_SKILL_SOURCE_CACHE_DIR)
}

pub fn personal_skills_root() -> PathBuf {
    crate::paths::clai_skills_root()
}

pub fn is_bundled_source(source: &SkillSourceConfig) -> bool {
    is_bundled_source_at(source, &bundled_skills_root())
}

pub fn is_personal_source(source: &SkillSourceConfig) -> bool {
    source_local_path_matches(source, &personal_skills_root())
}

pub fn ensure_bundled_skill_source(config: &mut ClaiConfig) -> bool {
    ensure_bundled_skill_source_at(
        config,
        &default_skill_source_cache_root(),
        &bundled_skills_root(),
    )
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BundledAgentTemplate {
    pub id: String,
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub default_skill_slugs: Vec<String>,
    #[serde(default)]
    pub default_execution: ExecutionCapabilityConfig,
}

pub fn load_bundled_agent_templates() -> Result<Vec<BundledAgentTemplate>, String> {
    let mut templates = Vec::new();
    for entry in BUNDLED_AGENT_TEMPLATES.entries() {
        let DirEntry::File(file) = entry else {
            continue;
        };
        if file.path().extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let contents = file
            .contents_utf8()
            .ok_or_else(|| format!("Bundled template is not UTF-8: {}", file.path().display()))?;
        let template: BundledAgentTemplate = serde_json::from_str(contents).map_err(|error| {
            format!(
                "Failed to parse bundled template {}: {}",
                file.path().display(),
                error
            )
        })?;
        templates.push(template);
    }
    templates.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(templates)
}

pub fn resolve_bundled_skill_id(slug: &str, config: &ClaiConfig) -> Option<String> {
    config
        .skill_sources
        .iter()
        .find(|source| is_bundled_source(source))
        .map(|source| format!("{}:{}", source.id, slug))
}

fn ensure_bundled_skill_source_at(
    config: &mut ClaiConfig,
    cache_root: &Path,
    legacy_root: &Path,
) -> bool {
    if let Some(index) = config
        .skill_sources
        .iter()
        .position(|source| is_bundled_source_at(source, legacy_root))
    {
        let mut changed = {
            let source = &mut config.skill_sources[index];
            let mut changed = false;
            if source.name != DEFAULT_SKILL_SOURCE_NAME {
                source.name = DEFAULT_SKILL_SOURCE_NAME.to_string();
                changed = true;
            }

            match &mut source.source {
                SkillSourceKind::Git {
                    uri,
                    reference,
                    local_path,
                } => {
                    if uri != DEFAULT_SKILL_SOURCE_URI {
                        *uri = DEFAULT_SKILL_SOURCE_URI.to_string();
                        changed = true;
                    }
                    if reference.is_some() {
                        *reference = None;
                        changed = true;
                    }
                    if local_path.is_none() {
                        *local_path = Some(cache_root.display().to_string());
                        changed = true;
                    }
                }
                SkillSourceKind::Local { .. } => {
                    source.source = SkillSourceKind::Git {
                        uri: DEFAULT_SKILL_SOURCE_URI.to_string(),
                        reference: None,
                        local_path: Some(cache_root.display().to_string()),
                    };
                    changed = true;
                }
            }
            changed
        };

        let before_len = config.skill_sources.len();
        let mut seen_default_source = false;
        config.skill_sources.retain(|source| {
            if !is_bundled_source_at(source, legacy_root) {
                return true;
            }
            if seen_default_source {
                return false;
            }
            seen_default_source = true;
            true
        });
        if config.skill_sources.len() != before_len {
            changed = true;
        }

        if changed {
            let source = &mut config.skill_sources[index];
            source.updated_at = chrono::Utc::now().to_rfc3339();
        }
        return changed;
    }

    config.skill_sources.push(SkillSourceConfig::new_git(
        DEFAULT_SKILL_SOURCE_NAME.to_string(),
        DEFAULT_SKILL_SOURCE_URI.to_string(),
        None,
        Some(cache_root.display().to_string()),
    ));
    true
}

fn source_local_path_matches(source: &SkillSourceConfig, expected: &Path) -> bool {
    matches!(
        &source.source,
        SkillSourceKind::Local { path } if Path::new(path) == expected
    )
}

fn is_bundled_source_at(source: &SkillSourceConfig, legacy_root: &Path) -> bool {
    match &source.source {
        SkillSourceKind::Git { uri, .. } => is_default_skill_source_uri(uri),
        SkillSourceKind::Local { path } => Path::new(path) == legacy_root,
    }
}

fn is_default_skill_source_uri(uri: &str) -> bool {
    let value = uri.trim().trim_end_matches('/');
    let value = value.strip_suffix(".git").unwrap_or(value);
    // The juacker form is the pre-org-move URL: configs written before the
    // 2026-07-04 transfer still carry it (GitHub redirects the git remote),
    // and recognizing it prevents ensure-default from adding a duplicate.
    value == "https://github.com/clairun/clai-skills"
        || value == "https://github.com/juacker/clai-skills"
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn ensure_bundled_skill_source_registers_default_git_source() {
        let temp_dir = TempDir::new().unwrap();
        let cache_root = temp_dir.path().join("skill-sources").join("clai-skills");
        let legacy_root = temp_dir.path().join("bundled").join("skills");
        let mut config = ClaiConfig::default();

        assert!(ensure_bundled_skill_source_at(
            &mut config,
            &cache_root,
            &legacy_root
        ));
        assert!(!ensure_bundled_skill_source_at(
            &mut config,
            &cache_root,
            &legacy_root
        ));

        assert_eq!(config.skill_sources.len(), 1);
        assert_eq!(config.skill_sources[0].name, DEFAULT_SKILL_SOURCE_NAME);
        assert!(matches!(
            &config.skill_sources[0].source,
            SkillSourceKind::Git {
                uri,
                reference: None,
                local_path: Some(local_path),
            } if uri == DEFAULT_SKILL_SOURCE_URI
                && local_path == &cache_root.display().to_string()
        ));
    }

    #[test]
    fn ensure_bundled_skill_source_migrates_legacy_local_source_in_place() {
        let temp_dir = TempDir::new().unwrap();
        let cache_root = temp_dir.path().join("skill-sources").join("clai-skills");
        let legacy_root = temp_dir.path().join("bundled").join("skills");
        let mut config = ClaiConfig::default();
        config.skill_sources.push(SkillSourceConfig::new_local(
            "Bundled Skills".to_string(),
            legacy_root.display().to_string(),
        ));
        let source_id = config.skill_sources[0].id.clone();

        assert!(ensure_bundled_skill_source_at(
            &mut config,
            &cache_root,
            &legacy_root
        ));

        assert_eq!(config.skill_sources.len(), 1);
        assert_eq!(config.skill_sources[0].id, source_id);
        assert_eq!(config.skill_sources[0].name, DEFAULT_SKILL_SOURCE_NAME);
        assert!(matches!(
            &config.skill_sources[0].source,
            SkillSourceKind::Git {
                uri,
                reference: None,
                local_path: Some(local_path),
            } if uri == DEFAULT_SKILL_SOURCE_URI
                && local_path == &cache_root.display().to_string()
        ));
    }

    #[test]
    fn ensure_bundled_skill_source_removes_duplicate_default_sources() {
        let temp_dir = TempDir::new().unwrap();
        let cache_root = temp_dir.path().join("skill-sources").join("clai-skills");
        let legacy_root = temp_dir.path().join("bundled").join("skills");
        let mut config = ClaiConfig::default();
        config.skill_sources.push(SkillSourceConfig::new_git(
            DEFAULT_SKILL_SOURCE_NAME.to_string(),
            DEFAULT_SKILL_SOURCE_URI.to_string(),
            None,
            Some(cache_root.display().to_string()),
        ));
        let source_id = config.skill_sources[0].id.clone();
        config.skill_sources.push(SkillSourceConfig::new_local(
            "Bundled Skills".to_string(),
            legacy_root.display().to_string(),
        ));

        assert!(ensure_bundled_skill_source_at(
            &mut config,
            &cache_root,
            &legacy_root
        ));

        assert_eq!(config.skill_sources.len(), 1);
        assert_eq!(config.skill_sources[0].id, source_id);
    }

    #[test]
    fn resolve_bundled_skill_id_uses_config_source_id() {
        let mut config = ClaiConfig::default();
        config.skill_sources.push(SkillSourceConfig::new_git(
            DEFAULT_SKILL_SOURCE_NAME.to_string(),
            DEFAULT_SKILL_SOURCE_URI.to_string(),
            None,
            Some("/tmp/clai-skills".to_string()),
        ));

        let source_id = config.skill_sources[0].id.clone();
        assert_eq!(
            resolve_bundled_skill_id("iterative-review", &config),
            Some(format!("{}:iterative-review", source_id))
        );
    }

    #[test]
    fn load_bundled_agent_templates_returns_expected_templates() {
        let templates = load_bundled_agent_templates().unwrap();
        let ids: Vec<_> = templates
            .iter()
            .map(|template| template.id.as_str())
            .collect();

        assert_eq!(ids, vec!["code-reviewer", "sow-tracker"]);
        assert!(templates
            .iter()
            .all(|template| !template.default_skill_slugs.is_empty()));
    }
}
