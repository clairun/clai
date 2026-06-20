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
    ProviderConnection, ProviderDescriptor, ProviderEvent, ResolvedImage, RunUsage,
    ToolInvocationDraft,
};

use super::types::{ProviderAdapter, ProviderError};

pub const ANTHROPIC_PROVIDER_ID: &str = "anthropic";

const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
const ANTHROPIC_API_VERSION: &str = "2023-06-01";

pub fn provider_descriptor() -> ProviderDescriptor {
    ProviderDescriptor {
        id: ANTHROPIC_PROVIDER_ID.to_string(),
        display_name: "Anthropic-Compatible".to_string(),
        protocol_family: ProtocolFamily::Anthropic,
        supported_auth_modes: vec![AuthMode::DeveloperApiKey],
        configurable_base_url: true,
        is_cli_backed: false,
    }
}

pub struct AnthropicAdapter;

#[async_trait]
impl ProviderAdapter for AnthropicAdapter {
    fn provider_id(&self) -> &'static str {
        ANTHROPIC_PROVIDER_ID
    }

    fn protocol_family(&self) -> ProtocolFamily {
        ProtocolFamily::Anthropic
    }

    async fn list_models(
        &self,
        connection: &ProviderConnection,
    ) -> Result<Vec<ModelInfo>, ProviderError> {
        let api_key = get_api_key(connection)?;
        let base = base_url(connection);
        let url = format!("{}/v1/models", base);

        tracing::info!(
            url = %url,
            provider_id = %connection.provider_id,
            "Fetching models from Anthropic-compatible provider"
        );

        let client = Client::new();
        let resp = client
            .get(&url)
            .header("x-api-key", &api_key)
            .header("anthropic-version", ANTHROPIC_API_VERSION)
            .send()
            .await
            .map_err(|e| {
                tracing::error!(url = %url, error = %e, "HTTP request failed");
                ProviderError::RequestFailed(e.to_string())
            })?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            tracing::error!(url = %url, status = %status, body = %body, "Provider returned error");
            return Err(ProviderError::RequestFailed(format!(
                "HTTP {}: {}",
                status, body
            )));
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
                let id = m["id"].as_str()?.to_string();
                let display_name = m["display_name"].as_str().unwrap_or(&id).to_string();
                Some(ModelInfo {
                    id,
                    display_name,
                    supports_tools: true,
                    // Anthropic API models in current rotation are all
                    // multimodal (Claude 3+ accept image blocks).
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
        let base = base_url(connection);
        let url = format!("{}/v1/messages", base);
        let body = build_request_body(&request);

        let client = Client::new();
        let resp = client
            .post(&url)
            .header("x-api-key", &api_key)
            .header("anthropic-version", ANTHROPIC_API_VERSION)
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| ProviderError::RequestFailed(e.to_string()))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::RequestFailed(format!(
                "HTTP {}: {}",
                status, body
            )));
        }

        let byte_stream = resp.bytes_stream();
        let event_stream = sse_to_provider_events(byte_stream);

        Ok(Box::pin(event_stream))
    }
}

fn get_api_key(connection: &ProviderConnection) -> Result<String, ProviderError> {
    ProviderSecretStorage::get_secret(&connection.secret_ref)
        .map_err(|e| ProviderError::RequestFailed(format!("Failed to read API key: {}", e)))?
        .ok_or(ProviderError::NotConfigured)
}

fn base_url(connection: &ProviderConnection) -> String {
    connection
        .base_url
        .as_deref()
        .unwrap_or(DEFAULT_BASE_URL)
        .trim_end_matches('/')
        .to_string()
}

// =============================================================================
// Request Building
// =============================================================================

