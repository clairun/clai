# CLI-Backed Agent Integration RFC

## Status
Draft — design complete, awaiting Phase 0 spike to confirm the streaming +
tool-name assumptions hold in practice. Once the spike passes, Phase 1 can
start immediately.

## Context

CLAI today drives agents via direct API calls through `ProviderAdapter`
implementations (`src-tauri/src/assistant/providers/{anthropic,openai}.rs`).
The engine owns the tool loop in `src-tauri/src/assistant/engine.rs`: send
messages → stream events → execute tool calls → repeat.

This works well for users who hold API keys, but most subscription tiers
(Claude Pro/Max, ChatGPT Plus, etc.) only expose their models through the
vendor's CLI, not the API. Users on a flat-rate subscription cannot pay
per-token for the same models a second time. We want them to be able to
use CLAI with their existing subscription by delegating execution to a
local CLI agent — Claude Code, Codex, or OpenCode — while keeping the rest
of CLAI's surface (UI, tool registry, workspace tasks, inter-agent calls,
permissions) intact.

A prior iteration of this repo wrapped these CLIs directly. Two real
problems killed it:

1. **No live conversation visibility.** Spawning the CLI per turn and
   reading rendered stdout meant CLAI saw only the final string. No
   incremental rendering, no tool-call timeline.
2. **Perceived latency.** Subprocess cold start (~200–500ms for Node-based
   CLIs) on every turn made short answers feel sluggish.

Both symptoms trace to the same root: the integration used the CLI's
*human-rendered* output mode and a fresh process per turn. Both
disappear when we commit to (a) the CLI's structured streaming output and
(b) a single long-lived CLI process per session.

## Goal

Let users associate a CLI binary (Claude Code, Codex, OpenCode) with any
agent, in addition to API-backed provider connections, such that:

- The agent's behavior in CLAI's UI is indistinguishable from an API
  agent: text streams in real time, tool calls show in the timeline,
  thinking is rendered, cancellation works, the transcript persists.
- The CLI uses CLAI's tool registry — bash, file ops, workspace tasks,
  inter-agent calls, MCP-routed external tools — via an MCP server that
  CLAI hosts internally. The CLI's own built-in tools are disabled.
- Permissions (`workspace_permissions`, `command_splitter`, path gating)
  apply uniformly regardless of which engine called the tool.
- Auth lives with the CLI. CLAI never sees subscription tokens.

## Core architectural decision

**Treat the CLI as a delegating agent runtime, not a completion provider.**

The existing `ProviderAdapter` shape (single `stream_completion` call,
CLAI re-invokes per tool turn) does not fit CLIs — they run their own
tool loop internally. A new `LocalAgentRuntime` runs alongside the
existing engine. The runtime:

- Spawns one CLI subprocess per `AssistantSession`, kept alive across
  turns.
- Drives the CLI via its structured streaming protocol (NDJSON stdio for
  Claude Code / Codex, HTTP+SSE for OpenCode).
- Disables every built-in CLI tool. Exposes CLAI's tools via an
  embedded MCP server the CLI connects to over loopback HTTP.
- Maps the CLI's stream events to the same `AssistantUiEvent`s the API
  engine emits, so the UI is engine-agnostic.

## Architecture

### 1. MCP server inside CLAI

CLAI already depends on `rmcp = "0.12"` with both client and
`transport-streamable-http-server` enabled (`src-tauri/Cargo.toml`). The
infrastructure to host an MCP server exists; only wiring is missing.

- Bind one HTTP MCP endpoint to `127.0.0.1:<random>` at app startup.
- Per session, issue a bearer token bound server-side to
  `{session_id, run_id, AppHandle}`. The handler resolves token →
  `ToolExecutionContext` and dispatches into the existing
  `tools::router::execute_tool`. No parallel tool registry, no
  re-implemented permissions.
- The token lives only in memory + a `chmod 600` temp `.mcp.json` the
  CLI subprocess reads. Rotated per session. Never logged.

### 2. Tool surface mirror

Every tool in `src-tauri/src/assistant/tools/` is exposed as one MCP
tool. The JSON schemas already exist
(`ToolDefinition.input_schema`); the mapping is mechanical:

