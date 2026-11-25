use std::pin::Pin;

use async_trait::async_trait;
use futures::stream::Stream;
use futures::StreamExt;
use reqwest::Client;
use serde_json::json;

type Bytes = axum::body::Bytes;

use crate::assistant::auth::secrets::ProviderSecretStorage;
use std::collections::HashMap;

use crate::assistant::types::{
    AuthMode, CompletionRequest, ContentPart, MessageRole, ModelInfo, ProtocolFamily,
    ProviderConnection, ProviderDescriptor, ProviderEvent, ResolvedImage, ToolInvocationDraft,
};

use super::catalog;
use super::failure::{classify_stream_finish, read_failure};
use super::types::{ProviderAdapter, ProviderError};
use super::ModelsSource;

pub const OPENAI_PROVIDER_ID: &str = "openai";
const DEFAULT_API_ROOT: &str = "https://api.openai.com/v1";

pub fn provider_descriptor() -> ProviderDescriptor {
    ProviderDescriptor {
        id: OPENAI_PROVIDER_ID.to_string(),
        display_name: "OpenAI-Compatible".to_string(),
        protocol_family: ProtocolFamily::OpenAiCompatible,
        supported_auth_modes: vec![AuthMode::DeveloperApiKey],
        configurable_base_url: true,
        is_cli_backed: false,
    }
}

pub struct OpenAiAdapter;

#[async_trait]
impl ProviderAdapter for OpenAiAdapter {
    fn provider_id(&self) -> &'static str {
        OPENAI_PROVIDER_ID
    }

    fn protocol_family(&self) -> ProtocolFamily {
        ProtocolFamily::OpenAiCompatible
    }

    async fn list_models(
        &self,
        connection: &ProviderConnection,
    ) -> Result<Vec<ModelInfo>, ProviderError> {
        let api_key = get_api_key_opt(connection);
        let models_url = match super::models_source(connection, endpoint_url) {
            ModelsSource::Curated(models) => return Ok(models),
            ModelsSource::OpenAiCompatible { url, capabilities } => {
                return super::fetch_models_openai_compatible(
                    &catalog::error_code_brand(connection),
                    &url,
                    api_key.as_deref(),
                    capabilities.as_ref(),
                )
                .await;
            }
            ModelsSource::Endpoint(url) => url,
        };

        tracing::info!(
            url = %models_url,
            protocol_id = %connection.protocol_id,
            "Fetching models from provider"
        );

        let client = Client::new();
        let mut builder = client.get(&models_url);
        if let Some(key) = &api_key {
            builder = builder.header("Authorization", format!("Bearer {}", key));
        }
        for (name, value) in catalog_extra_headers(connection) {
            builder = builder.header(name, value);
        }
        let resp = builder.send().await.map_err(|e| {
            tracing::error!(url = %models_url, error = %e, "HTTP request failed");
            ProviderError::RequestFailed(e.to_string())
        })?;

        let brand = catalog::error_code_brand(connection);
        if !resp.status().is_success() {
            return Err(ProviderError::Failed(read_failure(&brand, resp).await));
        }

        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| ProviderError::RequestFailed(e.to_string()))?;

        let models = body["data"]
            .as_array()
            .unwrap_or(&vec![])
            .iter()
            .filter_map(|m| {
                let id = m["id"].as_str()?;
                // Gemini's native model names are `models/{model}`; its chat
                // endpoint documents the bare `{model}`.
                let id = if brand == catalog::GEMINI_ID {
                    id.strip_prefix("models/").unwrap_or(id)
                } else {
                    id
                }
                .to_string();
                Some(ModelInfo {
                    id: id.clone(),
                    display_name: id,
                    supports_tools: true,
                    // Best-effort, provider-level (mirrors Anthropic): the
                    // common OpenAI chat models are vision-capable. A non-vision
                    // model errors at send time, which the engine surfaces
                    // non-destructively (the user's message + image are kept).
                    supports_images: true,
                })
            })
            .collect();

        Ok(models)
    }

    async fn stream_completion(
        &self,
        connection: &ProviderConnection,
        request: CompletionRequest,
    ) -> Result<
        Pin<Box<dyn Stream<Item = Result<ProviderEvent, ProviderError>> + Send>>,
        ProviderError,
    > {
        let api_key = get_api_key(connection)?;
        let url = endpoint_url(connection, "chat/completions");
        let brand = catalog::error_code_brand(connection);
        let body = build_request_body(&request, brand == catalog::GEMINI_ID);

        let client = Client::new();
        let mut builder = client
            .post(url)
            .header("Content-Type", "application/json")
            .json(&body);
        if let Some(key) = &api_key {
            builder = builder.header("Authorization", format!("Bearer {}", key));
        }
        for (name, value) in catalog_extra_headers(connection) {
            builder = builder.header(name, value);
        }
        let resp = builder
            .send()
            .await
            .map_err(|e| ProviderError::RequestFailed(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(ProviderError::Failed(read_failure(&brand, resp).await));
        }

        let byte_stream = resp.bytes_stream();
        let event_stream = sse_to_provider_events(byte_stream, brand);

        Ok(Box::pin(event_stream))
    }
}

/// Per-provider extra headers declared on the catalog entry.
fn catalog_extra_headers(connection: &ProviderConnection) -> Vec<(String, String)> {
    catalog::get_entry(&connection.provider_id)
        .map(|e| e.extra_headers)
        .unwrap_or_default()
}

/// Model discovery may use public endpoints before a key is entered.
fn get_api_key_opt(connection: &ProviderConnection) -> Option<String> {
    ProviderSecretStorage::get_secret(&connection.secret_ref)
        .ok()
        .flatten()
        .map(|key| key.trim().to_string())
        .filter(|key| !key.is_empty())
}

pub(super) fn get_api_key(
    connection: &ProviderConnection,
) -> Result<Option<String>, ProviderError> {
    let key = match ProviderSecretStorage::get_secret(&connection.secret_ref) {
        Ok(key) => key,
        Err(_) if !requires_api_key(connection) => None,
        Err(error) => {
            return Err(ProviderError::RequestFailed(format!(
                "Failed to read API key: {error}"
            )))
        }
    };
    validate_api_key(connection, key)
}

fn validate_api_key(
    connection: &ProviderConnection,
    key: Option<String>,
) -> Result<Option<String>, ProviderError> {
    let key = key.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    if key.is_none() && requires_api_key(connection) {
        return Err(ProviderError::NotConfigured);
    }
    Ok(key)
}

fn requires_api_key(connection: &ProviderConnection) -> bool {
    catalog::requires_api_key(&connection.provider_id)
}

/// URL of `endpoint` (e.g. `chat/completions`) under the connection's API root.
pub(super) fn endpoint_url(connection: &ProviderConnection, endpoint: &str) -> String {
    format!("{}/{endpoint}", api_root(connection))
}

