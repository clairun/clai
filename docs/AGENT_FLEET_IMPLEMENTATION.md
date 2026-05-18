# Agent Fleet Implementation Plan

> Status: Draft
> Created: 2026-05-16

## Summary

CLAI should evolve from "many scheduled agents" into a local control plane for
agent fleets.

The main product model is:

- global catalogs define reusable assets
- workspaces compose those assets into runtime teams
- the default workspace agent receives user and scheduled input
- workspace agents can collaborate inside the same workspace
- missing tools, missing context, and permission constraints are runtime
  outcomes, not preflight assignment blockers

This document supersedes the inter-workspace and global inter-agent tool-call
direction from `docs/MULTI_AGENT_COMMUNICATION_RFC.md`. Cross-workspace agent
communication should be removed from the target model. A workspace is the
collaboration and context boundary.

## Principles

### Agents Are Portable

An agent definition should be agnostic to the workspace where it runs.

For example, "Code Reviewer" should describe how to review code and report
findings, not which repository, MCP servers, memories, shell tools, or files it
will use. Those are supplied by the workspace and by the concrete task assigned
by the manager.

### Workspaces Are Runtime Teams

A workspace owns the project context:

- memory and files
- artifacts
- MCP servers
- schedules
- task history
- assigned agents
- the default manager agent

Agents assigned to the same workspace can communicate through workspace-local
runtime primitives. Agents in different workspaces do not communicate directly.

### Runtime Discovery Beats Setup Gates

The app should let users assign any agent to any workspace.

If a task requires `git` and the workspace environment does not have `git`,
that is a runtime blocker. The agent reports the blocker to the manager, and the
manager surfaces it to the user through workspace activity and app
notifications.

The assignment flow should not reject an agent because a skill might need a
tool that is not currently available.

### Permissions Stay Minimal

The app should still protect the user at execution boundaries such as shell
commands, filesystem writes outside expected roots, network access, secrets,
and external publishing.

Agent definitions should not need to list required permissions before they can
be assigned to a workspace. Permission and capability failures should be
reported by the runtime.

## Target Settings Model

### Agents

The Agents settings page becomes the global agent catalog.

An agent definition contains:

- id
- name
- short description
- system prompt / instructions
- default provider/model selection
- selected skills
- optional tags or category
- created/updated timestamps

It should not contain workspace-specific MCP servers, workspace memory, schedule
state, or task history.

### Skills

The Skills settings page becomes the catalog of available skills.

The first implementation should support:

- local app-defined skills
- skills loaded from one or more local or remote repositories
- existing skill repository standards where practical, including clients such
  as Claude Code
- pinned source metadata so the app knows where a skill came from

The app can normalize skill metadata internally, but users should not have to
rewrite skills into a CLAI-only format.

### Providers And Models

Provider/model configuration mostly exists today through assistant provider
connections.

Agent definitions should reference provider/model defaults from this catalog.
Workspaces may later override the manager provider or use fallback connections,
but provider availability should remain runtime-bound.

### Workspaces

Workspace definitions contain:

- id
- kind
- title
- root path
- selected MCP servers
- schedule configuration
- assigned agents
- default manager assignment
- workspace/user manager prompt override
- created/updated timestamps

Creating a workspace always results in one default manager agent assignment.
Messages sent by the user through the command line go to that manager.

## Domain Model

### AgentDefinition

Reusable global catalog entry.

Suggested shape:

```json
{
  "id": "agent-definition-id",
  "name": "Code Reviewer",
  "description": "Reviews code changes and reports blocking findings first.",
  "systemPrompt": "You are a code reviewer...",
  "providerConnectionIds": ["primary-provider", "fallback-provider"],
  "skillIds": ["code-review", "git-inspection"],
  "tags": ["engineering", "review"],
  "createdAt": "2026-05-16T00:00:00Z",
  "updatedAt": "2026-05-16T00:00:00Z"
}
```

