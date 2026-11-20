//! Codex `app-server` transport — JSON-RPC over a long-lived child process.
//!
//! The legacy Codex fallback (`run_codex_turn`) shells out to `codex exec`,
//! whose stdin is consumed once for the initial prompt and then closed. That
//! makes mid-run input impossible without killing and restarting the process.
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
//! Used by default for Codex connections; `codex exec` remains available as an
//! explicit fallback.
//!
//! # Enabling / testing
//!
//! Use a Codex connection. Turns run over `codex app-server`; a message sent
//! while a turn is in flight is injected via `turn/steer` (watch for the
//! "Steered queued user message(s) into the live Codex turn" log line) instead
//! of interrupting/restarting the process. Set `CLAI_CODEX_APP_SERVER=0`
//! (`false`, `no`, `off`, and an empty value also work) to fall back to
//! `codex exec`.

use std::process::Stdio;
use std::time::Duration;

use serde_json::{json, Map, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStderr, ChildStdin, Command};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};

/// Bound on `codex --version` and on each app-server handshake request.
/// Generous: covers a cold `codex` start on a slow disk, but converts a wedged
/// Codex into a clear fast-fail.
pub(crate) const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);

/// Env var that can force the Codex app-server transport on/off.
pub(crate) const APP_SERVER_ENABLED_ENV: &str = "CLAI_CODEX_APP_SERVER";

/// Whether the app-server transport is enabled for this process. Enabled by
/// default; explicit false-y env values (empty string, `0`, `false`, `no`,
/// `off`) keep `codex exec` available as a rollback path.
pub(crate) fn app_server_enabled() -> bool {
    !matches!(
        std::env::var(APP_SERVER_ENABLED_ENV)
            .ok()
            .as_deref()
            .map(str::trim)
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("") | Some("0") | Some("false") | Some("no") | Some("off")
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

/// `config/read`: the effective Codex config in the thread's cwd, which lists
/// the user's own MCP servers without starting them (see [`thread_config`]).
pub(crate) fn config_read_request(id: i64, options: &ThreadOptions) -> Value {
    request(id, "config/read", json!({ "cwd": options.cwd }))
}

/// A single `text` [`UserInput`] element.
pub(crate) fn text_user_input(text: &str) -> Value {
    json!({ "type": "text", "text": text })
}

/// A single `localImage` [`UserInput`] element (Codex reads the file itself).
pub(crate) fn local_image_user_input(path: &str) -> Value {
    json!({ "type": "localImage", "path": path })
}

/// The namespaces Codex may give the tools of the `clai` MCP server configured
/// in [`thread_config`]: `mcp__clai` by default, or `clai` when the user
/// enables Codex's experimental `features.non_prefixed_mcp_tool_names`
/// (globally, or for this server via its `server_names`).
/// An entry that matches no namespace is ignored.
const CLAI_TOOL_NAMESPACES: [&str; 2] = ["mcp__clai", "clai"];

/// The first Codex release that has every switch [`clai_tools_only`] and
/// [`thread_config`] need; `features.view_image` came last.
const ISOLATION_SINCE: (u64, u64, u64) = (0, 147, 0);

/// The version a Codex built from source reports.
const DEVELOPMENT_BUILD: (u64, u64, u64) = (0, 0, 0);

/// A `<major>.<minor>.<patch>` version, ignoring any pre-release suffix.
fn parse_version(text: &str) -> Option<(u64, u64, u64)> {
    let mut numbers = text.split(['.', '-', '+']);
    let mut next = || numbers.next()?.parse::<u64>().ok();
    Some((next()?, next()?, next()?))
}

fn version_text((major, minor, patch): (u64, u64, u64)) -> String {
    format!("{major}.{minor}.{patch}")
}

/// A Codex known to apply [`clai_tools_only`] and [`thread_config`]'s
/// isolation. Only the checks below make one, so a turn can't skip them.
pub(crate) struct IsolatingCodex(());

/// Checks the app server that sent this `initialize` result, whose
/// `userAgent` reads `<client>/<version> (<os>; <arch>) ...`, before anything
/// else is asked of it.
pub(crate) fn isolating_codex(initialize_result: &Value) -> Result<IsolatingCodex, String> {
    let version = initialize_result
        .get("userAgent")
        .and_then(Value::as_str)
        .and_then(|agent| agent.split_whitespace().next()?.rsplit_once('/'))
        .and_then(|(_, version)| parse_version(version));
    checked_version(version)
}

/// Checks the Codex CLI that `command` runs, for the transports that learn
/// no version from Codex: `exec` and the compaction summary. Dropping the
/// future kills the process.
pub(crate) async fn check_codex_cli(mut command: Command) -> Result<IsolatingCodex, String> {
    command
        .arg("--version")
        .stdin(Stdio::null())
        .kill_on_drop(true);
    let output = tokio::time::timeout(HANDSHAKE_TIMEOUT, command.output())
        .await
        .map_err(|_| {
            format!(
                "`codex --version` did not answer within {}s",
                HANDSHAKE_TIMEOUT.as_secs()
            )
        })?
        .map_err(|e| {
            format!("Failed to launch `codex`: {e}. Is Codex CLI installed and on PATH?")
        })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "`codex --version` failed ({}): {}",
            output.status,
            stderr
                .lines()
                .rev()
                .find(|line| !line.trim().is_empty() && !line.starts_with("WARNING:"))
                .unwrap_or_default()
        ));
    }
    isolating_codex_cli(&String::from_utf8_lossy(&output.stdout))
}

