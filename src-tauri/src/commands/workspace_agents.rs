//! Workspace-scoped agent commands.
//!
//! A workspace owns exactly one agent outright: its Main, stored in
//! `<workspace>/.clai/config.json` and editable here. Teammates are app-level
//! definitions ([`crate::commands::global_agents`]) made callable by an
//! assignment; the only parts of them a workspace may change are the local
//! overlays, which is why the update path below refuses an assignment id
//! instead of writing a copy of shared behavior into the workspace file.

use serde::{Deserialize, Serialize};
use tauri::State;
use ts_rs::TS;

use crate::config::{
    workspace_config, AppConfig, ExecutionCapabilityConfig, WorkspaceAgent, WorkspaceConfig,
};
use crate::AppState;

fn now_millis() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

#[derive(Debug, Clone, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "bindings.ts")]
pub struct WorkspaceAgentCreateRequest {
    pub workspace_id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub selected_skill_ids: Vec<String>,
    #[serde(default)]
    pub selected_mcp_server_ids: Vec<String>,
    #[serde(default)]
    pub provider_connection_ids: Vec<String>,
    #[serde(default)]
    pub execution: ExecutionCapabilityConfig,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Optional explicit id; if absent, a fresh UUID is generated.
    #[serde(default)]
    #[ts(optional)]
    pub id: Option<String>,
}

#[derive(Debug, Clone, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "bindings.ts")]
pub struct WorkspaceAgentUpdateRequest {
    pub workspace_id: String,
    pub agent_id: String,
    pub name: String,
    pub description: String,
    pub selected_skill_ids: Vec<String>,
    pub selected_mcp_server_ids: Vec<String>,
    pub provider_connection_ids: Vec<String>,
    pub execution: ExecutionCapabilityConfig,
    pub enabled: bool,
    /// The agent's `updatedAt` when the form loaded it. The save is refused
    /// if anything wrote the agent since, so it cannot silently revert that
    /// write.
    #[ts(type = "number")]
    pub expected_updated_at: i64,
}

/// Why saving the Main failed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[ts(export, export_to = "bindings.ts")]
pub enum WorkspaceAgentUpdateError {
    /// Something wrote the agent after the form loaded it: discard the edits
    /// and load it again, do not retry.
    Stale {
        message: String,
    },
    Failed {
        message: String,
    },
}

impl From<String> for WorkspaceAgentUpdateError {
    fn from(message: String) -> Self {
        Self::Failed { message }
    }
}

const STALE_AGENT: &str = "This agent was changed elsewhere after you opened it. Use Discard \
     to load the latest settings; your unsaved edits here will be lost.";

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceAgentEnabledRequest {
    pub workspace_id: String,
    pub agent_id: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "bindings.ts")]
