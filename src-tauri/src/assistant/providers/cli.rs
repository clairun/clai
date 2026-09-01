use std::path::Path;
use std::pin::Pin;
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use async_trait::async_trait;
use futures::{stream, Stream};
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::assistant::providers::types::{ProviderAdapter, ProviderError};
use crate::assistant::types::{
    AuthMode, CompletionRequest, ContentPart, MessageRole, ModelInfo, ProtocolFamily,
    ProviderConnection, ProviderDescriptor, ProviderEvent, ProviderInputMessage,
};

pub const CLAUDE_CODE_PROVIDER_ID: &str = "claude-code";
pub const CODEX_PROVIDER_ID: &str = "codex";
pub const OPENCODE_PROVIDER_ID: &str = "opencode";

const CLI_SUMMARY_TIMEOUT: Duration = Duration::from_secs(180);

pub fn provider_descriptors() -> Vec<ProviderDescriptor> {
    vec![
        ProviderDescriptor {
            id: CLAUDE_CODE_PROVIDER_ID.to_string(),
            display_name: "Claude Code".to_string(),
            protocol_family: ProtocolFamily::Custom,
            supported_auth_modes: vec![AuthMode::SubscriptionLogin],
            configurable_base_url: true,
            is_cli_backed: true,
        },
        ProviderDescriptor {
            id: CODEX_PROVIDER_ID.to_string(),
            display_name: "Codex CLI".to_string(),
            protocol_family: ProtocolFamily::Custom,
            supported_auth_modes: vec![AuthMode::SubscriptionLogin],
            configurable_base_url: true,
            is_cli_backed: true,
        },
        ProviderDescriptor {
            id: OPENCODE_PROVIDER_ID.to_string(),
            display_name: "OpenCode".to_string(),
            protocol_family: ProtocolFamily::Custom,
            supported_auth_modes: vec![AuthMode::SubscriptionLogin],
            configurable_base_url: true,
            is_cli_backed: true,
        },
    ]
}

pub fn is_cli_provider(provider_id: &str) -> bool {
    matches!(
        provider_id,
        CLAUDE_CODE_PROVIDER_ID | CODEX_PROVIDER_ID | OPENCODE_PROVIDER_ID
    )
}

pub fn command_for_provider(provider_id: &str) -> Option<&'static str> {
    match provider_id {
        CLAUDE_CODE_PROVIDER_ID => Some("claude"),
        CODEX_PROVIDER_ID => Some("codex"),
        OPENCODE_PROVIDER_ID => Some("opencode"),
        _ => None,
    }
}

#[derive(Clone, Copy)]
enum CliRuntime {
    ClaudeCode,
    Codex,
    OpenCode,
}

impl CliRuntime {
    fn for_provider_id(provider_id: &str) -> Option<Self> {
        match provider_id {
            CLAUDE_CODE_PROVIDER_ID => Some(Self::ClaudeCode),
            CODEX_PROVIDER_ID => Some(Self::Codex),
            OPENCODE_PROVIDER_ID => Some(Self::OpenCode),
            _ => None,
        }
    }

    fn provider_id(self) -> &'static str {
        match self {
            Self::ClaudeCode => CLAUDE_CODE_PROVIDER_ID,
            Self::Codex => CODEX_PROVIDER_ID,
            Self::OpenCode => OPENCODE_PROVIDER_ID,
        }
    }

    fn display_name(self) -> &'static str {
        match self {
            Self::ClaudeCode => "Claude Code",
            Self::Codex => "Codex CLI",
            Self::OpenCode => "OpenCode",
        }
    }

    fn default_command(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude",
            Self::Codex => "codex",
            Self::OpenCode => "opencode",
        }
    }
}

pub struct CliAdapter {
    runtime: CliRuntime,
}

impl CliAdapter {
    pub fn new(provider_id: &str) -> Option<Self> {
        Some(Self {
            runtime: CliRuntime::for_provider_id(provider_id)?,
        })
    }
}

