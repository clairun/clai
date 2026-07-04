//! Codex `app-server` transport — JSON-RPC over a long-lived child process.
//!
//! The default Codex path (`run_codex_turn`) shells out to `codex exec`, whose
//! stdin is consumed once for the initial prompt and then closed. That makes
//! mid-run input impossible without killing and restarting the process.
//!
//! `codex app-server` is the JSON-RPC protocol that powers every first-party
//! Codex surface (VS Code extension, desktop app, web). It exposes `turn/steer`,
//! which injects input into the **currently active turn** — genuine input
//! streaming, no interrupt/restart. This module is the transport layer for that
//! path: process lifecycle, newline-delimited JSON-RPC framing, and pure
//! request/notification builders. The turn *driver* (event → UI mapping, tool
//! persistence, steering policy) lives in `local_agent.rs` so it can reuse the
//! shared Codex stream helpers.
//!
//! Gated behind `CLAI_CODEX_APP_SERVER` (default off) while it bakes; `codex
//! exec` remains the default transport.
//!
//! # Enabling / testing
//!
//! Set `CLAI_CODEX_APP_SERVER=1` in the environment CLAI launches under, then
//! use a Codex connection. Turns run over `codex app-server`; a message sent
//! while a turn is in flight is injected via `turn/steer` (watch for the
//! "Steered queued user message(s) into the live Codex turn" log line) instead
//! of interrupting/restarting the process. Unset the var to fall back to
//! `codex exec`.

use std::process::Stdio;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStderr, ChildStdin, Command};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};

/// Env var that opts a Codex connection into the app-server transport.
pub(crate) const APP_SERVER_ENABLED_ENV: &str = "CLAI_CODEX_APP_SERVER";

/// Whether the app-server transport is enabled for this process. Off unless the
/// env var is explicitly truthy, so `codex exec` stays the default.
pub(crate) fn app_server_enabled() -> bool {
    matches!(
        std::env::var(APP_SERVER_ENABLED_ENV)
            .ok()
            .as_deref()
            .map(str::trim)
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("1") | Some("true") | Some("yes") | Some("on")
    )
}

/// A running `codex app-server` process. Reads are pumped off the stdout pipe
/// on a background task into an unbounded channel so the driver can `select!`
/// over messages, a steering poll timer, and cancellation without deadlocking
/// on the pipe.
pub(crate) struct AppServerTransport {
    child: Child,
    stdin: ChildStdin,
    stderr: Option<ChildStderr>,
    rx: UnboundedReceiver<Value>,
}

impl AppServerTransport {
    /// Spawn `<command> app-server` with piped stdio and start the reader task.
    /// `command` is expected to already carry the env (MCP token, timeouts) and
    /// working directory, built via `providers::build_host_cli_command` so it
    /// survives the Flatpak host hop.
    pub(crate) fn spawn(mut command: Command) -> Result<Self, String> {
        command
            .arg("app-server")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().map_err(|e| {
            format!("Failed to launch `codex app-server`: {e}. Is Codex CLI installed and on PATH?")
        })?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "codex app-server stdin was not captured".to_string())?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "codex app-server stdout was not captured".to_string())?;
        let stderr = child.stderr.take();

        let (tx, rx) = unbounded_channel();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if line.trim().is_empty() {
                    continue;
                }
                match serde_json::from_str::<Value>(&line) {
                    Ok(value) => {
                        if tx.send(value).is_err() {
                            break; // driver dropped the receiver
                        }
                    }
                    Err(error) => {
                        tracing::warn!(%error, line = %line, "codex app-server: unparseable JSON line");
                    }
                }
            }
        });

        Ok(Self {
            child,
            stdin,
            stderr,
            rx,
        })
    }

    /// Take the stderr pipe so the caller can attach the shared stderr logger
    /// (used to enrich failure messages). Returns `None` after the first call.
    pub(crate) fn take_stderr(&mut self) -> Option<ChildStderr> {
        self.stderr.take()
    }

    /// Write one JSON-RPC message as a single newline-delimited line.
    pub(crate) async fn send(&mut self, message: &Value) -> Result<(), String> {
        let mut line = serde_json::to_string(message).map_err(|e| e.to_string())?;
        line.push('\n');
        self.stdin
            .write_all(line.as_bytes())
            .await
            .map_err(|e| format!("codex app-server stdin write failed: {e}"))?;
        self.stdin
            .flush()
            .await
            .map_err(|e| format!("codex app-server stdin flush failed: {e}"))
    }

    /// Next server message, or `None` once the process closes stdout.
    pub(crate) async fn recv(&mut self) -> Option<Value> {
        self.rx.recv().await
    }

    /// Force-kill the child (used on cancel / teardown).
    pub(crate) async fn kill(&mut self) {
        let _ = self.child.kill().await;
    }
}

