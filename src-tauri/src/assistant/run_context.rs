//! The agent configuration a run executes with, resolved from the live
//! workspace config when the run starts.
//!
//! Nothing here is persisted: a session stores only which agent owns it, so a
//! config edit (MCP servers, grants, skills, roster) reaches the next run of
//! every session without rewriting session rows.

use std::path::PathBuf;

use crate::assistant::types::WorkspaceAgentSummary;
use crate::config::global_agents::AgentSource;
use crate::config::{workspace_config, ExecutionCapabilityConfig};
use crate::AppState;

#[derive(Debug, Clone, Default)]
pub struct RunContext {
    pub automation_id: Option<String>,
    pub automation_name: Option<String>,
    /// The agent's description with its skills' instructions folded in.
    pub agent_description: Option<String>,
    pub mcp_server_ids: Vec<String>,
    pub execution: ExecutionCapabilityConfig,
    pub workspace_agents: Vec<WorkspaceAgentSummary>,
    /// The run's workspace directory, resolved with the rest of the config.
    pub workspace_root: PathBuf,
}

/// Resolve the run configuration for `agent_id` in `workspace_id`.
///
/// `None` is a manager-less workspace: web access, no MCP servers. An agent
/// that is no longer on the roster, or whose library definition is gone, is an
/// error rather than a run on stale settings. `enabled` is not checked: entry
/// points decide whether a disabled agent may start.
pub fn resolve_run_context(
    state: &AppState,
    workspace_id: &str,
    agent_id: Option<&str>,
) -> Result<RunContext, String> {
    let root = state
        .workspace_root(workspace_id)
        .ok_or_else(|| format!("Workspace not found: {workspace_id}"))?;
    let config = workspace_config::load(&root).map_err(|e| e.to_string())?;
    let (config, roster) = state.resolve_roster_for(config)?;
    let app_config = state
        .config_manager
        .lock()
        .map_err(|e| format!("Lock error: {e}"))?
        .get();
    let workspace_agents =
        crate::commands::workspace::workspace_agent_summaries_from(&app_config, &config, &roster);

    let Some(agent_id) = agent_id else {
        let mut execution = ExecutionCapabilityConfig::default();
        execution.web.enabled = true;
        return Ok(RunContext {
            execution,
            workspace_agents,
            workspace_root: root,
            ..RunContext::default()
        });
    };
    let agent = roster
        .iter()
        .find(|resolved| resolved.agent.id == agent_id)
        .filter(|resolved| !matches!(resolved.source, AgentSource::Unavailable { .. }))
        .map(|resolved| &resolved.agent)
        .ok_or_else(|| format!("Agent {agent_id} is no longer part of workspace {workspace_id}"))?;
    let skill_ids = workspace_config::refs_to_skill_ids(&app_config, &agent.selected_skills);

    Ok(RunContext {
        automation_id: Some(agent.id.clone()),
        automation_name: Some(agent.name.clone()),
        agent_description: Some(crate::config::compose_agent_instructions(
            &app_config,
            &agent.description,
            &skill_ids,
        )),
        mcp_server_ids: workspace_config::enabled_mcp_ids(&agent.selected_mcp_servers),
        execution: agent.execution.clone(),
        workspace_agents,
        workspace_root: root,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assistant::repository;
    use crate::assistant::tools::workspace_tasks::task_session_context;
    use crate::assistant::types::AssistantSession;
    use crate::config::global_agents::AgentDefinition;
    use crate::config::workspace_config::{McpRef, WorkspaceAssignment, WorkspaceConfig};
    use crate::config::{AppConfig, FilesystemPathAccess, FilesystemPathGrant, ShellAccessMode};
    use crate::workspace_index::WorkspaceIndex;

    const WS: &str = "77777777-7777-4777-8777-777777777777";
    const MAIN: &str = "88888888-8888-4888-8888-888888888888";
    const MEMBER: &str = "99999999-9999-4999-8999-999999999999";
    const DEFINITION: &str = "def-reviewer";

    struct Fixture {
        _temp: tempfile::TempDir,
        state: AppState,
    }

    fn mcp(ids: &[&str]) -> Vec<McpRef> {
        ids.iter()
            .map(|id| McpRef {
                id: (*id).to_string(),
                disabled: false,
            })
            .collect()
    }

    fn definition() -> AgentDefinition {
        let mut behavior = crate::config::workspace_config::WorkspaceAgent::new_manager(
            DEFINITION.to_string(),
            1_000,
        );
        behavior.name = "Reviewer".to_string();
        AgentDefinition {
            id: DEFINITION.to_string(),
            revision: 1,
            archived: false,
            behavior,
        }
    }

    fn assignment(enabled: bool) -> WorkspaceAssignment {
        WorkspaceAssignment {
            id: MEMBER.to_string(),
            agent_definition_id: DEFINITION.to_string(),
            enabled,
            context: String::new(),
            filesystem_grants: Vec::new(),
            created_at: 1_000,
            updated_at: 1_000,
        }
    }

    fn fixture(with_main: bool) -> Fixture {
        let temp = tempfile::tempdir().expect("temp dir");
        let parent = temp.path().join("workspaces");
        let root = parent.join(WS);
        let mut config =
            WorkspaceConfig::new(WS.to_string(), "Run".to_string(), 1_000, MAIN.to_string());
        match config.main_agent.as_mut() {
            Some(main) if with_main => {
                main.selected_mcp_servers = mcp(&["a"]);
                main.execution.shell.mode = ShellAccessMode::Off;
            }
            _ => config.main_agent = None,
        }
        workspace_config::save(&root, &config).expect("seed workspace");
        let mut index = WorkspaceIndex::default();
        index.insert_config(root, &config);
        let app_config = AppConfig {
            workspace_dirs: vec![parent],
            agent_definitions: vec![definition()],
            ..AppConfig::default()
        };
        let config_manager =
            crate::ConfigManager::new_for_tests(app_config, temp.path().join("config.json"));
        let state = AppState::new_for_tests(config_manager, index).expect("app state");
        Fixture { _temp: temp, state }
    }

    async fn context_json(pool: &crate::db::DbPool, session_id: &str) -> (String, i64) {
        sqlx::query_as("SELECT context_json, updated_at FROM assistant_sessions WHERE id = ?")
            .bind(session_id)
            .fetch_one(pool)
            .await
            .expect("session row")
    }

    #[tokio::test]
    async fn run_context_follows_config_edit_without_reconcile() {
        let fx = fixture(true);
        let pool = fx.state.workspace_db(WS).await.expect("workspace db");
        let session = repository::find_or_create_manager_session(&pool, MAIN)
            .await
            .expect("session");
        let before = context_json(&pool, &session.id).await;

        let run = resolve_run_context(&fx.state, WS, Some(MAIN)).expect("resolve");
        assert_eq!(run.mcp_server_ids, vec!["a".to_string()]);
        assert_eq!(run.execution.shell.mode, ShellAccessMode::Off);
        assert!(run.workspace_agents.iter().all(|agent| agent.id != MEMBER));

        fx.state
            .update_workspace_config(WS, |config| {
                let main = config.main_agent.as_mut().expect("main");
                main.selected_mcp_servers = mcp(&["b"]);
                main.execution.shell.mode = ShellAccessMode::Restricted;
                main.description = "Edited description".to_string();
                config.assignments.push(assignment(true));
                Ok(())
            })
            .expect("edit config");

        let run = resolve_run_context(&fx.state, WS, Some(MAIN)).expect("resolve");
        assert_eq!(run.mcp_server_ids, vec!["b".to_string()]);
        assert_eq!(run.execution.shell.mode, ShellAccessMode::Restricted);
        assert!(run.workspace_agents.iter().any(|agent| agent.id == MEMBER));
        assert!(run
            .agent_description
            .as_deref()
            .is_some_and(|text| text.contains("Edited description")));
        assert_eq!(context_json(&pool, &session.id).await, before);
    }

    #[tokio::test]
    async fn get_or_create_does_not_rewrite_existing_session() {
        let fx = fixture(true);
        let pool = fx.state.workspace_db(WS).await.expect("workspace db");
        let first = repository::find_or_create_manager_session(&pool, MAIN)
            .await
            .expect("session");
        let before = context_json(&pool, &first.id).await;
        fx.state
            .update_workspace_config(WS, |config| {
                config
                    .main_agent
                    .as_mut()
                    .expect("main")
                    .selected_mcp_servers = mcp(&["b"]);
                Ok(())
            })
            .expect("edit config");
        let second = repository::find_or_create_manager_session(&pool, MAIN)
            .await
            .expect("session");
        assert_eq!(second.id, first.id);
        assert_eq!(context_json(&pool, &first.id).await, before);
    }

    #[test]
    fn run_context_fails_closed_for_missing_and_unavailable_agent() {
        let fx = fixture(true);
        assert!(resolve_run_context(&fx.state, WS, Some("unknown")).is_err());

        fx.state
            .update_workspace_config(WS, |config| {
                config.assignments.push(assignment(false));
                Ok(())
            })
            .expect("assign");
        let disabled = resolve_run_context(&fx.state, WS, Some(MEMBER)).expect("disabled runs");
        assert_eq!(disabled.automation_name.as_deref(), Some("Reviewer"));

        fx.state
            .config_manager
            .lock()
            .expect("config lock")
            .update(|config| config.agent_definitions.clear())
            .expect("delete definition");
        assert!(resolve_run_context(&fx.state, WS, Some(MEMBER)).is_err());
    }

    fn grant(path: &str) -> FilesystemPathGrant {
        FilesystemPathGrant {
            path: path.to_string(),
            access: FilesystemPathAccess::ReadOnly,
            origin: None,
        }
    }

    fn set_definition(fx: &Fixture, edit: impl FnOnce(&mut AgentDefinition)) {
        fx.state
            .config_manager
            .lock()
            .expect("config lock")
            .update(|config| edit(&mut config.agent_definitions[0]))
            .expect("edit definition");
    }

    /// A task session for MEMBER, created the way `assign_task` creates it.
    async fn member_task_session(pool: &crate::db::DbPool) -> AssistantSession {
        crate::db::test_support::insert_task(pool, "task-1", "queued", None, None).await;
        repository::create_session_and_link_task(pool, task_session_context(MEMBER), "task-1")
            .await
            .expect("task session")
    }

    fn assign_member(fx: &Fixture, grants: Vec<FilesystemPathGrant>) {
        fx.state
            .update_workspace_config(WS, |config| {
                let mut member = assignment(true);
                member.filesystem_grants = grants;
                config.assignments.push(member);
                Ok(())
            })
            .expect("assign");
    }

    fn has_grant(run: &RunContext, path: &str) -> bool {
        run.execution
            .filesystem
            .extra_paths
            .iter()
            .any(|grant| grant.path == path)
    }

    #[tokio::test]
    async fn task_run_context_tracks_assignee_live_config() {
        let fx = fixture(true);
        set_definition(&fx, |definition| {
            definition.behavior.selected_mcp_servers = mcp(&["m1"]);
            definition.behavior.execution.shell.mode = ShellAccessMode::Full;
        });
        assign_member(&fx, vec![grant("/a")]);
        let pool = fx.state.workspace_db(WS).await.expect("workspace db");
        let session = member_task_session(&pool).await;
        let before = context_json(&pool, &session.id).await;
        let agent_id = session.context.automation_id.as_deref();

        let run = resolve_run_context(&fx.state, WS, agent_id).expect("resolve");
        assert_eq!(run.mcp_server_ids, vec!["m1".to_string()]);
        assert!(has_grant(&run, "/a"));
        assert_eq!(run.execution.shell.mode, ShellAccessMode::Full);

        set_definition(&fx, |definition| {
            definition.behavior.selected_mcp_servers = mcp(&["m2"]);
            definition.behavior.execution.shell.mode = ShellAccessMode::Off;
        });
        fx.state
            .update_workspace_config(WS, |config| {
                config.assignments[0].filesystem_grants.clear();
                Ok(())
            })
            .expect("revoke grant");

        let run = resolve_run_context(&fx.state, WS, agent_id).expect("resolve");
        assert_eq!(run.mcp_server_ids, vec!["m2".to_string()]);
        assert!(!has_grant(&run, "/a"));
        assert_eq!(run.execution.shell.mode, ShellAccessMode::Off);
        assert_eq!(context_json(&pool, &session.id).await, before);
    }

    #[tokio::test]
    async fn task_run_context_is_assignees_not_callers() {
        let fx = fixture(true);
        fx.state
            .update_workspace_config(WS, |config| {
                config
                    .main_agent
                    .as_mut()
                    .expect("main")
                    .selected_mcp_servers = mcp(&["main-only"]);
                Ok(())
            })
            .expect("edit main");
        assign_member(&fx, Vec::new());
        let pool = fx.state.workspace_db(WS).await.expect("workspace db");
        let session = member_task_session(&pool).await;

        let run = resolve_run_context(&fx.state, WS, session.context.automation_id.as_deref())
            .expect("resolve");
        assert_eq!(run.automation_id.as_deref(), Some(MEMBER));
        assert!(run.mcp_server_ids.is_empty());
    }

    #[tokio::test]
    async fn task_run_context_fails_after_unassign() {
        let fx = fixture(true);
        assign_member(&fx, Vec::new());
        let pool = fx.state.workspace_db(WS).await.expect("workspace db");
        let session = member_task_session(&pool).await;
        let agent_id = session.context.automation_id.as_deref();
        assert!(resolve_run_context(&fx.state, WS, agent_id).is_ok());

        fx.state
            .update_workspace_config(WS, |config| {
                config.assignments.clear();
                Ok(())
            })
            .expect("unassign");
        let error = resolve_run_context(&fx.state, WS, agent_id).expect_err("unassigned");
        assert!(error.contains(MEMBER), "{error}");
    }

    #[tokio::test]
    async fn task_run_context_fails_after_definition_deleted() {
        let fx = fixture(true);
        assign_member(&fx, Vec::new());
        let pool = fx.state.workspace_db(WS).await.expect("workspace db");
        let session = member_task_session(&pool).await;
        let agent_id = session.context.automation_id.as_deref();

        fx.state
            .config_manager
            .lock()
            .expect("config lock")
            .update(|config| config.agent_definitions.clear())
            .expect("delete definition");
        let error = resolve_run_context(&fx.state, WS, agent_id).expect_err("definition gone");
        assert!(error.contains(MEMBER), "{error}");
    }

    /// The roster's default entry carries the Main's local id, so only a run
    /// as the Main gets the manager tools.
    #[test]
    fn manager_tools_are_offered_only_to_the_main_agent() {
        let fx = fixture(true);
        assign_member(&fx, Vec::new());
        let offers_assign = |agent_id: &str| {
            let run = resolve_run_context(&fx.state, WS, Some(agent_id)).expect("resolve");
            crate::assistant::tools::registry::available_tools(&run, &[])
                .iter()
                .any(|tool| tool.name == "workspace_assignTask")
        };

        assert!(offers_assign(MAIN));
        assert!(!offers_assign(MEMBER));
    }

    #[test]
    fn run_context_without_main_has_web_and_no_mcp() {
        let fx = fixture(false);
        let run = resolve_run_context(&fx.state, WS, None).expect("resolve");
        assert!(run.execution.web.enabled);
        assert!(run.mcp_server_ids.is_empty());
        assert!(run.automation_id.is_none());
        assert!(run.agent_description.is_none());
        assert!(run.workspace_root.ends_with(WS));
    }
}