Current code note: `src-tauri/src/config/types.rs` has `AgentConfig`, which is
currently closer to a scheduled automation than a portable agent definition.
The runtime also has `src-tauri/src/agents/types.rs::AgentDefinition`, which is
a scheduler-facing compiled type. During migration, keep naming clear so the
catalog entity and scheduler entity are not confused.

### SkillDefinition

Normalized representation of one available skill.

Suggested shape:

```json
{
  "id": "skill-id",
  "name": "Code Review",
  "description": "Review diffs and produce findings.",
  "sourceId": "skills-repo-id",
  "sourcePath": "skills/code-review/SKILL.md",
  "version": "pinned-ref-or-local-version",
  "metadata": {}
}
```

### SkillSource

Configured local or remote skill repository.

Suggested shape:

```json
{
  "id": "source-id",
  "name": "Company Skills",
  "kind": "local | git",
  "uri": "https://github.com/example/skills.git",
  "ref": "main",
  "enabled": true,
  "createdAt": "2026-05-16T00:00:00Z",
  "updatedAt": "2026-05-16T00:00:00Z"
}
```

### WorkspaceDefinition

Durable workspace metadata and context configuration.

Suggested shape:

```json
{
  "id": "workspace-id",
  "kind": "general | project | scheduled",
  "title": "CLAI Evolution",
  "rootPath": "~/.local/share/clai/agent-workspaces/workspace-id",
  "selectedMcpServerIds": ["filesystem", "github"],
  "managerPrompt": "Project-specific manager guidance...",
  "defaultWorkspaceAgentId": "workspace-agent-id",
  "schedule": {
    "enabled": false,
    "intervalMinutes": 60
  },
  "createdAt": "2026-05-16T00:00:00Z",
  "updatedAt": "2026-05-16T00:00:00Z"
}
```

### WorkspaceAgent

Concrete assignment of a reusable agent definition to one workspace.

Suggested shape:

```json
{
  "id": "workspace-agent-id",
  "workspaceId": "workspace-id",
  "agentDefinitionId": "agent-definition-id",
  "displayName": "Code Reviewer",
  "role": "manager | member",
  "enabled": true,
  "createdAt": "2026-05-16T00:00:00Z",
  "updatedAt": "2026-05-16T00:00:00Z"
}
```

The same `AgentDefinition` can be assigned to many workspaces. Each assignment
has workspace-local runtime state, sessions, memory references, and task
history.

### Task

Task assignment is the manager-to-agent collaboration primitive.

Suggested shape:

```json
{
  "id": "task-id",
  "workspaceId": "workspace-id",
  "createdByWorkspaceAgentId": "manager-workspace-agent-id",
  "assignedToWorkspaceAgentId": "reviewer-workspace-agent-id",
  "status": "queued | running | completed | failed | blocked | needs_user_input",
  "title": "Review auth changes",
  "instructions": "Review the current diff for auth regressions.",
  "resultSummary": null,
  "createdAt": "2026-05-16T00:00:00Z",
  "updatedAt": "2026-05-16T00:00:00Z"
}
```

Task results should support structured blockers:

```json
{
  "status": "blocked",
  "reason": "git is not available in this workspace environment",
  "requestedAction": "Install git or provide a diff artifact."
}
```

## Manager Prompt Composition

The manager prompt should be layered:

1. App-owned manager system prompt
2. User/workspace manager prompt override
3. Dynamic workspace context
4. Assigned-agent summaries
5. Current task or user message

The dynamic assigned-agent section should use summaries, not full worker system
prompts.

Example:

```text
Available workspace agents:
- Code Reviewer: reviews source changes and returns blocking findings first.
- Developer: implements scoped code changes in the workspace repository.
- Tester: runs focused verification and reports failures with reproduction steps.
```

Later, this can become a tool/API instead of prompt injection:

- `list_workspace_agents`
- `get_workspace_agent_profile`
- `assign_task_to_workspace_agent`
- `request_user_input`

## Migration From Current State

### Current State

Relevant current pieces:

