# Workspace Config Split — Design & Implementation Record

**Status:** In progress — Phases 1-7 + post-cut polish landed; the
Workspace Settings UI redesign is the next planned PR.
**Date:** 2026-05-24 (last updated)
**Author:** juan (with claude)
**Supersedes:** the per-workspace pieces of `AGENT_FLEET_IMPLEMENTATION.md` (the "workspace-local agents" refactor that introduced the current `workspace_agents` SQLite shape).

## Status snapshot

| Area | State |
|---|---|
| App config v1 at `~/.clai/config.json` with `workspaceDirs` + `providerConnections` | **Done** |
| Per-workspace `config.json` + per-workspace `data.sqlite` | **Done** |
| `WorkspaceIndex` discovery + ID collision handling | **Done** |
| `AppState::workspace_root(id)` + `ToolExecutionContext.workspace_root` | **Done** |
| `$HOME` RO injected by default on every new agent (backend) | **Done** |
| Schedule moved to workspace level (`WorkspaceConfig.schedule`) | **Done** |
| `exposed_tools` + inter-agent tool concept deleted | **Done** |
| Tabs/tiles/commands tables + types ripped out | **Done** |
| `mcp/bridge.rs` + `commands/bridge.rs` JS bridge ripped out | **Done** |
| Versioned migrations under `migrations/workspace/` | **Done** |
| **Global SQLite eliminated** (no `init_db`, no `~/.clai/clai.sqlite`) | **Done** |
| `pool: State<'_, DbPool>` stripped from every Tauri command | **Done** |
| `workspace_permissions.rs` (the legacy `.clai/permissions.json`) deleted | **Done** |
| Workspace Settings UI redesign (sidebar + agent management embedded) | **Next PR** |
| Frontend `workspaceStore.js` + dead `TabBar`/`TabView` components | **Deferred** (backend stubs are no-ops, no runtime breakage) |

`cargo check` / `cargo clippy --all-targets -- -D warnings` / `cargo test` /
`npm run build` / `eslint` all clean as of this update. 340 lib tests pass.

---

## 1. Why

We hit a sync gap investigating the permission-approval flow:

- "Always allow" / "Always deny" shell-command grants land in
  `<workspace_root>/.clai/permissions.json` (workspace-tier file).
- The workspace-settings form reads from `workspace_agents.execution.shell.*`
  (per-agent SQLite blob).
- Runtime unions both at policy time (`assistant/tools/local.rs:907-949`), so
  grants *work* — but they don't show up in the UI, and they can't be removed
  from the UI.
- Path-grant flow (newer) already chose a different persistence target: the
  per-agent SQLite blob (`commands/path_grants.rs:281-341`).

Two stores, two flows, drifting. The label even lies — `InlineApprovalCard.jsx:268-280`
says *"Always allow (this agent)"* but persistence is workspace-wide.

Rather than patch the symptom, we're collapsing the config surface into a
single human-editable per-workspace file. The deeper reason: workspace
portability. Today a workspace is "a folder + scattered SQLite rows referencing
its UUID." That can't be cloned, version-controlled, or hand-edited.

This app has no user base outside the author. **No backwards compatibility
is offered.** Local state will be wiped at cutover.

---

## 2. Decisions

