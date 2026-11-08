//! Network controls shared by the sandboxed shell and the web tools.

pub mod addr;
// The proxy serves the Linux sandbox only: a Unix socket bound into the
// command's own network namespace.
#[cfg(target_os = "linux")]
pub mod connect;
pub mod default_lists;
#[cfg(target_os = "linux")]
pub mod http;
#[cfg(target_os = "linux")]
pub mod local_listener;
pub mod matcher;
#[cfg(target_os = "linux")]
pub mod proxy;
pub mod run_state;
pub mod spawn_roots;
