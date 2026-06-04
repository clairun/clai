use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};

const STDERR_TAIL_LINES: usize = 20;

use serde_json::Value;
use tauri::Manager;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::assistant::engine::{
    build_system_prompt, build_trigger_message, AssistantDeps, AssistantEngineError, RunTurnInput,
};
use crate::assistant::events::{emit_event, AssistantUiEvent};
use crate::assistant::local_mcp::{self, ToolBinding};
use crate::assistant::providers::cli::{CLAUDE_CODE_PROVIDER_ID, CODEX_PROVIDER_ID};
use crate::assistant::repository::{
    self, CreateMessageParams, CreateRunParams, CreateToolCallParams,
};
use crate::assistant::types::{
    AssistantMessage, AssistantSession, ContentPart, MessageRole, ProviderConnection,
    ProviderInputMessage, RunNotice, RunStatus, RunUsage, ToolCallStatus,
};
use crate::AppState;

const CLAUDE_DISABLED_TOOLS: &str = "Bash,Read,Edit,Write,Glob,Grep,WebFetch,WebSearch,Task,TodoWrite,NotebookEdit,NotebookRead,LSP";
const CODEX_MCP_TOKEN_ENV: &str = "CLAI_MCP_TOKEN";

/// When `CLAI_LOG_CLI_STREAM` is set to a truthy value, every raw JSONL line
/// received from a CLI provider (Claude Code / Codex) is logged verbatim at
/// `info!` (visible under the default `info` filter). This is a diagnostic
/// hook for capturing the exact event envelope — e.g. to inspect what a
/// usage/rate-limit failure actually looks like on the wire, including any
/// structured fields (subtype, error code) we don't yet parse.
fn cli_stream_logging_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var("CLAI_LOG_CLI_STREAM")
            .map(|value| {
                let value = value.trim();
                !value.is_empty() && value != "0" && !value.eq_ignore_ascii_case("false")
            })
            .unwrap_or(false)
    })
}

/// When `CLAI_CC_DEBUG` is truthy, the spawned Claude Code process is launched
/// with `--debug-file`, capturing its internal debug logs (including the MCP
/// client) to a temp file. Diagnostic-only; off by default.
fn cc_debug_logging_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var("CLAI_CC_DEBUG")
            .map(|value| {
                let value = value.trim();
                !value.is_empty() && value != "0" && !value.eq_ignore_ascii_case("false")
            })
            .unwrap_or(false)
    })
}

fn log_cli_stream_line(provider: &str, run_id: &str, line: &str) {
    if cli_stream_logging_enabled() {
        tracing::info!(target: "clai::cli_stream", provider, run_id, raw = %line, "CLI stream line");
    }
}

#[derive(Clone, Copy)]
enum CliProviderRuntime {
    ClaudeCode,
    Codex,
}

impl CliProviderRuntime {
    fn for_provider_id(provider_id: &str) -> Option<Self> {
        match provider_id {
            CLAUDE_CODE_PROVIDER_ID => Some(Self::ClaudeCode),
            CODEX_PROVIDER_ID => Some(Self::Codex),
            _ => None,
        }
    }

    fn display_name(self) -> &'static str {
        match self {
            Self::ClaudeCode => "Claude Code",
            Self::Codex => "Codex",
        }
    }

    fn metadata_source(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude-code",
            Self::Codex => "codex",
        }
    }
}

pub async fn run_session_turn(
    deps: &AssistantDeps,
    input: RunTurnInput,
) -> Result<(), AssistantEngineError> {
    let mut session = repository::get_session(&deps.pool, &input.session_id)
        .await?
        .ok_or_else(|| AssistantEngineError::SessionNotFound(input.session_id.clone()))?;

    let connection = deps
        .app
        .try_state::<AppState>()
        .and_then(|state| {
            state
                .config_manager
                .lock()
                .ok()?
                .get_provider_connection(&input.connection_id)
        })
        .ok_or_else(|| AssistantEngineError::ProviderNotConfigured(input.connection_id.clone()))?;

    let run_id = resolve_run_id(deps, &session, &connection, &input).await?;

    let run = repository::update_run_status(&deps.pool, &run_id, RunStatus::Running, None).await?;
    let _ = emit_event(
        &deps.app,
        &session,
        Some(&run_id),
        AssistantUiEvent::RunStarted { run },
    );

    if input.cancel_token.is_cancelled() {
        cancel_run(deps, &session, &run_id, None).await?;
        return Ok(());
    }

    let provider_runtime =
        match CliProviderRuntime::for_provider_id(connection.provider_id.as_str()) {
            Some(runtime) => runtime,
            None => {
                let message = format!(
                    "CLI provider '{}' is registered but not implemented yet",
                    connection.provider_id
                );
                fail_run(deps, &session, &run_id, None, &message).await?;
                discard_if_unanswered(deps, &session, &run_id, &input, &None).await;
                return Err(AssistantEngineError::Provider(
                    crate::assistant::providers::types::ProviderError::RequestFailed(message),
                ));
            }
        };
    // A CLI session id is provider-specific (Claude generates its own UUID;
    // Codex returns a server-side thread id), so an id created by one CLI is
    // meaningless to another — resuming it fails (e.g. Codex: "no rollout
    // found for thread id"). If the session was last driven by a *known,
    // different* provider, drop the stale id so this run starts fresh. We only
    // act when the owning provider is known: legacy sessions (provider `None`,
    // created before this was tracked) are left alone and recover via the
    // session-lost path instead, so we don't needlessly reset live sessions.
    if session
        .context
        .cli_session_provider
        .as_deref()
        .is_some_and(|owner| owner != connection.provider_id)
        && session.context.cli_session_id.is_some()
    {
        clear_cli_session_id(deps, &mut session).await?;
    }

    let mcp_runtime = local_mcp::ensure_started(&deps.app).await?;
    let notices = Arc::new(Mutex::new(Vec::<RunNotice>::new()));
    let session_grants = Arc::new(Mutex::new(Vec::new()));
    let session_allowed_command_prefixes = Arc::new(Mutex::new(Vec::new()));
    let session_blocked_command_prefixes = Arc::new(Mutex::new(Vec::new()));
    // `binding_guard` removes the bearer token from the MCP runtime on
    // drop, including on panic or early return. Keep it alive until
    // after the CLI subprocess has finished making MCP calls.
    let binding_guard = mcp_runtime.bind_run(ToolBinding {
        pool: deps.pool.clone(),
        session_id: session.id.clone(),
        run_id: run_id.clone(),
        cancel_token: input.cancel_token.clone(),
        inter_agent_call_depth: input.inter_agent_call_depth,
        notices: notices.clone(),
        session_grants,
        session_allowed_command_prefixes,
        session_blocked_command_prefixes,
    });

    // A single assistant message is reused across attempts (see
    // `ensure_assistant_message_slot`): if the first attempt fails because the
    // CLI session was lost, we transparently restart with a fresh session and
    // refill the same chat bubble, so the user never sees a stray empty turn or
    // a "send your message again" error.
    let mut assistant_slot: Option<AssistantMessage> = None;
    let mut retried = false;

    let run_result = loop {
        let attempt = match provider_runtime {
            CliProviderRuntime::ClaudeCode => {
                let (cli_session_id, is_new_session) =
                    ensure_cli_session_id(deps, &mut session, &connection.provider_id).await?;
                let mcp_config_path =
                    match write_mcp_config(mcp_runtime.url(), binding_guard.token()) {
                        Ok(path) => path,
                        Err(error) => {
                            let message = error.message();
                            fail_run(deps, &session, &run_id, None, &message).await?;
                            discard_if_unanswered(deps, &session, &run_id, &input, &assistant_slot)
                                .await;
                            return Err(AssistantEngineError::Provider(
                                crate::assistant::providers::types::ProviderError::RequestFailed(
                                    message,
                                ),
                            ));
                        }
                    };
                let result = run_claude_turn(
                    deps,
                    &session,
                    &connection,
                    &run_id,
                    &cli_session_id,
                    is_new_session,
                    &mcp_config_path,
                    &input.cancel_token,
                    &input.trigger,
                    &mut assistant_slot,
                )
                .await;
                let _ = std::fs::remove_file(&mcp_config_path);
                result
            }
            CliProviderRuntime::Codex => {
                run_codex_turn(
                    deps,
                    &mut session,
                    &connection,
                    &run_id,
                    mcp_runtime.url(),
                    binding_guard.token(),
                    &input.cancel_token,
                    &input.trigger,
                    &mut assistant_slot,
                )
                .await
            }
        };

        // The CLI session id is provider-specific and can also be pruned/expired
        // server-side, so resuming it can fail with "no rollout found" / "No
        // conversation found with session ID". When that happens, drop the stale
        // id and retry exactly once with a fresh session — transparently. The
        // retried turn reuses `assistant_slot`, and a freshly-minted session
        // can't itself be "lost", so this is naturally bounded.
        if !retried {
            if let Err(LocalAgentRunError::Failed { message, .. }) = &attempt {
                if is_session_lost_error(provider_runtime, message) {
                    tracing::info!(
                        target: "clai::cli_session",
                        provider = provider_runtime.metadata_source(),
                        run_id = %run_id,
                        "{} session was lost; restarting with a fresh session",
                        provider_runtime.display_name()
                    );
                    clear_cli_session_id(deps, &mut session).await?;
                    retried = true;
                    continue;
                }
            }
        }

        break attempt;
    };

    drop(binding_guard);

    match run_result {
        Ok(usage) => {
            let notices = notices
                .lock()
                .map(|mut n| std::mem::take(&mut *n))
                .unwrap_or_default();
            let final_status = if notices.is_empty() {
                RunStatus::Completed
            } else {
                RunStatus::CompletedWithWarnings
            };
            let run = repository::complete_run(
                &deps.pool,
                &run_id,
                final_status,
                usage.as_ref(),
                None,
                &notices,
            )
            .await?;
            let _ = emit_event(
                &deps.app,
                &session,
                Some(&run_id),
                AssistantUiEvent::RunCompleted { run },
            );
            Ok(())
        }
        Err(LocalAgentRunError::Cancelled { usage }) => {
            cancel_run(deps, &session, &run_id, usage.as_ref()).await
        }
        Err(LocalAgentRunError::Failed { message, usage }) => {
            if is_usage_limit_error(&message) {
                // A usage/rate limit is a non-retryable failure (the user must
                // wait until the stated reset time), distinct from a transient
                // crash. Log it as such so it's diagnosable; the provider's
                // message is already user-facing so we pass it through unchanged.
                tracing::warn!(
                    target: "clai::usage_limit",
                    provider = provider_runtime.metadata_source(),
                    run_id = %run_id,
                    "{} reported a usage/rate limit: {}",
                    provider_runtime.display_name(),
                    message
                );
            }
            fail_run(deps, &session, &run_id, usage.as_ref(), &message).await?;
            discard_if_unanswered(deps, &session, &run_id, &input, &assistant_slot).await;
            Err(AssistantEngineError::Provider(
                crate::assistant::providers::types::ProviderError::RequestFailed(message),
            ))
        }
    }
}