// ---------------------------------------------------------------------------
// JSON-RPC message classification (pure)
// ---------------------------------------------------------------------------

/// A response carries an `id` and a `result`/`error`, with no `method`.
pub(crate) fn is_response(value: &Value) -> bool {
    value.get("method").is_none()
        && value.get("id").is_some()
        && (value.get("result").is_some() || value.get("error").is_some())
}

/// A server→client request carries both a `method` and an `id`.
pub(crate) fn is_server_request(value: &Value) -> bool {
    value.get("method").is_some() && value.get("id").is_some()
}

/// A notification carries a `method` and no `id`.
pub(crate) fn notification_method(value: &Value) -> Option<&str> {
    if value.get("id").is_some() {
        return None;
    }
    value.get("method").and_then(Value::as_str)
}

/// The response id (matched against outgoing request ids during the handshake).
pub(crate) fn response_id(value: &Value) -> Option<i64> {
    value.get("id").and_then(Value::as_i64)
}

// ---------------------------------------------------------------------------
// Request builders (pure)
// ---------------------------------------------------------------------------

fn request(id: i64, method: &str, params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
}

pub(crate) fn initialize_request(id: i64, client_version: &str) -> Value {
    request(
        id,
        "initialize",
        json!({
            "clientInfo": { "name": "clai", "title": "CLAI", "version": client_version }
        }),
    )
}

/// A single `text` [`UserInput`] element.
pub(crate) fn text_user_input(text: &str) -> Value {
    json!({ "type": "text", "text": text })
}

/// A single `localImage` [`UserInput`] element (Codex reads the file itself).
pub(crate) fn local_image_user_input(path: &str) -> Value {
    json!({ "type": "localImage", "path": path })
}

/// The `config` object mirroring the `-c mcp_servers.clai.*` flags the `exec`
/// path passes, so the app-server turn can reach the same local MCP server.
pub(crate) fn mcp_servers_config(mcp_url: &str, token_env: &str, tool_timeout_secs: u64) -> Value {
    json!({
        "mcp_servers": {
            "clai": {
                "url": mcp_url,
                "bearer_token_env_var": token_env,
                "enabled": true,
                "required": true,
                "tool_timeout_sec": tool_timeout_secs,
            }
        }
    })
}

/// Build `thread/start` params. `approvalPolicy: never` + `sandbox:
/// danger-full-access` is the app-server parallel of `exec`'s
/// `--dangerously-bypass-approvals-and-sandbox`: CLAI provides the external
/// sandbox (bwrap) and permission system through its MCP tools, so Codex must
/// not gate or sandbox anything itself.
#[allow(clippy::too_many_arguments)]
pub(crate) fn thread_start_request(
    id: i64,
    cwd: Option<&str>,
    model: Option<&str>,
    mcp_url: &str,
    token_env: &str,
    tool_timeout_secs: u64,
) -> Value {
    let mut params = json!({
        "approvalPolicy": "never",
        "sandbox": "danger-full-access",
        "config": mcp_servers_config(mcp_url, token_env, tool_timeout_secs),
    });
    if let Some(cwd) = cwd {
        params["cwd"] = json!(cwd);
    }
    if let Some(model) = model {
        params["model"] = json!(model);
    }
    request(id, "thread/start", params)
}