#[async_trait]
impl ProviderAdapter for CliAdapter {
    fn provider_id(&self) -> &'static str {
        self.runtime.provider_id()
    }

    fn protocol_family(&self) -> ProtocolFamily {
        ProtocolFamily::Custom
    }

    async fn list_models(
        &self,
        _connection: &ProviderConnection,
    ) -> Result<Vec<ModelInfo>, ProviderError> {
        models_for_provider(self.provider_id()).ok_or(ProviderError::NotConfigured)
    }

    async fn stream_completion(
        &self,
        _connection: &ProviderConnection,
        _request: CompletionRequest,
    ) -> Result<
        Pin<Box<dyn Stream<Item = Result<ProviderEvent, ProviderError>> + Send>>,
        ProviderError,
    > {
        Err(ProviderError::NotImplemented)
    }

    async fn stream_sessionless_completion(
        &self,
        connection: &ProviderConnection,
        request: CompletionRequest,
        working_dir: Option<&Path>,
    ) -> Result<
        Pin<Box<dyn Stream<Item = Result<ProviderEvent, ProviderError>> + Send>>,
        ProviderError,
    > {
        if !request.tools.is_empty() {
            return Err(ProviderError::NotImplemented);
        }
        if !request.images.is_empty() {
            return Err(ProviderError::NotImplemented);
        }

        let (system_prompt, prompt) = sessionless_prompt_parts(&request);
        let text = match self.runtime {
            CliRuntime::ClaudeCode => {
                run_claude_summary(connection, working_dir, &system_prompt, &prompt).await?
            }
            CliRuntime::Codex => {
                run_codex_summary(connection, working_dir, &system_prompt, &prompt).await?
            }
            CliRuntime::OpenCode => {
                run_opencode_summary(connection, working_dir, &system_prompt, &prompt).await?
            }
        };

        let events = vec![
            Ok(ProviderEvent::TextDelta { text }),
            Ok(ProviderEvent::MessageComplete),
        ];
        Ok(Box::pin(stream::iter(events)))
    }
}

async fn run_claude_summary(
    connection: &ProviderConnection,
    working_dir: Option<&Path>,
    system_prompt: &str,
    prompt: &str,
) -> Result<String, ProviderError> {
    let mut command = cli_command(
        connection,
        CliRuntime::ClaudeCode,
        &[("ENABLE_TOOL_SEARCH", "false")],
        working_dir,
    );
    command
        .arg("-p")
        .arg("--safe-mode")
        .arg("--output-format")
        .arg("stream-json")
        .arg("--include-partial-messages")
        .arg("--verbose")
        .arg("--tools")
        .arg("")
        .arg("--permission-mode")
        .arg("bypassPermissions")
        .arg("--no-session-persistence")
        .arg("--disable-slash-commands");
    if !system_prompt.trim().is_empty() {
        command.arg("--system-prompt").arg(system_prompt);
    }
    let model = connection.model_id.trim();
    if !model.is_empty() && model != "default" {
        command.arg("--model").arg(model);
    }

    let output = run_cli_json_command(CliRuntime::ClaudeCode, command, prompt).await?;
    parse_claude_summary(&output)
}

async fn run_codex_summary(
    connection: &ProviderConnection,
    working_dir: Option<&Path>,
    system_prompt: &str,
    prompt: &str,
) -> Result<String, ProviderError> {
    let mut command = cli_command(connection, CliRuntime::Codex, &[], working_dir);
    command
        .arg("exec")
        .arg("--json")
        .arg("--ephemeral")
        .arg("--skip-git-repo-check")
        .arg("--ignore-user-config")
        .arg("--ignore-rules")
        .arg("--disable")
        .arg("shell_tool")
        .arg("-c")
        .arg(format!(
            "developer_instructions={}",
            toml_string_value(system_prompt)
        ));
    let model = connection.model_id.trim();
    if !model.is_empty() && model != "default" {
        command.arg("--model").arg(model);
    }
    command.arg("-");

    let output = run_cli_json_command(CliRuntime::Codex, command, prompt).await?;
    parse_codex_summary(&output)
}

