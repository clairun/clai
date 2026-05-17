//! MCP (Model Context Protocol) plumbing.
//!
//! Hosts the frontend JS bridge (`bridge`), the external MCP client used to
//! talk to configured remote MCP servers (`client`), and the shared tool
//! error types (`tools`). The agent-facing tool surface itself lives in
//! `crate::assistant::tools`.

pub mod bridge;
pub mod client;
pub mod tools;

// Re-export types used by commands/bridge.rs
pub use bridge::{complete_pending_request, ToolResponse};