pub struct WorkspaceAgentDetail {
    pub id: String,
    pub workspace_id: String,
    pub name: String,
    pub description: String,
    pub selected_skill_ids: Vec<String>,
    pub selected_mcp_server_ids: Vec<String>,
    pub provider_connection_ids: Vec<String>,
    pub execution: ExecutionCapabilityConfig,
    pub enabled: bool,
    pub is_default: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

fn default_true() -> bool {
    true
}

/// Returns the default execution-capability shape that a brand-new agent
/// ships with (host `$HOME` read-only by default). The UI calls this when
/// opening the "Add agent" form so the user can see the granted defaults
/// — and, importantly, remove them before saving — instead of having the
/// backend silently inject them on create.
#[tauri::command]
pub async fn workspace_agent_default_execution() -> Result<ExecutionCapabilityConfig, String> {
    Ok(workspace_config::default_agent_execution())
}

/// The agent as *stored*, for the settings form — deliberately not the resolved
/// one.
///
/// A resolved agent carries the workspace and assignment context appended to its
/// instructions. Handing that to the editor would save it back as the agent's
/// own instructions, and the next resolution would append the same context
/// again, growing the prompt on every save.
#[tauri::command]
pub async fn workspace_get_agent(
    workspace_id: String,
    agent_id: String,
    state: State<'_, AppState>,
) -> Result<Option<WorkspaceAgentDetail>, String> {
    stored_agent_detail(state.inner(), &workspace_id, &agent_id)
}

fn stored_agent_detail(
    state: &AppState,
    workspace_id: &str,
    agent_id: &str,
) -> Result<Option<WorkspaceAgentDetail>, String> {
    let root = state
        .workspace_root(workspace_id)
        .ok_or_else(|| format!("Workspace not found: {}", workspace_id))?;
    let config = workspace_config::load(&root).map_err(|e| e.to_string())?;
    let app_config = app_config(state)?;

    if let Some(main) = config.main_agent.as_ref().filter(|a| a.id == agent_id) {
        return Ok(Some(detail_from_agent(&app_config, &config, main)));
    }

    // A teammate's stored behavior lives in the library. It is shown here
    // read-only; edits go through the library or the assignment's overlays.
    let Some(assignment) = config.assignment(agent_id) else {
        return Ok(None);
    };
    Ok(app_config
        .agent_definitions
        .iter()
        .find(|definition| definition.id == assignment.agent_definition_id)
        .map(|definition| {
            let mut agent = definition.behavior.clone();
            agent.id = assignment.id.clone();
            agent.enabled = agent.enabled && assignment.enabled && !definition.archived;
            detail_from_agent(&app_config, &config, &agent)
        }))
}

#[tauri::command]
pub async fn workspace_create_agent(
    request: WorkspaceAgentCreateRequest,
    state: State<'_, AppState>,
) -> Result<WorkspaceAgentDetail, String> {
    create_main_agent(state.inner(), request)
}

fn create_main_agent(
    state: &AppState,
    request: WorkspaceAgentCreateRequest,
) -> Result<WorkspaceAgentDetail, String> {
    let app_config = app_config(state)?;
    let id = request
        .id
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    // The host `$HOME` RO default is pre-populated by the UI via
    // `workspace_agent_default_execution` so the user can see and remove it
    // before saving. Trust the request's execution verbatim — if the user
    // cleared all path grants on purpose, we honor that.
    let mut execution = request.execution;
    execution.sandbox.canonicalize()?;

    let now = now_millis();
    let agent = WorkspaceAgent {
        id: id.clone(),
        name: request.name,
        description: request.description,
        enabled: request.enabled,
        selected_skills: workspace_config::skill_ids_to_refs(
            &app_config,
            &request.selected_skill_ids,
        ),
        selected_mcp_servers: workspace_config::mcp_ids_to_refs(&request.selected_mcp_server_ids),
        provider_connection_ids: request.provider_connection_ids,
        execution,
        // The Main always wears the same fixed face; only shared definitions
        // carry one.
        avatar: None,
        created_at: now,
        updated_at: now,
    };
    let ((), config) = state.update_workspace_config(&request.workspace_id, |config| {
        // Creating an agent means setting up this workspace's Main. Teammates
        // are assigned from the library, never created here, so a workspace
        // that already has a Main has nothing left to create.
        if let Some(existing) = config.main_agent.as_ref() {
            return Err(format!(
                "This workspace already has a Main agent ({}). Add agents from the library instead.",
                existing.id
            ));
        }
        config.updated_at = now;
        config.main_agent = Some(agent);
        Ok(())
    })?;

    let saved = config
        .main_agent
        .as_ref()
        .ok_or_else(|| "Workspace agent disappeared between write and read-back".to_string())?;
    Ok(detail_from_agent(&app_config, &config, saved))
}

#[tauri::command]
pub async fn workspace_update_agent(
    request: WorkspaceAgentUpdateRequest,
    state: State<'_, AppState>,
) -> Result<WorkspaceAgentDetail, WorkspaceAgentUpdateError> {
    update_main_agent(state.inner(), request)
}

fn update_main_agent(
    state: &AppState,
    mut request: WorkspaceAgentUpdateRequest,
) -> Result<WorkspaceAgentDetail, WorkspaceAgentUpdateError> {
    request.execution.sandbox.canonicalize()?;
    let app_config = app_config(state)?;
    let now = now_millis();
    let agent_id = request.agent_id.clone();
    let workspace_id = request.workspace_id.clone();
    let mut stale = false;
    let updated = state.update_workspace_config(&workspace_id, |config| {
        if config.assignment(&request.agent_id).is_some() {
            return Err(
                "This agent's behavior is shared. Edit it in the agent library, or change its local context and access in the workspace's Team settings."
                    .to_string(),
            );
        }
        let Some(agent) = config
            .main_agent
            .as_mut()
            .filter(|agent| agent.id == request.agent_id)
        else {
            return Err(format!("Workspace agent not found: {}", request.agent_id));
        };
        if agent.updated_at != request.expected_updated_at {
            stale = true;
            return Err(STALE_AGENT.to_string());
        }

        agent.name = request.name;
        agent.description = request.description;
        agent.selected_skills =
            workspace_config::skill_ids_to_refs(&app_config, &request.selected_skill_ids);
        // Settings edits attachment only; the context-bar `disabled` flag is
        // preserved for servers that stay attached (merge_mcp_selection).
        agent.selected_mcp_servers = workspace_config::merge_mcp_selection(
            &agent.selected_mcp_servers,
            &request.selected_mcp_server_ids,
        );
        agent.provider_connection_ids = request.provider_connection_ids;
        agent.execution = request.execution;
        agent.enabled = request.enabled;
        agent.touch(now);
        config.updated_at = now;
        Ok(())
    });
    let ((), config) = updated.map_err(|message| {
        if stale {
            WorkspaceAgentUpdateError::Stale { message }
        } else {
            WorkspaceAgentUpdateError::Failed { message }
        }
    })?;

    let saved = config
        .main_agent
        .as_ref()
        .filter(|agent| agent.id == agent_id)
        .ok_or_else(|| format!("Workspace agent not found after update: {}", agent_id))?;
    Ok(detail_from_agent(&app_config, &config, saved))
}

#[tauri::command]
pub async fn workspace_delete_agent(
    workspace_id: String,
    agent_id: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    state.update_workspace_config(&workspace_id, |config| {
        if config.main_agent_id() == agent_id {
            return Err(
                "Cannot delete the workspace's Main agent. Every workspace has exactly one."
                    .to_string(),
            );
        }

        // Unassigning drops the local identity and its local overlays. The
        // shared definition — and every other workspace using it — is
        // untouched.
        let before = config.assignments.len();
        config
            .assignments
            .retain(|assignment| assignment.id != agent_id);
        if config.assignments.len() == before {
            return Err(format!(
                "Workspace agent assignment not found: {}",
                agent_id
            ));
        }

        config.updated_at = now_millis();
        Ok(())
    })?;
    Ok(())
}

#[tauri::command]
pub async fn workspace_set_agent_enabled(
    request: WorkspaceAgentEnabledRequest,
    state: State<'_, AppState>,
) -> Result<WorkspaceAgentDetail, String> {
    let now = now_millis();
    let workspace_id = request.workspace_id.clone();
    let agent_id = request.agent_id.clone();
    state.update_workspace_config(&workspace_id, |config| {
        // Enabling is local for both kinds: an assignment carries its own
        // switch precisely so one workspace can park a teammate without
        // disabling it everywhere else.
        if let Some(assignment) = config
            .assignments
            .iter_mut()
            .find(|assignment| assignment.id == agent_id)
        {
            assignment.enabled = request.enabled;
            assignment.updated_at = now;
            config.updated_at = now;
            return Ok(());
        }
        let Some(agent) = config
            .main_agent
            .as_mut()
            .filter(|agent| agent.id == agent_id)
        else {
            return Err(format!("Workspace agent not found: {}", agent_id));
        };
        agent.enabled = request.enabled;
        agent.touch(now);
        config.updated_at = now;
        Ok(())
    })?;

    stored_agent_detail(state.inner(), &workspace_id, &agent_id)?
        .ok_or_else(|| format!("Workspace agent not found after toggle: {}", agent_id))
}

fn app_config(state: &AppState) -> Result<AppConfig, String> {
    Ok(state
        .config_manager
        .lock()
        .map_err(|e| format!("Lock error: {}", e))?
        .get())
}

pub(crate) fn detail_from_agent(
    app_config: &AppConfig,
    workspace: &WorkspaceConfig,
    agent: &WorkspaceAgent,
) -> WorkspaceAgentDetail {
    WorkspaceAgentDetail {
        id: agent.id.clone(),
        workspace_id: workspace.id.clone(),
        name: agent.name.clone(),
        description: agent.description.clone(),
        selected_skill_ids: workspace_config::refs_to_skill_ids(app_config, &agent.selected_skills),
        selected_mcp_server_ids: workspace_config::refs_to_mcp_ids(&agent.selected_mcp_servers),
        provider_connection_ids: agent.provider_connection_ids.clone(),
        execution: agent.execution.clone(),
        enabled: agent.enabled,
        is_default: workspace.main_agent_id() == agent.id,
        created_at: agent.created_at,
        updated_at: agent.updated_at,
    }
}

#[cfg(test)]
mod stored_agent_tests {
    //! The settings form must round-trip what is *stored*. Handing it a
    //! resolved agent is how prompts grow a duplicated copy of the workspace
    //! context on every save.

