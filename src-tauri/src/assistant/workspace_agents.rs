//! The workspace roster as rows: one per agent, Main first, built from the
//! live workspace config. Shared by the workspace commands and the agents'
//! task tools and prompt.

use std::collections::HashMap;

use crate::assistant::types::WorkspaceAgentSummary;
use crate::config::global_agents::{AgentSource, ResolvedAgent};
use crate::config::workspace_config::{self, AgentAvatarRef};
use crate::config::{AppConfig, ExecutionCapabilityConfig, WorkspaceConfig};
use crate::AppState;

#[derive(Debug, Clone, Default)]
pub(crate) struct WorkspaceAgentRow {
    pub(crate) id: String,
    pub(crate) workspace_id: String,
    /// The shared definition for a teammate, the local id for the Main.
    /// Callers address agents by `id`; this is provenance only.
    pub(crate) agent_definition_id: String,
    pub(crate) role: String,
    pub(crate) enabled: bool,
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) selected_skill_ids: Vec<String>,
    pub(crate) selected_mcp_server_ids: Vec<String>,
    pub(crate) provider_connection_ids: Vec<String>,
    pub(crate) execution: ExecutionCapabilityConfig,
    pub(crate) avatar: Option<AgentAvatarRef>,
    pub(crate) created_at: i64,
    pub(crate) updated_at: i64,
}

impl WorkspaceAgentRow {
    /// The name shown for this agent everywhere — crew list, prompt roster,
    /// task cards, tool output: its own name, or the definition id when blank.
    pub(crate) fn display_name(&self) -> &str {
        if self.name.trim().is_empty() {
            &self.agent_definition_id
        } else {
            &self.name
        }
    }
}

pub(crate) fn load_workspace_agent_rows(
    state: &AppState,
    workspace_id: &str,
) -> Result<Vec<WorkspaceAgentRow>, String> {
    let app_config = state
        .config_manager
        .lock()
        .map_err(|e| format!("Lock error: {e}"))?
        .get();
    let (config, roster) = state.resolve_workspace_roster(workspace_id)?;
    Ok(workspace_agent_rows_from(&app_config, &config, &roster))
}

pub(crate) fn workspace_agent_rows_from(
    app_config: &AppConfig,
    config: &WorkspaceConfig,
    roster: &[ResolvedAgent],
) -> Vec<WorkspaceAgentRow> {
    let mut rows: Vec<_> = roster
        .iter()
        .map(|resolved| workspace_agent_row_from_config(app_config, config, resolved))
        .collect();
    // Main first, then teammates in the order they joined.
    rows.sort_by_key(|row| (row.role != "manager", row.created_at));
    rows
}

fn workspace_agent_row_from_config(
    app_config: &AppConfig,
    workspace: &WorkspaceConfig,
    resolved: &ResolvedAgent,
) -> WorkspaceAgentRow {
    let agent = &resolved.agent;
    WorkspaceAgentRow {
        id: agent.id.clone(),
        workspace_id: workspace.id.clone(),
        agent_definition_id: resolved
            .definition_id()
            .unwrap_or(agent.id.as_str())
            .to_string(),
        role: if matches!(resolved.source, AgentSource::Main) {
            "manager".to_string()
        } else {
            "member".to_string()
        },
        enabled: agent.enabled,
        name: agent.name.clone(),
        description: agent.description.clone(),
        selected_skill_ids: workspace_config::refs_to_skill_ids(app_config, &agent.selected_skills),
        selected_mcp_server_ids: workspace_config::enabled_mcp_ids(&agent.selected_mcp_servers),
        provider_connection_ids: agent.provider_connection_ids.clone(),
        execution: agent.execution.clone(),
        avatar: agent.avatar.clone(),
        created_at: agent.created_at,
        updated_at: agent.updated_at,
    }
}

pub(crate) fn concise_agent_description(description: Option<String>) -> Option<String> {
    let text = description?;
    let first_line = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())?
        .to_string();

    const MAX_SUMMARY_CHARS: usize = 240;
    if first_line.chars().count() <= MAX_SUMMARY_CHARS {
        return Some(first_line);
    }

    let mut summary: String = first_line.chars().take(MAX_SUMMARY_CHARS).collect();
    summary.push_str("...");
    Some(summary)
}

/// Workspace-agent id → display name, for naming a task's assignee and
/// creator. Roster failures degrade to raw ids rather than failing the task read.
pub(crate) fn workspace_agent_display_names(
    state: &AppState,
    workspace_id: &str,
) -> HashMap<String, String> {
    agent_display_names(&load_workspace_agent_rows(state, workspace_id).unwrap_or_default())
}

pub(crate) fn agent_display_names(rows: &[WorkspaceAgentRow]) -> HashMap<String, String> {
    rows.iter()
        .map(|agent| (agent.id.clone(), agent.display_name().to_string()))
        .collect()
}

