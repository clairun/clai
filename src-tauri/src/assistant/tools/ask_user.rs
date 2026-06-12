//! `ask_user` tool — surface a question to the human in the chat and
//! block the agent's run until they answer.
//!
//! This is the only mechanism the LLM has for asking the user a question
//! that needs a synchronous decision. It replaces the old
//! `request_user_input` channel, which created a separate `workspace_tasks`
//! row in `needs_user_input` state, surfaced via Fleet/workspace attention
//! chips and a side-panel feedback form. That parallel surface was a UX
//! deadweight: questions lived outside the chat thread, answers had to be
//! re-injected as a synthetic user message, and the run had to be torn
//! down and re-spawned. Here the question is the tool_use that lands in
//! the chat naturally, the tool itself awaits the user's reply, and the
//! same MCP turn carries the answer back to the model.
//!
//! Mechanics:
//! 1. The LLM calls `ask_user` with a question and (optionally) structured
//!    options. The tool generates a pending id, registers a oneshot
//!    sender keyed by that id, and emits `AskUserRequested` to the FE.
//! 2. The FE renders an inline answer block in the conversation, with
//!    radio buttons + "Other" free-text for option-bearing questions, or
//!    a plain textarea otherwise.
//! 3. When the user submits, the FE calls `assistant_submit_user_input`
//!    (see `commands::assistant`), which fires the channel.
//! 4. The tool returns the answer as the tool_result so the LLM continues
//!    in the same run.
//!
//! Cancellation: the outer `tokio::select!` in
//! `assistant::local_mcp::execute_bound_tool` races our `rx.await` against
//! the run's `cancel_token`. If the run is cancelled while we're blocked,
//! our future is dropped — the `PendingGuard` removes the entry from the
//! global map, clears the inline panel, and cancels the run so the model
//! cannot continue without the missing answer.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;
use uuid::Uuid;

use crate::assistant::engine::AssistantDeps;
use crate::assistant::events::{emit_event, AssistantUiEvent};
use crate::assistant::repository;
use crate::assistant::tools::ToolExecutionContext;

const ASK_USER_TIMEOUT: Duration = Duration::from_secs(24 * 60 * 60);

// `deny_unknown_fields` keeps serde behavior aligned with the advertised
// schema's `additionalProperties: false` (schemars derives the latter from
// the former, and the router validates calls against that schema before
// dispatch — all three layers agree).
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AskUserParams {
    /// The question to surface to the user. Rendered as the prompt of
    /// the inline answer block in the chat.
    pub question: String,
    /// Optional list of structured choices. When present the user picks
    /// one (or chooses "Other" to free-text). When absent the answer is
    /// a plain textarea.
    #[serde(default)]
    pub options: Option<Vec<AskUserOption>>,
    /// When true (and `options` is present), the user may select MULTIPLE
    /// options instead of exactly one. The result then carries every
    /// selected label joined in `answer` plus `selectedOptionIndexes`.
    /// Ignored for questions without options. Defaults to false.
    #[serde(default)]
    pub multi_select: Option<bool>,
    /// Optional extra context to display alongside the question (e.g.
    /// what the agent has tried, what tradeoffs to weigh).
    #[serde(default)]
    pub context: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export, export_to = "bindings.ts")]
pub struct AskUserOption {
    pub label: String,
    #[serde(default)]
    pub description: Option<String>,
}

/// The answer payload delivered back from the FE through the Tauri
/// command and into the waiting tool.
#[derive(Debug, Clone)]
pub struct AskUserAnswer {
    pub text: String,
    /// Single-select questions: the picked option's index.
    pub selected_option_index: Option<usize>,
    /// Multi-select questions: every picked option's index, in option
    /// order. `None` for single-select / free-text answers.
    pub selected_option_indexes: Option<Vec<usize>>,
}

// Keyed by pending id; the value carries the owning session id so callers
// (mid-run input delivery) can check whether a session is currently blocked
// on a human answer without threading new state through the tool router.
#[derive(Debug)]
enum AskUserOutcome {
    Answer(AskUserAnswer),
    Superseded,
}