    use super::*;
    use crate::commands::path_grants::approval_destination_tests::*;

    #[test]
    fn the_form_never_sees_context_that_resolution_adds() {
        let fixture = Fixture::new();
        fixture
            .state
            .update_workspace_config(WORKSPACE_ID, |config| {
                config.context = "House rules".to_string();
                let main = config.main_agent.as_mut().expect("main");
                main.description = "Own instructions".to_string();
                Ok(())
            })
            .expect("seed context");

        let detail = stored_agent_detail(&fixture.state, WORKSPACE_ID, MAIN_ID)
            .expect("lookup")
            .expect("main detail");

        assert_eq!(detail.description, "Own instructions");
        assert!(detail.is_default);

        // The same agent, resolved for a turn, does carry the overlay — the
        // difference between the two paths is the point.
        let resolved = fixture
            .state
            .resolve_workspace_agent(WORKSPACE_ID, MAIN_ID)
            .expect("resolve")
            .expect("resolved main");
        assert!(resolved.agent.description.contains("House rules"));
    }

    #[test]
    fn a_teammates_form_shows_the_shared_behavior_under_its_local_id() {
        let fixture = Fixture::new();

        let detail = stored_agent_detail(&fixture.state, WORKSPACE_ID, ASSIGNMENT_ID)
            .expect("lookup")
            .expect("assignment detail");

        assert_eq!(detail.id, ASSIGNMENT_ID, "addressed by the local identity");
        assert_eq!(detail.name, "Reviewer", "behavior comes from the library");
        assert!(!detail.is_default);
    }

