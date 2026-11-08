use crate::assistant::sandbox::filesystem::{
    agent_path_string, resolve_existing, EffectiveFilesystemPolicy,
};
use futures::StreamExt;
use std::fs;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};

use serde::Deserialize;
use tokio::time::Duration;

use crate::assistant::network::addr::{
    embedded_ipv4, is_blocked_web_fetch_hostname, reject_private_web_fetch_addrs,
    reject_private_web_fetch_ip, strip_ipv6_url_brackets,
};
#[cfg(target_os = "linux")]
use crate::assistant::network::local_listener::{Listener, ListenerInspector, Loopback};
use crate::assistant::network::matcher::{
    canonical_host, parse_entry, parse_local_port, HostDecision, LOCALHOST,
};
#[cfg(target_os = "linux")]
use crate::assistant::network::run_state::{LocalDecision, LocalRule};
use crate::assistant::network::run_state::{RefusedTargets, RunNetwork};
use crate::assistant::sandbox::{
    keyring_only_unavailable_note, run_command, ProxyEndpoint, SandboxCommand,
    SandboxCommandOutput, SandboxEnv, SandboxNetworkMode, SandboxProfile, SandboxSessionBusMode,
    SessionBusProxy, SocketPolicy,
};
#[cfg(target_os = "linux")]
use crate::commands::network_access::{LocalListenerCard, LocalSnapshot, NetworkAccessDecision};
use crate::config::{
    ExecutionCapabilityConfig, FilesystemPathAccess, FilesystemPathGrant, GrantOrigin, NetworkMode,
    SandboxSessionBusConfig, ShellAccessMode,
};

use super::ToolExecutionContext;
use crate::commands::permissions::{AskFirstDecision, AskFirstRow};

const DEFAULT_BASH_TIMEOUT_MS: u64 = 300_000;
const MAX_BASH_TIMEOUT_MS: u64 = 1_800_000;
const DEFAULT_BASH_OUTPUT_LIMIT: usize = 20_000;
const MAX_BASH_OUTPUT_LIMIT: usize = 200_000;
const DEFAULT_WEB_FETCH_CONTENT_LIMIT: usize = 20_000;
const MAX_WEB_FETCH_CONTENT_LIMIT: usize = 100_000;
const MAX_WEB_FETCH_BODY_BYTES: usize = MAX_WEB_FETCH_CONTENT_LIMIT * 4;
const DEFAULT_WEB_FETCH_TIMEOUT_MS: u64 = 15_000;
const MAX_WEB_FETCH_TIMEOUT_MS: u64 = 30_000;
const MAX_WEB_FETCH_REDIRECTS: usize = 5;
const DEFAULT_WEB_SEARCH_MAX_RESULTS: usize = 10;
const MAX_WEB_SEARCH_MAX_RESULTS: usize = 20;
const WEB_SEARCH_TIMEOUT_MS: u64 = 10_000;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BashExecParams {
    command: String,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    timeout_ms: Option<u64>,
    #[serde(default)]
    max_output_chars: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FsRequestGrantParams {
    /// Path the agent wants access to. Absolute or `~`-prefixed; the
    /// backend resolves `~` against the host `HOME` env var.
    path: String,
    /// `read_only` or `read_write`.
    access: FilesystemPathAccess,
    /// Why the agent needs this access. Shown to the user in the modal so
    /// they can make an informed decision.
    reason: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct NetRequestAccessParams {
    host: String,
    #[serde(default)]
    port: Option<u16>,
    reason: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WebSearchParams {
    query: String,
    #[serde(default)]
    max_results: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WebFetchParams {
    url: String,
    #[serde(default)]
    max_content_chars: Option<usize>,
    #[serde(default)]
    timeout_ms: Option<u64>,
}

pub async fn execute_local_tool(
    deps: &crate::assistant::engine::AssistantDeps,
    context: &ToolExecutionContext,
    tool_name: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, String> {
    match tool_name {
        "bash_exec" => {
            let params: BashExecParams = serde_json::from_value(params)
                .map_err(|e| format!("Invalid bash_exec params: {}", e))?;
            execute_bash_exec(deps, context, params).await
        }
        "fs_request_grant" => {
            let params: FsRequestGrantParams = serde_json::from_value(params)
                .map_err(|e| format!("Invalid fs_request_grant params: {}", e))?;
            execute_fs_request_grant(deps, context, params).await
        }
        "web_search" | "web_fetch" => execute_web_tool(context, tool_name, params).await,
        "net_request_access" => {
            let params: NetRequestAccessParams = serde_json::from_value(params)
                .map_err(|e| format!("Invalid net_request_access params: {}", e))?;
            execute_net_request_access(deps, context, params).await
        }
        _ => Err(format!("Unknown local tool: {}", tool_name)),
    }
}

async fn execute_web_tool(
    context: &ToolExecutionContext,
    tool_name: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let gate = web_gate(context)?;
    if tool_name == "web_search" {
        let params: WebSearchParams = serde_json::from_value(params)
            .map_err(|e| format!("Invalid web_search params: {}", e))?;
        execute_web_search(params).await
    } else {
        let params: WebFetchParams = serde_json::from_value(params)
            .map_err(|e| format!("Invalid web_fetch params: {}", e))?;
        execute_web_fetch(gate, params).await
    }
}

async fn execute_bash_exec(
    deps: &crate::assistant::engine::AssistantDeps,
    context: &ToolExecutionContext,
    params: BashExecParams,
) -> Result<serde_json::Value, String> {
    if matches!(
        context.run_context.execution.shell.mode,
        ShellAccessMode::Off
    ) {
        return Err("Shell access is disabled for this agent".to_string());
    }

    let filesystem = filesystem_policy(context)?;
    let cwd = filesystem.authorize_cwd(params.cwd.as_deref().unwrap_or("."))?;
    let workspace_root = ensure_workspace_root(context)?;
    // Before any ask-first card: a profile the sandbox cannot enforce fails
    // here, so the user is never asked about a command that cannot run.
    let profile = sandbox_profile(
        context,
        workspace_root,
        filesystem,
        &crate::assistant::sandbox::status(),
    )
    .inspect_err(|error| {
        if error.starts_with("Sandboxed shell is unavailable") {
            context.note_warning();
        }
    })?;

    let sockets = profile.sockets.clone();

    if params.command.trim().is_empty() {
        return Err("Shell command cannot be empty".to_string());
    }
    let run_removed = context.run_removed_ask_first_snapshot();
    if let PolicyResult::AskFirst(rows) = evaluate_command_policy(
        &context.run_context.execution,
        &params.command,
        &run_removed,
    ) {
        await_ask_first(deps, context, &params.command, rows).await?;
    }

    // Whether the model set timeoutMs itself — drives the recovery hint on a
    // timeout ("the requested timeout" vs "the default timeout").
    let explicit_timeout = params.timeout_ms.is_some();
    let timeout_ms = params
        .timeout_ms
        .unwrap_or(DEFAULT_BASH_TIMEOUT_MS)
        .min(MAX_BASH_TIMEOUT_MS);
    let output_limit = params
        .max_output_chars
        .unwrap_or(DEFAULT_BASH_OUTPUT_LIMIT)
        .min(MAX_BASH_OUTPUT_LIMIT);

    // Resolve the shell up front. On Windows this can fail when no POSIX
    // shell is installed; surface that as an actionable warning + error rather
    // than a cryptic spawn failure for a missing `bash`.
    let argv = match super::posix_shell::shell_argv(params.command) {
        Ok(argv) => argv,
        Err(message) => {
            context.note_warning();
            return Err(message);
        }
    };

    let run = run_with_network(
        SandboxCommand {
            argv,
            cwd,
            timeout_ms,
            max_output_chars: output_limit,
            profile,
        },
        context.run_context.agent_display_name(),
        &context.run_network,
    )
    .await;
    let output = run.output.inspect_err(|error| {
        // Surface sandbox-availability failures (bwrap missing, kernel
        // refused namespaces, etc.) as a run warning so the run completes
        // with warnings rather than as a generic tool failure. The sandbox runner uses the
        // "Sandboxed shell is unavailable" sentinel prefix for exactly
        // this case; other errors stay as plain tool failures.
        if error.starts_with("Sandboxed shell is unavailable") {
            context.note_warning();
        }
    });
    let bus = run.session_bus_unavailable.as_deref();
    let localhost = output.as_ref().ok().and_then(|output| {
        localhost_refusal(output, run.allowed_local_ports.as_deref(), &run.refused)
    });
    let mut outcome = bash_exec_outcome(
        output,
        &run.refused,
        &sockets,
        bus,
        timeout_ms,
        explicit_timeout,
    );
    if let (Ok(result), Some(port)) = (&mut outcome, localhost) {
        result["network"]["localhost"] = localhost_note(port);
    }
    outcome
}

/// What a command run returned, and what the sandbox kept from it: the
/// `host:port` targets the network proxy refused, kept when the command
/// failed or timed out too (a client that kept retrying a refused host is a
/// likely cause), and why a keyring-only agent got no session bus.
struct NetworkRun {
    output: Result<SandboxCommandOutput, String>,
    refused: RefusedTargets,
    /// In Allowlist, the localhost ports allowed for the run when the
    /// command ended; `None` when it ran without the network proxy.
    allowed_local_ports: Option<Vec<u16>>,
    session_bus_unavailable: Option<String>,
}

/// Runs the command, in Allowlist behind a network proxy session of its
/// own, and with the keyring-only bus behind a bus proxy of its own. A bus
/// proxy that does not start leaves the command with no bus, never the
/// whole one. The programs it starts are recorded as commands of `agent`.
#[cfg(target_os = "linux")]
async fn run_with_network(
    mut command: SandboxCommand,
    agent: Option<String>,
    run_network: &std::sync::Arc<RunNetwork>,
) -> NetworkRun {
    use crate::assistant::network::{connect::HostConnector, proxy::ProxySession};
    use crate::assistant::sandbox::bus_proxy::BusProxy;
    // Starting waits for the proxy, and stopping waits for it to exit: both
    // block, so neither runs on a runtime worker.
    let bus_proxy = match command.profile.session_bus_proxy.clone() {
        Some(plan) => match tokio::task::spawn_blocking(move || BusProxy::start(&plan)).await {
            Ok(Ok(proxy)) => Some(proxy),
            Ok(Err(error)) => {
                command.profile.drop_session_bus(error);
                None
            }
            Err(error) => {
                command.profile.drop_session_bus(error.to_string());
                None
            }
        },
        None => None,
    };
    let session_bus_unavailable = command.profile.session_bus_unavailable.clone();
    let stop_bus_proxy = || {
        if let Some(proxy) = bus_proxy {
            tokio::task::spawn_blocking(move || drop(proxy));
        }
    };
    let session = match &mut command.profile.network {
        SandboxNetworkMode::Proxied(endpoint) => {
            // A port allowed later in the run answers from the next command.
            endpoint.forward_ports = run_network.local_ports();
            match ProxySession::start(
                &endpoint.socket,
                std::sync::Arc::clone(run_network),
                std::sync::Arc::new(HostConnector),
                crate::assistant::network::local_listener::host_inspector(),
            ) {
                Ok(session) => Some(session),
                Err(error) => {
                    stop_bus_proxy();
                    return NetworkRun {
                        output: Err(format!(
                            "Sandboxed shell is unavailable: the network proxy did not start: \
                             {error}"
                        )),
                        refused: RefusedTargets::default(),
                        allowed_local_ports: None,
                        session_bus_unavailable,
                    };
                }
            }
        }
        _ => None,
    };
    let output =
        crate::assistant::network::spawn_roots::as_agent(agent, run_command(command)).await;
    stop_bus_proxy();
    NetworkRun {
        output,
        allowed_local_ports: session.as_ref().map(|_| run_network.local_ports()),
        refused: session.map(|session| session.refused()).unwrap_or_default(),
        session_bus_unavailable,
    }
}

#[cfg(not(target_os = "linux"))]
async fn run_with_network(
    command: SandboxCommand,
    agent: Option<String>,
    _run_network: &std::sync::Arc<RunNetwork>,
) -> NetworkRun {
    let session_bus_unavailable = command.profile.session_bus_unavailable.clone();
    NetworkRun {
        output: crate::assistant::network::spawn_roots::as_agent(agent, run_command(command)).await,
        refused: RefusedTargets::default(),
        allowed_local_ports: None,
        session_bus_unavailable,
    }
}

const REFUSED_HINT: &str = "CLAI's network proxy refused these hosts: they are not in this \
                            agent's allowed hosts, or are in its denied hosts. Call \
                            `net_request_access` for a host the task needs; do not look for a \
                            way around the proxy.";

const SOCKET_HINT: &str = "The sandbox kept the command from reaching these Unix sockets of \
                           this computer. Do not look for a way around the sandbox: if the task \
                           needs one, call `fs_request_grant` with the exact socket path, not \
                           its folder. A Docker, Podman or containerd socket gives control of \
                           the whole computer, so say in the reason why the task needs it.";

fn bash_exec_outcome(
    output: Result<SandboxCommandOutput, String>,
    refused: &RefusedTargets,
    sockets: &SocketPolicy,
    session_bus_unavailable: Option<&str>,
    timeout_ms: u64,
    explicit_timeout: bool,
) -> Result<serde_json::Value, String> {
    match output {
        Ok(output) => Ok(bash_exec_result(
            &output,
            refused,
            sockets,
            session_bus_unavailable,
        )),
        Err(error) => {
            let mut error = augment_timeout_error(error, timeout_ms, explicit_timeout);
            if let Some(reason) = session_bus_unavailable {
                error = format!("{error}\n{}", keyring_only_unavailable_note(reason));
            }
            if refused.is_empty() {
                return Err(error);
            }
            let more = match refused.more {
                0 => String::new(),
                more => format!(" and {more} more"),
            };
            Err(format!(
                "{error}\nNetwork allowlist refused: {}{more}. {REFUSED_HINT}",
                refused.targets.join(", ")
            ))
        }
    }
}

/// Many clients hide the proxy's 403 body (curl prints only “CONNECT tunnel
/// failed, response 403”), so the result itself names the refused hosts, and
/// the host sockets the command could not reach.
fn bash_exec_result(
    output: &SandboxCommandOutput,
    refused: &RefusedTargets,
    sockets: &SocketPolicy,
    session_bus_unavailable: Option<&str>,
) -> serde_json::Value {
    let mut result = serde_json::json!({
        "cwd": agent_path_string(&output.cwd),
        "exitCode": output.exit_code,
        "success": output.success,
        "stdout": output.stdout,
        "stderr": output.stderr
    });
    if !refused.is_empty() {
        result["network"] = serde_json::json!({
            "refused": refused.targets,
            "hint": REFUSED_HINT,
        });
        if refused.more > 0 {
            result["network"]["refusedMore"] = refused.more.into();
        }
    }
    let unreachable = unreachable_sockets(output, sockets);
    if !unreachable.is_empty() {
        result["sockets"] = serde_json::json!({
            "unreachable": unreachable,
            "hint": SOCKET_HINT,
        });
    }
    if !sockets.skipped.is_empty() {
        result["socketGrantsSkipped"] = serde_json::json!(sockets.skipped);
    }
    if let Some(reason) = session_bus_unavailable {
        result["sessionBus"] = keyring_only_unavailable_note(reason).into();
    }
    result
}

/// Host Unix sockets a failed command could not reach, read from its output:
/// a path on a line that says "no such file or directory" (the socket is not
/// bound into the sandbox) or, where Landlock confines sockets, "permission
/// denied", when that path is a socket on the host and not an allowed one.
/// Only paths in `sockets.hint_places` are looked up, at most
/// `MAX_LOOKUPS` per result: the command chooses what it prints.
#[cfg(target_os = "linux")]
fn unreachable_sockets(output: &SandboxCommandOutput, sockets: &SocketPolicy) -> Vec<String> {
    use std::os::unix::fs::FileTypeExt;
    const MAX_SOCKETS: usize = 5;
    const MAX_LOOKUPS: usize = 16;
    let mut found: Vec<String> = Vec::new();
    let mut lookups = 0;
    if output.success {
        return found;
    }
    for line in output.stderr.lines().chain(output.stdout.lines()) {
        let lower = line.to_ascii_lowercase();
        if !(lower.contains("no such file or directory")
            || sockets.confined() && lower.contains("permission denied"))
        {
            continue;
        }
        for word in line.split(|c: char| c.is_whitespace() || "\"'(),;".contains(c)) {
            let path = word.strip_prefix("unix://").unwrap_or(word);
            let path = path.trim_end_matches([':', '.']);
            let in_place = Path::new(path).is_absolute()
                && sockets
                    .hint_places
                    .iter()
                    .any(|place| Path::new(path).starts_with(place));
            if !in_place || found.iter().any(|known| known == path) {
                continue;
            }
            lookups += 1;
            if lookups > MAX_LOOKUPS {
                return found;
            }
            let Ok(resolved) = fs::canonicalize(path) else {
                continue;
            };
            if !sockets
                .hint_places
                .iter()
                .any(|place| resolved.starts_with(place))
            {
                continue;
            }
            let is_socket = resolved
                .metadata()
                .is_ok_and(|meta| meta.file_type().is_socket());
            if is_socket && !sockets.allowed.iter().any(|socket| socket.host == resolved) {
                found.push(path.to_string());
                if found.len() == MAX_SOCKETS {
                    return found;
                }
            }
        }
    }
    found
}

/// Only the Linux sandbox keeps sockets out; elsewhere nothing to report.
#[cfg(not(target_os = "linux"))]
fn unreachable_sockets(_output: &SandboxCommandOutput, _sockets: &SocketPolicy) -> Vec<String> {
    Vec::new()
}

/// The `network.localhost` note for a refused localhost connection, naming
/// the port when the output did.
fn localhost_note(port: Option<u16>) -> serde_json::Value {
    let host = port.map_or_else(
        || "localhost:<port>".to_string(),
        |port| format!("localhost:{port}"),
    );
    let mut note = serde_json::json!({
        "hint": format!(
            "A connection to localhost was refused. Inside the sandbox, localhost is the \
             sandbox's own, not this computer's. If the service runs on this computer, call \
             `net_request_access` with host `{host}`; once granted, the port answers \
             from your next command."
        ),
    });
    if let Some(port) = port {
        note["port"] = port.into();
    }
    note
}

/// In Allowlist (`allowed` holds the run's allowed localhost ports), the
/// localhost port the command output says a connection was refused on,
/// `Some(None)` when the refusal names no port. A connection to an allowed
/// port reaches the computer, so a refusal there means nothing listens; one
/// the proxy refused is already named in `network.refused`.
#[cfg(target_os = "linux")]
fn localhost_refusal(
    output: &SandboxCommandOutput,
    allowed: Option<&[u16]>,
    refused: &RefusedTargets,
) -> Option<Option<u16>> {
    let allowed = allowed?;
    let mut portless = false;
    for line in output.stderr.lines().chain(output.stdout.lines()) {
        match refused_localhost_port(line) {
            Some(Some(port))
                if !allowed.contains(&port)
                    && !refused.targets.contains(&format!("localhost:{port}")) =>
            {
                return Some(Some(port));
            }
            Some(None) => portless = true,
            _ => {}
        }
    }
    portless.then_some(None)
}

#[cfg(not(target_os = "linux"))]
fn localhost_refusal(
    _output: &SandboxCommandOutput,
    _allowed: Option<&[u16]>,
    _refused: &RefusedTargets,
) -> Option<Option<u16>> {
    None
}

/// Whether `line` reports a refused connection to `localhost`, `127.0.0.1`
/// or `::1`, and the port it names: right after the address
/// (`localhost:3306`, `[::1]:3000`, `('127.0.0.1', 5678)`,
/// `/dev/tcp/localhost/5678`) or as `port 5432` / `port=8000`.
#[cfg(target_os = "linux")]
fn refused_localhost_port(line: &str) -> Option<Option<u16>> {
    const REFUSALS: [&str; 9] = [
        "connection refused",
        "econnrefused",
        "connectionrefusederror",
        "errno 111",
        "can't connect",
        "cannot connect",
        "could not connect",
        "couldn't connect",
        "failed to connect",
    ];
    let line = line.to_ascii_lowercase().replace('\u{2019}', "'");
    if !REFUSALS.iter().any(|refusal| line.contains(refusal)) {
        return None;
    }
    let mut loopback = false;
    for name in ["localhost", "127.0.0.1", "::1"] {
        for (at, _) in line.match_indices(name) {
            let after = &line[at + name.len()..];
            let mut next = after.chars();
            let longer = line[..at].chars().next_back().is_some_and(|c| {
                c.is_ascii_alphanumeric() || "._-".contains(c) || (name == "::1" && c == ':')
            }) || match next.next() {
                Some('.') => next.next().is_some_and(|c| c.is_ascii_alphanumeric()),
                Some(c) => c.is_ascii_alphanumeric() || "_-".contains(c),
                None => false,
            };
            if longer {
                continue;
            }
            loopback = true;
            if let Some(port) = port_after_address(after) {
                return Some(Some(port));
            }
        }
    }
    loopback.then(|| port_keyword(&line))
}

#[cfg(target_os = "linux")]
fn port_after_address(after: &str) -> Option<u16> {
    let after = after.trim_start_matches([']', '\'', '"']);
    let digits = match after.strip_prefix([':', '/']) {
        Some(digits) => digits,
        None => after.strip_prefix(',')?.trim_start(),
    };
    leading_port(digits)
}

#[cfg(target_os = "linux")]
fn port_keyword(line: &str) -> Option<u16> {
    line.match_indices("port").find_map(|(at, word)| {
        if line[..at]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_ascii_alphanumeric())
        {
            return None;
        }
        leading_port(line[at + word.len()..].trim_start_matches([' ', '=', ':']))
    })
}

#[cfg(target_os = "linux")]
fn leading_port(text: &str) -> Option<u16> {
    let end = text
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(text.len());
    text[..end].parse().ok().filter(|port| *port != 0)
}

/// Turn a bare sandbox timeout into an actionable error the model can recover
/// from. A plain "timed out after N ms" is a dead end — the caller can't tell
/// that a larger `timeoutMs` exists or what its ceiling is. When the run timed
/// out below the cap, point at `timeoutMs` (distinguishing the default from an
/// explicitly requested value); when it was already at the cap, tell the model
/// to narrow the command instead of waiting longer. Non-timeout errors pass
/// through unchanged.
fn augment_timeout_error(error: String, timeout_ms: u64, explicit_timeout: bool) -> String {
    if !error.contains("timed out after") {
        return error;
    }
    if timeout_ms < MAX_BASH_TIMEOUT_MS {
        let which = if explicit_timeout {
            "requested"
        } else {
            "default"
        };
        format!(
            "{error}. That was the {which} timeout; for long-running work \
             (builds, large test suites) re-run with a higher `timeoutMs` \
             (up to {MAX_BASH_TIMEOUT_MS} ms)."
        )
    } else {
        format!(
            "{error}. That is the maximum `timeoutMs` ({MAX_BASH_TIMEOUT_MS} ms); \
             narrow the command (e.g. a single package or a test subset) so it \
             finishes within the limit."
        )
    }
}

fn filesystem_policy(context: &ToolExecutionContext) -> Result<EffectiveFilesystemPolicy, String> {
    let workspace = ensure_workspace_root(context)?;
    EffectiveFilesystemPolicy::from_config(
        &workspace,
        &path_grants(context),
        crate::paths::real_home().as_deref(),
    )
}

/// The agent's saved path grants and those approved for this run only.
fn path_grants(context: &ToolExecutionContext) -> Vec<FilesystemPathGrant> {
    let mut grants = context.run_context.execution.filesystem.extra_paths.clone();
    grants.extend(context.session_grants_snapshot());
    grants
}

fn ensure_workspace_root(context: &ToolExecutionContext) -> Result<PathBuf, String> {
    let workspace_root = context.run_context.workspace_root.clone();

    // Create workspace root and pre-seed .clai/memory/ (including journal/)
    // so agents can use it immediately without spending a tool call.
    let memory_dir = workspace_root.join(".clai").join("memory");
    let journal_dir = memory_dir.join("journal");
    fs::create_dir_all(&journal_dir).map_err(|e| {
        format!(
            "Failed to create agent workspace {}: {}",
            workspace_root.display(),
            e
        )
    })?;
    crate::assistant::sandbox::filesystem::ensure_cli_project_config_dirs(&workspace_root);

    Ok(workspace_root)
}

fn sandbox_profile(
    context: &ToolExecutionContext,
    workspace_root: PathBuf,
    filesystem: EffectiveFilesystemPolicy,
    sandbox: &crate::assistant::sandbox::SandboxStatus,
) -> Result<SandboxProfile, String> {
    let network = sandbox_network(
        context.run_context.execution.sandbox.network_access.mode,
        sandbox,
        || proxy_endpoint(&workspace_root),
    )?;

    let (session_bus, session_bus_proxy, session_bus_unavailable) =
        session_bus_for(context.run_context.execution.sandbox.session_bus, || {
            keyring_proxy(&workspace_root)
        });

    let scratch_tmp = crate::assistant::sandbox::session_scratch(&workspace_root);

    let env_home = filesystem
        .home
        .as_deref()
        .unwrap_or(&filesystem.workspace_root);
    // Looked up for each command: a socket that appears, or a grant approved
    // during the run, counts from the next command on.
    #[cfg(target_os = "linux")]
    let sockets = {
        let grants: Vec<PathBuf> = path_grants(context)
            .iter()
            .filter_map(|grant| {
                crate::assistant::sandbox::filesystem::resolve_configured_path(&grant.path).ok()
            })
            .collect();
        let (allowed, skipped) = crate::assistant::sandbox::granted_sockets(&grants, &|path| {
            filesystem.covers(path, FilesystemPathAccess::ReadWrite)
        });
        SocketPolicy {
            allowed,
            skipped,
            ..crate::assistant::sandbox::socket_policy()
        }
    };
    #[cfg(not(target_os = "linux"))]
    let sockets = Default::default();
    Ok(SandboxProfile {
        env: SandboxEnv::filtered_from_current(env_home, session_bus),
        filesystem,
        network,
        session_bus,
        scratch_tmp,
        sockets,
        session_bus_proxy,
        session_bus_unavailable,
    })
}

/// The bus a command gets for the agent's setting. Only Full binds the
/// real bus. Keyring only gets its proxy, or, where the bus cannot be
/// filtered, no bus and the reason; `Ok(None)` from `plan` means the host
/// has no bus to give.
fn session_bus_for(
    config: SandboxSessionBusConfig,
    plan: impl FnOnce() -> Result<Option<SessionBusProxy>, String>,
) -> (
    SandboxSessionBusMode,
    Option<SessionBusProxy>,
    Option<String>,
) {
    match config {
        SandboxSessionBusConfig::Deny => (SandboxSessionBusMode::Deny, None, None),
        SandboxSessionBusConfig::Full => (SandboxSessionBusMode::Allow, None, None),
        SandboxSessionBusConfig::KeyringOnly => match plan() {
            Ok(Some(proxy)) => (SandboxSessionBusMode::Allow, Some(proxy), None),
            Ok(None) => (SandboxSessionBusMode::Deny, None, None),
            Err(reason) => (SandboxSessionBusMode::Deny, None, Some(reason)),
        },
    }
}

/// The keyring-only bus proxy for a command (see `bus_proxy::plan`).
#[cfg(target_os = "linux")]
fn keyring_proxy(workspace_root: &Path) -> Result<Option<SessionBusProxy>, String> {
    crate::assistant::sandbox::bus_proxy::plan(workspace_root)
}

/// No session bus to filter: macOS and Windows have none.
#[cfg(not(target_os = "linux"))]
#[expect(
    clippy::unnecessary_wraps,
    reason = "shares the Linux signature so the call site has no cfg"
)]
fn keyring_proxy(_workspace_root: &Path) -> Result<Option<SessionBusProxy>, String> {
    Ok(None)
}

/// The shell's network for a mode. A stored Allowlist where the sandbox
/// cannot enforce it (macOS for now, a hand-edited or copied config), or
/// whose helper or files are missing here, is an error, never the host
/// network.
fn sandbox_network(
    mode: NetworkMode,
    sandbox: &crate::assistant::sandbox::SandboxStatus,
    proxy_endpoint: impl FnOnce() -> Result<ProxyEndpoint, String>,
) -> Result<SandboxNetworkMode, String> {
    match mode {
        NetworkMode::Off => Ok(SandboxNetworkMode::Disabled),
        NetworkMode::HostNetwork => Ok(SandboxNetworkMode::Host),
        NetworkMode::Allowlist if sandbox.allowlist_available => {
            proxy_endpoint().map(SandboxNetworkMode::Proxied)
        }
        NetworkMode::Allowlist => Err(format!(
            "Allowlist network mode: {}. Choose Off or Host network in the agent's Local access \
             settings.",
            sandbox
                .allowlist_unavailable_reason
                .as_deref()
                .unwrap_or("not available with this sandbox")
        )),
    }
}

#[cfg(target_os = "linux")]
fn proxy_endpoint(workspace_root: &Path) -> Result<ProxyEndpoint, String> {
    crate::assistant::sandbox::helper::endpoint(workspace_root)
}

/// Only the Linux sandbox offers Allowlist; `sandbox_network` never asks
/// for an endpoint elsewhere.
#[cfg(not(target_os = "linux"))]
fn proxy_endpoint(_workspace_root: &Path) -> Result<ProxyEndpoint, String> {
    Err("Sandboxed shell is unavailable: Allowlist needs the Linux sandbox".to_string())
}

/// The tool error for a denied ask-first request: what was denied, the rule
/// each part matched, and the user's reason when there is one.
pub(crate) fn ask_first_denied_message(rows: &[AskFirstRow], reason: Option<&str>) -> String {
    let denied = rows
        .iter()
        .map(|row| format!("`{}` (ask-first rule `{}`)", row.text, row.rule))
        .collect::<Vec<_>>()
        .join(", ");
    let mut message = format!("User denied {denied}");
    if let Some(reason) = reason.map(str::trim).filter(|reason| !reason.is_empty()) {
        message.push_str(&format!(". Reason: {reason}"));
    }
    message.push('.');
    message
}

/// What `bash_exec` does with a command.
#[derive(Debug)]
pub(crate) enum PolicyResult {
    /// Run it.
    Allow,
    /// Ask the user first. Holds every row of the command that matched an
    /// ask-first rule.
    AskFirst(Vec<AskFirstRow>),
}

/// Checks `command` against the agent's ask-first list. Only Restricted
/// agents have one in force: Full checks nothing. `run_removed` holds the
/// rules "Always allow" removed earlier in this run, which the run's
/// settings snapshot still lists.
pub(crate) fn evaluate_command_policy(
    execution: &ExecutionCapabilityConfig,
    command: &str,
    run_removed: &[String],
) -> PolicyResult {
    use crate::assistant::tools::command_splitter::{split_command, Segment};

    if !matches!(execution.shell.mode, ShellAccessMode::Restricted) {
        return PolicyResult::Allow;
    }
    let rules: Vec<String> = execution
        .shell
        .ask_first_command_prefixes
        .iter()
        .filter(|rule| {
            !run_removed
                .iter()
                .any(|removed| removed.trim() == rule.trim())
        })
        .cloned()
        .collect();

    // No segments means nothing runs but shell grammar and comments.
    let segments = split_command(command);
    // A row the splitter could not name or parse may hide what runs in any
    // row of the input (`x='a[$(rm -rf y)]'; echo $((x))`), so the rules
    // are also read against every word of each row.
    let any_exact = segments
        .iter()
        .any(|s| matches!(s, Segment::Exact(_) | Segment::Unparsed(_)));
    let rows: Vec<AskFirstRow> = segments
        .iter()
        .flat_map(|segment| {
            let text = segment.text();
            matching_rules(&rules, text, any_exact)
                .into_iter()
                .map(|rule| AskFirstRow {
                    text: text.to_string(),
                    rule,
                })
        })
        .collect();

    if rows.is_empty() {
        PolicyResult::Allow
    } else {
        PolicyResult::AskFirst(rows)
    }
}

/// Every rule of `rules` that names a command the row `text` runs, trimmed,
/// once each, in list order. "Always allow" removes all of them, so the row
/// does not ask again through another rule.
///
/// A rule matches the row as written, and also when the command is run by
/// path or after assignments (`FOO=1 /usr/bin/rm`). The row is also read
/// as the head it runs a command with and the command that head runs:
/// `timeout 60 rm -rf x` matches `rm` and `timeout`, `xargs rm` matches
/// `rm` and `xargs`. Every command a misread option could hide is read
/// too: `xargs -0I rm echo` matches `rm`, and so are the commands `find`
/// runs for its matches and the method `gh api` requests. When `any_exact`
/// is set, some row of the input could not be named, so every run of words
/// is read.
fn matching_rules(rules: &[String], text: &str, any_exact: bool) -> Vec<String> {
    use crate::assistant::tools::prefix_detector::{
        by_any_reading, by_argument_command, by_find_action, by_head_name, by_run_command,
        canonicalize_for_deny, gh_api_method_forms,
    };

    let mut readings: Vec<String> = std::iter::once(text.to_string())
        .chain(by_head_name(text))
        .chain(by_run_command(text))
        .chain(by_argument_command(text))
        .chain(by_any_reading(text))
        .collect();
    // `find` first, so the method of a `gh api` that `find` runs is read too.
    let find_actions: Vec<String> = readings
        .iter()
        .map(String::as_str)
        .flat_map(by_find_action)
        .collect();
    readings.extend(find_actions);
    let gh_api_methods: Vec<String> = readings
        .iter()
        .map(String::as_str)
        .flat_map(gh_api_method_forms)
        .collect();
    readings.extend(gh_api_methods);
    // A rule cannot hide behind leading assignments or global options either:
    // `FOO=bar rm` reads as `rm`, `git -C repo push` as `git push`.
    let canonical: Vec<String> = readings
        .iter()
        .filter_map(|reading| canonicalize_for_deny(reading))
        .collect();
    readings.extend(canonical);
    if any_exact {
        readings.extend(word_runs(text));
    }

    let mut matched: Vec<String> = Vec::new();
    for rule in rules.iter().map(|rule| rule.trim()) {
        if !rule.is_empty()
            && !matched.iter().any(|seen| seen == rule)
            && readings
                .iter()
                .any(|reading| matches_prefix_literal(rule, reading))
        {
            matched.push(rule.to_string());
        }
    }
    matched
}

/// Words a rule may span in [`word_runs`].
const WORD_RUN: usize = 16;

/// Each word of `text` with up to [`WORD_RUN`] words after it, joined by single spaces,
/// with quotes and backslashes removed and the directory dropped from the
/// word: `x='a[$(/bin/rm -rf y)]'` gives `x`, `a`, `rm -rf y`, `-rf y`
/// and `y`.
fn word_runs(text: &str) -> impl Iterator<Item = String> {
    let unquoted: String = text
        .chars()
        .filter(|c| !matches!(c, '\'' | '"' | '\\'))
        .collect();
    let words: Vec<String> = unquoted
        .split(|c: char| !(c.is_alphanumeric() || "_-./+@%,:~".contains(c)))
        .filter(|word| !word.is_empty())
        .map(|word| word.rsplit('/').next().unwrap_or(word).to_string())
        .collect();
    (0..words.len()).map(move |at| words[at..words.len().min(at + WORD_RUN)].join(" "))
}

/// Cleans up an abandoned permission request when the approval-wait future
/// is dropped without a user decision — the run being cancelled, or the
/// run ending while this wait was orphaned by a CLI transport drop (the
/// rmcp session worker keeps the future alive past the dropped
/// connection; `BindingGuard` reaps it at run end, which drops this
/// future). The guard then removes the still-pending registry entry,
/// tells the frontend to drop the now-useless approval card, and cancels
/// the run so the model cannot continue without the missing decision
/// (a no-op when the run already ended). Disarmed on a normal decision or
/// explicit supersede, where the caller already removed/replaced the entry.
///
/// Cleanup is async (registry lock) so it's spawned onto the app runtime;
/// `Drop` can't await. `take` is a no-op if the entry was already removed
/// (e.g. a decision raced in), so the guard is safe even if it fires late.
struct AbandonedApprovalGuard {
    app: tauri::AppHandle,
    cancel_token: tokio_util::sync::CancellationToken,
    request_id: String,
    workspace_id: Option<String>,
    armed: bool,
}

impl AbandonedApprovalGuard {
    fn disarm(&mut self) {
        self.armed = false;
    }

    async fn expire_and_stop<T>(&mut self) -> T {
        use tauri::Manager;

        let state = self.app.state::<crate::AppState>();
        if let Some((_, remaining)) = state.pending_approvals.take(&self.request_id).await {
            crate::commands::permissions::emit_permission_resolved(&self.app, &self.request_id);
            crate::commands::permissions::emit_attention(
                &self.app,
                self.workspace_id.clone(),
                remaining,
            );
        }
        self.armed = false;
        super::cancel_run_and_park(&self.cancel_token).await
    }
}

impl Drop for AbandonedApprovalGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        self.cancel_token.cancel();
        use tauri::Manager;
        let app = self.app.clone();
        let request_id = std::mem::take(&mut self.request_id);
        let workspace_id = self.workspace_id.take();
        tauri::async_runtime::spawn(async move {
            let state = app.state::<crate::AppState>();
            if let Some((_, remaining)) = state.pending_approvals.take(&request_id).await {
                crate::commands::permissions::emit_permission_resolved(&app, &request_id);
                crate::commands::permissions::emit_attention(&app, workspace_id, remaining);
            }
        });
    }
}