async fn run_opencode_summary(
    connection: &ProviderConnection,
    working_dir: Option<&Path>,
    system_prompt: &str,
    prompt: &str,
) -> Result<String, ProviderError> {
    let config = opencode_summary_config_content();
    let mut command = cli_command(
        connection,
        CliRuntime::OpenCode,
        &[
            ("OPENCODE_CONFIG_CONTENT", config.as_str()),
            ("OPENCODE_DISABLE_AUTOUPDATE", "true"),
            ("OPENCODE_DISABLE_PRUNE", "true"),
            ("OPENCODE_DISABLE_CLAUDE_CODE", "true"),
            ("OPENCODE_DISABLE_CLAUDE_CODE_PROMPT", "true"),
            ("OPENCODE_DISABLE_CLAUDE_CODE_SKILLS", "true"),
            ("OPENCODE_DISABLE_DEFAULT_PLUGINS", "true"),
            ("OPENCODE_DISABLE_LSP_DOWNLOAD", "true"),
        ],
        working_dir,
    );
    command.arg("--pure").arg("run").arg("--format").arg("json");
    let model = connection.model_id.trim();
    if !model.is_empty() && model != "default" {
        command.arg("--model").arg(model);
    }
    if let Some(root) = working_dir {
        command.arg("--dir").arg(root);
    }

    let prompt = if system_prompt.trim().is_empty() {
        prompt.to_string()
    } else {
        format!("System instructions:\n{system_prompt}\n\nUser request:\n{prompt}")
    };
    let output = run_cli_json_command(CliRuntime::OpenCode, command, &prompt).await?;
    parse_opencode_summary(&output)
}

fn cli_command(
    connection: &ProviderConnection,
    runtime: CliRuntime,
    envs: &[(&str, &str)],
    working_dir: Option<&Path>,
) -> tokio::process::Command {
    let configured_binary = connection
        .base_url
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| runtime.default_command());
    let binary = crate::providers::resolve_command_path(configured_binary)
        .unwrap_or_else(|| configured_binary.to_string());
    crate::providers::build_host_cli_command(&binary, envs, working_dir)
}

async fn run_cli_json_command(
    runtime: CliRuntime,
    command: tokio::process::Command,
    prompt: &str,
) -> Result<String, ProviderError> {
    run_cli_json_command_with_timeout(runtime, command, prompt, CLI_SUMMARY_TIMEOUT).await
}