- `src-tauri/src/config/types.rs::AgentConfig` stores scheduled automations,
  MCP server selection, provider connection IDs, execution capability config,
  and exposed inter-agent tools.
- `src/components/Settings/AgentsSettings.jsx` exposes agents as autonomous and
  on-demand automations.
- `src-tauri/src/commands/workspace.rs` already has durable workspace snapshots,
  workspace metadata, sessions, files, memories, artifacts, provider selection,
  and MCP session updates.
- `src-tauri/src/db/mod.rs` already has a `workspaces` table.
- `docs/AGENT_WORKSPACE_MIGRATION_RFC.md` documents the move from tabs to
  workspace pages, much of which is already implemented.

### Target Migration

Current scheduled `AgentConfig` records should become one of two things:

- an `AgentDefinition` in the global agent catalog
- a workspace with that agent assigned as the default manager, if it currently
  represents an active scheduled automation

The old model "agent workspace id equals agent id" can remain as a transitional
compatibility rule, but new work should move toward:

- workspace id is the project/team identity
- workspace agent id is an assignment identity
- agent definition id is the reusable catalog identity

### Inter-Agent Tools

The old exposed-tool model should be deprecated.

Instead of any enabled agent calling tools exposed by any other enabled agent,
the manager should assign tasks to agents that are assigned to the same
workspace.

Existing `exposed_tools` fields can be left in place during migration, but the
new UX and prompt model should not depend on them.

## Implementation Phases

### Phase 1: Fleet Vocabulary And Settings Cleanup

Status: started.

Goal:

- align product language around global catalogs and workspace runtime teams
- stop treating "agent" as synonymous with scheduled automation
- keep the implementation vocabulary stable before larger migrations

Work:

- add this document as the current roadmap
- update settings labels and helper text from "automations/inter-agent tools" to
  "agent definitions/catalog"
- leave `docs/MULTI_AGENT_COMMUNICATION_RFC.md` as a historical RFC or mark it
  superseded
- rename confusing code types only when the surrounding implementation is ready

Done:

- `docs/AGENT_FLEET_IMPLEMENTATION.md` exists and states that cross-workspace
  direct communication is not the target model

### Phase 2: Workspace Agent Assignments

Status: backend started.

Goal:

- a workspace can list assigned agents
- one assigned agent is the default manager
- new workspaces always get a default manager assignment
- no runtime delegation yet

Backend work:

- add `workspace_agents` table
- add `default_workspace_agent_id` to the `workspaces` table
- add commands:
  - `workspace_list_agents(workspace_id)`
  - `workspace_assign_agent(workspace_id, agent_definition_id, role)`
  - `workspace_unassign_agent(workspace_agent_id)`
  - `workspace_set_default_agent(workspace_id, workspace_agent_id)`
- include assigned agents in `workspace_get_snapshot`
- when creating a workspace, assign the default manager agent automatically

Frontend work:

- show assigned agents in the workspace UI
- allow assigning/removing agents from the workspace
- allow marking one assigned agent as manager/default

Done:

- additive `workspace_agents` migration
- `workspaces.default_workspace_agent_id`
- workspace assignment commands registered in Tauri
- `assignedAgents` and `defaultWorkspaceAgentId` in workspace snapshots
- new general workspaces auto-create a manager assignment
- workspace UI can assign agents, remove non-manager agents, and set the manager
- manager assignment cannot be removed directly; another agent must be set as
  manager first

### Phase 3: Manager Prompt Uses Assigned Agents

Status: started.

Goal:

- the manager knows which agents are available inside the current workspace
- user and scheduled input are handled by the default manager assignment

Work:

- build a concise assigned-agent summary in the session context or run prompt
- compose manager prompt from app prompt, workspace/user prompt, dynamic
  workspace context, assigned-agent summaries, and current task/user message
- do not inject full worker prompts into the manager prompt
- make command-line/user messages target the workspace manager assignment
- keep missing capability and missing tool failures as runtime blockers