    fn update_request(fixture: &Fixture) -> WorkspaceAgentUpdateRequest {
        let main = fixture.config().main_agent.expect("main");
        WorkspaceAgentUpdateRequest {
            workspace_id: WORKSPACE_ID.to_string(),
            agent_id: main.id.clone(),
            name: main.name.clone(),
            description: "edited".to_string(),
            selected_skill_ids: Vec::new(),
            selected_mcp_server_ids: Vec::new(),
            provider_connection_ids: Vec::new(),
            execution: main.execution.clone(),
            enabled: true,
            expected_updated_at: main.updated_at,
        }
    }

    #[test]
    fn a_save_from_a_form_opened_before_an_always_approval_is_refused() {
        let fixture = Fixture::new();
        let opened = update_request(&fixture);
        assert!(fixture
            .config()
            .main_agent
            .unwrap()
            .execution
            .shell
            .ask_first_command_prefixes
            .contains(&"git push".to_string()));

        // "Always" on an ask-first card, saved while the form is open.
        crate::commands::permissions::persist_always_allow(
            &fixture.state,
            WORKSPACE_ID,
            MAIN_ID,
            &["git push".to_string()],
        )
        .expect("approval saved");

        let error = update_main_agent(&fixture.state, opened).expect_err("stale");
        assert!(
            matches!(&error, WorkspaceAgentUpdateError::Stale { message } if message.contains("Use Discard")),
            "{error:?}"
        );
        let stored = fixture.config().main_agent.unwrap();
        assert_ne!(stored.description, "edited");
        assert!(!stored
            .execution
            .shell
            .ask_first_command_prefixes
            .contains(&"git push".to_string()));

        // Reloaded, the same edit saves and keeps the approval.
        let saved = update_main_agent(&fixture.state, update_request(&fixture)).expect("fresh");
        assert_eq!(saved.description, "edited");
        assert!(!saved
            .execution
            .shell
            .ask_first_command_prefixes
            .contains(&"git push".to_string()));
    }

    #[test]
    fn a_save_from_a_form_opened_before_a_network_always_is_refused() {
        let fixture = Fixture::new();
        let opened = update_request(&fixture);
        let listed = |agent: &crate::config::WorkspaceAgent| {
            agent
                .execution
                .sandbox
                .network_access
                .allow_domains
                .contains(&"mirror.example.org".to_string())
        };

        // “Always for Manager” on a network card, saved while the form is open.
        crate::commands::network_access::persist_network_allow(
            &fixture.state,
            WORKSPACE_ID,
            MAIN_ID,
            "mirror.example.org",
        )
        .expect("approval saved");

        let error = update_main_agent(&fixture.state, opened).expect_err("stale");
        assert!(
            matches!(&error, WorkspaceAgentUpdateError::Stale { .. }),
            "{error:?}"
        );
        let stored = fixture.config().main_agent.unwrap();
        assert_ne!(stored.description, "edited");
        assert!(listed(&stored), "the saved host survives");

        let saved = update_main_agent(&fixture.state, update_request(&fixture)).expect("fresh");
        assert!(saved
            .execution
            .sandbox
            .network_access
            .allow_domains
            .contains(&"mirror.example.org".to_string()));
    }

