//! System prompt assembly.
//!
//! `build_system_prompt` renders the full system message handed to every
//! provider — API and CLI alike — from the session context, the live agent
//! description, the tool set, and the run trigger. It lived in `engine.rs`
//! until it grew to roughly a third of that file (R3.4); it has no dependency
//! on the run loop, so it lives on its own.
//!
//! `live_agent_description` comes along because it exists only to feed the
//! prompt and both call sites take the two as a pair. It is the sole reason
//! this otherwise pure module needs `AppHandle`/`AppState`.

use tauri::AppHandle;
use tauri::Manager;

use crate::assistant::types::{ContentPart, MessageRole, ProviderInputMessage, RunTrigger};
use crate::AppState;

/// Resolve the live agent description for the session's owning agent.
///
/// Returns the user-set description plus assembled skill content read fresh
/// from disk on each call. `None` when the session has no workspace/agent
/// binding, AppState is unavailable, or the workspace/agent has been
/// deleted — all "no agent instructions" scenarios.
pub(crate) fn live_agent_description(
    app: &AppHandle,
    context: &crate::assistant::types::SessionContext,
) -> Option<String> {
    let workspace_id = context.workspace_id.as_deref()?;
    let agent_id = context.automation_id.as_deref()?;
    let state = app.try_state::<AppState>()?;
    crate::commands::workspace::workspace_agent_runtime_description(
        state.inner(),
        workspace_id,
        agent_id,
    )
}

