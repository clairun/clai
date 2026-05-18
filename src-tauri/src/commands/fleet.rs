use serde::Serialize;
use tauri::State;

use crate::assistant::repository;
use crate::assistant::types::{AssistantMessage, ContentPart, MessageRole, RunStatus};
use crate::config::ExecutionCapabilityConfig;
use crate::db::DbPool;
use crate::AppState;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FleetSummary {
    pub total: usize,
    pub enabled: usize,
    pub running: usize,
    pub error: usize,
    pub idle: usize,
    pub disabled: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FleetAgentStatus {
    Running,
    Ok,
    Warning,
    Error,
    Idle,
    Disabled,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FleetRunEntry {
    pub status: RunStatus,
    pub started_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FleetAgentSnapshot {
    pub agent_id: String,
    pub name: String,
    pub description: String,
    pub enabled: bool,
    pub interval_minutes: u32,
    pub status: FleetAgentStatus,
    pub selected_mcp_server_ids: Vec<String>,
    pub selected_mcp_server_names: Vec<String>,
    pub selected_skill_ids: Vec<String>,
    pub provider_connection_ids: Vec<String>,
    pub provider_connection_names: Vec<String>,
    pub execution: ExecutionCapabilityConfig,
    pub tab_id: Option<String>,
    pub session_id: Option<String>,
    pub last_started_at: Option<i64>,
    pub last_completed_at: Option<i64>,
    pub next_run_in_seconds: Option<u64>,
    pub last_run_status: Option<RunStatus>,
    pub last_error: Option<String>,
    pub last_message_preview: Option<String>,
    pub recent_run_statuses: Vec<FleetRunEntry>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FleetSnapshot {
    pub summary: FleetSummary,
    pub agents: Vec<FleetAgentSnapshot>,
}

fn extract_message_preview(message: &AssistantMessage) -> Option<String> {
    let mut parts = Vec::new();

    for part in &message.content {
        if let ContentPart::Text { text } = part {
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                parts.push(trimmed.to_string());
            }
        }
    }

    if parts.is_empty() {
        None
    } else {
        let joined = parts.join(" ");
        let preview: String = joined.chars().take(200).collect();
        Some(preview)
    }
}

#[tauri::command]
pub async fn fleet_get_snapshot(
    state: State<'_, AppState>,
    pool: State<'_, DbPool>,
) -> Result<FleetSnapshot, String> {
    let (agents, mcp_servers) = {
        let config_manager = state
            .config_manager
            .lock()
            .map_err(|e| format!("Lock error: {}", e))?;
        (
            config_manager.get_agents(),
            config_manager.get_mcp_servers(),
        )
    };

    let mcp_name_map: std::collections::HashMap<&str, &str> = mcp_servers
        .iter()
        .map(|s| (s.id.as_str(), s.name.as_str()))
        .collect();

    let connections = repository::list_provider_connections(pool.inner()).await?;
    let connection_name_map: std::collections::HashMap<&str, &str> = connections
        .iter()
        .map(|c| (c.id.as_str(), c.name.as_str()))
        .collect();

    let sessions = repository::list_sessions(pool.inner(), None).await?;

    let scheduler_info: std::collections::HashMap<String, (Option<String>, bool, u64)> = {
        let scheduler = state.scheduler.lock().await;
        scheduler
            .all_instances()
            .map(|instance| {
                (
                    instance.agent_id.clone(),
                    (
                        instance.tab_id.clone(),
                        instance.is_running,
                        instance.seconds_until_next_run(),
                    ),
                )
            })
            .collect()
    };

    let mut items = Vec::with_capacity(agents.len());

    for agent in agents {
        let session = sessions
            .iter()
            .filter(|session| session.context.automation_id.as_deref() == Some(agent.id.as_str()))
            .max_by_key(|session| session.updated_at)
            .cloned();

        let (last_run, recent_run_statuses, last_message_preview) = if let Some(session) = &session
        {
            let runs = repository::list_runs(pool.inner(), &session.id).await?;

            let recent: Vec<FleetRunEntry> = runs
                .iter()
                .take(24)
                .map(|r| FleetRunEntry {
                    status: r.status.clone(),
                    started_at: Some(r.started_at),
                })
                .collect();

            let last_run = runs.into_iter().next();

            let messages = repository::list_messages(pool.inner(), &session.id).await?;
            let last_assistant_message = messages
                .iter()
                .rev()
                .find(|message| message.role == MessageRole::Assistant);

            (
                last_run,
                recent,
                last_assistant_message.and_then(extract_message_preview),
            )
        } else {
            (None, Vec::new(), None)
        };

        let scheduler_entry = scheduler_info.get(&agent.id);
        let tab_id = scheduler_entry
            .and_then(|(tab_id, _, _)| tab_id.clone())
            .or_else(|| session.as_ref().and_then(|value| value.tab_id.clone()));
        let is_running = scheduler_entry
            .map(|(_, running, _)| *running)
            .unwrap_or(false);
        let next_run_in_seconds = scheduler_entry.map(|(_, _, seconds)| *seconds);

        let status = if !agent.enabled {
            FleetAgentStatus::Disabled
        } else if is_running {
            FleetAgentStatus::Running
        } else if let Some(run) = &last_run {
            match run.status {
                RunStatus::Completed => FleetAgentStatus::Ok,
                RunStatus::CompletedWithWarnings => FleetAgentStatus::Warning,
                RunStatus::Failed | RunStatus::Cancelled => FleetAgentStatus::Error,
                // The scheduler is the source of truth for whether an agent is
                // actively running.  If we reach this branch, is_running is false,
                // so a DB run stuck in a non-terminal state is stale — treat it as
                // OK (it likely completed but the status update was lost).
                RunStatus::Queued | RunStatus::Running | RunStatus::WaitingForTool => {
                    FleetAgentStatus::Ok
                }
            }
        } else {
            FleetAgentStatus::Idle
        };

        let mcp_names: Vec<String> = agent
            .selected_mcp_server_ids
            .iter()
            .filter_map(|id| mcp_name_map.get(id.as_str()).map(|n| n.to_string()))
            .collect();

        let connection_names: Vec<String> = agent
            .provider_connection_ids
            .iter()
            .filter_map(|id| connection_name_map.get(id.as_str()).map(|n| n.to_string()))
            .collect();

        items.push(FleetAgentSnapshot {
            agent_id: agent.id,
            name: agent.name,
            description: agent.description,
            enabled: agent.enabled,
            interval_minutes: agent.interval_minutes,
            status,
            selected_mcp_server_ids: agent.selected_mcp_server_ids,
            selected_mcp_server_names: mcp_names,
            selected_skill_ids: agent.selected_skill_ids,
            provider_connection_ids: agent.provider_connection_ids,
            provider_connection_names: connection_names,
            execution: agent.execution,
            tab_id,
            session_id: session.as_ref().map(|value| value.id.clone()),
            last_started_at: last_run.as_ref().map(|run| run.started_at),
            last_completed_at: last_run.as_ref().and_then(|run| run.completed_at),
            next_run_in_seconds,
            last_run_status: last_run.as_ref().map(|run| run.status.clone()),
            last_error: last_run.as_ref().and_then(|run| run.error.clone()),
            last_message_preview,
            recent_run_statuses,
        });
    }

    items.sort_by_key(|a| a.name.to_lowercase());

    let summary = FleetSummary {
        total: items.len(),
        enabled: items.iter().filter(|agent| agent.enabled).count(),
        running: items
            .iter()
            .filter(|agent| matches!(agent.status, FleetAgentStatus::Running))
            .count(),
        error: items
            .iter()
            .filter(|agent| matches!(agent.status, FleetAgentStatus::Error))
            .count(),
        idle: items
            .iter()
            .filter(|agent| matches!(agent.status, FleetAgentStatus::Idle))
            .count(),
        disabled: items
            .iter()
            .filter(|agent| matches!(agent.status, FleetAgentStatus::Disabled))
            .count(),
    };

    Ok(FleetSnapshot {
        summary,
        agents: items,
    })
}

#[tauri::command]
pub async fn fleet_run_now(agent_id: String, state: State<'_, AppState>) -> Result<(), String> {
    // Verify the agent exists and is enabled
    {
        let config_manager = state
            .config_manager
            .lock()
            .map_err(|e| format!("Lock error: {}", e))?;
        let agent = config_manager
            .get_agents()
            .into_iter()
            .find(|a| a.id == agent_id)
            .ok_or_else(|| format!("Agent not found: {}", agent_id))?;
        if !agent.enabled {
            return Err("Agent is disabled. Enable it first.".to_string());
        }
    }

    let mut scheduler = state.scheduler.lock().await;
    if scheduler.force_ready(&agent_id) {
        Ok(())
    } else {
        Err("Agent is currently running or has no scheduler instance.".to_string())
    }
}
