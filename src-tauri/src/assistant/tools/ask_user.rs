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
//! global map so the channel doesn't leak.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;
use uuid::Uuid;

use crate::assistant::engine::AssistantDeps;
use crate::assistant::events::{emit_event, AssistantUiEvent};
use crate::assistant::repository;
use crate::assistant::tools::ToolExecutionContext;

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
    pub selected_option_index: Option<usize>,
}

type PendingMap = HashMap<String, oneshot::Sender<AskUserAnswer>>;
static PENDING: OnceLock<Mutex<PendingMap>> = OnceLock::new();

fn pending_map() -> &'static Mutex<PendingMap> {
    PENDING.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Deliver an answer to a waiting `ask_user` invocation. Called from the
/// `assistant_submit_user_input` Tauri command after the user submits
/// the inline answer block.
pub fn submit_answer(pending_id: &str, answer: AskUserAnswer) -> Result<(), String> {
    let mut map = pending_map()
        .lock()
        .map_err(|e| format!("ask_user pending map poisoned: {}", e))?;
    let tx = map
        .remove(pending_id)
        .ok_or_else(|| format!("No pending ask_user with id `{}`", pending_id))?;
    tx.send(answer)
        .map_err(|_| "ask_user receiver was dropped (run already ended)".to_string())
}

/// RAII guard: removes the pending entry if the tool future is dropped
/// before the channel resolves (e.g. on run cancellation).
struct PendingGuard {
    id: String,
}

impl Drop for PendingGuard {
    fn drop(&mut self) {
        if let Ok(mut map) = pending_map().lock() {
            map.remove(&self.id);
        }
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

    let pending_id = Uuid::new_v4().to_string();
    let (tx, rx) = oneshot::channel::<AskUserAnswer>();
    {
        let mut map = pending_map()
            .lock()
            .map_err(|e| format!("ask_user pending map poisoned: {}", e))?;
        map.insert(pending_id.clone(), tx);
    }
    let _guard = PendingGuard {
        id: pending_id.clone(),
    };

    // Load the session so the event envelope carries session_id +
    // workspace_id consistently with the rest of the assistant events.
    let session = repository::get_session(&deps.pool, &context.session_id)
        .await?
        .ok_or_else(|| format!("Session not found: {}", context.session_id))?;

    let _ = emit_event(
        &deps.app,
        &session,
        Some(context.run_id.as_str()),
        AssistantUiEvent::AskUserRequested {
            pending_id: pending_id.clone(),
            question: question.clone(),
            options: sanitize_options(params.options.clone()),
            extra_context: params.context.clone(),
        },
    );

    let answer = rx
        .await
        .map_err(|_| "ask_user channel closed (sender dropped)".to_string())?;

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
}