/// Build `thread/resume` params for an existing Codex thread id.
pub(crate) fn thread_resume_request(id: i64, thread_id: &str, model: Option<&str>) -> Value {
    let mut params = json!({
        "threadId": thread_id,
        "approvalPolicy": "never",
        "sandbox": "danger-full-access",
    });
    if let Some(model) = model {
        params["model"] = json!(model);
    }
    request(id, "thread/resume", params)
}

/// Build `turn/start` for a thread with the given input elements.
pub(crate) fn turn_start_request(id: i64, thread_id: &str, input: Vec<Value>) -> Value {
    request(
        id,
        "turn/start",
        json!({ "threadId": thread_id, "input": input }),
    )
}

/// Build `turn/steer` — inject input into the active turn. Guarded by
/// `expectedTurnId`: the server rejects it if that turn is no longer active.
pub(crate) fn turn_steer_request(
    id: i64,
    thread_id: &str,
    expected_turn_id: &str,
    input: Vec<Value>,
) -> Value {
    request(
        id,
        "turn/steer",
        json!({
            "threadId": thread_id,
            "expectedTurnId": expected_turn_id,
            "input": input,
        }),
    )
}

/// Build `turn/interrupt` (used on cancellation).
pub(crate) fn turn_interrupt_request(id: i64, thread_id: &str) -> Value {
    request(id, "turn/interrupt", json!({ "threadId": thread_id }))
}

// ---------------------------------------------------------------------------
// Server→client request responses (pure)
// ---------------------------------------------------------------------------

/// Best-effort response to a server→client request. With `approvalPolicy:
/// never` the server should not ask for approvals, but if it does we answer
/// permissively (CLAI already owns the real sandbox/permission gate) rather
/// than let the turn hang. Requests we can't answer get a JSON-RPC error so the
/// server can proceed instead of blocking on us.
pub(crate) fn server_request_response(id: &Value, method: &str) -> Value {
    match method {
        // Legacy (v1) approval requests use ReviewDecision.
        "execCommandApproval" | "applyPatchApproval" => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": { "decision": "approved" }
        }),
        // v2 approval requests use a per-kind decision enum whose "allow" arm
        // is `accept`.
        "item/commandExecution/requestApproval" | "item/fileChange/requestApproval" => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": { "decision": "accept" }
        }),
        // Anything else (permission profiles, elicitations, token refresh,
        // attestation, client-side tool calls) we don't implement — decline
        // cleanly so the server doesn't wait on us.
        _ => json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": -32601, "message": format!("clai does not handle server request `{method}`") }
        }),
    }
}

// ---------------------------------------------------------------------------
// Error classification (pure)
// ---------------------------------------------------------------------------