fn build_request_body(request: &CompletionRequest) -> serde_json::Value {
    // Anthropic separates system from messages
    let mut system_parts: Vec<serde_json::Value> = Vec::new();
    let mut messages: Vec<serde_json::Value> = Vec::new();

    for msg in &request.messages {
        match msg.role {
            MessageRole::System => {
                let text = msg
                    .content
                    .iter()
                    .filter_map(|p| match p {
                        ContentPart::Text { text } => Some(text.clone()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("");
                if !text.is_empty() {
                    system_parts.push(json!({ "type": "text", "text": text }));
                }
            }
            MessageRole::User | MessageRole::Assistant | MessageRole::Tool => {
                if let Some(m) = build_message(msg, &request.images) {
                    messages.push(m);
                }
            }
        }
    }

    let mut body = json!({
        "model": request.model_id,
        "messages": messages,
        "stream": true,
        "max_tokens": request.max_output_tokens.unwrap_or(8192),
    });

    if !system_parts.is_empty() {
        body["system"] = json!(system_parts);
    }

    if !request.tools.is_empty() {
        let tools: Vec<serde_json::Value> = request
            .tools
            .iter()
            .map(|t| {
                json!({
                    "name": t.name,
                    "description": t.description,
                    "input_schema": t.input_schema,
                })
            })
            .collect();
        body["tools"] = json!(tools);
    }

    if let Some(temp) = request.temperature {
        body["temperature"] = json!(temp);
    }

    body
}

/// Build a single Anthropic message. Tool results become role: "user" with tool_result content.
fn build_message(
    msg: &crate::assistant::types::ProviderInputMessage,
    images: &HashMap<String, ResolvedImage>,
) -> Option<serde_json::Value> {
    match msg.role {
        MessageRole::System => None, // handled separately
        MessageRole::User => {
            let content = user_content_parts(&msg.content, images);
            if content.is_empty() {
                return None;
            }
            Some(json!({ "role": "user", "content": content }))
        }
        MessageRole::Assistant => {
            let mut content: Vec<serde_json::Value> = Vec::new();

            for part in &msg.content {
                match part {
                    // Replay the signed thinking block verbatim. The engine
                    // places Thinking first in content, so it lands first here —
                    // which Anthropic requires for tool-use continuation. Do NOT
                    // reformat the text; the signature signs it byte-for-byte.
                    ContentPart::Thinking {
                        text,
                        signature: Some(signature),
                    } if !text.is_empty() => {
                        content.push(json!({
                            "type": "thinking",
                            "thinking": text,
                            "signature": signature,
                        }));
                    }
                    // Unsigned thinking (OpenAI reasoning_content, CLI agents, or
                    // pre-feature history) can't be re-signed; Anthropic rejects
                    // thinking blocks without a valid signature, so drop it.
                    ContentPart::Thinking { .. } => {}
                    ContentPart::Text { text } if !text.is_empty() => {
                        content.push(json!({ "type": "text", "text": text }));
                    }
                    ContentPart::Text { .. } => {}
                    ContentPart::ToolUse {
                        tool_call_id,
                        tool_name,
                        arguments,
                    } => {
                        content.push(json!({
                            "type": "tool_use",
                            "id": tool_call_id,
                            "name": tool_name,
                            "input": arguments,
                        }));
                    }
                    _ => {}
                }
            }

            if content.is_empty() {
                return None;
            }
            Some(json!({ "role": "assistant", "content": content }))
        }
        MessageRole::Tool => {
            // Anthropic: tool results are sent as role: "user" with tool_result content blocks
            let mut content: Vec<serde_json::Value> = Vec::new();

            for part in &msg.content {
                if let ContentPart::ToolResult {
                    tool_call_id,
                    payload,
                    ..
                } = part
                {
                    content.push(json!({
                        "type": "tool_result",
                        "tool_use_id": tool_call_id,
                        "content": payload.to_string(),
                    }));
                }
            }

            if content.is_empty() {
                return None;
            }
            Some(json!({ "role": "user", "content": content }))
        }
    }
}

/// User-message content blocks: text plus any resolved image blocks. An image
/// whose bytes were not resolved (missing file) is silently skipped — the
/// surrounding text is still sent.
fn user_content_parts(
    content: &[ContentPart],
    images: &HashMap<String, ResolvedImage>,
) -> Vec<serde_json::Value> {
    content
        .iter()
        .filter_map(|p| match p {
            ContentPart::Text { text } if !text.is_empty() => {
                Some(json!({ "type": "text", "text": text }))
            }
            ContentPart::Image { id, .. } => images.get(id).map(|img| {
                json!({
                    "type": "image",
                    "source": {
                        "type": "base64",
                        "media_type": img.media_type,
                        "data": img.data_base64,
                    }
                })
            }),
            _ => None,
        })
        .collect()
}

// =============================================================================
// SSE Stream Parsing (Anthropic format)
// =============================================================================

/// Anthropic SSE events use typed event fields:
///   event: message_start
///   event: content_block_start
///   event: content_block_delta
///   event: content_block_stop
///   event: message_delta
///   event: message_stop
///   event: ping
struct SseState {
    stream: Pin<Box<dyn Stream<Item = reqwest::Result<Bytes>> + Send>>,
    buf: Vec<u8>,
    emitted_start: bool,
    /// Accumulate tool use blocks by index
    tool_calls: Vec<PartialToolCall>,
}

#[derive(Default, Clone)]
struct PartialToolCall {
    id: String,
    name: String,
    arguments: String,
}

fn sse_to_provider_events(
    byte_stream: impl Stream<Item = reqwest::Result<Bytes>> + Send + 'static,
) -> impl Stream<Item = Result<ProviderEvent, ProviderError>> + Send {
    let state = SseState {
        stream: byte_stream.boxed(),
        buf: Vec::new(),
        emitted_start: false,
        tool_calls: Vec::new(),
    };

    futures::stream::unfold(state, |mut state| async move {
        loop {
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

                let events =
                    parse_sse_frame(&frame, &mut state.emitted_start, &mut state.tool_calls);
                if !events.is_empty() {
                    return Some((events, state));
                }
                continue;
            }

            match state.stream.next().await {
                Some(Ok(bytes)) => state.buf.extend_from_slice(bytes.as_ref()),
                Some(Err(e)) => {
                    return Some((
                        vec![Err(ProviderError::RequestFailed(e.to_string()))],
                        state,
                    ));
                }
                None => {
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
                                &mut state.emitted_start,
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

fn parse_sse_frame(
    frame: &str,
    emitted_start: &mut bool,
    tool_calls: &mut Vec<PartialToolCall>,
) -> Vec<Result<ProviderEvent, ProviderError>> {
    let mut events = Vec::new();
    let mut event_type = String::new();
    let mut data_buf = String::new();

    for line in frame.lines() {
        if let Some(ev) = line.strip_prefix("event: ") {
            event_type = ev.trim().to_string();
        } else if let Some(d) = line.strip_prefix("data: ") {
            data_buf.push_str(d.trim());
        } else if let Some(d) = line.strip_prefix("data:") {
            data_buf.push_str(d.trim());
        }
    }

    if data_buf.is_empty() && event_type.is_empty() {
        return events;
    }

    // Handle ping events
    if event_type == "ping" {
        return events;
    }

    let json: serde_json::Value = match serde_json::from_str(&data_buf) {
        Ok(v) => v,
        Err(e) => {
            if !data_buf.is_empty() {
                tracing::warn!(
                    "Failed to parse Anthropic SSE JSON: {} (event: {}, data: {})",
                    e,
                    event_type,
                    data_buf
                );
            }
            return events;
        }
    };

    match event_type.as_str() {
        "message_start" => {
            if !*emitted_start {
                *emitted_start = true;
                events.push(Ok(ProviderEvent::MessageStart));
            }
            // Extract usage from message_start if present
            if let Some(usage_obj) = json.get("message").and_then(|m| m.get("usage")) {
                let usage = parse_usage(usage_obj);
                events.push(Ok(ProviderEvent::Usage { usage }));
            }
        }
        "content_block_start" => {
            if !*emitted_start {
                *emitted_start = true;
                events.push(Ok(ProviderEvent::MessageStart));
            }
            if let Some(cb) = json.get("content_block") {
                match cb.get("type").and_then(|t| t.as_str()) {
                    Some("tool_use") => {
                        let index =
                            json.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
                        while tool_calls.len() <= index {
                            tool_calls.push(PartialToolCall::default());
                        }
                        if let Some(id) = cb.get("id").and_then(|i| i.as_str()) {
                            tool_calls[index].id = id.to_string();
                        }
                        if let Some(name) = cb.get("name").and_then(|n| n.as_str()) {
                            tool_calls[index].name = name.to_string();
                        }
                        // Anthropic itself opens tool_use blocks with
                        // `"input": {}` and streams the real input via
                        // input_json_delta. Some Anthropic-compatible
                        // providers ship the complete input object here
                        // instead, with no deltas — capture it, or the
                        // call dispatches with empty params.
                        if let Some(input) = cb
                            .get("input")
                            .filter(|i| i.as_object().is_some_and(|o| !o.is_empty()))
                        {
                            tool_calls[index].arguments = input.to_string();
                        }
                    }
                    // A thinking block opens empty; its text/signature arrive as
                    // content_block_delta. Nothing to capture at start.
                    Some("thinking") => {}
                    // Encrypted reasoning we can't read or re-sign. We don't
                    // round-trip it, which can break tool-use continuation
                    // against strict Anthropic; MiniMax does not emit these.
                    Some("redacted_thinking") => {
                        tracing::warn!(
                            "anthropic: redacted_thinking block dropped (not round-tripped)"
                        );
                    }
                    _ => {}
                }
            }
        }
        "content_block_delta" => {
            if let Some(delta) = json.get("delta") {
                let delta_type = delta.get("type").and_then(|t| t.as_str()).unwrap_or("");
                match delta_type {
                    "text_delta" => {
                        if let Some(text) = delta.get("text").and_then(|t| t.as_str()) {
                            if !text.is_empty() {
                                events.push(Ok(ProviderEvent::TextDelta {
                                    text: text.to_string(),
                                }));
                            }
                        }
                    }
                    "input_json_delta" => {
                        let index =
                            json.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
                        if let Some(partial) = delta.get("partial_json").and_then(|p| p.as_str()) {
                            if index < tool_calls.len() {
                                tool_calls[index].arguments.push_str(partial);
                            }
                        }
                    }
                    "thinking_delta" => {
                        if let Some(text) = delta.get("thinking").and_then(|t| t.as_str()) {
                            if !text.is_empty() {
                                events.push(Ok(ProviderEvent::ThinkingDelta {
                                    text: text.to_string(),
                                }));
                            }
                        }
                    }
                    "signature_delta" => {
                        if let Some(sig) = delta.get("signature").and_then(|s| s.as_str()) {
                            if !sig.is_empty() {
                                events.push(Ok(ProviderEvent::ThinkingSignature {
                                    signature: sig.to_string(),
                                }));
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        "content_block_stop" => {
            // When a tool_use content block stops, emit it as ready
            let index = json.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
            if index < tool_calls.len() && !tool_calls[index].id.is_empty() {
                let tc = &tool_calls[index];
                let params = super::parse_tool_arguments(&tc.name, &tc.arguments);
                events.push(Ok(ProviderEvent::ToolCallReady {
                    tool_call: ToolInvocationDraft {
                        tool_call_id: tc.id.clone(),
                        tool_name: tc.name.clone(),
                        params,
                    },
                }));
            }
        }
        "message_delta" => {
            // May contain usage info and stop_reason
            if let Some(usage_obj) = json.get("usage") {
                let usage = parse_usage(usage_obj);
                events.push(Ok(ProviderEvent::Usage { usage }));
            }
        }
        "message_stop" => {
            events.push(Ok(ProviderEvent::MessageComplete));
        }
        "error" => {
            let message = json
                .get("error")
                .and_then(|e| e.get("message"))
                .and_then(|m| m.as_str())
                .unwrap_or("Unknown Anthropic API error")
                .to_string();
            events.push(Ok(ProviderEvent::ProviderError { message }));
        }
        _ => {
            // Unknown event type, ignore
        }
    }

    events
}

fn parse_usage(usage_obj: &serde_json::Value) -> RunUsage {
    RunUsage {
        input_tokens: usage_obj.get("input_tokens").and_then(|v| v.as_u64()),
        output_tokens: usage_obj.get("output_tokens").and_then(|v| v.as_u64()),
        reasoning_tokens: None,
        total_tokens: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assistant::types::{ContentPart, MessageRole, ProviderInputMessage};
    use futures::StreamExt;

    #[test]
    fn build_message_system_extracted_to_top_level() {
        let request = CompletionRequest {
            run_id: "run_1".into(),
            session_id: "sess_1".into(),
            model_id: "claude-sonnet-4-20250514".into(),
            messages: vec![
                ProviderInputMessage {
                    role: MessageRole::System,
                    content: vec![ContentPart::Text {
                        text: "You are a helpful assistant.".into(),
                    }],
                },
                ProviderInputMessage {
                    role: MessageRole::User,
                    content: vec![ContentPart::Text {
                        text: "Hello".into(),
                    }],
                },
            ],
            tools: vec![],
            temperature: None,
            max_output_tokens: Some(4096),
            images: Default::default(),
        };

        let body = build_request_body(&request);

        // System should be at top level, not in messages
        assert!(body.get("system").is_some());
        let system = body["system"].as_array().unwrap();
        assert_eq!(system[0]["text"], "You are a helpful assistant.");

        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "user");
    }

    #[test]
    fn build_message_tool_use_in_assistant() {
        let msg = ProviderInputMessage {
            role: MessageRole::Assistant,
            content: vec![
                ContentPart::Text {
                    text: "Let me check.".into(),
                },
                ContentPart::ToolUse {
                    tool_call_id: "toolu_123".into(),
                    tool_name: "get_weather".into(),
                    arguments: json!({"city": "London"}),
                },
            ],
        };

        let built = build_message(&msg, &HashMap::new()).unwrap();
        assert_eq!(built["role"], "assistant");
        let content = built["content"].as_array().unwrap();
        assert_eq!(content.len(), 2);
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[1]["type"], "tool_use");
        assert_eq!(content[1]["id"], "toolu_123");
        assert_eq!(content[1]["name"], "get_weather");
    }

    #[test]
    fn build_message_user_includes_resolved_image_block() {
        let msg = ProviderInputMessage {
            role: MessageRole::User,
            content: vec![
                ContentPart::Text {
                    text: "what is this?".into(),
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

        let built = build_message(&msg, &images).unwrap();
        assert_eq!(built["role"], "user");
        let content = built["content"].as_array().unwrap();
        assert_eq!(content.len(), 2);
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[1]["type"], "image");
        assert_eq!(content[1]["source"]["type"], "base64");
        assert_eq!(content[1]["source"]["media_type"], "image/png");
        assert_eq!(content[1]["source"]["data"], "QUJD");
    }

    #[test]
    fn build_message_user_skips_unresolved_image() {
        let msg = ProviderInputMessage {
            role: MessageRole::User,
            content: vec![
                ContentPart::Text { text: "hi".into() },
                ContentPart::Image {
                    id: "missing".into(),
                    path: ".clai/images/missing.png".into(),
                    media_type: "image/png".into(),
                    filename: None,
                    width: None,
                    height: None,
                },
            ],
        };
        // No entry for "missing" -> only the text block survives.
        let built = build_message(&msg, &HashMap::new()).unwrap();
        let content = built["content"].as_array().unwrap();
        assert_eq!(content.len(), 1);
        assert_eq!(content[0]["type"], "text");
    }

    #[test]
    fn build_message_tool_result_becomes_user_role() {
        let msg = ProviderInputMessage {
            role: MessageRole::Tool,
            content: vec![ContentPart::ToolResult {
                tool_call_id: "toolu_123".into(),
                payload: json!({"temperature": 15}),
                started_at: Some(1_700_000_000_000),
                completed_at: Some(1_700_000_000_250),
            }],
        };

        let built = build_message(&msg, &HashMap::new()).unwrap();
        assert_eq!(built["role"], "user");
        let content = built["content"].as_array().unwrap();
        assert_eq!(content[0]["type"], "tool_result");
        assert_eq!(content[0]["tool_use_id"], "toolu_123");
    }

    #[test]
    fn build_request_body_includes_tools() {
        use crate::assistant::types::ToolDefinition;

        let request = CompletionRequest {
            run_id: "run_1".into(),
            session_id: "sess_1".into(),
            model_id: "claude-sonnet-4-20250514".into(),
            messages: vec![ProviderInputMessage {
                role: MessageRole::User,
                content: vec![ContentPart::Text {
                    text: "Hello".into(),
                }],
            }],
            tools: vec![ToolDefinition {
                name: "get_weather".into(),
                description: "Get weather for a city".into(),
                input_schema: json!({"type": "object", "properties": {"city": {"type": "string"}}}),
            }],
            temperature: Some(0.7),
            max_output_tokens: Some(2048),
            images: Default::default(),
        };

        let body = build_request_body(&request);
        let temp = body["temperature"].as_f64().unwrap();
        assert!((temp - 0.7).abs() < 0.001);
        assert_eq!(body["max_tokens"], 2048);
        assert!(body["stream"].as_bool().unwrap());

        let tools = body["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["name"], "get_weather");
        assert!(tools[0].get("input_schema").is_some());
    }

    #[tokio::test]
    async fn sse_stream_handles_split_utf8_across_transport_chunks() {
        let frame = "event: content_block_delta\ndata: {\"delta\":{\"type\":\"text_delta\",\"text\":\"€\"}}\n\n";
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

        let events = sse_to_provider_events(stream).collect::<Vec<_>>().await;

        assert!(events.iter().any(|event| matches!(
            event,
            Ok(ProviderEvent::TextDelta { text }) if text == "€"
        )));
    }

    #[test]
    fn parse_thinking_delta_emits_thinking() {
        let frame = "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"pondering\"}}";
        let mut emitted_start = true;
        let mut tool_calls = Vec::new();
        let events = parse_sse_frame(frame, &mut emitted_start, &mut tool_calls);
        assert!(events.iter().any(|e| matches!(
            e,
            Ok(ProviderEvent::ThinkingDelta { text }) if text == "pondering"
        )));
    }

    #[test]
    fn parse_signature_delta_emits_thinking_signature() {
        let frame = "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"signature_delta\",\"signature\":\"abc123\"}}";
        let mut emitted_start = true;
        let mut tool_calls = Vec::new();
        let events = parse_sse_frame(frame, &mut emitted_start, &mut tool_calls);
        assert!(events.iter().any(|e| matches!(
            e,
            Ok(ProviderEvent::ThinkingSignature { signature }) if signature == "abc123"
        )));
    }

    /// Anthropic-compatible providers may ship the complete tool input in
    /// `content_block_start` (real Anthropic sends `"input": {}` there and
    /// streams the input via input_json_delta). It must not be dropped.
    #[test]
    fn captures_full_tool_input_from_content_block_start() {
        let mut emitted_start = true;
        let mut tool_calls = Vec::new();
        let start = "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"bash_exec\",\"input\":{\"command\":\"ls\"}}}";
        parse_sse_frame(start, &mut emitted_start, &mut tool_calls);
        let stop = "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}";
        let events = parse_sse_frame(stop, &mut emitted_start, &mut tool_calls);
        assert!(events.iter().any(|e| matches!(
            e,
            Ok(ProviderEvent::ToolCallReady { tool_call })
                if tool_call.params == json!({"command": "ls"})
        )));
    }

    /// Real Anthropic opens tool_use blocks with `"input": {}` — that empty
    /// object must not clobber the input streamed via input_json_delta.
    #[test]
    fn streamed_input_deltas_still_accumulate_after_empty_start_input() {
        let mut emitted_start = true;
        let mut tool_calls = Vec::new();
        let start = "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"bash_exec\",\"input\":{}}}";
        parse_sse_frame(start, &mut emitted_start, &mut tool_calls);
        let delta = "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"command\\\":\\\"ls\\\"}\"}}";
        parse_sse_frame(delta, &mut emitted_start, &mut tool_calls);
        let stop = "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}";
        let events = parse_sse_frame(stop, &mut emitted_start, &mut tool_calls);
        assert!(events.iter().any(|e| matches!(
            e,
            Ok(ProviderEvent::ToolCallReady { tool_call })
                if tool_call.params == json!({"command": "ls"})
        )));
    }

    /// Malformed streamed input must surface as-is (wrapped under
    /// `invalid_json`), not silently degrade to `{}` — the user needs to
    /// see what the model actually sent.
    #[test]
    fn malformed_tool_input_is_preserved_in_params() {
        let mut emitted_start = true;
        let mut tool_calls = Vec::new();
        let start = "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"bash_exec\",\"input\":{}}}";
        parse_sse_frame(start, &mut emitted_start, &mut tool_calls);
        let delta = "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"command\\\": oops\"}}";
        parse_sse_frame(delta, &mut emitted_start, &mut tool_calls);
        let stop = "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}";
        let events = parse_sse_frame(stop, &mut emitted_start, &mut tool_calls);
        assert!(events.iter().any(|e| matches!(
            e,
            Ok(ProviderEvent::ToolCallReady { tool_call })
                if tool_call.params == json!({"invalid_json": "{\"command\": oops"})
        )));
    }

    #[test]
    fn build_message_round_trips_signed_thinking_first() {
        let msg = ProviderInputMessage {
            role: MessageRole::Assistant,
            content: vec![
                ContentPart::Thinking {
                    text: "reasoning".into(),
                    signature: Some("sig-xyz".into()),
                },
                ContentPart::Text {
                    text: "answer".into(),
                },
                ContentPart::ToolUse {
                    tool_call_id: "toolu_1".into(),
                    tool_name: "bash".into(),
                    arguments: json!({}),
                },
            ],
        };

        let built = build_message(&msg, &HashMap::new()).unwrap();
        let content = built["content"].as_array().unwrap();
        // Thinking must come first (Anthropic requires it for tool-use turns).
        assert_eq!(content[0]["type"], "thinking");
        assert_eq!(content[0]["thinking"], "reasoning");
        assert_eq!(content[0]["signature"], "sig-xyz");
        assert_eq!(content[1]["type"], "text");
        assert_eq!(content[2]["type"], "tool_use");
    }

    #[test]
    fn build_message_drops_unsigned_thinking() {
        let msg = ProviderInputMessage {
            role: MessageRole::Assistant,
            content: vec![
                ContentPart::Thinking {
                    text: "reasoning".into(),
                    signature: None,
                },
                ContentPart::Text {
                    text: "answer".into(),
                },
            ],
        };

        let built = build_message(&msg, &HashMap::new()).unwrap();
        let content = built["content"].as_array().unwrap();
        // Unsigned thinking can't be re-signed → dropped; only text remains.
        assert_eq!(content.len(), 1);
        assert_eq!(content[0]["type"], "text");
    }
}
