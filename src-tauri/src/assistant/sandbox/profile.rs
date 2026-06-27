use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const DEFAULT_PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";

const ENV_ALLOW_EXACT: &[&str] = &["PATH", "LANG", "TZ", "TERM"];

const ENV_DENY_EXACT: &[&str] = &[
    "SSH_AUTH_SOCK",
    "SSH_AGENT_PID",
    "DBUS_SESSION_BUS_ADDRESS",
    "DBUS_SYSTEM_BUS_ADDRESS",
    "XDG_RUNTIME_DIR",
    "DOCKER_HOST",
    "CONTAINER_HOST",
    "PODMAN_HOST",
    "WAYLAND_DISPLAY",
    "DISPLAY",
    "XAUTHORITY",
    "GPG_AGENT_INFO",
    "GPG_TTY",
    "GIT_ASKPASS",
    "SSH_ASKPASS",
    "SUDO_ASKPASS",
];

// Fields are consumed by the Linux (bwrap) and macOS (seatbelt) sandbox
// backends; on platforms without a sandbox (e.g. Windows -> `unsupported.rs`)
// the profile is built but never read, so allow the resulting dead_code there.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct SandboxProfile {
    pub workspace_root: PathBuf,
    pub path_grants: Vec<SandboxPathGrant>,
    pub network: SandboxNetworkMode,
    pub session_bus: SandboxSessionBusMode,
    pub env: SandboxEnv,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxPathGrant {
    pub host_path: PathBuf,
    pub access: SandboxPathAccess,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxPathAccess {
    ReadOnly,
    ReadWrite,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxNetworkMode {
    Disabled,
    Host,
}

/// Whether the sandboxed process can reach the host's session D-Bus.
/// `Allow` mode binds `$XDG_RUNTIME_DIR/bus` and passes through
/// `DBUS_SESSION_BUS_ADDRESS` + `XDG_RUNTIME_DIR`. See
/// [`crate::config::SandboxSessionBusConfig`] for the user-facing config.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxSessionBusMode {
    Deny,
    Allow,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxEnv {
    vars: BTreeMap<String, String>,
}

/// Env keys we punch through the denylist when the session bus is allowed.
/// `DBUS_SESSION_BUS_ADDRESS` carries the bus's address (typically
/// `unix:path=$XDG_RUNTIME_DIR/bus`); `XDG_RUNTIME_DIR` is the fallback
/// some libraries use to construct that path when the address var is
/// unset. Both must reach the sandboxed process for libsecret-based
/// auth (gh, git-credential-libsecret, secret-tool) to work.
const ENV_SESSION_BUS_PASSTHROUGH: &[&str] = &["DBUS_SESSION_BUS_ADDRESS", "XDG_RUNTIME_DIR"];

impl SandboxEnv {
    pub fn filtered_from_current(home: &Path, session_bus: SandboxSessionBusMode) -> Self {
        Self::filtered_from_iter(std::env::vars(), home, session_bus)
    }

    pub(crate) fn filtered_from_iter<I, K, V>(
        vars: I,
        home: &Path,
        session_bus: SandboxSessionBusMode,
    ) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        let mut filtered = BTreeMap::new();
        let allow_session_bus = matches!(session_bus, SandboxSessionBusMode::Allow);
        for (key, value) in vars {
            let key = key.into();
            let session_bus_passthrough =
                allow_session_bus && ENV_SESSION_BUS_PASSTHROUGH.contains(&key.as_str());
            if is_denied_env_key(&key) && !session_bus_passthrough {
                continue;
            }
            if is_allowed_env_key(&key) || session_bus_passthrough {
                filtered.insert(key, value.into());
            }
        }

        filtered
            .entry("PATH".to_string())
            .or_insert_with(|| DEFAULT_PATH.to_string());
        filtered.insert("HOME".to_string(), home.display().to_string());

        Self { vars: filtered }
    }

    // Read only by the Unix sandbox backends when applying the env to the
    // sandboxed command; unused on platforms without a sandbox.
    #[allow(dead_code)]
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.vars
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
    }

    #[allow(dead_code)]
    pub fn home(&self) -> Option<&str> {
        self.vars.get("HOME").map(String::as_str)
    }
}

/// The directory that holds the agent's workspace — i.e. the workspace root's
/// parent, which is the container for *all* workspaces (e.g.
/// `~/.clai/workspaces`). We mask this container so an agent can't reach
/// sibling workspaces through a broad grant (like `$HOME`), while its own
/// workspace is re-exposed by the workspace bind/allow that runs after the
/// mask. Both the bwrap and seatbelt backends — and the in-process `fs_*`
/// path validator — derive the mask from here so the policy is identical.
///
/// Returns `None` (no masking) when:
/// - the workspace root has no parent (pathological), or
/// - `home` is unknown (fail open rather than over-restrict), or
/// - the container is not a strict descendant of `home`. This last guard
///   keeps the requirement "HOME access stays the same": if a workspace were
///   configured directly at `$HOME`, masking its parent would hide all of
///   home, so we decline to mask in that case.
pub fn workspace_mask(workspace_root: &Path, home: Option<&Path>) -> Option<PathBuf> {
    let parent = workspace_root.parent()?;
    let home = home?;
    if parent.starts_with(home) && parent != home {
        Some(parent.to_path_buf())
    } else {
        None
    }
}

fn is_allowed_env_key(key: &str) -> bool {
    ENV_ALLOW_EXACT.contains(&key) || key.starts_with("LC_")
}

