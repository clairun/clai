//! Configuration management for CLAI.
//!
//! This module handles loading and saving the application configuration
//! to a JSON file in the platform-specific config directory.

pub mod types;

pub use types::{
    AgentConfig, AiProvider, ClaiConfig, ExecutionCapabilityConfig, ExposedAgentTool,
    FilesystemPathAccess, FilesystemPathGrant, McpServerAuth, McpServerConfig,
    McpServerIntegrationType, McpServerTransport, ShellAccessMode, SkillSourceConfig,
    SkillSourceKind,
};

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Name of the config file.
const CONFIG_FILE_NAME: &str = "config.json";

/// Application identifier for config and data directories.
pub const APP_IDENTIFIER: &str = "clai";

/// Manages loading and saving the application configuration.
pub struct ConfigManager {
    /// Cached configuration (protected by mutex).
    config: Mutex<ClaiConfig>,

    /// Path to the config file.
    config_path: PathBuf,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillDefinition {
    pub id: String,
    pub name: String,
    pub description: String,
    pub source_id: String,
    pub source_name: String,
    pub source_path: String,
    pub content: String,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillSourceDiagnostic {
    pub source_id: String,
    pub source_name: String,
    pub ok: bool,
    pub message: Option<String>,
    pub skill_count: usize,
}

impl SkillDefinition {
    fn prompt_section(&self) -> String {
        format!(
            "### {}\nSource: {}\n\n{}",
            self.name, self.source_path, self.content
        )
    }
}

impl ConfigManager {
    /// Creates a new ConfigManager, loading existing config or creating default.
    ///
    /// If no automations exist in the config, creates the default one.
    pub fn new() -> Result<Self, ConfigError> {
        let config_path = Self::get_config_path()?;

        if let Some(parent) = config_path.parent() {
            fs::create_dir_all(parent).map_err(|e| ConfigError::Io {
                operation: "create config directory".to_string(),
                source: e,
            })?;
        }

        let mut config = if config_path.exists() {
            Self::load_from_file(&config_path)?
        } else {
            ClaiConfig::default()
        };

        let needs_save = if config.agents.is_empty() {
            config.agents.push(AgentConfig::default_agent());
            true
        } else {
            false
        };

        let manager = Self {
            config: Mutex::new(config),
            config_path,
        };

        if needs_save {
            manager.save()?;
        }

        Ok(manager)
    }

    /// Gets the platform-specific config file path.
    fn get_config_path() -> Result<PathBuf, ConfigError> {
        let config_dir = dirs::config_dir().ok_or(ConfigError::NoConfigDir)?;
        Ok(config_dir.join(APP_IDENTIFIER).join(CONFIG_FILE_NAME))
    }

    /// Loads config from a file.
    fn load_from_file(path: &PathBuf) -> Result<ClaiConfig, ConfigError> {
        let contents = fs::read_to_string(path).map_err(|e| ConfigError::Io {
            operation: "read config file".to_string(),
            source: e,
        })?;

        serde_json::from_str(&contents).map_err(|e| ConfigError::Parse { source: e })
    }

    /// Saves the current config to disk.
    pub fn save(&self) -> Result<(), ConfigError> {
        let config = self.config.lock().unwrap();
        self.save_to_file(&config)
    }

    /// Saves config to file with atomic write.
    fn save_to_file(&self, config: &ClaiConfig) -> Result<(), ConfigError> {
        let json = serde_json::to_string_pretty(config)
            .map_err(|e| ConfigError::Serialize { source: e })?;

        let temp_path = self.config_path.with_extension("json.tmp");
        let mut file = fs::File::create(&temp_path).map_err(|e| ConfigError::Io {
            operation: "create temp config file".to_string(),
            source: e,
        })?;

        file.write_all(json.as_bytes())
            .map_err(|e| ConfigError::Io {
                operation: "write temp config file".to_string(),
                source: e,
            })?;

        file.sync_all().map_err(|e| ConfigError::Io {
            operation: "sync temp config file".to_string(),
            source: e,
        })?;

        fs::rename(&temp_path, &self.config_path).map_err(|e| ConfigError::Io {
            operation: "rename temp config file".to_string(),
            source: e,
        })?;

        Ok(())
    }

    /// Gets a clone of the current config.
    pub fn get(&self) -> ClaiConfig {
        self.config.lock().unwrap().clone()
    }

    /// Updates the config and saves to disk.
    pub fn update<F>(&self, f: F) -> Result<(), ConfigError>
    where
        F: FnOnce(&mut ClaiConfig),
    {
        let mut config = self.config.lock().unwrap();
        f(&mut config);
        self.save_to_file(&config)
    }

    /// Gets the current AI provider.
    pub fn get_ai_provider(&self) -> Option<AiProvider> {
        self.config.lock().unwrap().ai_provider.clone()
    }

    /// Sets the AI provider and saves config.
    pub fn set_ai_provider(&self, provider: AiProvider) -> Result<(), ConfigError> {
        self.update(|config| {
            config.ai_provider = Some(provider);
        })
    }

    /// Clears the AI provider and saves config.
    pub fn clear_ai_provider(&self) -> Result<(), ConfigError> {
        self.update(|config| {
            config.ai_provider = None;
        })
    }

    /// Checks if an AI provider is configured.
    pub fn has_ai_provider(&self) -> bool {
        self.config.lock().unwrap().ai_provider.is_some()
    }

    /// Gets all configured MCP servers.
    pub fn get_mcp_servers(&self) -> Vec<McpServerConfig> {
        self.config.lock().unwrap().mcp_servers.clone()
    }

    /// Gets a configured MCP server by ID.
    pub fn get_mcp_server(&self, id: &str) -> Option<McpServerConfig> {
        self.config
            .lock()
            .unwrap()
            .mcp_servers
            .iter()
            .find(|server| server.id == id)
            .cloned()
    }

    /// Adds a new MCP server and saves config.
    pub fn add_mcp_server(&self, server: McpServerConfig) -> Result<(), ConfigError> {
        self.update(|config| {
            config.mcp_servers.push(server);
        })
    }

    /// Updates an existing MCP server and saves config.
    pub fn update_mcp_server<F>(&self, id: &str, updater: F) -> Result<(), ConfigError>
    where
        F: FnOnce(&mut McpServerConfig),
    {
        self.update(|config| {
            if let Some(server) = config.mcp_servers.iter_mut().find(|server| server.id == id) {
                updater(server);
                server.updated_at = chrono::Utc::now().to_rfc3339();
            }
        })
    }

    /// Removes an MCP server and clears stale automation selections.
    ///
    /// Returns true if the server was removed.
    pub fn remove_mcp_server(&self, id: &str) -> Result<bool, ConfigError> {
        let mut removed = false;
        self.update(|config| {
            let initial_len = config.mcp_servers.len();
            config.mcp_servers.retain(|server| server.id != id);
            removed = config.mcp_servers.len() != initial_len;

            if removed {
                for agent in &mut config.agents {
                    agent
                        .selected_mcp_server_ids
                        .retain(|server_id| server_id != id);
                }
            }
        })?;
        Ok(removed)
    }

    /// Gets all automations.
    pub fn get_agents(&self) -> Vec<AgentConfig> {
        self.config.lock().unwrap().agents.clone()
    }

    /// Gets an automation by ID.
    pub fn get_agent(&self, id: &str) -> Option<AgentConfig> {
        self.config
            .lock()
            .unwrap()
            .agents
            .iter()
            .find(|agent| agent.id == id)
            .cloned()
    }

    /// Adds a new automation and saves config.
    pub fn add_agent(&self, agent: AgentConfig) -> Result<(), ConfigError> {
        self.update(|config| {
            config.agents.push(agent);
        })
    }

    /// Updates an existing automation and saves config.
    pub fn update_agent<F>(&self, id: &str, updater: F) -> Result<(), ConfigError>
    where
        F: FnOnce(&mut AgentConfig),
    {
        self.update(|config| {
            if let Some(agent) = config.agents.iter_mut().find(|agent| agent.id == id) {
                updater(agent);
                agent.updated_at = chrono::Utc::now().to_rfc3339();
            }
        })
    }

    /// Removes an automation by ID and saves config.
    ///
    /// Returns true if the automation was removed.
    pub fn remove_agent(&self, id: &str) -> Result<bool, ConfigError> {
        let mut removed = false;
        self.update(|config| {
            let initial_len = config.agents.len();
            config.agents.retain(|agent| agent.id != id);
            removed = config.agents.len() != initial_len;
        })?;
        Ok(removed)
    }

    /// Enables or disables an automation globally and saves config.
    pub fn set_agent_enabled(&self, agent_id: &str, enabled: bool) -> Result<bool, ConfigError> {
        let mut changed = false;
        self.update(|config| {
            if let Some(agent) = config.agents.iter_mut().find(|agent| agent.id == agent_id) {
                changed = agent.set_enabled(enabled);
            }
        })?;
        Ok(changed)
    }

    /// Gets all configured skill sources.
    pub fn get_skill_sources(&self) -> Vec<SkillSourceConfig> {
        self.config.lock().unwrap().skill_sources.clone()
    }

    /// Adds a skill source and saves config.
    pub fn add_skill_source(&self, source: SkillSourceConfig) -> Result<(), ConfigError> {
        self.update(|config| {
            config.skill_sources.push(source);
        })
    }

    /// Removes a skill source and clears agent selections from that source.
    pub fn remove_skill_source(&self, id: &str) -> Result<bool, ConfigError> {
        let mut removed = false;
        self.update(|config| {
            let initial_len = config.skill_sources.len();
            config.skill_sources.retain(|source| source.id != id);
            removed = config.skill_sources.len() != initial_len;

            if removed {
                let prefix = format!("{}:", id);
                for agent in &mut config.agents {
                    agent
                        .selected_skill_ids
                        .retain(|skill_id| !skill_id.starts_with(&prefix));
                }
            }
        })?;
        Ok(removed)
    }
}

pub fn discover_skills(config: &ClaiConfig) -> Result<Vec<SkillDefinition>, String> {
    Ok(discover_skills_with_diagnostics(config).0)
}

pub fn discover_skills_with_diagnostics(
    config: &ClaiConfig,
) -> (Vec<SkillDefinition>, Vec<SkillSourceDiagnostic>) {
    let mut skills = Vec::new();
    let mut diagnostics = Vec::new();

    for source in &config.skill_sources {
        if !source.enabled {
            diagnostics.push(SkillSourceDiagnostic {
                source_id: source.id.clone(),
                source_name: source.name.clone(),
                ok: true,
                message: Some("Source is disabled.".to_string()),
                skill_count: 0,
            });
            continue;
        }

        let Some(root) = skill_source_local_path(source) else {
            diagnostics.push(SkillSourceDiagnostic {
                source_id: source.id.clone(),
                source_name: source.name.clone(),
                ok: false,
                message: Some("Source has no local path yet. Refresh or re-add it.".to_string()),
                skill_count: 0,
            });
            continue;
        };
        if !root.exists() {
            diagnostics.push(SkillSourceDiagnostic {
                source_id: source.id.clone(),
                source_name: source.name.clone(),
                ok: false,
                message: Some(format!("Source path does not exist: {}", root.display())),
                skill_count: 0,
            });
            continue;
        }

        let before = skills.len();
        match discover_skill_files(source, &root, &root, &mut skills) {
            Ok(()) => diagnostics.push(SkillSourceDiagnostic {
                source_id: source.id.clone(),
                source_name: source.name.clone(),
                ok: true,
                message: None,
                skill_count: skills.len() - before,
            }),
            Err(error) => {
                skills.truncate(before);
                diagnostics.push(SkillSourceDiagnostic {
                    source_id: source.id.clone(),
                    source_name: source.name.clone(),
                    ok: false,
                    message: Some(error),
                    skill_count: 0,
                });
            }
        }
    }

    skills.sort_by(|left, right| {
        left.name
            .to_lowercase()
            .cmp(&right.name.to_lowercase())
            .then_with(|| left.id.cmp(&right.id))
    });
    (skills, diagnostics)
}

pub fn agent_instructions_with_skills(config: &ClaiConfig, agent: &AgentConfig) -> String {
    let base = agent.description.clone();
    let Ok(skills) = discover_skills(config) else {
        return base;
    };
    let selected: Vec<_> = agent
        .selected_skill_ids
        .iter()
        .filter_map(|skill_id| skills.iter().find(|skill| &skill.id == skill_id))
        .collect();

    if selected.is_empty() {
        return base;
    }

    let mut prompt = base;
    prompt.push_str("\n\n## Selected Skills\n");
    prompt.push_str(
        "Use these reusable skill instructions when they are relevant to the current task. \
         If a skill expects a tool or runtime capability that is unavailable, report that as a runtime blocker.\n",
    );
    for skill in selected {
        prompt.push('\n');
        prompt.push_str(&skill.prompt_section());
        prompt.push('\n');
    }
    prompt
}

fn skill_source_local_path(source: &SkillSourceConfig) -> Option<PathBuf> {
    match &source.source {
        SkillSourceKind::Local { path } => Some(PathBuf::from(path)),
        SkillSourceKind::Git { local_path, .. } => local_path.as_ref().map(PathBuf::from),
    }
}

fn discover_skill_files(
    source: &SkillSourceConfig,
    root: &Path,
    dir: &Path,
    skills: &mut Vec<SkillDefinition>,
) -> Result<(), String> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) => {
            return Err(format!(
                "Failed to read skill source directory {}: {}",
                dir.display(),
                error
            ));
        }
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            if should_skip_skill_dir(&path) {
                continue;
            }
            discover_skill_files(source, root, &path, skills)?;
            continue;
        }

        if !file_type.is_file()
            || path.file_name().and_then(|name| name.to_str()) != Some("SKILL.md")
        {
            continue;
        }

        let content = fs::read_to_string(&path)
            .map_err(|error| format!("Failed to read {}: {}", path.display(), error))?;
        let relative_path = path
            .strip_prefix(root)
            .unwrap_or(path.as_path())
            .to_string_lossy()
            .replace('\\', "/");
        let skill_dir = Path::new(&relative_path)
            .parent()
            .map(|path| path.to_string_lossy().replace('\\', "/"))
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "root".to_string());

        skills.push(SkillDefinition {
            id: format!("{}:{}", source.id, skill_dir),
            name: skill_name_from_content(&content).unwrap_or_else(|| skill_dir.clone()),
            description: skill_description_from_content(&content).unwrap_or_default(),
            source_id: source.id.clone(),
            source_name: source.name.clone(),
            source_path: relative_path,
            content,
        });
    }

    Ok(())
}

