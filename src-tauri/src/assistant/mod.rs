//! Assistant runtime modules.
//!
//! This subsystem owns the app-native assistant session model, provider
//! abstraction, event protocol, and persistence foundations.

pub mod auth;
pub mod compaction;
pub mod engine;
pub mod events;
pub mod image_store;
pub mod local_agent;
pub mod local_mcp;
pub mod providers;
pub mod repository;
pub mod runtime;
pub mod sandbox;
pub mod tools;
pub mod types;

#[cfg(test)]
mod repository_tests;
