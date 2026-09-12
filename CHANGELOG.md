# Changelog

All notable changes to CLAI will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