| CLAI tool            | MCP tool name (as seen by CLI)     |
| -------------------- | ---------------------------------- |
| `bash`               | `mcp__clai__Bash`                  |
| `read_file`          | `mcp__clai__Read`                  |
| `write_file`         | `mcp__clai__Write`                 |
| `edit_file`          | `mcp__clai__Edit`                  |
| `glob`               | `mcp__clai__Glob`                  |
| `grep`               | `mcp__clai__Grep`                  |
| `inter_agent_call`   | `mcp__clai__InterAgentCall`        |
| `workspace_task_*`   | `mcp__clai__WorkspaceTask*`        |
| (JS-bridge UI tools) | `mcp__clai__Notify`, etc.          |
| External MCP tools   | passed through with original names |

Tool names intentionally match what the CLI's underlying model was
trained on (`Bash`, `Read`, etc.) to minimize tool-name friction. The
`mcp__clai__` prefix is added by the CLI's MCP client and is not under
our control — the spike must confirm the model still selects these
tools as readily as native ones.

### 3. Long-lived CLI process per session

One CLI subprocess per `AssistantSession`, alive while the session is
open. Lifecycle:

- **Start**: first user turn spawns the CLI with stream-json mode,
  `--session-id <uuid>`, `--mcp-config <temp path>`, and the full
  built-in-tool-disable flag.
- **Subsequent turns**: feed user messages over stdin (CLIs that
  support `--input-format stream-json`) or via the daemon's HTTP API
  (OpenCode).
- **Cancellation**: graceful abort signal if the CLI supports one,
  then SIGTERM after a short timeout. MCP server sees the connection
  drop; in-flight tool calls honor the existing `CancellationToken`.
- **Crash**: surface an error, mark the run failed, respawn on next
  user message with the same `--session-id` to recover model-side
  context. Repeated crashes mark the connection unhealthy.
- **Session close**: terminate the subprocess, clean up the
  `.mcp.json`, drop the token from the server's binding table.

### 4. Stream parsing

Each CLI gets a small parser that maps its NDJSON/SSE events to
`AssistantUiEvent`:

| CLI event                      | CLAI event                                     |
| ------------------------------ | ---------------------------------------------- |
| text delta / `content_block`   | `AssistantDelta { text }`                      |
| thinking / reasoning delta     | `AssistantThinkingDelta { text }`              |
| tool_use start                 | timeline placeholder (corroborated by MCP)     |
| tool_use complete              | finalize timeline entry                        |
| usage / tokens                 | `RunUsage` update                              |
| result / message_stop          | `AssistantMessageCompleted`                    |
| error                          | `RunFailed`                                    |

The actual tool *execution* never appears in this stream — it flows
through the MCP server and goes straight into CLAI's existing
`ToolInvocation` rows + `AssistantUiEvent::ToolInvocation*` events.
The CLI's tool_use event is purely a "hint" we use to render the
timeline entry slightly earlier; the source of truth for tool calls
remains CLAI's MCP handler.

### 5. Conversation continuity

Two state stores must stay in sync:

- **CLAI's `AssistantMessage` log** — what the UI shows, what
  inter-agent callers see, what gets backed up. **Canonical for humans.**
- **CLI's session** (referenced by `--session-id`) — what the model
  sees on subsequent turns. **Canonical for the model.**

Each turn the stream parser writes assistant text/thinking/tool_use
deltas into the CLAI message exactly as the API engine does. The two
stores agree by construction because both are written from the same
event stream.

Backend switching (API ↔ CLI mid-session) is supported by replaying
CLAI's transcript as the first user message of the new CLI session
(one big "here's what we did so far" prompt). One-way friction:
API→CLI pays a token cost on switch; CLI→API is free because the API
engine reads CLAI's log directly.

### 6. Permission flow

Unchanged. The MCP tool handler calls `tools::router::execute_tool`,
which runs `workspace_permissions` + `command_splitter` exactly as
today. Denial → `RunNotice::CommandDenied`/`PathDenied` emitted, MCP
returns a tool error, CLI's model sees "tool failed: <reason>" and
reasons around it.

## Per-CLI specifics

### Claude Code (first target — cleanest streaming surface)
```
claude \
  -p "<user message>" \
  --output-format stream-json \
  --input-format stream-json \
  --session-id <uuid> \
  --mcp-config <temp .mcp.json> \
  --disallowedTools "Bash,Read,Edit,Write,Glob,Grep,WebFetch,Task,TodoWrite,NotebookEdit,NotebookRead"
```
WebSearch left enabled (safe, no system access). Resume via
`--session-id`. Long-lived via `--input-format stream-json` keeping
stdin open.

### Codex
`codex exec --json --session-id <uuid>` with MCP config in
`~/.codex/config.toml`. Built-in tool disable mechanism requires
audit during Phase 4 — codex's tool model is more opinionated than
Claude Code's.

