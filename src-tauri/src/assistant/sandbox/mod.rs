//! OS-backed local execution sandboxing.

pub mod filesystem;
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
    SandboxEnv, SandboxNetworkMode, SandboxPathAccess, SandboxProfile, SandboxSessionBusMode,
};
pub use runner::{run_command, SandboxCommand, SandboxCommandOutput};

/// Persistent per-workspace temp space for the sandboxed backends, reset on
/// the workspace's first sandboxed command of the app session.
///
/// Keyed off the process `$HOME` (falling back to the workspace when it is
/// unset) because scratch lives in the app's own cache directory, not in the
/// policy's view of the user's home. `None` — the workspace container cannot
/// be masked, or the directory could not be created — degrades to the old
/// ephemeral behaviour rather than failing the command; scratch is an
/// optimisation. Platforms that run commands unsandboxed have nothing to bind
/// it into, so they never create a directory that could not be used.
pub(crate) fn session_scratch(workspace_root: &std::path::Path) -> Option<std::path::PathBuf> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        let home = std::env::var_os("HOME")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| workspace_root.to_path_buf());
        scratch::ensure_session_scratch(workspace_root, Some(&home))
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = workspace_root;
        None
    }
}

// Only the sandboxed backends and their tests name this type; Windows runs
// commands unsandboxed, so gate it the same way `macos_seatbelt` is gated.
#[cfg(any(target_os = "macos", all(test, target_family = "unix")))]
pub use profile::SandboxPathGrant;