type PendingMap = HashMap<String, (String, oneshot::Sender<AskUserOutcome>)>;
static PENDING: OnceLock<Mutex<PendingMap>> = OnceLock::new();

fn pending_map() -> &'static Mutex<PendingMap> {
    PENDING.get_or_init(|| Mutex::new(HashMap::new()))
}

/// True while this session has an `ask_user` question awaiting the human.
/// Mid-run input delivery checks this before interrupting a live CLI turn:
/// interrupting would tear down the question the user is being asked.
pub fn session_has_pending_ask(session_id: &str) -> bool {
    pending_map()
        .lock()
        .map(|map| map.values().any(|(owner, _)| owner == session_id))
        .unwrap_or(false)
}

/// Deliver an answer to a waiting `ask_user` invocation. Called from the
/// `assistant_submit_user_input` Tauri command after the user submits
/// the inline answer block.
pub fn submit_answer(pending_id: &str, answer: AskUserAnswer) -> Result<(), String> {
    let mut map = pending_map()
        .lock()
        .map_err(|e| format!("ask_user pending map poisoned: {}", e))?;
    let (_, tx) = map
        .remove(pending_id)
        .ok_or_else(|| format!("No pending ask_user with id `{}`", pending_id))?;
    tx.send(AskUserOutcome::Answer(answer))
        .map_err(|_| "ask_user receiver was dropped (run already ended)".to_string())
}

/// Removes every pending ask for `session_id`, returning the removed
/// pending ids. Called when a new ask is registered for the session: the
/// frontend renders a single ask panel per session, so a still-pending
/// ask at registration time is an orphan (its CLI transport dropped
/// mid-call and the model is re-asking). The removed senders receive an
/// explicit superseded outcome, so channel closure remains reserved for
/// cancellation/teardown.
pub fn take_for_session(session_id: &str) -> Vec<String> {
    let Ok(mut map) = pending_map().lock() else {
        return Vec::new();
    };
    let ids: Vec<String> = map
        .iter()
        .filter(|(_, (owner, _))| owner == session_id)
        .map(|(id, _)| id.clone())
        .collect();
    for id in &ids {
        if let Some((_, tx)) = map.remove(id) {
            let _ = tx.send(AskUserOutcome::Superseded);
        }
    }
    ids
}

/// RAII guard: removes the pending entry if the tool future is dropped
/// before the channel resolves — run cancellation, or run-end reaping of
/// a wait orphaned by a CLI transport drop (`BindingGuard` cancels the
/// run scope, dropping this future). In that case the answer panel
/// cannot resume the original tool call, so clear the UI and cancel the
/// run rather than letting the model continue without the requested
/// human answer (a no-op when the run already ended).
struct PendingGuard {
    id: String,
    app: tauri::AppHandle,
    session: crate::assistant::types::AssistantSession,
    run_id: String,
    cancel_token: tokio_util::sync::CancellationToken,
    armed: bool,
}

impl PendingGuard {
    fn disarm(&mut self) {
        self.armed = false;
    }

    async fn expire_and_stop<T>(&mut self) -> T {
        if let Ok(mut map) = pending_map().lock() {
            map.remove(&self.id);
        }
        let _ = emit_event(
            &self.app,
            &self.session,
            Some(self.run_id.as_str()),
            AssistantUiEvent::AskUserResolved {
                pending_id: self.id.clone(),
            },
        );
        self.armed = false;
        super::cancel_run_and_park(&self.cancel_token).await
    }
}

impl Drop for PendingGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        self.cancel_token.cancel();
        if let Ok(mut map) = pending_map().lock() {
            map.remove(&self.id);
        }
        let app = self.app.clone();
        let session = self.session.clone();
        let run_id = self.run_id.clone();
        let pending_id = self.id.clone();
        tauri::async_runtime::spawn(async move {
            let _ = emit_event(
                &app,
                &session,
                Some(run_id.as_str()),
                AssistantUiEvent::AskUserResolved { pending_id },
            );
        });
    }
}

