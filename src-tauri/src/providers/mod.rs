//! AI Provider detection and validation.
//!
//! This module handles detecting which AI CLI tools are available on the system
//! and validating that they can be executed.
//!
//! # Platform Support
//!
//! - **Linux/macOS**: Uses `which` to check command availability
//! - **Windows**: Uses `where` to check command availability
//! - **Flatpak**: Uses `flatpak-spawn --host` to access host binaries

use serde::{Deserialize, Serialize};
use std::process::Command;

use crate::config::AiProvider;

// =============================================================================
// Available Provider Info
// =============================================================================

/// Information about an available AI provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AvailableProvider {
    /// The provider type
    pub provider: AiProvider,

    /// Human-readable name
    pub name: String,

    /// CLI command
    pub command: String,

    /// Version string (if detected)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,

    /// Whether the provider is available and working
    pub available: bool,

    /// Error message if not available
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl AvailableProvider {
    /// Creates info for an available provider.
    fn available(provider: AiProvider, version: Option<String>) -> Self {
        Self {
            name: provider.display_name().to_string(),
            command: provider.command().to_string(),
            provider,
            version,
            available: true,
            error: None,
        }
    }

    /// Creates info for an unavailable provider.
    fn unavailable(provider: AiProvider, error: String) -> Self {
        Self {
            name: provider.display_name().to_string(),
            command: provider.command().to_string(),
            provider,
            version: None,
            available: false,
            error: Some(error),
        }
    }
}

// =============================================================================
// Detection
// =============================================================================

/// Checks if we're running inside a Flatpak sandbox.
pub fn is_flatpak() -> bool {
    std::path::Path::new("/.flatpak-info").exists()
}

/// Checks if we're running inside a Snap sandbox.
#[allow(dead_code)]
pub fn is_snap() -> bool {
    std::env::var("SNAP").is_ok()
}

/// Gets a command that can run on the host system.
///
/// In Flatpak, this wraps the command with `flatpak-spawn --host`.
pub(crate) fn get_host_command(cmd: &str) -> Command {
    if is_flatpak() {
        let mut command = Command::new("flatpak-spawn");
        command.arg("--host").arg(cmd);
        command
    } else {
        Command::new(cmd)
    }
}

/// Common user-local binary paths to search when command isn't in PATH.
/// These are relative to the user's home directory.
const USER_BIN_PATHS: &[&str] = &[
    ".local/bin",      // Standard XDG user binaries
    ".bun/bin",        // Bun global installs
    ".npm-global/bin", // npm global installs (custom prefix)
    ".npm/bin",        // npm global installs (alternative)
    ".cargo/bin",      // Rust/Cargo tools
    "go/bin",          // Go binaries
    ".deno/bin",       // Deno
];

/// Gets the user's **real** home directory, even from within Flatpak.
///
/// Inside Flatpak `$HOME` points at the sandboxed per-app home
/// (`~/.var/app/<id>`); this resolves the real host home via
/// `flatpak-spawn --host` so config that lives under `~` (e.g. `~/.clai`,
/// shared with the native install) is found at its real path. Callers
/// that hit this often should cache the result — it spawns a host process
/// under Flatpak. Falls back to `$HOME` if the host lookup fails.
pub fn get_home_dir() -> Option<String> {
    if is_flatpak() {
        // In Flatpak, HOME points to sandbox. Get real home from /etc/passwd via host
        let mut command = Command::new("flatpak-spawn");
        command.args(["--host", "sh", "-c", "echo $HOME"]);
        if let Ok(output) = command.output() {
            if output.status.success() {
                let home = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if !home.is_empty() {
                    return Some(home);
                }
            }
        }
    }
    std::env::var("HOME").ok()
}

/// Checks if a command exists at a specific path.
fn command_exists_at_path(path: &str) -> bool {
    if is_flatpak() {
        // Use flatpak-spawn to check file existence on host
        let mut command = Command::new("flatpak-spawn");
        command.args(["--host", "test", "-x", path]);
        command
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    } else {
        std::path::Path::new(path).is_file()
    }
}