/// After a failed CLI run, drop the unanswered input — but only when the
/// turn produced nothing the user could see. The check runs against the
/// slot's *persisted* row (tool_use parts are flushed mid-run and partial
/// output is finalized before the error returns, so the DB is the source
/// of truth). Conservative on load errors: real output must never be
/// deleted, so "can't tell" counts as "has content".
async fn discard_if_unanswered(
    deps: &AssistantDeps,
    session: &AssistantSession,
    run_id: &str,
    input: &crate::assistant::engine::RunTurnInput,
    assistant_slot: &Option<AssistantMessage>,
) {
    let placeholder_id = match assistant_slot.as_ref() {
        None => None,
        Some(slot) => match repository::get_message(&deps.pool, &slot.id).await {
            Ok(Some(current))
                if crate::assistant::engine::run_produced_no_content(&current.content) =>
            {
                Some(slot.id.as_str())
            }
            Ok(None) => None,
            // Non-empty content or load error: the user saw (or may have
            // seen) output — keep everything.
            _ => return,
        },
    };
    crate::assistant::engine::discard_unanswered_run_input(
        deps,
        session,
        run_id,
        input.trigger_message_id.as_deref(),
        placeholder_id,
    )
    .await;
}

fn is_session_lost_error(provider_runtime: CliProviderRuntime, message: &str) -> bool {
    match provider_runtime {
        CliProviderRuntime::ClaudeCode => message.contains("No conversation found with session ID"),
        CliProviderRuntime::Codex => {
            message.contains("Session not found")
                || message.contains("No session found")
                || message.contains("failed to read thread")
                // `codex exec resume <id>` for an id Codex doesn't know (e.g. a
                // stale id left by another provider, or a pruned rollout).
                || message.contains("no rollout found")
                || message.contains("thread/resume failed")
        }
    }
}

/// Detects a provider usage/rate-limit failure from a CLI error message.
///
/// Both CLI providers surface these as free-text only (no structured error
/// code on the wire), so we match on the message:
///   - Claude Code: "You've hit your session limit · resets 4:40pm (…)"
///   - Codex:       "You've hit your usage limit. … try again at 9:47 PM."
///
/// Unlike a transient crash these are non-retryable until the stated reset
/// time, so callers treat them distinctly.
fn is_usage_limit_error(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("usage limit")
        || lower.contains("session limit")
        || lower.contains("rate limit")
        || lower.contains("rate_limit")
        || lower.contains("quota")
        || (lower.contains("you've hit your") && lower.contains("limit"))
}

async fn clear_cli_session_id(
    deps: &AssistantDeps,
    session: &mut AssistantSession,
) -> Result<(), AssistantEngineError> {
    session.context.cli_session_id = None;
    session.context.cli_session_provider = None;
    session.updated_at = chrono::Utc::now().timestamp_millis();
    *session = repository::update_session(&deps.pool, session).await?;
    Ok(())
}

async fn set_cli_session_id(
    deps: &AssistantDeps,
    session: &mut AssistantSession,
    cli_session_id: String,
    provider_id: &str,
) -> Result<(), LocalAgentRunError> {
    if session.context.cli_session_id.as_deref() == Some(cli_session_id.as_str())
        && session.context.cli_session_provider.as_deref() == Some(provider_id)
    {
        return Ok(());
    }
    session.context.cli_session_id = Some(cli_session_id);
    session.context.cli_session_provider = Some(provider_id.to_string());
    session.updated_at = chrono::Utc::now().timestamp_millis();
    *session = repository::update_session(&deps.pool, session)
        .await
        .map_err(LocalAgentRunError::failed)?;
    Ok(())
}

async fn resolve_run_id(
    deps: &AssistantDeps,
    session: &AssistantSession,
    connection: &ProviderConnection,
    input: &RunTurnInput,
) -> Result<String, AssistantEngineError> {
    match &input.run_id {
        Some(id) => {
            let existing_run = repository::get_run(&deps.pool, id).await?.ok_or_else(|| {
                AssistantEngineError::Persistence(format!("run not found: {}", id))
            })?;
            if existing_run.connection_id != input.connection_id {
                return Err(AssistantEngineError::RunConnectionMismatch(id.clone()));
            }
            Ok(id.clone())
        }
        None => {
            let run = repository::create_run(
                &deps.pool,
                CreateRunParams {
                    session_id: session.id.clone(),
                    status: RunStatus::Queued,
                    trigger: input.trigger.clone(),
                    connection_id: connection.id.clone(),
                    provider_id: connection.provider_id.clone(),
                    model_id: connection.model_id.clone(),
                    usage: None,
                    error: None,
                },
            )
            .await?;
            Ok(run.id)
        }
    }
}

async fn ensure_cli_session_id(
    deps: &AssistantDeps,
    session: &mut AssistantSession,
    provider_id: &str,
) -> Result<(String, bool), AssistantEngineError> {
    if let Some(id) = session.context.cli_session_id.clone() {
        // Backfill the owning provider for sessions created before it was
        // tracked, so a future provider switch is detected proactively.
        if session.context.cli_session_provider.as_deref() != Some(provider_id) {
            session.context.cli_session_provider = Some(provider_id.to_string());
            session.updated_at = chrono::Utc::now().timestamp_millis();
            *session = repository::update_session(&deps.pool, session).await?;
        }
        return Ok((id, false));
    }

    let id = Uuid::new_v4().to_string();
    session.context.cli_session_id = Some(id.clone());
    session.context.cli_session_provider = Some(provider_id.to_string());
    session.updated_at = chrono::Utc::now().timestamp_millis();
    *session = repository::update_session(&deps.pool, session).await?;
    Ok((id, true))
}

/// Resolve the turn's assistant message from a reusable slot.
///
/// The slot lets `run_session_turn` keep a single assistant message across an
/// automatic retry (when a stale CLI session is dropped and restarted fresh),
/// so the retry refills the *same* chat bubble instead of leaving a stray empty
/// one behind. On first use it creates the message (seeded with an empty Text
/// placeholder, like the rest of the streaming path expects) and emits
/// `MessageCreated`; subsequent calls return the stored message unchanged.
async fn ensure_assistant_message_slot(
    deps: &AssistantDeps,
    session: &AssistantSession,
    run_id: &str,
    metadata_source: &str,
    slot: &mut Option<AssistantMessage>,
) -> Result<AssistantMessage, LocalAgentRunError> {
    if let Some(existing) = slot.as_ref() {
        return Ok(existing.clone());
    }
    let assistant_message = repository::create_message(
        &deps.pool,
        CreateMessageParams {
            session_id: session.id.clone(),
            role: MessageRole::Assistant,
            content: vec![ContentPart::Text {
                text: String::new(),
            }],
            provider_metadata: Some(serde_json::json!({ "source": metadata_source })),
        },
    )
    .await?;
    let _ = emit_event(
        &deps.app,
        session,
        Some(run_id),
        AssistantUiEvent::MessageCreated {
            message: assistant_message.clone(),
        },
    );
    *slot = Some(assistant_message.clone());
    Ok(assistant_message)
}

