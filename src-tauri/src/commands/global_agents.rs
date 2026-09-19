//! Commands for the app-level agent library and for assigning its definitions
//! to workspaces.
//!
//! The split this file enforces: a definition is *behavior* the whole app
//! shares, an assignment is the *identity* one workspace calls. Editing
//! behavior therefore goes through the library and reaches every workspace on
//! the next turn; editing what is local to a project — whether a teammate is
//! on, the context it works under, the paths it may touch here — goes through
//! the assignment and reaches nobody else.

use serde::{Deserialize, Serialize};
use tauri::State;

use crate::config::global_agents::AgentDefinition;
use crate::config::workspace_config::{self, AgentAvatarRef, WorkspaceAssignment};
use crate::config::{
    ExecutionCapabilityConfig, FilesystemPathGrant, WorkspaceAgent, WorkspaceConfig,
};
use crate::AppState;

fn now_millis() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// A workspace that has this definition assigned. Shown before a shared edit or
/// an archive so the blast radius is visible rather than implied.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AssignedWorkspace {
    pub workspace_id: String,
    pub title: String,
    pub workspace_agent_id: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentDefinitionDetail {
    pub id: String,
    pub revision: u64,
    pub archived: bool,
    pub name: String,
    pub description: String,
    pub selected_skill_ids: Vec<String>,
    pub selected_mcp_server_ids: Vec<String>,
    pub provider_connection_ids: Vec<String>,
    pub execution: ExecutionCapabilityConfig,
    pub enabled: bool,
    pub avatar: Option<AgentAvatarRef>,
    pub created_at: i64,
    pub updated_at: i64,
    pub assigned_workspaces: Vec<AssignedWorkspace>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentDefinitionSaveRequest {
    /// Absent when creating.
    #[serde(default)]
    pub id: Option<String>,
    /// The revision the form was loaded from. Must match the stored revision,
    /// so a save built on stale settings is rejected instead of silently
    /// reverting somebody else's edit. Absent when creating.
    #[serde(default)]
    pub expected_revision: Option<u64>,
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
    #[serde(default)]
    pub archived: bool,
    /// Absent means "keep the stored face": the behaviour form does not carry
    /// it, only the face picker does.
    #[serde(default)]
    pub avatar: Option<AgentAvatarRef>,
}

fn default_true() -> bool {
    true
}

/// The workspace-local half of team settings: shared project context, the
/// grants every agent here receives, and the assignments themselves.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceTeamPolicy {
    pub context: String,
    pub filesystem_grants: Vec<FilesystemPathGrant>,
    pub assignments: Vec<WorkspaceAssignment>,
}

#[tauri::command]
pub async fn agent_definitions_list(
    state: State<'_, AppState>,
) -> Result<Vec<AgentDefinitionDetail>, String> {
    let app = state
        .config_manager
        .lock()
        .map_err(|e| format!("Lock error: {}", e))?
        .get();
    let locators = state
        .workspace_index
        .read()
        .map_err(|e| format!("Workspace index lock error: {}", e))?
        .locators_sorted();
    let workspaces: Vec<_> = locators
        .iter()
        .filter_map(|locator| workspace_config::load(&locator.root_path).ok())
        .collect();

    Ok(app
        .agent_definitions
        .iter()
        .map(|definition| {
            let assigned_workspaces = workspaces
                .iter()
                .filter_map(|workspace| {
                    let assignment = workspace
                        .assignments
                        .iter()
                        .find(|assignment| assignment.agent_definition_id == definition.id)?;
                    Some(AssignedWorkspace {
                        workspace_id: workspace.id.clone(),
                        title: workspace.title.clone(),
                        workspace_agent_id: assignment.id.clone(),
                        enabled: assignment.enabled,
                    })
                })
                .collect();
            detail_from_definition(&app, definition, assigned_workspaces)
        })
        .collect())
}

#[tauri::command]
pub async fn agent_definition_save(
    request: AgentDefinitionSaveRequest,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let now = now_millis();
    let manager = state
        .config_manager
        .lock()
        .map_err(|e| format!("Lock error: {}", e))?;
    let mut outcome = Err("Save did not run".to_string());
    manager
        .update(|config| outcome = upsert_definition(config, &request, now))
        .map_err(|e| e.to_string())?;
    outcome
}