/// Checks if a command exists on the system.
/// First checks PATH, then falls back to common user binary locations.
pub(crate) fn command_exists(cmd: &str) -> bool {
    #[cfg(target_os = "windows")]
    let check_cmd = "where";

    #[cfg(not(target_os = "windows"))]
    let check_cmd = "which";

    // First try the standard PATH lookup
    let mut command = get_host_command(check_cmd);
    command.arg(cmd);

    if let Ok(output) = command.output() {
        if output.status.success() {
            return true;
        }
    }

    // Fall back to checking common user binary paths
    #[cfg(not(target_os = "windows"))]
    if let Some(home) = get_home_dir() {
        for dir in USER_BIN_PATHS {
            let path = format!("{}/{}/{}", home, dir, cmd);
            if command_exists_at_path(&path) {
                return true;
            }
        }
    }

    false
}

/// Resolves a CLI command to an absolute path, searching PATH (the host PATH
/// under Flatpak, via `flatpak-spawn --host which`) and then the common user
/// bin dirs under the *real* home. Mirrors [`command_exists`] but returns the
/// resolved path.
///
/// Why this matters on the execution path: a GUI-launched app inherits the
/// desktop session's minimal PATH (not your login shell's), and a Flatpak's
/// PATH points at the runtime, not your host tools — so a bare `codex` often
/// won't resolve at spawn time even though it's installed. Resolving to an
/// absolute host path up front sidesteps both.
///
/// A value that already contains a path separator is returned unchanged (the
/// user gave an explicit path). Returns `None` if the command can't be found
/// anywhere, so callers fall back to the bare name and the usual "not found"
/// spawn error.
pub(crate) fn resolve_command_path(cmd: &str) -> Option<String> {
    if cmd.contains('/') {
        return Some(cmd.to_string());
    }

    #[cfg(target_os = "windows")]
    let which = "where";
    #[cfg(not(target_os = "windows"))]
    let which = "which";

    let mut command = get_host_command(which);
    command.arg(cmd);
    if let Ok(output) = command.output() {
        if output.status.success() {
            if let Some(path) = String::from_utf8_lossy(&output.stdout)
                .lines()
                .map(str::trim)
                .find(|line| !line.is_empty())
            {
                return Some(path.to_string());
            }
        }
    }

    #[cfg(not(target_os = "windows"))]
    if let Some(home) = get_home_dir() {
        for dir in USER_BIN_PATHS {
            let path = format!("{}/{}/{}", home, dir, cmd);
            if command_exists_at_path(&path) {
                return Some(path);
            }
        }
    }

    None
}

/// Builds a [`tokio::process::Command`] for running a host CLI binary.
///
/// Under Flatpak the binary lives on the **host** (the sandbox can't see host
/// system paths like `/usr/bin`), so we run it via `flatpak-spawn --host`.
/// Environment variables are injected with `--env=` because the host command
/// runs in the host's environment, not the sandbox's, so sandbox-side `.env()`
/// values wouldn't survive the hop. The working directory is set on the
/// wrapper process — `flatpak-spawn` forwards its cwd to the host command, and
/// the path is valid host-side because `--filesystem=home` maps the real home
/// at the same path inside the sandbox.
///
/// Outside Flatpak it's a plain `Command` with env/cwd applied directly.
/// Callers append the CLI's own arguments with `.arg(...)` afterwards — in
/// both modes those land as arguments to the target binary.
pub(crate) fn build_host_cli_command(
    binary: &str,
    envs: &[(&str, &str)],
    working_dir: Option<&std::path::Path>,
) -> tokio::process::Command {
    build_host_cli_command_impl(is_flatpak(), binary, envs, working_dir)
}

fn build_host_cli_command_impl(
    in_flatpak: bool,
    binary: &str,
    envs: &[(&str, &str)],
    working_dir: Option<&std::path::Path>,
) -> tokio::process::Command {
    let mut command = if in_flatpak {
        let mut command = tokio::process::Command::new("flatpak-spawn");
        command.arg("--host");
        for (key, value) in envs {
            command.arg(format!("--env={}={}", key, value));
        }
        command.arg(binary);
        command
    } else {
        let mut command = tokio::process::Command::new(binary);
        for (key, value) in envs {
            command.env(key, value);
        }
        command
    };
    if let Some(dir) = working_dir {
        command.current_dir(dir);
    }
    command
}