pub async fn execute(
    deps: &AssistantDeps,
    context: &ToolExecutionContext,
    params: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let params: AskUserParams =
        serde_json::from_value(params).map_err(|e| format!("Invalid ask_user params: {}", e))?;

    let question = params.question.trim().to_string();
    if question.is_empty() {
        return Err("ask_user: `question` is required.".to_string());
    }

    // Load the session so the event envelope carries session_id +
    // workspace_id consistently with the rest of the assistant events.
    let session = repository::get_session(&deps.pool, &context.session_id)
        .await?
        .ok_or_else(|| format!("Session not found: {}", context.session_id))?;

    // Supersede any orphaned ask for this session (the model re-asking
    // after a CLI transport drop lost the original call). The frontend
    // keeps one ask panel per session, so the fresh question below
    // replaces the stale one; resolving the old ids also unblocks
    // `session_has_pending_ask` for mid-run input delivery.
    for stale_id in take_for_session(&session.id) {
        let _ = emit_event(
            &deps.app,
            &session,
            Some(context.run_id.as_str()),
            AssistantUiEvent::AskUserResolved {
                pending_id: stale_id,
            },
        );
    }

    let pending_id = Uuid::new_v4().to_string();
    let (tx, rx) = oneshot::channel::<AskUserOutcome>();
    {
        let mut map = pending_map()
            .lock()
            .map_err(|e| format!("ask_user pending map poisoned: {}", e))?;
        map.insert(pending_id.clone(), (session.id.clone(), tx));
    }
    let mut guard = PendingGuard {
        id: pending_id.clone(),
        app: deps.app.clone(),
        session: session.clone(),
        run_id: context.run_id.clone(),
        cancel_token: context.cancel_token.clone(),
        armed: true,
    };

    let _ = emit_event(
        &deps.app,
        &session,
        Some(context.run_id.as_str()),
        AssistantUiEvent::AskUserRequested {
            pending_id: pending_id.clone(),
            question: question.clone(),
            options: sanitize_options(params.options.clone()),
            multi_select: params.multi_select.unwrap_or(false),
            extra_context: params.context.clone(),
        },
    );

    let wait_timeout = context.interactive_wait_timeout(ASK_USER_TIMEOUT);
    let answer = match tokio::time::timeout(wait_timeout, rx).await {
        Ok(Ok(AskUserOutcome::Answer(answer))) => answer,
        Ok(Ok(AskUserOutcome::Superseded)) => {
            // A fresh question for this session replaced this orphaned
            // wait after a transport drop. Ignore the stale future.
            guard.disarm();
            return Err(
                "ask_user was superseded by a newer question before an answer arrived".to_string(),
            );
        }
        Ok(Err(_)) if context.cancel_token.is_cancelled() => {
            return super::cancel_run_and_park(&context.cancel_token).await;
        }
        Ok(Err(_)) => {
            return guard.expire_and_stop().await;
        }
        Err(_) => {
            return guard.expire_and_stop().await;
        }
    };
    guard.disarm();

    let _ = emit_event(
        &deps.app,
        &session,
        Some(context.run_id.as_str()),
        AssistantUiEvent::AskUserResolved {
            pending_id: pending_id.clone(),
        },
    );

    let mut result = serde_json::Map::new();
    result.insert("answer".to_string(), serde_json::Value::String(answer.text));
    if let Some(idx) = answer.selected_option_index {
        result.insert(
            "selectedOptionIndex".to_string(),
            serde_json::Value::Number(idx.into()),
        );
    }
    if let Some(indexes) = answer.selected_option_indexes {
        result.insert(
            "selectedOptionIndexes".to_string(),
            serde_json::Value::Array(
                indexes
                    .into_iter()
                    .map(|idx| serde_json::Value::Number(idx.into()))
                    .collect(),
            ),
        );
    }
    Ok(serde_json::Value::Object(result))
}

