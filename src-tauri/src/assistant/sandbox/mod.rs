//! OS-backed local execution sandboxing.

pub mod profile;
pub mod runner;
// Only the sandboxed backends consume scratch space; compiling it on a
// platform that runs commands unsandboxed would be dead code and trips
// `cargo clippy -- -D warnings` in CI.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub mod scratch;

#[cfg(target_os = "linux")]
mod linux_bwrap;
#[cfg(any(target_os = "macos", all(test, target_family = "unix")))]
mod macos_seatbelt;
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod unsupported;

pub use profile::{
    SandboxEnv, SandboxNetworkMode, SandboxPathAccess, SandboxPathGrant, SandboxProfile,
    SandboxSessionBusMode,
};
pub use runner::{run_command, SandboxCommand, SandboxCommandOutput};