/// Asks the user about a command that matched the ask-first list. Registers
/// the request in app state, emits the request and attention events, waits
/// for the decision, and resolves to `Ok(())` when the command may run or
/// `Err(_)` carrying the user's reason on a denial. If the wait is abandoned
/// without a decision, `AbandonedApprovalGuard` cancels the run so the model
/// does not continue around a missing human decision. "Always allow" is saved
/// on the agent by [`crate::commands::permissions::submit_permission_decision`]
/// before the decision is delivered.
async fn await_ask_first(
    deps: &crate::assistant::engine::AssistantDeps,
    context: &ToolExecutionContext,
    command: &str,
    rows: Vec<AskFirstRow>,
) -> Result<(), String> {
    use crate::commands::permissions::{
        emit_attention, PendingApprovalOutcome, PermissionRequest, PERMISSION_REQUEST_EVENT,
    };
    use tauri::{Emitter, Manager};

    let app_state = deps.app.state::<crate::AppState>();
    let workspace_id = Some(deps.workspace_id.clone());
    let agent_id = context.run_context.automation_id.clone();

    // The constructor resolves where "Always allow" would be saved — in this
    // workspace for its Main, or on a shared definition in force wherever
    // that teammate works.
    let request = PermissionRequest::for_agent(
        &app_state,
        workspace_id.clone(),
        agent_id,
        command.to_string(),
        rows.clone(),
    );
    let request_id = request.request_id.clone();
    let rules = request.rules();

    // Supersede: if a previous request for this exact run + command is
    // still pending, it is an orphan — its CLI transport dropped mid-call
    // and the model is now re-asking. Replace it (and its UI card) with
    // the fresh request instead of stacking a duplicate the user can no
    // longer meaningfully answer. The stale waiter receives an explicit
    // supersede outcome (no run cancellation — this run is alive and
    // waiting on the NEW request).
    for stale in app_state
        .pending_approvals
        .take_superseded(&context.run_id, command)
        .await
    {
        crate::commands::permissions::emit_permission_resolved(&deps.app, &stale.request_id);
        emit_attention(&deps.app, stale.workspace_id.clone(), stale.remaining);
    }

    let (rx, count) = app_state
        .pending_approvals
        .register(request.clone(), context.run_id.clone())
        .await;

    if let Err(e) = deps.app.emit(PERMISSION_REQUEST_EVENT, &request) {
        tracing::warn!("Failed to emit permission request event: {}", e);
    }
    emit_attention(&deps.app, workspace_id.clone(), count);

    // Cleans up if this future is abandoned before a decision (CLI transport
    // drop mid-call, or run cancellation): clears the pending entry and tells
    // the frontend to drop the now-useless approval card. Disarmed below on a
    // normal decision and explicit supersede.
    let mut abandon_guard = AbandonedApprovalGuard {
        app: deps.app.clone(),
        cancel_token: context.cancel_token.clone(),
        request_id: request_id.clone(),
        workspace_id: workspace_id.clone(),
        armed: true,
    };

    let decision = match rx.await {
        Ok(PendingApprovalOutcome::Decision(decision)) => {
            // The submit command already removed the registry entry and
            // emitted `permissions://resolved`.
            abandon_guard.disarm();
            decision
        }
        Ok(PendingApprovalOutcome::Superseded) => {
            // A fresh registration for the same run + command replaced
            // this orphaned wait after a transport drop. This stale future
            // is intentionally ignored: no notice, no warning, no cancel.
            abandon_guard.disarm();
            let msg = "Permission request was superseded by a newer request \
                       for the same command before a decision was made";
            return Err(msg.to_string());
        }
        Err(_) if context.cancel_token.is_cancelled() => {
            return super::cancel_run_and_park(&context.cancel_token).await;
        }
        Err(_) => {
            return abandon_guard.expire_and_stop().await;
        }
    };

    apply_ask_first_decision(context, &rows, &rules, decision)
}

/// Applies the user's answer to the command waiting on it: `Ok(())` runs
/// it, and a denial is the tool error. `rules` are the rules the command
/// matched, which "Always allow" removed from the agent's list.
fn apply_ask_first_decision(
    context: &ToolExecutionContext,
    rows: &[AskFirstRow],
    rules: &[String],
    decision: AskFirstDecision,
) -> Result<(), String> {
    match decision {
        AskFirstDecision::AllowOnce => Ok(()),
        AskFirstDecision::AlwaysAllow => {
            // This run reads its settings from a snapshot taken at start, so
            // the removal saved on the agent would not reach it.
            context.add_run_removed_ask_first(rules);
            Ok(())
        }
        AskFirstDecision::Deny { reason } => {
            context.note_warning();
            Err(ask_first_denied_message(rows, reason.as_deref()))
        }
    }
}

async fn execute_fs_request_grant(
    deps: &crate::assistant::engine::AssistantDeps,
    context: &ToolExecutionContext,
    params: FsRequestGrantParams,
) -> Result<serde_json::Value, String> {
    let canonical =
        canonicalize_requested_path(&params.path, crate::paths::real_home().as_deref())?;
    let canonical_str = agent_path_string(&canonical);

    // If the path is already granted (by extra_paths, the preset, or an
    // earlier session grant), short-circuit — no user prompt, just say yes.
    // This keeps repeated requests cheap and avoids modal spam when the LLM
    // forgets it already has the grant.
    let filesystem = filesystem_policy(context)?;
    if matches!(params.access, FilesystemPathAccess::ReadWrite)
        && filesystem.is_read_only_state(&canonical)
    {
        return Err(format!(
            "{canonical_str} is CLAI's own state or a provider CLI's project \
             configuration, which the shell may only read; \
             agents write only under their own `.clai/memory/`"
        ));
    }
    if already_granted(&filesystem, &canonical, params.access) {
        return Ok(serde_json::json!({
            "granted": true,
            "path": canonical_str,
            "access": access_to_str(params.access),
            "scope": "already-granted",
        }));
    }

    let request = crate::commands::path_grants::PathGrantRequest {
        request_id: uuid::Uuid::new_v4().to_string(),
        workspace_id: Some(deps.workspace_id.clone()),
        agent_id: context.run_context.automation_id.clone(),
        agent_name: None,
        requested_path: canonical_str.clone(),
        requested_access: params.access,
        reason: params.reason.clone(),
    };

    let decision = await_path_grant_decision(deps, context, request).await?;

    match decision {
        crate::commands::path_grants::PathGrantDecision::Deny => {
            let msg = format!("User denied path grant for `{}`", canonical_str);
            context.note_warning();
            Err(msg)
        }
        crate::commands::path_grants::PathGrantDecision::AllowOnce { path, access } => {
            apply_session_grant(context, &path, access);
            context.note_warning();
            Ok(serde_json::json!({
                "granted": true,
                "path": agent_path_string(std::path::Path::new(&path)),
                "access": access_to_str(access),
                "scope": "once",
                "note": "Valid for the current turn only — NOT retained for later turns. If you need this path again in a future turn, request it again, or ask the user to grant it always.",
            }))
        }
        crate::commands::path_grants::PathGrantDecision::AllowAlways {
            path,
            access,
            scope: _,
        } => {
            // The submit handler already persisted to the agent's DB row;
            // we only need to make the grant visible in the *current* run.
            apply_session_grant(context, &path, access);
            context.note_warning();
            Ok(serde_json::json!({
                "granted": true,
                "path": agent_path_string(std::path::Path::new(&path)),
                "access": access_to_str(access),
                "scope": "always",
            }))
        }
    }
}

/// Whether the agent's grants answer a request for `path`. On Linux a
/// socket needs a grant on itself: a folder grant shows the socket, but
/// commands cannot connect to it.
fn already_granted(
    filesystem: &EffectiveFilesystemPolicy,
    path: &Path,
    access: FilesystemPathAccess,
) -> bool {
    filesystem.covers(path, access)
        && (!is_socket(path) || filesystem.path_grants.iter().any(|g| g.host_path == path))
}

#[cfg(target_os = "linux")]
fn is_socket(path: &Path) -> bool {
    use std::os::unix::fs::FileTypeExt;
    fs::metadata(path).is_ok_and(|meta| meta.file_type().is_socket())
}

#[cfg(not(target_os = "linux"))]
fn is_socket(_path: &Path) -> bool {
    false
}

/// Turn the agent's requested path into the identity the compiled policy
/// reasons about. `~` expands to the host home (not the process `HOME`, which
/// differs under Flatpak) and resolution goes through
/// [`resolve_existing`], the same resolver `EffectiveFilesystemPolicy::compile`
/// uses, so coverage checks and the persisted grant agree with enforcement.
fn canonicalize_requested_path(input: &str, home: Option<&Path>) -> Result<PathBuf, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err("fs_request_grant requires a non-empty path".to_string());
    }
    let expanded = if let Some(rest) = trimmed.strip_prefix("~/") {
        home.ok_or_else(|| "Cannot expand ~ in path: home directory is unknown".to_string())?
            .join(rest)
    } else if trimmed == "~" {
        home.ok_or_else(|| "Cannot expand ~ in path: home directory is unknown".to_string())?
            .to_path_buf()
    } else {
        PathBuf::from(trimmed)
    };
    if !expanded.is_absolute() {
        return Err(format!(
            "fs_request_grant requires an absolute or ~-prefixed path; got `{}`",
            input
        ));
    }
    resolve_existing(&expanded).map_err(|error| {
        format!(
            "fs_request_grant requires an existing path because the shell sandbox cannot bind a nonexistent target: {}. Request an existing parent directory instead ({})",
            expanded.display(),
            error
        )
    })
}

fn apply_session_grant(context: &ToolExecutionContext, path: &str, access: FilesystemPathAccess) {
    context.add_session_grant(FilesystemPathGrant {
        path: path.to_string(),
        access,
        origin: Some(GrantOrigin::Approval {
            reason: String::new(),
            granted_at_unix_ms: chrono::Utc::now().timestamp_millis(),
        }),
    });
}

fn access_to_str(access: FilesystemPathAccess) -> &'static str {
    match access {
        FilesystemPathAccess::ReadOnly => "read_only",
        FilesystemPathAccess::ReadWrite => "read_write",
    }
}