Done:

- `SessionContext` carries concise workspace-agent summaries
- `workspace_get_or_create_session` refreshes the manager session context with
  the current assigned-agent roster before user messages are sent
- general workspace sessions use the default manager agent's instructions and
  execution policy while keeping the workspace filesystem root as their runtime
  workspace
- the system prompt includes a `Workspace Team` section with assigned-agent
  summaries and manager/default-agent guidance

### Phase 4: Workspace-Local Task Delegation

Status: started.

Goal:

- managers can assign bounded tasks to assigned agents in the same workspace
- worker results return to the manager and appear in workspace activity
- this replaces global inter-agent exposed tools as the collaboration model

Backend work:

- add a task table or task records in assistant persistence
- add task statuses:
  - queued
  - running
  - completed
  - failed
  - blocked
  - needs_user_input
- add manager tools:
  - `workspace.list_agents`
  - `workspace.assign_task`
  - `workspace.get_task_result`
  - `workspace.request_user_input`
- execute assigned-agent tasks with the same workspace context and the target
  agent definition's prompt/model/skills

Frontend work:

- show task timeline/activity per workspace
- show task ownership, status, and result summaries
- let users inspect blocked tasks and worker outputs

Done:

- added persistent `workspace_tasks` storage
- added manager-only tools:
  - `workspace.listAgents`
  - `workspace.assignTask`
  - `workspace.getTaskResult`
- `workspace.assignTask` creates a background workspace-local worker session
  for the assigned workspace agent
- task runs use the same workspace root and workspace MCP context, while using
  the assigned agent definition's prompt, provider, and execution policy
- worker responses starting with `BLOCKED:` become blocked task status
- workspace snapshots now include recent task activity for the workspace UI
- workspace UI shows delegated task ownership, status, and latest
  result/blocker/error summary

### Phase 5: Blockers, User Feedback, And Notifications

Status: started.

Goal:

- runtime constraints become visible product states
- users can see which workspace or task needs attention

Work:

- represent blocker events for missing tools, missing MCP servers, denied
  permissions, missing provider/model, failed commands, and missing user input
- surface blockers in workspace activity
- surface pending user feedback in Fleet
- add app notifications for important blocked/failed states
- keep blockers actionable: reason, affected task/agent, requested user action

Done:

- worker task responses starting with `NEEDS_USER_INPUT:` become
  `needs_user_input` task status
- added manager-only `workspace.requestUserInput` tool to create a
  workspace-visible request for user feedback, approval, or missing information
- workspace list entries include running, blocked, failed, and
  needs-user-input task counts plus the latest attention task
- Fleet shows a workspace notification queue for blocked, failed, and
  waiting-for-user tasks across agent and general workspaces
- Workspace pages show an attention banner above task activity when a task is
  blocked, failed, or waiting for user input
- blocked, failed, and waiting-for-user task transitions emit an app-level
  notification event
- the desktop UI listens for task attention events, shows dismissible
  notifications, links directly to the affected workspace, and refreshes Fleet
  state
- workspace tasks can be acknowledged after review so resolved blockers no
  longer keep Fleet in an attention state
- users can submit feedback for tasks waiting on input; the response is stored
  on the task, clears the attention state, and is appended to the manager
  session when one exists

### Phase 6: Skills Catalog

Status: started.

Goal:

- users can define or load skills independently of agents
- agent definitions can select skills
- skill availability does not become a setup gate for workspace assignment

Work:

- add `skill_sources`
- support local skill directories
- support remote git skill repositories with pinned refs
- scan and normalize existing skill repository standards where practical
- let agent definitions select skills
- build worker prompts with selected skills
- keep skill/tool availability runtime-bound

Done:

- added `skill_sources` config storage with local and git source shapes
- added `selected_skill_ids` to agent definitions
- added backend commands to list skill sources, scan discovered skills, add
  local skill sources, and remove skill sources
- local skill sources are scanned recursively for `SKILL.md`
- discovered skills are normalized with id, name, description, source metadata,
  path, and content
