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
    pub filesystem: super::filesystem::EffectiveFilesystemPolicy,
    pub network: SandboxNetworkMode,
    pub session_bus: SandboxSessionBusMode,
    pub env: SandboxEnv,
    /// Host directory backing the sandbox's temp space, from
    /// [`super::scratch`]. `None` means scratch space was unavailable and the
    /// backend must fall back to an ephemeral per-command temp dir.
    pub scratch_tmp: Option<PathBuf>,
    pub sockets: SocketPolicy,
    /// With [`SandboxSessionBusMode::Allow`]: the keyring-only proxy to bind
    /// in place of the session bus. `None` binds the bus itself.
    pub session_bus_proxy: Option<SessionBusProxy>,
    /// Why an agent set to keyring only gets no bus for this command, worded
    /// for the user. Keyring only never falls back to the whole bus.
    pub session_bus_unavailable: Option<String>,
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
impl SandboxProfile {
    /// Keyring only whose proxy cannot run: the command gets no bus at all.
    pub fn drop_session_bus(&mut self, reason: String) {
        self.session_bus = SandboxSessionBusMode::Deny;
        self.session_bus_proxy = None;
        self.session_bus_unavailable = Some(reason);
        self.env.block_session_bus();
    }
}

/// One line for the user and the agent: why a keyring-only agent has no
/// session bus here, and how to give it the whole bus.
pub fn keyring_only_unavailable_note(reason: &str) -> String {
    format!(
        "Keyring-only agents get no session bus here: {reason}. To give an agent the whole \
         session bus, set its Session bus to Full in its Local access settings."
    )
}

/// The socket of the filtering proxy CLAI runs for one command (see
/// `bus_proxy`), and the session bus socket it stands in for: the sandbox
/// sees the proxy at the bus path.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionBusProxy {
    pub socket: PathBuf,
    pub bus: PathBuf,
}

/// How the Linux sandbox confines Unix sockets: `clai-sandbox-helper` starts
/// every command and applies Landlock, so a command reaches only the sockets
/// CLAI binds on purpose, not any other socket a grant makes visible.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SocketPolicy {
    /// The helper as the sandbox's bwrap sees it; `None` when it is not
    /// installed here.
    pub helper: Option<PathBuf>,
    /// Whether the kernel lets the helper confine pathname sockets (Landlock
    /// ABI 9). Without it CLAI hides known socket directories instead.
    pub kernel_confines: bool,
    /// Host sockets the agent may connect to, its path grants on a socket
    /// (see `granted_sockets`): bound into the sandbox and allowed by
    /// Landlock.
    pub allowed: Vec<AllowedSocket>,
    /// Socket grants refused for this command, each with the reason, worded
    /// for the user.
    pub skipped: Vec<String>,
    /// Where on the host a failed command's output may name a socket the
    /// result points out; other paths are never looked up.
    pub hint_places: Vec<PathBuf>,
}

/// A host socket the agent may connect to, bound at `host`, where it
/// resolves, and at `path`, as its grant names it, unless the sandbox link
/// `/var/run` to `/run` already makes them one file.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AllowedSocket {
    pub path: PathBuf,
    pub host: PathBuf,
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
impl SocketPolicy {
    pub fn confined(&self) -> bool {
        self.helper.is_some() && self.kernel_confines
    }
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SandboxNetworkMode {
    Disabled,
    Host,
    /// Allowlist (Linux): an own network namespace whose only way out is
    /// CLAI's network proxy.
    Proxied(ProxyEndpoint),
}

/// Host files the sandbox binds for [`SandboxNetworkMode::Proxied`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyEndpoint {
    /// Where the command's network proxy session listens.
    pub socket: PathBuf,
    /// The sandbox helper binary, as the host's bwrap sees it.
    pub helper: PathBuf,
    /// The OpenSSH client configuration that sends ssh through the proxy.
    pub ssh_config: PathBuf,
    /// The ports of this computer the run may reach as the command starts:
    /// the helper answers on them inside the sandbox, through the proxy.
    pub forward_ports: Vec<u16>,
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

/// The session bus address of a command that gets no bus: a path the
/// sandbox never creates or binds, so clients fail at once. Left empty,
/// godbus (gh) autolaunches a private bus and then waits on
/// `org.freedesktop.secrets` with no timeout.
pub(crate) const SESSION_BUS_OFF_ADDRESS: &str = "unix:path=/run/clai/session-bus-off";

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