/// Path-grant analogue of [`AbandonedApprovalGuard`]: clears a pending
/// filesystem path-grant request, drops its card, and cancels the run when
/// the approval-wait future is abandoned (run cancellation, or run-end
/// reaping of a wait orphaned by a CLI transport drop).
struct AbandonedPathGrantGuard {
    app: tauri::AppHandle,
    cancel_token: tokio_util::sync::CancellationToken,
    request_id: String,
    workspace_id: Option<String>,
    armed: bool,
}

impl AbandonedPathGrantGuard {
    fn disarm(&mut self) {
        self.armed = false;
    }

    async fn expire_and_stop<T>(&mut self) -> T {
        use tauri::Manager;

        let state = self.app.state::<crate::AppState>();
        if let Some((_, remaining)) = state.pending_path_grants.take(&self.request_id).await {
            crate::commands::path_grants::emit_path_grant_resolved(&self.app, &self.request_id);
            crate::commands::path_grants::emit_attention(
                &self.app,
                self.workspace_id.clone(),
                remaining,
            );
        }
        self.armed = false;
        super::cancel_run_and_park(&self.cancel_token).await
    }
}

impl Drop for AbandonedPathGrantGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        self.cancel_token.cancel();
        use tauri::Manager;
        let app = self.app.clone();
        let request_id = std::mem::take(&mut self.request_id);
        let workspace_id = self.workspace_id.take();
        tauri::async_runtime::spawn(async move {
            let state = app.state::<crate::AppState>();
            if let Some((_, remaining)) = state.pending_path_grants.take(&request_id).await {
                crate::commands::path_grants::emit_path_grant_resolved(&app, &request_id);
                crate::commands::path_grants::emit_attention(&app, workspace_id, remaining);
            }
        });
    }
}

/// Awaits the user's decision on a path-grant request. Mirrors
/// [`await_ask_first`] for shape; the only differences are the
/// request type and the registry it talks to.
async fn await_path_grant_decision(
    deps: &crate::assistant::engine::AssistantDeps,
    context: &ToolExecutionContext,
    request: crate::commands::path_grants::PathGrantRequest,
) -> Result<crate::commands::path_grants::PathGrantDecision, String> {
    use crate::commands::path_grants::{
        emit_attention, PendingPathGrantOutcome, PATH_GRANT_REQUEST_EVENT,
    };
    use tauri::{Emitter, Manager};

    let app_state = deps.app.state::<crate::AppState>();
    let workspace_id = Some(deps.workspace_id.clone());
    let request_id = request.request_id.clone();

    // Supersede a stale orphaned request for the same run + path + access
    // (the model re-asked after a CLI transport drop). See the analogous
    // block in `await_ask_first` for the full rationale.
    for stale in app_state
        .pending_path_grants
        .take_superseded(
            &context.run_id,
            &request.requested_path,
            request.requested_access,
        )
        .await
    {
        crate::commands::path_grants::emit_path_grant_resolved(&deps.app, &stale.request_id);
        emit_attention(&deps.app, stale.workspace_id.clone(), stale.remaining);
    }

    let (rx, count) = app_state
        .pending_path_grants
        .register(request.clone(), context.run_id.clone())
        .await;

    if let Err(e) = deps.app.emit(PATH_GRANT_REQUEST_EVENT, &request) {
        tracing::warn!("Failed to emit path-grant request event: {}", e);
    }
    emit_attention(&deps.app, workspace_id.clone(), count);

    // See `AbandonedApprovalGuard`: clears the pending entry and drops the
    // card if this future is abandoned before a decision. Disarmed on a
    // normal decision and explicit supersede.
    let mut abandon_guard = AbandonedPathGrantGuard {
        app: deps.app.clone(),
        cancel_token: context.cancel_token.clone(),
        request_id: request_id.clone(),
        workspace_id: workspace_id.clone(),
        armed: true,
    };

    match rx.await {
        Ok(PendingPathGrantOutcome::Decision(decision)) => {
            abandon_guard.disarm();
            Ok(decision)
        }
        Ok(PendingPathGrantOutcome::Superseded) => {
            // A fresh registration for the same run + path + access
            // replaced this orphaned wait after a transport drop. This
            // stale future is intentionally ignored.
            abandon_guard.disarm();
            let msg = "Path-grant request was superseded by a newer request \
                       for the same path before a decision was made"
                .to_string();
            Err(msg)
        }
        Err(_) if context.cancel_token.is_cancelled() => {
            super::cancel_run_and_park(&context.cancel_token).await
        }
        Err(_) => abandon_guard.expire_and_stop().await,
    }
}

/// Literal word-boundary prefix test: `command` equals `p`, or starts with
/// `p` followed by whitespace (so `git log` matches `git log --oneline` but
/// not `git logs`).
fn matches_prefix_literal(p: &str, command: &str) -> bool {
    command
        .strip_prefix(p)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with(|c: char| c.is_ascii_whitespace()))
}

// =============================================================================
// Web tools
// =============================================================================

/// Which hosts the web tools may reach for one agent. They run in this
/// process, so they follow the mode on every PC, one without a sandbox too.
#[derive(Debug, Clone, Copy)]
enum WebGate<'a> {
    /// Host network: any public host.
    AnyHost,
    /// Allowlist: only the hosts the run allows, on every redirect hop.
    Allowlist(&'a RunNetwork),
}

fn web_gate(context: &ToolExecutionContext) -> Result<WebGate<'_>, String> {
    web_access(
        context.run_context.execution.sandbox.network_access.mode,
        &context.run_network,
    )
}

fn web_access(mode: NetworkMode, network: &RunNetwork) -> Result<WebGate<'_>, String> {
    match mode {
        NetworkMode::Off => Err("Network access is off for this agent".to_string()),
        NetworkMode::Allowlist => Ok(WebGate::Allowlist(network)),
        NetworkMode::HostNetwork => Ok(WebGate::AnyHost),
    }
}

/// `host` as the URL carries it: lower case, punycode, IPv6 in brackets.
/// In Allowlist the run's host log records the outcome.
fn check_web_host(gate: WebGate<'_>, host: &str, port: Option<u16>) -> Result<(), String> {
    let WebGate::Allowlist(network) = gate else {
        return Ok(());
    };
    let decision = network.decide(host);
    network.record(host, port, decision == HostDecision::Allowed);
    match decision.refusal(
        host,
        crate::assistant::sandbox::status().allowlist_available,
    ) {
        None => Ok(()),
        Some(refusal) => Err(refusal),
    }
}

// =============================================================================
// net_request_access
// =============================================================================

async fn execute_net_request_access(
    deps: &crate::assistant::engine::AssistantDeps,
    context: &ToolExecutionContext,
    params: NetRequestAccessParams,
) -> Result<serde_json::Value, String> {
    use crate::commands::network_access::NetworkAccessRequest;
    use tauri::Manager;

    let ask = requested_host(
        &context.run_context.execution,
        crate::assistant::sandbox::status().allowlist_available,
        &context.run_network,
        &params.host,
        params.port,
    )?;
    let host = match ask {
        Ask::Allowed => {
            return Ok(serde_json::json!({
                "granted": true,
                "host": canonical_host(&params.host),
                "scope": "already-allowed",
            }))
        }
        Ask::Host(host) => host,
        Ask::LocalPort(local) => {
            return local_port_access(deps, context, local, params.reason).await
        }
    };

    let request = NetworkAccessRequest::for_agent(
        &deps.app.state::<crate::AppState>(),
        Some(deps.workspace_id.clone()),
        context.run_context.automation_id.clone(),
        host.clone(),
        params.port,
        params.reason,
        context.run_network.host_log(),
    );
    let decision = await_network_access_decision(deps, context, request).await?;
    apply_network_decision(context, &host, decision)
}

/// What `net_request_access` asks the user about.
#[derive(Debug, PartialEq, Eq)]
enum Ask {
    /// The run already reaches the host.
    Allowed,
    Host(String),
    /// A port on this computer: what listens there decides next whether a
    /// card is shown.
    LocalPort(LocalAsk),
}

/// A port on this computer, which the request names as [`LOCALHOST`]
/// whatever its spelling, so a re-ask spelled `127.0.0.1` or `[::1]`
/// replaces a pending request for the port.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LocalAsk {
    port: u16,
    /// The name the agent used, which decides the loopback address looked
    /// at, as in the network proxy.
    dialled: &'static str,
}

/// Asks for a port on this computer, with a card that shows what listens
/// there.
#[cfg(target_os = "linux")]
async fn local_port_access(
    deps: &crate::assistant::engine::AssistantDeps,
    context: &ToolExecutionContext,
    local: LocalAsk,
    reason: String,
) -> Result<serde_json::Value, String> {
    use crate::commands::network_access::NetworkAccessRequest;
    use tauri::Manager;

    let inspector = crate::assistant::network::local_listener::host_inspector();
    request_local_port(context, inspector, local, |listener| async move {
        let request = NetworkAccessRequest::for_local_port(
            &deps.app.state::<crate::AppState>(),
            Some(deps.workspace_id.clone()),
            context.run_context.automation_id.clone(),
            local.port,
            reason,
            context.run_network.host_log(),
            listener,
        );
        await_network_access_decision(deps, context, request).await
    })
    .await
}

#[cfg(not(target_os = "linux"))]
fn local_port_access(
    _deps: &crate::assistant::engine::AssistantDeps,
    _context: &ToolExecutionContext,
    local: LocalAsk,
    _reason: String,
) -> std::future::Ready<Result<serde_json::Value, String>> {
    std::future::ready(Err(format!(
        "`{LOCALHOST}:{}` cannot be requested on this PC.",
        local.port
    )))
}

/// Asks the user about a port on this computer, unless the deny list
/// refuses it, nothing a card could show listens there, or the run already
/// reaches the listener. Then applies the answer, bound to what listened
/// when the card was made: `ask` shows the card and waits.
#[cfg(target_os = "linux")]
async fn request_local_port<F, Fut>(
    context: &ToolExecutionContext,
    inspector: std::sync::Arc<dyn ListenerInspector>,
    local: LocalAsk,
    ask: F,
) -> Result<serde_json::Value, String>
where
    F: FnOnce(LocalListenerCard) -> Fut,
    Fut: std::future::Future<Output = Result<NetworkAccessDecision, String>>,
{
    let network = &context.run_network;
    let port = local.port;
    if let Some((name, denied)) = network.local_denial() {
        return Err(denied.refusal(name, true).unwrap_or_default());
    }
    let dials = crate::assistant::network::proxy::dials(local.dialled);
    let listener = tokio::task::spawn_blocking(move || first_listener(&*inspector, port, dials))
        .await
        .unwrap_or_else(|error| Err(std::io::Error::other(error)))
        .map_err(|error| {
            format!("CLAI could not check what listens on port {port} on this computer: {error}")
        })?;
    if network.decide_local(port, &listener) == LocalDecision::Allowed {
        return Ok(serde_json::json!({
            "granted": true,
            "host": LOCALHOST,
            "port": port,
            "scope": "already-allowed",
        }));
    }
    let Some(card) = LocalListenerCard::of(&listener) else {
        return Err(if listener == Listener::Clai {
            crate::assistant::network::local_listener::served_by_clai(port)
        } else {
            format!(
                "Nothing listens on port {port} on this computer. Ask the user to start \
                 the service or tunnel that should serve it, then call \
                 `net_request_access` again."
            )
        });
    };
    let snapshot = card.snapshot().clone();
    let decision = ask(card).await?;
    apply_local_decision(context, local, &snapshot, decision)
}

/// What listens on `port` at the first of `dials` where anything does, from
/// a full scan, as a card must show.
#[cfg(target_os = "linux")]
fn first_listener(
    inspector: &dyn ListenerInspector,
    port: u16,
    dials: &[Loopback],
) -> std::io::Result<Listener> {
    for &dial in dials {
        let listener = inspector.inspect(port, dial)?;
        if listener != Listener::Nothing {
            return Ok(listener);
        }
    }
    Ok(Listener::Nothing)
}

/// Applies the answer for a port on this computer to the run, bound to
/// `snapshot`: “Allow this run” adds its run bindings; “Always”,
/// already saved on the agent, adds the saved grant. A denial is the tool
/// error and the run's warning. The result names the port only, never the
/// listener the card showed.
#[cfg(target_os = "linux")]
fn apply_local_decision(
    context: &ToolExecutionContext,
    local: LocalAsk,
    snapshot: &LocalSnapshot,
    decision: NetworkAccessDecision,
) -> Result<serde_json::Value, String> {
    let entry = format!("{LOCALHOST}:{}", local.port);
    let (rules, scope, note) = match decision {
        NetworkAccessDecision::AllowRun => (
            run_rules(local.port, snapshot),
            "run",
            "Allowed for the rest of this run only, for what listens there now; a later \
             run has to ask again.",
        ),
        NetworkAccessDecision::AllowAlways { .. } => (
            vec![LocalRule::from(&snapshot.always_grant(local.port)?)],
            "always",
            "Saved on this agent for what listens there now.",
        ),
        NetworkAccessDecision::Deny { reason } => {
            context.note_warning();
            return Err(network_denied_message(&entry, reason.as_deref()));
        }
    };
    for rule in rules {
        context.run_network.allow_local(rule);
    }
    Ok(serde_json::json!({
        "granted": true,
        "host": LOCALHOST,
        "port": local.port,
        "entry": entry,
        "scope": scope,
        "note": format!("{note} Connect to `{entry}` from your next command."),
    }))
}

/// The rules “Allow this run” adds on `port`.
#[cfg(target_os = "linux")]
fn run_rules(port: u16, snapshot: &LocalSnapshot) -> Vec<LocalRule> {
    snapshot
        .run_bindings()
        .into_iter()
        .map(|binding| LocalRule { port, binding })
        .collect()
}

/// What to ask the user about. Refuses, without a card, where the tool is
/// not offered (a stale tool list), a host that is not one, a host on the
/// deny list, and this computer without a port or without the shell.
fn requested_host(
    execution: &ExecutionCapabilityConfig,
    allowlist_available: bool,
    network: &RunNetwork,
    raw: &str,
    port: Option<u16>,
) -> Result<Ask, String> {
    if let Some(refusal) =
        super::registry::net_request_access_refusal(execution, allowlist_available)
    {
        return Err(refusal.to_string());
    }
    if let Some((port, dialled)) = requested_local_port(raw.trim(), port)? {
        if matches!(execution.shell.mode, ShellAccessMode::Off) {
            return Err(
                "Ports on this computer are reached from `bash_exec`, which this \
                        agent does not have; web_fetch never reaches them."
                    .to_string(),
            );
        }
        return Ok(Ask::LocalPort(LocalAsk { port, dialled }));
    }
    // An entry, so that what reaches the card is what “Allow this run” can
    // apply: `parse_entry` refuses `%` (`%2a` would decode to a wildcard)
    // and characters a host entry may not hold.
    let raw = raw.trim();
    let entry = parse_entry(raw).map_err(|reason| {
        format!(
            "`{raw}` is not a host name ({reason}). Give the host only, such as \
             `mirror.example.org`, without a scheme, path or port."
        )
    })?;
    if entry.is_wildcard() {
        return Err(
            "Ask for one host, not a wildcard; the user can choose to allow its parent domain."
                .to_string(),
        );
    }
    let host = entry.normalized();
    match network.decide(&host) {
        HostDecision::Allowed => Ok(Ask::Allowed),
        HostDecision::Unlisted => Ok(Ask::Host(host)),
        refused => Err(refused.refusal(&host, true).unwrap_or_default()),
    }
}

/// The port and loopback name of a request for a port on this computer:
/// `localhost:<port>`, `127.0.0.1:<port>` or `[::1]:<port>`, or the name
/// alone with `port`. `None` for any other host.
fn requested_local_port(
    raw: &str,
    port: Option<u16>,
) -> Result<Option<(u16, &'static str)>, String> {
    let named = match parse_local_port(raw) {
        Ok(None) => return Ok(None),
        Ok(Some(named)) => named,
        Err(reason) => {
            let with_port = |port: u16| match raw.parse::<std::net::Ipv6Addr>() {
                Ok(_) => format!("[{raw}]:{port}"),
                Err(_) => format!("{raw}:{port}"),
            };
            match port {
                Some(port) if parse_local_port(&with_port(port)) == Ok(Some(port)) => port,
                _ => {
                    return Err(format!(
                        "`{raw}` is not a port on this computer ({reason}). Ask for \
                         `localhost:<port>`, such as `localhost:5432`."
                    ))
                }
            }
        }
    };
    if let Some(port) = port.filter(|port| *port != named) {
        return Err(format!(
            "`{raw}` names port {named}, but `port` is {port}; give the port once."
        ));
    }
    let dialled = if raw.starts_with('[') || raw.parse::<std::net::Ipv6Addr>().is_ok() {
        "::1"
    } else if raw.starts_with("127.") {
        "127.0.0.1"
    } else {
        "localhost"
    };
    Ok(Some((named, dialled)))
}

/// Applies the user's answer to the run: an allowed host or entry joins
/// the run's lists at once (an “Always” entry is already saved on the
/// agent); a denial is the tool error and, as for ask-first, the run's
/// warning.
fn apply_network_decision(
    context: &ToolExecutionContext,
    host: &str,
    decision: crate::commands::network_access::NetworkAccessDecision,
) -> Result<serde_json::Value, String> {
    use crate::commands::network_access::NetworkAccessDecision;

    let (entry, scope) = match decision {
        NetworkAccessDecision::AllowRun => (host.to_string(), "run"),
        NetworkAccessDecision::AllowAlways { entry } => (entry, "always"),
        NetworkAccessDecision::Deny { reason } => {
            context.note_warning();
            return Err(network_denied_message(host, reason.as_deref()));
        }
    };
    let parsed = parse_entry(&entry).map_err(|reason| format!("`{entry}`: {reason}"))?;
    context.run_network.allow(parsed);
    let mut result = serde_json::json!({
        "granted": true,
        "host": host,
        "entry": entry,
        "scope": scope,
    });
    if scope == "run" {
        result["note"] = serde_json::json!(
            "Allowed for the rest of this run only; a later run has to ask again."
        );
    }
    Ok(result)
}

pub(crate) fn network_denied_message(host: &str, reason: Option<&str>) -> String {
    match reason {
        Some(reason) => format!("User denied network access to `{host}`. Reason: {reason}."),
        None => format!("User denied network access to `{host}`."),
    }
}

/// Network-request analogue of [`AbandonedPathGrantGuard`]: when the wait
/// ends without a decision (the run is cancelled, or a CLI transport drop
/// orphaned it), it runs `clear` once, which takes the request off the
/// queue and drops its card, and cancels the run. `clear` is a closure so
/// the guard is tested without an app.
struct AbandonedNetworkRequestGuard<C: FnOnce()> {
    cancel_token: tokio_util::sync::CancellationToken,
    clear: Option<C>,
}

impl<C: FnOnce()> AbandonedNetworkRequestGuard<C> {
    fn disarm(&mut self) {
        self.clear = None;
    }

    async fn expire_and_stop<T>(&mut self) -> T {
        if let Some(clear) = self.clear.take() {
            clear();
        }
        super::cancel_run_and_park(&self.cancel_token).await
    }
}

impl<C: FnOnce()> Drop for AbandonedNetworkRequestGuard<C> {
    fn drop(&mut self) {
        if let Some(clear) = self.clear.take() {
            self.cancel_token.cancel();
            clear();
        }
    }
}

/// Registers the request, shows the card and waits for the user, as
/// [`await_path_grant_decision`] does for paths.
async fn await_network_access_decision(
    deps: &crate::assistant::engine::AssistantDeps,
    context: &ToolExecutionContext,
    request: crate::commands::network_access::NetworkAccessRequest,
) -> Result<crate::commands::network_access::NetworkAccessDecision, String> {
    use crate::commands::network_access::{
        emit_attention, emit_network_access_resolved, PendingNetworkOutcome,
        NETWORK_ACCESS_REQUEST_EVENT,
    };
    use tauri::{Emitter, Manager};

    let app_state = deps.app.state::<crate::AppState>();
    let workspace_id = Some(deps.workspace_id.clone());

    // A request of this run for the same host still pending is an orphan of
    // a CLI transport drop; the fresh one replaces it and its card.
    for stale in app_state
        .pending_network_requests
        .take_superseded(&context.run_id, &request.host, request.port)
        .await
    {
        emit_network_access_resolved(&deps.app, &stale.request_id);
        emit_attention(&deps.app, stale.workspace_id, stale.remaining);
    }

    let (rx, count) = app_state
        .pending_network_requests
        .register(request.clone(), context.run_id.clone())
        .await;
    if let Err(e) = deps.app.emit(NETWORK_ACCESS_REQUEST_EVENT, &request) {
        tracing::warn!("Failed to emit network access request event: {}", e);
    }
    emit_attention(&deps.app, workspace_id.clone(), count);

    let app = deps.app.clone();
    let request_id = request.request_id;
    let mut abandon_guard = AbandonedNetworkRequestGuard {
        cancel_token: context.cancel_token.clone(),
        clear: Some(move || {
            tauri::async_runtime::spawn(async move {
                let state = app.state::<crate::AppState>();
                if let Some((_, remaining)) = state.pending_network_requests.take(&request_id).await
                {
                    emit_network_access_resolved(&app, &request_id);
                    emit_attention(&app, workspace_id, remaining);
                }
            });
        }),
    };

    match rx.await {
        Ok(PendingNetworkOutcome::Decision(decision)) => {
            abandon_guard.disarm();
            Ok(decision)
        }
        Ok(PendingNetworkOutcome::Superseded) => {
            abandon_guard.disarm();
            Err(
                "Network access request was superseded by a newer request for the same host \
                 before a decision was made"
                    .to_string(),
            )
        }
        Err(_) if context.cancel_token.is_cancelled() => {
            super::cancel_run_and_park(&context.cancel_token).await
        }
        Err(_) => abandon_guard.expire_and_stop().await,
    }
}

const MAX_WEB_SEARCH_REDIRECTS: usize = 3;

/// web_search calls only its provider's fixed endpoint, which is allowed
/// implicitly; a redirect may not leave the provider or drop https.
fn provider_redirect_allowed(url: &reqwest::Url) -> bool {
    url.scheme() == "https"
        && url
            .host_str()
            .is_some_and(|host| host == "duckduckgo.com" || host.ends_with(".duckduckgo.com"))
}

/// Follows at most [`MAX_WEB_SEARCH_REDIRECTS`] redirects, each to a URL
/// `allowed` accepts. The counting rule is reqwest's `Policy::limited`.
fn redirect_policy_within(allowed: fn(&reqwest::Url) -> bool) -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(move |attempt| {
        if attempt.previous().len() > MAX_WEB_SEARCH_REDIRECTS {
            attempt.error("too many redirects")
        } else if allowed(attempt.url()) {
            attempt.follow()
        } else {
            let message = format!("redirect to `{}` leaves the search provider", attempt.url());
            attempt.error(message)
        }
    })
}

fn web_search_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(Duration::from_millis(WEB_SEARCH_TIMEOUT_MS))
        .redirect(redirect_policy_within(provider_redirect_allowed))
        .build()
        .map_err(|e| format!("Failed to create HTTP client: {}", e))
}

async fn execute_web_search(params: WebSearchParams) -> Result<serde_json::Value, String> {
    let max_results = params
        .max_results
        .unwrap_or(DEFAULT_WEB_SEARCH_MAX_RESULTS)
        .min(MAX_WEB_SEARCH_MAX_RESULTS);

    let response = web_search_client()?
        .get("https://html.duckduckgo.com/html/")
        .query(&[("q", &params.query)])
        .header("User-Agent", "CLAI/1.0")
        .send()
        .await
        .map_err(|e| format!("Web search request failed: {}", e))?;

    if !response.status().is_success() {
        return Err(format!(
            "Web search returned HTTP {}",
            response.status().as_u16()
        ));
    }

    let html = response
        .text()
        .await
        .map_err(|e| format!("Failed to read search response: {}", e))?;

    let results = parse_duckduckgo_results(&html, max_results);

    Ok(serde_json::json!({
        "query": params.query,
        "results": results,
        "resultCount": results.len()
    }))
}