#[allow(clippy::too_many_arguments)]
async fn run_claude_turn(
    deps: &AssistantDeps,
    session: &AssistantSession,
    connection: &ProviderConnection,
    run_id: &str,
    cli_session_id: &str,
    is_new_session: bool,
    mcp_config_path: &PathBuf,
    cancel_token: &CancellationToken,
    trigger: &crate::assistant::types::RunTrigger,
    assistant_slot: &mut Option<AssistantMessage>,
) -> Result<Option<RunUsage>, LocalAgentRunError> {
    let prompt = prepare_prompt(
        deps,
        session,
        run_id,
        trigger,
        CliProviderRuntime::ClaudeCode.metadata_source(),
        CliProviderRuntime::ClaudeCode.display_name(),
    )
    .await?;
    let system_prompt = system_prompt_text(&deps.app, session, trigger).await;
    let assistant_message = ensure_assistant_message_slot(
        deps,
        session,
        run_id,
        CliProviderRuntime::ClaudeCode.metadata_source(),
        assistant_slot,
    )
    .await?;

    let binary = connection
        .base_url
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("claude");
    let mut command = Command::new(binary);
    // Default MCP_TIMEOUT in Claude Code is ~30s, which is shorter than a
    // human typically takes to answer an `ask_user` prompt. When the user
    // takes longer the MCP HTTP request times out client-side and Claude
    // reports `MCP server "clai" transport dropped mid-call; response for
    // tool "ask_user" was lost`. Bump to 1h — runs are already bounded by
    // `cancel_token`, so an actually-stuck tool doesn't sit indefinitely.
    command.env("MCP_TIMEOUT", "3600000");
    // Force Claude Code's "tool search" off. CC 2.1.x can *optimistically*
    // enable tool search (a server-driven experiment, seen as
    // `[ToolSearch:optimistic] mode=tst, result=true` in its debug log), which
    // withholds tool definitions from the model for on-demand lookup via a
    // `ToolSearchTool`. But CLAI disallows that search tool (`--tools ""` +
    // `--disallowedTools`), so when the experiment fires the model ends up with
    // neither the MCP tools nor a way to find them -> `tools:[]`, intermittently
    // (it's an experiment bucket, hence the coin-flip). Pinning it off makes CC
    // inject our tool defs directly every run. Harmless: we only expose ~12
    // tools, well under any threshold where search would matter. See #63120.
    command.env("ENABLE_TOOL_SEARCH", "false");
    command
        .arg("-p")
        .arg("--output-format")
        .arg("stream-json")
        .arg("--include-partial-messages")
        .arg("--verbose");
    if is_new_session {
        command.arg("--session-id").arg(cli_session_id);
    } else {
        command.arg("--resume").arg(cli_session_id);
    }
    command
        .arg("--mcp-config")
        .arg(mcp_config_path)
        .arg("--strict-mcp-config")
        .arg("--tools")
        .arg("")
        .arg("--disallowedTools")
        .arg(CLAUDE_DISABLED_TOOLS)
        .arg("--permission-mode")
        .arg("bypassPermissions")
        .arg("--disable-slash-commands")
        .arg("--system-prompt")
        .arg(system_prompt)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if !connection.model_id.trim().is_empty() {
        command.arg("--model").arg(connection.model_id.trim());
    }

    // Opt-in: when `CLAI_CC_DEBUG` is truthy, route Claude Code's own debug
    // logging (incl. its MCP client) to a dedicated file. `--debug-file`
    // implicitly enables debug mode and keeps the output off stdout/stderr so
    // it can't corrupt the `stream-json` envelope we parse. Diagnostic hook for
    // the CC 2.1.153+ "tools/list served but tools:[] / status pending" issue
    // (#63120) — lets us see CC's side of the MCP handshake.
    if cc_debug_logging_enabled() {
        let debug_path = std::env::temp_dir().join("clai-cc-debug.log");
        tracing::info!(
            target: "clai::mcp",
            path = %debug_path.display(),
            "Claude Code debug logging enabled (--debug-file)"
        );
        command.arg("--debug-file").arg(&debug_path);
    }

    // Pin the subprocess cwd to the agent's workspace root. Without
    // this, Claude Code inherits CLAI's launch cwd — which on a dev
    // machine is the CLAI repo itself, causing Claude Code's
    // auto-memory loader (`~/.claude/projects/<hash-of-cwd>/memory/`)
    // to pull in memory from unrelated projects. Pinning cwd keys
    // memory to the workspace, giving each agent a clean per-workspace
    // context. Skipped when the session has no workspace_id (transient
    // sessions) or the workspace is no longer in the index.
    if let Some(workspace_id) = session.context.workspace_id.as_deref() {
        if let Some(root) = deps
            .app
            .state::<crate::AppState>()
            .workspace_root(workspace_id)
        {
            command.current_dir(&root);
        }
    }

    let mut child = command.spawn().map_err(|e| {
        LocalAgentRunError::failed(format!(
            "Failed to launch `{}`: {}. Is Claude Code installed and on PATH?",
            binary, e
        ))
    })?;
    if let Some(mut stdin) = child.stdin.take() {
        use tokio::io::AsyncWriteExt;
        stdin
            .write_all(prompt.as_bytes())
            .await
            .map_err(|e| LocalAgentRunError::failed(format!("Failed to write prompt: {}", e)))?;
        drop(stdin);
    }
    let stderr_tail: Arc<Mutex<VecDeque<String>>> =
        Arc::new(Mutex::new(VecDeque::with_capacity(STDERR_TAIL_LINES)));
    if let Some(stderr) = child.stderr.take() {
        spawn_stderr_logger(
            run_id.to_string(),
            CliProviderRuntime::ClaudeCode.display_name(),
            stderr,
            stderr_tail.clone(),
        );
    }
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| LocalAgentRunError::failed("Claude stdout was not captured"))?;
    let mut lines = BufReader::new(stdout).lines();
    let mut state = ClaudeStreamState::new();
    let mut usage: Option<RunUsage> = None;
    let mut result_error: Option<String> = None;

    loop {
        let line = tokio::select! {
            _ = cancel_token.cancelled() => {
                let _ = child.kill().await;
                finalize_assistant_message(deps, session, run_id, &assistant_message, &state)
                    .await?;
                return Err(LocalAgentRunError::Cancelled { usage });
            }
            next = lines.next_line() => next
        }
        .map_err(|e| LocalAgentRunError::failed(e.to_string()))?;

        let Some(line) = line else {
            break;
        };
        if line.trim().is_empty() {
            continue;
        }
        log_cli_stream_line("claude-code", run_id, &line);
        let value: Value = serde_json::from_str(&line).map_err(|e| {
            LocalAgentRunError::failed(format!("Invalid Claude stream-json event: {}", e))
        })?;
        handle_claude_event(
            deps,
            session,
            run_id,
            &assistant_message,
            &value,
            &mut state,
            &mut usage,
            &mut result_error,
        )
        .await?;
    }

    let status = child
        .wait()
        .await
        .map_err(|e| LocalAgentRunError::failed(e.to_string()))?;
    finalize_assistant_message(deps, session, run_id, &assistant_message, &state).await?;

    if let Some(message) = result_error {
        let enriched = append_stderr_tail(&message, &stderr_tail);
        return Err(LocalAgentRunError::Failed {
            message: enriched,
            usage,
        });
    }
    if !status.success() {
        let base = format!("Claude Code exited with status {}", status);
        return Err(LocalAgentRunError::Failed {
            message: append_stderr_tail(&base, &stderr_tail),
            usage,
        });
    }
    Ok(usage)
}

#[allow(clippy::too_many_arguments)]
async fn run_codex_turn(
    deps: &AssistantDeps,
    session: &mut AssistantSession,
    connection: &ProviderConnection,
    run_id: &str,
    mcp_url: &str,
    mcp_token: &str,
    cancel_token: &CancellationToken,
    trigger: &crate::assistant::types::RunTrigger,
    assistant_slot: &mut Option<AssistantMessage>,
) -> Result<Option<RunUsage>, LocalAgentRunError> {
    let prompt = prepare_prompt(
        deps,
        session,
        run_id,
        trigger,
        CliProviderRuntime::Codex.metadata_source(),
        CliProviderRuntime::Codex.display_name(),
    )
    .await?;
    let system_prompt = system_prompt_text(&deps.app, session, trigger).await;
    let prompt = codex_turn_prompt(&system_prompt, &prompt);
    let existing_thread_id = session.context.cli_session_id.clone();

    let assistant_message = ensure_assistant_message_slot(
        deps,
        session,
        run_id,
        CliProviderRuntime::Codex.metadata_source(),
        assistant_slot,
    )
    .await?;

    let binary = connection
        .base_url
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("codex");
    let workspace_root = workspace_root_for_session(deps, session);
    let mut command = Command::new(binary);
    command.arg("exec");
    match existing_thread_id.as_deref() {
        Some(thread_id) => {
            command.arg("resume");
            add_codex_common_args(&mut command, connection, mcp_url, false, None);
            command.arg(thread_id);
        }
        None => {
            add_codex_common_args(
                &mut command,
                connection,
                mcp_url,
                true,
                workspace_root.as_ref(),
            );
        }
    }
    command
        .arg("-")
        .env(CODEX_MCP_TOKEN_ENV, mcp_token)
        .env("MCP_TIMEOUT", "3600000")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(root) = workspace_root.as_ref() {
        command.current_dir(root);
    }

    let mut child = command.spawn().map_err(|e| {
        LocalAgentRunError::failed(format!(
            "Failed to launch `{}`: {}. Is Codex CLI installed and on PATH?",
            binary, e
        ))
    })?;
    if let Some(mut stdin) = child.stdin.take() {
        use tokio::io::AsyncWriteExt;
        stdin
            .write_all(prompt.as_bytes())
            .await
            .map_err(|e| LocalAgentRunError::failed(format!("Failed to write prompt: {}", e)))?;
        drop(stdin);
    }

    let stderr_tail: Arc<Mutex<VecDeque<String>>> =
        Arc::new(Mutex::new(VecDeque::with_capacity(STDERR_TAIL_LINES)));
    if let Some(stderr) = child.stderr.take() {
        spawn_stderr_logger(
            run_id.to_string(),
            CliProviderRuntime::Codex.display_name(),
            stderr,
            stderr_tail.clone(),
        );
    }
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| LocalAgentRunError::failed("Codex stdout was not captured"))?;
    let mut lines = BufReader::new(stdout).lines();
    let mut state = CodexStreamState::new();
    let mut usage: Option<RunUsage> = None;
    let mut result_error: Option<String> = None;

    loop {
        let line = tokio::select! {
            _ = cancel_token.cancelled() => {
                let _ = child.kill().await;
                finalize_assistant_message_from_parts(
                    deps,
                    session,
                    run_id,
                    &assistant_message,
                    &state.parts,
                )
                .await?;
                return Err(LocalAgentRunError::Cancelled { usage });
            }
            next = lines.next_line() => next
        }
        .map_err(|e| LocalAgentRunError::failed(e.to_string()))?;

        let Some(line) = line else {
            break;
        };
        if line.trim().is_empty() {
            continue;
        }
        log_cli_stream_line("codex", run_id, &line);
        let value: Value = serde_json::from_str(&line)
            .map_err(|e| LocalAgentRunError::failed(format!("Invalid Codex JSONL event: {}", e)))?;
        handle_codex_event(
            deps,
            session,
            run_id,
            &assistant_message,
            &value,
            &mut state,
            &mut usage,
            &mut result_error,
        )
        .await?;
    }

    let status = child
        .wait()
        .await
        .map_err(|e| LocalAgentRunError::failed(e.to_string()))?;
    finalize_assistant_message_from_parts(deps, session, run_id, &assistant_message, &state.parts)
        .await?;

    if let Some(message) = result_error {
        let enriched = append_stderr_tail(&message, &stderr_tail);
        return Err(LocalAgentRunError::Failed {
            message: enriched,
            usage,
        });
    }
    if !status.success() {
        let base = format!("Codex exited with status {}", status);
        return Err(LocalAgentRunError::Failed {
            message: append_stderr_tail(&base, &stderr_tail),
            usage,
        });
    }
    Ok(usage)
}