    #[test]
    fn a_write_in_the_same_millisecond_as_a_save_makes_that_save_stale() {
        let fixture = Fixture::new();
        let saved = update_main_agent(&fixture.state, update_request(&fixture)).expect("save");
        fixture
            .state
            .update_workspace_config(WORKSPACE_ID, |config| {
                let main = config.main_agent.as_mut().expect("main");
                main.description = "written elsewhere".to_string();
                main.touch(saved.updated_at);
                Ok(())
            })
            .expect("other write");

        let mut from_the_form = update_request(&fixture);
        from_the_form.expected_updated_at = saved.updated_at;
        let error = update_main_agent(&fixture.state, from_the_form).expect_err("stale");
        assert!(
            matches!(error, WorkspaceAgentUpdateError::Stale { .. }),
            "{error:?}"
        );
        let stored = fixture.config().main_agent.expect("main");
        assert_eq!(stored.description, "written elsewhere");
    }

    #[test]
    fn creating_the_main_with_an_invalid_host_entry_is_refused_naming_it() {
        let fixture = Fixture::new();
        fixture
            .state
            .update_workspace_config(WORKSPACE_ID, |config| {
                config.main_agent = None;
                Ok(())
            })
            .expect("drop the main");
        let request = |allow: &str| WorkspaceAgentCreateRequest {
            workspace_id: WORKSPACE_ID.to_string(),
            name: "Manager".to_string(),
            description: String::new(),
            selected_skill_ids: Vec::new(),
            selected_mcp_server_ids: Vec::new(),
            provider_connection_ids: Vec::new(),
            execution: {
                let mut execution = ExecutionCapabilityConfig::default();
                execution.sandbox.network_access.mode = crate::config::NetworkMode::Allowlist;
                execution.sandbox.network_access.allow_domains = vec![allow.to_string()];
                execution
            },
            enabled: true,
            id: None,
        };

        let error =
            create_main_agent(&fixture.state, request("git@github.com")).expect_err("refused");
        assert!(error.contains("`git@github.com`"), "{error}");
        assert!(fixture.config().main_agent.is_none());

        create_main_agent(&fixture.state, request("Bücher.example")).expect("valid create");
        let main = fixture.config().main_agent.expect("created");
        assert_eq!(
            main.execution.sandbox.network_access.allow_domains,
            ["xn--bcher-kva.example"]
        );
    }

    #[test]
    fn saving_the_main_with_an_invalid_host_entry_in_allowlist_is_refused_naming_it() {
        use crate::config::NetworkMode;
        let fixture = Fixture::new();
        let invalid = |mode| {
            let mut request = update_request(&fixture);
            let network = &mut request.execution.sandbox.network_access;
            network.mode = mode;
            network.deny_domains.push("*.0.0.5".to_string());
            request
        };
        let error = update_main_agent(&fixture.state, invalid(NetworkMode::Allowlist))
            .expect_err("refused");
        assert!(
            matches!(&error, WorkspaceAgentUpdateError::Failed { message } if message.contains("Denied hosts: `*.0.0.5`")),
            "{error:?}"
        );
        assert_ne!(fixture.config().main_agent.unwrap().description, "edited");

        // Switching to Host network saves, and keeps the entry for the user
        // to fix when they come back to Allowlist.
        update_main_agent(&fixture.state, invalid(NetworkMode::HostNetwork))
            .expect("saved in Host network");
        let main = fixture.config().main_agent.unwrap();
        assert_eq!(main.description, "edited");
        assert!(main
            .execution
            .sandbox
            .network_access
            .deny_domains
            .contains(&"*.0.0.5".to_string()));

        let mut request = update_request(&fixture);
        request.execution.sandbox.network_access.mode = NetworkMode::Allowlist;
        request.execution.sandbox.network_access.deny_domains = vec!["Bücher.example".to_string()];
        update_main_agent(&fixture.state, request).expect("valid save");
        let main = fixture.config().main_agent.unwrap();
        assert_eq!(main.description, "edited");
        assert_eq!(
            main.execution.sandbox.network_access.deny_domains,
            ["xn--bcher-kva.example"]
        );
    }

    #[test]
    fn an_unknown_id_is_absent_rather_than_an_error() {
        let fixture = Fixture::new();
        assert!(stored_agent_detail(&fixture.state, WORKSPACE_ID, "nobody")
            .expect("lookup")
            .is_none());
    }
}
