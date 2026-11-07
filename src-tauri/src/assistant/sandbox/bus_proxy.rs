//! The keyring-only session bus. For each command CLAI runs `xdg-dbus-proxy`
//! outside the sandbox, forwarding only the Secret Service, and binds its
//! socket where the session bus is, so clients find it as usual.

use std::fs::File;
use std::io::Read;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::OnceLock;

use super::SessionBusProxy;

/// The one bus name keyring clients (libsecret, `gh`) talk to.
const KEYRING: &str = "org.freedesktop.secrets";
const READY_TIMEOUT_MS: i32 = 5_000;

/// `xdg-dbus-proxy` on `PATH`, looked up once.
fn binary() -> Option<&'static Path> {
    static BINARY: OnceLock<Option<PathBuf>> = OnceLock::new();
    BINARY
        .get_or_init(|| {
            let path = std::env::var_os("PATH")?;
            std::env::split_paths(&path)
                .map(|dir| dir.join("xdg-dbus-proxy"))
                .find(|candidate| super::helper::is_runnable(candidate))
        })
        .as_deref()
}

/// Why the bus cannot be filtered here, worded for the user; `None` when it
/// can. Under Flatpak bwrap runs on the host, where the proxy would have to
/// run too.
pub(crate) fn unavailable_reason() -> Option<&'static str> {
    if crate::providers::is_flatpak() {
        Some("the Flatpak build cannot filter the session bus yet")
    } else if binary().is_none() {
        Some("xdg-dbus-proxy is not installed")
    } else {
        None
    }
}

/// Where a keyring-only command's proxy listens and the bus it stands in
/// for. `Ok(None)` when there is no session bus to filter; an error, worded
/// for the user, when there is one but it cannot be filtered here.
pub(crate) fn plan(workspace_root: &Path) -> Result<Option<SessionBusProxy>, String> {
    let Some(bus) = super::linux_bwrap::resolve_session_bus_socket(false) else {
        return Ok(None);
    };
    if let Some(reason) = unavailable_reason() {
        return Err(reason.to_string());
    }
    let socket = super::helper::bus_socket(workspace_root)
        .ok_or("CLAI cannot make a private directory for the keyring proxy socket")?;
    Ok(Some(SessionBusProxy { bus, socket }))
}

/// A running proxy. Dropping it stops the proxy and removes its socket.
pub(crate) struct BusProxy {
    child: Child,
    socket: PathBuf,
    /// The read end of the pipe whose write end the proxy got with `--fd`:
    /// the proxy exits when it closes, so it never outlives CLAI either.
    alive: File,
}