fn codex_turn_prompt(system_prompt: &str, prompt: &str) -> String {
    format!(
        "System instructions for this CLAI run:\n{}\n\nUse the connected `clai` MCP tools for workspace work, shell execution, file access, and user interaction.\n\nUser/task prompt:\n{}",
        system_prompt, prompt
    )
}

fn workspace_root_for_session(deps: &AssistantDeps, session: &AssistantSession) -> Option<PathBuf> {
    session
        .context
        .workspace_id
        .as_deref()
        .and_then(|workspace_id| {
            deps.app
                .state::<crate::AppState>()
                .workspace_root(workspace_id)
        })
}

fn add_codex_common_args(
    command: &mut Command,
    connection: &ProviderConnection,
    mcp_url: &str,
    include_new_session_flags: bool,
    workspace_root: Option<&PathBuf>,
) {
    command
        .arg("--json")
        .arg("--skip-git-repo-check")
        .arg("--ignore-user-config")
        .arg("--ignore-rules")
        .arg("--disable")
        .arg("shell_tool")
        .arg("-c")
        .arg(format!(
            "mcp_servers.clai.url={}",
            toml_string_value(mcp_url)
        ))
        .arg("-c")
        .arg(format!(
            "mcp_servers.clai.bearer_token_env_var={}",
            toml_string_value(CODEX_MCP_TOKEN_ENV)
        ))
        .arg("-c")
        .arg("mcp_servers.clai.enabled=true")
        .arg("-c")
        .arg("mcp_servers.clai.required=true")
        .arg("-c")
        .arg("mcp_servers.clai.tool_timeout_sec=3600")
        // Bypass Codex's own approval/sandbox layer entirely — this is the
        // direct parallel of `--permission-mode bypassPermissions` that we pass
        // to Claude Code. CLAI provides the external sandbox (our MCP
        // `bash_exec` runs under bwrap) and permission system, and Codex's
        // native `shell_tool` is disabled, so Codex has no unmediated execution
        // path of its own. Without this, non-interactive `exec` has no approval
        // channel and auto-cancels every MCP tool call as "user cancelled MCP
        // tool call" — neither `approval_policy=never` nor `--sandbox` fixes
        // that, because the MCP call still goes through Codex's confirmation
        // gate. The flag is documented for exactly this "externally sandboxed"
        // case and is accepted by both `exec` and `exec resume`.
        .arg("--dangerously-bypass-approvals-and-sandbox");

    let model = connection.model_id.trim();
    if !model.is_empty() && model != "default" {
        command.arg("--model").arg(model);
    }

    if include_new_session_flags {
        if let Some(root) = workspace_root {
            command.arg("--cd").arg(root);
        }
    }
}

fn toml_string_value(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_string())
}

fn append_stderr_tail(message: &str, tail: &Arc<Mutex<VecDeque<String>>>) -> String {
    let Ok(buffer) = tail.lock() else {
        return message.to_string();
    };
    if buffer.is_empty() {
        return message.to_string();
    }
    let snippet: Vec<String> = buffer.iter().cloned().collect();
    format!("{}\n--- stderr ---\n{}", message, snippet.join("\n"))
}

async fn prepare_prompt(
    deps: &AssistantDeps,
    session: &AssistantSession,
    run_id: &str,
    trigger: &crate::assistant::types::RunTrigger,
    metadata_source: &str,
    provider_display_name: &str,
) -> Result<String, LocalAgentRunError> {
    if let Some(trigger_content) = build_trigger_message(session, trigger) {
        let boundary_msg = repository::create_message(
            &deps.pool,
            CreateMessageParams {
                session_id: session.id.clone(),
                role: trigger_content.role.clone(),
                content: trigger_content.content.clone(),
                provider_metadata: Some(serde_json::json!({
                    "source": format!("{}-trigger", metadata_source),
                })),
            },
        )
        .await?;
        let _ = emit_event(
            &deps.app,
            session,
            Some(run_id),
            AssistantUiEvent::MessageCreated {
                message: boundary_msg,
            },
        );
        return Ok(provider_message_text(&trigger_content));
    }

    let queued_messages =
        repository::list_delivered_queued_messages_for_run(&deps.pool, &session.id, run_id).await?;
    if !queued_messages.is_empty() {
        let messages: Vec<AssistantMessage> = queued_messages
            .into_iter()
            .map(|queued| queued.message)
            .collect();
        return Ok(queued_messages_prompt(&messages));
    }

    let messages = repository::list_messages(&deps.pool, &session.id).await?;
    messages
        .iter()
        .rev()
        .find(|message| message.role == MessageRole::User)
        .map(message_text)
        .filter(|text| !text.trim().is_empty())
        .ok_or_else(|| {
            LocalAgentRunError::failed(format!(
                "No user message found for {} run",
                provider_display_name
            ))
        })
}

fn queued_messages_prompt(messages: &[AssistantMessage]) -> String {
    if messages.len() == 1 {
        return message_text(&messages[0]);
    }

    let mut prompt =
        "The user sent these additional messages while you were working. Respond to them in order:"
            .to_string();
    for (index, message) in messages.iter().enumerate() {
        let text = message_text(message);
        if text.trim().is_empty() {
            continue;
        }
        prompt.push_str(&format!("\n\nMessage {}:\n{}", index + 1, text));
    }
    prompt
}

async fn system_prompt_text(
    app: &tauri::AppHandle,
    session: &AssistantSession,
    trigger: &crate::assistant::types::RunTrigger,
) -> String {
    let tool_defs = crate::assistant::tools::available_tools(&session.context, &[]);
    let description = crate::assistant::engine::live_agent_description(app, &session.context);
    provider_message_text(&build_system_prompt(
        &session.context,
        description.as_deref(),
        &tool_defs,
        trigger,
    ))
}

/// In-flight state of a Claude Code stream.
///
/// The stream interleaves text, thinking, and tool_use content blocks
/// (each indexed by `content_block` index). To preserve order in the
/// persisted assistant message we keep a single ordered `Vec<ContentPart>`
/// and remember which content_block index maps to which open part.
///
/// `persisted_tool_use_ids` lets us safely consume tool_use blocks from
/// either the streamed (`stream_event` deltas) or the complete
/// (`"type":"assistant"`) message envelope without double-persisting.
/// `pending_tool_results` buffers `tool_result` blocks that arrive
/// before their `tool_use` (rare, but possible when Claude Code emits
/// tool_use only in the complete assistant message): we replay the
/// buffer as soon as the matching tool_use is registered.
/// `last_update_emit_at` throttles `AssistantMessageUpdated` emissions
/// — without it a tool-heavy turn fires one full message-replacement
/// event per tool_use and React re-renders the entire chat tree each
/// time, wedging WebKit on long runs.
struct ClaudeStreamState {
    parts: Vec<ContentPart>,
    open_blocks: HashMap<u64, OpenBlock>,
    persisted_tool_use_ids: std::collections::HashSet<String>,
    pending_tool_results: HashMap<String, Value>,
    last_update_emit_at: Option<std::time::Instant>,
}

/// Minimum gap between consecutive `AssistantMessageUpdated` emissions.
/// The DB write still happens on every flush (so the persisted state is
/// always up to date), but the frontend gets coalesced updates at most
/// ~5/sec. The turn-final `AssistantMessageCompleted` always fires
/// regardless, so the user sees the final state immediately on
/// completion.
const ASSISTANT_UPDATE_EMIT_THROTTLE_MS: u128 = 200;

enum OpenBlock {
    /// Text block currently being streamed. Deltas append to
    /// `parts[parts_index]`.
    Text { parts_index: usize },
    /// Thinking block currently being streamed. Deltas append to
    /// `parts[parts_index]`.
    Thinking { parts_index: usize },
    /// Tool-use block being streamed. JSON input chunks accumulate in
    /// `accumulated_json` until `content_block_stop`, at which point we
    /// parse it, persist the tool_call record with the Claude-supplied
    /// `tool_call_id`, and push a `ContentPart::ToolUse` onto `parts`.
    ToolUse {
        tool_call_id: String,
        tool_name: String,
        accumulated_json: String,
    },
}

impl ClaudeStreamState {
    fn new() -> Self {
        Self {
            parts: Vec::new(),
            open_blocks: HashMap::new(),
            persisted_tool_use_ids: std::collections::HashSet::new(),
            pending_tool_results: HashMap::new(),
            last_update_emit_at: None,
        }
    }

    /// True iff the message accumulated any prose (non-empty Text part).
    /// Used by the `result` fallback to decide whether to inject the
    /// `result` summary text.
    fn has_text(&self) -> bool {
        self.parts
            .iter()
            .any(|p| matches!(p, ContentPart::Text { text } if !text.is_empty()))
    }

    fn last_part_is_text(&self) -> bool {
        matches!(self.parts.last(), Some(ContentPart::Text { .. }))
    }

    fn last_part_text_ends_with(&self, suffix: &str) -> bool {
        match self.parts.last() {
            Some(ContentPart::Text { text }) => text.ends_with(suffix),
            _ => false,
        }
    }

    fn append_to_last_text(&mut self, extra: &str) {
        if let Some(ContentPart::Text { text }) = self.parts.last_mut() {
            text.push_str(extra);
        }
    }
}

struct CodexStreamState {
    parts: Vec<ContentPart>,
    persisted_tool_item_ids: std::collections::HashSet<String>,
    tool_item_to_call_id: HashMap<String, String>,
    last_update_emit_at: Option<std::time::Instant>,
}

