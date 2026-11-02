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

use super::catalog::{self, ModelsEndpointStyle};
use super::types::{ProviderAdapter, ProviderError};

pub const OPENAI_PROVIDER_ID: &str = "openai";

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
        // Quirk-as-data: route model listing per the catalog entry's declared
        // style — curated list only, or a dedicated models URL apart from the
        // chat base (e.g. MiniMax).
        if let Some(entry) = catalog::get_entry(&connection.provider_id) {
            match &entry.models_endpoint_style {
                ModelsEndpointStyle::None => return Ok(entry.curated_models.clone()),
                ModelsEndpointStyle::OpenAiCompatible { url } => {
                    return super::fetch_models_openai_compatible(
                        url,
                        &connection.provider_id,
                        api_key.as_deref(),
                        entry.capabilities.as_ref(),
                    )
                    .await;
                }
                ModelsEndpointStyle::Standard => {}
            }
        }
        let models_url = {
            let base = connection
                .base_url
                .as_deref()
                .unwrap_or("https://api.openai.com/v1")
                .trim_end_matches('/');
            if base.ends_with("/v1") {
                format!("{}/models", base)
            } else {
                format!("{}/v1/models", base)
            }
        };

        tracing::info!(
            url = %models_url,
            protocol_id = %connection.protocol_id,
            "Fetching models from provider"
        );

        let mut headers: Vec<(String, String)> = Vec::new();
        if let Some(key) = &api_key {
            headers.push(("Authorization".to_string(), format!("Bearer {}", key)));
        }
        headers.extend(catalog_extra_headers(connection));

        let wire = super::wire_log::Scope::begin(&super::wire_log::Request {
            kind: "models",
            provider_id: &connection.provider_id,
            protocol_id: &connection.protocol_id,
            model_id: None,
            run_id: None,
            session_id: None,
            method: "GET",
            url: &models_url,
            headers: &headers,
            body: &serde_json::Value::Null,
        });

        let text = super::get_logged(&wire, &models_url, &headers).await?;
        let body: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| ProviderError::RequestFailed(e.to_string()))?;

        let models = body["data"]
            .as_array()
            .unwrap_or(&vec![])
            .iter()
            .filter_map(|m| {
                let id = m["id"].as_str()?.to_string();
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
        let url = completions_url(connection);
        let body = build_request_body(&request);

        // One header list drives both the request and the wire log, so the
        // log cannot drift from what goes on the wire.
        let mut headers: Vec<(String, String)> =
            vec![("Content-Type".to_string(), "application/json".to_string())];
        if let Some(key) = &api_key {
            headers.push(("Authorization".to_string(), format!("Bearer {}", key)));
        }
        headers.extend(catalog_extra_headers(connection));

        let wire = super::wire_log::Scope::begin(&super::wire_log::Request {
            kind: "completion",
            provider_id: &connection.provider_id,
            protocol_id: &connection.protocol_id,
            model_id: Some(&request.model_id),
            // `stream_sessionless_completion` (compaction summaries) falls
            // through to this send, so the wire log covers both purposes.
            run_id: Some(&request.run_id),
            session_id: Some(&request.session_id),
            method: "POST",
            url: &url,
            headers: &headers,
            body: &body,
        });

        let client = Client::new();
        let mut builder = client.post(&url).json(&body);
        for (name, value) in &headers {
            builder = builder.header(name, value);
        }
        let resp = match builder.send().await {
            Ok(resp) => resp,
            Err(e) => {
                wire.transport_error(&e.to_string());
                return Err(ProviderError::RequestFailed(e.to_string()));
            }
        };

        wire.response(resp.status().as_u16(), resp.headers());

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            wire.body(&body);
            return Err(ProviderError::RequestFailed(format!(
                "HTTP {}: {}",
                status, body
            )));
        }

        let byte_stream = wire.wrap_stream(resp.bytes_stream());
        let event_stream = sse_to_provider_events(byte_stream);

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

fn completions_url(connection: &ProviderConnection) -> String {
    let base = connection
        .base_url
        .as_deref()
        .unwrap_or("https://api.openai.com/v1")
        .trim_end_matches('/');

    if base.ends_with("/v1") {
        format!("{}/chat/completions", base)
    } else {
        format!("{}/v1/chat/completions", base)
    }
}

// =============================================================================
// Request Building
// =============================================================================

fn build_request_body(request: &CompletionRequest) -> serde_json::Value {
    let messages: Vec<serde_json::Value> = request
        .messages
        .iter()
        .map(|m| build_message(m, &request.images))
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
    reason = "lint debt: 125 lines against a 100-line budget; split it, do not raise the budget"
)]
fn build_message(
    msg: &crate::assistant::types::ProviderInputMessage,
    images: &HashMap<String, ResolvedImage>,
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
            .map(|p| match p {
                ContentPart::ToolUse {
                    tool_call_id,
                    tool_name,
                    arguments,
                } => json!({
                    "id": tool_call_id,
                    "type": "function",
                    "function": {
                        "name": tool_name,
                        "arguments": arguments.to_string(),
                    }
                }),
                _ => json!(null),
            })
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
}