fn parse_duckduckgo_results(html: &str, max_results: usize) -> Vec<serde_json::Value> {
    use scraper::{Html, Selector};

    let document = Html::parse_document(html);
    let result_selector = Selector::parse(".result").unwrap();
    let link_selector = Selector::parse(".result__a").unwrap();
    let snippet_selector = Selector::parse(".result__snippet").unwrap();

    let mut results = Vec::new();

    for element in document.select(&result_selector) {
        if results.len() >= max_results {
            break;
        }

        let Some(link) = element.select(&link_selector).next() else {
            continue;
        };

        let title = link.text().collect::<String>().trim().to_string();
        if title.is_empty() {
            continue;
        }

        // DuckDuckGo HTML wraps URLs through a redirect; extract the actual URL
        let href = link.value().attr("href").unwrap_or_default();
        let url = extract_ddg_url(href);
        if url.is_empty() {
            continue;
        }

        let snippet = element
            .select(&snippet_selector)
            .next()
            .map(|el| el.text().collect::<String>().trim().to_string())
            .unwrap_or_default();

        results.push(serde_json::json!({
            "title": title,
            "url": url,
            "snippet": snippet
        }));
    }

    results
}

/// Extract the actual URL from DuckDuckGo's redirect wrapper.
/// DDG HTML results use hrefs like `//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com&rut=...`
fn extract_ddg_url(href: &str) -> String {
    if let Some(pos) = href.find("uddg=") {
        let encoded = &href[pos + 5..];
        let encoded = encoded.split('&').next().unwrap_or(encoded);
        urldecode(encoded)
    } else if href.starts_with("http") {
        href.to_string()
    } else {
        String::new()
    }
}

/// Minimal percent-decoding for URL extraction.
fn urldecode(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let mut chars = input.bytes();
    while let Some(b) = chars.next() {
        if b == b'%' {
            let hi = chars.next().unwrap_or(b'0');
            let lo = chars.next().unwrap_or(b'0');
            if let (Some(h), Some(l)) = (hex_val(hi), hex_val(lo)) {
                result.push((h << 4 | l) as char);
            } else {
                result.push('%');
                result.push(hi as char);
                result.push(lo as char);
            }
        } else if b == b'+' {
            result.push(' ');
        } else {
            result.push(b as char);
        }
    }
    result
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

async fn execute_web_fetch(
    gate: WebGate<'_>,
    params: WebFetchParams,
) -> Result<serde_json::Value, String> {
    let content_limit = params
        .max_content_chars
        .unwrap_or(DEFAULT_WEB_FETCH_CONTENT_LIMIT)
        .min(MAX_WEB_FETCH_CONTENT_LIMIT);
    let timeout_ms = params
        .timeout_ms
        .unwrap_or(DEFAULT_WEB_FETCH_TIMEOUT_MS)
        .min(MAX_WEB_FETCH_TIMEOUT_MS);

    let url = parse_web_fetch_url(params.url.trim())?;
    let response = fetch_public_url(url, timeout_ms, gate).await?;
    let final_url = response.url().clone();

    let status = response.status().as_u16();
    if !response.status().is_success() {
        return Err(format!("Fetch returned HTTP {}", status));
    }

    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    let body_limit = web_fetch_body_limit(content_limit);
    let (body, body_truncated) = read_limited_web_fetch_body(response, body_limit).await?;

    // Convert HTML to markdown, or return plain text as-is
    let markdown = if content_type.contains("text/html") || content_type.contains("xhtml") {
        html_to_markdown(&body)
    } else {
        body
    };

    let markdown_truncated = markdown.len() > content_limit;
    let content = if markdown_truncated {
        let chars: Vec<char> = markdown.chars().collect();
        let end = content_limit.min(chars.len());
        chars[..end].iter().collect::<String>()
    } else {
        markdown
    };
    let truncated = body_truncated || markdown_truncated;

    Ok(serde_json::json!({
        "url": final_url.as_str(),
        "contentType": content_type,
        "content": content,
        "truncated": truncated,
        "contentLength": content.len()
    }))
}

fn html_to_markdown(html: &str) -> String {
    htmd::convert(html).unwrap_or_else(|_| {
        // Fallback: strip tags with scraper and return plain text
        let document = scraper::Html::parse_document(html);
        document.root_element().text().collect::<String>()
    })
}

fn parse_web_fetch_url(raw: &str) -> Result<reqwest::Url, String> {
    let url = reqwest::Url::parse(raw)
        .map_err(|_| "URL must be an absolute http:// or https:// URL".to_string())?;
    validate_web_fetch_scheme(&url)?;
    Ok(url)
}

fn validate_web_fetch_scheme(url: &reqwest::Url) -> Result<(), String> {
    match url.scheme() {
        "http" | "https" => Ok(()),
        _ => Err("URL must start with http:// or https://".to_string()),
    }
}

async fn fetch_public_url(
    mut url: reqwest::Url,
    timeout_ms: u64,
    gate: WebGate<'_>,
) -> Result<reqwest::Response, String> {
    // A redirect must never downgrade the transport: if the original request
    // was https, refuse to follow a redirect to plain http (a MITM/attacker
    // could otherwise strip TLS by 30x-ing to http).
    let started_https = url.scheme() == "https";
    let mut redirects_followed = 0;
    loop {
        let dns_override = resolve_public_web_fetch_target(&url, gate).await?;
        let mut builder = reqwest::Client::builder()
            .timeout(Duration::from_millis(timeout_ms))
            .redirect(reqwest::redirect::Policy::none());

        if let Some((host, addrs)) = &dns_override {
            builder = builder.resolve_to_addrs(host, addrs);
        }

        let client = builder
            .build()
            .map_err(|e| format!("Failed to create HTTP client: {}", e))?;

        let response = client
            .get(url.clone())
            .header("User-Agent", "CLAI/1.0")
            .header("Accept", "text/html, text/plain, application/xhtml+xml")
            .send()
            .await
            .map_err(|e| format!("Fetch failed: {}", e))?;

        if is_followable_redirect(response.status()) {
            if redirects_followed >= MAX_WEB_FETCH_REDIRECTS {
                return Err("Fetch exceeded redirect limit".to_string());
            }
            redirects_followed += 1;

            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .ok_or_else(|| {
                    format!(
                        "Fetch returned HTTP {} without a Location header",
                        response.status().as_u16()
                    )
                })?
                .to_str()
                .map_err(|_| "Redirect location is not valid UTF-8".to_string())?;
            url = url
                .join(location)
                .map_err(|e| format!("Invalid redirect location: {}", e))?;
            validate_web_fetch_scheme(&url)?;
            if started_https && url.scheme() != "https" {
                return Err(
                    "Refusing to follow an https->http redirect (transport downgrade)".to_string(),
                );
            }
            continue;
        }

        return Ok(response);
    }
}

fn is_followable_redirect(status: reqwest::StatusCode) -> bool {
    matches!(status.as_u16(), 301 | 302 | 303 | 307 | 308)
}

/// Runs on every redirect hop.
async fn resolve_public_web_fetch_target(
    url: &reqwest::Url,
    gate: WebGate<'_>,
) -> Result<Option<(String, Vec<SocketAddr>)>, String> {
    validate_web_fetch_scheme(url)?;
    let host = url
        .host_str()
        .ok_or_else(|| "URL must include a host".to_string())?;
    let host_literal = strip_ipv6_url_brackets(host);
    let normalized_host = host_literal.trim_end_matches('.').to_ascii_lowercase();
    if is_loopback_web_host(&normalized_host) {
        let allowed_first = match gate {
            WebGate::Allowlist(_) => ", once the port is allowed",
            WebGate::AnyHost => "",
        };
        return Err(format!(
            "Fetching private/local URLs is not allowed: `web_fetch` never reaches this \
             computer's localhost. Use curl in `bash_exec` instead{allowed_first}."
        ));
    }
    if is_blocked_web_fetch_hostname(&normalized_host) {
        return Err("Fetching private/local URLs is not allowed".to_string());
    }
    let port = url
        .port_or_known_default()
        .ok_or_else(|| "URL must include a valid port".to_string())?;
    check_web_host(gate, host, Some(port))?;

    if let Ok(ip) = host_literal.parse::<IpAddr>() {
        reject_private_web_fetch_ip(ip)?;
        return Ok(None);
    }

    let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host, port))
        .await
        .map_err(|e| format!("Failed to resolve URL host: {}", e))?
        .collect();
    if addrs.is_empty() {
        return Err("URL host did not resolve to any address".to_string());
    }

    reject_private_web_fetch_addrs(&addrs)?;

    // Pin the public DNS result into reqwest for this request so a hostname
    // cannot validate as public and then resolve privately during connect.
    Ok(Some((host.to_string(), addrs)))
}

/// Whether `host` (lower case, without brackets) names this computer.
fn is_loopback_web_host(host: &str) -> bool {
    host == "localhost"
        || host.ends_with(".localhost")
        || match host.parse::<IpAddr>() {
            Ok(IpAddr::V4(ip)) => ip.is_loopback(),
            Ok(IpAddr::V6(ip)) => {
                ip.is_loopback() || embedded_ipv4(ip).is_some_and(|ip| ip.is_loopback())
            }
            Err(_) => false,
        }
}

fn web_fetch_body_limit(content_limit: usize) -> usize {
    content_limit
        .saturating_mul(4)
        .min(MAX_WEB_FETCH_BODY_BYTES)
}

async fn read_limited_web_fetch_body(
    response: reqwest::Response,
    body_limit: usize,
) -> Result<(String, bool), String> {
    let mut body = Vec::with_capacity(body_limit.min(16 * 1024));
    let mut stream = response.bytes_stream();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("Failed to read response body: {}", e))?;
        if append_limited_web_fetch_body_chunk(&mut body, &chunk, body_limit) {
            return Ok((String::from_utf8_lossy(&body).into_owned(), true));
        }
    }

    Ok((String::from_utf8_lossy(&body).into_owned(), false))
}