/// The team roster as agents see it in their prompt and tool context.
pub(crate) fn workspace_agent_summaries_from(
    app_config: &AppConfig,
    config: &WorkspaceConfig,
    roster: &[ResolvedAgent],
) -> Vec<WorkspaceAgentSummary> {
    let main_agent_id = config.main_agent.as_ref().map(|agent| agent.id.as_str());
    workspace_agent_rows_from(app_config, config, roster)
        .into_iter()
        .map(|row| WorkspaceAgentSummary {
            display_name: row.display_name().to_string(),
            is_default: main_agent_id == Some(row.id.as_str()),
            description: concise_agent_description(Some(row.description)),
            id: row.id,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::WorkspaceAgent;

    const WS: &str = "ws-1";
    const MGR: &str = "mgr-1";

    #[test]
    fn workspace_agent_rows_put_the_main_first_then_teammates_by_join_time() {
        let teammate = |id: &str, created_at: i64| {
            let mut agent = WorkspaceAgent::new_manager(id.to_string(), created_at);
            agent.name = id.to_string();
            ResolvedAgent {
                agent,
                source: AgentSource::Shared {
                    definition_id: format!("def-{id}"),
                    revision: 1,
                },
            }
        };
        let main = ResolvedAgent {
            agent: WorkspaceAgent::new_manager(MGR.to_string(), 50),
            source: AgentSource::Main,
        };
        // Ids sort opposite to join time, so an id-ordered roster would fail.
        let roster = vec![teammate("a-late", 30), main, teammate("z-early", 10)];
        let workspace = WorkspaceConfig::new(WS.to_string(), "W".to_string(), 1, MGR.to_string());

        let ids: Vec<_> = workspace_agent_rows_from(&AppConfig::default(), &workspace, &roster)
            .into_iter()
            .map(|row| row.id)
            .collect();

        assert_eq!(ids, [MGR, "z-early", "a-late"]);
    }

    #[test]
    fn display_name_is_the_name_or_the_definition_id_when_blank() {
        let mut row = WorkspaceAgentRow {
            agent_definition_id: "def-1".to_string(),
            name: "Reviewer".to_string(),
            ..WorkspaceAgentRow::default()
        };
        assert_eq!(row.display_name(), "Reviewer");

        row.name = "  ".to_string();
        assert_eq!(row.display_name(), "def-1");
    }

    #[test]
    fn agent_display_names_label_blank_names_with_the_definition_id() {
        let rows = [
            WorkspaceAgentRow {
                id: "agent-1".to_string(),
                agent_definition_id: "def-1".to_string(),
                name: "Reviewer".to_string(),
                ..WorkspaceAgentRow::default()
            },
            WorkspaceAgentRow {
                id: "agent-2".to_string(),
                agent_definition_id: "def-2".to_string(),
                name: String::new(),
                ..WorkspaceAgentRow::default()
            },
        ];

        let names = agent_display_names(&rows);

        assert_eq!(names.len(), 2);
        assert_eq!(names["agent-1"], "Reviewer");
        assert_eq!(names["agent-2"], "def-2");
    }

    // -----------------------------------------------------------------
    // concise_agent_description
    // -----------------------------------------------------------------

    #[test]
    fn concise_agent_description_none_returns_none() {
        assert_eq!(concise_agent_description(None), None);
    }

    #[test]
    fn concise_agent_description_empty_returns_none() {
        assert_eq!(concise_agent_description(Some("".to_string())), None);
    }

    #[test]
    fn concise_agent_description_whitespace_only_returns_none() {
        assert_eq!(
            concise_agent_description(Some("   \n\t  ".to_string())),
            None
        );
    }

    #[test]
    fn concise_agent_description_short_single_line() {
        let input = Some("A short description.".to_string());
        assert_eq!(
            concise_agent_description(input),
            Some("A short description.".to_string())
        );
    }

    #[test]
    fn concise_agent_description_takes_first_non_empty_line() {
        let input = Some("\n\nSecond line is first non-empty.\nThird line.".to_string());
        assert_eq!(
            concise_agent_description(input),
            Some("Second line is first non-empty.".to_string())
        );
    }

    #[test]
    fn concise_agent_description_trims_leading_whitespace() {
        let input = Some("   \t  Trimmed description.".to_string());
        assert_eq!(
            concise_agent_description(input),
            Some("Trimmed description.".to_string())
        );
    }

    #[test]
    fn concise_agent_description_truncates_long_line() {
        let long = "x".repeat(300);
        let result = concise_agent_description(Some(long));
        let expected = "x".repeat(240) + "...";
        assert_eq!(result, Some(expected));
    }

    #[test]
    fn concise_agent_description_exactly_max_length_passes_through() {
        let exact = "x".repeat(240);
        assert_eq!(concise_agent_description(Some(exact.clone())), Some(exact));
    }
}
