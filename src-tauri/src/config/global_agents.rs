//! The app-level teammate library, and the single place a workspace roster is
//! built from it.
//!
//! A definition is behavior without an execution identity: it has no workspace
//! directory, no conversation, no schedule and no provider session. It becomes
//! callable only where a workspace assigns it, and the local assignment id is
//! the `workspaceAgentId` tools address. Resolution therefore always starts
//! from a workspace — never from a definition — and produces a snapshot the
//! caller uses for one turn and throws away.

use serde::{Deserialize, Serialize};

use super::{
    AppConfig, FilesystemPathAccess, FilesystemPathGrant, WorkspaceAgent, WorkspaceConfig,
};

/// A teammate definition owned by the application and reusable by every
/// workspace that assigns it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentDefinition {
    pub id: String,
    /// Bumped on every saved edit. A form submitted against an older revision
    /// is rejected instead of silently overwriting the newer settings.
    pub revision: u64,
    /// Archived definitions stay readable and keep their history, but cannot be
    /// assigned or started again.
    #[serde(default)]
    pub archived: bool,
    /// The same behavior payload a workspace's Main owns locally, so prompt and
    /// tool resolution have exactly one implementation.
    pub behavior: WorkspaceAgent,
}

/// Where a resolved agent's behavior came from — the diagnostic half of a
/// resolution, kept out of [`WorkspaceAgent`] so nothing can persist it back
/// into a workspace file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentSource {
    /// The workspace's own Main.
    Main,
    /// An assigned app-level definition, at the revision resolved for this turn.
    Shared {
        definition_id: String,
        revision: u64,
    },
    /// An assignment whose definition is gone. Kept in the roster — rather than
    /// dropped — so the UI can say why a teammate stopped answering instead of
    /// quietly losing it.
    Unavailable { definition_id: String },
}

/// One runnable agent: a local identity plus the behavior in force this turn.
#[derive(Debug, Clone)]
pub struct ResolvedAgent {
    /// Behavior with workspace and assignment overlays applied. `agent.id` is
    /// the local `workspaceAgentId`: the Main's id, or an assignment id.
    pub agent: WorkspaceAgent,
    pub source: AgentSource,
}

impl ResolvedAgent {
    /// The shared definition behind this agent, if any. `None` for the Main.
    pub fn definition_id(&self) -> Option<&str> {
        match &self.source {
            AgentSource::Main => None,
            AgentSource::Shared { definition_id, .. }
            | AgentSource::Unavailable { definition_id } => Some(definition_id),
        }
    }
}

/// Build the roster a workspace executes against: its Main first, then every
/// assigned teammate, each with the workspace's shared context and grants
/// applied.
///
/// Nothing here is persisted. The behavior of a shared teammate lives in the
/// definition, and the file written back for this workspace still contains only
/// its Main and its assignments.
pub fn resolve_roster(workspace: &WorkspaceConfig, app: &AppConfig) -> Vec<ResolvedAgent> {
    let mut roster = Vec::with_capacity(workspace.assignments.len() + 1);
    if let Some(main) = workspace.main_agent.clone() {
        roster.push(ResolvedAgent {
            agent: main,
            source: AgentSource::Main,
        });
    }

    for assignment in &workspace.assignments {
        let definition = app
            .agent_definitions
            .iter()
            .find(|definition| definition.id == assignment.agent_definition_id);
        let Some(definition) = definition else {
            roster.push(ResolvedAgent {
                agent: unavailable_agent(assignment),
                source: AgentSource::Unavailable {
                    definition_id: assignment.agent_definition_id.clone(),
                },
            });
            continue;
        };

        let mut agent = definition.behavior.clone();
        agent.id = assignment.id.clone();
        // Three independent switches, all of which must be on: the definition's
        // own, the assignment's, and the definition not being archived.
        agent.enabled = agent.enabled && assignment.enabled && !definition.archived;
        append_context(&mut agent, "Assignment context", &assignment.context);
        agent.execution.filesystem.extra_paths = merge_grants(
            &agent.execution.filesystem.extra_paths,
            &assignment.filesystem_grants,
        );
        agent.created_at = assignment.created_at;
        agent.updated_at = agent.updated_at.max(assignment.updated_at);
        roster.push(ResolvedAgent {
            agent,
            source: AgentSource::Shared {
                definition_id: definition.id.clone(),
                revision: definition.revision,
            },
        });
    }

    for resolved in &mut roster {
        append_context(&mut resolved.agent, "Workspace context", &workspace.context);
        resolved.agent.execution.filesystem.extra_paths = merge_grants(
            &resolved.agent.execution.filesystem.extra_paths,
            &workspace.filesystem_grants,
        );
    }
    roster
}