/// Feed `prompt` to a CLI summarizer on stdin and collect its stdout.
///
/// The prompt reaches `SUMMARY_TRANSCRIPT_MAX_CHARS` (~96 KB), far more than a
/// pipe buffer holds, so the order here is load-bearing: both output pipes are
/// drained concurrently with the write, and the whole exchange — write, wait,
/// and reads — sits under one timeout. Writing first and reading afterwards
/// deadlocks against any child that writes more than a pipe buffer before it
/// has consumed the prompt, and a timeout around `wait()` alone cannot break it
/// because the hang happens before `wait()` is ever reached.
async fn run_cli_json_command_with_timeout(
    runtime: CliRuntime,
    mut command: tokio::process::Command,
    prompt: &str,
    timeout: Duration,
) -> Result<String, ProviderError> {
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().map_err(|e| {
        ProviderError::RequestFailed(format!(
            "Failed to launch {} summarizer: {}",
            runtime.display_name(),
            e
        ))
    })?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| ProviderError::RequestFailed("CLI stdout was not captured".to_string()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| ProviderError::RequestFailed("CLI stderr was not captured".to_string()))?;
    let stdout_task = tokio::spawn(read_pipe(stdout));
    let stderr_task = tokio::spawn(read_pipe(stderr));

    let stdin = child.stdin.take();
    let owned_prompt = prompt.to_string();
    let write_task = tokio::spawn(async move {
        let Some(mut stdin) = stdin else {
            return Ok(());
        };
        stdin.write_all(owned_prompt.as_bytes()).await?;
        stdin.shutdown().await
    });

    let write_abort = write_task.abort_handle();
    let exchange = async {
        // A write error is usually the child having exited early, in which case
        // its status and stderr say why; keep it and only report it if the child
        // then claims success on input it never received.
        let write_error = match write_task.await {
            Ok(result) => result.err(),
            Err(join_error) => Some(std::io::Error::other(join_error)),
        };
        let status = child.wait().await;
        (write_error, status)
    };

    let exchange = tokio::time::timeout(timeout, exchange).await;
    let (write_error, status) = match exchange {
        Ok(outcome) => outcome,
        Err(_) => {
            let _ = child.kill().await;
            write_abort.abort();
            stdout_task.abort();
            stderr_task.abort();
            return Err(ProviderError::RequestFailed(format!(
                "{} summarizer timed out after {}s",
                runtime.display_name(),
                timeout.as_secs()
            )));
        }
    };
    let status = status.map_err(|e| ProviderError::RequestFailed(e.to_string()))?;

    let stdout = stdout_task
        .await
        .map_err(|e| ProviderError::RequestFailed(e.to_string()))?
        .map_err(|e| ProviderError::RequestFailed(e.to_string()))?;
    let stderr = stderr_task
        .await
        .map_err(|e| ProviderError::RequestFailed(e.to_string()))?
        .map_err(|e| ProviderError::RequestFailed(e.to_string()))?;

    if !status.success() {
        return Err(command_failed(runtime, status, &stdout, &stderr));
    }
    if let Some(error) = write_error {
        return Err(ProviderError::RequestFailed(format!(
            "Failed to write prompt: {}",
            error
        )));
    }

    Ok(String::from_utf8_lossy(&stdout).into_owned())
}

async fn read_pipe<R>(mut pipe: R) -> Result<Vec<u8>, std::io::Error>
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    let mut buffer = Vec::new();
    pipe.read_to_end(&mut buffer).await?;
    Ok(buffer)
}