/// The versioned API root, which the OpenAI SDKs call `base_url`
/// (`https://api.openai.com/v1`, `https://api.z.ai/api/paas/v4`).
///
/// A base whose last segment is a version, or a catalog base flagged as the
/// root as written, is used as written. Any other base is read as a server
/// root serving the API under `/v1`, which is how CLAI has always read stored
/// custom URLs such as `http://localhost:11434`.
fn api_root(connection: &ProviderConnection) -> String {
    let catalog_base = catalog::get_entry(&connection.provider_id).and_then(|e| e.default_base_url);
    let base = connection
        .base_url
        .as_deref()
        .or(catalog_base.as_deref())
        .unwrap_or(DEFAULT_API_ROOT)
        .trim_end_matches('/');
    if ends_with_version_segment(base) || catalog::api_root_entry(base).is_some() {
        base.to_string()
    } else {
        format!("{base}/v1")
    }
}

/// `v`, a digit, then only alphanumerics: `v1`, `v4`, `v1beta`.
fn ends_with_version_segment(base: &str) -> bool {
    base.rsplit('/')
        .next()
        .and_then(|segment| segment.strip_prefix('v'))
        .is_some_and(|rest| {
            rest.starts_with(|c: char| c.is_ascii_digit())
                && rest.chars().all(|c| c.is_ascii_alphanumeric())
        })
}

// =============================================================================
// Request Building
// =============================================================================

/// `replay_thought_signatures` sends stored Gemini thought signatures back;
/// for every other brand the body is what it was before they existed.
fn build_request_body(
    request: &CompletionRequest,
    replay_thought_signatures: bool,
) -> serde_json::Value {
    let messages: Vec<serde_json::Value> = request
        .messages
        .iter()
        .map(|m| build_message(m, &request.images, replay_thought_signatures))
        .collect();

    let mut body = json!({
        "model": request.model_id,
        "messages": messages,
        "stream": true,
    });

    // Add tools if present
    if !request.tools.is_empty() {
        let tools: Vec<serde_json::Value> = request
            .tools
            .iter()
            .map(|t| {
                json!({
                    "type": "function",
                    "function": {
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.input_schema,
                    }
                })
            })
            .collect();
        body["tools"] = json!(tools);
    }

    if let Some(temp) = request.temperature {
        body["temperature"] = json!(temp);
    }

    if let Some(max_tokens) = request.max_output_tokens {
        body["max_completion_tokens"] = json!(max_tokens);
    }

    body
}