### OpenCode
HTTP daemon mode + SSE. One daemon per CLAI session, MCP server
configured via the daemon's API. Most flexible of the three;
implement last because the protocol design lets us learn from the
first two.

## Data model changes

Minimal — the existing schema already accommodates most of this.

- `AuthMode::SubscriptionLogin` exists
  (`src-tauri/src/assistant/types.rs:21`). Wire it for new provider IDs.
- New `provider_id` strings: `claude-code`, `codex`, `opencode`.
- `ProviderConnection`: store optional `cli_path` override (reuses
  the `base_url` slot or adds a `binary_path` field — decision in
  Phase 3).
- `AssistantSession.context`: add `cli_session_id: Option<String>`
  alongside the other context fields. Persisted on first turn,
  reused on subsequent turns.
- `ProviderDescriptor`: add a flag distinguishing API providers from
  CLI providers so the registry/UI can branch on setup flow.

No DB migration needed beyond the new optional column for
`cli_session_id`.

## UI changes

- **Connection setup wizard** for CLI providers:
  1. Detect the binary on PATH; allow manual override.
  2. Verify auth (`claude --version` + a tiny smoke prompt; equivalent
     for codex/opencode). If unauthenticated, modal explains how to
     run `claude /login` (or equivalent) in the user's terminal,
     "Click Continue when done."
  3. Run a one-shot MCP bridge test with a trivial tool call to
     confirm end-to-end works.