fn is_denied_env_key(key: &str) -> bool {
    ENV_DENY_EXACT.contains(&key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_mask_is_the_container_when_under_home() {
        let home = Path::new("/home/u");
        let ws = Path::new("/home/u/.clai/workspaces/abc");
        assert_eq!(
            workspace_mask(ws, Some(home)),
            Some(PathBuf::from("/home/u/.clai/workspaces"))
        );
    }

    #[test]
    fn workspace_mask_declines_when_container_is_home_itself() {
        // Workspace configured directly at $HOME — masking its parent would
        // hide all of home, violating "HOME access stays the same".
        let home = Path::new("/home/u");
        let ws = Path::new("/home/u/abc");
        assert_eq!(workspace_mask(ws, Some(home)), None);
    }

    #[test]
    fn workspace_mask_fails_open_when_home_unknown() {
        let ws = Path::new("/home/u/.clai/workspaces/abc");
        assert_eq!(workspace_mask(ws, None), None);
    }

    #[test]
    fn workspace_mask_declines_outside_home() {
        let home = Path::new("/home/u");
        let ws = Path::new("/srv/workspaces/abc");
        assert_eq!(workspace_mask(ws, Some(home)), None);
    }

    #[test]
    fn env_filter_keeps_small_allowlist_and_sets_home() {
        let home = Path::new("/workspace");
        let env = SandboxEnv::filtered_from_iter(
            [
                ("PATH", "/usr/bin"),
                ("LANG", "en_US.UTF-8"),
                ("LC_ALL", "C"),
                ("TERM", "xterm-256color"),
                ("RANDOM_SECRET", "secret"),
            ],
            home,
            SandboxSessionBusMode::Deny,
        );
        let vars = env.vars;

        assert_eq!(vars.get("PATH"), Some(&"/usr/bin".to_string()));
        assert_eq!(vars.get("LANG"), Some(&"en_US.UTF-8".to_string()));
        assert_eq!(vars.get("LC_ALL"), Some(&"C".to_string()));
        assert_eq!(vars.get("TERM"), Some(&"xterm-256color".to_string()));
        assert_eq!(vars.get("HOME"), Some(&"/workspace".to_string()));
        assert!(!vars.contains_key("RANDOM_SECRET"));
    }

    #[test]
    fn env_filter_denies_socket_and_display_variables_even_if_added_later() {
        let home = Path::new("/workspace");
        let env = SandboxEnv::filtered_from_iter(
            [
                ("SSH_AUTH_SOCK", "/run/user/1000/ssh-agent"),
                ("DBUS_SESSION_BUS_ADDRESS", "unix:path=/run/user/1000/bus"),
                ("XDG_RUNTIME_DIR", "/run/user/1000"),
                ("DOCKER_HOST", "unix:///run/user/1000/docker.sock"),
                ("DISPLAY", ":0"),
                ("WAYLAND_DISPLAY", "wayland-1"),
                ("XAUTHORITY", "/home/user/.Xauthority"),
                ("GIT_ASKPASS", "/usr/bin/askpass"),
            ],
            home,
            SandboxSessionBusMode::Deny,
        );

        assert_eq!(env.vars.len(), 2);
        assert!(env.vars.contains_key("HOME"));
        assert!(env.vars.contains_key("PATH"));
    }

    #[test]
    fn env_filter_passes_session_bus_through_when_allowed_but_keeps_other_socket_denies() {
        // When session_bus = Allow we punch DBUS_SESSION_BUS_ADDRESS and
        // XDG_RUNTIME_DIR through the denylist. Everything else on the
        // denylist (ssh-agent, docker, x11, etc.) stays denied because
        // they are independent surface — opening session bus for libsecret
        // doesn't mean opening every other host service.
        let home = Path::new("/workspace");
        let env = SandboxEnv::filtered_from_iter(
            [
                ("SSH_AUTH_SOCK", "/run/user/1000/ssh-agent"),
                ("DBUS_SESSION_BUS_ADDRESS", "unix:path=/run/user/1000/bus"),
                (
                    "DBUS_SYSTEM_BUS_ADDRESS",
                    "unix:path=/run/dbus/system_bus_socket",
                ),
                ("XDG_RUNTIME_DIR", "/run/user/1000"),
                ("DOCKER_HOST", "unix:///run/user/1000/docker.sock"),
                ("DISPLAY", ":0"),
                ("WAYLAND_DISPLAY", "wayland-1"),
                ("XAUTHORITY", "/home/user/.Xauthority"),
            ],
            home,
            SandboxSessionBusMode::Allow,
        );

        assert_eq!(
            env.vars.get("DBUS_SESSION_BUS_ADDRESS"),
            Some(&"unix:path=/run/user/1000/bus".to_string())
        );
        assert_eq!(
            env.vars.get("XDG_RUNTIME_DIR"),
            Some(&"/run/user/1000".to_string())
        );
        // Other socket / display vars stay denied.
        assert!(!env.vars.contains_key("SSH_AUTH_SOCK"));
        assert!(!env.vars.contains_key("DBUS_SYSTEM_BUS_ADDRESS"));
        assert!(!env.vars.contains_key("DOCKER_HOST"));
        assert!(!env.vars.contains_key("DISPLAY"));
        assert!(!env.vars.contains_key("WAYLAND_DISPLAY"));
        assert!(!env.vars.contains_key("XAUTHORITY"));
    }
}
