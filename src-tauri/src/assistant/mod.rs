//! Assistant runtime modules.
//!
//! This subsystem owns the app-native assistant session model, provider
//! abstraction, event protocol, and persistence foundations.

pub mod auth;
pub mod engine;
pub mod events;
pub mod providers;
pub mod repository;
pub mod runtime;
pub mod tools;
pub mod types;

#[cfg(test)]
mod repository_tests;