/// Checks the Codex CLI whose `codex --version` printed `version_output`,
/// `codex-cli <version>`.
pub(crate) fn isolating_codex_cli(version_output: &str) -> Result<IsolatingCodex, String> {
    let version = version_output
        .split_whitespace()
        .nth(1)
        .and_then(parse_version);
    checked_version(version)
}

fn checked_version(version: Option<(u64, u64, u64)>) -> Result<IsolatingCodex, String> {
    let found = match version {
        Some(version) if version >= ISOLATION_SINCE => return Ok(IsolatingCodex(())),
        Some(DEVELOPMENT_BUILD) => "found a development build".to_string(),
        Some(version) => format!("found {}", version_text(version)),
        None => "could not read its version".to_string(),
    };
    Err(format!(
        "CLAI needs a released Codex {} or newer to keep your own Codex setup out of its \
         threads; {found}.",
        version_text(ISOLATION_SINCE)
    ))
}

/// The MCP server [`thread_config`] registers for CLAI's own tools.
const CLAI_MCP_SERVER: &str = "clai";

/// What CLAI asks of the Codex thread a turn runs in.
pub(crate) struct ThreadOptions<'a> {
    /// The Codex thread to resume; `None` starts a new one.
    pub(crate) thread_id: Option<&'a str>,
    /// Also sent by [`config_read_request`], so it reports the MCP servers
    /// of this cwd's project config.
    pub(crate) cwd: Option<&'a str>,
    pub(crate) model: Option<&'a str>,
    pub(crate) developer_instructions: &'a str,
    /// CLAI's MCP server (see [`thread_config`]).
    pub(crate) mcp_url: &'a str,
    pub(crate) mcp_token_env: &'a str,
    pub(crate) mcp_tool_timeout_secs: u64,
}

/// `thread/start`, or `thread/resume` of `options.thread_id`, built from the
/// server's answer to [`config_read_request`]. Fails when the thread could
/// not be isolated (see [`thread_config`]).
pub(crate) fn thread_request(
    id: i64,
    _checked: &IsolatingCodex,
    options: &ThreadOptions,
    config_read_result: &Value,
) -> Result<Value, String> {
    let config = thread_config(options, config_read_result)?;
    Ok(match options.thread_id {
        Some(thread_id) => thread_resume_request(id, thread_id, options, config),
        None => thread_start_request(id, options, config),
    })
}