impl CodexStreamState {
    fn new() -> Self {
        Self {
            parts: Vec::new(),
            persisted_tool_item_ids: std::collections::HashSet::new(),
            tool_item_to_call_id: HashMap::new(),
            last_update_emit_at: None,
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn handle_codex_event(
    deps: &AssistantDeps,
    session: &mut AssistantSession,
    run_id: &str,
    assistant_message: &AssistantMessage,
    value: &Value,
    state: &mut CodexStreamState,
    usage: &mut Option<RunUsage>,
    result_error: &mut Option<String>,
) -> Result<(), LocalAgentRunError> {
    match value.get("type").and_then(Value::as_str) {
        Some("thread.started") => {
            if let Some(thread_id) = value.get("thread_id").and_then(Value::as_str) {
                set_cli_session_id(deps, session, thread_id.to_string(), CODEX_PROVIDER_ID).await?;
            }
        }
        Some("turn.completed") => {
            if let Some(parsed) = codex_usage_from_value(value.get("usage")) {
                *usage = Some(parsed);
            }
        }
        Some("turn.failed") => {
            *result_error = Some(
                value
                    .get("error")
                    .and_then(|error| error.get("message"))
                    .and_then(Value::as_str)
                    .unwrap_or("Codex run failed")
                    .to_string(),
            );
        }
        Some("error") => {
            *result_error = Some(
                value
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("Codex stream error")
                    .to_string(),
            );
        }
        Some("item.started") | Some("item.updated") | Some("item.completed") => {
            let terminal = value.get("type").and_then(Value::as_str) == Some("item.completed");
            if let Some(item) = value.get("item") {
                handle_codex_item(
                    deps,
                    session,
                    run_id,
                    assistant_message,
                    state,
                    item,
                    terminal,
                )
                .await?;
            }
        }
        _ => {}
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn handle_codex_item(
    deps: &AssistantDeps,
    session: &AssistantSession,
    run_id: &str,
    assistant_message: &AssistantMessage,
    state: &mut CodexStreamState,
    item: &Value,
    terminal: bool,
) -> Result<(), LocalAgentRunError> {
    match item.get("type").and_then(Value::as_str) {
        Some("agent_message") if terminal => {
            if let Some(text) = item.get("text").and_then(Value::as_str) {
                push_codex_text(state, text);
                let _ = emit_event(
                    &deps.app,
                    session,
                    Some(run_id),
                    AssistantUiEvent::AssistantDelta {
                        message_id: assistant_message.id.clone(),
                        text: text.to_string(),
                    },
                );
            }
        }
        Some("reasoning") if terminal => {
            if let Some(text) = item.get("text").and_then(Value::as_str) {
                state.parts.push(ContentPart::Thinking {
                    text: text.to_string(),
                    signature: None,
                });
                let _ = emit_event(
                    &deps.app,
                    session,
                    Some(run_id),
                    AssistantUiEvent::AssistantThinkingDelta {
                        message_id: assistant_message.id.clone(),
                        text: text.to_string(),
                    },
                );
            }
        }
        Some("mcp_tool_call") => {
            persist_codex_mcp_tool_use(deps, session, run_id, assistant_message, state, item)
                .await?;
            if terminal {
                apply_codex_mcp_tool_result(deps, session, run_id, state, item).await?;
            }
        }
        Some("command_execution") | Some("file_change") | Some("web_search") if terminal => {
            if let Some(summary) = codex_auxiliary_item_summary(item) {
                state.parts.push(ContentPart::Thinking {
                    text: summary.clone(),
                    signature: None,
                });
                let _ = emit_event(
                    &deps.app,
                    session,
                    Some(run_id),
                    AssistantUiEvent::AssistantThinkingDelta {
                        message_id: assistant_message.id.clone(),
                        text: summary,
                    },
                );
            }
        }
        Some("error") if terminal => {
            if let Some(message) = item.get("message").and_then(Value::as_str) {
                state.parts.push(ContentPart::Thinking {
                    text: message.to_string(),
                    signature: None,
                });
            }
        }
        _ => {}
    }
    Ok(())
}

fn push_codex_text(state: &mut CodexStreamState, text: &str) {
    if text.is_empty() {
        return;
    }
    if let Some(ContentPart::Text { text: existing }) = state.parts.last_mut() {
        if !existing.is_empty() && !existing.ends_with('\n') {
            existing.push_str("\n\n");
        }
        existing.push_str(text);
    } else {
        state.parts.push(ContentPart::Text {
            text: text.to_string(),
        });
    }
}

#[allow(clippy::too_many_arguments)]
async fn persist_codex_mcp_tool_use(
    deps: &AssistantDeps,
    session: &AssistantSession,
    run_id: &str,
    assistant_message: &AssistantMessage,
    state: &mut CodexStreamState,
    item: &Value,
) -> Result<(), LocalAgentRunError> {
    let raw_item_id = item
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    if state.persisted_tool_item_ids.contains(&raw_item_id) {
        return Ok(());
    }

    let tool_call_id = codex_tool_call_id(run_id, &raw_item_id);
    let tool_name = codex_tool_name(item);
    let params = item
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));

    let invocation = repository::create_tool_call(
        &deps.pool,
        CreateToolCallParams {
            id: tool_call_id.clone(),
            run_id: run_id.to_string(),
            session_id: session.id.clone(),
            tool_name: tool_name.clone(),
            params: params.clone(),
            status: ToolCallStatus::Running,
        },
    )
    .await
    .map_err(LocalAgentRunError::failed)?;

    let _ = emit_event(
        &deps.app,
        session,
        Some(run_id),
        AssistantUiEvent::ToolCallStarted {
            tool_call: invocation,
        },
    );

    state.parts.push(ContentPart::ToolUse {
        tool_call_id: tool_call_id.clone(),
        tool_name,
        arguments: params,
    });
    state.persisted_tool_item_ids.insert(raw_item_id.clone());
    state.tool_item_to_call_id.insert(raw_item_id, tool_call_id);
    flush_codex_assistant_message_content(deps, session, run_id, assistant_message, state).await
}

async fn apply_codex_mcp_tool_result(
    deps: &AssistantDeps,
    session: &AssistantSession,
    run_id: &str,
    state: &mut CodexStreamState,
    item: &Value,
) -> Result<(), LocalAgentRunError> {
    let raw_item_id = item
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| LocalAgentRunError::failed("Codex MCP tool item missing id"))?;
    let tool_call_id = state
        .tool_item_to_call_id
        .get(raw_item_id)
        .cloned()
        .unwrap_or_else(|| codex_tool_call_id(run_id, raw_item_id));

    let status_value = item.get("status").and_then(Value::as_str);
    let error_text = item
        .get("error")
        .and_then(|error| error.get("message"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let is_error = status_value == Some("failed") || error_text.is_some();
    let payload = if is_error {
        serde_json::json!({
            "error": error_text
                .clone()
                .unwrap_or_else(|| "MCP tool execution failed".to_string()),
        })
    } else {
        codex_mcp_result_payload(item.get("result"))
    };

    let updated = match repository::update_tool_call(
        &deps.pool,
        &tool_call_id,
        if is_error {
            ToolCallStatus::Failed
        } else {
            ToolCallStatus::Completed
        },
        (!is_error).then_some(&payload),
        error_text.as_deref(),
    )
    .await
    {
        Ok(tc) => tc,
        Err(err) => {
            tracing::warn!(
                tool_call_id = %tool_call_id,
                error = %err,
                "Codex MCP tool update failed even after tool_use was registered"
            );
            return Ok(());
        }
    };

    let started_at = updated.started_at;
    let completed_at = updated.completed_at;
    let ui_event = if is_error {
        AssistantUiEvent::ToolCallFailed { tool_call: updated }
    } else {
        AssistantUiEvent::ToolCallCompleted { tool_call: updated }
    };
    let _ = emit_event(&deps.app, session, Some(run_id), ui_event);

    let result_message = repository::create_message(
        &deps.pool,
        CreateMessageParams {
            session_id: session.id.clone(),
            role: MessageRole::Tool,
            content: vec![ContentPart::ToolResult {
                tool_call_id,
                payload,
                started_at: Some(started_at),
                completed_at,
            }],
            provider_metadata: Some(serde_json::json!({
                "source": CliProviderRuntime::Codex.metadata_source(),
            })),
        },
    )
    .await?;

    let _ = emit_event(
        &deps.app,
        session,
        Some(run_id),
        AssistantUiEvent::MessageCreated {
            message: result_message,
        },
    );
    Ok(())
}

fn codex_tool_call_id(run_id: &str, raw_item_id: &str) -> String {
    format!("codex:{}:{}", run_id, raw_item_id)
}

fn codex_tool_name(item: &Value) -> String {
    let server = item.get("server").and_then(Value::as_str).unwrap_or("mcp");
    let tool = item
        .get("tool")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    if server == "clai" {
        tool.to_string()
    } else {
        format!("{}/{}", server, tool)
    }
}

fn codex_mcp_result_payload(result: Option<&Value>) -> Value {
    let Some(result) = result else {
        return Value::Null;
    };
    if let Some(value) = result.get("structured_content") {
        if !value.is_null() {
            return value.clone();
        }
    }
    if let Some(value) = result.get("content") {
        if !value.is_null() {
            return value.clone();
        }
    }
    result.clone()
}

fn codex_auxiliary_item_summary(item: &Value) -> Option<String> {
    match item.get("type").and_then(Value::as_str)? {
        "command_execution" => {
            let command = item.get("command").and_then(Value::as_str)?;
            let status = item
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("completed");
            Some(format!("Codex command `{}` {}", command, status))
        }
        "file_change" => {
            let status = item
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("completed");
            Some(format!("Codex file change {}", status))
        }
        "web_search" => item
            .get("query")
            .and_then(Value::as_str)
            .map(|query| format!("Codex web search `{}`", query)),
        _ => None,
    }
}

fn codex_usage_from_value(value: Option<&Value>) -> Option<RunUsage> {
    let value = value?;
    let input_tokens = value
        .get("input_tokens")
        .and_then(Value::as_i64)
        .and_then(|v| u64::try_from(v).ok());
    let output_tokens = value
        .get("output_tokens")
        .and_then(Value::as_i64)
        .and_then(|v| u64::try_from(v).ok());
    let reasoning_tokens = value
        .get("reasoning_output_tokens")
        .and_then(Value::as_i64)
        .and_then(|v| u64::try_from(v).ok());
    let total_tokens = match (input_tokens, output_tokens, reasoning_tokens) {
        (None, None, None) => None,
        _ => Some(
            input_tokens.unwrap_or(0) + output_tokens.unwrap_or(0) + reasoning_tokens.unwrap_or(0),
        ),
    };
    Some(RunUsage {
        input_tokens,
        output_tokens,
        reasoning_tokens,
        total_tokens,
    })
}

async fn flush_codex_assistant_message_content(
    deps: &AssistantDeps,
    session: &AssistantSession,
    run_id: &str,
    assistant_message: &AssistantMessage,
    state: &mut CodexStreamState,
) -> Result<(), LocalAgentRunError> {
    let content = non_empty_content_parts(&state.parts);
    if content.is_empty() {
        return Ok(());
    }
    let updated =
        match repository::update_message_content(&deps.pool, &assistant_message.id, &content).await
        {
            Ok(m) => m,
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    message_id = %assistant_message.id,
                    "Failed to flush Codex assistant message content mid-turn"
                );
                return Ok(());
            }
        };
    let now = std::time::Instant::now();
    let should_emit = match state.last_update_emit_at {
        None => true,
        Some(last) => now.duration_since(last).as_millis() >= ASSISTANT_UPDATE_EMIT_THROTTLE_MS,
    };
    if should_emit {
        state.last_update_emit_at = Some(now);
        let _ = emit_event(
            &deps.app,
            session,
            Some(run_id),
            AssistantUiEvent::AssistantMessageUpdated { message: updated },
        );
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn handle_claude_event(
    deps: &AssistantDeps,
    session: &AssistantSession,
    run_id: &str,
    assistant_message: &AssistantMessage,
    value: &Value,
    state: &mut ClaudeStreamState,
    usage: &mut Option<RunUsage>,
    result_error: &mut Option<String>,
) -> Result<(), LocalAgentRunError> {
    match value.get("type").and_then(Value::as_str) {
        Some("stream_event") => {
            let event = value.get("event").unwrap_or(&Value::Null);
            handle_stream_event(
                deps,
                session,
                run_id,
                assistant_message,
                event,
                state,
                usage,
            )
            .await?;
        }
        Some("assistant") => {
            // Complete (non-partial) assistant message. We treat this
            // as the authoritative source of tool_use blocks — Claude
            // Code's `--include-partial-messages` is documented to
            // stream partial *text* deltas, but tool_use blocks may
            // only appear in this complete envelope. Adopting tool_use
            // blocks here, idempotent against the stream-event path
            // (skip ids we already saw), guarantees we never miss them.
            let message = value.get("message").unwrap_or(&Value::Null);
            adopt_complete_assistant_message(
                deps,
                session,
                run_id,
                assistant_message,
                message,
                state,
            )
            .await?;
        }
        Some("user") => {
            // Claude Code reports tool execution outputs by emitting the
            // matching tool_result blocks inside a synthesized `user`
            // message in the stream. We close out the corresponding
            // tool_call records here.
            let message = value.get("message").unwrap_or(&Value::Null);
            if let Some(content) = message.get("content").and_then(Value::as_array) {
                for block in content {
                    if block.get("type").and_then(Value::as_str) != Some("tool_result") {
                        continue;
                    }
                    handle_tool_result(deps, session, run_id, state, block).await?;
                }
            }
        }
        Some("result") => {
            if let Some(parsed) = usage_from_value(value.get("usage")) {
                *usage = Some(parsed);
            }
            if value
                .get("is_error")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                let errors_first = value
                    .get("errors")
                    .and_then(Value::as_array)
                    .and_then(|arr| arr.first())
                    .and_then(Value::as_str);
                *result_error = Some(
                    errors_first
                        .or_else(|| value.get("result").and_then(Value::as_str))
                        .or_else(|| value.get("error").and_then(Value::as_str))
                        .unwrap_or("Claude Code run failed")
                        .to_string(),
                );
            } else if !state.has_text() {
                if let Some(text) = value.get("result").and_then(Value::as_str) {
                    // Open a fresh text part for the trailing summary so
                    // it doesn't get appended to a stale tool_use part.
                    state.parts.push(ContentPart::Text {
                        text: text.to_string(),
                    });
                    let _ = emit_event(
                        &deps.app,
                        session,
                        Some(run_id),
                        AssistantUiEvent::AssistantDelta {
                            message_id: assistant_message.id.clone(),
                            text: text.to_string(),
                        },
                    );
                }
            }
        }
        Some("error") => {
            *result_error = Some(
                value
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("Claude Code stream error")
                    .to_string(),
            );
        }
        _ => {}
    }

    Ok(())
}

async fn handle_stream_event(
    deps: &AssistantDeps,
    session: &AssistantSession,
    run_id: &str,
    assistant_message: &AssistantMessage,
    event: &Value,
    state: &mut ClaudeStreamState,
    usage: &mut Option<RunUsage>,
) -> Result<(), LocalAgentRunError> {
    let event_type = event.get("type").and_then(Value::as_str);
    let block_index = event.get("index").and_then(Value::as_u64).unwrap_or(0);

    match event_type {
        Some("content_block_start") => {
            let block = event.get("content_block").unwrap_or(&Value::Null);
            match block.get("type").and_then(Value::as_str) {
                Some("text") => {
                    // Consecutive text blocks merge into one Text part
                    // with a "\n\n" paragraph separator so the
                    // downstream UI's `getTextContent` (which joins text
                    // parts with no separator) still renders breaks
                    // between Claude's individual text emissions.
                    if state.last_part_is_text() {
                        let separator = if state.last_part_text_ends_with("\n\n") {
                            ""
                        } else if state.last_part_text_ends_with("\n") {
                            "\n"
                        } else {
                            "\n\n"
                        };
                        if !separator.is_empty() {
                            state.append_to_last_text(separator);
                            let _ = emit_event(
                                &deps.app,
                                session,
                                Some(run_id),
                                AssistantUiEvent::AssistantDelta {
                                    message_id: assistant_message.id.clone(),
                                    text: separator.to_string(),
                                },
                            );
                        }
                        let parts_index = state.parts.len() - 1;
                        state
                            .open_blocks
                            .insert(block_index, OpenBlock::Text { parts_index });
                    } else {
                        state.parts.push(ContentPart::Text {
                            text: String::new(),
                        });
                        let parts_index = state.parts.len() - 1;
                        state
                            .open_blocks
                            .insert(block_index, OpenBlock::Text { parts_index });
                    }
                }
                Some("thinking") => {
                    state.parts.push(ContentPart::Thinking {
                        text: String::new(),
                        signature: None,
                    });
                    let parts_index = state.parts.len() - 1;
                    state
                        .open_blocks
                        .insert(block_index, OpenBlock::Thinking { parts_index });
                }
                Some("tool_use") => {
                    let tool_call_id = block
                        .get("id")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                        .unwrap_or_else(|| Uuid::new_v4().to_string());
                    let tool_name = block
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    state.open_blocks.insert(
                        block_index,
                        OpenBlock::ToolUse {
                            tool_call_id,
                            tool_name,
                            accumulated_json: String::new(),
                        },
                    );
                }
                _ => {}
            }
        }
        Some("content_block_delta") => {
            let delta = event.get("delta").unwrap_or(&Value::Null);
            match delta.get("type").and_then(Value::as_str) {
                Some("text_delta") => {
                    if let Some(text) = delta.get("text").and_then(Value::as_str) {
                        // Route to the Text part that content_block_start
                        // opened at this block_index. If we never saw a
                        // start (defensive — rare for Claude Code with
                        // partial messages enabled), fall back to the
                        // last Text part or create one.
                        let parts_index = match state.open_blocks.get(&block_index) {
                            Some(OpenBlock::Text { parts_index }) => Some(*parts_index),
                            _ => {
                                if state.last_part_is_text() {
                                    Some(state.parts.len() - 1)
                                } else {
                                    state.parts.push(ContentPart::Text {
                                        text: String::new(),
                                    });
                                    Some(state.parts.len() - 1)
                                }
                            }
                        };
                        if let Some(idx) = parts_index {
                            if let Some(ContentPart::Text { text: t }) = state.parts.get_mut(idx) {
                                t.push_str(text);
                            }
                        }
                        let _ = emit_event(
                            &deps.app,
                            session,
                            Some(run_id),
                            AssistantUiEvent::AssistantDelta {
                                message_id: assistant_message.id.clone(),
                                text: text.to_string(),
                            },
                        );
                    }
                }
                Some("thinking_delta") | Some("signature_delta") => {
                    if let Some(text) = delta
                        .get("thinking")
                        .or_else(|| delta.get("text"))
                        .and_then(Value::as_str)
                    {
                        let parts_index = match state.open_blocks.get(&block_index) {
                            Some(OpenBlock::Thinking { parts_index }) => Some(*parts_index),
                            _ => {
                                if matches!(state.parts.last(), Some(ContentPart::Thinking { .. }))
                                {
                                    Some(state.parts.len() - 1)
                                } else {
                                    state.parts.push(ContentPart::Thinking {
                                        text: String::new(),
                                        signature: None,
                                    });
                                    Some(state.parts.len() - 1)
                                }
                            }
                        };
                        if let Some(idx) = parts_index {
                            if let Some(ContentPart::Thinking { text: t, .. }) =
                                state.parts.get_mut(idx)
                            {
                                t.push_str(text);
                            }
                        }
                        let _ = emit_event(
                            &deps.app,
                            session,
                            Some(run_id),
                            AssistantUiEvent::AssistantThinkingDelta {
                                message_id: assistant_message.id.clone(),
                                text: text.to_string(),
                            },
                        );
                    }
                }
                Some("input_json_delta") => {
                    if let Some(partial) = delta.get("partial_json").and_then(Value::as_str) {
                        if let Some(OpenBlock::ToolUse {
                            accumulated_json, ..
                        }) = state.open_blocks.get_mut(&block_index)
                        {
                            accumulated_json.push_str(partial);
                        }
                    }
                }
                _ => {}
            }
        }
        Some("content_block_stop") => {
            if let Some(OpenBlock::ToolUse {
                tool_call_id,
                tool_name,
                accumulated_json,
            }) = state.open_blocks.remove(&block_index)
            {
                if !state.persisted_tool_use_ids.contains(&tool_call_id) {
                    let params: Value = if accumulated_json.is_empty() {
                        serde_json::json!({})
                    } else {
                        serde_json::from_str(&accumulated_json)
                            .unwrap_or_else(|_| serde_json::json!({}))
                    };

                    persist_tool_use(
                        deps,
                        session,
                        run_id,
                        assistant_message,
                        state,
                        &tool_call_id,
                        &tool_name,
                        params,
                    )
                    .await?;
                }
            }
        }
        Some("message_start") | Some("message_delta") => {
            if let Some(parsed) = usage_from_value(
                event
                    .get("usage")
                    .or_else(|| event.get("message").and_then(|m| m.get("usage"))),
            ) {
                *usage = Some(parsed);
            }
        }
        _ => {}
    }

    Ok(())
}

/// Parse a single tool_result content block (from a synthesized "user"
/// stream message) and close out the matching tool_call record.
///
/// If we haven't yet seen the matching `tool_use` (which can happen
/// when Claude Code emits tool_use only in the trailing
/// `"type":"assistant"` complete-message envelope rather than as
/// streamed `content_block_start`/`content_block_stop` deltas), the
/// raw block is buffered and replayed once `persist_tool_use` registers
/// the id.
async fn handle_tool_result(
    deps: &AssistantDeps,
    session: &AssistantSession,
    run_id: &str,
    state: &mut ClaudeStreamState,
    block: &Value,
) -> Result<(), LocalAgentRunError> {
    let tool_use_id = match block.get("tool_use_id").and_then(Value::as_str) {
        Some(id) if !id.is_empty() => id.to_string(),
        _ => return Ok(()),
    };
    if !state.persisted_tool_use_ids.contains(&tool_use_id) {
        // No matching tool_use yet — buffer and replay later.
        state
            .pending_tool_results
            .insert(tool_use_id, block.clone());
        return Ok(());
    }
    apply_tool_result(deps, session, run_id, &tool_use_id, block).await
}

/// Persist a tool_use block (from either streamed or complete envelopes)
/// and emit its `ToolCallStarted` UI event. Idempotent: callers should
/// guard with `state.persisted_tool_use_ids.contains(id)` before calling,
/// but if the DB INSERT fails because the row already exists we just
/// surface the error to the caller (it's unexpected at that point).
///
/// Crucially, this also flushes the growing assistant message content
/// to the DB and emits `AssistantMessageUpdated` so the chat surfaces
/// tool calls *live*. Without this step, mid-turn tool_use parts
/// stay in memory only and the UI sees just an empty bubble until the
/// whole Claude Code turn finalizes — which can take a long time when
/// Claude makes many tool calls in a row.
#[allow(clippy::too_many_arguments)]
async fn persist_tool_use(
    deps: &AssistantDeps,
    session: &AssistantSession,
    run_id: &str,
    assistant_message: &AssistantMessage,
    state: &mut ClaudeStreamState,
    tool_call_id: &str,
    tool_name: &str,
    params: Value,
) -> Result<(), LocalAgentRunError> {
    let invocation = repository::create_tool_call(
        &deps.pool,
        CreateToolCallParams {
            id: tool_call_id.to_string(),
            run_id: run_id.to_string(),
            session_id: session.id.clone(),
            tool_name: tool_name.to_string(),
            params: params.clone(),
            status: ToolCallStatus::Running,
        },
    )
    .await
    .map_err(LocalAgentRunError::failed)?;

    let _ = emit_event(
        &deps.app,
        session,
        Some(run_id),
        AssistantUiEvent::ToolCallStarted {
            tool_call: invocation,
        },
    );

    state.parts.push(ContentPart::ToolUse {
        tool_call_id: tool_call_id.to_string(),
        tool_name: tool_name.to_string(),
        arguments: params,
    });
    state
        .persisted_tool_use_ids
        .insert(tool_call_id.to_string());

    // Flush the running parts vec into the assistant message so the
    // chat UI's tool_use renderer can pick it up immediately. The DB
    // write happens every call; the AssistantMessageUpdated event is
    // throttled inside flush_assistant_message_content to avoid an
    // event storm on tool-heavy turns.
    flush_assistant_message_content(deps, session, run_id, assistant_message, state).await?;

    // If a tool_result arrived before this tool_use was registered (e.g.
    // tool_use only present in the complete assistant message), flush it
    // now.
    if let Some(pending) = state.pending_tool_results.remove(tool_call_id) {
        apply_tool_result(deps, session, run_id, tool_call_id, &pending).await?;
    }

    Ok(())
}

/// Push the in-memory `state.parts` to the assistant message row and,
/// when not throttled, emit `AssistantMessageUpdated`.
///
/// The DB UPDATE runs on every call (cheap, single-row write) so the
/// persisted state is always current. The frontend event is coalesced
/// to at most one emission per `ASSISTANT_UPDATE_EMIT_THROTTLE_MS` — on
/// a tool-heavy turn (e.g. 35 sequential tool_uses) un-throttled
/// emissions would re-render the entire chat tree dozens of times,
/// pinning WebKit at 100%+ CPU. The turn-final
/// `AssistantMessageCompleted` is always emitted by
/// `finalize_assistant_message`, so the user sees the final state
/// immediately when the run ends regardless of throttling.
async fn flush_assistant_message_content(
    deps: &AssistantDeps,
    session: &AssistantSession,
    run_id: &str,
    assistant_message: &AssistantMessage,
    state: &mut ClaudeStreamState,
) -> Result<(), LocalAgentRunError> {
    let content: Vec<ContentPart> = state
        .parts
        .iter()
        .filter(|p| !matches!(p, ContentPart::Text { text } if text.is_empty()))
        .cloned()
        .collect();
    if content.is_empty() {
        return Ok(());
    }
    let updated =
        match repository::update_message_content(&deps.pool, &assistant_message.id, &content).await
        {
            Ok(m) => m,
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    message_id = %assistant_message.id,
                    "Failed to flush assistant message content mid-turn"
                );
                return Ok(());
            }
        };

    let now = std::time::Instant::now();
    let should_emit = match state.last_update_emit_at {
        None => true,
        Some(last) => now.duration_since(last).as_millis() >= ASSISTANT_UPDATE_EMIT_THROTTLE_MS,
    };
    if should_emit {
        state.last_update_emit_at = Some(now);
        let _ = emit_event(
            &deps.app,
            session,
            Some(run_id),
            AssistantUiEvent::AssistantMessageUpdated { message: updated },
        );
    }
    Ok(())
}

/// Walks a complete `"type":"assistant"` envelope and consumes any
/// content blocks the streamed deltas didn't already cover. Tool_use is
/// the main reason this exists — partial-message streaming in Claude
/// Code may not include tool_use deltas, so we treat the complete
/// envelope as authoritative for that. Text/thinking blocks are skipped
/// here because the streamed path already accumulated them; re-adding
/// would double up.
async fn adopt_complete_assistant_message(
    deps: &AssistantDeps,
    session: &AssistantSession,
    run_id: &str,
    assistant_message: &AssistantMessage,
    message: &Value,
    state: &mut ClaudeStreamState,
) -> Result<(), LocalAgentRunError> {
    let Some(content) = message.get("content").and_then(Value::as_array) else {
        return Ok(());
    };
    for block in content {
        let Some(block_type) = block.get("type").and_then(Value::as_str) else {
            continue;
        };
        if block_type != "tool_use" {
            continue;
        }
        let tool_use_id = match block.get("id").and_then(Value::as_str) {
            Some(id) if !id.is_empty() => id.to_string(),
            _ => continue,
        };
        if state.persisted_tool_use_ids.contains(&tool_use_id) {
            continue;
        }
        let tool_name = block
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let input = block.get("input").cloned().unwrap_or(serde_json::json!({}));
        persist_tool_use(
            deps,
            session,
            run_id,
            assistant_message,
            state,
            &tool_use_id,
            &tool_name,
            input,
        )
        .await?;
    }
    Ok(())
}

/// Update the persisted tool_call record from a `tool_result` block,
/// emit the matching `ToolCallCompleted` / `ToolCallFailed` UI event,
/// and create the Tool-role message whose `ContentPart::ToolResult`
/// carries the payload the chat UI renders.
async fn apply_tool_result(
    deps: &AssistantDeps,
    session: &AssistantSession,
    run_id: &str,
    tool_use_id: &str,
    block: &Value,
) -> Result<(), LocalAgentRunError> {
    let is_error = block
        .get("is_error")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let payload = block.get("content").cloned().unwrap_or(Value::Null);

    let (status, result_arg, error_arg) = if is_error {
        let error_text = extract_text_from_tool_result(&payload)
            .unwrap_or_else(|| "Tool execution failed".to_string());
        (ToolCallStatus::Failed, None, Some(error_text))
    } else {
        (ToolCallStatus::Completed, Some(payload.clone()), None)
    };

    let updated = match repository::update_tool_call(
        &deps.pool,
        tool_use_id,
        status.clone(),
        result_arg.as_ref(),
        error_arg.as_deref(),
    )
    .await
    {
        Ok(tc) => tc,
        Err(err) => {
            tracing::warn!(
                tool_use_id = %tool_use_id,
                error = %err,
                "Claude tool_result update failed even after tool_use was registered"
            );
            return Ok(());
        }
    };

    let started_at = updated.started_at;
    let completed_at = updated.completed_at;

    let ui_event = if is_error {
        AssistantUiEvent::ToolCallFailed { tool_call: updated }
    } else {
        AssistantUiEvent::ToolCallCompleted { tool_call: updated }
    };
    let _ = emit_event(&deps.app, session, Some(run_id), ui_event);

    let result_message = repository::create_message(
        &deps.pool,
        CreateMessageParams {
            session_id: session.id.clone(),
            role: MessageRole::Tool,
            content: vec![ContentPart::ToolResult {
                tool_call_id: tool_use_id.to_string(),
                payload,
                started_at: Some(started_at),
                completed_at,
            }],
            provider_metadata: Some(serde_json::json!({
                "source": "claude-code",
            })),
        },
    )
    .await?;

    let _ = emit_event(
        &deps.app,
        session,
        Some(run_id),
        AssistantUiEvent::MessageCreated {
            message: result_message,
        },
    );

    Ok(())
}

/// Best-effort extraction of a text representation from a tool_result's
/// `content` field. Claude Code emits it as either a string or an array
/// of content blocks (each typically `{ "type": "text", "text": "..." }`).
fn extract_text_from_tool_result(payload: &Value) -> Option<String> {
    if let Some(s) = payload.as_str() {
        return Some(s.to_string());
    }
    if let Some(arr) = payload.as_array() {
        let mut buf = String::new();
        for block in arr {
            if let Some(t) = block.get("text").and_then(Value::as_str) {
                if !buf.is_empty() {
                    buf.push('\n');
                }
                buf.push_str(t);
            }
        }
        if !buf.is_empty() {
            return Some(buf);
        }
    }
    None
}

async fn finalize_assistant_message(
    deps: &AssistantDeps,
    session: &AssistantSession,
    run_id: &str,
    assistant_message: &AssistantMessage,
    state: &ClaudeStreamState,
) -> Result<(), LocalAgentRunError> {
    finalize_assistant_message_from_parts(deps, session, run_id, assistant_message, &state.parts)
        .await
}

async fn finalize_assistant_message_from_parts(
    deps: &AssistantDeps,
    session: &AssistantSession,
    run_id: &str,
    assistant_message: &AssistantMessage,
    parts: &[ContentPart],
) -> Result<(), LocalAgentRunError> {
    // Build the final content from the ordered parts vec. Drop empty
    // Text parts (left over when a turn was purely tool calls — the
    // assistant_message row was seeded with a placeholder empty Text
    // that we no longer need). If everything came out empty (e.g. a
    // run that errored before any content was emitted) we still write
    // a single empty Text to keep the message row schema consistent
    // with what the UI expects.
    let mut content = non_empty_content_parts(parts);
    if content.is_empty() {
        content.push(ContentPart::Text {
            text: String::new(),
        });
    }

    let updated =
        repository::update_message_content(&deps.pool, &assistant_message.id, &content).await?;
    let _ = emit_event(
        &deps.app,
        session,
        Some(run_id),
        AssistantUiEvent::AssistantMessageCompleted { message: updated },
    );
    Ok(())
}

fn non_empty_content_parts(parts: &[ContentPart]) -> Vec<ContentPart> {
    parts
        .iter()
        .filter(|p| !matches!(p, ContentPart::Text { text } if text.is_empty()))
        .cloned()
        .collect()
}

fn provider_message_text(message: &ProviderInputMessage) -> String {
    message
        .content
        .iter()
        .filter_map(content_part_text)
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn message_text(message: &AssistantMessage) -> String {
    message
        .content
        .iter()
        .filter_map(content_part_text)
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn content_part_text(part: &ContentPart) -> Option<String> {
    match part {
        ContentPart::Text { text } | ContentPart::Thinking { text, .. } => Some(text.clone()),
        ContentPart::ToolUse {
            tool_name,
            arguments,
            ..
        } => Some(format!("Tool use `{}`: {}", tool_name, arguments)),
        ContentPart::ToolResult { payload, .. } => Some(format!("Tool result: {}", payload)),
    }
}

fn usage_from_value(value: Option<&Value>) -> Option<RunUsage> {
    let value = value?;
    let input_tokens = value.get("input_tokens").and_then(Value::as_u64);
    let output_tokens = value.get("output_tokens").and_then(Value::as_u64);
    let reasoning_tokens = value.get("reasoning_tokens").and_then(Value::as_u64);
    let total_tokens = match (input_tokens, output_tokens, reasoning_tokens) {
        (None, None, None) => None,
        _ => Some(
            input_tokens.unwrap_or(0) + output_tokens.unwrap_or(0) + reasoning_tokens.unwrap_or(0),
        ),
    };
    Some(RunUsage {
        input_tokens,
        output_tokens,
        reasoning_tokens,
        total_tokens,
    })
}

fn write_mcp_config(url: &str, token: &str) -> Result<PathBuf, LocalAgentRunError> {
    let dir = dirs::config_dir()
        .ok_or_else(|| LocalAgentRunError::failed("Could not determine config directory"))?
        .join("clai")
        .join("tmp");
    std::fs::create_dir_all(&dir).map_err(|e| LocalAgentRunError::failed(e.to_string()))?;
    let path = dir.join(format!("claude-mcp-{}.json", Uuid::new_v4()));
    let config = serde_json::json!({
        "mcpServers": {
            "clai": {
                "type": "http",
                "url": url,
                "headers": {
                    "Authorization": format!("Bearer {}", token)
                }
            }
        }
    });
    std::fs::write(
        &path,
        serde_json::to_vec_pretty(&config)
            .map_err(|e| LocalAgentRunError::failed(e.to_string()))?,
    )
    .map_err(|e| LocalAgentRunError::failed(e.to_string()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(&path)
            .map_err(|e| LocalAgentRunError::failed(e.to_string()))?
            .permissions();
        permissions.set_mode(0o600);
        std::fs::set_permissions(&path, permissions)
            .map_err(|e| LocalAgentRunError::failed(e.to_string()))?;
    }

    Ok(path)
}

fn spawn_stderr_logger(
    run_id: String,
    provider_name: &'static str,
    stderr: tokio::process::ChildStderr,
    tail: Arc<Mutex<VecDeque<String>>>,
) {
    tauri::async_runtime::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            tracing::warn!(run_id = %run_id, provider = %provider_name, stderr = %line, "CLI provider stderr");
            if let Ok(mut buffer) = tail.lock() {
                if buffer.len() == STDERR_TAIL_LINES {
                    buffer.pop_front();
                }
                buffer.push_back(line);
            }
        }
    });
}

async fn fail_run(
    deps: &AssistantDeps,
    session: &AssistantSession,
    run_id: &str,
    usage: Option<&RunUsage>,
    error_msg: &str,
) -> Result<(), AssistantEngineError> {
    let run = repository::complete_run(
        &deps.pool,
        run_id,
        RunStatus::Failed,
        usage,
        Some(error_msg),
        &[],
    )
    .await?;
    let _ = emit_event(
        &deps.app,
        session,
        Some(run_id),
        AssistantUiEvent::RunFailed { run },
    );
    Ok(())
}

async fn cancel_run(
    deps: &AssistantDeps,
    session: &AssistantSession,
    run_id: &str,
    usage: Option<&RunUsage>,
) -> Result<(), AssistantEngineError> {
    let run = repository::complete_run(&deps.pool, run_id, RunStatus::Cancelled, usage, None, &[])
        .await?;
    let _ = emit_event(
        &deps.app,
        session,
        Some(run_id),
        AssistantUiEvent::RunCancelled { run },
    );
    Ok(())
}

enum LocalAgentRunError {
    Cancelled {
        usage: Option<RunUsage>,
    },
    Failed {
        message: String,
        usage: Option<RunUsage>,
    },
}

impl LocalAgentRunError {
    fn failed(message: impl Into<String>) -> Self {
        Self::Failed {
            message: message.into(),
            usage: None,
        }
    }

