use serde_json::{json, Map, Value};

use super::events::AssistantUiEvent;
use super::types::{
    AssistantMessage, ContentPart, ToolCallStatus, ToolInvocation, ToolResultSummary,
};

fn clean_tool_name(name: &str) -> &str {
    if name.starts_with("mcp__") {
        return name.rsplit("__").next().unwrap_or(name);
    }
    if name.starts_with("mcp.") {
        return name.rsplit('.').next().unwrap_or(name);
    }
    name
}

fn payload_object(value: &Value) -> Option<Map<String, Value>> {
    match value {
        Value::Object(object) => {
            if let Some(parts) = object.get("content").and_then(Value::as_array) {
                let text = parts.iter().find_map(|part| {
                    (part.get("type")?.as_str()? == "text")
                        .then(|| part.get("text")?.as_str())
                        .flatten()
                });
                return text.and_then(|text| serde_json::from_str::<Map<String, Value>>(text).ok());
            }
            if let Some(text) = object.get("text").and_then(Value::as_str) {
                return serde_json::from_str(text).ok();
            }
            Some(object.clone())
        }
        Value::String(text) => serde_json::from_str(text).ok(),
        Value::Array(parts) => parts.iter().find_map(|part| {
            let text = part.get("text")?.as_str()?;
            serde_json::from_str(text).ok()
        }),
        _ => None,
    }
}

fn clipped(value: Option<&Value>) -> String {
    let text = value.and_then(Value::as_str).unwrap_or_default().trim();
    let mut chars = text.chars();
    let prefix: String = chars.by_ref().take(300).collect();
    if chars.next().is_some() {
        format!("{}…", prefix.trim_end())
    } else {
        prefix
    }
}

fn compact_result(tool_name: &str, result: &Value) -> Option<Value> {
    let object = payload_object(result)?;
    match clean_tool_name(tool_name) {
        "create_vega_chart" => {
            let path = object.get("path")?.as_str()?;
            if object.get("ok") != Some(&Value::Bool(true)) {
                return None;
            }
            Some(
                json!({"ok": true, "path": path, "display": object.get("display").and_then(Value::as_bool).unwrap_or(true)}),
            )
        }
        "workspace_assignTask" | "workspace_getTaskResult" => {
            if object.get("ok") != Some(&Value::Bool(true)) {
                return None;
            }
            let task = object.get("task")?.as_object()?;
            let mut compact = Map::new();
            for field in [
                "id",
                "title",
                "status",
                "assignedToWorkspaceAgentId",
                "assignedAgentDefinitionId",
            ] {
                if let Some(value) = task.get(field).and_then(Value::as_str) {
                    compact.insert(field.to_string(), Value::String(value.to_string()));
                }
            }
            for field in ["instructions", "error", "resultSummary"] {
                compact.insert(field.to_string(), Value::String(clipped(task.get(field))));
            }
            Some(json!({"ok": true, "task": compact}))
        }
        _ => None,
    }
}

fn summary(
    tool_name: &str,
    result: Option<&Value>,
    error: Option<&str>,
) -> Option<ToolResultSummary> {
    let name = clean_tool_name(tool_name);
    let object = result.and_then(payload_object);
    let field = |key: &str| object.as_ref().and_then(|object| object.get(key));
    if name == "bash_exec" {
        if let Some(code) = field("exitCode").and_then(Value::as_i64) {
            return Some(ToolResultSummary {
                text: format!("exit {code}"),
                tone: if code == 0 { "neutral" } else { "error" }.into(),
            });
        }
    }
    if error.is_some() {
        return Some(ToolResultSummary {
            text: "error".into(),
            tone: "error".into(),
        });
    }
    let text = match name {
        "fs_read" => field("content").and_then(Value::as_str).map(|content| {
            let lines = content.trim_end_matches('\n').lines().count();
            format!("{lines} {}", if lines == 1 { "line" } else { "lines" })
        }),
        "fs_list" | "fs_glob" | "web_search" => {
            let (key, singular, plural) = match name {
                "fs_list" => ("entries", "entry", "entries"),
                "fs_glob" => ("matches", "match", "matches"),
                _ => ("results", "result", "results"),
            };
            field(key).and_then(Value::as_array).map(|items| {
                format!(
                    "{} {}",
                    items.len(),
                    if items.len() == 1 { singular } else { plural }
                )
            })
        }
        "fs_write" => Some("written".into()),
        "web_fetch" => Some("fetched".into()),
        "ask_user" => field("answer")
            .and_then(Value::as_str)
            .map(|_| "answered".into()),
        "create_vega_chart" => field("path").and_then(Value::as_str).map(str::to_string),
        _ => None,
    }?;
    Some(ToolResultSummary {
        text,
        tone: "neutral".into(),
    })
}