| # | Decision | Picked |
|---|---|---|
| D1 | Single source of truth for workspace config | `<root>/.clai/config.json` — supersedes `workspaces` row + `workspace_agents` rows + `.clai/permissions.json` |
| D2 | Format | JSON, atomic-rename writes, mutex per workspace |
| D3 | App-config file | Use `~/.clai/config.json`. Keep `version: 1` — the file at the new path is a fresh v1, not an upgraded v1. New fields use serde defaults; no migration code needed |
| D4 | Workspaces directory | List: `workspaceDirs: [path, ...]`. Discovery scans every entry, builds in-memory `id → root` index at startup |
| D5 | New-workspace dir picker | If list length == 1, silent create. If > 1, dropdown at creation time (no remembered default) |
| D6 | Default workspaces directory | `~/.clai/workspaces/` (matches the per-workspace `.clai/` convention; one dir to back up; cross-platform path symmetry) |
| D7 | Workspace ID | Lives in the file. On `cp -r` collision, second load refuses + offers "regenerate ID" |
| D8 | Per-workspace SQLite for runtime data | Yes. `<root>/.clai/data.sqlite` holds sessions/messages/runs/tool_calls/workspace_tasks. `workspace_id` columns disappear (implicit by DB) |
| D9 | Provider connections | Move into app config. `secret_ref` stays a keyring handle — secrets never touch JSON or SQLite. Drop the `provider_connections` SQLite table |
| D10 | Stable refs for skills | `{source: bundled\|personal\|remote, url?, slug}`. Survives clone-to-other-machine when target has the same source configured |
| D11 | Stable refs for MCPs | `{name}`. Names are user-chosen and unique per install; dangling on missing target |
| D12 | Stable refs for provider connections | `id` (machine-local). Dangling on clone = "set up a connection" prompt |
| D13 | Backwards compatibility | None. Author wipes local workspaces at cutover. App config starts fresh under `~/.clai/config.json`; provider connections may need re-setup |
| D14 | Live-reload of manual file edits | Not in v1. Read on workspace-open, write atomic. Add `notify`-watcher in a later iteration |
| D15 | Default workspace | Drop the special-cased `default` workspace. First-run creates a real workspace (with a UUID directory name) |
| D16 | App-dir override | Respect `$CLAI_HOME` env var. `clai_home() = $CLAI_HOME ?: $HOME/.clai`. Lets CI / clean profiles redirect without code changes |
| D17 | Workspace directory names | UUID, matching the `id` field in the workspace's `config.json`. Walking `~/.clai/workspaces/` shows opaque dirs; titles live in the file. Identity is location-stable across title renames. Non-UUID dirs in `workspaceDirs` entries are skipped during scan |
| D18 | How callers learn the workspace root | The engine resolves the root **once at run-build time** via `AppState::workspace_root(id)` and stores it on `ToolExecutionContext.workspace_root: Option<PathBuf>`. Tools read the path as a plain value — no lookup, no lock contention per tool call. Non-tool callers (Tauri commands, engine, workspace-create) already have `State<'_, AppState>` reach and call `AppState::workspace_root(id)` directly. The free function `agent_workspace_root_for_id` is deleted. Index storage is `Arc<RwLock<WorkspaceIndex>>` inside `AppState` — reads dominate, writes (create / delete / re-id) are rare, per-field locking is premature |
| D19 | Data/cache partition + filesystem-layout renames at cutover | Inside `~/.clai/`, partition user-content from regeneratable caches. Several legacy names are also renamed at the same cutover to match post-cut semantics: <br/>• `<data_dir>/clai/agent-workspaces/` → `~/.clai/workspaces/` (drop the "agent-" prefix — there's no longer a separate agent-workspace concept) <br/>• `<data_dir>/clai/workspace.db` → `~/.clai/clai.sqlite` (the global DB no longer holds workspace data after the cut; the old name is misleading) <br/>• `<data_dir>/clai/bundled/` → `~/.clai/cache/bundled/` (regeneratable from binary) <br/>• `<data_dir>/clai/skill-sources/<id>/` → `~/.clai/cache/skill-sources/<id>/` (regeneratable from git) <br/>• `<data_dir>/clai/skill-sources/personal/` → `~/.clai/skills/` (USER CONTENT — copy manually if needed) <br/><br/>The `cache/` subdir is safe to `rm -rf`: bundled assets re-materialize, git sources re-clone on next sync. The `APP_IDENTIFIER` constant (`"clai"`) becomes vestigial after Phase 7 |
| D20 | Versioned migrations via `sqlx::migrate!` | Per-workspace `data.sqlite` schema lives under `src-tauri/migrations/workspace/`. Each schema change is a new numbered `.sql` file. `_sqlx_migrations` table per DB tracks applied versions. Eager fan-out at startup applies pending migrations to every indexed workspace; lazy open does the same for workspaces created at runtime. **No global migrations folder** — there is no global DB schema to evolve |
| D21 | No global DB at all | After tracing all session-create paths, every assistant session in the current UI is workspace-bound. The global pool (`~/.clai/clai.sqlite`) held nothing real. `db::init_db` is deleted; `app_handle.manage(pool)` removed; every `pool: State<'_, DbPool>` parameter stripped from Tauri commands; `pool_for_new_session` errors with *"session context has no workspace_id"* instead of falling back. Future non-workspace sessions, if ever needed, get a dedicated subsystem rather than reviving a global pool |
| D22 | Schedule lives on the workspace, not on the agent | `WorkspaceConfig.schedule: WorkspaceSchedule { enabled, paused, interval_minutes }`. The scheduled tick invokes `WorkspaceConfig.default_agent_id`. `WorkspaceAgent` no longer carries `schedule_enabled` / `schedule_paused` / `interval_minutes`. Two scheduled flows in one workspace = either split into two workspaces, or have the scheduled manager delegate. Matches existing UI: sub-agents are reactive to manager invocations, not independently scheduled |
| D23 | `exposed_tools` deleted | The field was the carrier for a cross-workspace agent-tool exposure model that never materialized. The `inter_agent` tool implementation, the `ExposedAgentTool` struct, the `default_exposed_tools` slot on bundled agent templates, and the corresponding `AgentFormModal` UI section all removed. Future inter-workspace tool exposure (if needed) gets a fresh design at the workspace level — not per-agent |
| D24 | `$HOME` RO default lives in the backend | Helper `default_agent_execution()` returns an `ExecutionCapabilityConfig` with the host `$HOME` pre-granted RO. `WorkspaceAgent::new_manager` uses it for the manager agent. `workspace_create_agent` injects it for sub-agents that don't supply their own `extra_paths`. Frontend `AgentFormModal` no longer resolves `homeDir()` or pre-injects an entry — the form receives the populated default from the backend. Closes the asymmetry where the manager had no `$HOME` access while sub-agents (created via the form's `useEffect` injection) did |
| D25 | Permissions stay per-agent (for now) | Considered workspace-level baseline + per-agent override. Decision: keep the current per-agent shape. The "grant the same prefix twice" annoyance is acknowledged and accepted for now; if it gets painful in real use, revisit with workspace-baseline + override. The narrowing case (reviewer agent without shell) is naturally handled by per-agent `execution.shell.mode = off` |

---

## 3. End-State Layout

```
~/.clai/                                                   # app root (override via $CLAI_HOME)
  config.json                                              # APP config v1
    - workspaceDirs: ["~/.clai/workspaces"]
    - aiProvider, mcpServers, skillSources
    - providerConnections[]   (id + secretRef → OS keyring)
  clai.sqlite                                              # GLOBAL DB, shrunk: tabs, commands, workspace_meta

  workspaces/                                              # USER CONTENT: default workspaceDirs entry
    <uuid-1>/
      .clai/
        config.json                                        # WORKSPACE config (id == <uuid-1>)
        data.sqlite                                        # workspace runtime DB
        memory/
      <artifacts>
    <uuid-2>/
      .clai/...

  skills/                                                  # USER CONTENT: personal skill source default location
    <slug>/SKILL.md
    ...

  cache/                                                   # REGENERATABLE: safe to wipe
    bundled/
      skills/                                              # materialized from binary at startup
      agent-templates/                                     # materialized from binary at startup
    skill-sources/
      <source-id>/                                         # git clones, re-fetched on first sync
```

Data/cache partition rationale:
- `tar czf clai-backup.tar.gz ~/.clai` captures everything. `--exclude=cache/` for a slim backup.
- `rm -rf ~/.clai/cache/` is a safe "clear caches" operation; app re-materializes bundled assets and re-clones git sources on next launch.
- `rm -rf ~/.clai` returns to first-run state.

`SkillSourceConfig.path` for the bundled and personal sources now defaults
to `~/.clai/cache/bundled/skills/` and `~/.clai/skills/` respectively
(was `dirs::data_dir()/clai/bundled/skills/` and
`dirs::data_dir()/clai/skill-sources/personal/`).

### Global SQLite after the cut

Only **UI state** remains:

- `tabs` — workspace-grid tabs
- `commands` — per-tab command state
- `workspace_meta` — singleton k/v (`active_tab_id`)

