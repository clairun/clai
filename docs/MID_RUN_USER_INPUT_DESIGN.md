# Mid-Run User Input — Design & Feasibility

**Status:** Investigation / proposal — no implementation yet. Awaiting decision.
**Date:** 2026-06-09
**Author:** investigation notes for review

---

## 1. The problem

When the main agent is working on a task, the user sometimes wants to add a
comment or instruction **while it runs** — to steer or refine the work without
stopping it.

The TODO entry framed this as "we print an error and the user must wait." That
is **stale**: the app no longer errors. Today a mid-run message is *queued* and
delivered as a **brand-new run after the current one finishes**.

The real pain surfaced during review:

> For long-running tasks, queuing a message is useless if it's only sent in the
> next run — possibly 20 minutes later. By then the task is done and the message
> can no longer influence it.

So the goal is **not** "next-turn delivery." It is: **the user's message reaches
the agent *mid-task*, while it is still working, so it can change course.**

---

## 2. What happens today (baseline)

Current behavior is safe and non-blocking — it just doesn't land mid-task.

1. User sends a message while a run is active.
   - `assistant_send_message` checks `get_active_run()` (status in
     `queued | running | waiting_for_tool`).
     `src-tauri/src/commands/assistant.rs:~416`,
     `src-tauri/src/assistant/repository.rs:~697`.
   - If a run is active, the message is **queued** in `assistant_message_queue`
     (status `pending`) and the call returns `queued: true`. No error.
     `src-tauri/src/assistant/repository.rs:~585`,
     migration `migrations/workspace/20260531000000_assistant_message_queue.sql`.
2. The frontend shows a deletable **"Queued"** chip.
   `src/pages/Workspace.tsx` (`queuedMessageIds`, `onDeleteQueuedMessage`).
3. When the run completes, `start_queued_followup_if_idle()` marks the queued
   messages `delivered`, spawns a **new run**, and that new run picks them up.
   `src-tauri/src/commands/assistant.rs:~778`.

### Why it can't reach the live run today

For every CLI provider, clai uses a **one-shot process per turn**:

- It writes the prompt to the child's stdin **once**, then **closes stdin**
  (`drop(stdin)`), and reads the JSONL stream until the process exits.
  `src-tauri/src/assistant/local_agent.rs` — Claude `~729`, Codex `~891`,
  OpenCode `~1070`.
- One run = one process. Once stdin is closed, there is no channel to deliver
  additional input into the running process.

CLI invocation today (`src-tauri/src/assistant/local_agent.rs`):

| Provider | Invocation (one-shot) |
|---|---|
| Claude Code | `claude --output-format stream-json --session-id/--resume … --mcp-config …` |
| Codex | `codex exec resume <thread_id> … -` (trailing `-` = read stdin), `--json` |
| OpenCode | `opencode --pure run --format json --session <id>` |

CLI providers are identified by `is_cli_provider()` in
`src-tauri/src/assistant/providers/cli.rs`; the engine branches to
`local_agent::run_session_turn()` for them (`engine.rs:~102`).

---

## 3. Loop ownership — two cases

The TODO correctly split the problem by **who runs the agent loop**:

- **(a) We own the loop** — direct provider API (built-in adapters, local
  agent). clai drives the tool-call loop itself (`engine.rs:65–389`). Injecting
  a mid-task user message is **easy and low-risk**: between tool iterations,
  check the queue and append the message to the conversation. No subprocess
  protocol involved.
- **(b) We don't own the loop** — Claude Code / Codex / OpenCode. The CLI runs
  its own autonomous tool loop. clai only spawns it, streams its output, and
  services MCP tool calls. To inject mid-task we need each CLI's own
  persistent/streaming mechanism. **This is the hard case and the focus below.**

A 20-minute task in case (b) is **one CLI turn** (one user message → the agent
works autonomously through many tool calls → a final result). "Deliver at the
next turn boundary" therefore means "after the 20 minutes." That is the crux.

---

## 4. Two mechanisms for landing input mid-task

