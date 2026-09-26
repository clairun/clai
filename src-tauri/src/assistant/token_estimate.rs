use crate::assistant::types::{AssistantMessage, ContentPart, ToolDefinition};

const MESSAGE_FRAMING_TOKENS: usize = 4;
const IMAGE_TOKENS: usize = 1_600;

/// Token count of `text` under the fixed `cl100k_base` encoding, which serves
/// every provider: an estimate of the provider's count, not its tokenizer's
/// answer. The encoding is embedded in the crate and initialized once.
pub(crate) fn text_tokens(text: &str) -> usize {
    tiktoken_rs::cl100k_base_singleton().count_ordinary(text)
}

/// A message as the provider receives it: text and thinking, tool calls and
/// results at full size, a fixed allowance per image, plus framing.
pub(crate) fn message_tokens(message: &AssistantMessage) -> usize {
    message
        .content
        .iter()
        .map(|part| match part {
            ContentPart::Text { text } | ContentPart::Thinking { text, .. } => text_tokens(text),
            ContentPart::ToolUse {
                tool_name,
                arguments,
                ..
            } => text_tokens(tool_name) + text_tokens(&json_string(arguments)),
            ContentPart::ToolResult { payload, .. } => text_tokens(&json_string(payload)),
            ContentPart::Image { .. } => IMAGE_TOKENS,
        })
        .sum::<usize>()
        + MESSAGE_FRAMING_TOKENS
}

pub(crate) fn tool_tokens(tools: &[ToolDefinition]) -> usize {
    tools
        .iter()
        .map(|tool| {
            text_tokens(&tool.name)
                + text_tokens(&tool.description)
                + text_tokens(&json_string(&tool.input_schema))
        })
        .sum()
}

pub(crate) fn json_string(value: &serde_json::Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| value.to_string())
}