/// Codex config that offers the model only CLAI's tools, as every other
/// provider does, and as direct tools. Both transports apply it: the app
/// server in [`thread_config`], `exec` through [`config_flags`].
///
/// Codex's model catalog still adds tools no config switches off:
/// `apply_patch`, and on code-mode models the `exec`/`wait` wrapper (plus a
/// few newer models' clock and message tools). Its MCP resource tools read
/// only the MCP servers the thread has, which is CLAI's alone. On the app
/// server, `multi_env_copy_file` stays too when the user turned on that
/// under-development feature: Codex installs it per process, not per thread.
pub(crate) fn clai_tools_only() -> Value {
    json!({
        "features": {
            // `exec_command`/`write_stdin` and the legacy shell: Codex would
            // run commands itself, outside CLAI's permission checks and
            // sandbox, instead of through CLAI's gated `bash_exec`.
            "shell_tool": false,
            // Plugins (with their MCP servers, skills and hooks, and the
            // bundled computer and browser use) and apps connectors.
            "plugins": false,
            "apps": false,
            "view_image": false,
            "image_generation": false,
            // get_goal, create_goal, update_goal.
            "goals": false,
            // spawn_agent and the other collaboration tools.
            "multi_agent_v2": false,
            "memories": false,
            "sleep_tool": false,
            "current_time_reminder": false,
            "send_message_to_user_async": false,
            "request_permissions_tool": false,
            "token_budget": false,
            "deferred_executor": false,
            "multi_env_copy_file": false,
            "standalone_web_search": false,
            // The `skills.*` tools.
            "executor_capability_discovery_v2": false,
            // In code mode Codex calls MCP tools from a script cell that
            // yields after about 30 s; the model then polls it and may
            // terminate it. Terminating drops a CLAI call still waiting on the
            // user (an ask-first approval, a path or network grant,
            // `ask_user`), which removes the prompt and cancels the run. As
            // direct tools, CLAI's calls hold the turn until they return, and
            // are not deferred behind Codex's `tool_search`. Codex 0.167's
            // experimental `code_mode_only_strict_3p_tools` (off by default)
            // runs after this and defers MCP tools again.
            "code_mode": { "direct_only_tool_namespaces": CLAI_TOOL_NAMESPACES },
        },
        "web_search": "disabled",
        // The multi-agent tools a model's catalog entry turns on.
        "agents": { "enabled": false },
        "tools": {
            "experimental_request_user_input": { "enabled": false },
            "update_plan": { "enabled": false },
        },
    })
}

/// `config` as `codex -c key=value` overrides, one per value that is not a
/// table, with keys in dotted form. Strings, booleans and arrays of them read
/// the same in JSON and TOML. Keys must not contain dots: Codex splits the
/// key at each one.
fn config_flags(config: &Value) -> Vec<String> {
    fn push(flags: &mut Vec<String>, key: String, value: &Value) {
        match value.as_object() {
            Some(table) => table
                .iter()
                .for_each(|(name, value)| push(flags, format!("{key}.{name}"), value)),
            None => flags.push(format!("{key}={value}")),
        }
    }
    let mut flags = Vec::new();
    for (name, value) in config.as_object().into_iter().flatten() {
        push(&mut flags, name.clone(), value);
    }
    flags
}

/// The `codex exec` arguments that isolate it as [`thread_config`] isolates
/// an app-server thread: without the user's config and rules, with
/// [`clai_tools_only`], and with `cwd` untrusted.
pub(crate) fn exec_isolation_args(_checked: &IsolatingCodex, cwd: &str) -> Vec<String> {
    let mut args = vec![
        "--ignore-user-config".to_string(),
        "--ignore-rules".to_string(),
    ];
    for flag in config_flags(&clai_tools_only()) {
        args.extend(["-c".to_string(), flag]);
    }
    // A whole table, as the cwd has dots `config_flags` would split it at.
    // Even with `--ignore-user-config`, an undecided cwd is recorded as
    // trusted in the user's `config.toml`.
    args.extend([
        "-c".to_string(),
        format!(
            "projects={{{}={{trust_level=\"untrusted\"}}}}",
            toml_string(cwd)
        ),
    ]);
    args
}

/// `text` as a TOML basic string, whose escapes are JSON's, except that TOML
/// also needs DEL escaped.
pub(crate) fn toml_string(text: &str) -> String {
    serde_json::to_string(text)
        .expect("a string serializes to JSON")
        .replace('\u{7f}', "\\u007F")
}