### Mechanism A — true mid-turn steer (no stop)
The message is injected into the **in-flight turn**; the agent folds it into the
work it's already doing. Nothing is interrupted.

### Mechanism B — interrupt + resume + re-inject (brief stop, keeps all work)
Stop the current turn, then immediately **resume the same session** with the
prior context **plus** the new instruction. All completed work (files written,
tool results) is preserved in the session transcript; only the in-flight thought
is cut, and the agent re-plans with the new info. Reuses session-resume, which
clai already does today.

---

## 5. Per-provider capability matrix

| Provider | Mechanism A (mid-turn, no stop) | Mechanism B (interrupt + resume) | Notes |
|---|---|---|---|
| **Claude Code** | ❌ Not available — stream-json input **queues to the next turn**, i.e. after the long task | ✅ via `--resume` + SDK-style interrupt | The weak link: cannot land mid-task without stopping. |
| **Codex** | ✅ `turn/steer` appends to the in-flight turn | ✅ `turn/interrupt` then `thread/resume` | `app-server` mode required (not `exec`). Experimental. |
| **OpenCode** | ✅ mid-session message POST (queue/inject/**pause** modes) | ✅ `POST /session/:id/abort` then re-send | `serve` mode required (not `run`). |
| **Direct API (we own loop)** | ✅ trivial — inject between tool iterations | ✅ trivial | Not a subprocess; easiest case. |

**Headline:** the "without stopping" experience is **only** natively available on
**Codex** and **OpenCode**. On **Claude Code**, the honest best is Mechanism B —
a near-instant pause that keeps all completed work and continues with the new
instruction folded in.

### Provider mode changes required (case b)

| Provider | From (one-shot) | To (persistent, supports mid-run input) | Transport |
|---|---|---|---|
| Claude Code | `--output-format stream-json`, stdin closed | add `--input-format stream-json`, keep stdin open, write NDJSON user messages | stdin/stdout NDJSON |
| Codex | `codex exec resume … -` | **`codex app-server`** (`thread/start`/`resume`, `turn/start`, `turn/steer`, `turn/interrupt`) | stdin/stdout JSON-RPC (NDJSON) |
| OpenCode | `opencode … run …` | **`opencode serve`** (`POST /session`, `POST /session/:id/prompt_async`, `GET /event` SSE, `POST /session/:id/abort`) | HTTP + SSE |

---

## 6. The minimal architectural change

We do **not** need a process that outlives the run. The window where mid-run
input matters is exactly the window the process is already alive. The lifecycle:

1. User sends first message → spawn the process (as today).
2. **Stop closing stdin immediately** (the one-line change at the root of it) —
   keep the input channel open while the agent works.
3. If the user types again mid-run → write it into the live process
   (stream-json / `turn/steer` / mid-session POST).
4. When the agent emits its terminal "done/result" event → close the input
   channel; the process exits. Done.
5. Next user message after the run ends → fresh process, as today.

"Per run, closed on the terminal event" — **not** a long-lived idle process
between turns.

### The end-of-run race
The agent may finish at the same instant the user hits send. Handling: when the
terminal event arrives, check the queue first — if a message is pending, deliver
it as one more turn instead of closing; otherwise close. If we miss it, the
existing queue → new-run path catches it. Both outcomes are safe.

### Code that changes (case b)
- `src-tauri/src/assistant/local_agent.rs` — the per-turn subprocess functions
  become a per-run session loop: don't `drop(stdin)`; add a writer path for
  queued messages; detect the real end-of-run event; protocol handling per
  provider (NDJSON / JSON-RPC / HTTP+SSE).
- `src-tauri/src/commands/assistant.rs` + `repository.rs` — queued messages feed
  the **live** run instead of (or before falling back to) spawning a new run.
- `src-tauri/src/assistant/runtime.rs` — run/session lifecycle, plus a handle to
  the live input channel for delivery.
- Frontend (`src/pages/Workspace.tsx`) — surface "delivered to the running
  agent" vs "queued for next run" so the user knows which happened.

---

## 7. Risk analysis

**Does a new message kill the session?** No. In all three CLIs, input-while-busy
is a handled path, not an edge case:
- Codex `turn/steer` returns a typed error (`ActiveTurnNotSteerable`) when the
  turn can't be steered — an error we read, not a crash.
- OpenCode mid-session POST is an explicit feature with defined modes + an
  `abort` endpoint.
- Claude Code buffers stdin input and processes it at the next turn boundary; it
  does not die.

**Where the real risk is:** our own lifecycle/state-machine code — e.g. closing
stdin while a message is buffered, mis-detecting the terminal event (hang), or
interrupting cleanly while a tool is mid-execution (Mechanism B).

**The fail-safe principle (non-negotiable):**

> Keep today's queue-and-deliver-after-run as the guaranteed fallback. Mid-run
> injection is a best-effort enhancement: if anything fails (steer rejected,
> stream hiccup, version mismatch), degrade to the existing behavior. Worst case
> = exactly what users get today. Never worse.

This is what makes the "experimental/undocumented" caveats tolerable: a future
CLI update can break the *enhancement*, but not the app.

### Relative risk of the two mechanisms
- **Mechanism B is lower architectural risk** despite sounding cruder — it
  reuses session-resume machinery that already works in clai. Main hazard:
  interrupting cleanly mid-tool-call (bounded, testable).
- **Mechanism A is nicer UX but more failure surface** — newer per-provider APIs
  (Codex `turn/steer` experimental; OpenCode `serve` needs an HTTP+SSE client
  with reconnection logic).

---

## 8. Caveats per provider

- **Claude Code:** `--input-format stream-json` works but is **officially
  undocumented** (wire format reverse-engineered; ref GitHub issue #24594). And
  it only queues to the next turn — **no mid-turn injection at all**. Mechanism A
  is impossible here; only Mechanism B.
- **Codex:** `app-server` is documented but marked **Experimental** — method and
  field names may change between versions; pin the Codex version. The lower-level
  `codex proto` (SQ/EQ) protocol exists but is explicitly *not* a stable wire
  contract.
- **OpenCode:** `serve` is documented, but the `GET /event` SSE stream has known
  reconnection bugs (disconnect after sleep; immediate-close in some versions) —
  needs reconnection logic. The exact config keys for the three mid-turn modes
  (queue / inject / pause) were not pinned to a doc line; verify on the installed
  version.

---

## 9. Options for decision

### Option 0 — do nothing more
Keep today's queue → new-run behavior. Cheap. Does not solve the long-task case.

### Option 1 — Mechanism B everywhere (uniform, safer)
Interrupt + resume + re-inject. Works on all providers (incl. Claude Code) via
existing resume machinery. "Sub-second pause that keeps all completed work and
redirects immediately." Not literally non-stop, but solves the 20-minute problem.
Lower architectural risk.

### Option 2 — Mechanism A where supported (Codex/OpenCode), B for Claude
True non-stop steer on Codex (`turn/steer`) and OpenCode (inject/pause); Claude
falls back to Mechanism B. Best UX, but per-provider work and newer APIs (more
flaky-error surface). Claude Code still can't be truly non-stop.

### Option 3 — start with the direct-API (we-own-the-loop) case only
Lowest risk of all: inject between tool iterations for built-in-adapter
providers. Doesn't help the CLI providers (which is where the user feels the
pain), but proves the queue→live-delivery plumbing end-to-end.

### Recommended sequencing (if we proceed)
1. Land the **fail-safe fallback contract** first (it already exists — make it
   explicit and tested).
2. Build **Mechanism B** as a vertical slice on **one** CLI provider behind a
   flag; test the interrupt-mid-tool and end-of-run race paths hard.
3. Evaluate UX. Only then consider **Mechanism A** on Codex/OpenCode for the
   true non-stop upgrade.

Pick the first provider by what you actually use most. If Claude Code is primary,
Option 1 / Mechanism B is the realistic target (A is impossible there).

---

## 10. Open decisions for the reviewer

1. **Is a sub-second "pause → keep all work → redirect immediately" acceptable**,
   or do you specifically need *literally never stops*? (If the pause is OK,
   Option 1 is uniform and safer. If not, we're limited to Codex/OpenCode and
   Claude Code can't participate.)
2. **Primary provider(s)** to target first?
3. **Scope:** ship Mechanism B uniform (Option 1), or invest in Mechanism A for
   Codex/OpenCode (Option 2)?
4. Do we also want the easy **direct-API** injection (Option 3) for built-in
   providers, independently of the CLI work?

---

## 11. Appendix — wire-format references

### Claude Code (NDJSON over stdin; queues to next turn)
```
claude -p --input-format stream-json --output-format stream-json --verbose [--resume <id>]
```
```json
{"type":"user","message":{"role":"user","content":[{"type":"text","text":"…"}]}}
```
Mid-turn message buffers and runs at the next turn boundary. Interrupt is an
SDK-level operation; no documented CLI control message.

### Codex app-server (JSON-RPC 2.0 over NDJSON on stdin/stdout)
```jsonc
{"method":"initialize","id":0,"params":{"clientInfo":{"name":"clai"},"capabilities":{}}}
{"method":"initialized"}
{"method":"thread/start","id":1,"params":{"cwd":"/repo","model":"…","sandbox":"workspaceWrite"}}
{"method":"thread/resume","id":2,"params":{"threadId":"thr_…"}}
{"method":"turn/start","id":4,"params":{"threadId":"thr_…","input":[{"type":"text","text":"Run tests"}]}}
// mid-task:
{"method":"turn/steer","id":5,"params":{"threadId":"thr_…","expectedTurnId":"turn_…","input":[{"type":"text","text":"Focus on the failing tests first"}]}}
// interrupt:
{"method":"turn/interrupt","id":6,"params":{"threadId":"thr_…","turnId":"turn_…"}}
```
Server streams `turn/*` and `item/agentMessage/delta` notifications. `turn/steer`
fails with `ActiveTurnNotSteerable` when not steerable (e.g. during a compaction).

### OpenCode serve (HTTP + SSE)
```
opencode serve [--port 4096] [--hostname 127.0.0.1]
POST /session                         -> { id, … }
POST /session/:id/prompt_async        { "parts":[{"type":"text","content":"…"}] }  // 204, fire-and-forget
GET  /event                           // SSE bus: server.connected, deltas, session.idle
POST /session/:id/abort               // interrupt the running turn
```
Posting a message mid-generation queues/injects/pauses depending on configured
mode. `session.idle` marks the turn boundary. SSE needs reconnection logic.

### Sources
- Claude Code stream-json input (undocumented): GitHub issue #24594; Agent SDK
  streaming-vs-single-mode docs.
- Codex app-server: `openai/codex` `codex-rs/app-server/README.md`;
  developers.openai.com/codex/app-server; SQ/EQ `protocol_v1.md`.
- OpenCode server: opencode.ai/docs/server/; mid-turn message issue #21388;
  `run` teardown race #15267; SSE issues #26697 / #17769.

### Key code locations (clai)
- CLI provider registry / `is_cli_provider`: `src-tauri/src/assistant/providers/cli.rs`
- Engine CLI dispatch: `src-tauri/src/assistant/engine.rs:~102`
- CLI turn execution + stdin write/`drop`: `src-tauri/src/assistant/local_agent.rs`
  (Claude `~729`, Codex `~891`, OpenCode `~1070`); `prepare_prompt` `~1296`
- Send + queue: `src-tauri/src/commands/assistant.rs:~407`; followup `~778`
- Queue queries: `src-tauri/src/assistant/repository.rs:~547,~697,~719`
- Queue schema: `migrations/workspace/20260531000000_assistant_message_queue.sql`
- Frontend queued chip: `src/pages/Workspace.tsx` (`queuedMessageIds`)