/// Create or update one definition in the catalog.
///
/// Runs inside the app config's write lock, which is what makes the revision
/// check meaningful: two settings windows editing the same agent are serialized
/// here, and the second one is told to reload instead of quietly reverting the
/// first.
fn upsert_definition(
    config: &mut crate::config::AppConfig,
    request: &AgentDefinitionSaveRequest,
    now: i64,
) -> Result<String, String> {
    let name = request.name.trim().to_string();
    if name.is_empty() {
        return Err("Agent name is required".to_string());
    }
    let id = request
        .id
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let previous = config
        .agent_definitions
        .iter()
        .find(|definition| definition.id == id);
    if request.id.is_some() && previous.is_none() {
        return Err("This shared agent no longer exists.".to_string());
    }
    if previous.map(|definition| definition.revision) != request.expected_revision {
        return Err("This agent changed somewhere else. Reload it before saving.".to_string());
    }

    if matches!(&request.avatar, Some(avatar) if avatar.seed.trim().is_empty()) {
        return Err("The agent's face needs a seed.".to_string());
    }
    // Most saves come from the behaviour form, which does not carry the face:
    // absent means "keep what is stored". A seed is stored trimmed, the same
    // way it was validated, so the face drawn later hashes the same text.
    let avatar = request
        .avatar
        .as_ref()
        .map(|avatar| AgentAvatarRef {
            seed: avatar.seed.trim().to_string(),
            generator_version: avatar.generator_version,
        })
        .or_else(|| previous.and_then(|definition| definition.behavior.avatar.clone()));

    let definition = AgentDefinition {
        id: id.clone(),
        revision: previous.map_or(1, |definition| definition.revision + 1),
        archived: request.archived,
        behavior: WorkspaceAgent {
            // The behavior carries the definition id so a resolved agent always
            // has an id even before an assignment renames it to the local one.
            id: id.clone(),
            name,
            description: request.description.clone(),
            enabled: request.enabled,
            selected_skills: workspace_config::skill_ids_to_refs(
                config,
                &request.selected_skill_ids,
            ),
            selected_mcp_servers: workspace_config::mcp_ids_to_refs(
                &request.selected_mcp_server_ids,
            ),
            provider_connection_ids: request.provider_connection_ids.clone(),
            execution: request.execution.clone(),
            avatar,
            created_at: previous.map_or(now, |definition| definition.behavior.created_at),
            updated_at: now,
        },
    };
    config
        .agent_definitions
        .retain(|existing| existing.id != definition.id);
    config.agent_definitions.push(definition);
    Ok(id)
}

/// Assign a shared definition to a workspace, creating the local identity that
/// tools address.
#[tauri::command]
pub async fn workspace_assign_agent(
    workspace_id: String,
    definition_id: String,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let definitions = state
        .config_manager
        .lock()
        .map_err(|e| format!("Lock error: {}", e))?
        .get()
        .agent_definitions;
    if !is_assignable(&definitions, &definition_id) {
        return Err("This shared agent is not available to assign.".to_string());
    }

    let id = uuid::Uuid::new_v4().to_string();
    let now = now_millis();
    state.update_workspace_config(&workspace_id, |config| {
        add_assignment(config, &id, &definition_id, now)
    })?;
    Ok(id)
}

/// Whether a definition may join a team: it has to exist, and an archived one
/// is kept for history rather than offered for new work.
fn is_assignable(definitions: &[AgentDefinition], definition_id: &str) -> bool {
    definitions
        .iter()
        .any(|definition| definition.id == definition_id && !definition.archived)
}

/// Put a definition on a workspace's team.
///
/// One assignment per definition per workspace: a second local identity for the
/// same teammate would give the roster two rows that behave identically, with
/// nothing to choose between them.
fn add_assignment(
    config: &mut WorkspaceConfig,
    id: &str,
    definition_id: &str,
    now: i64,
) -> Result<(), String> {
    if config
        .assignments
        .iter()
        .any(|assignment| assignment.agent_definition_id == definition_id)
    {
        return Err("This agent is already on the workspace team.".to_string());
    }
    config.assignments.push(WorkspaceAssignment {
        id: id.to_string(),
        agent_definition_id: definition_id.to_string(),
        enabled: true,
        context: String::new(),
        filesystem_grants: Vec::new(),
        created_at: now,
        updated_at: now,
    });
    config.updated_at = now;
    Ok(())
}