/// Build the system prompt for the assistant.
///
/// `agent_description` is the live-computed seed (user-set description plus
/// resolved skill content) for the agent owning this session. It is NOT
/// persisted on the session — callers re-derive it at turn start so toggling
/// a skill or editing a description is immediately visible to the model.
/// Pass `None` only for sessions that have no associated agent (e.g. tests,
/// or sessions whose underlying agent has been deleted).
pub(crate) fn build_system_prompt(
    context: &crate::assistant::types::SessionContext,
    agent_description: Option<&str>,
    tool_defs: &[crate::assistant::types::ToolDefinition],
    trigger: &RunTrigger,
) -> ProviderInputMessage {
    let tool_names: Vec<&str> = tool_defs.iter().map(|t| t.name.as_str()).collect();
    let current_datetime = chrono::Local::now()
        .format("%Y-%m-%d %H:%M:%S %:z")
        .to_string();

    let mut prompt = String::from(
        "You are CLAI, a workspace assistant and multi-agent orchestration tool built into a desktop app. \
         You help users inspect available capabilities, choose the right tools for the job, update the workspace, \
         and explain outcomes clearly.\n\n",
    );

    prompt.push_str(&format!(
        "Current local date and time: `{}`.\n\n",
        current_datetime
    ));

    // Role-identity callout: if this session belongs to the workspace's
    // default manager AND there are non-manager members in the team,
    // put a short "you are the manager, here are your members" header
    // ABOVE the tool list. Without this, LLMs frequently hallucinate
    // their own toolset on the first turn ("I don't have reviewer
    // agents available") despite the team being listed lower down in
    // the prompt. Placing role identity first keeps the model from
    // framing itself as a solo assistant.
    let own_agent = context
        .workspace_agents
        .iter()
        .find(|a| a.is_default && Some(a.id.as_str()) == context.automation_id.as_deref());
    let member_agents: Vec<&crate::assistant::types::WorkspaceAgentSummary> = context
        .workspace_agents
        .iter()
        .filter(|a| !a.is_default)
        .collect();
    if let Some(own_agent) = own_agent {
        if !member_agents.is_empty() {
            prompt.push_str(
                "## Your Role\n\
                 You are the **manager** of this workspace. The user talks to you; you decide how the work gets done. \
                 You have member agents available for delegation via `workspace_assignTask` — prefer delegating specialized work to them over doing it yourself, then poll `workspace_getTaskResult` for the outcome. \
                 The roster below is your team; you do not need to call `workspace_listAgents` to confirm it. \
                 Each entry carries the `workspaceAgentId` that `workspace_assignTask` expects: it matches on that id, never on the display name.\n\n\
                 Member agents you can delegate to:\n",
            );
            for agent in &member_agents {
                let summary = agent
                    .description
                    .as_deref()
                    .filter(|d| !d.trim().is_empty())
                    .unwrap_or("(no description)");
                prompt.push_str(&format!(
                    "- **{}** ({}) — `workspaceAgentId`: `{}` — {}\n",
                    agent.display_name, agent.role, agent.id, summary
                ));
            }
            // Self-tasking is offered below as the way to parallelize work, so
            // the manager needs its own id here too — not left to be inferred
            // from the roster further down the prompt. Suppressed in a worker
            // session for the same reason the fan-out mechanics are: a worker
            // must not be handed the argument for spawning task chains.
            if !matches!(trigger, RunTrigger::WorkspaceTask) {
                prompt.push_str(&format!(
                    "\nYour own `workspaceAgentId` is `{}` — use it to assign a task to yourself.\n",
                    own_agent.id
                ));
            }
            prompt.push('\n');
        } else {
            prompt.push_str(
                "## Your Role\n\
                 You are the **manager** (and only agent) of this workspace. The user talks to you; you decide how the work gets done.\n",
            );
            // Self-tasking — the offer and the id it needs — is withheld from
            // a worker session, which the Task Worker Context below forbids
            // from spawning chains.
            if !matches!(trigger, RunTrigger::WorkspaceTask) {
                prompt.push_str(&format!(
                    "There are no member agents to delegate to, but the task tools still work: you can assign a task to *yourself* to run another instance of you in the background. \
                     Your own `workspaceAgentId` is `{}` — pass it verbatim; `workspace_assignTask` matches on the id, never on a display name.\n",
                    own_agent.id
                ));
            }
            prompt.push('\n');
        }

        // Delegation mechanics — skipped when this manager session *is* a
        // task worker (a self-assigned task): the Task Worker Context block
        // below carries the don't-spawn-chains guidance instead, and
        // advertising fan-out there would invite recursive task spawning.
        if !matches!(trigger, RunTrigger::WorkspaceTask) {
            prompt.push_str(
                "### How tasks run\n\
                 - `workspace_assignTask` is asynchronous: it returns a task id immediately and the task runs in its own separate session, in parallel with you. Keep working while it runs; poll `workspace_getTaskResult` for the outcome.\n\
                 - Lifecycle invariant: after you assign any workspace task, you own that task until it reaches a terminal status (`completed`, `failed`, or `blocked`). NEVER send a final response or let the main agent end while a task you spawned is still `queued` or `running`.\n\
                 - Before your final response, check every task id you assigned in this run. If any are still non-terminal, continue polling instead of ending. If progress is unclear, use the task's `sessionId`/`runId` from `workspace_getTaskResult` with `history_query` to inspect the subagent transcript/tool calls and decide whether it is progressing, stalled, hung, or blocked. If a task fails or blocks, integrate that terminal status into your final response.\n\
                 - Tasks run concurrently with no per-agent limit. Fan out independent subtasks freely — several tasks for the *same* agent at once is fine.\n\
                 - Assigning a task to yourself is the supported way to parallelize work while you stay responsive, but the same lifecycle invariant applies: do not finish the assigning run before the task finishes.\n\
                 - A task worker does NOT see this conversation. Write self-contained instructions: include the goal, the relevant file paths, and any context it needs.\n\
                 - All tasks share this workspace's directory. Partition parallel work so concurrent tasks don't write the same files.\n\
                 - Record task ids in memory (e.g. `.clai/memory/state.md`) only as crash/recovery insurance, not as permission to end the main run with tasks still in flight.\n\n",
            );
        }
    }

    if matches!(trigger, RunTrigger::WorkspaceTask) {
        prompt.push_str(
            "## Task Worker Context\n\
             This session is a background task worker: you were spawned via `workspace_assignTask` to complete one bounded task, running in parallel with the agent that assigned it (possibly another instance of yourself). \
             Your final assistant message is captured as the task's result summary — make it a concise, self-contained outcome. \
             Do not assign further tasks from here unless your instructions explicitly require fan-out; never create task chains or loops.\n\n",
        );
    }

    if !tool_names.is_empty() {
        prompt.push_str("You have the following tools available:\n");
        for td in tool_defs {
            prompt.push_str(&format!("- `{}`: {}\n", td.name, td.description));
        }
        prompt.push('\n');
    }

    // Tool usage guidance
    prompt.push_str(
        "## Tool Usage Guidelines\n\
         - First inspect what is available in this session and choose the smallest set of tools needed.\n\
         - Use the configured MCP tools available in this session for domain-specific work.\n\
         - Use exposed CLAI tools such as `fs_list`, `fs_read`, `fs_write`, `fs_glob`, and `bash_exec` only when those local execution capabilities are available in this session.\n\
         - Prior tool outputs in the conversation may be stale. Treat them as historical context, not guaranteed current state.\n\
         - Evaluate whether prior tool outputs are still fresh enough for the current decision. When information can expire or change over time (for example issues, alerts, metrics, repo state, or external system status), re-run the relevant tools if freshness matters.\n\
         - Chat is the default output channel. Use normal assistant replies for status, findings, and conclusions.\n\
         - When looking for code, files, or prior work, ALWAYS search your workspace first — `fs_glob`/`fs_list` from the workspace root, or a `bash_exec` search scoped to it — before searching other granted paths. The workspace holds your own artifacts and earlier outputs; prefer a match found there over an equivalent one found elsewhere.\n\
         - Durable outputs belong in the workspace: write them there with `fs_write` so they persist after the run as user-visible artifacts.\n\
         - Before creating a new durable artifact, search the workspace for an existing relevant one and update it rather than creating a duplicate.\n\
         - Chat is the default output channel for status, findings, and conclusions.\n",
    );

    // Response style. Two failure modes: responses too long for a human to read,
    // and circling a plan the evidence has already ruled out because work was
    // invested in it. Self-referential filler is one driver of the first.
    prompt.push_str(
        "## Response Style\n\
         Answer in as few words as the question allows. A human reads every line you write, so length is a cost you impose on them. Lead with the answer, then only the evidence that changes what they do next.\n\
         - Default to a few sentences. Add structure — lists, sections, tables — only when the content is genuinely structured, never to look thorough.\n\
         - Do not narrate your process, grade the user's message, keep score of who was right, or recap what an earlier exchange settled.\n\
         - When the evidence points away from the current plan, say so first and recommend dropping it. Work already invested is not a reason to continue.\n\
         - State in one plain sentence anything you could not verify. No apology, no hedging at length.\n\n",
    );

    // Transport-drop recovery for grant/response-blocking tools. The local MCP
    // transport can drop an in-flight call (surfaced to the model as
    // "transport dropped mid-call; response for tool <name> was lost"). For a
    // tool that blocks on a user grant or answer, the outcome is then unknown,
    // so the model must re-ask rather than assume an answer or proceed. The
    // backend treats the re-asked call as superseding the orphaned one (the
    // stale approval/question card is replaced in place), so no UI caveats are
    // needed here. Scoped to sessions that actually expose such a tool;
    // ordinary read/write tools, which can be retried without side effects,
    // need no special handling.
    let has_interactive_tool = tool_names
        .iter()
        .any(|n| matches!(*n, "ask_user" | "bash_exec" | "fs_request_grant"));
    if has_interactive_tool {
        prompt.push_str(
            "\n## Interactive Tool Reliability\n\
             A tool call can occasionally fail with a transport error such as `MCP server \"clai\" transport dropped mid-call; response for tool <name> was lost`. This means CLAI lost the in-flight call before its result reached you, so the call's outcome is UNKNOWN — it may or may not have run.\n\
             - This matters specifically for tools that block on a user grant or response — `ask_user`, and approval-gated `bash_exec` / `fs_request_grant`. When one of these drops mid-call, the user may never have answered, or they answered but the decision was lost.\n\
             - When it happens, re-issue the SAME interactive call once. CLAI replaces the lost prompt with the fresh one in the app, so the user simply answers the new prompt. Do NOT assume the lost call was approved, denied, or answered, and do NOT proceed past it.\n\
             - Apply this only to active CLAI human waits. If a user-input, command-approval, or filesystem-grant prompt is explicitly cancelled or denied, do not invent convoluted workarounds to bypass it. If the permission or answer is required, stop and explain what is blocked; retry only after a transport drop where the outcome is unknown.\n\
             - For non-interactive tools (reads, searches, writes), a transport drop needs no special handling — just retry normally if you still need the result.\n",
        );
    }

    if context.space_id.is_some() || !context.mcp_server_ids.is_empty() {
        prompt.push_str(
            "- This tab already carries session-specific context and capabilities. \
             Use the MCP tools attached to this session when they are relevant.\n",
        );
    }

    prompt.push_str("\n## Run Mode\n");
    match trigger {
        RunTrigger::Scheduled | RunTrigger::ManualAutomation => {
            prompt.push_str(
                "This is an autonomous automation pass. You should inspect the current state, \
                 decide what needs to be refreshed, and communicate the result clearly.\n",
            );
        }
        RunTrigger::InterAgentCall => {
            prompt.push_str(
                "This is a synchronous inter-agent call. The caller is waiting for your response.\n",
            );
        }
        RunTrigger::WorkspaceTask => {
            prompt.push_str(
                "This is a workspace-local task assigned by the manager agent. Complete the bounded task using the current workspace context, then report the result clearly. If blocked by missing capability, context, permission, or runtime failure, start with `BLOCKED:` and state the specific manager or user action needed. If you specifically need user feedback or approval, start with `NEEDS_USER_INPUT:` and state the decision needed.\n",
            );
        }
        RunTrigger::UserMessage | RunTrigger::Retry => {
            prompt.push_str(
                "This is a user-driven run. Prioritize the user's latest message and use prior context only as support.\n",
            );
        }
    }

    if !context.workspace_agents.is_empty() {
        prompt.push_str("\n## Workspace Team\n");
        prompt.push_str(
            "This workspace has assigned agents. The default manager agent receives user messages and is responsible for routing work inside this workspace.\n\
             Use this roster as workspace-local context. Do not assume agents outside this list are available for collaboration.\n\
             When task delegation tools are available, assign bounded tasks only to assigned workspace agents, addressed by the `workspaceAgentId` shown in the roster (a display name is not accepted). Tasks run asynchronously and in parallel, each in its own session. Use `ask_user` when work is blocked on a short answer only the user can give — approval, a missing fact, a choice between ready-made options. If the decision needs deliberation, write the analysis in your reply and end your turn instead of forcing it into a modal. If delegation tools are not available in this session, explain which assigned agent should handle the work and what is blocked.\n\n",
        );
        prompt.push_str("Assigned workspace agents:\n");
        for agent in &context.workspace_agents {
            let role = if agent.is_default {
                "manager"
            } else {
                agent.role.as_str()
            };
            if let Some(description) = agent
                .description
                .as_deref()
                .filter(|value| !value.is_empty())
            {
                prompt.push_str(&format!(
                    "- {} ({}) — `workspaceAgentId`: `{}` — {}\n",
                    agent.display_name, role, agent.id, description
                ));
            } else {
                prompt.push_str(&format!(
                    "- {} ({}) — `workspaceAgentId`: `{}`\n",
                    agent.display_name, role, agent.id
                ));
            }
        }
    }

    if let Some(automation_name) = context.automation_name.as_deref() {
        prompt.push_str("\n## Automation Context\n");
        prompt.push_str(&format!(
            "This session belongs to the automation `{}`.\n",
            automation_name
        ));
        prompt.push_str(
            "Your assistant text is visible to the user in chat. Treat chat as the primary way to communicate progress and outcomes.\n\
             Save durable outputs as files in the workspace (via `fs_write`) so they surface as artifacts.\n\
             For routine scheduled passes, a concise chat update is often sufficient.\n\
             Prefer updating existing visuals over recreating duplicate panels when the topic is unchanged.\n",
        );

        if let Some(description) = agent_description.filter(|s| !s.is_empty()) {
            prompt.push_str("\nAgent instructions:\n");
            prompt.push_str(description);
            prompt.push('\n');
        }
    }

    if matches!(trigger, RunTrigger::InterAgentCall) {
        prompt.push_str(
            "\n## Inter-Agent Call\n\
             You have been called by another agent. The latest user message includes the request parameters, the required JSON output schema, and a trace ID.\n\
             Return exactly one JSON object that matches the output schema.\n\
             Do not wrap the response in markdown fences.\n\
             Do not ask follow-up questions because you will not receive answers.\n",
        );
    }

    if let Some(workspace_id) = context.agent_workspace_id.as_deref() {
        prompt.push_str("\n## Local Execution Capabilities\n");
        prompt.push_str(&format!(
            "- Your workspace (id `{workspace_id}`) is your read_write home and your default shell working directory (run `pwd` for its path). Do your work here: write documents, scratch files, code, and durable outputs to the workspace unless the user points you elsewhere. Files in the workspace are shown to the user as **artifacts** in the CLAI app, so treat them as user-facing. The workspace is shared with other agents in the *same* workspace.\n",
        ));

        if context.execution.filesystem.extra_paths.is_empty() {
            prompt.push_str("- Additional path grants: none\n");
        } else {
            prompt.push_str("- Additional path grants:\n");
            for grant in &context.execution.filesystem.extra_paths {
                let access = match grant.access {
                    crate::config::FilesystemPathAccess::ReadOnly => "read_only",
                    crate::config::FilesystemPathAccess::ReadWrite => "read_write",
                };
                prompt.push_str(&format!("  - `{}` ({})\n", grant.path, access));
            }
        }

        let shell_mode = match context.execution.shell.mode {
            crate::config::ShellAccessMode::Off => "off",
            crate::config::ShellAccessMode::Restricted => "restricted",
            crate::config::ShellAccessMode::Full => "full",
        };
        prompt.push_str(&format!("- Shell mode: {}\n", shell_mode));
        let network_status = match context.execution.sandbox.network {
            crate::config::SandboxNetworkConfig::Enabled => "network allowed",
            crate::config::SandboxNetworkConfig::Disabled => "network disabled",
        };
        let sandbox_status = if cfg!(target_os = "linux") {
            let session_bus_status = match context.execution.sandbox.session_bus {
                crate::config::SandboxSessionBusConfig::Allow => "session bus available",
                crate::config::SandboxSessionBusConfig::Deny => "session bus blocked",
            };
            format!(
                "sandboxed shell on Linux through bubblewrap when `bash_exec` is available ({}, {})",
                network_status, session_bus_status
            )
        } else if cfg!(target_os = "macos") {
            format!(
                "sandboxed shell on macOS through Seatbelt/sandbox-exec when `bash_exec` is available ({})",
                network_status
            )
        } else {
            "host shell — sandbox not yet available on this platform".to_string()
        };
        prompt.push_str(&format!("- Shell sandbox: {}\n", sandbox_status));
        if cfg!(target_os = "linux")
            && matches!(
                context.execution.sandbox.session_bus,
                crate::config::SandboxSessionBusConfig::Allow
            )
        {
            prompt.push_str(
                "- Session bus is available: tools that authenticate through libsecret (e.g. `gh`, `git-credential-libsecret`, `secret-tool`) can reach the host keyring directly. Use the host's existing auth instead of asking the user for tokens.\n",
            );
        }

        if !context.execution.shell.blocked_command_prefixes.is_empty() {
            prompt.push_str(&format!(
                "- Blocked command prefixes: {}\n",
                context.execution.shell.blocked_command_prefixes.join(", ")
            ));
        }

        match context.execution.shell.mode {
            crate::config::ShellAccessMode::Restricted => {
                let allowed = context.execution.shell.effective_allowed_command_prefixes();
                let allowed_text = if allowed.is_empty() {
                    "none".to_string()
                } else {
                    allowed.join(", ")
                };
                prompt.push_str(&format!("- Allowed command prefixes: {}\n", allowed_text));
            }
            _ => {
                prompt.push_str("- Allowed command prefixes: any command not blocked\n");
            }
        }

        if context.execution.web.enabled {
            prompt.push_str("- Web access: enabled (`web_search` and `web_fetch` available)\n");
        }

        prompt.push_str(
            "\n## Filesystem boundary\n\
             The path grants listed above are the ONLY locations you are authorized to read, write, or operate against. The `fs_*` tools enforce this in-process. On Linux and macOS, `bash_exec` also runs inside an OS sandbox that allows only the workspace, configured path grants, and required platform system files; if the sandbox is unavailable, `bash_exec` fails closed. On platforms where the shell sandbox is not implemented yet, `bash_exec` is labeled as a host shell and this paragraph remains the authorization boundary.\n\
             - Do not `cd`, redirect to, or pass paths outside the listed grants — not even via subshells, heredocs, scripts, or absolute paths.\n\
             - Do not invoke commands that touch paths outside the grants (no editing the user's other repos, no installing to global locations, no reading personal files like `~/.ssh`, etc.).\n\
             - If a task genuinely needs a path outside your current grants (e.g. `~/.ssh` for `git push`, `~/.config/gh` for the `gh` CLI), call `fs_request_grant({path, access, reason})` BEFORE attempting the work. The user can approve once (lasts this run), approve always (persists to agent settings), narrow the path, or deny. Request the narrowest path that satisfies the task — prefer `~/.config/gh` over `~/.config`, prefer a specific file over its parent directory. Prefer `read_only` unless writes are genuinely needed.\n\
             - If `fs_request_grant` is denied, do not retry the same path. Either request a narrower path, ask the user via `ask_user`, or stop and explain what was blocked.\n\
             - Do not silently extend your reach by other means. The grant flow is the only sanctioned escape valve.\n\
             - Default your writes to the workspace. Other grants (often `$HOME`) are commonly read_only, so writing there fails — check the access listed above first, and if you genuinely need to write to a read_only or ungranted path, `fs_request_grant` it rather than attempting the write and failing.\n\
             - Other CLAI workspaces exist on this machine but are intentionally isolated: you cannot see, list, or read them, and they will never appear in your grants. If the user asks you to work with a different workspace, ask them for its workspace id (the value they can read most easily in the CLAI app; you cannot enumerate workspaces). That workspace lives next to yours — same parent directory as your workspace, named with that id — so `fs_request_grant` that path (e.g. read_only first) to gain access.\n",
        );

        // Git/SSH etiquette guard. The agent shouldn't rewrite commit authorship
        // to bypass GitHub's email-privacy block: that destroys provenance and
        // does an end-run around a user-configured policy. Also note the SSH
        // /etc/ssh overlay so the agent doesn't have to discover the
        // -F /dev/null workaround experimentally.
        prompt.push_str(
            "\n## Git and SSH conventions inside the sandbox\n\
             - Never rewrite commit authorship. Do not run `git commit --amend --reset-author`, do not change `user.email` / `user.name` away from what the commit already has, and do not use the `--author=` flag to overwrite an existing author. If a push is rejected because of GitHub's email privacy (error `GH007`) or because the author's email is not allowed, STOP and escalate via `ask_user` with the exact failing email and the rejection reason. The user owns the choice of which email to publish.\n",
        );
        if cfg!(target_os = "linux") {
            prompt.push_str(
                "             - The Linux sandbox overlays an empty tmpfs at `/etc/ssh`, so OpenSSH only consults `~/.ssh/config` and its built-in defaults. You do not need `-F /dev/null` workarounds; if you see `Bad owner or permissions` from ssh, the cause is something else (likely an explicit `-F` pointing at an unreadable path).\n",
            );
        }

        prompt.push_str(
            "\n## Agent Memory\n\
             The `.clai/memory/` directory inside your workspace is pre-created and ready to use as durable memory across runs. These memory files are surfaced to the user in the CLAI app's **Memory** view, so write them to be human-readable, not just machine notes.\n\
             Memory has three layers, each with a distinct purpose:\n\n\
             ### 1. State — short-horizon working memory (`state.md`)\n\
             Current focus, pending actions, open questions, and outcome of the last run.\n\
             Replaced (not appended) every run — this is what you are thinking about *right now*.\n\n\
             ### 2. Knowledge — curated durable heuristics (`knowledge.md`)\n\
             Patterns, baselines, and lessons that remain valid across multiple runs.\n\
             Each entry should have a confidence tag and supporting evidence:\n\
             - `hypothesis` — observed once, not yet confirmed.\n\
             - `provisional` — observed multiple times or partially corroborated.\n\
             - `confirmed` — verified through repeated observation or explicit validation.\n\
             Remove or downgrade entries when contradicted by fresh evidence.\n\n\
             ### 3. Journal — append-only audit trail (`journal/{date}.md`)\n\
             One file per calendar day. Append timestamped entries for significant decisions, actions, and observations.\n\
             Journals are write-once: never edit past entries, only append new ones.\n\n\
             ### Additional files\n\
             - `index.md` — catalog of all memory files with one-line summaries. Read this first to decide what else to read. Update it whenever you create, rename, or delete a memory file.\n\
             - `checkpoints/<task>.md` — for multi-step work that spans several runs.\n\n\
             ### File conventions\n\
             - Each memory file should start with YAML frontmatter:\n\
             ```\n\
             ---\n\
             updated_at: YYYY-MM-DDTHH:MM:SS\n\
             summary: one-line description of this file's purpose\n\
             tags: [subsystem, topic]   # optional, cross-cutting labels for retrieval\n\
             ---\n\
             ```\n\
             - Keep each file under ~200 lines. When a file grows past this, prune stale entries or split into focused files.\n\
             - Replace outdated sections rather than appending indefinitely (except in `journal/`).\n\
             - Cross-link related memory with relative markdown links inside `.clai/memory/`, e.g. from `knowledge.md`: `[transport-drop fix](journal/2026-06-12.md)` or `[migration plan](checkpoints/db-migration.md)`. Link a knowledge entry to the evidence behind it (a journal day, a checkpoint, a PR url) so memory forms a navigable graph instead of disconnected notes.\n\
             - Use `tags` to group cross-cutting entries (e.g. `[mcp, concurrency]`) so related heuristics are easy to retrieve as `knowledge.md` grows.\n\
             - Tolerate broken links: a link whose target was pruned or not yet written is not an error — leave it or tidy it on your next pass, never let it block you.\n\n",
        );

        match trigger {
            RunTrigger::Scheduled | RunTrigger::ManualAutomation => {
                prompt.push_str(
                    "### Startup protocol (autonomous runs)\n\
                     1. Read `index.md` (if it exists) to see what memory is available.\n\
                     2. Read `state.md` to resume context from the previous run.\n\
                     3. Read `knowledge.md` only if the current task needs historical patterns.\n\
                     4. Do your work.\n\
                     5. Update `state.md` with current focus and outcome.\n\
                     6. Append a journal entry to `journal/{today}.md`.\n\
                     7. If you discovered a durable pattern, add it to `knowledge.md` with the appropriate confidence level.\n\
                     8. If any analysis you produced is worth preserving, file it as a checkpoint or knowledge entry — don't let valuable findings vanish into chat history.\n\
                     9. Update `index.md` if you created or removed any files.\n\
                     10. Prune stale entries: if a knowledge entry or checkpoint is no longer relevant, remove it.\n",
                );
            }
            RunTrigger::InterAgentCall
            | RunTrigger::WorkspaceTask
            | RunTrigger::UserMessage
            | RunTrigger::Retry => {
                prompt.push_str(
                    "### Memory in user-driven runs\n\
                     - Do NOT read memory unless the user's request specifically needs historical context.\n\
                     - Focus on the user's latest message. Memory is supporting context, not the starting point.\n\
                     - If the message seems to assume earlier context you don't have — it references prior decisions, files, or an ongoing task, but you see no conversation history — your session may have been reset (e.g. switching the underlying provider starts a fresh session). Before asking the user to repeat anything, read `.clai/memory/` (start with `index.md`, then `state.md` and any relevant file) to recover the lost context, then continue.\n\
                     - If you discover something worth remembering for future runs, write it to the appropriate memory file.\n\
                     - If the user's request produces a durable finding, consider filing it into knowledge or a checkpoint.\n",
                );
            }
        }

        prompt.push_str(
            "\n### Hierarchy of truth\n\
             When sources conflict, trust the higher-ranked source and update the lower one:\n\
             1. User instruction or human directive (highest)\n\
             2. Live tool output (fresh data from the current run)\n\
             3. Agent knowledge (`knowledge.md`)\n\
             4. Agent state (`state.md`, lowest)\n\n\
             ### Guardrails\n\
             - Treat memory as fallible working notes, not ground truth. Re-check time-sensitive facts with tools before acting.\n\
             - Do not store secrets in memory unless the operator explicitly configured a path for that purpose.\n\
             - Knowledge is not a dashboard — don't duplicate transient metrics there. State is not knowledge — don't put durable heuristics in `state.md`.\n",
        );

        prompt.push_str(
            "\n## Conversation History Database (read-only)\n\
             The complete conversation record of this workspace lives in a SQLite database at `.clai/data.sqlite` (relative to your workspace root): every message, run, and tool call with its full output, across all agents in this workspace — including detail long since compacted out of your context window. Use it when memory files don't have what you need and you must recover verbatim past work: the exact command that was run, the full text of an old error, what the user or a sibling agent said weeks ago.\n\
             - PREFERRED: use the `history_query` tool. It runs a single read-only SQL query against THIS workspace's `.clai/data.sqlite` and returns rows as JSON. It needs no approval because it is structurally incapable of writing or escaping, so it is your always-available way to recover context: if you ever find yourself missing earlier context — for example right after a compaction — query the record to recover it instead of asking the user to repeat anything.\n\
             - Discover the schema first rather than assuming it — it changes between app versions: `SELECT name FROM sqlite_master WHERE type='table'`, then `PRAGMA table_info(<table>)`. The key tables are `assistant_messages` (conversation, `content_json`), `assistant_tool_calls` (every tool invocation and its result), `assistant_runs`, and `workspace_tasks`.\n\
             - Keep queries narrow. Single rows can hold megabytes of tool output, so always SELECT specific columns, filter (`WHERE ... LIKE`, `json_extract`, time ranges on `created_at`) and page with `LIMIT`/`OFFSET`; never dump whole tables or `SELECT *` unbounded.\n\
             - `history_query` reads only THIS workspace's database. To read a DIFFERENT workspace's DB that you have been granted access to, fall back to the shell — STRICTLY READ-ONLY: open it only as `sqlite3 'file:<path>/.clai/data.sqlite?mode=ro'` (or python3's sqlite3 module with the same `mode=ro` URI). Never INSERT/UPDATE/DELETE, never VACUUM or ALTER, never open it without `mode=ro` — a write can corrupt the app's state.\n\
             - Your own in-flight run is in there too. This is a tool for finding *past* work — check memory files first, and reach for the database when you need the verbatim record.\n",
        );
    }

    ProviderInputMessage {
        role: MessageRole::System,
        content: vec![ContentPart::Text { text: prompt }],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assistant::types::SessionContext;
    use crate::assistant::types::WorkspaceAgentSummary;
    use crate::config::{ExecutionCapabilityConfig, ShellAccessMode};

    #[test]
    fn build_system_prompt_includes_agent_memory_guidance_for_automations() {
        let context = SessionContext {
            agent_workspace_id: Some("agent-123".to_string()),
            execution: ExecutionCapabilityConfig::default(),
            ..Default::default()
        };

        let message = build_system_prompt(&context, None, &[], &RunTrigger::Scheduled);
        let text = match &message.content[0] {
            ContentPart::Text { text } => text,
            other => panic!("expected text content, got {:?}", other),
        };

        assert!(text.contains("## Agent Memory"));
        assert!(text.contains("`.clai/memory/`"));
        // Three-layer memory model
        assert!(text.contains("`state.md`"));
        assert!(text.contains("`knowledge.md`"));
        assert!(text.contains("`journal/{date}.md`"));
        assert!(text.contains("`index.md`"));
        // Knowledge confidence levels
        assert!(text.contains("`hypothesis`"));
        assert!(text.contains("`provisional`"));
        assert!(text.contains("`confirmed`"));
        // Schema convention
        assert!(text.contains("updated_at:"));
        assert!(text.contains("summary:"));
        // Cross-cutting tags + cross-linking conventions (OKF-inspired). These
        // are additive: the three-layer model and confidence tags above stay
        // authoritative; links/tags just make memory navigable and retrievable.
        assert!(text.contains("tags: [subsystem, topic]"));
        assert!(text.contains("Cross-link related memory"));
        assert!(text.contains("Tolerate broken links"));
        // Size hint
        assert!(text.contains("~200 lines"));
        // Hierarchy of truth
        assert!(text.contains("### Hierarchy of truth"));
        assert!(text.contains("User instruction or human directive"));
        assert!(text.contains("Live tool output"));
        // Guardrails
        assert!(text.contains("Treat memory as fallible working notes"));
        assert!(text.contains("Knowledge is not a dashboard"));
        // Autonomous startup protocol
        assert!(text.contains("### Startup protocol (autonomous runs)"));
        assert!(text.contains("Read `index.md`"));
        assert!(text.contains("Read `state.md`"));
    }

    #[test]
    fn build_system_prompt_tells_user_runs_to_recover_lost_context_from_memory() {
        let context = SessionContext {
            agent_workspace_id: Some("agent-123".to_string()),
            execution: ExecutionCapabilityConfig::default(),
            ..Default::default()
        };

        let message = build_system_prompt(&context, None, &[], &RunTrigger::UserMessage);
        let text = match &message.content[0] {
            ContentPart::Text { text } => text,
            other => panic!("expected text content, got {:?}", other),
        };

        // A turn whose session was reset (e.g. provider switch) should recover
        // context from memory rather than asking the user to repeat themselves.
        assert!(text.contains("### Memory in user-driven runs"));
        assert!(text.contains("your session may have been reset"));
        assert!(text.contains("`.clai/memory/`"));
        assert!(text.contains("Before asking the user to repeat anything"));
    }

    #[test]
    fn build_system_prompt_omits_agent_memory_guidance_without_workspace() {
        let context = SessionContext::default();

        let message = build_system_prompt(&context, None, &[], &RunTrigger::Scheduled);
        let text = match &message.content[0] {
            ContentPart::Text { text } => text,
            other => panic!("expected text content, got {:?}", other),
        };

        assert!(!text.contains("## Agent Memory"));
    }

    #[test]
    fn build_system_prompt_makes_agent_self_aware_of_clai_workspace_model() {
        let context = SessionContext {
            agent_workspace_id: Some("ws-abc".to_string()),
            execution: ExecutionCapabilityConfig::default(),
            ..Default::default()
        };

        let message = build_system_prompt(&context, None, &[], &RunTrigger::UserMessage);
        let text = match &message.content[0] {
            ContentPart::Text { text } => text,
            other => panic!("expected text content, got {:?}", other),
        };

        // Workspace = the write-home, named by id, files visible as artifacts.
        assert!(text.contains("id `ws-abc`"));
        assert!(text.contains("read_write home"));
        assert!(text.contains("shown to the user as **artifacts**"));
        // Default writes to the workspace (read-only-grant awareness).
        assert!(text.contains("Default your writes to the workspace"));
        // Cross-workspace isolation + how to reach another via its id.
        assert!(text.contains("Other CLAI workspaces exist"));
        assert!(text.contains("ask them for its workspace id"));
        // Memory is surfaced to the user in the app.
        assert!(text.contains("**Memory** view"));
    }

    #[test]
    fn build_system_prompt_documents_readonly_conversation_history_db() {
        let context = SessionContext {
            agent_workspace_id: Some("ws-abc".to_string()),
            execution: ExecutionCapabilityConfig::default(),
            ..Default::default()
        };

        let message = build_system_prompt(&context, None, &[], &RunTrigger::UserMessage);
        let text = match &message.content[0] {
            ContentPart::Text { text } => text,
            other => panic!("expected text content, got {:?}", other),
        };

        // Where the record lives, the preferred always-available tool, and
        // the read-only mandate for the cross-workspace shell fallback.
        assert!(text.contains("## Conversation History Database (read-only)"));
        assert!(text.contains(".clai/data.sqlite"));
        assert!(text.contains("`history_query` tool"));
        assert!(text.contains("STRICTLY READ-ONLY"));
        assert!(text.contains("?mode=ro'"));
        // Schema discovery over hardcoded assumptions; narrow queries.
        assert!(text.contains("sqlite_master"));
        assert!(text.contains("Keep queries narrow"));
        // Memory stays the first stop; the DB is the verbatim fallback.
        assert!(text.contains("check memory files first"));
    }

    #[test]
    fn build_system_prompt_omits_history_db_without_workspace() {
        let context = SessionContext::default();

        let message = build_system_prompt(&context, None, &[], &RunTrigger::UserMessage);
        let text = match &message.content[0] {
            ContentPart::Text { text } => text,
            other => panic!("expected text content, got {:?}", other),
        };

        assert!(!text.contains("## Conversation History Database"));
    }

    #[test]
    fn build_system_prompt_describes_shell_mode_alongside_memory_guidance() {
        let mut execution = ExecutionCapabilityConfig::default();
        execution.shell.mode = ShellAccessMode::Restricted;
        execution.shell.allowed_command_prefixes = vec!["cargo check".to_string()];

        let context = SessionContext {
            agent_workspace_id: Some("agent-123".to_string()),
            execution,
            ..Default::default()
        };

        let message = build_system_prompt(&context, None, &[], &RunTrigger::Scheduled);
        let text = match &message.content[0] {
            ContentPart::Text { text } => text,
            other => panic!("expected text content, got {:?}", other),
        };

        assert!(text.contains("- Shell mode: restricted"));
        assert!(text.contains("- Allowed command prefixes: cargo check"));
        assert!(text.contains("cargo check"));
        assert!(text.contains("## Agent Memory"));
    }

    #[test]
    fn build_system_prompt_makes_chat_the_primary_output_channel() {
        let context = SessionContext {
            automation_name: Some("Health Monitor".to_string()),
            ..Default::default()
        };

        let message = build_system_prompt(&context, None, &[], &RunTrigger::Scheduled);
        let text = match &message.content[0] {
            ContentPart::Text { text } => text,
            other => panic!("expected text content, got {:?}", other),
        };

        assert!(text.contains("Chat is the default output channel."));
        assert!(
            text.contains("Treat chat as the primary way to communicate progress and outcomes.")
        );
        assert!(text
            .contains("For routine scheduled passes, a concise chat update is often sufficient."));
    }

    #[test]
    fn build_system_prompt_includes_current_datetime() {
        let context = SessionContext::default();

        let message = build_system_prompt(&context, None, &[], &RunTrigger::Scheduled);
        let text = match &message.content[0] {
            ContentPart::Text { text } => text,
            other => panic!("expected text content, got {:?}", other),
        };

        assert!(text.contains("Current local date and time: `"));
    }

    #[test]
    fn build_system_prompt_warns_that_prior_tool_results_may_be_stale() {
        let context = SessionContext::default();

        let message = build_system_prompt(&context, None, &[], &RunTrigger::Scheduled);
        let text = match &message.content[0] {
            ContentPart::Text { text } => text,
            other => panic!("expected text content, got {:?}", other),
        };

        assert!(text.contains("Prior tool outputs in the conversation may be stale."));
        assert!(text.contains(
            "Evaluate whether prior tool outputs are still fresh enough for the current decision."
        ));
        assert!(text.contains("re-run the relevant tools if freshness matters."));
    }

    #[test]
    fn build_system_prompt_always_includes_response_style_guidance() {
        // Unconditional: no tools, no memory, plain user message.
        let context = SessionContext::default();
        let message = build_system_prompt(&context, None, &[], &RunTrigger::UserMessage);
        let text = match &message.content[0] {
            ContentPart::Text { text } => text,
            _ => panic!("expected text"),
        };

        assert!(text.contains("## Response Style"));
        assert!(text.contains("Answer in as few words as the question allows."));
        assert!(text.contains("Work already invested is not a reason to continue."));
        assert!(text.contains("keep score of who was right"));
    }

    #[test]
    fn build_system_prompt_adds_interactive_reliability_guidance_when_blocking_tool_present() {
        let context = SessionContext::default();
        let tools = [crate::assistant::types::ToolDefinition {
            name: "ask_user".to_string(),
            description: "desc".to_string(),
            input_schema: serde_json::json!({}),
        }];

        let message = build_system_prompt(&context, None, &tools, &RunTrigger::UserMessage);
        let text = match &message.content[0] {
            ContentPart::Text { text } => text,
            other => panic!("expected text content, got {:?}", other),
        };

        assert!(text.contains("## Interactive Tool Reliability"));
        assert!(text.contains("transport dropped mid-call"));
        assert!(text.contains("re-issue the SAME interactive call once"));
        assert!(text.contains("Apply this only to active CLAI human waits"));
        assert!(text.contains("do not invent convoluted workarounds"));
        // The backend supersedes the orphaned request when the model
        // re-asks, so the prompt must NOT push stale-card caveats (e.g.
        // telling the user to dismiss duplicates) onto the model.
        assert!(text.contains("replaces the lost prompt with the fresh one"));
        assert!(!text.contains("dismiss"));
    }

    #[test]
    fn build_system_prompt_omits_interactive_reliability_guidance_without_blocking_tool() {
        let context = SessionContext::default();
        let tools = [crate::assistant::types::ToolDefinition {
            name: "fs_read".to_string(),
            description: "desc".to_string(),
            input_schema: serde_json::json!({}),
        }];

        let message = build_system_prompt(&context, None, &tools, &RunTrigger::UserMessage);
        let text = match &message.content[0] {
            ContentPart::Text { text } => text,
            other => panic!("expected text content, got {:?}", other),
        };

        assert!(!text.contains("## Interactive Tool Reliability"));
    }

    #[test]
    fn build_system_prompt_describes_autonomous_run_mode() {
        let context = SessionContext {
            agent_workspace_id: Some("agent-123".to_string()),
            ..Default::default()
        };

        let message = build_system_prompt(&context, None, &[], &RunTrigger::Scheduled);
        let text = match &message.content[0] {
            ContentPart::Text { text } => text,
            other => panic!("expected text content, got {:?}", other),
        };

        assert!(text.contains("## Run Mode"));
        assert!(text.contains("This is an autonomous automation pass."));
        assert!(text.contains("### Startup protocol (autonomous runs)"));
        assert!(text.contains("Read `index.md`"));
        assert!(text.contains("Read `state.md`"));
        assert!(text.contains("Append a journal entry"));
        assert!(text.contains("Prune stale entries"));
        assert!(text.contains("don't let valuable findings vanish into chat history"));
    }

    #[test]
    fn build_system_prompt_describes_user_driven_run_mode() {
        let context = SessionContext {
            agent_workspace_id: Some("agent-123".to_string()),
            ..Default::default()
        };

        let message = build_system_prompt(&context, None, &[], &RunTrigger::UserMessage);
        let text = match &message.content[0] {
            ContentPart::Text { text } => text,
            other => panic!("expected text content, got {:?}", other),
        };

        assert!(text.contains("## Run Mode"));
        assert!(text.contains("This is a user-driven run."));
        assert!(text.contains("### Memory in user-driven runs"));
        assert!(text.contains("Do NOT read memory unless"));
    }

    #[test]
    fn build_system_prompt_includes_workspace_agent_roster() {
        let context = SessionContext {
            workspace_agents: vec![
                WorkspaceAgentSummary {
                    id: "workspace-agent-manager".to_string(),
                    agent_definition_id: "manager-definition".to_string(),
                    display_name: "Manager".to_string(),
                    role: "manager".to_string(),
                    is_default: true,
                    description: Some("Coordinates workspace tasks.".to_string()),
                },
                WorkspaceAgentSummary {
                    id: "workspace-agent-reviewer".to_string(),
                    agent_definition_id: "reviewer-definition".to_string(),
                    display_name: "Code Reviewer".to_string(),
                    role: "member".to_string(),
                    is_default: false,
                    description: Some("Reviews source changes.".to_string()),
                },
            ],
            ..Default::default()
        };

        let message = build_system_prompt(&context, None, &[], &RunTrigger::UserMessage);
        let text = match &message.content[0] {
            ContentPart::Text { text } => text,
            other => panic!("expected text content, got {:?}", other),
        };

        assert!(text.contains("## Workspace Team"));
        assert!(text.contains("The default manager agent receives user messages"));
        assert!(text.contains("- Manager (manager)"));
        assert!(text.contains("- Code Reviewer (member)"));
        assert!(text.contains("Reviews source changes."));
        // `workspace_assignTask` matches on the workspace agent id, so the
        // roster has to carry it: a display name is not addressable.
        assert!(text.contains("`workspaceAgentId`: `workspace-agent-manager`"));
        assert!(text.contains("`workspaceAgentId`: `workspace-agent-reviewer`"));
    }

    #[test]
    fn build_system_prompt_roster_renders_the_id_for_a_descriptionless_agent() {
        let context = SessionContext {
            workspace_agents: vec![WorkspaceAgentSummary {
                description: None,
                ..manager_summary()
            }],
            ..Default::default()
        };

        let message = build_system_prompt(&context, None, &[], &RunTrigger::UserMessage);
        let text = match &message.content[0] {
            ContentPart::Text { text } => text,
            other => panic!("expected text content, got {:?}", other),
        };

        assert!(
            text.contains("- Manager (manager) — `workspaceAgentId`: `workspace-agent-manager`")
        );
    }

    fn manager_summary() -> WorkspaceAgentSummary {
        WorkspaceAgentSummary {
            id: "workspace-agent-manager".to_string(),
            // Deliberately different from `id`: the roster must render the
            // workspace agent id (what `workspace_assignTask` matches on),
            // and identical fixtures would hide a swap between the two.
            agent_definition_id: "manager-definition".to_string(),
            display_name: "Manager".to_string(),
            role: "manager".to_string(),
            is_default: true,
            description: Some("Coordinates workspace tasks.".to_string()),
        }
    }

    fn member_summary() -> WorkspaceAgentSummary {
        WorkspaceAgentSummary {
            id: "workspace-agent-reviewer".to_string(),
            agent_definition_id: "reviewer-definition".to_string(),
            display_name: "Code Reviewer".to_string(),
            role: "member".to_string(),
            is_default: false,
            description: Some("Reviews source changes.".to_string()),
        }
    }

    #[test]
    fn build_system_prompt_documents_parallel_task_mechanics_for_manager() {
        let context = SessionContext {
            automation_id: Some("workspace-agent-manager".to_string()),
            workspace_agents: vec![manager_summary(), member_summary()],
            ..Default::default()
        };

        let message = build_system_prompt(&context, None, &[], &RunTrigger::UserMessage);
        let text = match &message.content[0] {
            ContentPart::Text { text } => text,
            other => panic!("expected text content, got {:?}", other),
        };

        // Async + parallel semantics, fan-out, self-tasking, the manager
        // lifecycle invariant, and the caveats (shared workspace dir,
        // self-contained instructions, durable ids).
        // The member roster is what the manager delegates from, so it must
        // carry the id `workspace_assignTask` matches on.
        assert!(text.contains(
            "- **Code Reviewer** (member) — `workspaceAgentId`: `workspace-agent-reviewer`"
        ));
        assert!(text.contains("never on the display name"));
        // Self-tasking needs the manager's own id, not an inference from the
        // roster block further down the prompt.
        assert!(text.contains("Your own `workspaceAgentId` is `workspace-agent-manager`"));
        assert!(text.contains("### How tasks run"));
        assert!(text.contains("no per-agent limit"));
        assert!(text.contains("Assigning a task to yourself"));
        assert!(text.contains("does NOT see this conversation"));
        assert!(text.contains("Partition parallel work"));
        assert!(text.contains("NEVER send a final response"));
        assert!(text.contains("still `queued` or `running`"));
        assert!(text.contains("history_query"));
        assert!(text.contains("stalled, hung, or blocked"));
        assert!(text.contains("Record task ids in memory"));
        assert!(!text.contains("your run can end before the task finishes"));
    }

    #[test]
    fn build_system_prompt_offers_self_tasking_to_solo_manager() {
        let context = SessionContext {
            automation_id: Some("workspace-agent-manager".to_string()),
            workspace_agents: vec![manager_summary()],
            ..Default::default()
        };

        let message = build_system_prompt(&context, None, &[], &RunTrigger::UserMessage);
        let text = match &message.content[0] {
            ContentPart::Text { text } => text,
            other => panic!("expected text content, got {:?}", other),
        };

        // No members: the role callout still renders, framed around
        // self-tasking as the background-work mechanism.
        assert!(text.contains("(and only agent)"));
        assert!(text.contains("assign a task to *yourself*"));
        assert!(text.contains("There are no member agents to delegate to"));
        // A solo manager self-assigns by id too, so hand it its own id
        // rather than pointing it at `workspace_listAgents`.
        assert!(text.contains("Your own `workspaceAgentId` is `workspace-agent-manager`"));
        assert!(text.contains("### How tasks run"));
    }

    #[test]
    fn build_system_prompt_marks_workspace_task_runs_as_workers() {
        let context = SessionContext {
            automation_id: Some("workspace-agent-manager".to_string()),
            workspace_agents: vec![manager_summary()],
            ..Default::default()
        };

        let message = build_system_prompt(&context, None, &[], &RunTrigger::WorkspaceTask);
        let text = match &message.content[0] {
            ContentPart::Text { text } => text,
            other => panic!("expected text content, got {:?}", other),
        };

        assert!(text.contains("## Task Worker Context"));
        assert!(text.contains("result summary"));
        assert!(text.contains("never create task chains"));
        // A worker (even a self-tasked manager instance) must not be invited
        // to fan out further tasks — neither the offer to self-assign nor the
        // id such a chain would need.
        assert!(!text.contains("### How tasks run"));
        assert!(!text.contains("Your own `workspaceAgentId`"));
        assert!(!text.contains("assign a task to *yourself*"));
    }

    #[test]
    fn build_system_prompt_withholds_own_id_from_a_manager_worker_with_members() {
        let context = SessionContext {
            automation_id: Some("workspace-agent-manager".to_string()),
            workspace_agents: vec![manager_summary(), member_summary()],
            ..Default::default()
        };

        let message = build_system_prompt(&context, None, &[], &RunTrigger::WorkspaceTask);
        let text = match &message.content[0] {
            ContentPart::Text { text } => text,
            other => panic!("expected text content, got {:?}", other),
        };

        // A self-assigned manager task still renders the member roster — it
        // may need to know who exists — but must not be told to assign a task
        // to itself, which is what a task chain would need.
        assert!(text.contains("`workspaceAgentId`: `workspace-agent-reviewer`"));
        assert!(!text.contains("Your own `workspaceAgentId`"));
        assert!(!text.contains("### How tasks run"));
        assert!(text.contains("never create task chains"));
    }

    #[test]
    fn build_system_prompt_hides_task_mechanics_from_members() {
        let context = SessionContext {
            automation_id: Some("workspace-agent-reviewer".to_string()),
            workspace_agents: vec![manager_summary(), member_summary()],
            ..Default::default()
        };

        let message = build_system_prompt(&context, None, &[], &RunTrigger::UserMessage);
        let text = match &message.content[0] {
            ContentPart::Text { text } => text,
            other => panic!("expected text content, got {:?}", other),
        };

        assert!(!text.contains("## Your Role"));
        assert!(!text.contains("### How tasks run"));
        assert!(!text.contains("## Task Worker Context"));
    }

    #[test]
    fn build_system_prompt_memory_guardrails_present_in_both_modes() {
        let context = SessionContext {
            agent_workspace_id: Some("agent-123".to_string()),
            ..Default::default()
        };

        for trigger in &[RunTrigger::Scheduled, RunTrigger::UserMessage] {
            let message = build_system_prompt(&context, None, &[], trigger);
            let text = match &message.content[0] {
                ContentPart::Text { text } => text,
                other => panic!("expected text content, got {:?}", other),
            };

            assert!(
                text.contains("### Guardrails"),
                "Missing guardrails for {:?}",
                trigger
            );
            assert!(
                text.contains("### Hierarchy of truth"),
                "Missing hierarchy of truth for {:?}",
                trigger
            );
            assert!(text.contains("Treat memory as fallible working notes"));
            assert!(text.contains("Do not store secrets in memory"));
            assert!(text.contains("Knowledge is not a dashboard"));
        }
    }
}
