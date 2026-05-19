//! Automation infrastructure.
//!
//! This module provides the core infrastructure for scheduled automations
//! that run through the assistant engine with MCP-backed capabilities.
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────────┐
//! │                        AGENT SYSTEM                             │
//! │                                                                 │
//! │  ┌──────────────┐    ┌──────────────┐    ┌──────────────┐     │
//! │  │  Definition  │───▶│   Instance   │───▶│ Assistant    │     │
//! │  │  (template)  │    │  (runtime)   │    │  Runtime     │     │
//! │  └──────────────┘    └──────────────┘    └──────────────┘     │
//! │                              │                   │              │
//! │                              ▼                   ▼              │
//! │                      ┌──────────────┐    ┌──────────────┐     │
//! │                      │  Scheduler   │    │  JS Bridge / │     │
//! │                      │ (when to run)│    │  MCP clients │     │
//! │                      └──────────────┘    └──────────────┘     │
//! └─────────────────────────────────────────────────────────────────┘
//! ```
//!
//! # Components
//!
//! - **types** - Core type definitions (AgentDefinition, AgentInstance)
//! - **scheduler** - Manages when agents should run (one at a time)
//! - **runner** - Background loop that checks scheduler and runs agents
//! - **init** - Scheduler initialization and instance management
//! - **tmp** - Startup cleanup for ephemeral runtime files

pub mod init;
pub mod runner;
pub mod scheduler;
pub mod tmp;
pub mod types;

// Re-export main types for convenience
pub use types::AgentDefinition;

// Scheduler exports (used by lib.rs and runner)
pub use scheduler::{create_shared_scheduler, SchedulerState, SharedScheduler};

// Runner export (used by lib.rs)
pub use runner::start_agent_runner;

// Startup cleanup helpers
pub use tmp::clear_tmp_dir;
