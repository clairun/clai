use serde_json::{json, Map, Value};

use super::events::AssistantUiEvent;
use super::types::{
    AssistantMessage, AssistantMessagePage, ContentPart, ToolInvocation, ToolResultSummary,
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
            if let Some(structured) = object.get("structuredContent").and_then(Value::as_object) {
                return Some(structured.clone());
            }
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

fn compact_input(tool_name: &str, input: &Value) -> Value {
    let parsed = input
        .as_str()
        .and_then(|text| serde_json::from_str::<Value>(text).ok());
    let Some(object) = parsed.as_ref().unwrap_or(input).as_object() else {
        return Value::Null;
    };
    let preferred = match clean_tool_name(tool_name) {
        "fs_read" | "fs_write" | "fs_list" | "fs_request_grant" => "path",
        "fs_glob" => "pattern",
        "bash_exec" => "command",
        "web_search" => "query",
        "web_fetch" => "url",
        "ask_user" => "question",
        "create_vega_chart" => "title",
        _ => "",
    };
    let selected = object.get_key_value(preferred).or_else(|| {
        object.iter().find(|(_, value)| {
            matches!(value, Value::String(_) | Value::Number(_) | Value::Bool(_))
        })
    });
    let Some((key, value)) = selected else {
        return json!({});
    };
    let preview = match value {
        Value::String(_) => Value::String(clipped(Some(value))),
        other => other.clone(),
    };
    json!({key: preview})
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
    call.has_full_result = call.result.is_some();
    let compact_params = compact_input(&call.tool_name, &call.params);
    call.has_full_input = compact_params != call.params;
    call.params = compact_params;
    call.result = call
        .result
        .as_ref()
        .and_then(|result| compact_result(&call.tool_name, result));
    call
}

pub fn message(mut message: AssistantMessage) -> AssistantMessage {
    for part in &mut message.content {
        match part {
            ContentPart::ToolResult { payload, .. } => *payload = Value::Null,
            ContentPart::ToolUse {
                tool_name,
                arguments,
                ..
            } => *arguments = compact_input(tool_name, arguments),
            _ => {}
        }
    }
    message
}

pub fn page(mut page: AssistantMessagePage) -> AssistantMessagePage {
    page.messages = page.messages.into_iter().map(message).collect();
    page.tool_calls = page.tool_calls.into_iter().map(tool_call).collect();
    page
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
    use crate::assistant::types::ToolCallStatus;

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
            has_full_input: false,
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
    fn both_input_copies_are_compact_in_events_and_pages() {
        use crate::assistant::types::MessageRole;
        let large = "payload".repeat(10_000);
        let full = json!({"path": "report.md", "content": large});
        let message = AssistantMessage {
            id: "m".into(),
            session_id: "s".into(),
            role: MessageRole::Assistant,
            content: vec![ContentPart::ToolUse {
                tool_call_id: "t".into(),
                tool_name: "fs_write".into(),
                arguments: full.clone(),
            }],
            created_at: 1,
            provider_metadata: None,
        };
        let mut invocation = call("fs_write", Value::Null);
        invocation.params = full;
        let displayed_event = event(AssistantUiEvent::AssistantMessageUpdated {
            message: message.clone(),
        });
        let displayed_call = event(AssistantUiEvent::ToolCallStarted {
            tool_call: invocation.clone(),
        });
        let displayed_page = page(AssistantMessagePage {
            messages: vec![message],
            tool_calls: vec![invocation],
            next_cursor: None,
            has_more: false,
            total_count: 1,
        });
        let serialized =
            serde_json::to_string(&(displayed_event, displayed_call, &displayed_page)).unwrap();
        assert!(!serialized.contains("payloadpayload"));
        assert!(serialized.contains("report.md"));
        assert_eq!(
            displayed_page.tool_calls[0].params,
            json!({"path":"report.md"})
        );
        assert!(displayed_page.tool_calls[0].has_full_input);
        assert!(matches!(&displayed_page.messages[0].content[0],
            ContentPart::ToolUse { arguments, .. } if arguments == &json!({"path":"report.md"})));
    }

    #[test]
    fn has_full_input_only_when_projection_changes_params() {
        let cases = [
            (json!({}), json!({}), false),
            (
                json!({"path": "report.md"}),
                json!({"path": "report.md"}),
                false,
            ),
            (
                json!({"path": "report.md", "content": "short"}),
                json!({"path": "report.md"}),
                true,
            ),
            (json!(["report.md"]), Value::Null, true),
            (json!("report.md"), Value::Null, true),
            (
                json!("{\"path\":\"report.md\"}"),
                json!({"path": "report.md"}),
                true,
            ),
        ];
        for (params, expected, has_full_input) in cases {
            let mut invocation = call("fs_write", Value::Null);
            invocation.params = params;
            let displayed = tool_call(invocation);
            assert_eq!(displayed.params, expected);
            assert_eq!(displayed.has_full_input, has_full_input);
        }

        let long_path = "x".repeat(301);
        let mut invocation = call("fs_write", Value::Null);
        invocation.params = json!({"path": long_path});
        let displayed = tool_call(invocation);
        assert!(displayed.has_full_input);
        assert_eq!(
            displayed.params["path"].as_str().unwrap().chars().count(),
            301
        );
        assert!(displayed.params["path"].as_str().unwrap().ends_with('…'));
    }

    #[test]
    fn tool_use_without_matching_call_uses_the_same_input_projection() {
        use crate::assistant::types::MessageRole;
        let full = json!({"path": "report.md", "content": "private"});
        let displayed = page(AssistantMessagePage {
            messages: vec![AssistantMessage {
                id: "m".into(),
                session_id: "s".into(),
                role: MessageRole::Assistant,
                content: vec![ContentPart::ToolUse {
                    tool_call_id: "missing".into(),
                    tool_name: "fs_write".into(),
                    arguments: full,
                }],
                created_at: 1,
                provider_metadata: None,
            }],
            tool_calls: vec![],
            next_cursor: None,
            has_more: false,
            total_count: 1,
        });
        assert!(matches!(&displayed.messages[0].content[0],
            ContentPart::ToolUse { arguments, .. } if arguments == &json!({"path": "report.md"})));
    }

    #[test]
    fn filesystem_summaries_handle_bare_and_client_envelopes() {
        let cases = [
            ("fs_list", json!({"entries": [1, 2]}), "2 entries"),
            ("fs_read", json!({"content": "one\ntwo\n"}), "2 lines"),
            ("fs_glob", json!({"matches": ["a"]}), "1 match"),
        ];
        for (name, value, expected) in cases {
            let envelope = json!({"content": [{"type": "text", "text": value.to_string()}]});
            assert_eq!(
                tool_call(call(name, value)).result_summary.unwrap().text,
                expected
            );
            assert_eq!(
                tool_call(call(name, envelope)).result_summary.unwrap().text,
                expected
            );
        }
        let structured = json!({"content": [{"type":"image", "data":"large"}],
            "structuredContent": {"entries": [1]}});
        assert_eq!(
            tool_call(call("fs_list", structured))
                .result_summary
                .unwrap()
                .text,
            "1 entry"
        );
        let mut failed = call("fs_read", json!({"content": []}));
        failed.result = None;
        failed.error = Some("disk error".into());
        failed.status = ToolCallStatus::Failed;
        let displayed = tool_call(failed);
        assert_eq!(displayed.result_summary.unwrap().text, "error");
        assert!(!displayed.has_full_result);
        assert!(tool_call(call("fs_read", json!({"content": []})))
            .result_summary
            .is_none());
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