Everything else either moves to `<root>/.clai/data.sqlite` or to JSON.

---

## 4. Schemas

### 4.1 `AppConfig` — `~/.clai/config.json`

Wire-shape note: the existing `ClaiConfig` (`config/types.rs:577`) uses
**snake_case** JSON (no `rename_all` attribute) and has **no `version`
field**. The new `AppConfig` switches to **camelCase** (matching the new
`WorkspaceConfig` and the Tauri command wire format) and introduces an
explicit `version`. A one-off manual cutover can read the legacy snake_case
file and write the new camelCase shape, but no migration binary is shipped.
After the cut, the legacy `ClaiConfig` type is deleted.

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppConfig {
    /// Schema version. Starts at 1 for the new shape. `#[serde(default)]`
    /// gives `1` when the field is absent. Fresh app-created files write it
    /// explicitly; the default mostly exists for hand-edited files that
    /// omitted it.
    #[serde(default = "default_app_config_version")]
    pub version: u32,

    /// Where workspaces live. List, not single path — first entry is the
    /// default for silent create when length == 1. Tilde-prefixed paths
    /// are expanded against `$HOME` at load time.
    #[serde(default = "default_workspace_dirs")]
    pub workspace_dirs: Vec<PathBuf>,

    /// Global AI provider for headless automations (existing field).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ai_provider: Option<AiProvider>,

    /// User-configured MCP servers (existing field, unchanged schema).
    #[serde(default)]
    pub mcp_servers: Vec<McpServerConfig>,

    /// User-configured skill sources (existing field, unchanged schema).
    #[serde(default)]
    pub skill_sources: Vec<SkillSourceConfig>,

    /// MOVED FROM SQLITE. The `secret_ref` is a keyring handle —
    /// the actual API key lives in the OS vault, not this file.
    #[serde(default)]
    pub provider_connections: Vec<ProviderConnection>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderConnection {
    pub id: String,                          // UUID, machine-local
    pub name: String,
    pub provider_id: String,                 // "openai", "anthropic", ...
    pub auth_mode: AuthMode,                 // reuses existing enum from
                                             // `assistant/types.rs:20`:
                                             //   subscription_login, subscription_api_key,
                                             //   developer_api_key, workspace_token
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    pub secret_ref: String,                  // keyring handle
    pub model_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_label: Option<String>,
    pub enabled: bool,
    pub created_at: i64,
    pub updated_at: i64,
}
```

Today `assistant/repository.rs:148, 468, 513` already serialize `AuthMode`
to a JSON-string SQL column. Moving to a typed JSON field is a strict
simplification — no more `to_json_string` / `parse_json` indirection.

### 4.2 `WorkspaceConfig` — `<root>/.clai/config.json`

```rust
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
    pub version: u32,                        // 1
    pub id: String,                          // UUID, in-file, collision-detected
    pub title: String,
    pub created_at: i64,
    pub updated_at: i64,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preferred_provider_connection_id: Option<String>,

    pub default_agent_id: String,            // points to one of agents[].id

    /// Per-workspace schedule. When `enabled` and not `paused`, the
    /// scheduler fires the default (`default_agent_id`) agent every
    /// `interval_minutes` minutes. Sub-agents never run on their own
    /// schedule — they're invoked by the manager via delegation.
    #[serde(default)]
    pub schedule: WorkspaceSchedule,

    #[serde(default)]
    pub agents: Vec<WorkspaceAgent>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceAgent {
    pub id: String,                          // UUID, unique within workspace
    pub name: String,
    pub description: String,                 // system prompt body
    pub enabled: bool,

    #[serde(default)]
    pub selected_skills: Vec<SkillRef>,
    #[serde(default)]
    pub selected_mcp_servers: Vec<McpRef>,
    #[serde(default)]
    pub provider_connection_ids: Vec<String>,

    /// Per-agent sandbox config. New agents are created with the host
    /// `$HOME` granted RO by default (see `default_agent_execution()`).
    /// The user can ×-remove `$HOME` in agent settings to harden any
    /// specific agent.
    #[serde(default)]
    pub execution: ExecutionCapabilityConfig,

    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "camelCase")]
