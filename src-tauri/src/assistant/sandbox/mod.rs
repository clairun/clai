//! OS-backed local execution sandboxing.

pub mod profile;
pub mod runner;

#[cfg(target_os = "linux")]
mod linux_bwrap;
#[cfg(not(target_os = "linux"))]
mod unsupported;

pub use profile::{
    SandboxEnv, SandboxNetworkMode, SandboxPathAccess, SandboxPathGrant, SandboxProfile,
    SandboxSessionBusMode,
};
pub use runner::{run_command, SandboxCommand, SandboxCommandOutput};