/// The per-thread `config` override. It mirrors the `-c` flags the `exec`
/// path passes, so the app-server turn reaches the same local MCP server with
/// [`clai_tools_only`], and gives Codex only what CLAI offers, like `exec`'s
/// `--ignore-user-config`. Codex merges it over its own config for this thread
/// only and does not save it.
/// Fails closed when the user's MCP servers can't all be switched off.
fn thread_config(options: &ThreadOptions, config_read_result: &Value) -> Result<Value, String> {
    // Switch off every server the user configured, then add CLAI's.
    let mut mcp_servers = disabled_configured_mcp_servers(config_read_result)?;
    mcp_servers.insert(
        CLAI_MCP_SERVER.to_string(),
        json!({
            "url": options.mcp_url,
            "bearer_token_env_var": options.mcp_token_env,
            "enabled": true,
            "required": true,
            "tool_timeout_sec": options.mcp_tool_timeout_secs,
        }),
    );
    let mut config = clai_tools_only();
    // The user's own hooks would also act outside CLAI's permission checks.
    config["features"]["hooks"] = json!(false);
    // The user's legacy `notify` command runs after every turn regardless of
    // `features.hooks`; an empty command disables it.
    config["notify"] = json!([]);
    config["mcp_servers"] = Value::Object(mcp_servers);
    if let Some(cwd) = options.cwd {
        // An explicit decision for the cwd, which Codex looks up before its
        // repository root. Undecided, `thread/start` would record the project
        // as trusted in the user's `config.toml` and then load project config
        // `config/read` did not report. Untrusted, it loads no project config
        // from the cwd itself.
        config["projects"] = json!({ cwd: { "trust_level": "untrusted" } });
    }
    Ok(config)
}

/// A disabled entry for every MCP server the effective Codex config (a
/// `config/read` result) defines. Each keeps the server's `command` or `url`:
/// Codex rejects an entry without a transport, and the thread's untrusted cwd
/// (see [`thread_config`]) drops the definitions its project config held.
/// Codex always sends the table (`{}` when empty), so without it nothing
/// proves there is nothing to switch off.
fn disabled_configured_mcp_servers(
    config_read_result: &Value,
) -> Result<Map<String, Value>, String> {
    let servers = config_read_result
        .pointer("/config/mcp_servers")
        .and_then(Value::as_object)
        .ok_or(
            "Refusing to start a Codex thread without isolating your own Codex MCP \
             servers: Codex's `config/read` answer has no `mcp_servers` table. Update \
             Codex, or report this.",
        )?;
    if servers.contains_key(CLAI_MCP_SERVER) {
        // Codex would merge it into CLAI's server definition.
        return Err(format!(
            "Your Codex config defines an MCP server named `{CLAI_MCP_SERVER}`, a name \
             CLAI reserves for its own tools. Rename that server in your Codex config \
             and try again."
        ));
    }
    Ok(servers
        .iter()
        .map(|(name, server)| {
            let mut disabled: Map<String, Value> = ["command", "url"]
                .into_iter()
                .filter_map(|key| Some((key.to_string(), server.get(key)?.clone())))
                .collect();
            disabled.insert("enabled".to_string(), json!(false));
            (name.clone(), Value::Object(disabled))
        })
        .collect())
}

/// Build `thread/start` params. `approvalPolicy: never` + `sandbox:
/// danger-full-access` is the app-server parallel of `exec`'s
/// `--dangerously-bypass-approvals-and-sandbox`: CLAI provides the external
/// sandbox (bwrap) and permission system through its MCP tools. Crucially, the
/// `config` (see [`thread_config`]) also disables Codex's native `shell_tool`,
/// so Codex has *no* unmediated execution path — every command routes through
/// the permission-gated MCP `bash_exec`, matching the exec path exactly.
fn thread_start_request(id: i64, options: &ThreadOptions, config: Value) -> Value {
    let mut params = json!({
        "approvalPolicy": "never",
        "sandbox": "danger-full-access",
        "developerInstructions": options.developer_instructions,
        "config": config,
    });
    if let Some(cwd) = options.cwd {
        params["cwd"] = json!(cwd);
    }
    if let Some(model) = options.model {
        params["model"] = json!(model);
    }
    request(id, "thread/start", params)
}

