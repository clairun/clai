//! Bundled building blocks shipped with CLAI.
//!
//! Bundled skill slugs and bundled agent-template ids are stable public
//! references. Do not rename them across releases without adding an alias or
//! explicit migration for existing configs.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use include_dir::{include_dir, Dir, DirEntry};
use serde::{Deserialize, Serialize};

use super::{ClaiConfig, ExecutionCapabilityConfig, SkillSourceConfig, SkillSourceKind};

static BUNDLED_SKILLS: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/embedded/skills");
static BUNDLED_AGENT_TEMPLATES: Dir<'_> =
    include_dir!("$CARGO_MANIFEST_DIR/embedded/agent-templates");

#[cfg(test)]
const BUNDLED_SKILL_SLUGS: &[&str] = &[
    "code-review-checklist",
    "delegation",
    "iterative-review",
    "self-reflection",
    "sow-workflow",
    "unbiased-second-opinion",
];

pub fn bundled_root() -> PathBuf {
    crate::paths::clai_cache_bundled_root()
}

pub fn bundled_skills_root() -> PathBuf {
    bundled_root().join("skills")
}

pub fn bundled_agent_templates_root() -> PathBuf {
    bundled_root().join("agent-templates")
}

pub fn personal_skills_root() -> PathBuf {
    crate::paths::clai_skills_root()
}

pub fn is_bundled_source(source: &SkillSourceConfig) -> bool {
    source_local_path_matches(source, &bundled_skills_root())
}

pub fn is_personal_source(source: &SkillSourceConfig) -> bool {
    source_local_path_matches(source, &personal_skills_root())
}

pub fn materialize_bundled_skills() -> io::Result<()> {
    materialize_embedded_dir(&BUNDLED_SKILLS, &bundled_skills_root())?;
    materialize_embedded_dir(&BUNDLED_AGENT_TEMPLATES, &bundled_agent_templates_root())
}

pub fn ensure_bundled_skill_source(config: &mut ClaiConfig) -> bool {
    ensure_bundled_skill_source_at(config, &bundled_skills_root())
}

pub fn ensure_personal_skill_source_lazy(config: &mut ClaiConfig) -> Result<bool, String> {
    ensure_personal_skill_source_at(config, &personal_skills_root())
        .map_err(|error| format!("Failed to prepare personal skill source: {}", error))
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

fn materialize_embedded_dir(source: &Dir<'_>, target: &Path) -> io::Result<()> {
    if target.exists() {
        fs::remove_dir_all(target)?;
    }
    fs::create_dir_all(target)?;
    write_dir_entries(source, target)
}

fn write_dir_entries(source: &Dir<'_>, target: &Path) -> io::Result<()> {
    for entry in source.entries() {
        match entry {
            DirEntry::Dir(dir) => {
                let dir_target = target.join(dir.path());
                fs::create_dir_all(&dir_target)?;
                write_dir_entries(dir, target)?;
            }
            DirEntry::File(file) => {
                let path = target.join(file.path());
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::write(path, file.contents())?;
            }
        }
    }
    Ok(())
}

fn ensure_bundled_skill_source_at(config: &mut ClaiConfig, root: &Path) -> bool {
    if config
        .skill_sources
        .iter()
        .any(|source| source_local_path_matches(source, root))
    {
        return false;
    }

    config.skill_sources.push(SkillSourceConfig::new_local(
        "Bundled Skills".to_string(),
        root.display().to_string(),
    ));
    true
}

fn ensure_personal_skill_source_at(config: &mut ClaiConfig, root: &Path) -> io::Result<bool> {
    fs::create_dir_all(root)?;
    let Some(source) = config
        .skill_sources
        .iter_mut()
        .find(|source| source_local_path_matches(source, root))
    else {
        config.skill_sources.push(SkillSourceConfig::new_local(
            "Personal Skills".to_string(),
            root.display().to_string(),
        ));
        return Ok(true);
    };

    if source.enabled {
        return Ok(false);
    }

    source.enabled = true;
    source.updated_at = chrono::Utc::now().to_rfc3339();
    Ok(true)
}

fn source_local_path_matches(source: &SkillSourceConfig, expected: &Path) -> bool {
    matches!(
        &source.source,
        SkillSourceKind::Local { path } if Path::new(path) == expected
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn materialize_bundled_skills_writes_and_overwrites_files() {
        let temp_dir = TempDir::new().unwrap();
        let target = temp_dir.path().join("skills");
        let edited = target.join("iterative-review").join("SKILL.md");

        materialize_embedded_dir(&BUNDLED_SKILLS, &target).unwrap();
        assert!(target.join("iterative-review").join("SKILL.md").exists());

        fs::write(&edited, "user edit").unwrap();
        materialize_embedded_dir(&BUNDLED_SKILLS, &target).unwrap();

        let content = fs::read_to_string(edited).unwrap();
        assert!(content.contains("name: \"Iterative Review\""));
        assert!(!content.contains("user edit"));
    }

    #[test]
    fn ensure_bundled_skill_source_is_idempotent() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path().join("bundled").join("skills");
        let mut config = ClaiConfig::default();

        assert!(ensure_bundled_skill_source_at(&mut config, &root));
        assert!(!ensure_bundled_skill_source_at(&mut config, &root));

        assert_eq!(config.skill_sources.len(), 1);
        assert!(source_local_path_matches(&config.skill_sources[0], &root));
    }

    #[test]
    fn ensure_personal_skill_source_is_lazy_and_idempotent() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path().join("personal");
        let mut config = ClaiConfig::default();

        assert!(ensure_personal_skill_source_at(&mut config, &root).unwrap());
        assert!(root.exists());
        assert!(!ensure_personal_skill_source_at(&mut config, &root).unwrap());

        assert_eq!(config.skill_sources.len(), 1);
        assert!(source_local_path_matches(&config.skill_sources[0], &root));
    }

    #[test]
    fn resolve_bundled_skill_id_uses_config_source_id() {
        let mut config = ClaiConfig::default();
        ensure_bundled_skill_source(&mut config);

        let source_id = config.skill_sources[0].id.clone();
        assert_eq!(
            resolve_bundled_skill_id("iterative-review", &config),
            Some(format!("{}:iterative-review", source_id))
        );
    }

    #[test]
    fn bundled_skill_slugs_are_stable() {
        let mut actual: Vec<String> = BUNDLED_SKILLS
            .entries()
            .iter()
            .filter_map(|entry| match entry {
                DirEntry::Dir(dir) => dir
                    .path()
                    .file_name()
                    .and_then(|name| name.to_str())
                    .map(str::to_string),
                DirEntry::File(_) => None,
            })
            .collect();
        actual.sort();

        let mut expected: Vec<String> = BUNDLED_SKILL_SLUGS
            .iter()
            .map(|slug| (*slug).to_string())
            .collect();
        expected.sort();

        assert_eq!(actual, expected);
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
