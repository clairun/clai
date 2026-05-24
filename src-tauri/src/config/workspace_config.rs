use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::{bundled, AppConfig, SkillSourceKind};
use crate::config::{ExecutionCapabilityConfig, FilesystemPathAccess, FilesystemPathGrant};

const WORKSPACE_CONFIG_VERSION: u32 = 1;

#[derive(Debug)]
pub enum WorkspaceConfigError {
    Io {
        operation: &'static str,
        path: PathBuf,
        source: std::io::Error,
    },
    Parse {
        path: PathBuf,
        source: serde_json::Error,
    },
    Serialize {
        source: serde_json::Error,
    },
}

impl std::fmt::Display for WorkspaceConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WorkspaceConfigError::Io {
                operation,
                path,
                source,
            } => write!(f, "Failed to {} {}: {}", operation, path.display(), source),
            WorkspaceConfigError::Parse { path, source } => {
                write!(f, "Failed to parse {}: {}", path.display(), source)
            }
            WorkspaceConfigError::Serialize { source } => {
                write!(f, "Failed to serialize workspace config: {}", source)
            }
        }
    }
}

impl std::error::Error for WorkspaceConfigError {}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceSchedule {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub paused: bool,
    #[serde(default)]
    pub interval_minutes: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceConfig {
    #[serde(default = "default_workspace_config_version")]
    pub version: u32,
    pub id: String,
    pub title: String,
    pub created_at: i64,
    pub updated_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preferred_provider_connection_id: Option<String>,
    pub default_agent_id: String,
    #[serde(default)]
    pub schedule: WorkspaceSchedule,
    #[serde(default)]
    pub agents: Vec<WorkspaceAgent>,
}

fn default_workspace_config_version() -> u32 {
    WORKSPACE_CONFIG_VERSION
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceAgent {
    pub id: String,
    pub name: String,
    pub description: String,
    pub enabled: bool,
    #[serde(default)]
    pub selected_skills: Vec<SkillRef>,
    #[serde(default)]
    pub selected_mcp_servers: Vec<McpRef>,
    #[serde(default)]
    pub provider_connection_ids: Vec<String>,
    #[serde(default)]
    pub execution: ExecutionCapabilityConfig,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "source", rename_all = "camelCase")]
pub enum SkillRef {
    Bundled { slug: String },
    Personal { slug: String },
    Remote { url: String, slug: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct McpRef {
    pub name: String,
}

impl WorkspaceConfig {
    pub fn new(id: String, title: String, now: i64, manager_id: String) -> Self {
        Self {
            version: WORKSPACE_CONFIG_VERSION,
            id,
            title,
            created_at: now,
            updated_at: now,
            preferred_provider_connection_id: None,
            default_agent_id: manager_id.clone(),
            schedule: WorkspaceSchedule::default(),
            agents: vec![WorkspaceAgent::new_manager(manager_id, now)],
        }
    }
}

/// Build the default sandbox config for a new agent. Every fresh agent —
/// manager, sub-agent, or template-instantiated — ships with the host
/// `$HOME` granted read-only so it can read user dotfiles (`.gitconfig`,
/// `.bashrc`, ...) the way the user's shell would. The user can ×-remove
/// it in agent settings to harden any specific agent.
pub fn default_agent_execution() -> ExecutionCapabilityConfig {
    let mut execution = ExecutionCapabilityConfig::default();
    if let Some(home) = dirs::home_dir() {
        let path = home.display().to_string();
        execution.filesystem.extra_paths.push(FilesystemPathGrant {
            path,
            access: FilesystemPathAccess::ReadOnly,
            origin: None,
        });
    }
    execution
}

impl WorkspaceAgent {
    pub fn new_manager(id: String, now: i64) -> Self {
        Self {
            id,
            name: "Manager".to_string(),
            description: String::new(),
            enabled: true,
            selected_skills: Vec::new(),
            selected_mcp_servers: Vec::new(),
            provider_connection_ids: Vec::new(),
            execution: default_agent_execution(),
            created_at: now,
            updated_at: now,
        }
    }
}

pub fn config_path(root: &Path) -> PathBuf {
    root.join(".clai").join("config.json")
}

pub fn data_path(root: &Path) -> PathBuf {
    root.join(".clai").join("data.sqlite")
}

pub fn load(root: &Path) -> Result<WorkspaceConfig, WorkspaceConfigError> {
    let path = config_path(root);
    let contents = fs::read_to_string(&path).map_err(|source| WorkspaceConfigError::Io {
        operation: "read",
        path: path.clone(),
        source,
    })?;
    serde_json::from_str(&contents).map_err(|source| WorkspaceConfigError::Parse { path, source })
}

pub fn save(root: &Path, config: &WorkspaceConfig) -> Result<(), WorkspaceConfigError> {
    let path = config_path(root);
    let parent = path.parent().unwrap_or(root);
    fs::create_dir_all(parent).map_err(|source| WorkspaceConfigError::Io {
        operation: "create directory",
        path: parent.to_path_buf(),
        source,
    })?;

    let json = serde_json::to_string_pretty(config)
        .map_err(|source| WorkspaceConfigError::Serialize { source })?;
    let temp_path = path.with_extension("json.tmp");
    let mut file = fs::File::create(&temp_path).map_err(|source| WorkspaceConfigError::Io {
        operation: "create",
        path: temp_path.clone(),
        source,
    })?;
    file.write_all(json.as_bytes())
        .map_err(|source| WorkspaceConfigError::Io {
            operation: "write",
            path: temp_path.clone(),
            source,
        })?;
    file.sync_all().map_err(|source| WorkspaceConfigError::Io {
        operation: "sync",
        path: temp_path.clone(),
        source,
    })?;
    fs::rename(&temp_path, &path).map_err(|source| WorkspaceConfigError::Io {
        operation: "rename",
        path,
        source,
    })?;
    Ok(())
}

pub fn skill_ids_to_refs(config: &AppConfig, ids: &[String]) -> Vec<SkillRef> {
    ids.iter()
        .map(|id| {
            let Some((source_id, slug)) = id.split_once(':') else {
                return SkillRef::Personal { slug: id.clone() };
            };
            let Some(source) = config
                .skill_sources
                .iter()
                .find(|source| source.id == source_id)
            else {
                return SkillRef::Personal { slug: id.clone() };
            };
            if bundled::is_bundled_source(source) {
                SkillRef::Bundled {
                    slug: slug.to_string(),
                }
            } else if bundled::is_personal_source(source) {
                SkillRef::Personal {
                    slug: slug.to_string(),
                }
            } else if let SkillSourceKind::Git { uri, .. } = &source.source {
                SkillRef::Remote {
                    url: uri.clone(),
                    slug: slug.to_string(),
                }
            } else {
                SkillRef::Personal { slug: id.clone() }
            }
        })
        .collect()
}

pub fn refs_to_skill_ids(config: &AppConfig, refs: &[SkillRef]) -> Vec<String> {
    refs.iter()
        .filter_map(|skill_ref| match skill_ref {
            SkillRef::Bundled { slug } => config
                .skill_sources
                .iter()
                .find(|source| bundled::is_bundled_source(source))
                .map(|source| format!("{}:{}", source.id, slug)),
            SkillRef::Personal { slug } => config
                .skill_sources
                .iter()
                .find(|source| bundled::is_personal_source(source))
                .map(|source| format!("{}:{}", source.id, slug))
                .or_else(|| Some(slug.clone())),
            SkillRef::Remote { url, slug } => config
                .skill_sources
                .iter()
                .find(|source| match &source.source {
                    SkillSourceKind::Git { uri, .. } => uri == url,
                    SkillSourceKind::Local { .. } => false,
                })
                .map(|source| format!("{}:{}", source.id, slug)),
        })
        .collect()
}

pub fn mcp_ids_to_refs(config: &AppConfig, ids: &[String]) -> Vec<McpRef> {
    ids.iter()
        .map(|id| {
            let name = config
                .mcp_servers
                .iter()
                .find(|server| server.id == *id)
                .map(|server| server.name.clone())
                .unwrap_or_else(|| id.clone());
            McpRef { name }
        })
        .collect()
}

pub fn refs_to_mcp_ids(config: &AppConfig, refs: &[McpRef]) -> Vec<String> {
    refs.iter()
        .map(|mcp_ref| {
            config
                .mcp_servers
                .iter()
                .find(|server| server.name == mcp_ref.name)
                .map(|server| server.id.clone())
                .unwrap_or_else(|| mcp_ref.name.clone())
        })
        .collect()
}