/// Gets the version of a command by running `<cmd> --version`.
/// Currently unused for performance reasons, but kept for future use.
#[allow(dead_code)]
fn get_command_version(cmd: &str) -> Option<String> {
    let mut command = get_host_command(cmd);
    command.arg("--version");

    match command.output() {
        Ok(output) if output.status.success() => {
            let version = String::from_utf8_lossy(&output.stdout);
            // Take first line, trim whitespace
            version.lines().next().map(|s| s.trim().to_string())
        }
        _ => None,
    }
}

/// Checks if a provider is available and can be executed.
///
/// Note: Version detection is skipped for performance reasons.
/// Running `<cmd> --version` can take 1-2 seconds per provider,
/// causing noticeable UI delays when opening settings.
pub fn check_provider(provider: &AiProvider) -> AvailableProvider {
    let cmd = provider.command();

    // Check if command exists (fast - just runs `which`)
    if !command_exists(cmd) {
        return AvailableProvider::unavailable(
            provider.clone(),
            format!(
                "Command '{}' not found. Please install {}.",
                cmd,
                provider.display_name()
            ),
        );
    }

    // Skip version detection for performance - spawning CLIs is slow
    AvailableProvider::available(provider.clone(), None)
}

/// First-run detection of the CLI providers clai supports. Returns the
/// `provider_id`s (matching `ProviderConnection.provider_id`) whose CLI is
/// present on the host so the caller can auto-register connections, removing
/// the need for the user to configure a provider manually before first use.
pub fn detect_supported_cli_providers() -> Vec<&'static str> {
    let mut found = Vec::new();
    if check_provider(&AiProvider::Claude { model: None }).available {
        found.push(crate::assistant::providers::cli::CLAUDE_CODE_PROVIDER_ID);
    }
    if check_provider(&AiProvider::Codex { model: None }).available {
        found.push(crate::assistant::providers::cli::CODEX_PROVIDER_ID);
    }
    if check_provider(&AiProvider::OpenCode { model: None }).available {
        found.push(crate::assistant::providers::cli::OPENCODE_PROVIDER_ID);
    }
    found
}

/// Gets all known providers with their availability status.
/// Uses parallel threads for faster detection.
pub fn get_available_providers() -> Vec<AvailableProvider> {
    use std::thread;

    let known_providers = vec![
        AiProvider::OpenCode { model: None },
        AiProvider::Claude { model: None },
        AiProvider::Gemini { model: None },
        AiProvider::Codex { model: None },
    ];

    // Check providers in parallel using threads
    let handles: Vec<_> = known_providers
        .into_iter()
        .map(|provider| thread::spawn(move || check_provider(&provider)))
        .collect();

    // Collect results
    handles
        .into_iter()
        .map(|h| {
            h.join().unwrap_or_else(|_| {
                AvailableProvider::unavailable(
                    AiProvider::Claude { model: None }, // fallback
                    "Failed to check provider".to_string(),
                )
            })
        })
        .collect()
}

/// Validates that a specific provider is available.
pub fn validate_provider(provider: &AiProvider) -> Result<AvailableProvider, String> {
    let info = check_provider(provider);
    if info.available {
        Ok(info)
    } else {
        Err(info
            .error
            .unwrap_or_else(|| "Provider not available".to_string()))
    }
}

// =============================================================================
// Model Information
// =============================================================================

/// Information about an available model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    /// Model identifier (what to pass to the CLI)
    pub id: String,

    /// Human-readable name
    pub name: String,

    /// Brief description
    pub description: String,

    /// Whether this is the recommended/default model
    #[serde(default)]
    pub recommended: bool,
}