fn append_limited_web_fetch_body_chunk(body: &mut Vec<u8>, chunk: &[u8], limit: usize) -> bool {
    let remaining = limit.saturating_sub(body.len());
    if remaining == 0 {
        return true;
    }

    if chunk.len() > remaining {
        body.extend_from_slice(&chunk[..remaining]);
        return true;
    }

    body.extend_from_slice(chunk);
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assistant::run_context::RunContext;
    use crate::config::NetworkPolicy;
    use tempfile::tempdir;

    fn context_with_grants(
        workspace_root: &std::path::Path,
        extra_paths: Vec<FilesystemPathGrant>,
    ) -> ToolExecutionContext {
        let mut execution = ExecutionCapabilityConfig::default();
        execution.filesystem.extra_paths = extra_paths;
        ToolExecutionContext {
            session_id: "session".to_string(),
            run_id: "run".to_string(),
            tool_call_id: None,
            cancel_token: tokio_util::sync::CancellationToken::new(),
            run_context: std::sync::Arc::new(RunContext {
                automation_id: Some("agent".to_string()),
                execution,
                workspace_root: workspace_root.to_path_buf(),
                ..RunContext::default()
            }),
            had_warnings: Default::default(),
            session_grants: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            run_removed_ask_first: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            run_network: Default::default(),
        }
    }

    #[test]
    fn execution_context_compiles_configured_and_session_grants() {
        let workspace = tempdir().unwrap();
        let external = tempdir().unwrap();
        let context = context_with_grants(
            workspace.path(),
            vec![FilesystemPathGrant {
                path: workspace.path().join("docs").display().to_string(),
                access: FilesystemPathAccess::ReadOnly,
                origin: None,
            }],
        );
        context
            .session_grants
            .lock()
            .unwrap()
            .push(FilesystemPathGrant {
                path: external.path().display().to_string(),
                access: FilesystemPathAccess::ReadOnly,
                origin: None,
            });
        let policy = filesystem_policy(&context).unwrap();
        assert_eq!(
            policy.workspace_root,
            workspace.path().canonicalize().unwrap()
        );
        assert_eq!(policy.path_grants.len(), 1);
        assert!(policy.covers(
            &external.path().canonicalize().unwrap(),
            FilesystemPathAccess::ReadOnly
        ));
        assert!(!policy.covers(
            &external.path().canonicalize().unwrap(),
            FilesystemPathAccess::ReadWrite
        ));
        assert!(policy.covers(
            &policy.workspace_root.join("docs"),
            FilesystemPathAccess::ReadWrite
        ));
    }

    // ------------------------------------------------------------------
    // canonicalize_requested_path
    // ------------------------------------------------------------------

    #[test]
    fn canonicalize_rejects_empty_path() {
        let err = canonicalize_requested_path("   ", None).unwrap_err();
        assert!(err.contains("non-empty"));
    }

    #[test]
    fn canonicalize_rejects_relative_paths() {
        let err = canonicalize_requested_path("relative/path", None).unwrap_err();
        assert!(err.contains("absolute"));
    }

    #[test]
    fn canonicalize_expands_tilde_with_supplied_home() {
        let temp = tempdir().unwrap();
        let nested = temp.path().join("nested");
        std::fs::create_dir(&nested).unwrap();

        let resolved = canonicalize_requested_path("~", Some(temp.path())).unwrap();
        assert_eq!(resolved, temp.path().canonicalize().unwrap());

        let resolved = canonicalize_requested_path("~/nested", Some(temp.path())).unwrap();
        assert_eq!(resolved, nested.canonicalize().unwrap());
    }

    #[test]
    fn canonicalize_rejects_tilde_without_known_home() {
        let err = canonicalize_requested_path("~/anything", None).unwrap_err();
        assert!(err.contains("home directory is unknown"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn canonicalize_resolves_symlinked_request_to_its_target() {
        let temp = tempdir().unwrap();
        let target = temp.path().join("target");
        std::fs::create_dir(&target).unwrap();
        let link = temp.path().join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let resolved = canonicalize_requested_path(&link.display().to_string(), None).unwrap();
        assert_eq!(resolved, target.canonicalize().unwrap());
    }

    #[test]
    fn canonicalize_rejects_nonexistent_grant_target() {
        let temp = tempdir().unwrap();
        let missing = temp.path().join("future-cache");
        let error = canonicalize_requested_path(&missing.display().to_string(), None).unwrap_err();
        assert!(error.contains("requires an existing path"), "{error}");
        assert!(error.contains("existing parent directory"), "{error}");
    }

    #[test]
    fn agent_path_string_normalizes_paths_for_the_agent() {
        // Windows branch is exercised by the windows-latest CI job.
        #[cfg(windows)]
        {
            assert_eq!(
                agent_path_string(std::path::Path::new(r"C:\Users\dev\proj\file.rs")),
                "C:/Users/dev/proj/file.rs"
            );
            // `std::fs::canonicalize` emits a `\\?\` verbatim prefix; strip it.
            assert_eq!(
                agent_path_string(std::path::Path::new(r"\\?\C:\Users\dev\file.rs")),
                "C:/Users/dev/file.rs"
            );
        }
        #[cfg(not(windows))]
        {
            // Unix: returned verbatim (backslash is a legal filename byte).
            assert_eq!(
                agent_path_string(std::path::Path::new("/home/dev/proj/file.rs")),
                "/home/dev/proj/file.rs"
            );
            assert_eq!(
                agent_path_string(std::path::Path::new("/home/dev/we\\ird")),
                "/home/dev/we\\ird"
            );
        }
    }

    #[test]
    fn extract_ddg_url_decodes_redirect_wrapper() {
        let href = "//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Fpage&rut=abc123";
        assert_eq!(extract_ddg_url(href), "https://example.com/page");
    }

    #[test]
    fn extract_ddg_url_passes_through_direct_urls() {
        assert_eq!(
            extract_ddg_url("https://example.com"),
            "https://example.com"
        );
    }

    #[test]
    fn extract_ddg_url_returns_empty_for_unknown_format() {
        assert_eq!(extract_ddg_url("/some/relative/path"), "");
    }

    #[test]
    fn urldecode_handles_percent_encoding() {
        assert_eq!(urldecode("hello%20world"), "hello world");
        assert_eq!(
            urldecode("https%3A%2F%2Fexample.com"),
            "https://example.com"
        );
        assert_eq!(urldecode("a+b"), "a b");
        assert_eq!(urldecode("plain"), "plain");
    }

    #[test]
    fn parse_web_fetch_url_rejects_non_absolute_http_urls() {
        assert!(parse_web_fetch_url("/relative/path").is_err());
        assert!(parse_web_fetch_url("ftp://example.com/file").is_err());
    }

    #[test]
    fn web_fetch_body_reader_truncates_chunks_at_limit() {
        let mut body = Vec::new();

        assert!(!append_limited_web_fetch_body_chunk(&mut body, b"hello", 8));
        assert!(append_limited_web_fetch_body_chunk(&mut body, b" world", 8));
        assert_eq!(body, b"hello wo");
    }

    #[test]
    fn web_fetch_body_limit_scales_but_stays_bounded() {
        assert_eq!(web_fetch_body_limit(0), 0);
        assert_eq!(web_fetch_body_limit(1_000), 4_000);
        assert_eq!(web_fetch_body_limit(MAX_WEB_FETCH_CONTENT_LIMIT), 400_000);
        assert_eq!(
            web_fetch_body_limit(MAX_WEB_FETCH_CONTENT_LIMIT * 10),
            MAX_WEB_FETCH_BODY_BYTES
        );
    }

    #[test]
    fn parse_duckduckgo_results_extracts_from_html() {
        let html = r#"
        <div class="result">
            <a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Frust-lang.org&rut=x">Rust Language</a>
            <a class="result__snippet">A systems programming language</a>
        </div>
        <div class="result">
            <a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fdocs.rs&rut=y">Docs.rs</a>
            <a class="result__snippet">Rust package documentation</a>
        </div>
        "#;

        let results = parse_duckduckgo_results(html, 10);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0]["title"], "Rust Language");
        assert_eq!(results[0]["url"], "https://rust-lang.org");
        assert_eq!(results[1]["title"], "Docs.rs");
    }

    #[test]
    fn parse_duckduckgo_results_respects_max_results() {
        let html = r#"
        <div class="result">
            <a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fa.com&rut=x">A</a>
        </div>
        <div class="result">
            <a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fb.com&rut=y">B</a>
        </div>
        "#;

        let results = parse_duckduckgo_results(html, 1);
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn html_to_markdown_converts_basic_html() {
        let html = "<html><body><h1>Title</h1><p>Hello <strong>world</strong></p></body></html>";
        let md = html_to_markdown(html);
        assert!(md.contains("Title"));
        assert!(md.contains("**world**"));
    }

    #[tokio::test]
    async fn web_fetch_rejects_non_http_urls() {
        let params = WebFetchParams {
            url: "ftp://example.com/file".to_string(),
            max_content_chars: None,
            timeout_ms: None,
        };
        let result = execute_web_fetch(WebGate::AnyHost, params).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("http:// or https://"));
    }

    #[tokio::test]
    async fn web_fetch_rejects_private_urls() {
        let params = WebFetchParams {
            url: "http://192.168.1.1/admin".to_string(),
            max_content_chars: None,
            timeout_ms: None,
        };
        let result = execute_web_fetch(WebGate::AnyHost, params).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("private/local"));
    }

    #[tokio::test]
    async fn web_fetch_rejects_private_ipv6_literals() {
        let url = parse_web_fetch_url("http://[::1]/admin").unwrap();

        let err = resolve_public_web_fetch_target(&url, WebGate::AnyHost)
            .await
            .unwrap_err();
        assert!(err.contains("private/local"));
    }

    #[tokio::test]
    async fn web_fetch_rejects_redirect_targets_to_private_urls() {
        let start = parse_web_fetch_url("https://example.com/start").unwrap();
        let redirected = start.join("http://127.0.0.1/admin").unwrap();

        let err = resolve_public_web_fetch_target(&redirected, WebGate::AnyHost)
            .await
            .unwrap_err();
        assert!(err.contains("private/local"));
    }

    // ------------------------------------------------------------------
    // Web tools under the agent's network mode
    // ------------------------------------------------------------------

    fn allowlist(allow: &[&str], deny: &[&str]) -> NetworkPolicy {
        NetworkPolicy {
            mode: NetworkMode::Allowlist,
            allow_domains: allow.iter().map(|entry| entry.to_string()).collect(),
            deny_domains: deny.iter().map(|entry| entry.to_string()).collect(),
            local_ports: Vec::new(),
        }
    }

    fn run_network(allow: &[&str], deny: &[&str]) -> RunNetwork {
        RunNetwork::new(&allowlist(allow, deny))
    }

    fn logged(network: &RunNetwork) -> Vec<(String, Option<u16>, bool)> {
        network
            .host_log()
            .into_iter()
            .map(|entry| (entry.host, entry.port, entry.allowed))
            .collect()
    }

    #[test]
    fn web_access_follows_the_mode() {
        let network = run_network(&["github.com"], &[]);
        assert!(matches!(
            web_access(NetworkMode::Allowlist, &network),
            Ok(WebGate::Allowlist(_))
        ));
        assert!(matches!(
            web_access(NetworkMode::HostNetwork, &network),
            Ok(WebGate::AnyHost)
        ));
        assert_eq!(
            web_access(NetworkMode::Off, &network).unwrap_err(),
            "Network access is off for this agent"
        );
    }

    /// `net_request_access` is offered only where Allowlist is enforceable for
    /// shell commands, so elsewhere the way out is the settings alone.
    #[tokio::test]
    async fn web_fetch_refuses_an_unlisted_host_naming_it_and_the_way_out() {
        let network = run_network(&["github.com"], &[]);
        let url = parse_web_fetch_url("https://example.org/page").unwrap();
        let err = resolve_public_web_fetch_target(&url, WebGate::Allowlist(&network))
            .await
            .unwrap_err();
        assert!(
            err.contains("`example.org` is not in this agent's allowed hosts"),
            "{err}"
        );
        assert_eq!(
            err.contains("net_request_access"),
            crate::assistant::sandbox::status().allowlist_available,
            "{err}"
        );
        assert!(err.contains("Local access settings"), "{err}");
        assert_eq!(
            logged(&network),
            [("example.org".to_string(), Some(443), false)]
        );
    }

    #[tokio::test]
    async fn web_fetch_refuses_a_denied_host_naming_the_entry() {
        let network = run_network(&["*.amazonaws.com"], &["*.s3.amazonaws.com"]);
        let url = parse_web_fetch_url("https://bucket.s3.amazonaws.com/x").unwrap();
        let err = resolve_public_web_fetch_target(&url, WebGate::Allowlist(&network))
            .await
            .unwrap_err();
        assert!(err.contains("`*.s3.amazonaws.com`"), "{err}");
    }

    #[tokio::test]
    async fn web_fetch_checks_the_allowlist_on_every_redirect_hop() {
        let network = run_network(&["github.com"], &[]);
        let start = parse_web_fetch_url("https://github.com/start").unwrap();
        let redirected = start.join("https://example.org/landing").unwrap();
        let err = resolve_public_web_fetch_target(&redirected, WebGate::Allowlist(&network))
            .await
            .unwrap_err();
        assert!(err.contains("`example.org`"), "{err}");
    }

    #[tokio::test]
    async fn web_fetch_keeps_the_private_address_filter_in_every_mode() {
        let listed = run_network(&["10.0.0.5", "localhost"], &[]);
        for (gate, raw) in [
            (WebGate::AnyHost, "http://127.0.0.1/admin"),
            (WebGate::AnyHost, "http://[::1]/"),
            (WebGate::Allowlist(&listed), "http://10.0.0.5/"),
            (WebGate::Allowlist(&listed), "http://localhost:8080/"),
        ] {
            let url = parse_web_fetch_url(raw).unwrap();
            let err = resolve_public_web_fetch_target(&url, gate)
                .await
                .unwrap_err();
            assert!(err.contains("private/local"), "{raw}: {err}");
        }
    }

    /// `web_fetch` never reaches this computer, so it points at `bash_exec`,
    /// not at `net_request_access`, listed or not.
    #[tokio::test]
    async fn web_fetch_sends_this_computers_localhost_to_bash_exec() {
        let listed = run_network(&["localhost", "127.0.0.1"], &[]);
        let unlisted = run_network(&[], &[]);
        for raw in [
            "http://127.0.0.1:5678/",
            "http://127.1.2.3/",
            "http://[::1]:5678/",
            "http://[::ffff:127.0.0.1]/",
            "http://localhost:5678/",
            "http://api.localhost/",
        ] {
            let url = parse_web_fetch_url(raw).unwrap();
            for (gate, once) in [
                (WebGate::Allowlist(&unlisted), true),
                (WebGate::Allowlist(&listed), true),
                (WebGate::AnyHost, false),
            ] {
                let err = resolve_public_web_fetch_target(&url, gate)
                    .await
                    .unwrap_err();
                assert!(
                    err.contains("`web_fetch` never reaches this computer's localhost. Use curl in `bash_exec` instead"),
                    "{raw}: {err}"
                );
                assert_eq!(
                    err.contains(", once the port is allowed."),
                    once,
                    "{raw}: {err}"
                );
                assert!(!err.contains("net_request_access"), "{raw}: {err}");
            }
        }
        assert!(logged(&unlisted).is_empty());
    }

    #[test]
    fn only_the_allowlist_gate_checks_and_logs_hosts() {
        let network = run_network(&["github.com"], &[]);
        let gate = WebGate::Allowlist(&network);
        assert!(check_web_host(gate, "github.com", Some(443)).is_ok());
        assert!(check_web_host(gate, "example.org", Some(80)).is_err());
        assert!(check_web_host(gate, "[2606:4700::1111]", Some(443)).is_err());
        assert_eq!(
            logged(&network),
            [
                ("github.com".to_string(), Some(443), true),
                ("example.org".to_string(), Some(80), false),
                ("2606:4700::1111".to_string(), Some(443), false),
            ]
        );
        assert!(check_web_host(WebGate::AnyHost, "example.org", Some(443)).is_ok());
    }

    // ------------------------------------------------------------------
    // net_request_access
    // ------------------------------------------------------------------

    fn allowlist_execution(allow: &[&str], deny: &[&str]) -> ExecutionCapabilityConfig {
        let mut execution = ExecutionCapabilityConfig::default();
        execution.sandbox.network_access = allowlist(allow, deny);
        execution.shell.mode = ShellAccessMode::Restricted;
        execution
    }

    #[test]
    fn a_request_is_refused_without_a_card_where_the_tool_is_not_offered() {
        let network = RunNetwork::default();
        let mut execution = allowlist_execution(&[], &[]);
        assert!(
            requested_host(&execution, false, &network, "example.org", None)
                .unwrap_err()
                .contains("not available on this PC")
        );
        for (mode, says) in [
            (NetworkMode::Off, "Network access is off"),
            (NetworkMode::HostNetwork, "can already reach any host"),
        ] {
            execution.sandbox.network_access.mode = mode;
            let error =
                requested_host(&execution, true, &network, "example.org", None).unwrap_err();
            assert!(error.contains(says), "{error}");
        }
    }

    #[test]
    fn a_request_names_one_host_the_lists_do_not_decide() {
        let execution = allowlist_execution(&["github.com"], &["*.s3.amazonaws.com"]);
        let network = RunNetwork::new(&execution.sandbox.network_access);
        let ask = |raw| requested_host(&execution, true, &network, raw, None);

        assert_eq!(
            ask(" Mirror.Example.ORG. "),
            Ok(Ask::Host("mirror.example.org".to_string()))
        );
        assert_eq!(
            ask("[2001:db8::1]"),
            Ok(Ask::Host("2001:db8::1".to_string()))
        );
        assert_eq!(
            ask("GitHub.com"),
            Ok(Ask::Allowed),
            "already allowed: no card"
        );
        let denied = ask("bucket.s3.amazonaws.com").unwrap_err();
        assert!(
            denied.contains("denied hosts (entry `*.s3.amazonaws.com`)")
                && denied.contains("Local access settings"),
            "{denied}"
        );
        for raw in [
            "*.example.org",
            "%2a.com",
            "%2a.example.org",
            "a!b.example.org",
            "https://example.org",
            "example.org/x",
            "example.org:443",
            "a b",
            "",
        ] {
            assert!(ask(raw).is_err(), "{raw:?}");
        }
    }

    #[test]
    fn an_invalid_deny_entry_refuses_web_hosts_and_requests_naming_it() {
        let execution = allowlist_execution(&["github.com"], &["github.com:443"]);
        let network = RunNetwork::new(&execution.sandbox.network_access);
        let errors = [
            check_web_host(WebGate::Allowlist(&network), "github.com", Some(443)).unwrap_err(),
            requested_host(&execution, true, &network, "example.org", None).unwrap_err(),
        ];
        for error in errors {
            assert!(
                error.contains("`github.com:443`") && error.contains("Local access settings"),
                "{error}"
            );
        }
    }

    #[test]
    fn an_answer_widens_this_run_and_a_denial_carries_its_reason() {
        use crate::commands::network_access::NetworkAccessDecision;

        let workspace = tempdir().unwrap();
        let mut context = context_with_grants(workspace.path(), Vec::new());
        context.run_network = std::sync::Arc::new(run_network(&[], &["secret.example.org"]));

        let allowed = apply_network_decision(
            &context,
            "mirror.example.org",
            NetworkAccessDecision::AllowRun,
        )
        .unwrap();
        assert_eq!(allowed["scope"], "run");
        assert_eq!(
            context.run_network.decide("mirror.example.org"),
            HostDecision::Allowed
        );
        assert_eq!(
            context.run_network.decide("www.example.org"),
            HostDecision::Unlisted
        );

        let always = apply_network_decision(
            &context,
            "www.example.org",
            NetworkAccessDecision::AllowAlways {
                entry: "*.example.org".to_string(),
            },
        )
        .unwrap();
        assert_eq!(always["entry"], "*.example.org");
        assert_eq!(
            context.run_network.decide("cdn.example.org"),
            HostDecision::Allowed
        );
        assert!(matches!(
            context.run_network.decide("secret.example.org"),
            HostDecision::Denied { .. }
        ));
        assert!(!context.had_warnings(), "an allow is not a warning");

        let error = apply_network_decision(
            &context,
            "other.example.com",
            NetworkAccessDecision::Deny {
                reason: Some("not needed".to_string()),
            },
        )
        .unwrap_err();
        assert_eq!(
            error,
            "User denied network access to `other.example.com`. Reason: not needed."
        );
        assert!(
            context.had_warnings(),
            "a denial ends the run with warnings"
        );
        assert_eq!(
            context.run_network.decide("other.example.com"),
            HostDecision::Unlisted
        );
    }

    /// Ports on this computer: what the card shows, what the answer binds
    /// to, and what the agent is told.
    #[cfg(target_os = "linux")]
    mod local_port_requests {
        use super::*;
        use crate::assistant::network::local_listener::{Holder, Program, RunOnly};
        use crate::commands::network_access::{NetworkAccessRequest, PendingNetworkRequests};
        use crate::config::types::LocalPortGrant;
        use std::sync::{Arc, Mutex};

        const SSH: &str = "/usr/bin/ssh";
        const PYTHON: &str = "/usr/bin/python3";
        /// In every command line: the card shows it, the agent never sees it.
        const SECRET: &str = "secret-host";

        struct FakeInspector {
            listener: Mutex<Result<Listener, String>>,
            dials: Mutex<Vec<Loopback>>,
        }

        impl FakeInspector {
            fn new(listener: Result<Listener, String>) -> Arc<Self> {
                Arc::new(Self {
                    listener: Mutex::new(listener),
                    dials: Mutex::default(),
                })
            }

            fn serving(listener: Listener) -> Arc<Self> {
                Self::new(Ok(listener))
            }

            fn set(&self, listener: Listener) {
                *self.listener.lock().unwrap() = Ok(listener);
            }

            fn dials(&self) -> Vec<Loopback> {
                self.dials.lock().unwrap().clone()
            }
        }

        impl ListenerInspector for FakeInspector {
            fn inspect(&self, _port: u16, dial: Loopback) -> std::io::Result<Listener> {
                self.dials.lock().unwrap().push(dial);
                self.listener
                    .lock()
                    .unwrap()
                    .clone()
                    .map_err(std::io::Error::other)
            }

            fn still_serves(&self, _port: u16, _dial: Loopback, _programs: &[String]) -> bool {
                panic!("a card comes from a full scan only")
            }
        }

        fn program(exe: &str) -> Listener {
            Listener::Program(Program {
                exe: exe.to_string(),
                pid: 4321,
                cmdline: format!("{exe} -N -L 5432:db:5432 {SECRET}"),
                interpreter: false,
                started_by: None,
            })
        }

        fn other_user(sockets: &[u64]) -> Listener {
            Listener::OtherUser {
                // SAFETY: geteuid has no preconditions and cannot fail.
                uid: unsafe { libc::geteuid() }.wrapping_add(1),
                name: "postgres".to_string(),
                sockets: sockets.to_vec(),
            }
        }

        fn run_only(reason: RunOnly, sockets: &[u64]) -> Listener {
            Listener::RunOnly {
                reason,
                sockets: sockets.to_vec(),
                holders: vec![Holder {
                    pid: 77,
                    exe: "/usr/bin/rootlessport".to_string(),
                    cmdline: format!("rootlessport {SECRET}"),
                }],
                started_by: None,
            }
        }

        fn port_5432() -> LocalAsk {
            LocalAsk {
                port: 5432,
                dialled: "localhost",
            }
        }

        fn context_with(policy: NetworkPolicy) -> (tempfile::TempDir, ToolExecutionContext) {
            let workspace = tempdir().unwrap();
            let mut context = context_with_grants(workspace.path(), Vec::new());
            context.run_network = Arc::new(RunNetwork::new(&policy));
            (workspace, context)
        }

        async fn no_card(_: LocalListenerCard) -> Result<NetworkAccessDecision, String> {
            panic!("no card is shown")
        }

        #[tokio::test]
        async fn a_port_on_this_computer_is_asked_as_localhost_whatever_its_spelling() {
            let execution = allowlist_execution(&[], &[]);
            let network = RunNetwork::new(&execution.sandbox.network_access);
            let ask = |raw, port| requested_host(&execution, true, &network, raw, port);
            for (raw, port, dialled) in [
                ("localhost:5432", None, "localhost"),
                (" LOCALHOST:5432 ", None, "localhost"),
                ("127.0.0.1:5432", None, "127.0.0.1"),
                ("[::1]:5432", None, "::1"),
                ("localhost", Some(5432), "localhost"),
                ("localhost:5432", Some(5432), "localhost"),
                ("::1", Some(5432), "::1"),
                ("[::1]", Some(5432), "::1"),
            ] {
                assert_eq!(
                    ask(raw, port),
                    Ok(Ask::LocalPort(LocalAsk {
                        port: 5432,
                        dialled
                    })),
                    "{raw:?} {port:?}"
                );
            }
            for (raw, port, says) in [
                ("localhost", None, "add the port"),
                ("127.0.0.1", None, "add the port"),
                ("localhost:0", None, "1 to 65535"),
                ("localhost:5432-5433", None, "one port"),
                ("localhost:5432", Some(6379), "give the port once"),
                ("127.0.0.2:5432", None, "as localhost"),
                ("app.localhost:3000", None, "as localhost"),
            ] {
                let error = ask(raw, port).unwrap_err();
                assert!(error.contains(says), "{raw:?}: {error}");
            }

            // A re-ask under another spelling replaces the pending request.
            let request = |ask: Result<Ask, String>| {
                let Ok(Ask::LocalPort(local)) = ask else {
                    panic!("expected a local port, got {ask:?}");
                };
                NetworkAccessRequest {
                    request_id: uuid::Uuid::new_v4().to_string(),
                    workspace_id: Some("ws".to_string()),
                    agent_id: None,
                    agent_name: None,
                    host: LOCALHOST.to_string(),
                    port: Some(local.port),
                    reason: String::new(),
                    run_hosts: Vec::new(),
                    suggested_wildcard: None,
                    persists_to_shared_agent: false,
                    also_affects_workspaces: Vec::new(),
                    local_listener: None,
                }
            };
            let pending = PendingNetworkRequests::default();
            let mut earlier = request(ask("localhost:5432", None));
            for raw in ["[::1]:5432", "127.0.0.1:5432"] {
                let _ = pending.register(earlier.clone(), "run".into()).await;
                let again = request(ask(raw, None));
                let taken = pending
                    .take_superseded("run", &again.host, again.port)
                    .await;
                assert_eq!(taken.len(), 1, "{raw}");
                assert_eq!(taken[0].request_id, earlier.request_id);
                earlier = again;
            }

            let mut no_shell = allowlist_execution(&[], &[]);
            no_shell.shell.mode = ShellAccessMode::Off;
            no_shell.web.enabled = true;
            let error =
                requested_host(&no_shell, true, &network, "localhost:5432", None).unwrap_err();
            assert!(error.contains("bash_exec"), "{error}");
        }

        #[tokio::test]
        async fn nothing_clai_or_a_denied_name_is_refused_without_a_card() {
            for (listener, says) in [
                (Ok(Listener::Nothing), "Nothing listens on port 5432"),
                (Ok(Listener::Clai), "served by CLAI"),
                (Err("permission denied".to_string()), "could not check"),
            ] {
                let inspector = FakeInspector::new(listener);
                let (_workspace, context) = context_with(allowlist(&[], &[]));
                let error = request_local_port(&context, inspector.clone(), port_5432(), no_card)
                    .await
                    .unwrap_err();
                assert!(error.contains(says), "{error}");
                assert!(!error.contains(SECRET), "{error}");
                assert_eq!(
                    context.run_network.decide_local(5432, &program(SSH)),
                    LocalDecision::NotGranted
                );
            }

            let inspector = FakeInspector::serving(Listener::Nothing);
            let (_workspace, context) = context_with(allowlist(&[], &[]));
            let _ = request_local_port(&context, inspector.clone(), port_5432(), no_card).await;
            assert_eq!(inspector.dials(), [Loopback::V4, Loopback::V6]);
            let inspector = FakeInspector::serving(Listener::Nothing);
            let ipv6 = LocalAsk {
                dialled: "::1",
                ..port_5432()
            };
            let _ = request_local_port(&context, inspector.clone(), ipv6, no_card).await;
            assert_eq!(inspector.dials(), [Loopback::V6]);

            for denied in ["localhost", "127.0.0.1", "::1"] {
                let inspector = FakeInspector::serving(program(SSH));
                let (_workspace, context) = context_with(allowlist(&[], &[denied]));
                let error = request_local_port(&context, inspector.clone(), port_5432(), no_card)
                    .await
                    .unwrap_err();
                assert!(error.contains("denied hosts"), "{denied}: {error}");
                assert!(inspector.dials().is_empty(), "not even looked up");
            }
        }

        #[tokio::test]
        async fn the_answer_binds_the_listener_the_card_showed() {
            for (shown, then, allowed, changed) in [
                (program(SSH), program(PYTHON), program(SSH), program(PYTHON)),
                (
                    run_only(RunOnly::OtherNamespace, &[11]),
                    run_only(RunOnly::OtherNamespace, &[12]),
                    run_only(RunOnly::OtherNamespace, &[11]),
                    run_only(RunOnly::OtherNamespace, &[12]),
                ),
            ] {
                let inspector = FakeInspector::serving(shown);
                let (_workspace, context) = context_with(allowlist(&[], &[]));
                let changing = inspector.clone();
                let result = request_local_port(&context, inspector, port_5432(), |_| {
                    // The listener changes before the user answers.
                    changing.set(then);
                    async { Ok(NetworkAccessDecision::AllowRun) }
                })
                .await
                .unwrap();
                assert_eq!(result["scope"], "run");
                let network = &context.run_network;
                assert_eq!(network.decide_local(5432, &allowed), LocalDecision::Allowed);
                assert_eq!(network.decide_local(5432, &changed), LocalDecision::Changed);
                assert_eq!(
                    network.decide_local(6379, &allowed),
                    LocalDecision::NotGranted
                );
            }
        }

        /// As “Always” would save it, “Allow this run” for another user's
        /// service allows that user on the port: the socket the card did not
        /// show (the other loopback address) and the service restarted
        /// mid-run are the same service.
        #[tokio::test]
        async fn allow_this_run_for_another_users_service_allows_that_user() {
            let inspector = FakeInspector::serving(other_user(&[11]));
            let (_workspace, context) = context_with(allowlist(&[], &[]));
            let result = request_local_port(&context, inspector, port_5432(), |_| async {
                Ok(NetworkAccessDecision::AllowRun)
            })
            .await
            .unwrap();
            assert_eq!(result["scope"], "run");
            let network = &context.run_network;
            let ipv4 = other_user(&[11]);
            let ipv6 = other_user(&[12]);
            let restarted = other_user(&[13]);
            for listener in [&ipv4, &ipv6, &restarted] {
                assert_eq!(
                    network.decide_local(5432, listener),
                    LocalDecision::Allowed,
                    "{listener:?}"
                );
            }
            let someone_else = Listener::OtherUser {
                // SAFETY: geteuid has no preconditions and cannot fail.
                uid: unsafe { libc::geteuid() }.wrapping_add(2),
                name: "redis".to_string(),
                sockets: vec![11],
            };
            assert_eq!(
                network.decide_local(5432, &someone_else),
                LocalDecision::Changed
            );
            assert_eq!(network.decide_local(6379, &ipv6), LocalDecision::NotGranted);
        }

        /// A user grant never names the user CLAI runs as: a listener of
        /// that user is allowed for the run by the sockets the card showed.
        #[tokio::test]
        async fn allow_this_run_never_allows_the_user_clai_runs_as() {
            // SAFETY: geteuid has no preconditions and cannot fail.
            let own = |sockets: &[u64]| Listener::OtherUser {
                uid: unsafe { libc::geteuid() },
                name: "me".to_string(),
                sockets: sockets.to_vec(),
            };
            let inspector = FakeInspector::serving(own(&[11]));
            let (_workspace, context) = context_with(allowlist(&[], &[]));
            request_local_port(&context, inspector, port_5432(), |_| async {
                Ok(NetworkAccessDecision::AllowRun)
            })
            .await
            .unwrap();
            let network = &context.run_network;
            assert_eq!(
                network.decide_local(5432, &own(&[11])),
                LocalDecision::Allowed
            );
            assert_eq!(
                network.decide_local(5432, &own(&[12])),
                LocalDecision::Changed
            );
        }

        #[tokio::test]
        async fn always_adds_the_saved_grant_and_the_result_names_the_port_only() {
            let always = || NetworkAccessDecision::AllowAlways {
                entry: "localhost:5432".to_string(),
            };
            for (listener, decision, scope, later) in [
                (
                    program(SSH),
                    NetworkAccessDecision::AllowRun,
                    "run",
                    program(SSH),
                ),
                (program(SSH), always(), "always", program(SSH)),
                // A user grant covers a restarted service of that user.
                (other_user(&[11]), always(), "always", other_user(&[12])),
            ] {
                let inspector = FakeInspector::serving(listener);
                let (_workspace, context) = context_with(allowlist(&[], &[]));
                let mut shown = Vec::new();
                let result = request_local_port(&context, inspector, port_5432(), |card| {
                    shown.push(serde_json::to_value(&card).unwrap());
                    async move { Ok(decision) }
                })
                .await
                .unwrap();

                let mut keys: Vec<&str> = result
                    .as_object()
                    .unwrap()
                    .keys()
                    .map(String::as_str)
                    .collect();
                keys.sort_unstable();
                assert_eq!(keys, ["entry", "granted", "host", "note", "port", "scope"]);
                assert_eq!(result["host"], "localhost");
                assert_eq!(result["port"], 5432);
                assert_eq!(result["entry"], "localhost:5432");
                assert_eq!(result["scope"], scope);
                assert!(!result.to_string().contains(SECRET), "{result}");
                assert_eq!(
                    context.run_network.decide_local(5432, &later),
                    LocalDecision::Allowed
                );
                let [card] = shown.as_slice() else {
                    panic!("one card, got {shown:?}");
                };
                if card["kind"] == "program" {
                    let command_line = card["commandLine"].as_str().unwrap();
                    assert!(command_line.contains(SECRET), "the card shows it");
                }
            }
        }

        #[tokio::test]
        async fn a_denial_is_the_tool_error_and_allows_nothing() {
            let inspector = FakeInspector::serving(program(SSH));
            let (_workspace, context) = context_with(allowlist(&[], &[]));
            let error = request_local_port(&context, inspector, port_5432(), |_| async {
                Ok(NetworkAccessDecision::Deny {
                    reason: Some("use the staging database".to_string()),
                })
            })
            .await
            .unwrap_err();
            assert_eq!(
                error,
                "User denied network access to `localhost:5432`. Reason: use the staging database."
            );
            assert!(context.had_warnings());
            assert_eq!(
                context.run_network.decide_local(5432, &program(SSH)),
                LocalDecision::NotGranted
            );
        }

        #[tokio::test]
        async fn a_listener_the_run_already_reaches_needs_no_card() {
            let mut policy = allowlist(&[], &[]);
            policy.local_ports = vec![LocalPortGrant::Program {
                port: 5432,
                program: SSH.to_string(),
            }];
            let (_workspace, context) = context_with(policy);
            let result = request_local_port(
                &context,
                FakeInspector::serving(program(SSH)),
                port_5432(),
                no_card,
            )
            .await
            .unwrap();
            assert_eq!(
                result,
                serde_json::json!({
                    "granted": true,
                    "host": "localhost",
                    "port": 5432,
                    "scope": "already-allowed",
                })
            );
        }
    }

    fn network_guard(
        token: &tokio_util::sync::CancellationToken,
        cleared: &std::sync::Arc<std::sync::atomic::AtomicUsize>,
    ) -> AbandonedNetworkRequestGuard<impl FnOnce()> {
        let cleared = cleared.clone();
        AbandonedNetworkRequestGuard {
            cancel_token: token.clone(),
            clear: Some(move || {
                cleared.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }),
        }
    }

    #[test]
    fn an_abandoned_network_wait_clears_its_request_and_cancels_the_run() {
        use std::sync::atomic::Ordering;
        let cleared = std::sync::Arc::default();

        let token = tokio_util::sync::CancellationToken::new();
        drop(network_guard(&token, &cleared));
        assert!(token.is_cancelled());
        assert_eq!(cleared.load(Ordering::SeqCst), 1);

        let token = tokio_util::sync::CancellationToken::new();
        let mut decided = network_guard(&token, &cleared);
        decided.disarm();
        drop(decided);
        assert!(!token.is_cancelled(), "a decided wait leaves the run alone");
        assert_eq!(cleared.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_wait_whose_sender_is_gone_clears_once_and_parks_the_cancelled_run() {
        use std::sync::atomic::Ordering;
        let cleared = std::sync::Arc::default();
        let token = tokio_util::sync::CancellationToken::new();
        let mut guard = network_guard(&token, &cleared);

        let parked = tokio::time::timeout(
            std::time::Duration::from_millis(20),
            guard.expire_and_stop::<()>(),
        )
        .await;
        assert!(parked.is_err(), "the tool returns no result");
        assert!(token.is_cancelled());
        assert_eq!(cleared.load(Ordering::SeqCst), 1);
        drop(guard);
        assert_eq!(cleared.load(Ordering::SeqCst), 1, "cleared once");
    }

    #[tokio::test]
    async fn web_tools_refuse_before_any_request_when_network_is_off() {
        let workspace = tempdir().unwrap();
        let mut context = context_with_grants(workspace.path(), Vec::new());
        let mut execution = context.run_context.execution.clone();
        execution.sandbox.network_access = network(NetworkMode::Off);
        context.run_context = std::sync::Arc::new(RunContext {
            execution,
            ..(*context.run_context).clone()
        });
        for (tool, params) in [
            ("web_search", serde_json::json!({ "query": "rust" })),
            // Private, so even a missing gate fails fast without a request.
            (
                "web_fetch",
                serde_json::json!({ "url": "http://192.168.1.1/" }),
            ),
        ] {
            assert_eq!(
                execute_web_tool(&context, tool, params).await.unwrap_err(),
                "Network access is off for this agent",
                "{tool}"
            );
        }
    }

    /// A server on 127.0.0.1 that answers every request with a redirect to
    /// itself, and counts the requests.
    async fn redirecting_server() -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let hits = std::sync::Arc::new(AtomicUsize::new(0));
        let counter = hits.clone();
        let response = format!(
            "HTTP/1.1 302 Found\r\nLocation: {base}/next\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        );
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                counter.fetch_add(1, Ordering::SeqCst);
                let response = response.clone();
                tokio::spawn(async move {
                    let mut head = [0u8; 4096];
                    let _ = socket.read(&mut head).await;
                    let _ = socket.write_all(response.as_bytes()).await;
                    let _ = socket.shutdown().await;
                });
            }
        });
        (base, hits)
    }

    #[tokio::test]
    async fn web_search_refuses_a_redirect_off_the_provider() {
        use std::sync::atomic::Ordering;
        let (base, hits) = redirecting_server().await;
        let error = web_search_client()
            .unwrap()
            .get(format!("{base}/start"))
            .send()
            .await
            .unwrap_err();
        assert!(
            format!("{error:?}").contains("leaves the search provider"),
            "{error:?}"
        );
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn the_provider_redirect_policy_follows_at_most_three_hops() {
        use std::sync::atomic::Ordering;
        let (base, hits) = redirecting_server().await;
        let error = reqwest::Client::builder()
            .redirect(redirect_policy_within(|_| true))
            .build()
            .unwrap()
            .get(format!("{base}/start"))
            .send()
            .await
            .unwrap_err();
        assert!(
            format!("{error:?}").contains("too many redirects"),
            "{error:?}"
        );
        // The first request and three redirects, as `Policy::limited(3)`.
        assert_eq!(hits.load(Ordering::SeqCst), 1 + MAX_WEB_SEARCH_REDIRECTS);
    }

    #[test]
    fn web_search_redirects_stay_on_the_provider_over_https() {
        for (raw, allowed) in [
            ("https://html.duckduckgo.com/html/?q=x", true),
            ("https://duckduckgo.com/", true),
            ("http://html.duckduckgo.com/html/", false),
            ("https://duckduckgo.com.evil.org/", false),
            ("https://evilduckduckgo.com/", false),
            ("https://example.org/", false),
        ] {
            let url = reqwest::Url::parse(raw).unwrap();
            assert_eq!(provider_redirect_allowed(&url), allowed, "{raw}");
        }
    }

    // ------------------------------------------------------------------
    // Network mode in bash_exec
    // ------------------------------------------------------------------

    fn network(mode: NetworkMode) -> NetworkPolicy {
        NetworkPolicy {
            mode,
            allow_domains: Vec::new(),
            deny_domains: Vec::new(),
            local_ports: Vec::new(),
        }
    }

    fn test_endpoint() -> ProxyEndpoint {
        ProxyEndpoint {
            socket: PathBuf::from("/c/.clai-net/0123456789ab.sock"),
            helper: PathBuf::from("/opt/clai/clai-sandbox-helper"),
            ssh_config: PathBuf::from("/c/.clai-net/ssh_config"),
            forward_ports: Vec::new(),
        }
    }

    #[test]
    fn network_modes_map_onto_the_sandbox_network_per_platform() {
        use crate::assistant::sandbox::{status_for, SandboxBackend};
        use NetworkMode::{Allowlist, HostNetwork, Off};
        use SandboxNetworkMode::{Disabled, Host, Proxied};
        let linux = status_for(Some(SandboxBackend::Bubblewrap));
        let macos = status_for(Some(SandboxBackend::Seatbelt));
        let none = status_for(None);
        for (status, mode, expected) in [
            (&linux, Off, Ok(Disabled)),
            (&linux, Allowlist, Ok(Proxied(test_endpoint()))),
            (&linux, HostNetwork, Ok(Host)),
            (&macos, Off, Ok(Disabled)),
            (&macos, Allowlist, Err(())),
            (&macos, HostNetwork, Ok(Host)),
            (&none, Off, Ok(Disabled)),
            (&none, Allowlist, Err(())),
            (&none, HostNetwork, Ok(Host)),
        ] {
            let asked = std::cell::Cell::new(false);
            let endpoint = || {
                asked.set(true);
                Ok(test_endpoint())
            };
            assert_eq!(
                sandbox_network(mode, status, endpoint).map_err(|_| ()),
                expected,
                "{:?} {mode:?}",
                status.backend
            );
            assert_eq!(
                asked.get(),
                matches!(expected, Ok(Proxied(_))),
                "only an enforceable Allowlist prepares the proxy files"
            );
        }
        assert_eq!(
            sandbox_network(Allowlist, &macos, || Ok(test_endpoint())).unwrap_err(),
            "Allowlist network mode: Not available on macOS yet. Choose Off or Host network \
             in the agent's Local access settings."
        );
        let missing = || Err("Sandboxed shell is unavailable: network helper not found".into());
        assert_eq!(
            sandbox_network(Allowlist, &linux, missing),
            Err("Sandboxed shell is unavailable: network helper not found".to_string()),
            "a missing helper fails closed, never the host network"
        );
    }

    /// The sandbox of a command is recorded as started by its agent while
    /// the command runs, so a server it starts can be asked for. Needs bwrap.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn an_agent_command_is_recorded_while_it_runs() {
        use crate::assistant::network::spawn_roots::{self, StartedBy};
        let workspace = tempdir().unwrap();
        let agent = || Some("Ana, in bash_exec".to_string());
        let by = StartedBy::Agent(agent());
        let command = SandboxCommand {
            argv: vec!["/bin/sh".into(), "-c".into(), "sleep 1".into()],
            cwd: workspace.path().to_path_buf(),
            timeout_ms: 10_000,
            max_output_chars: 10_000,
            profile: SandboxProfile::for_test(
                workspace.path().to_path_buf(),
                Vec::new(),
                SandboxNetworkMode::Disabled,
                SandboxSessionBusMode::Deny,
                SandboxEnv::filtered_from_iter(
                    [("PATH", "/usr/bin:/bin")],
                    workspace.path(),
                    SandboxSessionBusMode::Deny,
                ),
                None,
            ),
        };
        let network = std::sync::Arc::new(run_network(&[], &[]));
        let run = run_with_network(command, agent(), &network);
        let running = spawn_roots::eventually(|| !spawn_roots::started_by(&by).is_empty());
        let (run, running) = tokio::join!(run, running);
        assert_eq!(
            run.allowed_local_ports, None,
            "no proxy, so no localhost hint"
        );
        match run.output {
            Ok(output) => assert_eq!(output.exit_code, Some(0), "{output:?}"),
            Err(error) if error.contains("Sandboxed shell is unavailable") => {
                crate::assistant::sandbox::skip_without_bwrap(&error);
                return;
            }
            Err(error) => panic!("{error}"),
        }
        assert!(running, "recorded while the command runs");
        assert!(spawn_roots::started_by(&by).is_empty(), "gone once reaped");
    }

    /// The helper `cargo test` builds next to the test binary.
    #[cfg(target_os = "linux")]
    fn built_helper() -> Option<PathBuf> {
        let exe = std::env::current_exe().unwrap();
        let helper = exe
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("clai-sandbox-helper");
        if helper.is_file() {
            return Some(helper);
        }
        crate::assistant::sandbox::skip_without_bwrap(&format!(
            "{} is not built",
            helper.display()
        ));
        None
    }

    /// An Allowlist command running `script` in `workspace`, with its proxy
    /// files in `files`.
    #[cfg(target_os = "linux")]
    fn allowlist_command(
        workspace: &Path,
        files: &Path,
        helper: PathBuf,
        script: &str,
        timeout_ms: u64,
    ) -> SandboxCommand {
        let endpoint = ProxyEndpoint {
            socket: files.join("net.sock"),
            helper,
            ssh_config: files.join("ssh_config"),
            forward_ports: Vec::new(),
        };
        std::fs::write(&endpoint.ssh_config, "").unwrap();
        SandboxCommand {
            argv: vec!["/bin/sh".into(), "-c".into(), script.into()],
            cwd: workspace.to_path_buf(),
            timeout_ms,
            max_output_chars: 10_000,
            profile: SandboxProfile::for_test(
                workspace.to_path_buf(),
                Vec::new(),
                SandboxNetworkMode::Proxied(endpoint),
                SandboxSessionBusMode::Deny,
                SandboxEnv::filtered_from_iter(
                    [("PATH", "/usr/bin:/bin")],
                    workspace,
                    SandboxSessionBusMode::Deny,
                ),
                None,
            ),
        }
    }

    /// The proxy session lives for the command and its refusals reach the
    /// result. Needs bwrap and the helper `cargo test` builds.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn an_allowlist_command_runs_behind_its_own_proxy_session() {
        let Some(helper) = built_helper() else {
            return;
        };
        let files = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let socket = files.path().join("net.sock");
        let command = |script: &str, timeout_ms| {
            allowlist_command(
                workspace.path(),
                files.path(),
                helper.clone(),
                script,
                timeout_ms,
            )
        };
        let refuse = "/run/clai/helper connect --proxy 127.0.0.1:3128 example.org 443 </dev/null";
        let network = std::sync::Arc::new(run_network(&["github.com"], &[]));
        let run = run_with_network(command(refuse, 20_000), None, &network).await;
        let (output, refused) = match run.output {
            Ok(output) => (output, run.refused),
            Err(error) if error.contains("Sandboxed shell is unavailable") => {
                crate::assistant::sandbox::skip_without_bwrap(&error);
                return;
            }
            Err(error) => panic!("{error}"),
        };
        assert_eq!(output.exit_code, Some(255), "{output:?}");
        assert_eq!(refused.targets, ["example.org:443"]);
        assert_eq!(
            logged(&network),
            [("example.org".to_string(), Some(443), false)]
        );
        assert!(!socket.exists(), "the session ended with the command");

        let run = run_with_network(
            command(&format!("{refuse}; sleep 30"), 2_000),
            None,
            &network,
        )
        .await;
        let refused = run.refused;
        let error = run.output.unwrap_err();
        assert!(error.contains("timed out after"), "{error}");
        assert_eq!(
            refused.targets,
            ["example.org:443"],
            "a timed-out command keeps its refusals"
        );
        assert!(!socket.exists(), "the session ended with the timeout");
    }

    /// The local ports a command forwards are those of the run as it starts:
    /// one allowed while it runs answers from the next command. This test
    /// listens on the port, so the proxy refuses it as CLAI's own; only a
    /// forwarded port gets that far. Needs bwrap.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn a_port_allowed_during_a_command_is_forwarded_from_the_next() {
        use crate::assistant::network::run_state::{LocalBinding, LocalRule};
        use std::sync::atomic::{AtomicBool, Ordering};
        let Some(helper) = built_helper() else {
            return;
        };
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let files = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let connect = format!(
            "python3 -c 'import socket; socket.create_connection((\"127.0.0.1\", {port}), 5).recv(1); print(\"connected\")' 2>/dev/null || echo refused"
        );
        let command = |script: &str| {
            allowlist_command(
                workspace.path(),
                files.path(),
                helper.clone(),
                script,
                20_000,
            )
        };
        let network = std::sync::Arc::new(run_network(&[], &[]));
        let finished = AtomicBool::new(false);
        let first = async {
            let script =
                format!("touch started; while [ ! -e go ]; do sleep 0.05; done; {connect}");
            let run = run_with_network(command(&script), None, &network).await;
            finished.store(true, Ordering::SeqCst);
            run
        };
        let grant = async {
            while !workspace.path().join("started").exists() && !finished.load(Ordering::SeqCst) {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
            network.allow_local(LocalRule {
                port,
                binding: LocalBinding::Program("/usr/bin/served".to_string()),
            });
            std::fs::write(workspace.path().join("go"), "").unwrap();
        };
        let (first, ()) = tokio::join!(first, grant);
        let output = match first.output {
            Ok(output) => output,
            Err(error) if error.contains("Sandboxed shell is unavailable") => {
                crate::assistant::sandbox::skip_without_bwrap(&error);
                return;
            }
            Err(error) => panic!("{error}"),
        };
        assert_eq!(output.stdout, "refused\n", "{output:?}");
        assert!(first.refused.is_empty(), "{:?}", first.refused);
        assert_eq!(first.allowed_local_ports, Some(vec![port]));

        let next = run_with_network(command(&connect), None, &network).await;
        let output = next.output.unwrap();
        assert_eq!(output.stdout, "connected\n", "{output:?}");
        assert_eq!(next.refused.targets, [format!("localhost:{port}")]);
    }

    #[test]
    fn a_failed_command_still_names_the_hosts_the_proxy_refused() {
        let mut refused = RefusedTargets::default();
        let timed_out = || Err("Command timed out after 2000 ms".to_string());
        let error = bash_exec_outcome(
            timed_out(),
            &refused,
            &SocketPolicy::default(),
            None,
            2_000,
            true,
        )
        .unwrap_err();
        assert!(!error.contains("Network allowlist refused"), "{error}");

        refused.add("example.org:443".to_string());
        refused.add("pypi.org:443".to_string());
        refused.more = 2;
        let error = bash_exec_outcome(
            timed_out(),
            &refused,
            &SocketPolicy::default(),
            None,
            2_000,
            true,
        )
        .unwrap_err();
        let (timeout, network) = error.split_once('\n').unwrap();
        assert!(
            timeout.contains("re-run with a higher `timeoutMs`"),
            "{error}"
        );
        assert_eq!(
            network,
            format!("Network allowlist refused: example.org:443, pypi.org:443 and 2 more. {REFUSED_HINT}")
        );
    }

    #[test]
    fn a_result_names_the_hosts_the_proxy_refused() {
        let output = SandboxCommandOutput {
            cwd: PathBuf::from("/w"),
            exit_code: Some(7),
            success: false,
            stdout: String::new(),
            stderr: "curl: (56) CONNECT tunnel failed, response 403".to_string(),
        };
        assert!(bash_exec_result(
            &output,
            &RefusedTargets::default(),
            &SocketPolicy::default(),
            None
        )
        .get("network")
        .is_none());
        let mut refused = RefusedTargets::default();
        refused.add("example.org:443".to_string());
        let result = bash_exec_result(&output, &refused, &SocketPolicy::default(), None);
        assert_eq!(
            result["network"]["refused"],
            serde_json::json!(["example.org:443"])
        );
        assert!(result["network"].get("refusedMore").is_none());
        let hint = result["network"]["hint"].as_str().unwrap();
        assert!(hint.contains("net_request_access"), "{hint}");
        assert_eq!(result["exitCode"], 7);

        refused.more = 3;
        assert_eq!(
            bash_exec_result(&output, &refused, &SocketPolicy::default(), None)["network"]
                ["refusedMore"],
            3
        );
    }

    /// One line per client as it reports a refused connection to localhost,
    /// and the port it names.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_refused_localhost_connection_is_read_from_the_client_output() {
        for (line, port) in [
            (r#"psql: error: connection to server at "localhost" (127.0.0.1), port 5432 failed: Connection refused"#, Some(5432)),
            (r#"psql: error: connection to server at "localhost" (::1), port 5678 failed: Connection refused"#, Some(5678)),
            ("Could not connect to Redis at 127.0.0.1:6379: Connection refused", Some(6379)),
            ("curl: (7) Failed to connect to localhost port 8080 after 0 ms: Couldn't connect to server", Some(8080)),
            ("curl: (7) Failed to connect to ::1 port 8081 after 0 ms: Connection refused", Some(8081)),
            ("Error: connect ECONNREFUSED 127.0.0.1:3000", Some(3000)),
            ("Error: connect ECONNREFUSED ::1:3001", Some(3001)),
            ("ERROR 2003 (HY000): Can't connect to MySQL server on 'localhost:3306' (111)", Some(3306)),
            ("Can\u{2019}t connect to MySQL server on \u{2019}localhost:3307\u{2019}", Some(3307)),
            ("ERROR 2003 (HY000): Can't connect to MySQL server on '127.0.0.1' (111)", None),
            ("requests.exceptions.ConnectionError: HTTPConnectionPool(host='localhost', port=8000): Max retries exceeded with url: / (Caused by NewConnectionError('<urllib3.connection.HTTPConnection object at 0x7f>: Failed to establish a new connection: [Errno 111] Connection refused'))", Some(8000)),
            ("ConnectionRefusedError: [Errno 111] Connect call failed ('127.0.0.1', 5679)", Some(5679)),
            ("nc: connect to localhost (127.0.0.1) port 5680 (tcp) failed: Connection refused", Some(5680)),
            ("bash: line 1: /dev/tcp/localhost/5681: Connection refused", Some(5681)),
            ("dial tcp [::1]:9000: connect: connection refused", Some(9000)),
        ] {
            assert_eq!(refused_localhost_port(line), Some(port), "{line}");
        }
        for line in [
            r#"psql: error: connection to server at "db.example.org" (10.0.0.5), port 5432 failed: Connection refused"#,
            "Listening on http://localhost:8080",
            "connected to 127.0.0.1:6379",
            "curl: (7) Failed to connect to localhost.example.org port 80: Connection refused",
            "dial tcp [fe80::1]:9000: connect: connection refused",
            "Error: connect ECONNREFUSED 127.0.0.10:3000",
            // Python and Ncat name no host here: out of reach of the hint.
            "ConnectionRefusedError: [Errno 111] Connection refused",
            "Ncat: Connection refused.",
        ] {
            assert_eq!(refused_localhost_port(line), None, "{line}");
        }
    }

    /// In Allowlist only, and not for a port the run allows (nothing listens
    /// there then) or one the proxy already named.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_localhost_hint_is_for_ports_the_run_does_not_allow() {
        let output = |stderr: &str| SandboxCommandOutput {
            cwd: PathBuf::from("/w"),
            exit_code: Some(2),
            success: false,
            stdout: String::new(),
            stderr: stderr.to_string(),
        };
        let psql = |port: u16| {
            format!(
                r#"psql: error: connection to server at "localhost" (::1), port {port} failed: Connection refused"#
            )
        };
        let none = RefusedTargets::default();
        let refused = output(&psql(5678));
        assert_eq!(
            localhost_refusal(&refused, Some(&[]), &none),
            Some(Some(5678))
        );
        assert_eq!(
            localhost_refusal(&refused, Some(&[5432]), &none),
            Some(Some(5678))
        );
        assert_eq!(localhost_refusal(&refused, Some(&[5678]), &none), None);
        assert_eq!(
            localhost_refusal(&refused, None, &none),
            None,
            "Host network or Off"
        );
        let mut named = RefusedTargets::default();
        named.add("localhost:5678".to_string());
        assert_eq!(localhost_refusal(&refused, Some(&[]), &named), None);

        let both = output(&format!("{}\n{}", psql(5432), psql(6379)));
        assert_eq!(
            localhost_refusal(&both, Some(&[5432]), &none),
            Some(Some(6379))
        );
        let portless =
            output("ERROR 2003 (HY000): Can't connect to MySQL server on '127.0.0.1' (111)");
        assert_eq!(localhost_refusal(&portless, Some(&[]), &none), Some(None));
        let on_stdout = SandboxCommandOutput {
            stdout: psql(5678),
            ..output("")
        };
        assert_eq!(
            localhost_refusal(&on_stdout, Some(&[]), &none),
            Some(Some(5678))
        );
    }

    #[test]
    fn the_localhost_note_names_the_port_to_ask_for() {
        let note = localhost_note(Some(5678));
        assert_eq!(note["port"], 5678);
        let hint = note["hint"].as_str().unwrap();
        assert!(
            hint.contains("call `net_request_access` with host `localhost:5678`"),
            "{hint}"
        );
        assert!(hint.contains("not this computer's"), "{hint}");
        let note = localhost_note(None);
        assert!(note.get("port").is_none());
        assert!(note["hint"]
            .as_str()
            .unwrap()
            .contains("`localhost:<port>`"));
    }

    /// The hint leads to the folder-access card, for the socket itself.
    #[test]
    fn the_socket_hint_asks_for_a_grant_on_the_socket_and_says_why() {
        assert!(SOCKET_HINT.contains("call `fs_request_grant` with the exact socket path"));
        assert!(SOCKET_HINT.contains("not its folder"));
        assert!(SOCKET_HINT.contains("A Docker, Podman or containerd socket gives control"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_result_names_the_host_sockets_a_failed_command_could_not_reach() {
        let temp = tempdir().unwrap();
        let socket = temp.path().canonicalize().unwrap().join("docker.sock");
        let _listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let file = temp.path().join("plain.sock");
        fs::write(&file, "").unwrap();
        let path = socket.display().to_string();
        let output = |success: bool, stderr: &str| SandboxCommandOutput {
            cwd: PathBuf::from("/w"),
            exit_code: Some(if success { 0 } else { 1 }),
            success,
            stdout: String::new(),
            stderr: stderr.to_string(),
        };
        let unreachable = |output: &SandboxCommandOutput, sockets: &SocketPolicy| {
            bash_exec_result(output, &RefusedTargets::default(), sockets, None)
                .get("sockets")
                .map(|sockets| sockets["unreachable"].clone())
        };
        let confined = SocketPolicy {
            helper: Some(PathBuf::from("/opt/h")),
            kernel_confines: true,
            allowed: Vec::new(),
            skipped: Vec::new(),
            hint_places: vec![temp.path().canonicalize().unwrap()],
        };
        let denied = output(
            false,
            &format!(
                "permission denied while trying to connect to the Docker daemon socket at \
                 unix://{path}: dial unix {path}: connect: permission denied\nopen {}: \
                 permission denied",
                file.display()
            ),
        );
        assert_eq!(
            unreachable(&denied, &confined),
            Some(serde_json::json!([path]))
        );
        let result = bash_exec_result(&denied, &RefusedTargets::default(), &confined, None);
        assert_eq!(result["sockets"]["hint"], SOCKET_HINT);

        // Without Landlock a refusal comes from the host's permissions.
        let unconfined = SocketPolicy {
            kernel_confines: false,
            ..confined.clone()
        };
        assert_eq!(unreachable(&denied, &unconfined), None);
        // A socket not bound into the sandbox is named either way.
        let missing = output(
            false,
            &format!("dial unix {path}: connect: no such file or directory"),
        );
        assert_eq!(
            unreachable(&missing, &unconfined),
            Some(serde_json::json!([path]))
        );
        let allowed = SocketPolicy {
            allowed: vec![crate::assistant::sandbox::AllowedSocket {
                path: socket.clone(),
                host: socket.clone(),
            }],
            ..confined.clone()
        };
        assert_eq!(unreachable(&denied, &allowed), None);
        assert_eq!(unreachable(&output(true, &denied.stderr), &confined), None);

        // m3: a path outside the usual socket places is never looked up, so
        // a command cannot probe the host by printing paths.
        let elsewhere = tempdir().unwrap();
        let hidden = elsewhere.path().canonicalize().unwrap().join("hidden.sock");
        let _hidden = std::os::unix::net::UnixListener::bind(&hidden).unwrap();
        let probe = |path: String| output(false, &format!("{path}: no such file or directory"));
        assert_eq!(
            unreachable(&probe(hidden.display().to_string()), &confined),
            None
        );
        // Nor is one that only seems to be in such a place.
        let dotdot = format!(
            "{}/../{}",
            temp.path().canonicalize().unwrap().display(),
            hidden
                .strip_prefix(elsewhere.path().parent().unwrap().canonicalize().unwrap())
                .unwrap()
                .display()
        );
        assert_eq!(unreachable(&probe(dotdot), &confined), None);
        // At most 16 lookups per result: a socket after 16 other paths is
        // not named.
        let mut many: String = (0..16)
            .map(|index| {
                format!(
                    "{}/missing-{index}: no such file or directory\n",
                    temp.path().display()
                )
            })
            .collect();
        many.push_str(&missing.stderr);
        assert_eq!(unreachable(&output(false, &many), &unconfined), None);
        // Paths elsewhere are not even looked up, so they use none of them.
        let mut elsewhere_paths: String = (0..16)
            .map(|index| format!("/elsewhere/missing-{index}: no such file or directory\n"))
            .collect();
        elsewhere_paths.push_str(&missing.stderr);
        assert_eq!(
            unreachable(&output(false, &elsewhere_paths), &unconfined),
            Some(serde_json::json!([path]))
        );
    }

    #[test]
    fn the_shell_profile_takes_its_network_from_the_mode() {
        use crate::assistant::sandbox::{status_for, SandboxBackend};
        let workspace = tempdir().unwrap();
        let profile = |mode, status| {
            let mut context = context_with_grants(workspace.path(), Vec::new());
            let mut execution = context.run_context.execution.clone();
            execution.sandbox.network_access = network(mode);
            context.run_context = std::sync::Arc::new(RunContext {
                execution,
                ..(*context.run_context).clone()
            });
            let filesystem = filesystem_policy(&context).unwrap();
            sandbox_profile(&context, workspace.path().to_path_buf(), filesystem, status)
        };
        let linux = status_for(Some(SandboxBackend::Bubblewrap));
        assert_eq!(
            profile(NetworkMode::Off, &linux).unwrap().network,
            SandboxNetworkMode::Disabled
        );
        // The test binary has no helper beside it, as an install without one.
        #[cfg(target_os = "linux")]
        assert!(profile(NetworkMode::Allowlist, &linux)
            .unwrap_err()
            .starts_with("Sandboxed shell is unavailable: network helper not found"));
        let macos = status_for(Some(SandboxBackend::Seatbelt));
        assert!(profile(NetworkMode::Allowlist, &macos).is_err());
    }

    #[test]
    fn only_full_binds_the_whole_bus_and_keyring_only_fails_closed() {
        let proxy = SessionBusProxy {
            socket: PathBuf::from("/c/.clai-net/bus-1.sock"),
            bus: PathBuf::from("/run/user/1000/bus"),
        };
        let unused = || -> Result<Option<SessionBusProxy>, String> { panic!("no plan needed") };
        assert_eq!(
            session_bus_for(SandboxSessionBusConfig::Full, unused),
            (SandboxSessionBusMode::Allow, None, None)
        );
        assert_eq!(
            session_bus_for(SandboxSessionBusConfig::Deny, unused),
            (SandboxSessionBusMode::Deny, None, None)
        );
        let keyring = |plan: Result<Option<SessionBusProxy>, String>| {
            session_bus_for(SandboxSessionBusConfig::KeyringOnly, || plan)
        };
        assert_eq!(
            keyring(Ok(Some(proxy.clone()))),
            (SandboxSessionBusMode::Allow, Some(proxy), None)
        );
        assert_eq!(
            keyring(Err("xdg-dbus-proxy is not installed".to_string())),
            (
                SandboxSessionBusMode::Deny,
                None,
                Some("xdg-dbus-proxy is not installed".to_string())
            )
        );
        assert_eq!(keyring(Ok(None)), (SandboxSessionBusMode::Deny, None, None));
    }

    /// The review's B1 case: a keyring-only agent whose workspace is outside
    /// the home directory. Whatever this host offers, the profile never binds
    /// the real bus: it has the proxy, or no bus.
    #[test]
    fn a_keyring_only_profile_never_binds_the_real_bus() {
        use crate::assistant::sandbox::{status_for, SandboxBackend};
        let workspace = tempdir().unwrap();
        let mut context = context_with_grants(workspace.path(), Vec::new());
        let mut execution = context.run_context.execution.clone();
        execution.sandbox.network_access = network(NetworkMode::Off);
        execution.sandbox.session_bus = SandboxSessionBusConfig::KeyringOnly;
        context.run_context = std::sync::Arc::new(RunContext {
            execution,
            ..(*context.run_context).clone()
        });
        let filesystem = filesystem_policy(&context).unwrap();
        let profile = sandbox_profile(
            &context,
            workspace.path().to_path_buf(),
            filesystem,
            &status_for(Some(SandboxBackend::Bubblewrap)),
        )
        .unwrap();
        match profile.session_bus {
            SandboxSessionBusMode::Allow => assert!(profile.session_bus_proxy.is_some()),
            SandboxSessionBusMode::Deny => {
                assert!(profile.session_bus_proxy.is_none());
                let address: Vec<_> = profile
                    .env
                    .iter()
                    .filter(|(key, _)| *key == "DBUS_SESSION_BUS_ADDRESS")
                    .map(|(_, value)| value)
                    .collect();
                assert_eq!(
                    address,
                    [crate::assistant::sandbox::profile::SESSION_BUS_OFF_ADDRESS]
                );
            }
        }
    }

    #[test]
    fn a_result_says_why_keyring_only_has_no_bus_and_how_to_get_it() {
        let output = SandboxCommandOutput {
            cwd: PathBuf::from("/w"),
            exit_code: Some(0),
            success: true,
            stdout: String::new(),
            stderr: String::new(),
        };
        let none = SocketPolicy::default();
        let refused = RefusedTargets::default();
        assert!(bash_exec_result(&output, &refused, &none, None)
            .get("sessionBus")
            .is_none());
        let reason = Some("xdg-dbus-proxy is not installed");
        let note = bash_exec_result(&output, &refused, &none, reason)["sessionBus"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(!note.contains('\n'), "one line: {note}");
        assert!(note.contains("xdg-dbus-proxy is not installed"), "{note}");
        assert!(note.contains("set its Session bus to Full"), "{note}");
        let error = bash_exec_outcome(
            Err("Command timed out after 2000 ms".to_string()),
            &refused,
            &none,
            reason,
            2_000,
            true,
        )
        .unwrap_err();
        assert!(error.ends_with(&note), "{error}");
    }

    /// A keyring proxy that cannot start leaves the command with no bus and
    /// says so; it neither fails the command nor binds the real bus.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn a_keyring_proxy_that_does_not_start_leaves_no_bus() {
        let workspace = tempdir().unwrap();
        let bus = workspace.path().join("host-bus");
        let mut profile = SandboxProfile::for_test(
            workspace.path().to_path_buf(),
            Vec::new(),
            SandboxNetworkMode::Disabled,
            SandboxSessionBusMode::Allow,
            SandboxEnv::filtered_from_iter(
                [
                    ("PATH", "/usr/bin:/bin"),
                    ("DBUS_SESSION_BUS_ADDRESS", "unix:path=/nonexistent/bus"),
                ],
                workspace.path(),
                SandboxSessionBusMode::Allow,
            ),
            None,
        );
        profile.session_bus_proxy = Some(SessionBusProxy {
            socket: PathBuf::from("/nonexistent/dir/bus-1.sock"),
            bus: bus.clone(),
        });
        let command = SandboxCommand {
            argv: vec![
                "/bin/sh".into(),
                "-c".into(),
                "echo ${DBUS_SESSION_BUS_ADDRESS-none}".into(),
            ],
            cwd: workspace.path().to_path_buf(),
            timeout_ms: 10_000,
            max_output_chars: 10_000,
            profile,
        };
        let network = std::sync::Arc::new(run_network(&[], &[]));
        let run = run_with_network(command, None, &network).await;
        assert!(
            run.session_bus_unavailable.is_some(),
            "the result says why there is no bus"
        );
        let output = match run.output {
            Ok(output) => output,
            Err(error) if error.contains("Sandboxed shell is unavailable") => {
                crate::assistant::sandbox::skip_without_bwrap(&error);
                return;
            }
            Err(error) => panic!("{error}"),
        };
        assert_eq!(
            output.stdout.trim(),
            crate::assistant::sandbox::profile::SESSION_BUS_OFF_ADDRESS,
            "{output:?}"
        );
    }

    /// The sockets a shell profile hands over for these saved and run-only
    /// grants.
    #[cfg(target_os = "linux")]
    fn sockets_for(
        workspace: &Path,
        saved: Vec<FilesystemPathGrant>,
        run_only: Vec<FilesystemPathGrant>,
    ) -> SocketPolicy {
        use crate::assistant::sandbox::{status_for, SandboxBackend};
        let mut context = context_with_grants(workspace, saved);
        let mut execution = context.run_context.execution.clone();
        execution.sandbox.network_access = network(NetworkMode::Off);
        context.run_context = std::sync::Arc::new(RunContext {
            execution,
            ..(*context.run_context).clone()
        });
        for grant in run_only {
            context.add_session_grant(grant);
        }
        let filesystem = filesystem_policy(&context).unwrap();
        sandbox_profile(
            &context,
            workspace.to_path_buf(),
            filesystem,
            &status_for(Some(SandboxBackend::Bubblewrap)),
        )
        .unwrap()
        .sockets
    }

    #[cfg(target_os = "linux")]
    fn grant_on(path: &Path, access: FilesystemPathAccess) -> FilesystemPathGrant {
        FilesystemPathGrant {
            path: path.display().to_string(),
            access,
            origin: None,
        }
    }

    /// A socket and the directory that holds it, outside the workspace.
    #[cfg(target_os = "linux")]
    fn listening_socket() -> (tempfile::TempDir, PathBuf, std::os::unix::net::UnixListener) {
        let dir = tempdir().unwrap();
        let socket = dir.path().canonicalize().unwrap().join("docker.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        (dir, socket, listener)
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn an_exact_grant_on_a_socket_lets_commands_connect_read_only_or_read_write() {
        let workspace = tempdir().unwrap();
        let (_dir, socket, _listener) = listening_socket();
        for access in [
            FilesystemPathAccess::ReadWrite,
            FilesystemPathAccess::ReadOnly,
        ] {
            let sockets = sockets_for(
                workspace.path(),
                vec![grant_on(&socket, access)],
                Vec::new(),
            );
            assert_eq!(
                sockets.allowed,
                [crate::assistant::sandbox::AllowedSocket {
                    path: socket.clone(),
                    host: socket.clone(),
                }],
                "{access:?}: {sockets:?}"
            );
            assert!(sockets.skipped.is_empty(), "{access:?}: {sockets:?}");
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_grant_on_a_socket_for_this_run_only_counts() {
        let workspace = tempdir().unwrap();
        let (_dir, socket, _listener) = listening_socket();
        let run_only = vec![grant_on(&socket, FilesystemPathAccess::ReadOnly)];
        let sockets = sockets_for(workspace.path(), Vec::new(), run_only);
        assert_eq!(sockets.allowed.len(), 1, "{sockets:?}");
        assert_eq!(sockets.allowed[0].host, socket);
    }

    /// The hint sends the agent to `fs_request_grant` for a socket; a folder
    /// grant over it must not answer that request without a card.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_socket_under_a_granted_folder_still_needs_its_own_grant() {
        let workspace = tempdir().unwrap();
        let (dir, socket, _listener) = listening_socket();
        let file = dir.path().canonicalize().unwrap().join("notes.txt");
        fs::write(&file, "").unwrap();
        let read = FilesystemPathAccess::ReadOnly;
        let context = context_with_grants(workspace.path(), vec![grant_on(dir.path(), read)]);
        let filesystem = filesystem_policy(&context).unwrap();
        assert!(already_granted(&filesystem, &file, read));
        assert!(!already_granted(&filesystem, &socket, read));

        context.add_session_grant(grant_on(&socket, read));
        let filesystem = filesystem_policy(&context).unwrap();
        assert!(already_granted(&filesystem, &socket, read));
        assert!(!already_granted(
            &filesystem,
            &socket,
            FilesystemPathAccess::ReadWrite
        ));
    }

    /// H2: a directory grant shows a socket in it, but does not let a
    /// command connect to it.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_socket_under_a_granted_directory_stays_unreachable() {
        let workspace = tempdir().unwrap();
        let (dir, _socket, _listener) = listening_socket();
        for access in [
            FilesystemPathAccess::ReadOnly,
            FilesystemPathAccess::ReadWrite,
        ] {
            let sockets = sockets_for(
                workspace.path(),
                vec![grant_on(dir.path(), access)],
                Vec::new(),
            );
            assert!(sockets.allowed.is_empty(), "{access:?}: {sockets:?}");
            assert!(sockets.skipped.is_empty(), "{access:?}: {sockets:?}");
        }
    }

    /// M2: in a directory the sandbox can write, a command could swap the
    /// socket for a link to another one, so its grant is refused and the
    /// result says why: under a read-write directory grant, or in the
    /// workspace.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_socket_grant_in_a_writable_directory_is_refused_and_the_result_says_why() {
        let workspace = tempdir().unwrap();
        let (dir, socket, _listener) = listening_socket();
        let in_workspace = workspace.path().canonicalize().unwrap().join("dev.sock");
        let _dev = std::os::unix::net::UnixListener::bind(&in_workspace).unwrap();
        let sockets = sockets_for(
            workspace.path(),
            vec![
                grant_on(dir.path(), FilesystemPathAccess::ReadWrite),
                grant_on(&socket, FilesystemPathAccess::ReadWrite),
            ],
            vec![grant_on(&in_workspace, FilesystemPathAccess::ReadOnly)],
        );
        assert!(sockets.allowed.is_empty(), "{sockets:?}");
        let output = SandboxCommandOutput {
            cwd: PathBuf::from("/w"),
            exit_code: Some(0),
            success: true,
            stdout: String::new(),
            stderr: String::new(),
        };
        let result = bash_exec_result(&output, &RefusedTargets::default(), &sockets, None);
        let skipped = result["socketGrantsSkipped"].as_array().unwrap();
        assert_eq!(skipped.len(), 2, "{skipped:?}");
        let dir = dir.path().canonicalize().unwrap();
        for (refusal, (path, writable)) in skipped.iter().zip([
            (&socket, dir.as_path()),
            (&in_workspace, in_workspace.parent().unwrap()),
        ]) {
            let refusal = refusal.as_str().unwrap();
            assert!(
                refusal.starts_with(&path.display().to_string()),
                "{refusal}"
            );
            assert!(
                refusal.contains(&format!("can write at {}", writable.display())),
                "{refusal}"
            );
        }
    }

    // ------------------------------------------------------------------
    // evaluate_command_policy — the ask-first list
    // ------------------------------------------------------------------

    use crate::config::types::ShellCapabilityConfig;
    use crate::config::{ExecutionCapabilityConfig, ShellAccessMode};

    fn config_in(mode: ShellAccessMode, rules: &[&str]) -> ExecutionCapabilityConfig {
        ExecutionCapabilityConfig {
            shell: ShellCapabilityConfig {
                mode,
                ask_first_command_prefixes: rules.iter().map(|s| s.to_string()).collect(),
            },
            ..Default::default()
        }
    }

    /// A Restricted (Ask first) agent whose ask-first list is `rules`.
    fn ask_first_config(rules: &[&str]) -> ExecutionCapabilityConfig {
        config_in(ShellAccessMode::Restricted, rules)
    }

    /// A Full agent with `rules` on its ask-first list, which Full never reads.
    fn full_execution_config(rules: &[&str]) -> ExecutionCapabilityConfig {
        config_in(ShellAccessMode::Full, rules)
    }

    /// The rows `command` asks about; empty when it runs without asking.
    fn asked(exec: &ExecutionCapabilityConfig, command: &str) -> Vec<AskFirstRow> {
        match evaluate_command_policy(exec, command, &[]) {
            PolicyResult::AskFirst(rows) => rows,
            PolicyResult::Allow => Vec::new(),
        }
    }

    fn asks_first(exec: &ExecutionCapabilityConfig, command: &str) -> bool {
        !asked(exec, command).is_empty()
    }

    #[test]
    fn ask_first_reads_every_command_of_the_input() {
        // The matcher's readings of each row, each run against a rule on the
        // ask-first list.
        let cases: &[(&str, &str)] = &[
            ("git push", "git push"),
            ("git push", "git push --force origin main"),
            ("git push", "cd x && git push"),
            ("git push", "git log | git push"),
            ("git push", "timeout 60 git push origin main"),
            ("git push", "git -C ../r push"),
            ("git push", "git -c x=y push"),
            ("git push", "git --no-pager -C repo push"),
            ("git push", "FOO=1 /usr/bin/git push"),
            ("git push", "printf x | xargs git push"),
            ("git push", "if true; then git push; fi"),
            ("git push", "echo $(git push)"),
            ("git push", "cat <<EOF\n$(git push)\nEOF"),
            ("git push", "git -c diff.external=/bin/echo push"),
            ("git push", "git -C repo -c e=v push"),
            ("git status", "git --git-dir=.git status --short"),
            ("git status", "git --git-dir .git status --short"),
            ("git status", "git status -C repo"),
            ("git status", "git --config-env core.fsmonitor=FSMON status"),
            ("git status", "timeout 600 git -C repo status --short"),
            ("git status", "FOO=bar git -C repo status --short"),
            ("git -c", "FOO=bar git -c diff.external=/bin/echo diff"),
            ("rm", "rm -rf /tmp"),
            ("rm", "timeout 5 rm x"),
            ("rm", "xargs rm"),
            ("rm", "find . | xargs rm -f"),
            ("rm", "find . -exec rm {} +"),
            ("rm", "find . -name x -execdir rm -f {} \\;"),
            ("rm", "find . -ok /bin/rm {} ';'"),
            ("git push", "find . -exec git -C r push \\;"),
            ("rm", "FOO=bar rm -rf /tmp/x"),
            ("rm", "/usr/bin/rm -rf x"),
            ("rm", "./rm x"),
            ("rm", "FOO=1 /bin/rm x"),
            ("rm", "ls; ../bin/rm x"),
            ("rm", "FOO='a b' /usr/bin/rm x"),
            ("rm", "rm\tx"),
            ("rm", "/bin/rm\t-rf x"),
            ("rm", "A=$'\\'' rm x"),
            ("rm", "FOO=$'a\\' b' /usr/bin/rm x/y"),
            ("rm", "A+=x A[1 + 1]=y rm x"),
            ("rm", "timeout 60 rm -rf x"),
            ("rm", "timeout --signal=KILL 60 rm -rf x"),
            ("rm", "nice -n 5 rm -rf x"),
            ("rm", "env FOO=1 rm -rf x"),
            ("timeout", "timeout 60 cargo build"),
            ("[", "[ -f /tmp/x ]"),
        ];
        for (rule, command) in cases {
            let rules = asked(&ask_first_config(&[rule]), command)
                .into_iter()
                .map(|row| row.rule)
                .collect::<Vec<_>>();
            assert!(
                rules.iter().any(|matched| matched == rule),
                "`{command}` must ask for `{rule}`, asked for {rules:?}"
            );
        }
    }

    #[test]
    fn ask_first_does_not_fire_for_what_is_not_the_command() {
        let exec = ask_first_config(&["git push", "rm"]);
        for command in [
            "echo git push",
            "git pushes",
            "git status",
            "cat <<'EOF'\ngit push\nEOF",
            "rmdir x",
            "bash -c \"git push\"",
            "gh api repos/o/r -X DELETE",
            "find . -name x",
            "find . -name rm -print",
            "find . -exec ls {} +",
        ] {
            assert!(asked(&exec, command).is_empty(), "`{command}` must run");
        }
    }

    #[test]
    fn two_matches_in_one_input_give_one_request_with_two_rows() {
        let exec = ask_first_config(&["git push", "rm"]);
        assert_eq!(
            asked(&exec, "rm -f x && cargo test && git push origin main"),
            vec![
                AskFirstRow {
                    text: "rm -f x".to_string(),
                    rule: "rm".to_string(),
                },
                AskFirstRow {
                    text: "git push origin main".to_string(),
                    rule: "git push".to_string(),
                },
            ]
        );
    }

    #[test]
    fn the_default_list_asks_in_restricted_and_never_in_full() {
        let mut restricted = ask_first_config(&[]);
        restricted.shell.ask_first_command_prefixes =
            crate::config::types::default_ask_first_command_prefixes();
        let mut full = restricted.clone();
        full.shell.mode = ShellAccessMode::Full;
        for command in [
            "git push",
            "cd x && git push",
            "timeout 5 rm x",
            "rm -rf x",
            "printf x | xargs rm",
            "sudo ls",
            "cargo publish",
            "gh api -X DELETE repos/o/r",
        ] {
            assert!(asks_first(&restricted, command), "`{command}` must ask");
            assert!(
                matches!(
                    evaluate_command_policy(&full, command, &[]),
                    PolicyResult::Allow
                ),
                "`{command}` must run without a card in Full"
            );
        }
        // A former Restricted agent runs everyday commands without a card.
        for command in ["cargo test", "ls -la", "git status", "touch x && cat x"] {
            assert!(!asks_first(&restricted, command), "`{command}` must run");
        }
    }

    #[test]
    fn the_default_list_asks_for_common_spellings_of_its_commands() {
        let mut exec = ask_first_config(&[]);
        exec.shell.ask_first_command_prefixes =
            crate::config::types::default_ask_first_command_prefixes();
        for (command, rule) in [
            ("kubectl -n ns delete pod x", "kubectl delete"),
            ("kubectl --context prod delete pod x", "kubectl delete"),
            ("kubectl --namespace=ns delete pod x", "kubectl delete"),
            (
                "kubectl --certificate-authority ca.crt delete pod x",
                "kubectl delete",
            ),
            ("kubectl --profile cpu delete pod x", "kubectl delete"),
            ("docker --context x push img", "docker push"),
            ("docker -H tcp://x push img", "docker push"),
            ("docker image push img", "docker image push"),
            ("docker --context x image push img", "docker image push"),
            ("npm --prefix x publish", "npm publish"),
            ("npm -w pkg publish", "npm publish"),
            ("npm --tag beta publish", "npm publish"),
            ("npm --otp 123456 publish", "npm publish"),
            ("npm --access public publish", "npm publish"),
            ("npm -L project publish", "npm publish"),
            ("npm --location project publish", "npm publish"),
            ("npm --scope @o --auth-type web publish", "npm publish"),
            ("cargo +nightly publish", "cargo publish"),
            ("cargo -Z unstable publish", "cargo publish"),
            ("pnpm --filter x publish", "pnpm publish"),
            ("pnpm -C dir publish", "pnpm publish"),
            ("yarn npm publish", "yarn npm publish"),
            ("gh -R o/r release create v1", "gh release"),
            ("gh --repo o/r release delete v1", "gh release"),
            ("gh api -X DELETE repos/o/r", "gh api -X DELETE"),
            ("gh api -XDELETE repos/o/r", "gh api -X DELETE"),
            ("gh api --method DELETE repos/o/r", "gh api -X DELETE"),
            ("gh api --method=DELETE repos/o/r", "gh api -X DELETE"),
            ("gh api -X=DELETE repos/o/r", "gh api -X DELETE"),
            ("timeout 5 gh api --method DELETE x", "gh api -X DELETE"),
            (
                "gh api repos/o/r/git/refs/heads/x -X DELETE",
                "gh api -X DELETE",
            ),
            (
                "env -S \"gh api --method DELETE repos/o/r\"",
                "gh api -X DELETE",
            ),
            (
                "find . -exec gh api --method DELETE {} \\;",
                "gh api -X DELETE",
            ),
        ] {
            let rules: Vec<String> = asked(&exec, command)
                .into_iter()
                .map(|row| row.rule)
                .collect();
            assert_eq!(rules, [rule], "`{command}`");
        }
        for command in [
            "kubectl -n ns get pods",
            "kubectl --context prod get pods",
            "docker --context x ps",
            "docker image ls",
            "npm --prefix x run build",
            "cargo +nightly build",
            "pnpm --filter x test",
            "yarn npm info x",
            "gh -R o/r pr view 1",
            "gh api repos/o/r",
            "gh api -X GET repos/o/r",
        ] {
            assert!(!asks_first(&exec, command), "`{command}` must run");
        }
    }

    #[test]
    fn a_row_lists_every_rule_it_matches() {
        let exec = ask_first_config(&["git push", "rm", " git ", "git push"]);
        let rules: Vec<String> = asked(&exec, "git push origin")
            .into_iter()
            .map(|row| row.rule)
            .collect();
        assert_eq!(rules, ["git push", "git"]);
        // With every listed rule removed, the row runs: none is left to ask.
        assert!(matches!(
            evaluate_command_policy(&exec, "git push origin", &rules),
            PolicyResult::Allow
        ));
        assert!(asks_first(&exec, "git status"), "`git` still asks");
    }

    #[test]
    fn always_allow_stops_the_rules_asking_for_the_rest_of_this_run_only() {
        let workspace = tempdir().unwrap();
        let run = context_with_grants(workspace.path(), Vec::new());
        let other_run = context_with_grants(workspace.path(), Vec::new());
        let exec = ask_first_config(&["git push", "git"]);
        let command = "git push origin main";
        let asks_in = |context: &ToolExecutionContext| {
            matches!(
                evaluate_command_policy(&exec, command, &context.run_removed_ask_first_snapshot()),
                PolicyResult::AskFirst(_)
            )
        };
        let PolicyResult::AskFirst(rows) = evaluate_command_policy(&exec, command, &[]) else {
            panic!("`{command}` must ask");
        };
        let rules = vec!["git push".to_string(), "git".to_string()];

        apply_ask_first_decision(&run, &rows, &rules, AskFirstDecision::AllowOnce)
            .expect("Allow once runs the command");
        assert!(asks_in(&run), "Allow once keeps the rules");

        apply_ask_first_decision(&run, &rows, &rules, AskFirstDecision::AlwaysAllow)
            .expect("Always allow runs the command");
        assert!(!asks_in(&run), "the same run asks no more");
        assert!(asks_in(&other_run), "another run keeps its own snapshot");

        let denied = apply_ask_first_decision(
            &other_run,
            &rows,
            &rules,
            AskFirstDecision::Deny {
                reason: Some("no".to_string()),
            },
        );
        assert_eq!(denied, Err(ask_first_denied_message(&rows, Some("no"))));
        assert!(other_run.had_warnings() && !run.had_warnings());
    }

    #[test]
    fn a_rule_removed_in_this_run_no_longer_asks() {
        let exec = ask_first_config(&["git push", "rm"]);
        let removed = vec!["git push".to_string()];
        assert!(matches!(
            evaluate_command_policy(&exec, "git push origin main", &removed),
            PolicyResult::Allow
        ));
        // Only the removed rule: another rule still asks.
        let PolicyResult::AskFirst(rows) =
            evaluate_command_policy(&exec, "git push && rm -f x", &removed)
        else {
            panic!("rm must still ask");
        };
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].rule, "rm");
    }

    #[test]
    fn an_empty_ask_first_list_asks_nothing() {
        let exec = ask_first_config(&[]);
        assert!(!asks_first(&exec, "git push && rm -rf x"));
    }

    #[test]
    fn denied_message_names_each_row_its_rule_and_the_reason() {
        let rows = vec![
            AskFirstRow {
                text: "rm -f x".to_string(),
                rule: "rm".to_string(),
            },
            AskFirstRow {
                text: "git push".to_string(),
                rule: "git push".to_string(),
            },
        ];
        assert_eq!(
            ask_first_denied_message(&rows, Some("  keep x  ")),
            "User denied `rm -f x` (ask-first rule `rm`), `git push` (ask-first rule `git push`). \
             Reason: keep x."
        );
        assert_eq!(
            ask_first_denied_message(&rows[..1], Some(" ")),
            "User denied `rm -f x` (ask-first rule `rm`)."
        );
        assert_eq!(
            ask_first_denied_message(&rows[..1], None),
            "User denied `rm -f x` (ask-first rule `rm`)."
        );
    }

    #[test]
    fn ask_first_sees_commands_inside_blocks() {
        // Each of these used to reach the matcher as one segment headed by
        // grammar (`f(){`, `{`, `(`, `for`, `cat <<EOF`), so `rm` ran.
        let exec = ask_first_config(&["rm"]);
        for command in [
            "f(){ rm -rf x; }; f",
            "f() { rm -rf x; }",
            "function f { rm -rf x; }",
            "{ rm -rf x; }",
            "(rm -rf x)",
            "for i in 1; do rm -rf x; done",
            "if true; then rm -rf x; fi",
            "case a in a) rm -rf x;; esac",
            "shopt -s extglob\ncase a in @(a|b)) rm -rf y;; esac",
            "echo $(case x in x) a;; esac); rm -rf x",
            "echo $(echo case in a); rm -rf x",
            "echo $(echo case); echo $(echo in a); rm -rf x",
            "! rm -rf x",
            "cat <<EOF | rm -rf x\nhi\nEOF",
            "cat <<\\EOF\nhi\nEOF\nrm -rf x",
            "rm(){ :; }; unset -f rm; rm -rf ~",
            "case x\nin x) rm -rf y;; esac",
        ] {
            assert!(
                matches!(
                    evaluate_command_policy(&exec, command, &[]),
                    PolicyResult::AskFirst(_)
                ),
                "{command:?} should ask for rm"
            );
        }
    }

    #[test]
    fn ask_first_reads_every_row_when_one_is_asked_by_exact_text() {
        // The value arithmetic evaluates, or the arguments a call passes to
        // `"$@"`, are written in another row.
        for command in [
            "i='a[$(rm -rf x)]'; cat <<EOF\n$((i))\nEOF",
            "i='a[$(rm -rf x)]'; for ((j=0;j<i;j++)); do :; done",
            "f(){ \"$@\"; }; f rm -rf x",
            "ls\nrm -rf x <<'E'\nno end",
        ] {
            for (exec, asks) in [
                (full_execution_config(&["rm"]), false),
                (ask_first_config(&["rm"]), true),
            ] {
                assert_eq!(asks_first(&exec, command), asks, "{command:?}");
            }
        }
    }

    #[test]
    fn ask_first_word_scan_reads_paths_and_entries_of_several_words() {
        for (rule, command) in [
            ("rm", "x='a[$(/bin/rm -rf y)]'; echo $((x))"),
            ("git push", "x='a[$(git push origin)]'; echo $((x))"),
        ] {
            for (exec, asks) in [
                (full_execution_config(&[rule]), false),
                (ask_first_config(&[rule]), true),
            ] {
                assert_eq!(asks_first(&exec, command), asks, "{rule:?} {command:?}");
            }
        }
    }

    #[test]
    fn heredoc_bash_ends_inside_a_substitution_hides_no_command() {
        // In `$(…)` and `<(…)` bash ends a heredoc at a line that starts
        // with its delimiter (`EOF)`) and runs `rm -rf x`.
        for command in [
            "echo $(cat <<EOF\nhi\nEOF)\nrm -rf x\n: <<ls\nEOF\n)\nls",
            "echo $(cat <<'EOF'\nhi\nEOF)\nrm -rf x\n: <<ls\nEOF\n)\nls",
            "echo $(cat <<-EOF\nhi\n\tEOF)\nrm -rf x\n: <<ls\nEOF\n)\nls",
            "cat <(cat <<EOF\nhi\nEOF)\nrm -rf x\n: <<ls\nEOF\n)\nls",
            "echo $(cat <<EOF\nhi\nEOFX)\nrm -rf x\n: <<ls\nEOF\n)\nls",
        ] {
            for (exec, asks) in [
                (full_execution_config(&["rm"]), false),
                (ask_first_config(&["rm"]), true),
            ] {
                assert_eq!(asks_first(&exec, command), asks, "{command:?}");
            }
        }
    }

    #[test]
    fn ask_first_reads_every_word_of_unparsed_input() {
        // bash runs the `touch PWN` in each; the splitter cannot find it, so
        // the rules are read against every word of the input. A `touch`
        // rule asks in Restricted, unless this run removed it; another rule
        // does not; Full never asks.
        let commands = [
            "echo $(case a in a) echo;& b) echo;; esac); touch PWN",
            "cat <<EOF|touch PWN\nhi\nEOF",
            "echo x\r# ; touch PWN",
            "echo $(ls >& case in a)\ntouch PWN\n;; esac)",
            "echo $(ls &> case in a)\ntouch PWN\n;; esac)",
            "echo $(ls &>> case in a)\ntouch PWN\n;; esac)",
            "echo $(ls >| case in a)\ntouch PWN\n;; esac)",
            "echo $(cat <& 0 case in a </dev/null)\ntouch PWN\n;; esac)",
            "echo \"$(ls >& case in a)\"\ntouch PWN\n;; esac)",
            "git --version ; for i in a; { touch PWN; }",
            "cat <<$'E'\nE\ntouch PWN",
            "cat <<$\"E\"\nE\ntouch PWN",
            "cat <<$\\\n'E'\nE\ntouch PWN",
            "cat <<$(x)\n$(x)\ntouch PWN\n$",
            "cat <<`a`\n`a`\ntouch PWN",
            "cat <<E\\\nX\nEX\\\n\ntouch PWN\nEX",
            "cat <<''\n'\n\ntouch PWN\n'",
            "cat <<\"\"\n'\n\ntouch PWN\n'",
            "cat <<'E\u{1}'\nE\u{1}\n'\nE\u{1}\u{1}\ntouch PWN\n'",
            "cat <<'E\u{7f}'\nE\u{7f}\n'\nE\u{1}\u{7f}\ntouch PWN\n'",
            "cat <<-\"\tE\"\n\tE\ntouch PWN",
            "cat <<-'\tE'\n\tE\ntouch PWN",
            "cat <<-\\\tE\n\tE\ntouch PWN",
            "cat <<\"E\\\\\"\nE\\\ntouch PWN\nE\\\\",
            "cat <<\"E\\\"F\"\nE\"F\ntouch PWN",
            "cat <<\"E\\$\"\nE$\ntouch PWN\nE\\$",
            "cat <<\"E\\`\"\nE`\ntouch PWN\nE\\`",
            "cat <<E\"\\\\\"\nE\\\ntouch PWN\nE\\\\",
            "cat <<-\"E\\\\\"\n\tE\\\ntouch PWN\nE\\\\",
            "[ x <<\"E\\\\\" ]\nE\\\ntouch PWN\nE\\\\",
            "echo x\n((echo a) ; touch PWN )",
            "((touch PWN) )",
            "echo $((echo a) ; touch PWN )",
        ];
        for (exec, removed, asks) in [
            (ask_first_config(&["touch"]), &[][..], true),
            (
                ask_first_config(&["touch"]),
                &["touch".to_string()][..],
                false,
            ),
            (ask_first_config(&["sudo"]), &[][..], false),
            (full_execution_config(&["touch"]), &[][..], false),
        ] {
            for command in commands {
                let result = evaluate_command_policy(&exec, command, removed);
                assert_eq!(
                    matches!(result, PolicyResult::AskFirst(_)),
                    asks,
                    "{:?} {removed:?} {command:?} -> {result:?}",
                    exec.shell.mode
                );
            }
        }
    }

    #[test]
    fn ask_first_sees_a_command_after_an_escaped_blank_and_hash() {
        let exec = ask_first_config(&["touch"]);
        assert_eq!(
            asked(&exec, "echo a\\ #; touch PWN"),
            vec![AskFirstRow {
                text: "touch PWN".to_string(),
                rule: "touch".to_string(),
            }]
        );
        let continued = "echo a\\ \\\n#; touch PWN";
        // The continued form is Unparsed: the rules are read against its words.
        assert!(asks_first(&exec, continued));
        assert!(!asks_first(&full_execution_config(&["touch"]), continued));
    }

    #[test]
    fn full_mode_with_the_default_list_never_asks() {
        // Owner rule: Full mode never raises a card and refuses nothing.
        let exec = ExecutionCapabilityConfig {
            shell: ShellCapabilityConfig {
                mode: ShellAccessMode::Full,
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(!exec.shell.ask_first_command_prefixes.is_empty());
        for command in [
            "cd /x && SCRATCH=a bash tools/check-links.sh 2>&1 | tail -30",
            "bash -c 'echo hi'",
            "echo \"unclosed",
            "rm -rf x",
            "timeout 5 rm x",
            "git push --force",
        ] {
            assert!(
                matches!(
                    evaluate_command_policy(&exec, command, &[]),
                    PolicyResult::Allow
                ),
                "{command} must run without a card in Full mode"
            );
        }
    }

    #[test]
    fn full_mode_heredoc_commands_run_without_a_prompt() {
        // Hotfix regression cases: heredoc-fed scripts must run in Full
        // mode without an approval prompt while the default ask-first list is
        // active. The quoted-delimiter form is Opaque, the unquoted one
        // also yields the command inside its `$(...)`, and the
        // write-then-run form is Unparsed — none of them may prompt.
        let exec = ExecutionCapabilityConfig {
            shell: ShellCapabilityConfig {
                mode: ShellAccessMode::Full,
                ..Default::default()
            },
            ..Default::default()
        };
        let commands = [
            "python3 - <<'EOF'\nimport re, pathlib\np = pathlib.Path(\"x.py\"); s = p.read_text()\nprint(f\"{len(s)} $HOME `x`\")\nEOF",
            "python3 - <<EOF\nimport os\nprint(\"x $(touch PWN)\")\nEOF",
            "cat > f.py <<'EOF'\nprint(1)\nEOF && python3 f.py",
        ];
        for command in commands {
            assert!(
                matches!(
                    evaluate_command_policy(&exec, command, &[]),
                    PolicyResult::Allow
                ),
                "{command} must run without a prompt in Full mode"
            );
        }
    }

    #[test]
    fn heredoc_forms_current_segment_kinds() {
        // Pins what the splitter currently makes of each heredoc form.
        // The Full-mode no-prompt rule must hold whichever kind appears.
        use crate::assistant::tools::command_splitter::{split_command, Segment};
        let kinds = |input: &str| -> Vec<&'static str> {
            split_command(input)
                .iter()
                .map(|segment| match segment {
                    Segment::Simple(_) => "Simple",
                    Segment::Opaque(_) => "Opaque",
                    Segment::Unparsed(_) => "Unparsed",
                    Segment::Exact(_) => "Exact",
                })
                .collect()
        };
        assert_eq!(
            kinds("python3 - <<'EOF'\nprint(\"$HOME `x`\")\nEOF"),
            ["Opaque"]
        );
        // An unquoted delimiter expands its body, so the splitter reports
        // the command inside the `$(...)` as a segment of its own — the
        // ask-first list still applies to it.
        assert_eq!(
            kinds("python3 - <<EOF\nprint(\"x $(touch PWN)\")\nEOF"),
            ["Opaque", "Simple"]
        );
        assert_eq!(
            kinds("cat > f.py <<'EOF'\nprint(1)\nEOF && python3 f.py"),
            ["Unparsed"]
        );
    }

    #[test]
    fn augment_timeout_error_passes_through_non_timeout() {
        let err = "Shell command failed: boom".to_string();
        assert_eq!(
            augment_timeout_error(err.clone(), DEFAULT_BASH_TIMEOUT_MS, false),
            err
        );
    }

    #[test]
    fn augment_timeout_error_points_at_timeoutms_below_cap() {
        let err = "Sandboxed shell command timed out after 300000 ms".to_string();
        let out = augment_timeout_error(err, DEFAULT_BASH_TIMEOUT_MS, false);
        assert!(out.contains("timed out after 300000 ms"));
        assert!(out.contains("default timeout"));
        assert!(out.contains("`timeoutMs`"));
        assert!(out.contains(&MAX_BASH_TIMEOUT_MS.to_string()));
    }

    #[test]
    fn augment_timeout_error_labels_requested_timeout() {
        let err = "Sandboxed shell command timed out after 600000 ms".to_string();
        let out = augment_timeout_error(err, 600_000, true);
        assert!(out.contains("requested timeout"));
    }

    #[test]
    fn augment_timeout_error_at_cap_advises_narrowing() {
        let err = "Sandboxed shell command timed out after 1800000 ms".to_string();
        let out = augment_timeout_error(err, MAX_BASH_TIMEOUT_MS, true);
        assert!(out.contains("maximum `timeoutMs`"));
        assert!(out.contains("narrow the command"));
    }

    // ------------------------------------------------------------------
    // Commands that run their arguments, heredoc bodies, lookups
    // ------------------------------------------------------------------

    #[test]
    fn ask_first_sees_the_command_an_argument_runner_runs() {
        for (exec, asks) in [
            (full_execution_config(&["rm"]), false),
            (ask_first_config(&["rm"]), true),
        ] {
            for command in [
                "find . | xargs rm -f",
                "find . | xargs -0 -I {} -n 1 rm -f {}",
                "exec rm -rf x",
                "exec -a name rm x",
                "eval rm -rf x",
                "eval -- timeout 5 rm x",
                "watch rm x",
                "watch -d -n 2 rm x",
                "nohup xargs -P 4 rm",
                "xargs eval rm",
                "xargs rm",
                "timeout 5 rm x",
            ] {
                assert_eq!(asks_first(&exec, command), asks, "{command} must ask first");
            }
        }
    }

    #[test]
    fn argument_runners_without_a_listed_command_run() {
        let exec = full_execution_config(&["rm"]);
        for command in ["exec >log 2>&1", "xargs grep foo", "watch -n 2 ls"] {
            assert!(
                matches!(
                    evaluate_command_policy(&exec, command, &[]),
                    PolicyResult::Allow
                ),
                "{command}"
            );
        }
        let exec = ask_first_config(&["rm"]);
        for command in ["exec >log 2>&1", "xargs grep foo", "watch -n 2 ls"] {
            assert!(!asks_first(&exec, command), "{command}");
        }
    }

    #[test]
    fn ask_first_sees_commands_an_unquoted_heredoc_body_runs() {
        for (exec, asks) in [
            (full_execution_config(&["rm"]), false),
            (ask_first_config(&["rm"]), true),
        ] {
            for command in [
                "cat <<EOF\n${x:-$(rm -rf ~)}\nEOF",
                "cat <<EOF\n`rm -rf ~`\nEOF",
                "cat <<EOF\nx\n`rm b` $(echo c)\nEOF",
                "cat <<EOF\n$(echo `rm a`)\nEOF",
                "cat <<EOF\n`echo \\`rm a\\``\nEOF",
            ] {
                assert_eq!(
                    asks_first(&exec, command),
                    asks,
                    "{command:?} must ask first"
                );
            }
        }
    }

    #[test]
    fn quoted_heredoc_body_runs_nothing_ask_first_sees() {
        let command = "cat <<'EOF'\n`rm x` $(rm y)\nEOF";
        assert!(!asks_first(&full_execution_config(&["rm"]), command));
        assert!(!asks_first(&ask_first_config(&["rm"]), command));
    }

    #[test]
    fn command_lookup_is_not_the_command_it_names() {
        for (exec, asks) in [
            (full_execution_config(&["rm"]), false),
            (ask_first_config(&["rm"]), true),
        ] {
            for command in ["command -v rm", "command -V rm", "command -pv rm"] {
                assert!(!asks_first(&exec, command), "{command} only looks rm up");
            }
            for command in ["command rm x", "command -p rm x"] {
                assert_eq!(asks_first(&exec, command), asks, "{command} runs rm");
            }
        }
    }

    #[test]
    fn runner_options_are_read_as_getopt_reads_them() {
        // `xargs -0I grep echo RAN` runs `echo` with `grep` as the replace
        // string, `exec -la grep echo RAN` runs `echo` named `grep`, and
        // `xargs --process grep` abbreviates `--process-slot-var grep`.
        for (exec, asks) in [
            (full_execution_config(&["rm"]), false),
            (ask_first_config(&["rm"]), true),
        ] {
            for command in [
                "xargs -0n 1 rm -rf x",
                "xargs -0I grep rm -rf x",
                "xargs -rI grep rm -rf x",
                "xargs -J % rm -rf x",
                "xargs --process grep rm -rf x",
                "xargs -in rm -rf x",
                "exec -la grep rm -rf x",
                "env -iu FOO rm -rf x",
                "env - rm -rf x",
                "timeout -sKILL 5 rm -rf x",
                "timeout --pres 5 rm -rf x",
            ] {
                assert_eq!(asks_first(&exec, command), asks, "{command} must ask first");
            }
        }
    }

    #[test]
    fn ask_first_sees_a_command_under_every_reading_of_runner_options() {
        for (exec, asks) in [
            (full_execution_config(&["rm"]), false),
            (ask_first_config(&["rm"]), true),
        ] {
            for command in [
                "xargs --max-lines rm -rf x",
                "xargs --max-l rm -rf x",
                "xargs -0I rm echo x",
                "xargs -I rm rm x",
                "xargs -Irm rm",
                "xargs --replace rm",
                "xargs --replace=rm rm x",
                "xargs --frob rm echo x",
                "xargs --frob x rm -rf y",
                "exec xargs -0I rm echo x",
                "env -S'rm -rf' x",
                "env -Srm -rf x",
                "env -S 'rm -rf' x",
                "env \"-Srm -rf\" x",
                "env --split-string='rm -rf' x",
                "env -i -S'timeout 5 rm -rf' x",
                "env -S'\\_rm -rf' x",
                "env -S'env -S\\_rm' x",
                "env -S'r''m -rf' x",
                "taskset 1 rm -rf x",
                "taskset 0x3 rm -rf x",
                "taskset -c 0,1 rm x",
                "chrt 10 rm -rf x",
                "chrt -f 10 rm -rf x",
                "chrt -o rm x",
                "chrt 10 -- rm x",
            ] {
                assert_eq!(asks_first(&exec, command), asks, "{command} must ask first");
            }
        }
        // Only a word an option may take is read as the command.
        let exec = ask_first_config(&["rm"]);
        for command in [
            "xargs -0 git rm x",
            "xargs -n 1 git rm",
            "xargs grep rm",
            "env -u FOO git rm",
            "timeout 5 git rm",
            "chrt -o echo rm",
            "taskset 1 git rm x",
        ] {
            assert!(
                matches!(
                    evaluate_command_policy(&exec, command, &[]),
                    PolicyResult::Allow
                ),
                "{command} runs no rm"
            );
        }
    }

    #[test]
    fn ask_first_sees_a_listed_subcommand_under_every_reading() {
        for (exec, asks) in [
            (full_execution_config(&["git push"]), false),
            (ask_first_config(&["git push"]), true),
        ] {
            for command in ["xargs -0I git push origin", "env -S'\\_git push'"] {
                assert_eq!(asks_first(&exec, command), asks, "{command} must ask first");
            }
        }
    }

    #[test]
    fn many_replace_strings_take_linear_time() {
        let command = format!("xargs {}git {}", "-I zq ".repeat(5_000), "w ".repeat(5_000));
        let started = std::time::Instant::now();
        for exec in [full_execution_config(&["rm"]), ask_first_config(&["rm"])] {
            evaluate_command_policy(&exec, &command, &[]);
        }
        assert!(started.elapsed() < Duration::from_secs(30));
    }

    #[test]
    fn deeply_nested_runners_do_not_overflow_the_stack() {
        let handle = std::thread::Builder::new()
            .stack_size(2 << 20)
            .spawn(|| {
                for runner in ["exec ", "nohup "] {
                    let command = format!("{}grep x", runner.repeat(20_000));
                    assert!(
                        !asks_first(&ask_first_config(&["rm"]), &command),
                        "{runner}"
                    );
                }
                let command = format!("{}rm x", "exec ".repeat(20_000));
                assert!(asks_first(&ask_first_config(&["rm"]), &command));
            })
            .expect("spawn");
        handle.join().expect("nested runners must not overflow");
    }
}
