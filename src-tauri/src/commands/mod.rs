//! Tauri commands module.
//!
//! This module exposes Rust functionality to the JavaScript frontend.
//! Each submodule contains related commands grouped by functionality.

pub mod agents;
pub mod api;
pub mod assistant;
pub mod auth;
pub mod bridge;
pub mod fleet;
pub mod mcp_servers;
pub mod provider;
pub mod provider_connections;
pub mod skills;
pub mod workspace;