/// Edit the local overlays of one assignment. Never touches shared behavior.
#[tauri::command]
pub async fn workspace_configure_assignment(
    workspace_id: String,
    assignment: WorkspaceAssignment,
    state: State<'_, AppState>,
) -> Result<(), String> {
    state.update_workspace_config(&workspace_id, |config| {
        apply_assignment_edit(config, &assignment, now_millis())
    })?;
    Ok(())
}

/// Write the local overlays of an existing assignment. Identity is fixed: an
/// assignment is the local name of one shared agent, and repointing it would
/// silently change who answers to an id that tasks and history already use.
fn apply_assignment_edit(
    config: &mut WorkspaceConfig,
    edit: &WorkspaceAssignment,
    now: i64,
) -> Result<(), String> {
    let Some(current) = config
        .assignments
        .iter_mut()
        .find(|current| current.id == edit.id)
    else {
        return Err(format!("Workspace agent assignment not found: {}", edit.id));
    };
    // The form replaces the whole overlay, so a save built on a stale copy
    // would silently drop anything written since — most likely a path grant the
    // agent asked for while the pane sat open.
    if current.updated_at > edit.updated_at {
        return Err(
            "This agent's workspace settings changed while you were editing. Reload before saving."
                .to_string(),
        );
    }
    if current.agent_definition_id != edit.agent_definition_id {
        return Err(
            "An assignment cannot be pointed at a different shared agent. Remove it and add the other one."
                .to_string(),
        );
    }
    current.enabled = edit.enabled;
    current.context = edit.context.clone();
    current.filesystem_grants = edit.filesystem_grants.clone();
    current.updated_at = now;
    config.updated_at = now;
    Ok(())
}

#[tauri::command]
pub async fn workspace_team_policy(
    workspace_id: String,
    state: State<'_, AppState>,
) -> Result<WorkspaceTeamPolicy, String> {
    let root = state
        .workspace_root(&workspace_id)
        .ok_or_else(|| format!("Workspace not found: {}", workspace_id))?;
    let config = workspace_config::load(&root).map_err(|e| e.to_string())?;
    Ok(WorkspaceTeamPolicy {
        context: config.context,
        filesystem_grants: config.filesystem_grants,
        assignments: config.assignments,
    })
}

#[tauri::command]
pub async fn workspace_save_team_policy(
    workspace_id: String,
    context: String,
    filesystem_grants: Vec<FilesystemPathGrant>,
    state: State<'_, AppState>,
) -> Result<(), String> {
    state.update_workspace_config(&workspace_id, |config| {
        config.context = context.clone();
        config.filesystem_grants = filesystem_grants.clone();
        config.updated_at = now_millis();
        Ok(())
    })?;
    Ok(())
}

/// Mutate one definition inside the app config's own lock.
///
/// The read-modify-write must happen under that lock: taking a snapshot,
/// editing it and writing the whole config back would drop any unrelated change
/// saved in between — exactly the lost-update bug the workspace config path
/// already serializes against.
pub(crate) fn update_definition(
    state: &AppState,
    definition_id: &str,
    mutate: impl FnOnce(&mut AgentDefinition),
) -> Result<(), String> {
    let manager = state
        .config_manager
        .lock()
        .map_err(|e| format!("Lock error: {}", e))?;
    let mut outcome = Ok(());
    manager
        .update(|config| {
            let Some(definition) = config
                .agent_definitions
                .iter_mut()
                .find(|definition| definition.id == definition_id)
            else {
                outcome = Err(format!("Shared agent not found: {}", definition_id));
                return;
            };
            mutate(definition);
        })
        .map_err(|e| e.to_string())?;
    outcome
}