/// Full state carried through the SSE stream unfold.
struct SseState {
    stream: Pin<Box<dyn Stream<Item = reqwest::Result<Bytes>> + Send>>,
    buf: Vec<u8>,
    is_first: bool,
    tool_calls: Vec<PartialToolCall>,
}

fn sse_to_provider_events(
    byte_stream: impl Stream<Item = reqwest::Result<Bytes>> + Send + 'static,
) -> impl Stream<Item = Result<ProviderEvent, ProviderError>> + Send {
    let state = SseState {
        stream: byte_stream.boxed(),
        buf: Vec::new(),
        is_first: true,
        tool_calls: Vec::new(),
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

                let events = parse_sse_frame(&frame, &mut state.is_first, &mut state.tool_calls);
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
#[allow(
    clippy::cognitive_complexity,
    reason = "lint debt: cognitive complexity 27 against a budget of 25"
)]
fn parse_sse_frame(
    frame: &str,
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

                // Check finish_reason for tool_calls
                if let Some(reason) = choice
                    .and_then(|c| c.get("finish_reason"))
                    .and_then(|r| r.as_str())
                {
                    if reason == "tool_calls" {
                        flush_tool_calls(tool_calls_buf, &mut events);
                    }
                }

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

                        // Accumulate id
                        if let Some(id) = tc.get("id").and_then(|i| i.as_str()) {
                            tool_calls_buf[index].id = id.to_string();
                        }

                        // Accumulate function name
                        if let Some(name) = tc
                            .get("function")
                            .and_then(|f| f.get("name"))
                            .and_then(|n| n.as_str())
                        {
                            tool_calls_buf[index].name = name.to_string();
                        }

                        // Accumulate function arguments (streamed as partial JSON)
                        if let Some(args) = tc
                            .get("function")
                            .and_then(|f| f.get("arguments"))
                            .and_then(|a| a.as_str())
                        {
                            tool_calls_buf[index].arguments.push_str(args);
                        }
                    }
                }
            }
            Err(e) => {
                tracing::warn!("Failed to parse SSE JSON: {} (data: {})", e, data);
            }
        }
    }

    events
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

        let built = build_message(&msg, &HashMap::new());
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

        let built = build_message(&msg, &images);
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
        let built = build_message(&msg, &HashMap::new());
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

        let events = sse_to_provider_events(stream).collect::<Vec<_>>().await;

        assert!(matches!(
            events.first(),
            Some(Ok(ProviderEvent::MessageStart))
        ));
        assert!(events.iter().any(|event| matches!(
            event,
            Ok(ProviderEvent::TextDelta { text }) if text == "€"
        )));
    }
    fn wire_test_completion_request(
        run_id: &str,
        session_id: &str,
        model: &str,
    ) -> CompletionRequest {
        use crate::assistant::types::ToolDefinition;
        CompletionRequest {
            run_id: run_id.into(),
            session_id: session_id.into(),
            model_id: model.into(),
            messages: vec![ProviderInputMessage {
                role: MessageRole::User,
                content: vec![ContentPart::Text {
                    text: "pr\u{fae}ba full prompt".into(),
                }],
            }],
            tools: vec![ToolDefinition {
                name: "get_weather".into(),
                description: "Get weather".into(),
                input_schema: json!({"type": "object"}),
            }],
            temperature: None,
            max_output_tokens: None,
            images: Default::default(),
        }
    }

    /// Adapter-level proof of the opt-in wire log: a full streamed completion
    /// exchange (request body verbatim, redacted Authorization, response
    /// status, raw SSE chunks, end) plus the non-2xx error-body path.
    /// The std Mutex guard is held across awaits on purpose: these tests swap
    /// a process-global logger and must not interleave.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn wire_log_captures_full_completion_exchange() {
        use crate::assistant::providers::wire_log;
        use axum::{http::HeaderMap, routing::post, Router};

        let _guard = wire_log::TEST_LOCK.lock().expect("test lock poisoned");
        let dir = tempfile::tempdir().expect("tempdir");
        let log_path = dir.path().join("wire.log");
        wire_log::testing::install(&log_path);

        let seen_auth = std::sync::Arc::new(std::sync::Mutex::new(None));
        let captured_auth = seen_auth.clone();
        let sse_body =
            "data: {\"choices\":[{\"delta\":{\"content\":\"h\u{e9}llo\"}}]}\n\ndata: [DONE]\n\n";
        let router = Router::new().route(
            "/v1/chat/completions",
            post(move |headers: HeaderMap| {
                let captured_auth = captured_auth.clone();
                async move {
                    *captured_auth.lock().unwrap() = headers
                        .get("authorization")
                        .map(|value| value.to_str().unwrap().to_string());
                    axum::http::Response::builder()
                        .status(200)
                        .header("content-type", "text/event-stream")
                        .body(axum::body::Body::from(sse_body.to_string()))
                        .unwrap()
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });

        let mut connection = conn_with_brand("z-ai-glm");
        connection.base_url = Some(base_url.clone());
        connection.secret_ref = "provider-connection::wire-log-test".into();
        ProviderSecretStorage::set_secret(&connection.secret_ref, "sk-wire-secret").unwrap();

        let request = wire_test_completion_request("run-wire", "sess-wire", "glm-4.7");

        let events = futures::StreamExt::collect::<Vec<_>>(
            OpenAiAdapter
                .stream_completion(&connection, request)
                .await
                .expect("stream starts"),
        )
        .await;
        assert!(events.iter().any(|event| matches!(
            event,
            Ok(ProviderEvent::TextDelta { text }) if text == "h\u{e9}llo"
        )));

        // The Authorization header reached the server unredacted...
        assert_eq!(
            seen_auth.lock().unwrap().as_deref(),
            Some("Bearer sk-wire-secret")
        );

        wire_log::testing::disable();
        drop(_guard);

        // ...while the wire log holds everything else, credentials redacted.
        let file = std::fs::read_to_string(&log_path).expect("wire log written");
        let events: Vec<serde_json::Value> = file
            .lines()
            .map(|line| serde_json::from_str(line).expect("valid JSONL"))
            .collect();
        let request_event = events
            .iter()
            .find(|e| {
                e["event"] == "request" && e["url"] == format!("{base_url}/v1/chat/completions")
            })
            .expect("request event");
        assert_eq!(request_event["kind"], "completion");
        assert_eq!(request_event["model_id"], "glm-4.7");
        assert_eq!(request_event["run_id"], "run-wire");
        assert_eq!(request_event["session_id"], "sess-wire");
        assert_eq!(request_event["headers"]["Authorization"], "[REDACTED]");
        assert_eq!(request_event["headers"]["Content-Type"], "application/json");
        assert_eq!(request_event["body"]["model"], "glm-4.7");
        assert_eq!(request_event["body"]["stream"], true);
        assert_eq!(
            request_event["body"]["messages"][0]["content"],
            "pr\u{fae}ba full prompt"
        );
        assert_eq!(
            request_event["body"]["tools"][0]["function"]["name"],
            "get_weather"
        );

        let id = request_event["id"].as_str().expect("request id");
        let response = events
            .iter()
            .find(|e| e["event"] == "response" && e["id"] == id)
            .expect("response event");
        assert_eq!(response["status"], 200);

        let chunk_texts: String = events
            .iter()
            .filter(|e| e["event"] == "chunk" && e["id"] == id)
            .map(|e| e["payload"]["text"].as_str().unwrap_or(""))
            .collect();
        assert!(chunk_texts.contains("h\u{e9}llo"));
        assert!(chunk_texts.contains("[DONE]"));

        let end = events
            .iter()
            .find(|e| e["event"] == "end" && e["id"] == id)
            .expect("end event");
        assert!(end["chunks"].as_u64().expect("chunk count") >= 1);
        assert!(!events
            .iter()
            .any(|e| e["event"] == "cancelled" && e["id"] == id));
    }

    /// The non-2xx path: status plus the provider's error body, verbatim.
    /// Guard held across awaits on purpose (process-global logger).
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn wire_log_captures_http_error_body() {
        use crate::assistant::providers::wire_log;
        use axum::{http::StatusCode, routing::post, Router};

        let _guard = wire_log::TEST_LOCK.lock().expect("test lock poisoned");
        let dir = tempfile::tempdir().expect("tempdir");
        let log_path = dir.path().join("wire.log");
        wire_log::testing::install(&log_path);

        let router = Router::new().route(
            "/v1/chat/completions",
            post(|| async {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "{\"error\":{\"message\":\"minimax exploded\"}}",
                )
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });

        let mut connection = conn_with_brand("minimax");
        connection.base_url = Some(base_url.clone());
        connection.secret_ref = "provider-connection::wire-log-err-test".into();
        ProviderSecretStorage::set_secret(&connection.secret_ref, "sk-wire-secret").unwrap();

        let request = wire_test_completion_request("run-err", "sess-err", "abab6.5s-chat");

        let Err(error) = OpenAiAdapter.stream_completion(&connection, request).await else {
            panic!("non-2xx status must fail")
        };
        assert!(error.to_string().contains("HTTP 500"));

        wire_log::testing::disable();
        drop(_guard);

        let file = std::fs::read_to_string(&log_path).expect("wire log written");
        let events: Vec<serde_json::Value> = file
            .lines()
            .map(|line| serde_json::from_str(line).expect("valid JSONL"))
            .collect();
        let request_event = events
            .iter()
            .find(|e| e["event"] == "request" && e["kind"] == "completion")
            .expect("request event");
        let id = request_event["id"].as_str().expect("request id");
        let response = events
            .iter()
            .find(|e| e["event"] == "response" && e["id"] == id)
            .expect("response event");
        assert_eq!(response["status"], 500);
        let body = events
            .iter()
            .find(|e| e["event"] == "body" && e["id"] == id)
            .expect("error body logged");
        assert_eq!(
            body["text"],
            "{\"error\":{\"message\":\"minimax exploded\"}}"
        );
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
}