/// Returns the available models for a provider.
///
/// These are hardcoded since the CLIs don't provide a way to list models dynamically.
/// Uses short aliases where possible for stability (avoids version-specific IDs).
pub fn get_models_for_provider(provider_type: &str) -> Vec<ModelInfo> {
    match provider_type {
        // OpenCode supports 75+ LLM providers via Models.dev
        // Model format: provider/model (e.g., "anthropic/claude-sonnet-4-5")
        // See: https://opencode.ai/docs/models/
        "opencode" => vec![
            ModelInfo {
                id: "anthropic/claude-sonnet-4-5".to_string(),
                name: "Claude Sonnet 4.5".to_string(),
                description: "Fast and capable, recommended for most tasks".to_string(),
                recommended: true,
            },
            ModelInfo {
                id: "anthropic/claude-opus-4".to_string(),
                name: "Claude Opus 4".to_string(),
                description: "Most powerful, best for complex reasoning".to_string(),
                recommended: false,
            },
            ModelInfo {
                id: "openai/gpt-4o".to_string(),
                name: "GPT-4o".to_string(),
                description: "OpenAI's most capable model".to_string(),
                recommended: false,
            },
            ModelInfo {
                id: "google/gemini-2.5-pro".to_string(),
                name: "Gemini 2.5 Pro".to_string(),
                description: "Google's most capable model".to_string(),
                recommended: false,
            },
        ],
        // Claude Code supports short aliases: sonnet, opus, haiku
        // These map to the latest version of each model family
        // See: https://code.claude.com/docs/en/model-config
        "claude" => vec![
            ModelInfo {
                id: "sonnet".to_string(),
                name: "Sonnet".to_string(),
                description: "Fast and capable, recommended for most tasks".to_string(),
                recommended: true,
            },
            ModelInfo {
                id: "opus".to_string(),
                name: "Opus".to_string(),
                description: "Most powerful, best for complex reasoning".to_string(),
                recommended: false,
            },
            ModelInfo {
                id: "haiku".to_string(),
                name: "Haiku".to_string(),
                description: "Fastest, good for simple tasks".to_string(),
                recommended: false,
            },
        ],
        // Gemini CLI uses model names like gemini-2.5-flash, gemini-2.5-pro
        // Set via GEMINI_MODEL environment variable
        "gemini" => vec![
            ModelInfo {
                id: "gemini-2.5-flash".to_string(),
                name: "Gemini 2.5 Flash".to_string(),
                description: "Fast and efficient, recommended for most tasks".to_string(),
                recommended: true,
            },
            ModelInfo {
                id: "gemini-2.5-pro".to_string(),
                name: "Gemini 2.5 Pro".to_string(),
                description: "Most capable, best for complex tasks".to_string(),
                recommended: false,
            },
        ],
        // Codex CLI uses --model flag with model names.
        "codex" => vec![
            ModelInfo {
                id: "gpt-5.5".to_string(),
                name: "GPT-5.5".to_string(),
                description: "Frontier model for complex coding tasks".to_string(),
                recommended: true,
            },
            ModelInfo {
                id: "gpt-5.4".to_string(),
                name: "GPT-5.4".to_string(),
                description: "Strong model for everyday coding".to_string(),
                recommended: false,
            },
            ModelInfo {
                id: "gpt-5.4-mini".to_string(),
                name: "GPT-5.4 Mini".to_string(),
                description: "Fast, cost-efficient coding model".to_string(),
                recommended: false,
            },
        ],
        _ => vec![],
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_flatpak() {
        // This test just verifies the function runs
        // In a real Flatpak environment, it would return true
        let _ = is_flatpak();
    }

    fn argv(command: &tokio::process::Command) -> Vec<String> {
        let std = command.as_std();
        let mut out = vec![std.get_program().to_string_lossy().into_owned()];
        out.extend(std.get_args().map(|a| a.to_string_lossy().into_owned()));
        out
    }

    #[test]
    fn host_cli_command_outside_flatpak_runs_binary_directly() {
        let command = build_host_cli_command_impl(
            false,
            "/usr/bin/codex",
            &[("MCP_TIMEOUT", "3600000")],
            Some(std::path::Path::new("/home/u/ws")),
        );
        // Program is the binary itself; env is set on the process (not argv).
        assert_eq!(argv(&command), vec!["/usr/bin/codex".to_string()]);
        let std = command.as_std();
        let env: Vec<_> = std
            .get_envs()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().into_owned(),
                    v.map(|v| v.to_string_lossy().into_owned()),
                )
            })
            .collect();
        assert!(env.contains(&("MCP_TIMEOUT".to_string(), Some("3600000".to_string()))));
        assert_eq!(
            std.get_current_dir(),
            Some(std::path::Path::new("/home/u/ws"))
        );
    }

    #[test]
    fn host_cli_command_in_flatpak_wraps_with_flatpak_spawn() {
        let command = build_host_cli_command_impl(
            true,
            "/usr/bin/codex",
            &[("CLAI_MCP_TOKEN", "tok"), ("MCP_TIMEOUT", "3600000")],
            Some(std::path::Path::new("/home/u/ws")),
        );
        // flatpak-spawn --host --env=... --env=... <binary>; env is injected as
        // argv flags (the host command runs in the host's env, so .env() on the
        // wrapper wouldn't reach it), and the binary is the last arg here so
        // caller-appended CLI args land after it.
        assert_eq!(
            argv(&command),
            vec![
                "flatpak-spawn".to_string(),
                "--host".to_string(),
                "--env=CLAI_MCP_TOKEN=tok".to_string(),
                "--env=MCP_TIMEOUT=3600000".to_string(),
                "/usr/bin/codex".to_string(),
            ]
        );
        // cwd is set on the wrapper; flatpak-spawn forwards it to the host cmd.
        assert_eq!(
            command.as_std().get_current_dir(),
            Some(std::path::Path::new("/home/u/ws"))
        );
    }

    #[test]
    fn host_cli_command_appends_caller_args_after_binary() {
        // Caller appends the CLI's own args; under Flatpak they must land after
        // the binary so they're args to the binary, not to flatpak-spawn.
        let mut command = build_host_cli_command_impl(true, "codex", &[], None);
        command.arg("exec").arg("-");
        assert_eq!(
            argv(&command),
            vec![
                "flatpak-spawn".to_string(),
                "--host".to_string(),
                "codex".to_string(),
                "exec".to_string(),
                "-".to_string(),
            ]
        );
    }

    #[test]
    fn resolve_command_path_passes_through_explicit_paths() {
        // A value with a separator is the user's explicit path — returned
        // unchanged, no PATH lookup.
        assert_eq!(
            resolve_command_path("/usr/bin/codex"),
            Some("/usr/bin/codex".to_string())
        );
        assert_eq!(
            resolve_command_path("./local/codex"),
            Some("./local/codex".to_string())
        );
    }

    #[test]
    fn test_available_provider_creation() {
        let provider = AiProvider::Claude { model: None };

        // Version is now optional (skipped for performance)
        let available = AvailableProvider::available(provider.clone(), None);
        assert!(available.available);
        assert_eq!(available.name, "Claude Code");
        assert_eq!(available.version, None);

        let unavailable = AvailableProvider::unavailable(provider, "Not found".to_string());
        assert!(!unavailable.available);
        assert_eq!(unavailable.error, Some("Not found".to_string()));
    }

    #[test]
    fn test_get_available_providers() {
        // This will check for real CLI tools on the system
        // In CI, they likely won't be installed, which is fine
        let providers = get_available_providers();

        // Should always return 4 providers (available or not)
        assert_eq!(providers.len(), 4);

        // Each should have a name and command
        for p in providers {
            assert!(!p.name.is_empty());
            assert!(!p.command.is_empty());
        }
    }

    #[test]
    fn test_ai_provider_methods() {
        assert_eq!(AiProvider::OpenCode { model: None }.command(), "opencode");
        assert_eq!(AiProvider::Claude { model: None }.command(), "claude");
        assert_eq!(AiProvider::Gemini { model: None }.command(), "gemini");
        assert_eq!(AiProvider::Codex { model: None }.command(), "codex");

        let custom = AiProvider::Custom {
            command: "my-ai".to_string(),
            args: vec![],
            model: None,
        };
        assert_eq!(custom.command(), "my-ai");
    }
}