/// Build a single OpenAI message from a ProviderInputMessage.
/// Handles text, tool_use (assistant with tool_calls), and tool_result (role: tool).
#[expect(
    clippy::too_many_lines,
    reason = "lint debt: 111 lines against a 100-line budget; split it, do not raise the budget"
)]
fn build_message(
    msg: &crate::assistant::types::ProviderInputMessage,
    images: &HashMap<String, ResolvedImage>,
    replay_thought_signatures: bool,
) -> serde_json::Value {
    let role = match msg.role {
        MessageRole::System => "system",
        MessageRole::User => "user",
        MessageRole::Assistant => "assistant",
        MessageRole::Tool => "tool",
    };

    // Check if this message contains tool uses (assistant calling tools)
    let tool_uses: Vec<&ContentPart> = msg
        .content
        .iter()
        .filter(|p| matches!(p, ContentPart::ToolUse { .. }))
        .collect();

    // Check if this is a tool result message
    if let Some(ContentPart::ToolResult {
        tool_call_id,
        payload,
        started_at,
        completed_at,
    }) = msg.content.first()
    {
        if msg.role == MessageRole::Tool {
            let content = serde_json::json!({
                "startedAt": started_at.and_then(format_timestamp_millis_as_rfc3339),
                "completedAt": completed_at.and_then(format_timestamp_millis_as_rfc3339),
                "payload": payload,
            });
            return json!({
                "role": "tool",
                "tool_call_id": tool_call_id,
                "content": content.to_string(),
            });
        }
    }

    // If assistant message has tool calls, format with tool_calls array
    if msg.role == MessageRole::Assistant && !tool_uses.is_empty() {
        let text_content: String = msg
            .content
            .iter()
            .filter_map(|p| match p {
                ContentPart::Text { text } => Some(text.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("");

        let reasoning_content: String = msg
            .content
            .iter()
            .filter_map(|p| match p {
                ContentPart::Thinking { text, .. } => Some(text.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("");

        let tool_calls: Vec<serde_json::Value> = tool_uses
            .iter()
            .map(|part| tool_call_json(part, replay_thought_signatures))
            .collect();

        // Always include `reasoning_content` (empty string when we
        // didn't capture any). LiteLLM/OpenAI o1/kimi reject the
        // message entirely if the field is *missing* from an
        // assistant-with-tool_calls message while thinking is enabled.
        // Sending an empty string keeps the schema check happy for
        // legacy messages we ingested before we started parsing
        // reasoning_content.
        let mut message = json!({
            "role": "assistant",
            "tool_calls": tool_calls,
            "reasoning_content": reasoning_content,
        });
        if !text_content.is_empty() {
            message["content"] = json!(text_content);
        }
        return message;
    }

    // User message carrying resolved images: send a multimodal content array
    // (text parts + `image_url` data-URL blocks). An image whose bytes weren't
    // resolved (missing file) is skipped; the surrounding text is still sent.
    if msg.role == MessageRole::User {
        let image_blocks: Vec<serde_json::Value> = msg
            .content
            .iter()
            .filter_map(|p| match p {
                ContentPart::Image { id, .. } => images.get(id).map(|img| {
                    json!({
                        "type": "image_url",
                        "image_url": {
                            "url": format!("data:{};base64,{}", img.media_type, img.data_base64),
                        }
                    })
                }),
                _ => None,
            })
            .collect();

        if !image_blocks.is_empty() {
            let mut parts: Vec<serde_json::Value> = Vec::new();
            let text: String = msg
                .content
                .iter()
                .filter_map(|p| match p {
                    ContentPart::Text { text } if !text.is_empty() => Some(text.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("");
            if !text.is_empty() {
                parts.push(json!({ "type": "text", "text": text }));
            }
            parts.extend(image_blocks);
            return json!({ "role": "user", "content": parts });
        }
    }

    // Default: text content
    let content = msg
        .content
        .iter()
        .filter_map(|part| match part {
            ContentPart::Text { text } => Some(text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("");

    json!({
        "role": role,
        "content": content,
    })
}

/// A `tool_calls[]` entry. Gemini's thought signature goes back on the call
/// that carried it, in `extra_content.google.thought_signature`.
fn tool_call_json(part: &ContentPart, replay_thought_signatures: bool) -> serde_json::Value {
    let ContentPart::ToolUse {
        tool_call_id,
        tool_name,
        arguments,
        thought_signature,
    } = part
    else {
        return json!(null);
    };
    let mut call = json!({
        "id": tool_call_id,
        "type": "function",
        "function": {
            "name": tool_name,
            "arguments": arguments.to_string(),
        }
    });
    if let Some(signature) = thought_signature
        .as_ref()
        .filter(|_| replay_thought_signatures)
    {
        call["extra_content"] = json!({ "google": { "thought_signature": signature } });
    }
    call
}

fn format_timestamp_millis_as_rfc3339(timestamp_ms: i64) -> Option<String> {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(timestamp_ms).map(|dt| dt.to_rfc3339())
}

// =============================================================================
// SSE Stream Parsing
// =============================================================================

/// State for accumulating partial tool calls across SSE frames.
#[derive(Default, Clone)]
struct PartialToolCall {
    id: String,
    name: String,
    arguments: String,
    thought_signature: Option<String>,
}

impl PartialToolCall {
    /// Applies one `delta.tool_calls[]` fragment.
    fn apply_delta(&mut self, delta: &serde_json::Value) {
        // Only a non-empty id/name/signature (re)binds: some
        // OpenAI-compatible streams repeat `id` (and `name`) as an empty
        // string on argument fragments, which must not erase the value the
        // first fragment set.
        let non_empty = |pointer: &str| {
            delta
                .pointer(pointer)
                .and_then(|value| value.as_str())
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        };
        if let Some(id) = non_empty("/id") {
            self.id = id;
        }
        if let Some(name) = non_empty("/function/name") {
            self.name = name;
        }
        // Gemini's signature, where its OpenAI-compatible layer puts it on
        // a tool call.
        if let Some(signature) = non_empty("/extra_content/google/thought_signature") {
            self.thought_signature = Some(signature);
        }
        // Accumulate function arguments (streamed as partial JSON)
        if let Some(args) = delta
            .pointer("/function/arguments")
            .and_then(|a| a.as_str())
        {
            self.arguments.push_str(args);
        }
    }
}

/// Full state carried through the SSE stream unfold.
struct SseState {
    stream: Pin<Box<dyn Stream<Item = reqwest::Result<Bytes>> + Send>>,
    buf: Vec<u8>,
    is_first: bool,
    tool_calls: Vec<PartialToolCall>,
    /// Classifies errors reported inside the stream (`error_code_brand`).
    brand: String,
}

fn sse_to_provider_events(
    byte_stream: impl Stream<Item = reqwest::Result<Bytes>> + Send + 'static,
    brand: String,
) -> impl Stream<Item = Result<ProviderEvent, ProviderError>> + Send {
    let state = SseState {
        stream: byte_stream.boxed(),
        buf: Vec::new(),
        is_first: true,
        tool_calls: Vec::new(),
        brand,
    };

    futures::stream::unfold(state, |mut state| async move {
        loop {
            // Check if we have a complete SSE frame in the buffer
            if let Some((pos, delim_len)) = find_sse_frame_delimiter(&state.buf) {
                let frame_bytes = state.buf[..pos].to_vec();
                state.buf.drain(..pos + delim_len);

                let frame = match String::from_utf8(frame_bytes) {
                    Ok(frame) => frame,
                    Err(e) => {
                        return Some((
                            vec![Err(ProviderError::RequestFailed(format!(
                                "Invalid UTF-8 in SSE frame: {}",
                                e
                            )))],
                            state,
                        ));
                    }
                };

                let events = parse_sse_frame(
                    &frame,
                    &state.brand,
                    &mut state.is_first,
                    &mut state.tool_calls,
                );
                if !events.is_empty() {
                    return Some((events, state));
                }
                continue;
            }

            // Need more data
            match state.stream.next().await {
                Some(Ok(bytes)) => state.buf.extend_from_slice(bytes.as_ref()),
                Some(Err(e)) => {
                    return Some((
                        vec![Err(ProviderError::RequestFailed(e.to_string()))],
                        state,
                    ));
                }
                None => {
                    // Stream ended. Parse any remaining buffer.
                    if !state.buf.is_empty() {
                        let remaining = std::mem::take(&mut state.buf);
                        let text = match String::from_utf8(remaining) {
                            Ok(text) => text,
                            Err(e) => {
                                return Some((
                                    vec![Err(ProviderError::RequestFailed(format!(
                                        "Invalid UTF-8 in trailing SSE buffer: {}",
                                        e
                                    )))],
                                    state,
                                ));
                            }
                        };
                        if !text.trim().is_empty() {
                            let events = parse_sse_frame(
                                text.trim(),
                                &state.brand,
                                &mut state.is_first,
                                &mut state.tool_calls,
                            );
                            if !events.is_empty() {
                                return Some((events, state));
                            }
                        }
                    }
                    return None;
                }
            }
        }
    })
    .flat_map(futures::stream::iter)
}

fn find_sse_frame_delimiter(buf: &[u8]) -> Option<(usize, usize)> {
    buf.windows(2)
        .position(|window| window == b"\n\n")
        .map(|pos| (pos, 2))
        .or_else(|| {
            buf.windows(4)
                .position(|window| window == b"\r\n\r\n")
                .map(|pos| (pos, 4))
        })
}

/// Parse a single SSE frame into ProviderEvent values.
fn parse_sse_frame(
    frame: &str,
    brand: &str,
    is_first: &mut bool,
    tool_calls_buf: &mut Vec<PartialToolCall>,
) -> Vec<Result<ProviderEvent, ProviderError>> {
    let mut events = Vec::new();

    for line in frame.lines() {
        let data = if let Some(stripped) = line.strip_prefix("data: ") {
            stripped.trim()
        } else if let Some(stripped) = line.strip_prefix("data:") {
            stripped.trim()
        } else {
            continue;
        };

        if data.is_empty() {
            continue;
        }

        if data == "[DONE]" {
            // Flush any accumulated tool calls before completing
            flush_tool_calls(tool_calls_buf, &mut events);
            events.push(Ok(ProviderEvent::MessageComplete));
            continue;
        }

        match serde_json::from_str::<serde_json::Value>(data) {
            Ok(json) => {
                if *is_first {
                    *is_first = false;
                    events.push(Ok(ProviderEvent::MessageStart));
                }

                let choice = json.get("choices").and_then(|c| c.get(0));

                let delta = choice.and_then(|c| c.get("delta"));

                // Extract text delta
                if let Some(content) = delta
                    .and_then(|d| d.get("content"))
                    .and_then(|c| c.as_str())
                {
                    if !content.is_empty() {
                        events.push(Ok(ProviderEvent::TextDelta {
                            text: content.to_string(),
                        }));
                    }
                }

                // Extract reasoning_content delta (o1 / kimi / "thinking"-
                // enabled models). The provider streams partial thinking
                // text in `choices[0].delta.reasoning_content`. We capture
                // it so the engine can attach a `ContentPart::Thinking` to
                // the assistant message — and so we can echo it back on
                // the next turn, which strict providers (LiteLLM-fronted
                // OpenAI) require.
                if let Some(reasoning) = delta
                    .and_then(|d| d.get("reasoning_content"))
                    .and_then(|c| c.as_str())
                {
                    if !reasoning.is_empty() {
                        events.push(Ok(ProviderEvent::ThinkingDelta {
                            text: reasoning.to_string(),
                        }));
                    }
                }

                // Extract tool_calls deltas
                if let Some(tool_calls) = delta
                    .and_then(|d| d.get("tool_calls"))
                    .and_then(|t| t.as_array())
                {
                    for tc in tool_calls {
                        let index = tc.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;

                        // Ensure buffer is large enough
                        while tool_calls_buf.len() <= index {
                            tool_calls_buf.push(PartialToolCall::default());
                        }
                        tool_calls_buf[index].apply_delta(tc);
                    }
                }

                // The terminal chunk may carry its tool-call fragment in
                // the same delta as the finish reason, so apply the delta
                // first.
                let finish_reason = choice
                    .and_then(|c| c.get("finish_reason"))
                    .and_then(|r| r.as_str());
                push_finish(brand, &json, finish_reason, tool_calls_buf, &mut events);
            }
            Err(e) => {
                tracing::warn!("Failed to parse SSE JSON: {} (data: {})", e, data);
            }
        }
    }

    events
}

/// A failure the chunk reports, or else its finish reason (`length` = output
/// budget exhausted) so consumers can reject truncated output, after the
/// tool calls a `tool_calls` finish announces.
fn push_finish(
    brand: &str,
    chunk: &serde_json::Value,
    finish_reason: Option<&str>,
    tool_calls_buf: &mut Vec<PartialToolCall>,
    events: &mut Vec<Result<ProviderEvent, ProviderError>>,
) {
    if let Some(failure) = classify_stream_finish(brand, finish_reason, chunk) {
        events.push(Ok(ProviderEvent::ProviderError { failure }));
        return;
    }
    let Some(reason) = finish_reason else {
        return;
    };
    if reason == "tool_calls" {
        flush_announced_tool_calls(tool_calls_buf, events);
    }
    events.push(Ok(ProviderEvent::StopReason {
        reason: reason.to_string(),
    }));
}

/// Flush on the terminal `finish_reason:"tool_calls"` frame. When the
/// provider announced tool calls but none survived parsing, fail the
/// turn: completing silently ends the run green with neither visible
/// text nor tool calls.
fn flush_announced_tool_calls(
    tool_calls_buf: &mut Vec<PartialToolCall>,
    events: &mut Vec<Result<ProviderEvent, ProviderError>>,
) {
    let before = events.len();
    flush_tool_calls(tool_calls_buf, events);
    if events.len() == before {
        events.push(Err(ProviderError::RequestFailed(
            "provider announced finish_reason=tool_calls but no tool call could be parsed"
                .to_string(),
        )));
    }
}

/// Flush accumulated tool calls as ToolCallReady events.
fn flush_tool_calls(
    tool_calls_buf: &mut Vec<PartialToolCall>,
    events: &mut Vec<Result<ProviderEvent, ProviderError>>,
) {
    for tc in tool_calls_buf.drain(..) {
        if tc.id.is_empty() {
            continue;
        }
        let params = super::parse_tool_arguments(&tc.name, &tc.arguments);

        events.push(Ok(ProviderEvent::ToolCallReady {
            tool_call: ToolInvocationDraft {
                tool_call_id: tc.id,
                tool_name: tc.name,
                params,
                thought_signature: tc.thought_signature,
            },
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assistant::types::{ContentPart, MessageRole, ProviderInputMessage};
    use futures::StreamExt;

    #[test]
    fn build_message_includes_tool_result_timestamps() {
        let msg = ProviderInputMessage {
            role: MessageRole::Tool,
            content: vec![ContentPart::ToolResult {
                tool_call_id: "call_123".to_string(),
                payload: serde_json::json!({ "openIssues": 0 }),
                started_at: Some(1_700_000_000_000),
                completed_at: Some(1_700_000_000_250),
            }],
        };

        let built = build_message(&msg, &HashMap::new(), false);
        assert_eq!(built["role"], "tool");
        assert_eq!(built["tool_call_id"], "call_123");

        let content = built["content"]
            .as_str()
            .expect("tool content should be a string");
        let parsed: serde_json::Value =
            serde_json::from_str(content).expect("tool content should be valid json");

        assert_eq!(parsed["startedAt"], "2023-11-14T22:13:20+00:00");
        assert_eq!(parsed["completedAt"], "2023-11-14T22:13:20.250+00:00");
        assert_eq!(parsed["payload"]["openIssues"], 0);
    }

    #[test]
    fn build_message_user_emits_image_url_block() {
        let msg = ProviderInputMessage {
            role: MessageRole::User,
            content: vec![
                ContentPart::Text {
                    text: "look at this".into(),
                },
                ContentPart::Image {
                    id: "img-1".into(),
                    path: ".clai/images/img-1.png".into(),
                    media_type: "image/png".into(),
                    filename: None,
                    width: None,
                    height: None,
                },
            ],
        };
        let mut images = HashMap::new();
        images.insert(
            "img-1".to_string(),
            ResolvedImage {
                media_type: "image/png".into(),
                data_base64: "QUJD".into(),
            },
        );

        let built = build_message(&msg, &images, false);
        assert_eq!(built["role"], "user");
        let content = built["content"].as_array().expect("content array");
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[0]["text"], "look at this");
        assert_eq!(content[1]["type"], "image_url");
        assert_eq!(content[1]["image_url"]["url"], "data:image/png;base64,QUJD");
    }

    #[test]
    fn build_message_user_unresolved_image_falls_back_to_text() {
        // No resolved bytes for the image → plain string content, text preserved.
        let msg = ProviderInputMessage {
            role: MessageRole::User,
            content: vec![
                ContentPart::Text { text: "hi".into() },
                ContentPart::Image {
                    id: "img-x".into(),
                    path: ".clai/images/img-x.png".into(),
                    media_type: "image/png".into(),
                    filename: None,
                    width: None,
                    height: None,
                },
            ],
        };
        let built = build_message(&msg, &HashMap::new(), false);
        assert_eq!(built["content"], "hi");
    }

    #[tokio::test]
    async fn sse_stream_handles_split_utf8_across_transport_chunks() {
        let frame = "data: {\"choices\":[{\"delta\":{\"content\":\"€\"}}]}\n\n";
        let bytes = frame.as_bytes();
        let split_at = bytes
            .iter()
            .position(|b| *b == 0xE2)
            .expect("euro sign should exist in frame")
            + 1;
        let stream = futures::stream::iter(vec![
            Ok(Bytes::from(bytes[..split_at].to_vec())),
            Ok(Bytes::from(bytes[split_at..].to_vec())),
        ]);

        let events = sse_to_provider_events(stream, "openai".into())
            .collect::<Vec<_>>()
            .await;

        assert!(matches!(
            events.first(),
            Some(Ok(ProviderEvent::MessageStart))
        ));
        assert!(events.iter().any(|event| matches!(
            event,
            Ok(ProviderEvent::TextDelta { text }) if text == "€"
        )));
    }
    fn conn_with_brand(brand: &str) -> ProviderConnection {
        ProviderConnection {
            id: "t".into(),
            name: "t".into(),
            protocol_id: "openai".into(),
            provider_id: brand.into(),
            auth_mode: AuthMode::DeveloperApiKey,
            base_url: None,
            secret_ref: "provider-connection::t".into(),
            model_id: String::new(),
            account_label: None,
            enabled: true,
            created_at: 0,
            updated_at: 0,
            env: Vec::new(),
        }
    }

    #[tokio::test]
    async fn openrouter_model_probe_works_without_key_and_uses_stored_key() {
        use axum::{http::HeaderMap, routing::get, Router};
        use std::sync::{Arc, Mutex};

        let seen_auth = Arc::new(Mutex::new(Vec::new()));
        let captured_auth = seen_auth.clone();
        let router = Router::new().route(
            "/v1/models",
            get(move |headers: HeaderMap| {
                let seen_auth = captured_auth.clone();
                async move {
                    let auth = headers
                        .get("authorization")
                        .map(|value| value.to_str().unwrap().to_string());
                    seen_auth.lock().unwrap().push(auth);
                    axum::Json(serde_json::json!({"data": [{"id": "openrouter/auto"}]}))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });

        let mut connection = conn_with_brand("openrouter");
        connection.base_url = Some(base_url);
        connection.secret_ref = "provider-connection::model-probe-no-key".into();
        let models = OpenAiAdapter.list_models(&connection).await.unwrap();
        assert_eq!(models[0].id, "openrouter/auto");

        connection.secret_ref = "provider-connection::model-probe-with-key".into();
        ProviderSecretStorage::set_secret(&connection.secret_ref, "  valid-key  ").unwrap();
        let models = OpenAiAdapter.list_models(&connection).await.unwrap();
        assert_eq!(models[0].id, "openrouter/auto");
        assert_eq!(
            *seen_auth.lock().unwrap(),
            vec![None, Some("Bearer valid-key".to_string())]
        );
    }

    #[tokio::test]
    async fn gemini_model_ids_lose_the_native_models_prefix() {
        use axum::{routing::get, Router};

        let router = Router::new().route(
            "/v1/models",
            get(|| async {
                axum::Json(serde_json::json!({
                    "data": [{ "id": "models/gemini-4-neon" }, { "id": "gemini-4-argon" }]
                }))
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });

        for (brand, want) in [
            ("gemini", ["gemini-4-neon", "gemini-4-argon"]),
            ("custom-openai", ["models/gemini-4-neon", "gemini-4-argon"]),
        ] {
            let mut connection = conn_with_brand(brand);
            connection.base_url = Some(base_url.clone());
            let models = OpenAiAdapter.list_models(&connection).await.unwrap();
            let ids: Vec<_> = models.iter().map(|m| m.id.as_str()).collect();
            assert_eq!(ids, want, "{brand}");
        }
    }

    /// Stored and hand-typed bases are read as they are, never rewritten:
    /// a versioned root or an as-written catalog base is used as written,
    /// anything else keeps the `/v1` that CLAI has always appended.
    #[test]
    fn api_root_keeps_legacy_bases_and_honours_versioned_ones() {
        let cases = [
            (
                "custom-openai",
                Some("http://localhost:11434"),
                "http://localhost:11434/v1",
            ),
            (
                "custom-openai",
                Some("http://localhost:11434/"),
                "http://localhost:11434/v1",
            ),
            (
                "groq",
                Some("https://api.groq.com/openai"),
                "https://api.groq.com/openai/v1",
            ),
            (
                "groq",
                Some("https://api.groq.com/openai/v1/"),
                "https://api.groq.com/openai/v1",
            ),
            (
                "custom-openai",
                Some("https://v1.example.com"),
                "https://v1.example.com/v1",
            ),
            (
                "custom-openai",
                Some("https://api.z.ai/api/coding/paas/v4"),
                "https://api.z.ai/api/coding/paas/v4",
            ),
            (
                "custom-openai",
                Some("https://example.com/api/v1beta"),
                "https://example.com/api/v1beta",
            ),
            (
                "custom-openai",
                Some("https://example.com/vision"),
                "https://example.com/vision/v1",
            ),
            (
                "custom-openai",
                Some("https://example.com/v1-beta"),
                "https://example.com/v1-beta/v1",
            ),
            (
                "deepseek",
                Some("https://api.deepseek.com/v1"),
                "https://api.deepseek.com/v1",
            ),
            (
                "litellm",
                Some("http://localhost:4000"),
                "http://localhost:4000/v1",
            ),
            (
                "perplexity",
                Some("https://api.perplexity.ai"),
                "https://api.perplexity.ai",
            ),
            // Saved before brands existed: the base, not the brand, decides.
            (
                "openai",
                Some("https://API.perplexity.ai/"),
                "https://API.perplexity.ai",
            ),
            // No stored base: the brand's catalog base, else OpenAI.
            ("openrouter", None, "https://openrouter.ai/api/v1"),
            (
                "gemini",
                None,
                "https://generativelanguage.googleapis.com/v1beta/openai",
            ),
            (
                "custom-openai",
                Some("https://generativelanguage.googleapis.com/v1beta/openai/"),
                "https://generativelanguage.googleapis.com/v1beta/openai",
            ),
            ("custom-openai", None, "https://api.openai.com/v1"),
        ];
        for (brand, base, want) in cases {
            let mut connection = conn_with_brand(brand);
            connection.base_url = base.map(str::to_string);
            assert_eq!(api_root(&connection), want, "{brand} {base:?}");
        }
    }

    #[test]
    fn hosted_provider_requires_stored_key() {
        let connection = conn_with_brand("openrouter");
        assert!(matches!(
            validate_api_key(&connection, None),
            Err(ProviderError::NotConfigured)
        ));
        assert!(matches!(
            validate_api_key(&connection, Some("  ".into())),
            Err(ProviderError::NotConfigured)
        ));
        assert_eq!(
            validate_api_key(&connection, Some("  token  ".into())).unwrap(),
            Some("token".into())
        );
    }

    #[test]
    fn keyless_provider_allows_missing_key() {
        assert_eq!(
            validate_api_key(&conn_with_brand("ollama"), None).unwrap(),
            None
        );
    }

    #[test]
    fn unknown_provider_requires_stored_key() {
        for brand in ["groq", "unknown-custom"] {
            let connection = conn_with_brand(brand);
            assert!(matches!(
                validate_api_key(&connection, None),
                Err(ProviderError::NotConfigured)
            ));
            assert!(matches!(
                validate_api_key(&connection, Some("  ".into())),
                Err(ProviderError::NotConfigured)
            ));
        }
    }

    #[test]
    fn catalog_extra_headers_are_brand_scoped() {
        // OpenRouter's attribution headers come from its catalog entry.
        let openrouter = catalog_extra_headers(&conn_with_brand("openrouter"));
        assert!(openrouter.iter().any(|(k, _)| k == "HTTP-Referer"));
        assert!(openrouter.iter().any(|(k, _)| k == "X-Title"));
        // A brand with no extra headers (or no catalog entry) yields none.
        assert!(catalog_extra_headers(&conn_with_brand("openai")).is_empty());
        assert!(catalog_extra_headers(&conn_with_brand("not-a-brand")).is_empty());
    }

    /// Regression (qwen3.8 via LiteLLM, captured wire log): the first
    /// tool_call delta carries the id, the 27 argument fragments that
    /// follow repeat `id` as an empty string. The empty strings must not
    /// erase the id, or `flush_tool_calls` drops the call and the turn
    /// ends with no visible text and no tool calls.
    #[test]
    fn empty_id_fragments_do_not_erase_tool_call_id() {
        let mut is_first = false;
        let mut tool_calls = Vec::new();
        let call_id = "call_c8d731cf5ccd4d88b4c36489";
        let args = r#"{"cmd":"sed -n '35,75p' .github/workflows/ci.yml"}"#.to_string();

        let first = format!(
            "data: {{\"choices\":[{{\"delta\":{{\"tool_calls\":[{{\"index\":0,\"id\":\"{call_id}\",\"type\":\"function\",\"function\":{{\"name\":\"bash_exec\",\"arguments\":\"\"}}}}]}}}}]}}\n\n"
        );
        let events = parse_sse_frame(&first, "openai", &mut is_first, &mut tool_calls);
        assert!(events.is_empty());

        // 27 fragments with a present-but-empty id, each carrying a slice
        // of the arguments JSON, exactly like the captured stream.
        for slice in [
            &args[..12],
            &args[12..24],
            &args[24..36],
            &args[36..args.len()],
        ] {
            let frame = format!(
                "data: {{\"choices\":[{{\"delta\":{{\"tool_calls\":[{{\"index\":0,\"id\":\"\",\"function\":{{\"arguments\":\"{}\"}}}}]}}}}]}}\n\n",
                slice.replace('"', r#"\""#)
            );
            let events = parse_sse_frame(&frame, "openai", &mut is_first, &mut tool_calls);
            assert!(events.is_empty(), "fragments carry no terminal reason");
        }

        let terminal = "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n";
        let events = parse_sse_frame(terminal, "openai", &mut is_first, &mut tool_calls);
        assert!(matches!(
            events.as_slice(),
            [
                Ok(ProviderEvent::ToolCallReady { tool_call }),
                Ok(ProviderEvent::StopReason { reason }),
            ] if
                tool_call.tool_call_id == call_id &&
                tool_call.tool_name == "bash_exec" &&
                tool_call.params == serde_json::json!({"cmd": "sed -n '35,75p' .github/workflows/ci.yml"}) &&
                reason == "tool_calls"
        ));
    }

    /// A terminal `finish_reason:"tool_calls"` that flushes zero calls
    /// must fail the turn, not complete it silently: the run would end
    /// green with neither visible text nor tool calls.
    #[test]
    fn announced_tool_calls_with_none_parsed_fails_the_turn() {
        let mut is_first = false;
        let mut tool_calls = Vec::new();
        // Fragments that never carried an id: nothing to flush.
        let fragment = "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"name\":\"bash_exec\",\"arguments\":\"{}\"}}]}}]}\n\n";
        assert!(parse_sse_frame(fragment, "openai", &mut is_first, &mut tool_calls).is_empty());

        let terminal = "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n";
        let events = parse_sse_frame(terminal, "openai", &mut is_first, &mut tool_calls);
        assert!(matches!(
            events.as_slice(),
            [
                Err(ProviderError::RequestFailed(message)),
                Ok(ProviderEvent::StopReason { reason }),
            ] if message.contains("no tool call could be parsed") && reason == "tool_calls"
        ));
    }

    /// A terminal chunk may carry its first (or only) tool-call fragment
    /// in the same delta that announces `finish_reason:"tool_calls"`: the
    /// delta must be applied before the flush, or the call is lost and
    /// the turn trips the zero-flush guard.
    #[test]
    fn terminal_chunk_with_first_tool_call_fragment_is_flushed() {
        let mut is_first = false;
        let mut tool_calls = Vec::new();
        let terminal = r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"bash_exec","arguments":"{\"cmd\":\"ls\"}"}}]},"finish_reason":"tool_calls"}]}"#;
        let events = parse_sse_frame(terminal, "openai", &mut is_first, &mut tool_calls);
        assert!(matches!(
            events.as_slice(),
            [
                Ok(ProviderEvent::ToolCallReady { tool_call }),
                Ok(ProviderEvent::StopReason { reason }),
            ] if
                tool_call.tool_call_id == "call_1" &&
                tool_call.tool_name == "bash_exec" &&
                tool_call.params == serde_json::json!({"cmd": "ls"}) &&
                reason == "tool_calls"
        ));
    }

    /// The terminal chunk may also carry the final arguments fragment:
    /// flushing before applying that delta would truncate the call.
    #[test]
    fn terminal_chunk_final_argument_fragment_is_not_lost() {
        let mut is_first = false;
        let mut tool_calls = Vec::new();
        let head = r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_2","function":{"name":"bash_exec","arguments":"{\"cmd\":"}}]}}]}"#;
        assert!(parse_sse_frame(head, "openai", &mut is_first, &mut tool_calls).is_empty());

        let terminal = r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"ls\"}"}}]},"finish_reason":"tool_calls"}]}"#;
        let events = parse_sse_frame(terminal, "openai", &mut is_first, &mut tool_calls);
        assert!(matches!(
            events.as_slice(),
            [
                Ok(ProviderEvent::ToolCallReady { tool_call }),
                Ok(ProviderEvent::StopReason { reason }),
            ] if
                tool_call.tool_call_id == "call_2" &&
                tool_call.params == serde_json::json!({"cmd": "ls"}) &&
                reason == "tool_calls"
        ));
    }

    /// Gemini's thought signatures on its OpenAI-compatible layer. The
    /// tool-call shapes are the documented Chat Completions "Parallel
    /// function calling example" (docs.cloud.google.com/gemini-enterprise-agent-platform/models/thinking/thought-signatures):
    /// `extra_content.google.thought_signature`, on the first call only.
    /// Streamed, they arrive as `delta.tool_calls` fragments, which is where
    /// the Vercel AI SDK's openai-compatible provider reads them.
    mod gemini_thought_signatures {
        use super::*;

        const PARIS: &str = "function-call-f3b9ecb3-d55f-4076-98c8-b13e9d1c0e01";
        const LONDON: &str = "function-call-335673ad-913e-42d1-bbf5-387c8ab80f44";
        const PARALLEL_CALLS: &str = r#"data: {"choices":[{"index":0,"delta":{"role":"assistant","tool_calls":[{"index":0,"id":"function-call-f3b9ecb3-d55f-4076-98c8-b13e9d1c0e01","type":"function","function":{"name":"get_current_temperature","arguments":"{\"location\":\"Paris\"}"},"extra_content":{"google":{"thought_signature":"<SIGNATURE_A>"}}},{"index":1,"id":"function-call-335673ad-913e-42d1-bbf5-387c8ab80f44","type":"function","function":{"name":"get_current_temperature","arguments":"{\"location\":\"London\"}"}}]}}]}"#;
        const TOOL_CALLS_FINISH: &str =
            r#"data: {"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#;

        fn ready_calls(frames: &[&str]) -> Vec<ToolInvocationDraft> {
            let mut is_first = true;
            let mut buf = Vec::new();
            frames
                .iter()
                .flat_map(|frame| parse_sse_frame(frame, "gemini", &mut is_first, &mut buf))
                .filter_map(|event| match event {
                    Ok(ProviderEvent::ToolCallReady { tool_call }) => Some(tool_call),
                    _ => None,
                })
                .collect()
        }

        fn tool_use(id: &str, location: &str, signature: Option<&str>) -> ContentPart {
            ContentPart::ToolUse {
                tool_call_id: id.into(),
                tool_name: "get_current_temperature".into(),
                arguments: serde_json::json!({ "location": location }),
                thought_signature: signature.map(Into::into),
            }
        }

        fn parallel_turn(signature: Option<&str>) -> CompletionRequest {
            CompletionRequest {
                run_id: "run".into(),
                session_id: "session".into(),
                model_id: "gemini-4-neon".into(),
                messages: vec![
                    ProviderInputMessage {
                        role: MessageRole::User,
                        content: vec![ContentPart::Text {
                            text: "Check the weather in Paris and London.".into(),
                        }],
                    },
                    ProviderInputMessage {
                        role: MessageRole::Assistant,
                        content: vec![
                            tool_use(PARIS, "Paris", signature),
                            tool_use(LONDON, "London", None),
                        ],
                    },
                ],
                tools: Vec::new(),
                temperature: None,
                max_output_tokens: None,
                images: HashMap::new(),
            }
        }

        #[test]
        fn only_the_first_parallel_call_keeps_a_signature() {
            let calls = ready_calls(&[PARALLEL_CALLS, TOOL_CALLS_FINISH]);
            let got: Vec<_> = calls
                .iter()
                .map(|c| (c.tool_call_id.as_str(), c.thought_signature.as_deref()))
                .collect();
            assert_eq!(got, [(PARIS, Some("<SIGNATURE_A>")), (LONDON, None)]);
        }

        /// A signature on the first fragment survives the argument
        /// fragments after it, which carry none.
        #[test]
        fn signature_survives_later_fragments() {
            let head = r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"bash_exec","arguments":""},"extra_content":{"google":{"thought_signature":"sig"}}}]}}]}"#;
            let tail = r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"","function":{"arguments":"{}"},"extra_content":{"google":{"thought_signature":""}}}]}}]}"#;
            let calls = ready_calls(&[head, tail, TOOL_CALLS_FINISH]);
            assert_eq!(calls.len(), 1);
            assert_eq!(calls[0].thought_signature.as_deref(), Some("sig"));
        }

        #[test]
        fn a_call_without_a_signature_is_sent_back_without_one() {
            let unsigned = r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"bash_exec","arguments":"{}"}}]},"finish_reason":"tool_calls"}]}"#;
            let calls = ready_calls(&[unsigned]);
            assert_eq!(calls.len(), 1);
            assert_eq!(calls[0].thought_signature, None);

            let body = build_request_body(&parallel_turn(None), true);
            assert!(!body.to_string().contains("extra_content"), "{body}");
        }

        /// The assistant message of the documented step 2 request.
        #[test]
        fn signature_is_sent_back_on_the_call_that_carried_it() {
            let body = build_request_body(&parallel_turn(Some("<SIGNATURE_A>")), true);
            let documented = serde_json::json!([
                {
                    "extra_content": { "google": { "thought_signature": "<SIGNATURE_A>" } },
                    "function": {
                        "arguments": "{\"location\":\"Paris\"}",
                        "name": "get_current_temperature"
                    },
                    "id": PARIS,
                    "type": "function"
                },
                {
                    "function": {
                        "arguments": "{\"location\":\"London\"}",
                        "name": "get_current_temperature"
                    },
                    "id": LONDON,
                    "type": "function"
                }
            ]);
            assert_eq!(body["messages"][1]["tool_calls"], documented);
        }

        /// Every other brand gets the body it got before signatures were
        /// stored, even for history Gemini signed.
        #[test]
        fn other_brands_get_a_byte_identical_body() {
            let signed = build_request_body(&parallel_turn(Some("<SIGNATURE_A>")), false);
            let unsigned = build_request_body(&parallel_turn(None), false);
            assert_eq!(signed.to_string(), unsigned.to_string());
        }

        /// Through `stream_completion`: the Gemini brand gets the signature
        /// back, a custom OpenAI-compatible server does not.
        #[tokio::test]
        async fn only_the_gemini_brand_is_sent_signatures() {
            use axum::{routing::post, Router};
            use std::sync::{Arc, Mutex};

            let bodies = Arc::new(Mutex::new(Vec::new()));
            let captured = bodies.clone();
            let router = Router::new().route(
                "/v1/chat/completions",
                post(move |body: String| {
                    let captured = captured.clone();
                    async move {
                        captured.lock().unwrap().push(body);
                        "data: [DONE]\n\n"
                    }
                }),
            );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base_url = format!("http://{}", listener.local_addr().unwrap());
            tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });

            for brand in ["gemini", "custom-openai"] {
                let mut connection = conn_with_brand(brand);
                connection.base_url = Some(base_url.clone());
                connection.secret_ref = format!("provider-connection::signatures-{brand}");
                ProviderSecretStorage::set_secret(&connection.secret_ref, "key").unwrap();
                let stream = OpenAiAdapter
                    .stream_completion(&connection, parallel_turn(Some("<SIGNATURE_A>")))
                    .await
                    .unwrap();
                stream.collect::<Vec<_>>().await;
            }

            let bodies = bodies.lock().unwrap();
            assert_eq!(bodies.len(), 2);
            assert!(bodies[0].contains(r#""thought_signature":"<SIGNATURE_A>""#));
            assert!(!bodies[1].contains("extra_content"), "{}", bodies[1]);
        }
    }

    mod stream_errors {
        use super::*;
        use crate::assistant::providers::failure::FailureKind;
        use crate::assistant::providers::retry::{
            retry_transient, until_first_output, RetryHooks, RetryStop,
        };
        use crate::assistant::providers::types::ProviderStream;
        use tokio_util::sync::CancellationToken;

        /// OpenRouter's documented mid-stream failure: HTTP 200, then one
        /// chunk with a top-level `error` and `finish_reason:"error"`
        /// (openrouter.ai/docs/api-reference/streaming).
        const ERROR_CHUNK: &str = r#"data: {"id":"cmpl-abc123","object":"chat.completion.chunk","created":1234567890,"model":"openai/gpt-4o","provider":"openai","error":{"code":502,"message":"Provider disconnected unexpectedly"},"choices":[{"index":0,"delta":{"content":""},"finish_reason":"error"}]}"#;
        const TEXT: &str = r#"data: {"choices":[{"delta":{"content":"Hello"}}]}"#;
        const THINKING: &str = r#"data: {"choices":[{"delta":{"reasoning_content":"Hmm"}}]}"#;
        const DONE: &str = "data: [DONE]";

        fn finish(reason: &str) -> String {
            format!(r#"data: {{"choices":[{{"delta":{{}},"finish_reason":"{reason}"}}]}}"#)
        }

        fn sse(frames: &[String]) -> ProviderStream {
            let body: String = frames.iter().map(|frame| format!("{frame}\n\n")).collect();
            let bytes = futures::stream::iter(vec![Ok(Bytes::from(body))]);
            Box::pin(sse_to_provider_events(bytes, "openrouter".into()))
        }

        async fn events(frames: &[String]) -> Vec<ProviderEvent> {
            sse(frames).map(|event| event.unwrap()).collect().await
        }

        /// Opens the streams in `attempts`, one per request, the way the
        /// turn and the compaction open theirs. Returns what the caller gets
        /// and the number of requests.
        async fn open(attempts: &[&[String]]) -> (Result<Vec<ProviderEvent>, RetryStop>, usize) {
            let cancel = CancellationToken::new();
            let hooks = RetryHooks::silent(&cancel);
            let mut requests = 0;
            let opened = retry_transient(&hooks, || {
                let frames = attempts[requests];
                requests += 1;
                until_first_output(sse(frames))
            })
            .await;
            let outcome = match opened {
                Ok(stream) => Ok(stream.map(|event| event.unwrap()).collect().await),
                Err(stop) => Err(stop),
            };
            (outcome, requests)
        }

        fn s(frames: &[&str]) -> Vec<String> {
            frames.iter().map(|frame| frame.to_string()).collect()
        }

        #[tokio::test]
        async fn error_chunk_becomes_a_classified_provider_error() {
            assert!(matches!(
                events(&s(&[ERROR_CHUNK])).await.as_slice(),
                [
                    ProviderEvent::MessageStart,
                    ProviderEvent::ProviderError { failure },
                ] if failure.kind == FailureKind::Transient
                    && failure.message == "Provider disconnected unexpectedly"
            ));
        }

        #[tokio::test(start_paused = true)]
        async fn error_chunk_as_the_first_chunk_is_retried() {
            let (outcome, requests) = open(&[&s(&[ERROR_CHUNK]), &s(&[TEXT, DONE])]).await;
            assert_eq!(requests, 2);
            let events = outcome.unwrap();
            assert!(events.iter().any(
                |event| matches!(event, ProviderEvent::TextDelta { text } if text == "Hello")
            ));
            assert!(!events
                .iter()
                .any(|event| matches!(event, ProviderEvent::ProviderError { .. })));
        }

        #[tokio::test(start_paused = true)]
        async fn error_chunk_after_output_is_not_retried() {
            for output in [TEXT, THINKING] {
                let (outcome, requests) = open(&[&s(&[output, ERROR_CHUNK])]).await;
                assert_eq!(requests, 1, "{output}");
                assert!(
                    matches!(
                        outcome.unwrap().as_slice(),
                        [
                            ProviderEvent::MessageStart,
                            ProviderEvent::TextDelta { .. } | ProviderEvent::ThinkingDelta { .. },
                            ProviderEvent::ProviderError { failure },
                        ] if failure.kind == FailureKind::Transient
                    ),
                    "{output}"
                );
            }
        }

        #[tokio::test(start_paused = true)]
        async fn filtered_finish_reasons_are_content_filtered_and_not_retried() {
            for reason in ["sensitive", "content_filter"] {
                let after_text = events(&[TEXT.into(), finish(reason)]).await;
                assert!(
                    matches!(
                        after_text.as_slice(),
                        [
                            ProviderEvent::MessageStart,
                            ProviderEvent::TextDelta { .. },
                            ProviderEvent::ProviderError { failure },
                        ] if failure.kind == FailureKind::ContentFiltered
                    ),
                    "{reason}: {after_text:?}"
                );
                let (outcome, requests) = open(&[&[finish(reason)]]).await;
                assert_eq!(requests, 1, "{reason}");
                assert!(
                    matches!(
                        outcome,
                        Err(RetryStop::Failed(ref error))
                            if error.kind() == Some(FailureKind::ContentFiltered)
                    ),
                    "{reason}"
                );
            }
        }

        /// The turn compacts once when the opening fails with
        /// `is_context_limit`; a context-window finish before any output
        /// now fails the opening.
        #[tokio::test(start_paused = true)]
        async fn context_window_finish_before_output_goes_to_compaction() {
            let (outcome, requests) = open(&[&[finish("model_context_window_exceeded")]]).await;
            assert_eq!(requests, 1);
            let Err(RetryStop::Failed(error)) = outcome else {
                panic!("expected a failed opening, got {outcome:?}");
            };
            assert!(error.is_context_limit());
        }

        /// A stream with no output is read to its end before it is handed
        /// over; the caller must still be able to poll it.
        #[tokio::test]
        async fn a_stream_without_output_is_replayed_whole() {
            let (outcome, requests) = open(&[&[finish("stop"), DONE.into()]]).await;
            assert_eq!(requests, 1);
            assert!(matches!(
                outcome.unwrap().as_slice(),
                [
                    ProviderEvent::MessageStart,
                    ProviderEvent::StopReason { .. },
                    ProviderEvent::MessageComplete,
                ]
            ));
        }
    }
}