/// Strip any LLM-supplied option whose label is literally "Other" (the FE
/// always appends its own "Other" free-text fallback, so a duplicate
/// shows up as two identical radios). Returns `None` when filtering
/// leaves no options, so the FE falls back to a plain textarea.
fn sanitize_options(options: Option<Vec<AskUserOption>>) -> Option<Vec<AskUserOption>> {
    let filtered: Vec<AskUserOption> = options?
        .into_iter()
        .filter(|opt| !opt.label.trim().eq_ignore_ascii_case("other"))
        .collect();
    if filtered.is_empty() {
        None
    } else {
        Some(filtered)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opt(label: &str) -> AskUserOption {
        AskUserOption {
            label: label.to_string(),
            description: None,
        }
    }

    #[test]
    fn sanitize_options_strips_other_variants() {
        let input = Some(vec![
            opt("Option A"),
            opt("Other"),
            opt("Option B"),
            opt("  other  "),
            opt("OTHER"),
        ]);
        let result = sanitize_options(input).unwrap();
        let labels: Vec<&str> = result.iter().map(|o| o.label.as_str()).collect();
        assert_eq!(labels, vec!["Option A", "Option B"]);
    }

    #[test]
    fn sanitize_options_returns_none_when_only_other_supplied() {
        let input = Some(vec![opt("Other"), opt("other")]);
        assert!(sanitize_options(input).is_none());
    }

    #[test]
    fn sanitize_options_passes_through_when_empty() {
        assert!(sanitize_options(None).is_none());
        assert!(sanitize_options(Some(vec![])).is_none());
    }

    #[test]
    fn sanitize_options_keeps_other_like_labels_intact() {
        let input = Some(vec![opt("Other option"), opt("None of the above")]);
        let result = sanitize_options(input).unwrap();
        let labels: Vec<&str> = result.iter().map(|o| o.label.as_str()).collect();
        assert_eq!(labels, vec!["Other option", "None of the above"]);
    }

    // PENDING is a process-wide static shared across parallel tests, so
    // each test uses unique session ids to stay isolated.

    #[test]
    fn take_for_session_removes_only_that_sessions_asks_and_closes_channels() {
        let (tx_a, rx_a) = oneshot::channel::<AskUserOutcome>();
        let (tx_b, mut rx_b) = oneshot::channel::<AskUserOutcome>();
        {
            let mut map = pending_map().lock().unwrap();
            map.insert(
                "tfs-pending-a".to_string(),
                ("tfs-session-a".to_string(), tx_a),
            );
            map.insert(
                "tfs-pending-b".to_string(),
                ("tfs-session-b".to_string(), tx_b),
            );
        }

        let removed = take_for_session("tfs-session-a");
        assert_eq!(removed, vec!["tfs-pending-a".to_string()]);
        assert!(!session_has_pending_ask("tfs-session-a"));
        assert!(session_has_pending_ask("tfs-session-b"));

        // The orphaned wait receives an explicit supersede signal.
        assert!(matches!(
            rx_a.blocking_recv(),
            Ok(AskUserOutcome::Superseded)
        ));
        // ...while the other session's channel stays open.
        assert!(matches!(
            rx_b.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));

        // Cleanup so other tests see an empty map for these ids.
        take_for_session("tfs-session-b");
    }

    #[test]
    fn submit_answer_fails_for_superseded_pending_id() {
        let (tx, _rx) = oneshot::channel::<AskUserOutcome>();
        pending_map()
            .lock()
            .unwrap()
            .insert("sup-pending".to_string(), ("sup-session".to_string(), tx));
        take_for_session("sup-session");

        let result = submit_answer(
            "sup-pending",
            AskUserAnswer {
                text: "late".to_string(),
                selected_option_index: None,
                selected_option_indexes: None,
            },
        );
        assert!(result.is_err(), "superseded ask must not accept answers");
    }
}