- **Agent picker**: CLI-backed agents get a badge ("via Claude Code
  (subscription)") so users know which engine they're using.
- **Error surfacing**: distinct UI states for rate-limit-hit,
  auth-expired, CLI-version-too-old, MCP-handshake-failed.
- **Settings**: per-CLI version pin + "test connection" button.

## Risks and open questions

1. **Tool-name training penalty** *(the single largest risk)*. Claude
   and other models are trained to reach for tools named exactly
   `Bash`/`Read`/`Edit`. When those become `mcp__clai__Bash`, etc.,
   the model may be slower or less consistent. **Phase 0 spike
   acceptance criteria**: on a fixed 20-turn benchmark, the
   MCP-only variant must complete in ≤120% of the time and ≤120% of
   the tool calls of native-tools Claude Code on the same prompts.
   If it fails badly, fall back to per-CLI built-in tools + a
   thin bridge for the CLAI-specific tools (`InterAgentCall`,
   `WorkspaceTask*`).

2. **Cannot fully disable all built-ins.** Some CLIs may have
   hardcoded tools (Claude Code's `TodoWrite`, codex's internal
   shell). Audit per CLI in Phase 0/4/5. If a tool can't be
   disabled, document the divergence and decide whether to ignore
   it or work around it.

3. **Localhost MCP server attack surface.** Full filesystem + bash
   exposed over loopback HTTP. Mitigations: bind strictly to
   `127.0.0.1`, never `0.0.0.0`; per-session bearer tokens; tokens
   never logged or persisted in world-readable files; rotate per
   session. On shared multi-user machines, any local UID-shared
   process can reach the endpoint — document this caveat.

4. **CLI version churn.** `stream-json` schemas evolve. Pin minimum
   versions; fail loud with an actionable error when the parser
   sees an unrecognized event type.

5. **MCP tool results are unary.** A long-running bash command
   can't stream incremental output back to the CLI's model — same
   limitation that exists today on the API path. Worth noting; not
   blocking.

6. **Subscription rate limits.** When the CLI returns
   "you've hit your usage limit," translate to a specific
   `RunStatus::Failed` variant with a user-friendly message and an
   ETA if the CLI provides one.

7. **Token telemetry.** Subscription mode means `RunUsage` is
   partial or absent. UI must handle "usage unknown" without
   breaking cost dashboards.

8. **Backend switching loses inter-agent live state.** If agent A
   (API) calls agent B (CLI) via `inter_agent_call`, B's session
   is fresh each call unless we cache the cli_session_id by
   `(caller_session, callee_agent)`. Decide caching policy in
   Phase 2.

9. **Multi-window CLAI.** If a user opens two CLAI windows, both
   want to host an MCP server on localhost. Either share one
   server across windows (process singleton) or bind to different
   ports. Decision in Phase 1.

## Phases

### Phase 0 — De-risking spike (1–2 days, **gate**)
Goal: validate streaming and tool-name assumptions before building.

Deliverables:
- Minimal rmcp HTTP server with one tool (`Bash`).
- Launch Claude Code with `--mcp-config`, `--disallowedTools`,
  `--output-format stream-json`, `--input-format stream-json`.
- Measurements on a fixed 5-prompt set:
  - Cold-start TTFT.
  - Warm (long-lived process) TTFT.
  - Time-between-deltas vs. direct API.
  - End-to-end overhead vs. direct API on a 10-Bash-call turn.
  - Tool-selection consistency on the 20-turn benchmark.

**Gate**: if warm TTFT is within 200ms of API, deltas feel native,
and tool selection lands within 120% / 120%, proceed. Otherwise,
revisit the architecture (likely falling back to per-CLI tool
allowlists + thin bridge).

### Phase 1 — MCP server inside CLAI (3–5 days)
- rmcp streamable-http server, bound to `127.0.0.1:<random>`.
- Per-session bearer token + binding table
  `token → {session_id, run_id, AppHandle}`.
- MCP tool definitions for every tool in `tools::router`. Reuse
  existing JSON schemas. Permission gate unchanged.
- Token issuance / rotation / cleanup on session close.
- Tests: dispatch through MCP equals dispatch through the
  in-process router for a representative tool set.

### Phase 2 — Claude Code adapter end-to-end (5–7 days)
- New `LocalAgentRuntime` module, separate from `engine.rs`.
- Subprocess lifecycle: spawn, stdin pump, stdout NDJSON parse,
  stderr log, SIGTERM-on-cancel, respawn-with-session-id on crash.
- Stream parser: Claude Code's stream-json → `AssistantUiEvent`
  family. Golden-fixture tests with recorded stream-json captures.
- `cli_session_id` persisted on `AssistantSession.context`.
- Cancellation cascade integrated with existing
  `CancellationToken`.
- Backend switching: replay-on-switch logic for the first turn
  after engine change.

### Phase 3 — Data model + UI (4–6 days)
- New provider IDs (`claude-code`) + descriptor flag
  `is_cli_backed`.
- DB migration: `cli_session_id` column (nullable) on
  `assistant_sessions`.
- Connection setup wizard (binary detection, auth probe, MCP
  smoke test).
- Agent picker badge + per-engine error surfaces (rate limit,
  auth expired, version mismatch).
- Settings panel: cli binary override + "test connection" button.

### Phase 4 — Codex adapter (4–6 days)
- Codex `--json` event parser.
- Audit and implement built-in tool disable mechanism (may need
  config-file write rather than CLI flag).
- Reuse Phase 2's `LocalAgentRuntime` lifecycle; only the parser
  and spawn flags differ.
- Surface codex-specific error modes.

### Phase 5 — OpenCode adapter (4–6 days)
- Daemon-mode driver: launch once per session, speak HTTP+SSE.
- MCP config injection through opencode's API.
- Lifecycle differs (long-lived daemon, not stdin-pumped
  subprocess) — generalize Phase 2's runtime if needed.

### Phase 6 — Hardening + polish (3–5 days)
- Cross-platform binary detection (PATH search + common install
  locations on macOS/Linux/Windows).
- Version pinning + clear upgrade prompts when the CLI is too
  old.
- Telemetry: success/failure rates per CLI, latency histograms,
  rate-limit-hit counts.
- Documentation: end-user docs explaining the subscription path,
  caveats, and how to authenticate.
- Edge cases: CLAI quit mid-turn (kill CLI cleanly), CLI auth
  expires mid-session, MCP server port collision after laptop
  sleep/wake.

## Out of scope (for now)

- Vendor CLIs not in the initial three (Gemini CLI, Aider, etc.) —
  same architecture applies; add when there's user demand.
- Sharing one CLI subprocess across multiple CLAI sessions —
  one-per-session keeps cancellation and crash isolation clean.
- Exposing CLAI's MCP server to external clients on the network —
  loopback only.
- Mid-turn engine switching — only allowed at turn boundaries.
- Cost reconciliation for subscription users — usage data is
  partial; we render what the CLI tells us, no more.

## Success criteria

The integration is considered shipped when:

1. A user can add a Claude Code connection, authenticate via
   `claude /login`, and assign it to an agent.
2. That agent answers in CLAI with streaming text indistinguishable
   in feel from an API agent.
3. The agent uses CLAI's tools (Bash, Read, etc.) — every tool call
   appears in CLAI's timeline, is gated by CLAI's permissions, and
   is auditable in the run log.
4. Inter-agent calls and workspace tasks work from a CLI-backed
   agent.
5. Cancellation, retry, and crash recovery work without losing
   conversation state.
6. Codex and OpenCode reach the same bar.
