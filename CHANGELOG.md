# Changelog

All notable changes to CLAI will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **Agents are shared across workspaces.** Settings gains an *Agents* tab: a
  library of teammate definitions — instructions, skills, providers, MCP
  selection, shell policy — that any workspace can add to its team. One edit
  reaches every workspace using that agent, from its next turn. Each workspace
  still owns its *Main* agent outright, along with its history, schedule,
  memory and files, and adds the strictly local parts of a teammate itself:
  whether it is enabled here, the context it works under, and the paths it may
  touch. A workspace-level *Team* section carries project context and the
  grants every agent here receives.
- **Approvals are saved where they belong.** An "always allow" for a command is
  saved on the shared agent, because the judgement is about the agent. A path
  grant approved during a run is saved on the local assignment, because the
  judgement is about this machine and this project — approving it in one
  workspace never widens access in another.

- **The pre-library config is kept.** The first save after upgrading copies the
  old `config.json` to `config.pre-agent-library.json` in the same folder, once.
  Teammates from the flat `agents` array are not migrated — a shared agent is a
  deliberate act — so the file you need in order to re-create them stays
  readable instead of being overwritten by the first workspace you open.

### Fixed

- **A delegated task no longer inherits the caller's MCP servers.** Task setup
  preferred the caller's session selection when it had one, handing a worker
  servers its own settings never granted. It resolves its own tools now.
- **A read-only grant can no longer take away write access it sits inside.**
  Grants compose additively, but the backends disagreed about a nested pair:
  Linux binds shallowest-first so a read-only `/srv/data/docs` under a
  read-write `/srv/data` revoked write on `docs`, while macOS kept it. The
  nested read-only entry is now dropped before the profile is built, on every
  platform — including inside the workspace root, which is read-write by
  definition. A workspace you granted explicitly is no longer swallowed by the
  mask that hides the others: on Linux it is now mounted after that mask rather
  than before it, so the grant survives, as it already did on macOS.

### Changed

- **An "always" decision for a shared agent says so.** Allow and deny both save
  on the shared definition, so the approval card now reads "(every workspace)"
  for a teammate and names the workspaces it reaches — a command allowlist skips
  the prompt entirely next time, so the scope has to be visible while deciding.
- **Agent templates are gone.** The two embedded templates and their
  `agent_templates_list` command are removed; a shared agent created once in the
  library is the reusable thing they were approximating. Bundled skills are
  unaffected. This also retires the `sow-tracker` template, which had been
  requesting a skill that no longer exists.
- **A workspace config now stores a `mainAgent` and its assignments** instead of
  a flat `agents` array with a `defaultAgentId` pointer. Configs written before
  this recover exactly the agent that pointer selected, keeping its id, policy
  and history; other agents in the old list are ignored, and shared teammates
  are re-created deliberately. An ambiguous or unreadable selection leaves the
  workspace in its normal setup state with files and history intact.
- **`workspace_set_default_agent` is removed.** A workspace has one Main and it
  is edited in place; pointing it at a shared teammate would have dragged that
  teammate's other workspaces along.

### Code quality

- **The Rust crate now has a lint policy.** `Cargo.toml` carries an explicit
  `[lints]` block and `clippy.toml` states the complexity budgets (100 lines,
  cognitive complexity 25), so the thresholds are reviewable instead of
  inherited silently. CI lints `--all-targets`, which brings test code
  under the same rules for the first time.
- **Signatures that lied have been corrected.** Eight helpers were `async`
  with nothing to await, and five returned a `Result`/`Option` that was always
  `Ok`/`Some` — one of which had grown a fallback branch that could never run
  (`compose_agent_instructions`). Callers no longer pay for an unreachable
  error path or a pointless future.
- **The unused `workspace_write_file` command is gone**, along with its
  request type, its path resolver, and the `writeWorkspaceFile` wrapper in the
  frontend: nothing in the app had called it since the artifact panel landed.
  A new `ipc_surface` test now fails the build when the two name lists
  disagree: a registered Tauri command that no `invoke()` names, or an
  `invoke()` naming a command that does not exist — the second case used to
  reach users as a silent runtime `Command not found`. It does not catch a
  wrapper that still invokes but has no importers; that is unused-export
  analysis and belongs on the TypeScript side.
- **`tower` is no longer a direct dependency**; it was declared but never
  named in the code.

### Codex

- **Codex now uses `codex app-server` by default.** This enables live mid-turn
  steering for queued user messages. Set `CLAI_CODEX_APP_SERVER=0` (also
  accepts `false`, `no`, `off`, or an empty value) to fall back to legacy
  `codex exec`.