impl BusProxy {
    /// Starts the proxy for `plan` and waits until it listens.
    pub(crate) fn start(plan: &SessionBusProxy) -> Result<Self, String> {
        let binary = binary().ok_or("xdg-dbus-proxy is not installed")?;
        let (alive, write) = pipe()?;
        let write_fd = write.as_raw_fd();
        let mut command = Command::new(binary);
        command
            .args(proxy_args(write_fd, plan))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        // SAFETY: fcntl is async-signal-safe. It clears close-on-exec on the
        // write end in the child only, so the proxy inherits it and no other
        // program CLAI starts does.
        unsafe {
            command.pre_exec(move || {
                if libc::fcntl(write_fd, libc::F_SETFD, 0) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = command
            .spawn()
            .map_err(|error| format!("cannot run {}: {error}", binary.display()))?;
        drop(write);
        let proxy = Self {
            child,
            socket: plan.socket.clone(),
            alive,
        };
        wait_ready(&proxy.alive, READY_TIMEOUT_MS)?;
        Ok(proxy)
    }
}

/// The proxy writes one byte to its `--fd` once it listens; it closes the
/// pipe instead if it fails. A signal does not cut the wait short.
fn wait_ready(alive: &File, timeout_ms: i32) -> Result<(), String> {
    let mut poll = libc::pollfd {
        fd: alive.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms as u64);
    let ready = loop {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        // SAFETY: one valid pollfd for the duration of the call.
        let ready = unsafe { libc::poll(&mut poll, 1, left.as_millis() as i32) };
        let interrupted = ready == -1
            && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted;
        if !interrupted || left.is_zero() {
            break ready;
        }
    };
    let mut byte = [0_u8; 1];
    if ready == 1 && (&*alive).read(&mut byte).is_ok_and(|read| read == 1) {
        Ok(())
    } else {
        Err("xdg-dbus-proxy did not start".to_string())
    }
}

impl Drop for BusProxy {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(&self.socket);
    }
}

/// The proxy's arguments: it reports on `write_fd`, and with `--filter`
/// forwards only calls to the keyring; everything else on the bus is hidden.
fn proxy_args(write_fd: i32, plan: &SessionBusProxy) -> Vec<std::ffi::OsString> {
    let mut address = std::ffi::OsString::from("unix:path=");
    address.push(&plan.bus);
    vec![
        format!("--fd={write_fd}").into(),
        address,
        plan.socket.clone().into(),
        "--filter".into(),
        format!("--talk={KEYRING}").into(),
    ]
}

fn pipe() -> Result<(File, OwnedFd), String> {
    let mut fds = [0; 2];
    // SAFETY: pipe2 fills both descriptors on success.
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } == -1 {
        return Err(format!("pipe: {}", std::io::Error::last_os_error()));
    }
    // SAFETY: two new descriptors nothing else owns.
    Ok(unsafe { (File::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
}

/// A private session bus for tests, with a process that owns the keyring
/// name and `org.example.Spawner` and answers `org.example.T.Ping` on `/t`
/// for both. Nothing outside the test talks to it.
#[cfg(test)]
pub(crate) mod test_bus {
    use std::io::{BufRead, BufReader};
    use std::path::{Path, PathBuf};
    use std::process::{Child, Command, Stdio};

    const OWNER: &str = r#"import sys
import gi
gi.require_version("Gio", "2.0")
from gi.repository import Gio, GLib
XML = "<node><interface name='org.example.T'><method name='Ping'><arg type='s' direction='out'/></method></interface></node>"
conn = Gio.DBusConnection.new_for_address_sync(sys.argv[1], Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT | Gio.DBusConnectionFlags.MESSAGE_BUS_CONNECTION, None, None)
def call(conn, sender, path, iface, method, params, invocation):
    invocation.return_value(GLib.Variant("(s)", ("pong",)))
conn.register_object("/t", Gio.DBusNodeInfo.new_for_xml(XML).interfaces[0], call, None, None)
for name in sys.argv[2:]:
    conn.call_sync("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "RequestName", GLib.Variant("(su)", (name, 4)), None, 0, -1, None)
print("ready", flush=True)
GLib.MainLoop().run()
"#;

    pub(crate) struct TestBus {
        pub(crate) socket: PathBuf,
        daemon: Child,
        owner: Child,
        _dir: tempfile::TempDir,
    }

    impl TestBus {
        /// `None`, with a note, where dbus-daemon, xdg-dbus-proxy or Python
        /// GObject is missing.
        pub(crate) fn start() -> Option<Self> {
            if super::unavailable_reason().is_some() {
                skip_without_bus_proxy("xdg-dbus-proxy is not available");
                return None;
            }
            let dir = tempfile::tempdir().unwrap();
            let socket = dir.path().join("bus");
            let address = format!("unix:path={}", socket.display());
            let Ok(mut daemon) = Command::new("dbus-daemon")
                .args(["--session", "--nofork", "--nopidfile", "--print-address"])
                .arg(format!("--address={address}"))
                .stdout(Stdio::piped())
                .spawn()
            else {
                skip_without_bus_proxy("dbus-daemon is not available");
                return None;
            };
            let mut line = String::new();
            BufReader::new(daemon.stdout.take().unwrap())
                .read_line(&mut line)
                .unwrap();
            let mut owner = Command::new("python3")
                .args([
                    "-I",
                    "-c",
                    OWNER,
                    &address,
                    super::KEYRING,
                    "org.example.Spawner",
                ])
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            line.clear();
            BufReader::new(owner.stdout.take().unwrap())
                .read_line(&mut line)
                .unwrap();
            let bus = Self {
                socket,
                daemon,
                owner,
                _dir: dir,
            };
            if line.trim() != "ready" {
                skip_without_bus_proxy("Python GObject is not available");
                return None;
            }
            Some(bus)
        }
    }

    /// A test that needs the bus proxy tools calls this where one is missing.
    /// It skips, unless `CLAI_REQUIRE_BUS_PROXY` is set, as in CI, which
    /// installs them.
    pub(crate) fn skip_without_bus_proxy(reason: &str) {
        assert!(
            std::env::var_os("CLAI_REQUIRE_BUS_PROXY").is_none(),
            "CLAI_REQUIRE_BUS_PROXY is set, but this test cannot run: {reason}"
        );
        eprintln!("skipped: {reason}");
    }

    impl Drop for TestBus {
        fn drop(&mut self) {
            for child in [&mut self.owner, &mut self.daemon] {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }

    /// `gdbus call` on `/t` of `name` through the bus at `socket`: its
    /// output on success, the error otherwise.
    pub(crate) fn ping(socket: &Path, name: &str) -> Result<String, String> {
        let output = Command::new("gdbus")
            .args(["call", "--session", "--dest", name, "--object-path", "/t"])
            .args(["--method", "org.example.T.Ping"])
            .env(
                "DBUS_SESSION_BUS_ADDRESS",
                format!("unix:path={}", socket.display()),
            )
            .output()
            .unwrap();
        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
        } else {
            Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_bus::{ping, TestBus};
    use super::*;

    #[test]
    fn the_proxy_forwards_only_the_keyring_and_stops_when_dropped() {
        let Some(bus) = TestBus::start() else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let plan = SessionBusProxy {
            socket: dir.path().join("proxy.sock"),
            bus: bus.socket.clone(),
        };
        let proxy = BusProxy::start(&plan).unwrap();
        assert_eq!(ping(&plan.socket, KEYRING), Ok("('pong',)".to_string()));
        let other = ping(&plan.socket, "org.example.Spawner").unwrap_err();
        assert!(other.contains("ServiceUnknown"), "{other}");
        assert_eq!(
            ping(&bus.socket, "org.example.Spawner"),
            Ok("('pong',)".to_string())
        );

        let pid = proxy.child.id();
        drop(proxy);
        assert!(!plan.socket.exists());
        assert!(!Path::new(&format!("/proc/{pid}")).exists());
    }

    #[test]
    fn a_signal_does_not_cut_the_ready_wait_short() {
        use std::io::Write;
        use std::os::unix::thread::JoinHandleExt;
        extern "C" fn ignore(_: libc::c_int) {}
        // SAFETY: a handler that does nothing, for a signal no other test uses.
        unsafe { libc::signal(libc::SIGUSR2, ignore as *const () as libc::sighandler_t) };
        let (alive, write) = pipe().unwrap();
        let waiter = std::thread::spawn(move || wait_ready(&alive, 5_000));
        std::thread::sleep(std::time::Duration::from_millis(100));
        // SAFETY: the thread is alive until it is joined below.
        unsafe { libc::pthread_kill(waiter.as_pthread_t(), libc::SIGUSR2) };
        std::thread::sleep(std::time::Duration::from_millis(100));
        File::from(write).write_all(&[1]).unwrap();
        assert_eq!(waiter.join().unwrap(), Ok(()));
    }

    #[test]
    fn the_proxy_filters_the_bus_down_to_the_keyring() {
        let plan = SessionBusProxy {
            socket: PathBuf::from("/c/.clai-net/bus-1.sock"),
            bus: PathBuf::from("/run/user/1000/bus"),
        };
        assert_eq!(
            proxy_args(7, &plan),
            [
                "--fd=7",
                "unix:path=/run/user/1000/bus",
                "/c/.clai-net/bus-1.sock",
                "--filter",
                "--talk=org.freedesktop.secrets",
            ]
        );
    }

    #[test]
    fn a_proxy_that_cannot_listen_fails_to_start() {
        if let Some(reason) = unavailable_reason() {
            super::test_bus::skip_without_bus_proxy(reason);
            return;
        }
        let plan = SessionBusProxy {
            socket: PathBuf::from("/nonexistent/dir/proxy.sock"),
            bus: PathBuf::from("/nonexistent/bus"),
        };
        assert_eq!(
            BusProxy::start(&plan).err().as_deref(),
            Some("xdg-dbus-proxy did not start")
        );
    }
}