fn command_failed(
    runtime: CliRuntime,
    status: ExitStatus,
    stdout: &[u8],
    stderr: &[u8],
) -> ProviderError {
    let stderr = String::from_utf8_lossy(stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(stdout).trim().to_string();
    let detail = if !stderr.is_empty() {
        stderr
    } else if !stdout.is_empty() {
        stdout
    } else {
        "no output".to_string()
    };
    ProviderError::RequestFailed(format!(
        "{} summarizer exited with status {}: {}",
        runtime.display_name(),
        status,
        detail
    ))
}

fn sessionless_prompt_parts(request: &CompletionRequest) -> (String, String) {
    let mut system = Vec::new();
    let mut prompt = Vec::new();
    for message in &request.messages {
        let text = input_message_text(message);
        if text.trim().is_empty() {
            continue;
        }
        if matches!(&message.role, MessageRole::System) {
            system.push(text);
        } else {
            prompt.push(format!("{} message:\n{}", role_label(&message.role), text));
        }
    }
    if let Some(max_output_tokens) = request.max_output_tokens {
        prompt.push(format!(
            "Write the answer within roughly {max_output_tokens} output tokens."
        ));
    }
    (system.join("\n\n"), prompt.join("\n\n"))
}

fn input_message_text(message: &ProviderInputMessage) -> String {
    message
        .content
        .iter()
        .filter_map(|part| match part {
            ContentPart::Text { text } => Some(text.clone()),
            ContentPart::Thinking { .. } => None,
            ContentPart::ToolUse {
                tool_name,
                arguments,
                ..
            } => Some(format!(
                "[tool call: {} {}]",
                tool_name,
                serde_json::to_string(arguments).unwrap_or_else(|_| arguments.to_string())
            )),
            ContentPart::ToolResult { payload, .. } => Some(format!(
                "[tool result: {}]",
                serde_json::to_string(payload).unwrap_or_else(|_| payload.to_string())
            )),
            ContentPart::Image { .. } => Some("[image omitted]".to_string()),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn role_label(role: &MessageRole) -> &'static str {
    match role {
        MessageRole::System => "system",
        MessageRole::User => "user",
        MessageRole::Assistant => "assistant",
        MessageRole::Tool => "tool",
    }
}

fn parse_claude_summary(stdout: &str) -> Result<String, ProviderError> {
    let mut text = String::new();
    let mut final_result = None;
    let mut error = None;
    for value in json_lines(stdout)? {
        match value.get("type").and_then(Value::as_str) {
            Some("stream_event") => {
                let event = value.get("event").unwrap_or(&Value::Null);
                if event.get("type").and_then(Value::as_str) == Some("content_block_delta") {
                    let delta = event.get("delta").unwrap_or(&Value::Null);
                    if delta.get("type").and_then(Value::as_str) == Some("text_delta") {
                        if let Some(delta_text) = delta.get("text").and_then(Value::as_str) {
                            text.push_str(delta_text);
                        }
                    }
                }
            }
            Some("result") => {
                if value
                    .get("is_error")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                {
                    error = Some(
                        value
                            .get("errors")
                            .and_then(Value::as_array)
                            .and_then(|arr| arr.first())
                            .and_then(Value::as_str)
                            .or_else(|| value.get("result").and_then(Value::as_str))
                            .or_else(|| value.get("error").and_then(Value::as_str))
                            .unwrap_or("Claude Code summarizer failed")
                            .to_string(),
                    );
                } else if let Some(result) = value.get("result").and_then(Value::as_str) {
                    final_result = Some(result.to_string());
                }
            }
            Some("error") => {
                error = Some(
                    value
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("Claude Code summarizer failed")
                        .to_string(),
                );
            }
            _ => {}
        }
    }
    parsed_text_or_error("Claude Code", text, final_result, error)
}

fn parse_codex_summary(stdout: &str) -> Result<String, ProviderError> {
    let mut text = String::new();
    let mut error = None;
    for value in json_lines(stdout)? {
        match value.get("type").and_then(Value::as_str) {
            Some("item.completed") => {
                if let Some(item) = value.get("item") {
                    if item.get("type").and_then(Value::as_str) == Some("agent_message") {
                        if let Some(item_text) = item.get("text").and_then(Value::as_str) {
                            if !text.is_empty() {
                                text.push_str("\n\n");
                            }
                            text.push_str(item_text);
                        }
                    }
                }
            }
            Some("turn.failed") => {
                error = Some(
                    value
                        .get("error")
                        .and_then(|error| error.get("message"))
                        .and_then(Value::as_str)
                        .unwrap_or("Codex summarizer failed")
                        .to_string(),
                );
            }
            Some("error") => {
                error = Some(
                    value
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("Codex summarizer failed")
                        .to_string(),
                );
            }
            _ => {}
        }
    }
    parsed_text_or_error("Codex", text, None, error)
}

fn parse_opencode_summary(stdout: &str) -> Result<String, ProviderError> {
    let mut text = String::new();
    let mut error = None;
    for value in json_lines(stdout)? {
        match value.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(part_text) = value
                    .get("part")
                    .and_then(|part| part.get("text"))
                    .and_then(Value::as_str)
                {
                    text.push_str(part_text);
                }
            }
            Some("error") => {
                error = Some(
                    value
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("OpenCode summarizer failed")
                        .to_string(),
                );
            }
            _ => {}
        }
    }
    parsed_text_or_error("OpenCode", text, None, error)
}

fn json_lines(stdout: &str) -> Result<Vec<Value>, ProviderError> {
    let mut values = Vec::new();
    for line in stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        values.push(
            serde_json::from_str(line).map_err(|e| {
                ProviderError::RequestFailed(format!("Invalid CLI JSON event: {}", e))
            })?,
        );
    }
    Ok(values)
}

