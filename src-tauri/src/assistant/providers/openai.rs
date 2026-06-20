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
        let api_key = get_api_key(connection)?;
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
            provider_id = %connection.provider_id,
            "Fetching models from provider"
        );

        let client = Client::new();
        let resp = client
            .get(&models_url)
            .header("Authorization", format!("Bearer {}", api_key))
            .send()
            .await
            .map_err(|e| {
                tracing::error!(url = %models_url, error = %e, "HTTP request failed");
                ProviderError::RequestFailed(e.to_string())
            })?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            tracing::error!(
                url = %models_url,
                status = %status,
                body = %body,
                "Provider returned error"
            );
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

        let client = Client::new();
        let resp = client
            .post(url)
            .header("Authorization", format!("Bearer {}", api_key))
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
        "stream_options": { "include_usage": true },
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

                // Extract usage if present
                if let Some(usage_obj) = json.get("usage") {
                    if !usage_obj.is_null() {
                        let usage = RunUsage {
                            input_tokens: usage_obj.get("prompt_tokens").and_then(|v| v.as_u64()),
                            output_tokens: usage_obj
                                .get("completion_tokens")
                                .and_then(|v| v.as_u64()),
                            reasoning_tokens: usage_obj
                                .get("completion_tokens_details")
                                .and_then(|d| d.get("reasoning_tokens"))
                                .and_then(|v| v.as_u64()),
                            total_tokens: usage_obj.get("total_tokens").and_then(|v| v.as_u64()),
                        };
                        events.push(Ok(ProviderEvent::Usage { usage }));
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
}