/// Placeholder for an assignment whose definition was deleted from the library.
/// Disabled, so no entry point can start it, and named so the roster explains
/// itself.
fn unavailable_agent(assignment: &super::workspace_config::WorkspaceAssignment) -> WorkspaceAgent {
    WorkspaceAgent {
        id: assignment.id.clone(),
        name: "Unavailable agent".to_string(),
        description: "This teammate's shared definition no longer exists. Re-create it in the agent library, or remove the assignment.".to_string(),
        enabled: false,
        selected_skills: Vec::new(),
        selected_mcp_servers: Vec::new(),
        provider_connection_ids: Vec::new(),
        execution: super::ExecutionCapabilityConfig::default(),
        created_at: assignment.created_at,
        updated_at: assignment.updated_at,
    }
}

/// Append an overlay to an agent's instructions under its own heading, so the
/// agent can tell project context from its own definition.
fn append_context(agent: &mut WorkspaceAgent, heading: &str, context: &str) {
    let context = context.trim();
    if context.is_empty() {
        return;
    }
    if !agent.description.is_empty() {
        agent.description.push_str("\n\n");
    }
    agent.description.push_str("## ");
    agent.description.push_str(heading);
    agent.description.push_str("\n\n");
    agent.description.push_str(context);
}

/// Combine two grant lists, keeping one entry per path at the highest access.
///
/// This settles the *same* path appearing twice — a workspace read grant on a
/// path the agent may already write. Overlapping but unequal paths are a
/// different question, answered where the effective list is composed against
/// the workspace mask: see `assistant::sandbox::filesystem::EffectiveFilesystemPolicy::compile`.
fn merge_grants(
    base: &[FilesystemPathGrant],
    extra: &[FilesystemPathGrant],
) -> Vec<FilesystemPathGrant> {
    let mut merged: Vec<FilesystemPathGrant> = Vec::with_capacity(base.len() + extra.len());
    for grant in base.iter().chain(extra.iter()) {
        match merged
            .iter_mut()
            .find(|existing| existing.path == grant.path)
        {
            Some(existing) => {
                if matches!(grant.access, FilesystemPathAccess::ReadWrite) {
                    existing.access = FilesystemPathAccess::ReadWrite;
                    existing.origin = grant.origin.clone();
                }
            }
            None => merged.push(grant.clone()),
        }
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::workspace_config::WorkspaceAssignment;
    use crate::config::{FilesystemPathAccess, ShellAccessMode};

    fn definition(id: &str, name: &str) -> AgentDefinition {
        let mut behavior = WorkspaceAgent::new_manager(id.to_string(), 10);
        behavior.name = name.to_string();
        behavior.description = "shared instructions".to_string();
        behavior.execution.shell.mode = ShellAccessMode::Full;
        behavior.execution.filesystem.extra_paths = vec![FilesystemPathGrant {
            path: "/opt/shared".to_string(),
            access: FilesystemPathAccess::ReadOnly,
            origin: None,
        }];
        AgentDefinition {
            id: id.to_string(),
            revision: 3,
            archived: false,
            behavior,
        }
    }

    fn assignment(id: &str, definition_id: &str) -> WorkspaceAssignment {
        WorkspaceAssignment {
            id: id.to_string(),
            agent_definition_id: definition_id.to_string(),
            enabled: true,
            context: String::new(),
            filesystem_grants: Vec::new(),
            created_at: 20,
            updated_at: 21,
        }
    }

    fn workspace() -> WorkspaceConfig {
        WorkspaceConfig::new("ws".to_string(), "W".to_string(), 1, "main-1".to_string())
    }

    #[test]
    fn a_teammate_answers_to_its_local_id_not_the_shared_one() {
        let mut app = AppConfig::default();
        app.agent_definitions.push(definition("def-1", "Reviewer"));
        let mut workspace = workspace();
        workspace.assignments.push(assignment("assign-1", "def-1"));

        let roster = resolve_roster(&workspace, &app);

        assert_eq!(roster.len(), 2);
        assert_eq!(roster[0].agent.id, "main-1");
        assert_eq!(roster[0].source, AgentSource::Main);
        assert_eq!(
            roster[1].agent.id, "assign-1",
            "tools address the assignment, not the definition"
        );
        assert_eq!(roster[1].agent.name, "Reviewer");
        assert_eq!(roster[1].agent.execution.shell.mode, ShellAccessMode::Full);
        assert_eq!(
            roster[1].source,
            AgentSource::Shared {
                definition_id: "def-1".to_string(),
                revision: 3,
            }
        );
    }

    #[test]
    fn resolving_never_edits_the_library_or_the_workspace() {
        let mut app = AppConfig::default();
        app.agent_definitions.push(definition("def-1", "Reviewer"));
        let mut workspace = workspace();
        workspace.context = "house rules".to_string();
        workspace.assignments.push(assignment("assign-1", "def-1"));

        let first = resolve_roster(&workspace, &app);
        let second = resolve_roster(&workspace, &app);

        assert_eq!(
            app.agent_definitions[0].behavior.description, "shared instructions",
            "overlays are applied to the snapshot, not written back"
        );
        assert_eq!(
            first[1].agent.description, second[1].agent.description,
            "resolution is idempotent; context cannot accumulate"
        );
    }

    #[test]
    fn context_overlays_are_layered_under_their_own_headings() {
        let mut app = AppConfig::default();
        app.agent_definitions.push(definition("def-1", "Reviewer"));
        let mut workspace = workspace();
        workspace.context = "Ship on Fridays".to_string();
        let mut assigned = assignment("assign-1", "def-1");
        assigned.context = "Only review the API crate".to_string();
        workspace.assignments.push(assigned);

        let roster = resolve_roster(&workspace, &app);
        let teammate = &roster[1].agent.description;

        assert!(teammate.starts_with("shared instructions"));
        assert!(teammate.contains("## Assignment context\n\nOnly review the API crate"));
        assert!(teammate.contains("## Workspace context\n\nShip on Fridays"));
        assert!(
            roster[0].agent.description.contains("## Workspace context"),
            "the Main works under the same project context"
        );
    }

    #[test]
    fn workspace_grants_add_access_and_never_narrow_it() {
        let mut app = AppConfig::default();
        let mut shared = definition("def-1", "Reviewer");
        shared.behavior.execution.filesystem.extra_paths = vec![FilesystemPathGrant {
            path: "/srv/data".to_string(),
            access: FilesystemPathAccess::ReadWrite,
            origin: None,
        }];
        app.agent_definitions.push(shared);

        let mut workspace = workspace();
        // The same path, granted read-only at the workspace level: additive
        // composition must keep the write access the teammate already had.
        workspace.filesystem_grants = vec![
            FilesystemPathGrant {
                path: "/srv/data".to_string(),
                access: FilesystemPathAccess::ReadOnly,
                origin: None,
            },
            FilesystemPathGrant {
                path: "/opt/tools".to_string(),
                access: FilesystemPathAccess::ReadOnly,
                origin: None,
            },
        ];
        let mut assigned = assignment("assign-1", "def-1");
        assigned.filesystem_grants = vec![FilesystemPathGrant {
            path: "/home/me/project".to_string(),
            access: FilesystemPathAccess::ReadWrite,
            origin: None,
        }];
        workspace.assignments.push(assigned);

        let roster = resolve_roster(&workspace, &app);
        let grants = &roster[1].agent.execution.filesystem.extra_paths;

        let data = grants.iter().find(|g| g.path == "/srv/data").expect("path");
        assert_eq!(data.access, FilesystemPathAccess::ReadWrite);
        assert_eq!(
            grants.iter().filter(|g| g.path == "/srv/data").count(),
            1,
            "one entry per path: a duplicate leaves the outcome to the sandbox backend"
        );
        assert!(grants.iter().any(|g| g.path == "/home/me/project"));
        assert!(grants.iter().any(|g| g.path == "/opt/tools"));
    }

    #[test]
    fn every_switch_can_disable_a_teammate_on_its_own() {
        let mut app = AppConfig::default();
        app.agent_definitions.push(definition("def-1", "Reviewer"));
        app.agent_definitions.push(AgentDefinition {
            archived: true,
            ..definition("def-2", "Archived")
        });
        let mut disabled_behavior = definition("def-3", "Off");
        disabled_behavior.behavior.enabled = false;
        app.agent_definitions.push(disabled_behavior);

        let mut workspace = workspace();
        workspace.assignments.push(WorkspaceAssignment {
            enabled: false,
            ..assignment("assign-1", "def-1")
        });
        workspace.assignments.push(assignment("assign-2", "def-2"));
        workspace.assignments.push(assignment("assign-3", "def-3"));

        let roster = resolve_roster(&workspace, &app);
        assert!(!roster[1].agent.enabled, "parked in this workspace");
        assert!(!roster[2].agent.enabled, "archived definition");
        assert!(!roster[3].agent.enabled, "disabled definition");
    }

    #[test]
    fn an_assignment_without_its_definition_stays_visible_but_cannot_run() {
        let app = AppConfig::default();
        let mut workspace = workspace();
        workspace.assignments.push(assignment("assign-1", "gone"));

        let roster = resolve_roster(&workspace, &app);

        assert_eq!(roster.len(), 2);
        assert_eq!(roster[1].agent.id, "assign-1");
        assert!(!roster[1].agent.enabled);
        assert_eq!(
            roster[1].source,
            AgentSource::Unavailable {
                definition_id: "gone".to_string()
            }
        );
    }

    #[test]
    fn a_workspace_without_a_main_still_resolves_its_team() {
        let mut app = AppConfig::default();
        app.agent_definitions.push(definition("def-1", "Reviewer"));
        let mut workspace = workspace();
        workspace.main_agent = None;
        workspace.assignments.push(assignment("assign-1", "def-1"));

        let roster = resolve_roster(&workspace, &app);
        assert_eq!(roster.len(), 1);
        assert_eq!(roster[0].agent.id, "assign-1");
    }
}