fn detail_from_definition(
    app_config: &crate::config::AppConfig,
    definition: &AgentDefinition,
    assigned_workspaces: Vec<AssignedWorkspace>,
) -> AgentDefinitionDetail {
    let behavior = &definition.behavior;
    AgentDefinitionDetail {
        id: definition.id.clone(),
        revision: definition.revision,
        archived: definition.archived,
        name: behavior.name.clone(),
        description: behavior.description.clone(),
        selected_skill_ids: workspace_config::refs_to_skill_ids(
            app_config,
            &behavior.selected_skills,
        ),
        selected_mcp_server_ids: workspace_config::refs_to_mcp_ids(&behavior.selected_mcp_servers),
        provider_connection_ids: behavior.provider_connection_ids.clone(),
        execution: behavior.execution.clone(),
        enabled: behavior.enabled,
        avatar: behavior.avatar.clone(),
        created_at: behavior.created_at,
        updated_at: behavior.updated_at,
        assigned_workspaces,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AppConfig;

    fn save_request(name: &str) -> AgentDefinitionSaveRequest {
        AgentDefinitionSaveRequest {
            id: None,
            expected_revision: None,
            name: name.to_string(),
            description: "instructions".to_string(),
            selected_skill_ids: Vec::new(),
            selected_mcp_server_ids: Vec::new(),
            provider_connection_ids: Vec::new(),
            execution: ExecutionCapabilityConfig::default(),
            enabled: true,
            archived: false,
            avatar: None,
        }
    }

    fn face(seed: &str) -> AgentAvatarRef {
        AgentAvatarRef {
            seed: seed.to_string(),
            generator_version: 1,
        }
    }

    #[test]
    fn the_picked_face_is_stored_and_survives_edits() {
        let mut config = AppConfig::default();
        let request = AgentDefinitionSaveRequest {
            avatar: Some(face("nonce-3")),
            ..save_request("Reviewer")
        };
        let id = upsert_definition(&mut config, &request, 100).expect("create");
        assert_eq!(
            config.agent_definitions[0].behavior.avatar,
            Some(face("nonce-3"))
        );

        let detail = detail_from_definition(&config, &config.agent_definitions[0], Vec::new());
        assert_eq!(detail.avatar, Some(face("nonce-3")));

        // The behaviour form does not know about faces: an edit that omits the
        // face keeps the stored one.
        let edit = AgentDefinitionSaveRequest {
            id: Some(id.clone()),
            expected_revision: Some(1),
            avatar: None,
            ..save_request("Reviewer, sharpened")
        };
        upsert_definition(&mut config, &edit, 200).expect("edit");
        let behavior = &config.agent_definitions[0].behavior;
        assert_eq!(behavior.name, "Reviewer, sharpened");
        assert_eq!(behavior.avatar, Some(face("nonce-3")));

        // A new pick replaces it.
        let repick = AgentDefinitionSaveRequest {
            id: Some(id.clone()),
            expected_revision: Some(2),
            avatar: Some(face("nonce-9")),
            ..save_request("Reviewer")
        };
        upsert_definition(&mut config, &repick, 300).expect("repick");
        assert_eq!(
            config.agent_definitions[0].behavior.avatar,
            Some(face("nonce-9"))
        );
    }

    #[test]
    fn a_padded_seed_is_stored_trimmed() {
        let mut config = AppConfig::default();
        let request = AgentDefinitionSaveRequest {
            avatar: Some(face("  nonce-5 ")),
            ..save_request("Reviewer")
        };
        upsert_definition(&mut config, &request, 100).expect("padded seed");
        assert_eq!(
            config.agent_definitions[0].behavior.avatar,
            Some(face("nonce-5"))
        );
    }

    #[test]
    fn a_face_with_a_blank_seed_is_refused() {
        let mut config = AppConfig::default();
        let request = AgentDefinitionSaveRequest {
            avatar: Some(face("   ")),
            ..save_request("Reviewer")
        };
        let error = upsert_definition(&mut config, &request, 100).expect_err("blank seed");
        assert!(error.contains("seed"), "{error}");
        assert!(config.agent_definitions.is_empty());
    }

    #[test]
    fn saving_an_edit_built_on_a_stale_revision_is_refused() {
        let mut config = AppConfig::default();
        let id = upsert_definition(&mut config, &save_request("Reviewer"), 100).expect("create");
        assert_eq!(config.agent_definitions[0].revision, 1);

        let edit = AgentDefinitionSaveRequest {
            id: Some(id.clone()),
            expected_revision: Some(1),
            description: "sharpened".to_string(),
            ..save_request("Reviewer")
        };
        upsert_definition(&mut config, &edit, 200).expect("first edit wins");
        assert_eq!(config.agent_definitions[0].revision, 2);
        assert_eq!(
            config.agent_definitions[0].behavior.description,
            "sharpened"
        );
        assert_eq!(
            config.agent_definitions[0].behavior.created_at, 100,
            "an edit keeps the definition's creation time"
        );

        let stale = AgentDefinitionSaveRequest {
            id: Some(id),
            expected_revision: Some(1),
            description: "written against yesterday's form".to_string(),
            ..save_request("Reviewer")
        };
        let error = upsert_definition(&mut config, &stale, 300).expect_err("stale save refused");
        assert!(error.contains("Reload"), "{error}");
        assert_eq!(
            config.agent_definitions[0].behavior.description,
            "sharpened"
        );
    }

    #[test]
    fn creating_requires_a_name_and_updating_requires_an_existing_agent() {
        let mut config = AppConfig::default();
        assert!(upsert_definition(&mut config, &save_request("   "), 1).is_err());

        let ghost = AgentDefinitionSaveRequest {
            id: Some("gone".to_string()),
            expected_revision: Some(1),
            ..save_request("Reviewer")
        };
        assert!(upsert_definition(&mut config, &ghost, 1).is_err());
        assert!(config.agent_definitions.is_empty());
    }

    #[test]
    fn an_archived_or_unknown_definition_cannot_join_a_team() {
        let mut config = AppConfig::default();
        let id = upsert_definition(&mut config, &save_request("Reviewer"), 1).expect("create");
        assert!(is_assignable(&config.agent_definitions, &id));
        assert!(!is_assignable(&config.agent_definitions, "nobody"));

        let archived = AgentDefinitionSaveRequest {
            id: Some(id.clone()),
            expected_revision: Some(1),
            archived: true,
            ..save_request("Reviewer")
        };
        upsert_definition(&mut config, &archived, 2).expect("archive");
        assert!(
            !is_assignable(&config.agent_definitions, &id),
            "an archived agent stays readable for history but takes no new work"
        );
    }

    #[test]
    fn a_definition_is_assigned_to_a_workspace_at_most_once() {
        let mut workspace =
            WorkspaceConfig::new("ws".to_string(), "W".to_string(), 1, "main".to_string());
        add_assignment(&mut workspace, "assign-1", "def-1", 10).expect("first assignment");

        let error = add_assignment(&mut workspace, "assign-2", "def-1", 20)
            .expect_err("a second identity for the same teammate is refused");
        assert!(error.contains("already"), "{error}");
        assert_eq!(workspace.assignments.len(), 1);
    }

    #[test]
    fn an_assignment_keeps_its_identity_while_its_overlays_change() {
        let mut workspace =
            WorkspaceConfig::new("ws".to_string(), "W".to_string(), 1, "main".to_string());
        add_assignment(&mut workspace, "assign-1", "def-1", 10).expect("assignment");

        let mut edit = workspace.assignments[0].clone();
        edit.enabled = false;
        edit.context = "API crate only".to_string();
        edit.filesystem_grants = vec![FilesystemPathGrant {
            path: "/srv/data".to_string(),
            access: crate::config::FilesystemPathAccess::ReadWrite,
            origin: None,
        }];
        apply_assignment_edit(&mut workspace, &edit, 50).expect("edit");

        let saved = &workspace.assignments[0];
        assert!(!saved.enabled);
        assert_eq!(saved.context, "API crate only");
        assert_eq!(saved.filesystem_grants.len(), 1);
        assert_eq!(saved.updated_at, 50);

        let stale = WorkspaceAssignment {
            context: "written against an older copy".to_string(),
            updated_at: 10,
            ..workspace.assignments[0].clone()
        };
        let error = apply_assignment_edit(&mut workspace, &stale, 70)
            .expect_err("a save built on a stale copy is refused, not merged blindly");
        assert!(error.contains("Reload"), "{error}");
        assert_eq!(workspace.assignments[0].context, "API crate only");

        let repointed = WorkspaceAssignment {
            agent_definition_id: "def-2".to_string(),
            ..workspace.assignments[0].clone()
        };
        let error = apply_assignment_edit(&mut workspace, &repointed, 60)
            .expect_err("an id already used by tasks cannot change who answers it");
        assert!(error.contains("Remove it"), "{error}");
        assert_eq!(workspace.assignments[0].agent_definition_id, "def-1");
    }

    #[test]
    fn editing_an_assignment_that_is_gone_reports_it() {
        let mut workspace =
            WorkspaceConfig::new("ws".to_string(), "W".to_string(), 1, "main".to_string());
        let edit = WorkspaceAssignment {
            id: "assign-1".to_string(),
            agent_definition_id: "def-1".to_string(),
            enabled: true,
            context: String::new(),
            filesystem_grants: Vec::new(),
            created_at: 1,
            updated_at: 1,
        };
        assert!(apply_assignment_edit(&mut workspace, &edit, 2).is_err());
    }
}