fn should_skip_skill_dir(path: &Path) -> bool {
    matches!(
        path.file_name().and_then(|name| name.to_str()),
        Some(".git" | "node_modules" | "target" | "dist" | "build" | ".venv" | "venv")
    )
}

fn skill_name_from_content(content: &str) -> Option<String> {
    front_matter_field(content, "name").or_else(|| {
        content
            .lines()
            .map(str::trim)
            .find_map(|line| line.strip_prefix("# ").map(str::trim))
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    })
}

fn skill_description_from_content(content: &str) -> Option<String> {
    front_matter_field(content, "description").or_else(|| {
        content
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#') && *line != "---")
            .find(|line| !line.contains(':'))
            .map(|line| line.chars().take(240).collect())
    })
}

fn front_matter_field(content: &str, key: &str) -> Option<String> {
    let mut lines = content.lines().map(str::trim);
    if lines.next()? != "---" {
        return None;
    }
    let prefix = format!("{}:", key);
    lines
        .take_while(|line| *line != "---")
        .find_map(|line| line.strip_prefix(&prefix).map(str::trim))
        .map(|value| value.trim_matches('"').trim_matches('\'').to_string())
        .filter(|value| !value.is_empty())
}

/// Errors that can occur during config operations.
#[derive(Debug)]
pub enum ConfigError {
    /// Could not determine config directory.
    NoConfigDir,