/// Turn a Codex app-server error (`error.message` + `error.codexErrorInfo`)
/// into a message that CLAI's existing CLI error classifiers recognize.
///
/// The `exec` path only sees free text, so the recovery classifiers
/// (`is_context_limit_error`, `is_usage_limit_error`) match on phrases. The
/// app-server gives us a *structured* code (`contextWindowExceeded`,
/// `usageLimitExceeded`); we fold the matching phrase into the message so the
/// same recovery/notice paths fire.
pub(crate) fn classify_error_message(codex_error_info: Option<&str>, message: &str) -> String {
    match codex_error_info {
        Some("contextWindowExceeded")
            if !message.to_ascii_lowercase().contains("context window") =>
        {
            format!("{message} (context window exceeded)")
        }
        Some("usageLimitExceeded") if !message.to_ascii_lowercase().contains("usage limit") => {
            format!("{message} (usage limit exceeded)")
        }
        _ => message.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enabled_only_for_truthy_values() {
        for (val, want) in [
            ("1", true),
            ("true", true),
            ("TRUE", true),
            ("on", true),
            ("yes", true),
            ("0", false),
            ("false", false),
            ("", false),
            ("maybe", false),
        ] {
            std::env::set_var(APP_SERVER_ENABLED_ENV, val);
            assert_eq!(app_server_enabled(), want, "value {val:?}");
        }
        std::env::remove_var(APP_SERVER_ENABLED_ENV);
        assert!(!app_server_enabled());
    }

    #[test]
    fn message_classification_matches_jsonrpc_shapes() {
        let resp = json!({ "jsonrpc": "2.0", "id": 3, "result": { "ok": true } });
        assert!(is_response(&resp));
        assert!(!is_server_request(&resp));
        assert_eq!(notification_method(&resp), None);
        assert_eq!(response_id(&resp), Some(3));

        let note = json!({ "jsonrpc": "2.0", "method": "turn/started", "params": {} });
        assert!(!is_response(&note));
        assert!(!is_server_request(&note));
        assert_eq!(notification_method(&note), Some("turn/started"));

        let sreq =
            json!({ "jsonrpc": "2.0", "id": 9, "method": "execCommandApproval", "params": {} });
        assert!(!is_response(&sreq));
        assert!(is_server_request(&sreq));
        assert_eq!(notification_method(&sreq), None);
    }

    #[test]
    fn thread_start_bypasses_approvals_and_carries_mcp_config() {
        let req = thread_start_request(
            2,
            Some("/ws"),
            Some("gpt-5.5"),
            "http://127.0.0.1:9/mcp",
            "CLAI_MCP_TOKEN",
            3600,
        );
        let params = &req["params"];
        assert_eq!(params["approvalPolicy"], "never");
        assert_eq!(params["sandbox"], "danger-full-access");
        assert_eq!(params["cwd"], "/ws");
        assert_eq!(params["model"], "gpt-5.5");
        let clai = &params["config"]["mcp_servers"]["clai"];
        assert_eq!(clai["url"], "http://127.0.0.1:9/mcp");
        assert_eq!(clai["bearer_token_env_var"], "CLAI_MCP_TOKEN");
        assert_eq!(clai["enabled"], true);
        assert_eq!(clai["tool_timeout_sec"], 3600);
    }

    #[test]
    fn thread_start_omits_default_model() {
        let req = thread_start_request(2, None, None, "u", "T", 60);
        assert!(req["params"].get("model").is_none());
        assert!(req["params"].get("cwd").is_none());
    }

    #[test]
    fn steer_carries_expected_turn_precondition() {
        let req = turn_steer_request(7, "thread-1", "turn-9", vec![text_user_input("hi")]);
        assert_eq!(req["method"], "turn/steer");
        assert_eq!(req["params"]["threadId"], "thread-1");
        assert_eq!(req["params"]["expectedTurnId"], "turn-9");
        assert_eq!(req["params"]["input"][0]["type"], "text");
        assert_eq!(req["params"]["input"][0]["text"], "hi");
    }

    #[test]
    fn approval_requests_answered_permissively() {
        let id = json!(5);
        assert_eq!(
            server_request_response(&id, "execCommandApproval")["result"]["decision"],
            "approved"
        );
        assert_eq!(
            server_request_response(&id, "item/fileChange/requestApproval")["result"]["decision"],
            "accept"
        );
        // Unknown requests get a clean JSON-RPC error, never a hang.
        assert_eq!(
            server_request_response(&id, "attestation/generate")["error"]["code"],
            -32601
        );
    }

    #[test]
    fn error_classification_folds_structured_codes_into_text() {
        let ctx = classify_error_message(Some("contextWindowExceeded"), "boom");
        assert!(ctx.to_ascii_lowercase().contains("context window"));
        let usage = classify_error_message(Some("usageLimitExceeded"), "boom");
        assert!(usage.to_ascii_lowercase().contains("usage limit"));
        // Already-descriptive messages are left alone.
        assert_eq!(
            classify_error_message(Some("contextWindowExceeded"), "the context window is full"),
            "the context window is full"
        );
        assert_eq!(classify_error_message(None, "plain"), "plain");
    }
}
