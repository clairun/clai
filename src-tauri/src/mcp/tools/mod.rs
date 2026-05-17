//! MCP tool error types.
//!
//! The agent-facing tool surface lives in `assistant::tools`; this module
//! only retains the shared error enum that other MCP code paths reference.

use serde::{Deserialize, Serialize};

// =============================================================================
// Error Types
// =============================================================================

/// Errors that can occur during tool execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)] // Used by tool implementations called via MCP
pub enum ToolError {
    /// Invalid tool name format (expected namespace.method).
    InvalidToolName(String),

    /// Unknown namespace.
    UnknownNamespace(String),

    /// Unknown method within a namespace.
    UnknownMethod(String),

    /// Invalid parameters provided.
    InvalidParams(String),

    /// Tool execution failed.
    ExecutionFailed(String),

    /// Network or API error.
    ApiError(String),
}

impl std::fmt::Display for ToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ToolError::InvalidToolName(name) => {
                write!(
                    f,
                    "Invalid tool name '{}': expected 'namespace.method'",
                    name
                )
            }
            ToolError::UnknownNamespace(ns) => write!(f, "Unknown namespace: {}", ns),
            ToolError::UnknownMethod(method) => write!(f, "Unknown method: {}", method),
            ToolError::InvalidParams(msg) => write!(f, "Invalid parameters: {}", msg),
            ToolError::ExecutionFailed(msg) => write!(f, "Execution failed: {}", msg),
            ToolError::ApiError(msg) => write!(f, "API error: {}", msg),
        }
    }
}

impl std::error::Error for ToolError {}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tool_error_display() {
        let err = ToolError::InvalidToolName("foo".into());
        assert_eq!(
            format!("{}", err),
            "Invalid tool name 'foo': expected 'namespace.method'"
        );

        let err = ToolError::UnknownNamespace("unknown".into());
        assert_eq!(format!("{}", err), "Unknown namespace: unknown");

        let err = ToolError::UnknownMethod("netdata.foo".into());
        assert_eq!(format!("{}", err), "Unknown method: netdata.foo");

        let err = ToolError::InvalidParams("missing query".into());
        assert_eq!(format!("{}", err), "Invalid parameters: missing query");
    }
}
