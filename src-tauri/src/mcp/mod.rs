//! MCP (Model Context Protocol) plumbing.
//!
//! Hosts the external MCP client used to talk to configured remote MCP
//! servers (`client`) and the shared tool error types (`tools`). The
//! agent-facing tool surface itself lives in `crate::assistant::tools`.

pub mod client;
pub mod tools;