/// Build `thread/resume` params for an existing Codex thread id. Re-applies the
/// same bypass + [`thread_config`] as `thread/start` (each turn spawns a fresh
/// `codex app-server` process, so the shell_tool disable and MCP registration
/// must be set again on resume, mirroring how the exec path re-passes its flags
/// on `exec resume`).
///
/// Deliberately omits `excludeTurns`, which leaves the app server's default —
/// the thread is restored with its turns included. Nothing here depends on the
/// difference any more (the one notification we used to consume on resume,
/// `thread/tokenUsage/updated`, no longer has a handler), so this is a
/// deliberate "follow upstream's default" rather than a load-bearing flag.
/// Setting it would be a behaviour change to weigh on its own merits, which is
/// why the assertion below pins the current shape.
fn thread_resume_request(
    id: i64,
    thread_id: &str,
    options: &ThreadOptions,
    config: Value,
) -> Value {
    let mut params = json!({
        "threadId": thread_id,
        "approvalPolicy": "never",
        "sandbox": "danger-full-access",
        "developerInstructions": options.developer_instructions,
        "config": config,
    });
    // Without it Codex resumes in the thread's recorded cwd, whose project
    // config layers may differ from the ones `config/read` listed.
    if let Some(cwd) = options.cwd {
        params["cwd"] = json!(cwd);
    }
    if let Some(model) = options.model {
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
/// The `exec` path only sees free text, so the classifiers
/// (`is_context_limit_error`, `cli_failure_kind`) match on phrases. The
/// app-server gives us a *structured* code (`contextWindowExceeded`,
/// `usageLimitExceeded`, `rateLimitExceeded`); we fold the matching phrase
/// into the message so the same recovery/notice paths fire.
///
/// `sessionBudgetExceeded` is left as it is: it is a token budget for one
/// Codex session, set by Codex's config or host, so neither waiting nor the
/// provider lifts it.
pub(crate) fn classify_error_message(codex_error_info: Option<&str>, message: &str) -> String {
    let phrase = match codex_error_info {
        Some("contextWindowExceeded") => "context window",
        Some("usageLimitExceeded") => "usage limit",
        Some("rateLimitExceeded") => "rate limit",
        _ => return message.to_string(),
    };
    if message.to_ascii_lowercase().contains(phrase) {
        message.to_string()
    } else {
        format!("{message} ({phrase} exceeded)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enabled_by_default_unless_explicitly_disabled() {
        for (val, want) in [
            ("1", true),
            ("true", true),
            ("TRUE", true),
            ("on", true),
            ("yes", true),
            ("maybe", true),
            ("0", false),
            ("false", false),
            ("FALSE", false),
            ("off", false),
            ("no", false),
            ("", false),
        ] {
            std::env::set_var(APP_SERVER_ENABLED_ENV, val);
            assert_eq!(app_server_enabled(), want, "value {val:?}");
        }
        std::env::remove_var(APP_SERVER_ENABLED_ENV);
        assert!(app_server_enabled());
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

    /// An `initialize` result as Codex sends it, for a server of `version`.
    fn initialized(version: &str) -> Value {
        json!({
            "userAgent": format!("clai/{version} (Arch Linux Unknown; x86_64) unknown (clai; 26.11.8)"),
            "codexHome": "/home/u/.codex",
            "platformFamily": "unix",
            "platformOs": "linux",
        })
    }

    /// A `config/read` result whose effective config defines `mcp_servers`.
    fn config_read(mcp_servers: Value) -> Value {
        json!({ "config": { "model": "gpt-5.5", "mcp_servers": mcp_servers }, "origins": {} })
    }

    fn options(thread_id: Option<&str>) -> ThreadOptions<'_> {
        ThreadOptions {
            thread_id,
            cwd: Some("/ws"),
            model: Some("gpt-5.5"),
            developer_instructions: "Follow CLAI instructions",
            mcp_url: "http://127.0.0.1:9/mcp",
            mcp_token_env: "CLAI_MCP_TOKEN",
            mcp_tool_timeout_secs: 3600,
        }
    }

    fn codex() -> IsolatingCodex {
        isolating_codex(&initialized("0.170.0")).unwrap()
    }

    /// The thread request to a current Codex whose config defines `mcp_servers`.
    fn request_with(options: &ThreadOptions, mcp_servers: Value) -> Result<Value, String> {
        thread_request(3, &codex(), options, &config_read(mcp_servers))
    }

    #[test]
    fn thread_start_bypasses_approvals_and_carries_mcp_config() {
        let req = request_with(&options(None), json!({})).unwrap();
        assert_eq!(req["method"], "thread/start");
        let params = &req["params"];
        assert!(params.get("threadId").is_none());
        assert_eq!(params["approvalPolicy"], "never");
        assert_eq!(params["sandbox"], "danger-full-access");
        assert_eq!(params["cwd"], "/ws");
        assert_eq!(params["model"], "gpt-5.5");
        let clai = &params["config"]["mcp_servers"]["clai"];
        assert_eq!(clai["url"], "http://127.0.0.1:9/mcp");
        assert_eq!(clai["bearer_token_env_var"], "CLAI_MCP_TOKEN");
        assert_eq!(clai["enabled"], true);
        assert_eq!(clai["tool_timeout_sec"], 3600);
        // Codex's native shell must be disabled so it has no ungated
        // execution path (parity with exec's `--disable shell_tool`).
        assert_eq!(params["config"]["features"]["shell_tool"], false);
        assert_eq!(params["developerInstructions"], "Follow CLAI instructions");
        assert!(params["config"].get("developer_instructions").is_none());
    }

    #[test]
    fn thread_resume_disables_shell_tool_and_carries_mcp_config() {
        let req = request_with(&options(Some("thread-1")), json!({})).unwrap();
        let params = &req["params"];
        assert_eq!(req["method"], "thread/resume");
        assert_eq!(params["threadId"], "thread-1");
        // The `config/read` cwd, so both see the same project config layers.
        assert_eq!(params["cwd"], "/ws");
        assert_eq!(params["model"], "gpt-5.5");
        assert_eq!(params["approvalPolicy"], "never");
        assert_eq!(params["sandbox"], "danger-full-access");
        assert_eq!(params["config"]["features"]["shell_tool"], false);
        assert_eq!(params["developerInstructions"], "Follow CLAI instructions");
        assert!(params["config"].get("developer_instructions").is_none());
        let clai = &params["config"]["mcp_servers"]["clai"];
        assert_eq!(clai["url"], "http://127.0.0.1:9/mcp");
        assert_eq!(clai["tool_timeout_sec"], 3600);
        // Pins upstream's default rather than a requirement of ours: see the
        // doc comment on `thread_resume_request`.
        assert!(params.get("excludeTurns").is_none());
    }

    #[test]
    fn config_read_asks_for_the_thread_cwd() {
        let req = config_read_request(2, &options(Some("thread-1")));
        assert_eq!(req["method"], "config/read");
        assert_eq!(req["params"], json!({ "cwd": "/ws" }));
    }

    #[test]
    fn user_mcp_servers_are_disabled_and_clai_stays_enabled() {
        let req = request_with(
            &options(Some("thread-1")),
            json!({
                "user_one": { "command": "npx", "args": ["server"], "enabled": true },
                "dotted.name": { "url": "https://example.com/mcp" },
            }),
        )
        .unwrap();
        let servers = &req["params"]["config"]["mcp_servers"];
        assert_eq!(servers.as_object().unwrap().len(), 3);
        // The switch and the transport, which an entry needs where the thread
        // drops the server's own definition.
        assert_eq!(
            servers["user_one"],
            json!({ "command": "npx", "enabled": false })
        );
        assert_eq!(
            servers["dotted.name"],
            json!({ "url": "https://example.com/mcp", "enabled": false })
        );
        assert_eq!(servers["clai"]["enabled"], true);
        assert_eq!(servers["clai"]["required"], true);
    }

    #[test]
    fn unlisted_user_mcp_servers_refuse_the_thread() {
        for effective in [
            json!({}),
            json!({ "config": {} }),
            config_read(json!(null)),
            config_read(json!(["user_one"])),
        ] {
            let error = thread_request(3, &codex(), &options(None), &effective).unwrap_err();
            assert!(
                error.contains("without isolating") && error.contains("Update Codex, or report"),
                "{effective}: {error}"
            );
        }
    }

    #[test]
    fn user_mcp_server_named_clai_must_be_renamed() {
        let error = request_with(
            &options(None),
            json!({ "clai": { "url": "https://example.com/mcp" } }),
        )
        .unwrap_err();
        assert!(
            error.contains("named `clai`") && error.contains("Rename"),
            "{error}"
        );
    }

    #[test]
    fn user_hooks_and_notify_are_off() {
        let req = request_with(&options(None), json!({})).unwrap();
        let config = &req["params"]["config"];
        assert_eq!(config["features"]["hooks"], false);
        assert_eq!(config["notify"], json!([]));
    }

    #[test]
    fn codex_offers_only_clai_tools() {
        assert_eq!(
            clai_tools_only(),
            json!({
                "features": {
                    "shell_tool": false,
                    "plugins": false,
                    "apps": false,
                    "view_image": false,
                    "image_generation": false,
                    "goals": false,
                    "multi_agent_v2": false,
                    "memories": false,
                    "sleep_tool": false,
                    "current_time_reminder": false,
                    "send_message_to_user_async": false,
                    "request_permissions_tool": false,
                    "token_budget": false,
                    "deferred_executor": false,
                    "multi_env_copy_file": false,
                    "standalone_web_search": false,
                    "executor_capability_discovery_v2": false,
                    "code_mode": { "direct_only_tool_namespaces": ["mcp__clai", "clai"] },
                },
                "web_search": "disabled",
                "agents": { "enabled": false },
                "tools": {
                    "experimental_request_user_input": { "enabled": false },
                    "update_plan": { "enabled": false },
                },
            })
        );
        let tools_off = config_flags(&clai_tools_only());
        for thread_id in [None, Some("thread-1")] {
            let req = request_with(&options(thread_id), json!({})).unwrap();
            let thread_flags = config_flags(&req["params"]["config"]);
            let missing: Vec<_> = tools_off
                .iter()
                .filter(|flag| !thread_flags.contains(flag))
                .collect();
            assert!(missing.is_empty(), "{}: {missing:?}", req["method"]);
        }
    }

    #[test]
    fn config_flags_are_dotted_toml_overrides() {
        let config = json!({
            "web_search": "disabled",
            "tools": { "update_plan": { "enabled": false } },
            "features": { "shell_tool": false, "code_mode": { "namespaces": ["a", "b"] } },
        });
        let mut flags = config_flags(&config);
        flags.sort();
        assert_eq!(
            flags,
            [
                r#"features.code_mode.namespaces=["a","b"]"#,
                "features.shell_tool=false",
                "tools.update_plan.enabled=false",
                "web_search=\"disabled\"",
            ]
        );
    }

    #[test]
    fn codex_that_cannot_isolate_is_refused() {
        for (version, isolates) in [
            ("0.147.0", true),
            ("0.170.0", true),
            ("0.200.1-alpha.2", true),
            ("1.0.0", true),
            ("0.146.9", false),
            ("0.134.0", false),
            ("0.129.0", false),
        ] {
            let result = isolating_codex(&initialized(version));
            assert_eq!(result.is_ok(), isolates, "{version}");
        }
        let error = isolating_codex(&initialized("0.146.0")).err().unwrap();
        assert!(
            error.contains("needs a released Codex 0.147.0 or newer")
                && error.ends_with("; found 0.146.0."),
            "{error}"
        );
        let error = isolating_codex(&initialized("0.0.0")).err().unwrap();
        assert!(error.ends_with("; found a development build."), "{error}");
        for result in [
            json!({}),
            json!({ "userAgent": "clai" }),
            json!({ "userAgent": "clai/unknown (linux)" }),
            json!({ "userAgent": 167 }),
        ] {
            let error = isolating_codex(&result).err().unwrap();
            assert!(
                error.ends_with("; could not read its version."),
                "{result}: {error}"
            );
        }
    }

    #[test]
    fn codex_cli_that_cannot_isolate_is_refused() {
        for (output, isolates) in [
            ("codex-cli 0.147.0\n", true),
            ("codex-cli 0.170.0\n", true),
            ("codex-cli 0.146.9\n", false),
            ("codex-cli 0.134.0\n", false),
            ("codex-cli 0.0.0\n", false),
            ("codex-cli\n", false),
            ("", false),
        ] {
            let result = isolating_codex_cli(output);
            assert_eq!(result.is_ok(), isolates, "{output:?}");
        }
        let error = isolating_codex_cli("codex-cli 0.144.0\n").err().unwrap();
        assert!(
            error.contains("Codex 0.147.0 or newer") && error.ends_with("; found 0.144.0."),
            "{error}"
        );
    }

    #[test]
    fn codex_exec_is_isolated_like_a_thread() {
        let args = exec_isolation_args(&codex(), "/ws");
        assert_eq!(args[..2], ["--ignore-user-config", "--ignore-rules"]);
        let overrides: Vec<&str> = args[2..]
            .chunks(2)
            .map(|pair| {
                assert_eq!(pair[0], "-c");
                pair[1].as_str()
            })
            .collect();
        let tools_off = config_flags(&clai_tools_only());
        assert_eq!(overrides[..tools_off.len()], tools_off);
        assert_eq!(
            overrides[tools_off.len()..],
            [r#"projects={"/ws"={trust_level="untrusted"}}"#]
        );
    }

    #[test]
    fn exec_untrusted_cwd_is_one_toml_key() {
        let args = exec_isolation_args(&codex(), "C:\\Users\\a.b\\x\"y\u{7f}");
        assert_eq!(
            args.last().unwrap(),
            r#"projects={"C:\\Users\\a.b\\x\"y\u007F"={trust_level="untrusted"}}"#
        );
    }

    #[test]
    fn thread_cwd_is_untrusted_for_this_thread() {
        for thread_id in [None, Some("thread-1")] {
            let req = request_with(&options(thread_id), json!({})).unwrap();
            assert_eq!(
                req["params"]["config"]["projects"],
                json!({ "/ws": { "trust_level": "untrusted" } }),
                "{}",
                req["method"]
            );
        }
    }

    #[test]
    fn thread_requests_omit_default_model_and_cwd() {
        for thread_id in [None, Some("thread-1")] {
            let options = ThreadOptions {
                cwd: None,
                model: None,
                ..options(thread_id)
            };
            let req = request_with(&options, json!({})).unwrap();
            assert!(req["params"].get("model").is_none(), "{}", req["method"]);
            assert!(req["params"].get("cwd").is_none(), "{}", req["method"]);
            assert!(
                req["params"]["config"].get("projects").is_none(),
                "{}",
                req["method"]
            );
        }
    }

    #[test]
    fn clai_tool_namespace_names_the_configured_server() {
        let req = request_with(&options(None), json!({})).unwrap();
        let config = &req["params"]["config"];
        let servers: Vec<&String> = config["mcp_servers"].as_object().unwrap().keys().collect();
        assert_eq!(servers, ["clai"]);
        assert_eq!(
            CLAI_TOOL_NAMESPACES,
            [format!("mcp__{}", servers[0]), servers[0].clone()]
        );
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
        let rate = classify_error_message(Some("rateLimitExceeded"), "boom");
        assert!(rate.to_ascii_lowercase().contains("rate limit"));
        // A session budget is no provider limit, so its text stays as sent.
        assert_eq!(
            classify_error_message(Some("sessionBudgetExceeded"), "boom"),
            "boom"
        );
        // Already-descriptive messages are left alone.
        assert_eq!(
            classify_error_message(Some("contextWindowExceeded"), "the context window is full"),
            "the context window is full"
        );
        assert_eq!(classify_error_message(None, "plain"), "plain");
    }
}