    fn message(self) -> String {
        match self {
            LocalAgentRunError::Cancelled { .. } => "run cancelled".to_string(),
            LocalAgentRunError::Failed { message, .. } => message,
        }
    }
}

impl From<String> for LocalAgentRunError {
    fn from(value: String) -> Self {
        LocalAgentRunError::failed(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_real_usage_limit_messages() {
        // Captured verbatim from the live CLI streams.
        assert!(is_usage_limit_error(
            "You've hit your usage limit. To get more access now, send a request to your admin or try again at 9:47 PM."
        ));
        assert!(is_usage_limit_error(
            "You've hit your session limit · resets 4:40pm (Europe/Madrid)"
        ));
        // Common API-style phrasings should also classify.
        assert!(is_usage_limit_error("rate limit exceeded"));
        assert!(is_usage_limit_error("Error: rate_limit_error"));
        assert!(is_usage_limit_error("You have exceeded your quota"));
    }

    #[test]
    fn does_not_flag_unrelated_failures() {
        assert!(!is_usage_limit_error(
            "Codex exited with status exit status: 2"
        ));
        assert!(!is_usage_limit_error("user cancelled MCP tool call"));
        assert!(!is_usage_limit_error(
            "No conversation found with session ID"
        ));
        assert!(!is_usage_limit_error(""));
    }

    #[test]
    fn codex_resume_failures_are_session_lost() {
        // Real message seen when resuming a thread id Codex doesn't own (e.g.
        // a stale id from a different provider after switching Claude→Codex).
        assert!(is_session_lost_error(
            CliProviderRuntime::Codex,
            "Codex exited with status exit status: 1 --- stderr --- Error: thread/resume: thread/resume failed: no rollout found for thread id 729ca14e-b692-4679-aee5-375bac0fb91e (code -32600)"
        ));
        assert!(is_session_lost_error(
            CliProviderRuntime::Codex,
            "Session not found"
        ));
        // A usage limit is NOT a session-loss — must not trigger the reset path.
        assert!(!is_session_lost_error(
            CliProviderRuntime::Codex,
            "You've hit your usage limit. Try again at 9:47 PM."
        ));
        // Claude's resume failure stays matched on the Claude side only.
        assert!(is_session_lost_error(
            CliProviderRuntime::ClaudeCode,
            "No conversation found with session ID abc"
        ));
        assert!(!is_session_lost_error(
            CliProviderRuntime::ClaudeCode,
            "no rollout found for thread id abc"
        ));
    }

    #[test]
    fn codex_usage_maps_jsonl_turn_usage() {
        let usage = codex_usage_from_value(Some(&serde_json::json!({
            "input_tokens": 10,
            "output_tokens": 7,
            "reasoning_output_tokens": 3
        })))
        .expect("usage");

        assert_eq!(usage.input_tokens, Some(10));
        assert_eq!(usage.output_tokens, Some(7));
        assert_eq!(usage.reasoning_tokens, Some(3));
        assert_eq!(usage.total_tokens, Some(20));
    }

    #[test]
    fn codex_mcp_result_prefers_structured_content() {
        let payload = codex_mcp_result_payload(Some(&serde_json::json!({
            "content": [{"type": "text", "text": "fallback"}],
            "structured_content": {"ok": true}
        })));

        assert_eq!(payload, serde_json::json!({"ok": true}));
    }

    #[test]
    fn codex_tool_call_ids_are_run_scoped() {
        assert_eq!(codex_tool_call_id("run-a", "item_0"), "codex:run-a:item_0");
    }
}
