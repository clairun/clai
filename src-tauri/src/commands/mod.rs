//! Tauri commands module.
//!
//! This module exposes Rust functionality to the JavaScript frontend.
//! Each submodule contains related commands grouped by functionality.

pub mod agent_templates;
pub mod assistant;
pub mod mcp_servers;
pub mod path_grants;
pub mod permissions;
pub mod provider;
pub mod provider_connections;
pub mod skills;
pub mod workspace;
pub mod workspace_agents;