pub fn tool_call(mut call: ToolInvocation) -> ToolInvocation {
    call.result_summary = summary(&call.tool_name, call.result.as_ref(), call.error.as_deref());
    call.has_full_result = call.result.is_some() || call.status == ToolCallStatus::Failed;
    call.result = call
        .result
        .as_ref()
        .and_then(|result| compact_result(&call.tool_name, result));
    call
}

pub fn message(mut message: AssistantMessage) -> AssistantMessage {
    for part in &mut message.content {
        if let ContentPart::ToolResult { payload, .. } = part {
            *payload = Value::Null;
        }
    }
    message
}

pub fn event(event: AssistantUiEvent) -> AssistantUiEvent {
    match event {
        AssistantUiEvent::MessageCreated { message: item } => AssistantUiEvent::MessageCreated {
            message: message(item),
        },
        AssistantUiEvent::AssistantMessageUpdated { message: item } => {
            AssistantUiEvent::AssistantMessageUpdated {
                message: message(item),
            }
        }
        AssistantUiEvent::AssistantMessageCompleted { message: item } => {
            AssistantUiEvent::AssistantMessageCompleted {
                message: message(item),
            }
        }
        AssistantUiEvent::ToolCallStarted { tool_call: call } => {
            AssistantUiEvent::ToolCallStarted {
                tool_call: tool_call(call),
            }
        }
        AssistantUiEvent::ToolCallCompleted { tool_call: call } => {
            AssistantUiEvent::ToolCallCompleted {
                tool_call: tool_call(call),
            }
        }
        AssistantUiEvent::ToolCallFailed { tool_call: call } => AssistantUiEvent::ToolCallFailed {
            tool_call: tool_call(call),
        },
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(tool_name: &str, result: Value) -> ToolInvocation {
        ToolInvocation {
            id: "t".into(),
            run_id: "r".into(),
            session_id: "s".into(),
            tool_name: tool_name.into(),
            params: json!({}),
            status: ToolCallStatus::Completed,
            result: Some(result),
            result_summary: None,
            has_full_result: false,
            error: None,
            started_at: 0,
            completed_at: Some(1),
        }
    }

    #[test]
    fn ordinary_result_is_omitted_but_summary_remains() {
        let displayed = tool_call(call(
            "bash_exec",
            json!({"stdout": "private", "exitCode": 0}),
        ));
        assert!(displayed.result.is_none());
        assert!(displayed.has_full_result);
        assert_eq!(displayed.result_summary.unwrap().text, "exit 0");
    }

    #[test]
    fn chart_and_task_keep_only_card_fields() {
        let chart = tool_call(call(
            "mcp__clai__create_vega_chart",
            json!({"ok": true, "path": "charts/a.vl.json", "spec": "private"}),
        ));
        assert_eq!(
            chart.result.unwrap(),
            json!({"ok": true, "path": "charts/a.vl.json", "display": true})
        );
        let task = tool_call(call(
            "workspace_assignTask",
            json!({"ok": true, "task": {"id": "t", "title": "Title", "status": "running", "assignedToWorkspaceAgentId": "agent", "instructions": "brief", "secret": "private"}}),
        ));
        let compact = task.result.unwrap();
        assert_eq!(compact["task"]["id"], "t");
        assert!(compact["task"].get("secret").is_none());
    }

    #[test]
    fn tool_message_payload_is_removed_from_live_event() {
        use crate::assistant::types::MessageRole;
        let message = AssistantMessage {
            id: "m".into(),
            session_id: "s".into(),
            role: MessageRole::Tool,
            content: vec![ContentPart::ToolResult {
                tool_call_id: "t".into(),
                payload: json!({"private": "large result"}),
                started_at: Some(1),
                completed_at: Some(2),
            }],
            created_at: 2,
            provider_metadata: None,
        };
        let displayed = event(AssistantUiEvent::MessageCreated { message });
        let AssistantUiEvent::MessageCreated { message } = displayed else {
            panic!("wrong event")
        };
        assert!(
            matches!(&message.content[0], ContentPart::ToolResult { payload: Value::Null, tool_call_id, .. } if tool_call_id == "t")
        );
    }

    #[test]
    fn mcp_envelope_is_unwrapped_without_forwarding_text() {
        let call = tool_call(call(
            "bash_exec",
            json!({"content": [{"type": "text", "text": "{\"exitCode\":2,\"stdout\":\"private\"}"}]}),
        ));
        assert!(call.result.is_none());
        assert_eq!(call.result_summary.unwrap().text, "exit 2");
    }
}