fn parsed_text_or_error(
    provider: &str,
    text: String,
    final_result: Option<String>,
    error: Option<String>,
) -> Result<String, ProviderError> {
    if let Some(error) = error {
        return Err(ProviderError::RequestFailed(error));
    }

    let summary = if text.trim().is_empty() {
        final_result.unwrap_or_default()
    } else {
        text
    };
    if !summary.trim().is_empty() {
        return Ok(summary.trim().to_string());
    }
    Err(ProviderError::RequestFailed(error.unwrap_or_else(|| {
        format!("{provider} summarizer produced no text")
    })))
}

fn toml_string_value(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_string())
}

fn opencode_summary_config_content() -> String {
    serde_json::json!({
        "$schema": "https://opencode.ai/config.json",
        "autoupdate": false,
        "share": "disabled",
        "instructions": [],
        "plugin": [],
        "lsp": false,
        "formatter": false,
        "tools": {
            "bash": false,
            "edit": false,
            "write": false,
            "read": false,
            "grep": false,
            "glob": false,
            "lsp": false,
            "apply_patch": false,
            "skill": false,
            "todowrite": false
        }
    })
    .to_string()
}

pub fn models_for_provider(provider_id: &str) -> Option<Vec<ModelInfo>> {
    let models = match provider_id {
        CLAUDE_CODE_PROVIDER_ID => vec![
            ModelInfo {
                id: "sonnet".to_string(),
                display_name: "Sonnet".to_string(),
                supports_tools: true,
                supports_images: true,
            },
            ModelInfo {
                id: "opus".to_string(),
                display_name: "Opus".to_string(),
                supports_tools: true,
                supports_images: true,
            },
            ModelInfo {
                id: "haiku".to_string(),
                display_name: "Haiku".to_string(),
                supports_tools: true,
                supports_images: true,
            },
        ],
        CODEX_PROVIDER_ID => vec![
            ModelInfo {
                id: "gpt-5.5".to_string(),
                display_name: "GPT-5.5".to_string(),
                supports_tools: true,
                supports_images: true,
            },
            ModelInfo {
                id: "gpt-5.4".to_string(),
                display_name: "GPT-5.4".to_string(),
                supports_tools: true,
                supports_images: true,
            },
            ModelInfo {
                id: "gpt-5.4-mini".to_string(),
                display_name: "GPT-5.4 Mini".to_string(),
                supports_tools: true,
                supports_images: true,
            },
            ModelInfo {
                id: "gpt-5.3-codex".to_string(),
                display_name: "GPT-5.3 Codex".to_string(),
                supports_tools: true,
                supports_images: true,
            },
        ],
        OPENCODE_PROVIDER_ID => vec![ModelInfo {
            id: "default".to_string(),
            display_name: "Default".to_string(),
            supports_tools: true,
            // OpenCode fronts arbitrary models via Models.dev; the "default"
            // id can't tell us if the active model is vision-capable, so gate
            // images OFF until per-model capability is resolvable.
            supports_images: false,
        }],
        _ => return None,
    };
    Some(models)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cli_connection(provider_id: &str) -> ProviderConnection {
        ProviderConnection {
            id: "connection".to_string(),
            name: "CLI".to_string(),
            protocol_id: provider_id.to_string(),
            provider_id: provider_id.to_string(),
            auth_mode: AuthMode::SubscriptionLogin,
            base_url: None,
            secret_ref: String::new(),
            model_id: "default".to_string(),
            account_label: None,
            enabled: true,
            created_at: 0,
            updated_at: 0,
        }
    }

    fn summary_request() -> CompletionRequest {
        CompletionRequest {
            run_id: "r".to_string(),
            session_id: "s".to_string(),
            model_id: "default".to_string(),
            messages: vec![ProviderInputMessage {
                role: MessageRole::User,
                content: vec![ContentPart::Text {
                    text: "summarize this".to_string(),
                }],
            }],
            tools: Vec::new(),
            temperature: None,
            max_output_tokens: Some(128),
            images: Default::default(),
        }
    }

    #[test]
    fn image_capability_gated_per_provider() {
        // Codex CLI ingests images via `codex exec --image <FILE>`; Claude Code
        // accepts image content blocks — both report vision-capable.
        for provider in [CODEX_PROVIDER_ID, CLAUDE_CODE_PROVIDER_ID] {
            let models = models_for_provider(provider).unwrap();
            assert!(!models.is_empty());
            assert!(
                models.iter().all(|m| m.supports_images),
                "{provider} models should support images"
            );
        }
        // OpenCode fronts arbitrary models; gated off until per-model vision
        // capability is resolvable.
        let opencode = models_for_provider(OPENCODE_PROVIDER_ID).unwrap();
        assert!(opencode.iter().all(|m| !m.supports_images));
    }

    #[test]
    fn sessionless_prompt_splits_system_from_user_messages() {
        let request = CompletionRequest {
            run_id: "r".to_string(),
            session_id: "s".to_string(),
            model_id: "default".to_string(),
            messages: vec![
                ProviderInputMessage {
                    role: MessageRole::System,
                    content: vec![ContentPart::Text {
                        text: "summarize compactly".to_string(),
                    }],
                },
                ProviderInputMessage {
                    role: MessageRole::User,
                    content: vec![ContentPart::Text {
                        text: "transcript".to_string(),
                    }],
                },
            ],
            tools: Vec::new(),
            temperature: None,
            max_output_tokens: Some(128),
            images: Default::default(),
        };

        let (system, prompt) = sessionless_prompt_parts(&request);

        assert_eq!(system, "summarize compactly");
        assert!(prompt.contains("user message:\ntranscript"));
        assert!(prompt.contains("128 output tokens"));
        assert!(!prompt.contains("system message"));
    }

    #[tokio::test]
    async fn sessionless_cli_summary_rejects_tool_requests() {
        let adapter = CliAdapter::new(CODEX_PROVIDER_ID).expect("adapter");
        let connection = cli_connection(CODEX_PROVIDER_ID);
        let mut request = summary_request();
        request.tools.push(crate::assistant::types::ToolDefinition {
            name: "probe".to_string(),
            description: "not allowed in summarizer".to_string(),
            input_schema: serde_json::json!({ "type": "object" }),
        });

        let result = adapter
            .stream_sessionless_completion(&connection, request, None)
            .await;

        assert!(matches!(result, Err(ProviderError::NotImplemented)));
    }

    #[tokio::test]
    async fn sessionless_cli_summary_rejects_image_requests() {
        let adapter = CliAdapter::new(CLAUDE_CODE_PROVIDER_ID).expect("adapter");
        let connection = cli_connection(CLAUDE_CODE_PROVIDER_ID);
        let mut request = summary_request();
        request.images.insert(
            "img".to_string(),
            crate::assistant::types::ResolvedImage {
                media_type: "image/png".to_string(),
                data_base64: "AA==".to_string(),
            },
        );

        let result = adapter
            .stream_sessionless_completion(&connection, request, None)
            .await;

        assert!(matches!(result, Err(ProviderError::NotImplemented)));
    }

    #[test]
    fn parse_claude_summary_prefers_streamed_text_over_final_result() {
        let stdout = r#"
{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"hello "}}}
{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"world"}}}
{"type":"result","is_error":false,"result":"duplicate final"}
"#;

        assert_eq!(parse_claude_summary(stdout).unwrap(), "hello world");
    }

    #[test]
    fn parse_codex_summary_reads_completed_agent_message() {
        let stdout = r#"
{"type":"item.started","item":{"type":"agent_message","text":"partial"}}
{"type":"item.completed","item":{"type":"agent_message","text":"final summary"}}
"#;

        assert_eq!(parse_codex_summary(stdout).unwrap(), "final summary");
    }

    #[test]
    fn parse_opencode_summary_reads_text_parts() {
        let stdout = r#"
{"type":"text","part":{"text":"final "}}
{"type":"text","part":{"text":"summary"}}
"#;

        assert_eq!(parse_opencode_summary(stdout).unwrap(), "final summary");
    }

    #[test]
    fn parse_claude_summary_errors_even_with_partial_text() {
        let stdout = r#"
{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"partial"}}}
{"type":"result","is_error":true,"error":"context limit"}
"#;

        let err = parse_claude_summary(stdout).unwrap_err().to_string();

        assert!(err.contains("context limit"), "got: {err}");
    }

    #[test]
    fn parse_codex_summary_errors_even_after_agent_message() {
        let stdout = r#"
{"type":"item.completed","item":{"type":"agent_message","text":"partial summary"}}
{"type":"turn.failed","error":{"message":"input too large"}}
"#;

        let err = parse_codex_summary(stdout).unwrap_err().to_string();

        assert!(err.contains("input too large"), "got: {err}");
    }

    #[test]
    fn parse_opencode_summary_errors_even_after_text() {
        let stdout = r#"
{"type":"text","part":{"text":"partial summary"}}
{"type":"error","message":"provider failed"}
"#;

        let err = parse_opencode_summary(stdout).unwrap_err().to_string();

        assert!(err.contains("provider failed"), "got: {err}");
    }

    /// The prompt is bigger than any pipe buffer, which is the whole point:
    /// these three tests fail (by timing out) if the write is not concurrent
    /// with the reads, or if the timeout does not cover the write.
    #[cfg(unix)]
    fn oversized_prompt() -> String {
        "x".repeat(4 * 1024 * 1024)
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cli_summary_survives_a_prompt_larger_than_the_pipe_buffer() {
        // `cat` echoes stdin to stdout, so it stops reading once its own output
        // pipe fills — the exact shape that deadlocks a write-then-read driver.
        let prompt = oversized_prompt();
        let output = tokio::time::timeout(
            Duration::from_secs(30),
            run_cli_json_command(
                CliRuntime::Codex,
                tokio::process::Command::new("cat"),
                &prompt,
            ),
        )
        .await
        .expect("writing and draining must be concurrent, not sequential")
        .expect("cat should succeed");

        assert_eq!(output.len(), prompt.len());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cli_summary_times_out_while_the_prompt_is_still_being_written() {
        // A child that never reads stdin blocks our write forever; the timeout
        // has to cover the write, not just `wait()`.
        let mut command = tokio::process::Command::new("sh");
        command.arg("-c").arg("sleep 60");

        let result = tokio::time::timeout(
            Duration::from_secs(30),
            run_cli_json_command_with_timeout(
                CliRuntime::Codex,
                command,
                &oversized_prompt(),
                Duration::from_millis(250),
            ),
        )
        .await
        .expect("the timeout must fire even though the write never finished");

        match result {
            Err(ProviderError::RequestFailed(message)) => {
                assert!(
                    message.contains("timed out"),
                    "unexpected message: {message}"
                );
            }
            other => panic!("expected a timeout error, got {other:?}"),
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cli_summary_reports_the_child_failure_not_the_broken_pipe() {
        // Exiting before reading the prompt breaks our write; the useful
        // diagnosis is the child's own stderr, not "Failed to write prompt".
        let mut command = tokio::process::Command::new("sh");
        command.arg("-c").arg("echo 'boom' >&2; exit 3");

        let result = tokio::time::timeout(
            Duration::from_secs(30),
            run_cli_json_command(CliRuntime::Codex, command, &oversized_prompt()),
        )
        .await
        .expect("an early exit must not hang the write");

        match result {
            Err(ProviderError::RequestFailed(message)) => {
                assert!(message.contains("boom"), "unexpected message: {message}");
                assert!(
                    message.contains("exited with status"),
                    "unexpected message: {message}"
                );
            }
            other => panic!("expected the child failure, got {other:?}"),
        }
    }
}