    /// IO error during config operations.
    Io {
        operation: String,
        source: std::io::Error,
    },

    /// Error parsing config file.
    Parse { source: serde_json::Error },

    /// Error serializing config.
    Serialize { source: serde_json::Error },
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigError::NoConfigDir => write!(f, "Could not determine config directory"),
            ConfigError::Io { operation, source } => {
                write!(f, "Failed to {}: {}", operation, source)
            }
            ConfigError::Parse { source } => write!(f, "Failed to parse config: {}", source),
            ConfigError::Serialize { source } => {
                write!(f, "Failed to serialize config: {}", source)
            }
        }
    }
}

impl std::error::Error for ConfigError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ConfigError::NoConfigDir => None,
            ConfigError::Io { source, .. } => Some(source),
            ConfigError::Parse { source } => Some(source),
            ConfigError::Serialize { source } => Some(source),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn create_test_manager() -> (ConfigManager, TempDir) {
        let temp_dir = TempDir::new().unwrap();
        let config_path = temp_dir.path().join("config.json");

        let manager = ConfigManager {
            config: Mutex::new(ClaiConfig::default()),
            config_path,
        };

        (manager, temp_dir)
    }

    #[test]
    fn test_config_manager_save_load() {
        let (manager, _temp_dir) = create_test_manager();

        manager
            .set_ai_provider(AiProvider::Claude { model: None })
            .unwrap();

        assert!(manager.config_path.exists());

        let loaded = ConfigManager::load_from_file(&manager.config_path).unwrap();
        assert!(matches!(
            loaded.ai_provider,
            Some(AiProvider::Claude { .. })
        ));
    }

    #[test]
    fn test_remove_mcp_server_cleans_agent_selection() {
        let (manager, _temp_dir) = create_test_manager();

        let server = McpServerConfig::new(
            "Filesystem".to_string(),
            McpServerTransport::Stdio {
                command: "npx".to_string(),
                args: vec!["@modelcontextprotocol/server-filesystem".to_string()],
            },
        );
        let server_id = server.id.clone();
        manager.add_mcp_server(server).unwrap();

        let mut agent = AgentConfig::new("Test".to_string(), "Desc".to_string(), 5);
        agent.selected_mcp_server_ids = vec![server_id.clone()];
        manager.add_agent(agent).unwrap();

        assert!(manager.remove_mcp_server(&server_id).unwrap());

        let agent = manager.get_agents().into_iter().next().unwrap();
        assert!(agent.selected_mcp_server_ids.is_empty());
    }

    #[test]
    fn test_discover_skills_loads_local_skill_md() {
        let temp_dir = TempDir::new().unwrap();
        let skill_dir = temp_dir.path().join("code-review");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\ndescription: \"Review diffs carefully.\"\n---\n# Code Review\nReview with care.",
        )
        .unwrap();

        let source = SkillSourceConfig::new_local(
            "Local Skills".to_string(),
            temp_dir.path().display().to_string(),
        );
        let mut config = ClaiConfig::default();
        config.skill_sources.push(source.clone());

        let skills = discover_skills(&config).unwrap();

        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "Code Review");
        assert_eq!(skills[0].description, "Review diffs carefully.");
        assert_eq!(skills[0].source_id, source.id);
        assert_eq!(skills[0].source_path, "code-review/SKILL.md");

        let mut agent = AgentConfig::new("Reviewer".to_string(), "Base prompt".to_string(), 5);
        agent.selected_skill_ids = vec![skills[0].id.clone()];
        let prompt = agent_instructions_with_skills(&config, &agent);

        assert!(prompt.contains("Base prompt"));
        assert!(prompt.contains("## Selected Skills"));
        assert!(prompt.contains("Review with care."));
    }

    #[test]
    fn test_remove_skill_source_cleans_agent_selection() {
        let (manager, _temp_dir) = create_test_manager();
        let source =
            SkillSourceConfig::new_local("Local Skills".to_string(), "/tmp/skills".to_string());
        let source_id = source.id.clone();
        manager.add_skill_source(source).unwrap();

        let mut agent = AgentConfig::new("Test".to_string(), "Desc".to_string(), 5);
        agent.selected_skill_ids = vec![format!("{}:code-review", source_id)];
        manager.add_agent(agent).unwrap();

        assert!(manager.remove_skill_source(&source_id).unwrap());

        let agent = manager.get_agents().into_iter().next().unwrap();
        assert!(agent.selected_skill_ids.is_empty());
    }
}