pub enum SkillRef {
    /// `bundled:<slug>` — ships with the binary, always available.
    Bundled { slug: String },
    /// `personal:<slug>` — user's personal local skill dir.
    Personal { slug: String },
    /// `remote:<url>#<slug>` — git-sourced; target machine needs the same
    /// remote configured in its app `skillSources`.
    Remote { url: String, slug: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpRef {
    /// Matches AppConfig.mcp_servers[].name on the resolving machine.
    pub name: String,
}
```

### 4.3 Per-Workspace `data.sqlite` — `<root>/.clai/data.sqlite`

```sql
-- workspace_id columns removed everywhere; the DB itself implies the workspace.
-- tab_id stays as a soft reference (no FK, since tabs live in the global DB).
-- connection_id is a soft ref into AppConfig.providerConnections by id.

CREATE TABLE assistant_sessions (
    id          TEXT PRIMARY KEY,
    tab_id      TEXT,                           -- soft ref, no FK
    kind        TEXT NOT NULL,
    title       TEXT,
    context_json TEXT NOT NULL,                 -- minus the workspace_id field
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL
);

CREATE TABLE assistant_runs (
    id           TEXT PRIMARY KEY,
    session_id   TEXT NOT NULL REFERENCES assistant_sessions(id) ON DELETE CASCADE,
    status       TEXT NOT NULL,
    trigger      TEXT NOT NULL,
    connection_id TEXT NOT NULL,                -- soft ref into AppConfig
    provider_id  TEXT NOT NULL,
    model_id     TEXT NOT NULL,
    usage_json   TEXT,
    error        TEXT,
    notices_json TEXT,
    started_at   INTEGER NOT NULL,
    completed_at INTEGER
);

CREATE TABLE assistant_messages (...);          -- shape unchanged
CREATE TABLE assistant_tool_calls (...);        -- shape unchanged

CREATE TABLE workspace_tasks (
    -- everything currently in workspace_tasks EXCEPT workspace_id
    id                            TEXT PRIMARY KEY,
    created_by_workspace_agent_id TEXT,
    assigned_to_workspace_agent_id TEXT NOT NULL,
    -- ... title, instructions, status, etc.
);
```

### 4.4 Global SQLite after the cut

```sql
CREATE TABLE tabs (...);                        -- unchanged
CREATE TABLE commands (...);                    -- unchanged
CREATE TABLE workspace_meta (...);              -- unchanged (active_tab_id k/v)

-- DROPPED:
-- workspaces
-- workspace_agents
-- provider_connections
-- assistant_sessions, assistant_runs, assistant_messages, assistant_tool_calls  (moved per-workspace)
-- workspace_tasks  (moved per-workspace)
```

---

## 5. Workspace Discovery + ID Collision

### Discovery

On startup, walk `workspaceDirs` in order. For each entry:

1. List immediate subdirectories.
2. **Skip any subdir whose name is not a valid UUID** (per D17 — workspaces always have UUID directory names).
3. For each remaining subdir, parse `<sub>/.clai/config.json`.
4. **Verify `config.id == directory_name`.** Mismatch is treated as collision (see below) — refuse to load, log loudly.

Build an in-memory index:

```rust
struct WorkspaceIndex {
    by_id: HashMap<String, WorkspaceLocator>,   // workspace_id → metadata
    sorted_by_updated: Vec<String>,             // for fleet ordering
    load_failures: Vec<WorkspaceLoadFailure>,   // surface in UI
}

struct WorkspaceLocator {
    id: String,
    root_path: PathBuf,                         // <workspaceDir>/<uuid>
    title: String,
    updated_at: i64,
    // schedule_enabled / schedule_paused / interval_minutes cached here
    // so fleet listing doesn't need to reparse the file for every card.
}

struct WorkspaceLoadFailure {
    path: PathBuf,
    reason: LoadFailureReason,                  // BadName | IdMismatch | DuplicateId | Unparseable | ...
}
```

`agent_workspace_root_for_id` (today: `assistant/tools/local.rs:40`) becomes
an index lookup. The path is no longer derivable from the ID.

### Collision / mismatch cases

| Situation | Detection | Action |
|---|---|---|
| Two dirs with same ID (clean `cp -r <uuid1> <uuid2>` where both files still have id=uuid1) | Same `id` value seen from two different directories | First wins; second goes to `load_failures` with reason `DuplicateId`. UI surfaces with a one-click "regenerate ID" that rewrites the second file with a fresh UUID, renames the directory, and triggers a rescan |
| Wonky `cp -r <uuid1> <uuid1-copy>` | Directory name `<uuid1-copy>` isn't a valid UUID | Skipped silently (per D17) |
| Hand-edited `id` doesn't match dir name | `config.id != dir.file_name()` | `load_failures` with reason `IdMismatch`. UI shows "directory says X, file says Y — pick one" |
| `data.sqlite` corrupt or unopenable | Sweep fan-out (Phase 6) catches it | `load_failures` with reason `DbCorrupt`. Workspace appears in UI as broken but isn't loaded |

### Missing or unreadable `workspaceDirs` entries

Warn, skip the entry, keep the app functional. Allows an unmounted external
drive in the list.

### How callers learn the workspace root

The index is the single source of `id → root_path`. Two consumer surfaces:

**Non-tool callers** (Tauri commands, engine, workspace-create / -delete) already hold `State<'_, AppState>`. They call:

```rust
impl AppState {
    /// Look up a known workspace's filesystem root. Returns `None` if the
    /// workspace was deleted, has a load failure, or never existed.
    pub fn workspace_root(&self, workspace_id: &str) -> Option<PathBuf> {
        self.workspace_index.read().ok()?.by_id.get(workspace_id).map(|loc| loc.root_path.clone())
    }

    /// Resolve the directory where a *new* workspace should land. Distinct
    /// from `workspace_root` because no entry exists yet for it. Frontend
    /// passes the target dir (chosen from the dropdown, or omitted when
    /// `workspaceDirs.len() == 1`).
    pub fn workspace_create_target(&self, requested: Option<&Path>) -> Result<PathBuf, String> { ... }
}
```

**Tool callers** never touch the index. The engine's run-builder resolves the path once and stores it on the context:

```rust
pub struct ToolExecutionContext {
    // existing fields...
    pub agent_workspace_id: Option<String>,
    /// Resolved at run-build time from `AppState::workspace_root(agent_workspace_id)`.
    /// `None` only when this run isn't tied to a workspace (e.g., a generic chat
    /// before any workspace context).
    pub workspace_root: Option<PathBuf>,
}
```

`ensure_workspace_root(context)` (today: `assistant/tools/local.rs:601`) becomes a value read of `context.workspace_root` plus the existing `mkdir .clai/memory/journal` side-effect. No lookup, no lock.

**Staleness trade-off.** The `PathBuf` is captured once per run. If the user deletes the workspace mid-run (vanishingly rare — workspaces aren't deleted casually), the cached path goes stale and the next filesystem operation in the run fails with a normal "no such file" error. Acceptable; the alternative ("re-lookup on every tool call") trades a benign edge case for per-call lock overhead.

**Resolution failure at run-start.** If `AppState::workspace_root(id)` returns `None` (workspace was deleted between session-create and run-start, or has a load failure), the run-builder must fail the run with a clear `"workspace <id> no longer exists or failed to load"` error. Engine error path already exists for unrecoverable run-start conditions; we just plug into it.

---

## 6. Blast-Radius Map

### 6.1 SQL touchpoints (files that go away or change shape)

| File | What lives there now | Action |
|---|---|---|
| `src-tauri/src/db/mod.rs` | All schema migrations + table creates for `workspaces`, `workspace_agents`, `workspace_tasks`, `provider_connections`, `assistant_*` | Drop all five from the global migrator. Add a per-workspace migrator (called on each `data.sqlite` open). Keep `tabs`, `commands`, `workspace_meta` migrations. |
| `src-tauri/src/commands/workspace.rs` | `workspace_create`, `workspace_list`, `workspace_delete`, `workspace_set_title`, `workspace_set_provider`, `workspace_set_default_agent`, `workspace_get_snapshot`, `workspace_run_now`, `workspace_set_schedule_paused`, plus all SQL utilities (`workspace_default_agent_id`, `set_workspace_default_agent_id`, `load_workspace_agent_rows`, etc.) | Rewrite every command to operate on the in-memory index + `WorkspaceConfig` file I/O. SQL utilities deleted. |
| `src-tauri/src/commands/workspace_agents.rs` | `workspace_get_agent`, `workspace_create_agent`, `workspace_update_agent`, `workspace_delete_agent`, `workspace_set_agent_enabled`, `load_detail` | Rewrite to mutate `WorkspaceConfig.agents[]` in the file. |
| `src-tauri/src/commands/permissions.rs:262, 273-347` | `persist_decisions` → `.clai/permissions.json` | Rewrite to mutate `WorkspaceConfig.agents[<id>].execution.shell.{allowed,blocked}_command_prefixes`. Drop the `workspace_permissions` module. |
| `src-tauri/src/commands/path_grants.rs:281-341` | `persist_grant_to_agent` → SQLite `execution` blob | Rewrite to mutate `WorkspaceConfig.agents[<id>].execution.filesystem.extra_paths`. |
| `src-tauri/src/commands/provider_connections.rs:190` | Lookup workspace_agents that reference a provider | Switch to scanning all `WorkspaceConfig` files (across all `workspaceDirs`). |
| `src-tauri/src/commands/skills.rs:28, 52` | Update agents referencing a skill | Same — scan + write all relevant `WorkspaceConfig` files. |
| `src-tauri/src/commands/mcp_servers.rs:14, 30` | Update agents referencing an MCP | Same. |
| `src-tauri/src/commands/assistant.rs:150` | Read execution blob for a workspace agent | Index lookup. |
| `src-tauri/src/commands/fleet.rs:285` | Read agent `enabled` | Index lookup. |
| `src-tauri/src/assistant/tools/inter_agent.rs:37` | Load sibling agent rows | Index lookup. |
| `src-tauri/src/assistant/tools/workspace_tasks.rs:646, 669, 730, 777, 814, 848` | Load/create/update workspace_tasks rows joined with workspace_agents | Tasks move to per-workspace `data.sqlite`; agent names come from the in-memory index. |
| `src-tauri/src/agents/init.rs:43-90` | `populate_scheduler_from_workspace_agents` — reads workspace_agents at startup | Replace SELECT with an iteration over `WorkspaceIndex` (which already contains schedule fields). |
| `src-tauri/src/agents/runner.rs:69-110` | `load_agent_row` — reads workspace_agents at run time | Replace with index lookup. |
| `src-tauri/src/assistant/repository.rs` (provider_connections CRUD: lines 460, 506, 535, 551, 563) | SQLite-backed provider connections | Move CRUD to in-memory `AppConfig`, persist to JSON. |

### 6.2 Path-helper touchpoints

| File:line | Today | Action |
|---|---|---|
| `config/mod.rs:107` | `get_config_path()` uses `dirs::config_dir().join(APP_IDENTIFIER)` | Replace with `clai_home().join("config.json")` |
| `assistant/tools/local.rs:40` | `agent_workspace_root_for_id(uuid)` — pure function, deterministic path | **Deleted.** Tool callers read `context.workspace_root`; non-tool callers use `AppState::workspace_root(id)` |
| `assistant/tools/local.rs:601` | `ensure_workspace_root(context)` calls the pure helper | Reads `context.workspace_root` directly; keeps the `mkdir .clai/memory/journal` side-effect |
| `assistant/engine.rs:775` | calls the helper for the running automation | Replaced when the engine builds `ToolExecutionContext` — resolves the root once via `AppState::workspace_root(id)` and sets `context.workspace_root` |
| `commands/workspace.rs:2325, 674` | `agent_workspace_root_for_id(id)` inside command handlers | `state.workspace_root(id)` (or `state.workspace_create_target(...)` for the create path, which doesn't have an existing entry to look up) |
| `commands/workspace.rs:2412, 2644` | helper called from list / delete handlers | Same — `state.workspace_root(id)` |
| `commands/workspace.rs:629` | `ensure_agent_workspace_root` creates `<root>/.clai/memory/journal` etc. | unchanged (creates within the resolved root) |
| `db/mod.rs:13` | `get_db_path()` returns `<data_dir>/clai/workspace.db` | Replace with `clai_home().join("clai.sqlite")`. Rename the file at the same time (D19) |
| `assistant/tools/local.rs:20-21` | `APP_IDENTIFIER = "clai"`, `AGENT_WORKSPACES_DIR = "agent-workspaces"` constants | Delete the local copies; the rename to `workspaces/` lives in the index now |
| `config/bundled.rs:33` | `bundled_root() = <data_dir>/clai/bundled/` | Replace with `clai_cache_bundled_root() = clai_home()/cache/bundled/` |
| `config/bundled.rs:37` | `bundled_skills_root()` joins `bundled_root()` + `"skills"` | Unchanged in shape; resolves under the new cache root automatically |
| `config/bundled.rs:41` | `bundled_agent_templates_root()` joins `bundled_root()` + `"agent-templates"` | Same — resolves under new cache root |
| `config/bundled.rs:45` | `personal_skills_root() = <data_dir>/clai/skill-sources/personal/` | Replace with `clai_skills_root() = clai_home()/skills/`. User content can be copied manually before cutover if needed |
| `commands/skills.rs:454` | `skill_source_cache_root() = <data_dir>/clai/skill-sources/` | Replace with `clai_cache_skill_sources_root() = clai_home()/cache/skill-sources/` |
| *(new)* `paths/mod.rs` | n/a | New module with `clai_home()` (`$CLAI_HOME ?: $HOME/.clai`), `clai_global_db_path()`, `clai_skills_root()`, `clai_cache_root()`, `clai_cache_bundled_root()`, `clai_cache_skill_sources_root()`, `expand_tilde(&Path)` |
| *(new)* `lib.rs` `AppState` | n/a | Adds `workspace_index: Arc<RwLock<WorkspaceIndex>>`; gains `workspace_root(id)` and `workspace_create_target(requested)` methods |
| *(new)* `tools/mod.rs:21` `ToolExecutionContext` | n/a | Adds `workspace_root: Option<PathBuf>`. Engine run-builder populates it; `None` only for non-workspace runs |

### 6.3 Frontend (read-only impact)

Tauri command **interfaces** stay byte-compatible for v1. The frontend
shouldn't notice the storage change.

| File | Calls | Behavior change |
|---|---|---|
| `src/pages/Workspace.jsx`, `src/pages/Fleet.jsx` | `workspaceGetAgent`, `workspaceCreateAgent`, `workspaceUpdateAgent`, `workspaceDeleteAgent`, `listWorkspaces`, `getWorkspaceSnapshot`, `runWorkspaceNow`, `setWorkspaceSchedulePaused` | None — same wire shapes. Approval grants now show up in the form (the original bug). |
| `src/components/Settings/AgentFormModal.jsx` | reads `execution` from snapshot | None — same shape, now sourced from the file. |
| `src/components/InlineApprovalCard.jsx`, `src/components/InlinePathGrantCard.jsx` | invoke `submit_permission_decision`, `submit_path_grant_decision` | None — backend rewires, frontend doesn't notice. Fix the label text from "(this agent)" — it's accurate now, but keep the wording consistent with the form. |
| `src/api/client.js` | provider connection CRUD via `provider_connection_*` commands | None — wire shape preserved. |

### 6.4 Cross-cutting types

| Type | File | Action |
|---|---|---|
| `AssistantSessionContext.workspace_id` | `assistant/types.rs:135` | Now redundant (session lives in the per-workspace DB). Keep the field for one transition release, then drop. |
| `WorkspaceTaskTarget.workspace_id` | `assistant/tools/workspace_tasks.rs:55` | Drop; tasks are scoped by DB. |
| `WorkspaceListEntry` | `commands/workspace.rs:2243` | Rebuild from `WorkspaceIndex`, same wire shape. |
| `WorkspaceSnapshot` | `commands/workspace.rs:122` | Rebuild from `WorkspaceConfig` + in-memory schedule cache; `schedule_enabled` / `schedule_paused` / `interval_minutes` / `next_run_in_seconds` already added in commit `06a7c46`. |

### 6.5 What stays untouched

- Memory store (`<root>/.clai/memory/`) — already file-based.
- Skill **discovery logic** (`discover_skills`, `materialize_embedded_dir`, frontmatter parsing) — behavior unchanged. Only the *paths* it operates on move (covered in §6.2): bundled materialization target, personal-source default, git-cache root.
- Scheduler crate (`agents/scheduler.rs`) — internals unchanged, only the population path moves from SQL to index.
- MCP client (`mcp/client.rs`) — uses `McpServerConfig.id` resolved from app config; we'll resolve `McpRef.name → id` at session-build time.
- OS keyring usage (`auth/token.rs`, `assistant/local_mcp.rs:248`) — unchanged.

---

## 7. Phased PR Plan

> All phases below are **Done** (Phase 0 was the doc itself; Phases 1-7
> landed across several commits; the post-cut polish — schedule to workspace,
> `$HOME` RO default, `exposed_tools` delete, global DB elimination, tabs/tiles
> rip — is captured in D20-D25 and is also Done). The bullets are preserved as
> the historical record. **The next planned PR is the Workspace Settings UI
> redesign — see §7.2.**

### 7.1 Manual cutover note

No `migrate-to-clai-home` binary is shipped for this cutover. Per D13, local
workspace state is disposable. Personal skills are the only user-authored files
in the old layout; copy `<data_dir>/clai/skill-sources/personal/` to
`~/.clai/skills/` manually before first launch if they should survive. Provider
connections are recreated through the UI.

### 7.2 Next planned PR — Workspace Settings UI redesign

The Workspace Settings entry point today opens `AgentFormModal` in
`mode="workspace"`, which is really the *manager agent's* form mislabeled. The
agents drawer hosts edit / remove / add buttons that hide all agent management
behind a side-drawer affordance. The next PR replaces this with a unified
Workspace Settings surface. Decisions (from chat, not yet codified in code):

- **Modal**, sidebar nav, single panel for everything workspace + every agent.
- Workspace sections: General (title), Schedule (the new workspace-level fields), Provider/Storage later.
- Agent sections: one per agent (manager + every sub-agent). "+ Add agent" lives here.
- The manager agent does NOT have a user-facing name; it IS "the main agent of this workspace." The workspace's title is its own field, no longer doubling as the manager's name.
- The agents drawer **stays** as an at-a-glance status surface. Delete works inline. Edit and Add deep-link into the Workspace Settings modal scrolled to the relevant agent (or to the "+ Add" form for create).
- Confirmed deletions for this PR: the `AgentFormModal` *Schedule* and *Exposed Tools* sections (already gone backend-side); the frontend `homeDir()` injection useEffect (already gone).

### Phase 0 — Design doc (this file)

No code. Decision record + blast-radius map. Reviewed → merged → starts the clock.

### Phase 1 — Types, schema, paths

Pure type/serde work. Nothing wired up yet.

- New `paths/mod.rs`: `clai_home()`, `clai_global_db_path()`, `clai_skills_root()`, `clai_cache_root()`, `clai_cache_bundled_root()`, `clai_cache_skill_sources_root()`, `expand_tilde(&Path)`. Wire `db/mod.rs`, `config/mod.rs`, `config/bundled.rs`, and `commands/skills.rs` to use these instead of `dirs::data_dir()`/`dirs::config_dir()`-rooted paths (the renames in D19 ride this change).
- Add `AppConfig` fields (`workspaceDirs` defaulted to `["~/.clai/workspaces"]`, `providerConnections` defaulted to empty).
- Add `WorkspaceConfig`, `WorkspaceAgent`, `SkillRef`, `McpRef` types in a new module `config/workspace_config.rs`.
- Atomic-rename writer + lenient reader, mutex-guarded.
- Per-workspace `data.sqlite` schema constants + migrator stub.
- `WorkspaceIndex` skeleton type (storage layout only, scanning lands in Phase 2).
- `AppState::workspace_root(id)` and `workspace_create_target(...)` methods on `AppState` (return `None` / use legacy path until the index is populated in Phase 2 — keeps existing call sites green during the transition).
- Add `workspace_root: Option<PathBuf>` field to `ToolExecutionContext`. Default to `None` everywhere it's constructed; tools still call the legacy `agent_workspace_root_for_id` until Phase 3 wires the engine to populate the field.
- Unit tests for round-trips, version-default behavior, malformed file, atomic rename, tilde expansion, UUID validation.

### Phase 2 — Workspace index + discovery

Build the scan + in-memory index. No callers yet — wired in phase 3.

- `WorkspaceIndex` with `by_id` map + `Vec` sorted by `updated_at` + `load_failures` list.
- `scan_workspace_dirs(&AppConfig) -> WorkspaceIndex` walking the list, parsing each `config.json`, enforcing UUID directory names + `id == dir_name`, detecting ID collisions.
- Populate `AppState::workspace_index` at startup (after `AppConfig` load). `AppState::workspace_root(id)` now returns real paths from the index.
- Rescan trigger (manual via command), no fs-notify yet.
- The free function `agent_workspace_root_for_id` is **not** rewired here — it still returns the legacy deterministic path until Phase 3 deletes it.

### Phase 3 — Switch reads to file/index

Storage flip-day for *reads*. SQLite still holds the data; we just stop
sourcing the UI from it.

- `workspace_get_snapshot`, `workspace_list`, `workspace_get_agent`, `workspace_list_agents`, fleet listings: read from index + `WorkspaceConfig` file.
- Scheduler population reads from the index.
- **Engine run-builder** resolves `workspace_root` via `AppState::workspace_root(id)` and sets `ToolExecutionContext.workspace_root`. Fail the run cleanly when the lookup returns `None`.
- **Tool layer** switches: `ensure_workspace_root` (`assistant/tools/local.rs:601`) reads `context.workspace_root` instead of calling the free function.
- **Non-tool call sites** in `commands/workspace.rs` switch from `agent_workspace_root_for_id(id)` to `state.workspace_root(id)` (or `state.workspace_create_target(...)` in `workspace_create`).
- Delete the free function `agent_workspace_root_for_id` once no callers remain.
- First launch creates the new `~/.clai/config.json`; workspaces start empty and get created fresh.

### Phase 4 — Switch writes to file

- `workspace_create`, `workspace_set_title`, `workspace_delete`: edit the file system (create dir + write `config.json`, rename, delete dir).
- `workspace_create_agent`, `workspace_update_agent`, `workspace_delete_agent`, `workspace_set_agent_enabled`: mutate `WorkspaceConfig.agents[]` in the file.
- `workspace_set_schedule_paused`, `workspace_run_now`: edit `WorkspaceConfig.agents[default].schedule_paused`, then poke the scheduler instance.
- `submit_permission_decision`: writes to `WorkspaceConfig.agents[<id>].execution.shell.{allowed,blocked}_command_prefixes`. **Original bug closed by construction.**
- `submit_path_grant_decision`: writes to `WorkspaceConfig.agents[<id>].execution.filesystem.extra_paths`.

At end of phase 4, no code writes to `workspaces` / `workspace_agents` / `.clai/permissions.json` anymore.

### Phase 5 — Provider connections to app config

- Move provider-connection CRUD (`commands/provider_connections.rs`, `assistant/repository.rs:460-563`) from SQLite to `AppConfig.provider_connections`.
- Same atomic write pattern as `WorkspaceConfig`.
- `assistant_runs.connection_id` becomes a plain soft-ref string (drop any implicit FK assumption in code paths that load runs).

### Phase 6 — Per-workspace `data.sqlite`

The single largest PR. Worth a sub-doc if it gets unwieldy.

#### Storage move

- Pool map: `HashMap<workspace_id, Arc<SqlitePool>>` held inside `WorkspaceIndex`.
- Move `assistant_sessions`, `assistant_messages`, `assistant_runs`, `assistant_tool_calls`, `workspace_tasks` schema + repository functions into a `WorkspaceDb` abstraction.
- Replace all global-pool calls for these tables with `index.workspace_db(workspace_id).await`.
- Drop `workspace_id` columns and FKs that crossed scopes.
- Drop the corresponding tables from the global DB.

#### Startup recovery — eager fan-out (NOT lazy)

Today (`src-tauri/src/lib.rs:156`, `src-tauri/src/db/mod.rs:976`, `src-tauri/src/assistant/repository.rs:845`) two sweeps run **once** at startup against the **global** pool:

1. `sweep_orphaned_running_state` — marks `workspace_tasks.status = 'running'` rows as failed.
2. `recover_stale_runs` — marks `assistant_runs` and `assistant_tool_calls` stuck in non-terminal states as failed.

If per-workspace DBs were opened lazily (on first UI access), a workspace the user never opens after a crash would keep showing stale "running" rows in the fleet view *forever* — its DB never gets swept because it never gets opened. **Unacceptable.**

The phase 6 PR replaces lazy-open with **eager fan-out at startup**:

```rust
async fn startup_recovery(index: &mut WorkspaceIndex) {
    // Fan-out, bounded parallelism (e.g., tokio::JoinSet with N=8 concurrent).
    for locator in index.all() {
        match open_workspace_db(&locator.root_path).await {
            Ok(pool) => {
                if let Err(e) = run_workspace_migrations(&pool).await {
                    record_load_failure(locator, LoadFailureReason::MigrationFailed(e));
                    continue;
                }
                if let Err(e) = sweep_orphaned_running_state(&pool).await {
                    tracing::warn!(ws = %locator.id, "task sweep failed: {e}");
                }
                if let Err(e) = recover_stale_runs(&pool).await {
                    tracing::warn!(ws = %locator.id, "run sweep failed: {e}");
                }
                index.attach_pool(locator.id.clone(), pool);
            }
            Err(e) => {
                record_load_failure(locator, LoadFailureReason::DbCorrupt(e));
            }
        }
    }
}
```

Properties:

- **Correctness**: every indexed workspace's running state is reconciled before the UI shows the fleet.
- **Scale**: O(N) DB opens at startup. At N=100 this is tens of milliseconds; at N=1000 it's a few hundred. We don't anticipate N>>100 for a single user.
- **Failure isolation**: a corrupt or unopenable DB doesn't block startup of other workspaces — it lands in `index.load_failures` and surfaces in the UI as broken.
- **Pool lifetime**: pools stay open after the sweep. No idle close. SQLite handles are cheap; premature complexity is not worth it.
- **New workspace created at runtime**: `workspace_create` does the same open + migrate + sweep (sweep is a no-op on an empty DB) and inserts the pool into the index.
- **Workspace deleted at runtime**: pool is dropped before the directory is removed.

#### Scheduler interaction

`populate_scheduler_from_workspace_agents` (currently a SQL query in `agents/init.rs:40`) becomes an iteration over `WorkspaceIndex`. The index already caches `schedule_enabled` / `schedule_paused` / `intervalMinutes` per workspace (from parsing `config.json`), so no per-workspace-DB read is needed for population.

When a scheduled agent runs, the runner needs the per-workspace pool to write its session/messages/runs/tool_calls. The runner already has the `workspace_id`; it asks the index for the pool.

### Phase 7 — Cleanup / final cut

- Drop `workspaces`, `workspace_agents` tables from the global DB schema.
- Delete `assistant/tools/workspace_permissions.rs` + remove `.clai/permissions.json` reader.
- Delete the legacy `default` workspace handling in `resolve_workspace_descriptor`.
- Delete dead code: `agent_definition_id` placeholder, `kind` field, `WorkspaceAgentDetail.is_default` (now `id == config.defaultAgentId`).
- Strip `workspace_id` from `AssistantSessionContext` if no remaining consumer needs it (most don't after the moves).
- README + this doc updated to reflect the final shape.

### Estimated PR count

7 commits (phase 0 = doc, phases 1-7 = code). Phases 1, 2, 5 are small (~100-300 LOC). Phases 3, 4, 6, 7 are medium-to-large.

---

## 8. Risks & Follow-ups

- **Cross-workspace queries that don't exist yet.** Today there are none beyond the fleet listing. If we later add "search all my sessions" or similar, it's a fan-out across N DBs. Plan: a `for_each_workspace_db` helper.
- **Bundled skill slugs are now a public API.** Renaming `code-review-checklist` later breaks every `workspace.json` referencing it. We're locked in — document the constraint near the bundled-skill definitions.
- **Stable identity for personal skills.** `personal:<slug>` works as long as the user's personal skill dir layout is stable. If they reorganize, refs break. Tolerable; surface dangling refs in the UI.
- **Workspace dir + symlinks.** Canonicalize paths on load; dedupe; reject ancestor/descendant overlaps so we don't double-scan and false-positive ID collisions.
- **Concurrent writes.** Two paths could try to mutate the same file (UI save + grant flow + manual edit). Mitigated by:
  - In-process per-workspace mutex.
  - Atomic rename pattern.
  - v2: `notify`-based external-change detection + reload prompt.
- **Provider connection deletion races.** Today an in-flight run holds a `connection_id`. With the connection living in JSON, deletion is just an array removal — but a running tool might still need to read it. Keep the existing "soft delete" semantics: removal marks `enabled=false` until no runs reference it, then garbage-collected by a background task. (May be overkill for solo use — could just allow deletion and accept transient errors.)
- **Performance.** Filesystem scan on startup at the author's scale is microseconds. We're not adding caches we don't need.
- **Snapshot semantics during phase 6.** While moving runtime data per-workspace, sessions and tasks live in both old (global) and new (per-workspace) places. The phase 6 PR should do the move + drop in a single commit to avoid that window. Acceptable since "no BC" means we can just wipe.
- **`~/.clai/cache/` is disposable.** Document the "wipe-only-caches" idiom (`rm -rf ~/.clai/cache/`) in the README. App must tolerate a missing cache dir on every startup, not just first run: `materialize_bundled_skills` already does `remove_dir_all` + recreate, and git-source sync re-clones on missing `local_path`. Verify both code paths are robust to a user manually deleting `cache/` while the app is closed.

---

## 9. Open questions

### Resolved (during execution)

- **Schedule scope** — workspace, not per-agent. See D22.
- **`exposed_tools`** — deleted. See D23.
- **`$HOME` RO default scope** — backend-injected via `default_agent_execution()`, applied to manager and every new sub-agent. See D24.
- **Global DB necessity** — none. Eliminated. See D21.
- **Phase 3 export step** — skipped; no migration code in production.
- **Workspace baseline permissions vs per-agent** — per-agent stays. See D25.
- **Manager agent name** — no user-facing name; the workspace title is its own field. See §7.2.
- **Workspace Settings UI shape** — modal with sidebar nav, agent management embedded. See §7.2.
- **Agents drawer fate** — kept as at-a-glance + inline delete; edit/add deep-link into Workspace Settings. See §7.2.

### Still open

- **`AssistantSessionContext.workspace_id` lifetime.** With per-workspace DBs in place, the field is partly redundant (the workspace is implicit by which DB you're in). Audit + possibly drop in a follow-up cleanup pass.
- **Reload UX for external edits.** v1 = none. Acceptable risk; document it in the workspace settings page ("changes to `config.json` while the app is open won't be visible until reopen").
- **Bundled skill slug stability.** Add a CI check that fails if a bundled skill changes its slug?
- **Phase 6 fan-out concurrency.** Currently sequential; doc originally proposed `JoinSet` with 8 concurrent. At the author's scale, sequential is fine — revisit if startup becomes laggy.
- **`load_failures` UX.** Where does the fleet view show "this workspace is broken"? Probably a banner above the card grid, with a "details" expander. Refine when the UI work lands (§7.2).
- **Frontend `workspaceStore.js` + dead `TabBar`/`TabView` components.** Backend stubs for `load_workspace_state` / `save_workspace_state` return empty, so no runtime errors. Ripping them is the next round of frontend cleanup.

---

## 10. References

- Original bug surface (pre-cut): `commands/permissions.rs:262`, `commands/workspace_agents.rs:393`.
- Two-tier comment that no longer applies: `assistant/tools/workspace_permissions.rs:8-10` (module deleted at cutover).
- Path-grant precedent (per-agent persistence): `commands/path_grants.rs:281-341`.
- Runtime union site (deleted at cutover): `assistant/tools/local.rs:907-949`.
- $HOME default helper (new): `config/workspace_config.rs::default_agent_execution()`.
- Workspace-level schedule: `config/workspace_config.rs::WorkspaceSchedule`.