        let mut env = Self { vars: filtered };
        if !allow_session_bus {
            env.block_session_bus();
        }
        env
    }

    fn block_session_bus(&mut self) {
        self.vars
            .retain(|key, _| !ENV_SESSION_BUS_PASSTHROUGH.contains(&key.as_str()));
        self.vars.insert(
            "DBUS_SESSION_BUS_ADDRESS".to_string(),
            SESSION_BUS_OFF_ADDRESS.to_string(),
        );
    }

    /// Whether the command is given a session bus address to dial. An
    /// empty address, or one whose entries are all `autolaunch:`, names
    /// none: godbus autolaunches a private bus from `""` and `autolaunch:`.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub fn names_session_bus(&self) -> bool {
        self.vars
            .get("DBUS_SESSION_BUS_ADDRESS")
            .is_some_and(|address| {
                address
                    .split(';')
                    .any(|entry| !entry.is_empty() && !entry.starts_with("autolaunch:"))
            })
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
/// mask. The filesystem compiler resolves this configured container once for
/// both backends. Scratch allocation also uses the configured container.
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

#[cfg(all(test, target_family = "unix"))]
impl SandboxProfile {
    pub(crate) fn for_test(
        workspace_root: PathBuf,
        path_grants: Vec<SandboxPathGrant>,
        network: SandboxNetworkMode,
        session_bus: SandboxSessionBusMode,
        env: SandboxEnv,
        scratch_tmp: Option<PathBuf>,
    ) -> Self {
        let filesystem = super::filesystem::EffectiveFilesystemPolicy::compile(
            &workspace_root,
            &path_grants,
            env.home().map(Path::new),
        )
        .unwrap();
        Self {
            filesystem,
            network,
            session_bus,
            env,
            scratch_tmp,
            sockets: SocketPolicy::default(),
            session_bus_proxy: None,
            session_bus_unavailable: None,
        }
    }
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

        assert_eq!(env.vars.len(), 3);
        assert!(env.vars.contains_key("HOME"));
        assert!(env.vars.contains_key("PATH"));
        assert_eq!(
            env.vars.get("DBUS_SESSION_BUS_ADDRESS").map(String::as_str),
            Some(SESSION_BUS_OFF_ADDRESS)
        );

        // No host bus at all: the command still gets the off address, so
        // godbus does not autolaunch one.
        let env = SandboxEnv::filtered_from_iter(
            [("PATH", "/usr/bin")],
            home,
            SandboxSessionBusMode::Deny,
        );
        assert_eq!(
            env.vars.get("DBUS_SESSION_BUS_ADDRESS").map(String::as_str),
            Some(SESSION_BUS_OFF_ADDRESS)
        );
    }

    /// Keyring only whose proxy cannot start: the host bus variables go and
    /// the command gets the off address.
    #[cfg(target_family = "unix")]
    #[test]
    fn dropping_the_session_bus_leaves_the_off_address() {
        let workspace = tempfile::tempdir().unwrap();
        let env = SandboxEnv::filtered_from_iter(
            [
                ("DBUS_SESSION_BUS_ADDRESS", "unix:path=/run/user/1000/bus"),
                ("XDG_RUNTIME_DIR", "/run/user/1000"),
            ],
            workspace.path(),
            SandboxSessionBusMode::Allow,
        );
        let mut profile = SandboxProfile::for_test(
            workspace.path().to_path_buf(),
            Vec::new(),
            SandboxNetworkMode::Disabled,
            SandboxSessionBusMode::Allow,
            env,
            None,
        );
        profile.drop_session_bus("xdg-dbus-proxy is not installed".to_string());
        assert_eq!(profile.session_bus, SandboxSessionBusMode::Deny);
        assert_eq!(
            profile
                .env
                .vars
                .get("DBUS_SESSION_BUS_ADDRESS")
                .map(String::as_str),
            Some(SESSION_BUS_OFF_ADDRESS)
        );
        assert!(!profile.env.vars.contains_key("XDG_RUNTIME_DIR"));
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