### Filesystem tools

- **The `fs_list`, `fs_glob`, `fs_read`, and `fs_write` tools are gone.**
  Agents do their file work through `bash_exec` instead, which is one
  permission surface rather than two and gives an agent the whole shell
  vocabulary (`rg`, `sed`, `find`, pipelines) where it previously had four
  fixed verbs. `fs_request_grant` stays: it is how an agent asks for a path
  outside its grants. Past conversations still render the removed tool calls.
- **Restricted allowlists gained the file commands.** The default restricted
  allowlist now includes what the removed tools used to do (`mkdir`, `cp`,
  `mv`, `touch`, `tee`, `sed`, and friends), and new agents default to the
  restricted tier, since the shell is now also the filesystem.
- **Existing agents are left alone.** There is no config migration: an agent
  you already configured keeps exactly the allowlist you gave it. The first
  time it needs `mkdir` or `tee`, Restricted mode asks, and "Always allow"
  widens the list — so the commands arrive when they are actually used, by
  your decision, instead of being written into your config on upgrade.
- **Prompt guidance follows real capability.** An agent with no shell, or with
  an allowlist containing no command that writes a file, is no longer told to
  keep memory files, save durable outputs, or write companion data files for
  charts — guidance it could only have stalled on. It is told what it can do
  instead: report in chat, and save charts with `create_vega_chart`, which
  writes its own file without a shell.

### Shell permissions

- **Restricted mode is now interactive.** When an agent in Restricted
  shell-access mode runs a command that isn't in its allowlist, the user
  is prompted instead of getting a silent denial. Each pipeline segment
  is shown separately with a smart-prefix suggestion the user can edit;
  decisions are per-segment (Allow once / Always allow / Deny once /
  Always deny).
- **Pipeline-bypass closed.** Allowlist matching now evaluates each
  pipeline segment independently (split on `|`, `||`, `&&`, `;`, `&`,
  `|&`, newline). A saved `git status` prefix no longer auto-approves
  `git status | rm -rf ~/` — each segment is its own decision.
- **Smart prefix suggestion.** Per-CLI rules give sensible defaults:
  `kubectl logs my-pod` → `kubectl logs`; `kubectl get pods` →
  `kubectl get pods`; `aws ec2 describe-instances` → keeps the
  hyphenated verb; `cat /etc/hosts` → just `cat`.
- **Opaque segments require fresh approval.** Substitutions (`$(…)`,
  backticks), executors (`bash -c`, `xargs`, `eval`), redirects, and
  control flow can't be safely allowlisted, so each invocation prompts.
- **Workspace permissions file.** Each agent's persistent allow/block
  lists live in `<workspace_root>/.clai/permissions.json` — plain JSON
  designed to be committed to git so permissions travel with the
  workspace.
- **Migration.** Existing per-agent allow/blocklist entries containing
  shell separators are split into per-segment entries on first launch
  (idempotent; unknown fields preserved).

### Added
- Initial beta release
- Desktop app for building, running, and supervising small teams of AI agents
- Workspace-local agent teams — add helper agents, each with their own
  prompts, skills, MCP servers, providers, and execution policy; the main
  agent delegates to them as tools
- Fleet view for supervising every workspace, including scheduled runs and
  tasks that need attention, with live chat previews
- MCP-native tools, configured once and attachable per workspace or per
  agent (HTTP and stdio transports)
- Multiple providers — OpenAI-compatible and Anthropic-compatible API
  connections, plus local CLI agents (Claude Code, OpenAI Codex, OpenCode)
- Local execution sandbox — per-agent filesystem grants and three shell
  modes (Off, Restricted, Full)
- Inspectable tasks — delegated work streams a live transcript of the helper
  agent's conversation, tool calls, and verdict
- Memory & artifacts persisted to the workspace directory, with read-only
  previews in the drawer
- Scheduled (periodic) workspaces that run the main agent on an interval
- Default skills and agent templates (`code-reviewer`, `sow-tracker`)
- Run notices that surface policy denials instead of failing silently
- Chat interface with markdown rendering
- Conversation history and context management
- Light and dark themes
- Cross-platform support (Windows, macOS, Linux)
- Permission-based capabilities system

### Technical
- Built with Tauri 2.0, React 19, and Vite
- Platform-specific styling (macOS, Windows, Linux)
- Responsive resizable panels
- Syntax highlighting for code blocks