- removing a skill source clears agent skill selections from that source
- agent create/update flows persist selected skills without preflight tool or
  workspace capability validation
- agent runtime prompts append selected skill instructions when available
- scheduled agent sessions, workspace manager sessions, and delegated worker
  task sessions use skill-enriched instructions
- Settings now has a Skills catalog page for local sources and discovered
  skills
- agent creation/editing in Settings and Fleet can select reusable skills
- git skill sources clone into the app data directory and can be refreshed
  later
- git skill sources support an optional branch, tag, or commit ref
- skill sources can be enabled or disabled without deleting their configuration
- skill metadata parsing now supports `name` and `description` front matter
  before falling back to markdown headings and paragraphs
- catalog responses include per-source diagnostics so stale, missing, disabled,
  or unreadable sources do not break the whole catalog
- the Skills UI surfaces source diagnostics next to the affected source

Remaining:

- add richer multi-line metadata parsing if needed for specific skill
  repositories

### Phase 7: Remove Cross-Workspace Communication

Status: started.

Goal:

- enforce workspace as the collaboration boundary
- replace the global callable-agent model with workspace-local task delegation

Work:

- remove global callable-agent tool registration from the target UX
- stop depending on `exposed_tools` for new collaboration flows
- migrate any useful executor code into workspace-local task execution
- add migration notes for users with existing exposed inter-agent tools
- remove direct inter-workspace routing after compatibility is no longer needed

Done:

- removed the legacy global exposed-tools editor from the active agent form
  while preserving existing `exposed_tools` data for compatibility
- updated Agents settings copy to describe reusable agent definitions and
  workspace-local manager delegation instead of global inter-agent calls
- existing agents with legacy exposed tools show the count as compatibility
  metadata, not as the preferred collaboration path
- global `agent.*` tools are no longer registered in assistant tool lists
- direct `agent.*` tool execution now returns a compatibility error pointing to
  workspace-local task delegation

### Phase 8: Fleet Control Plane Polish

Status: started.

Goal:

- Fleet becomes the operational dashboard for many workspace teams

Work:

- show workspace health and recent activity
- show assigned agents and default manager per workspace
- show running, blocked, failed, and waiting-for-user tasks
- show schedules and next runs
- show recent failures and notifications
- later, show usage/cost once provider accounting is available

Done:

- Fleet already shows workspace attention states, schedules, next runs, recent
  run ribbons, and recent failures
- workspace cards now show assigned-agent count and the default manager name

## First Data Migration Proposal

Additive migrations only:

```sql
CREATE TABLE IF NOT EXISTS workspace_agents (
  id TEXT PRIMARY KEY,
  workspace_id TEXT NOT NULL,
  agent_definition_id TEXT NOT NULL,
  display_name TEXT,
  role TEXT NOT NULL DEFAULT 'member',
  enabled INTEGER NOT NULL DEFAULT 1,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_workspace_agents_workspace
ON workspace_agents(workspace_id);
```

Then add a nullable manager column:

```sql
ALTER TABLE workspaces ADD COLUMN default_workspace_agent_id TEXT;
```

If SQLite column-exists checks are used, this can be added safely like the
existing `preferred_provider_connection_id` migration.

## Open Decisions

- Whether portable agent definitions should remain in the config file first, or
  move into SQLite before workspace assignments are implemented.
- Whether the existing `AgentConfig` type should be renamed before or after
  adding workspace assignments.
- Whether provider/model selection belongs only on `AgentDefinition`, or whether
  workspace manager assignments should support overrides.
- How much of the existing `execution` capability config remains on agents
  during the transition.
- Whether task records should be their own table immediately, or initially
  represented as assistant sessions/runs with metadata.

## Recommended Next Step

Continue Phase 8 polish.

The remaining high-value work is operational: richer workspace health
aggregation, usage/cost once provider accounting exists, and migration cleanup
for any users with legacy exposed inter-agent tools.
