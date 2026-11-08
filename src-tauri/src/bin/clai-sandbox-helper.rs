//! The helper that starts every command inside the Linux sandbox.
//!
//! It first confines Unix sockets with Landlock where the kernel can: the
//! command may connect only to the sockets named with `--allow-unix` and to
//! sockets created inside the sandbox, not to any other socket a grant makes
//! visible (a desktop input method, Docker, an ssh or gpg agent), nor to an
//! abstract socket of the host (X11, an abstract D-Bus), nor signal a process
//! outside. On an older kernel it runs the command unconfined.
//!
//! - `run [--allow-unix <path>]… -- <command>…` then runs the command in its
//!   place (Off and Host network).
//! - `bridge --socket <path> --http <port> [--forward <port>]… [--allow-unix
//!   <path>]… -- <command>…` (Allowlist, whose network namespace has only a
//!   loopback interface) listens on 127.0.0.1:<port>, where `HTTP(S)_PROXY`
//!   point, forwards each connection to CLAI's network proxy at the socket,
//!   runs the command and exits as it does. Each `--forward` port, a port of
//!   this computer the run may reach, also answers on 127.0.0.1 and [::1]:
//!   each connection there asks the proxy for a tunnel to that port.
//! - `connect --proxy <host:port> <host> <port>` is ssh's `ProxyCommand`: it
//!   opens a CONNECT tunnel through the proxy and pipes stdin and stdout
//!   through it.
//!
//! Standard library and libc only, without `clai_lib`: under Flatpak it runs
//! on the host, where the app's runtime libraries are not installed.

#[cfg(target_os = "linux")]
fn main() {
    std::process::exit(linux::main(std::env::args_os().skip(1).collect()));
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("clai-sandbox-helper: not supported on this platform");
    std::process::exit(2);
}

#[cfg(target_os = "linux")]
mod linux {
    use std::ffi::OsString;
    use std::fs::File;
    use std::io::{self, Read, Write};
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, Shutdown, SocketAddr, TcpListener, TcpStream};
    use std::os::fd::AsFd;
    use std::os::unix::net::UnixStream;
    use std::os::unix::process::{CommandExt, ExitStatusExt};
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::thread;

    const USAGE: &str = "usage: clai-sandbox-helper run [--allow-unix <path>]... [--host-abstract-unix] -- <command>...\n       clai-sandbox-helper bridge --socket <path> --http <port> [--forward <port>]... [--allow-unix <path>]... -- <command>...\n       clai-sandbox-helper connect --proxy <host:port> <host> <port>";
    const MAX_HEAD_BYTES: usize = 16 * 1024;
    /// What ssh reports for a ProxyCommand that could not connect.
    const CONNECT_FAILED: i32 = 255;

    #[derive(Debug, PartialEq, Eq)]
    pub(crate) enum Mode {
        Run {
            allow_unix: Vec<PathBuf>,
            /// Host network: host abstract sockets stay reachable, as every
            /// host service on localhost is.
            host_abstract_unix: bool,
            argv: Vec<OsString>,
        },
        Bridge {
            socket: PathBuf,
            http_port: u16,
            /// Ports of this computer that answer inside the sandbox.
            forward: Vec<u16>,
            allow_unix: Vec<PathBuf>,
            argv: Vec<OsString>,
        },
        Connect {
            proxy: String,
            host: String,
            port: u16,
        },
    }

    pub fn main(args: Vec<OsString>) -> i32 {
        let mode = match parse(args) {
            Ok(mode) => mode,
            Err(error) => {
                eprintln!("clai-sandbox-helper: {error}\n{USAGE}");
                return 2;
            }
        };
        if let Some(confinement) = confinement(&mode) {
            let Confinement {
                allowed,
                scope_abstract,
            } = confinement;
            if let Err(error) = landlock::confine_unix_sockets(&allowed, scope_abstract) {
                eprintln!("clai-sandbox-helper: cannot confine Unix sockets: {error}");
                return 125;
            }
        }
        match mode {
            Mode::Run { argv, .. } => run(&argv),
            Mode::Bridge {
                socket,
                http_port,
                forward,
                argv,
                ..
            } => bridge(socket, http_port, &forward, &argv),
            Mode::Connect { proxy, host, port } => connect(&proxy, &host, port),
        }
    }

    /// The Landlock domain a mode starts its command in.
    #[derive(Debug, PartialEq, Eq)]
    pub(crate) struct Confinement {
        /// The sockets made outside the sandbox the command may reach.
        allowed: Vec<PathBuf>,
        /// Whether host abstract sockets are refused too.
        scope_abstract: bool,
    }

    /// Every mode that starts a command confines it; `connect` starts none.
    pub(crate) fn confinement(mode: &Mode) -> Option<Confinement> {
        match mode {
            Mode::Run {
                allow_unix,
                host_abstract_unix,
                ..
            } => Some(Confinement {
                allowed: allow_unix.clone(),
                scope_abstract: !host_abstract_unix,
            }),
            Mode::Bridge {
                socket, allow_unix, ..
            } => Some(Confinement {
                allowed: std::iter::once(socket.clone())
                    .chain(allow_unix.iter().cloned())
                    .collect(),
                scope_abstract: true,
            }),
            Mode::Connect { .. } => None,
        }
    }

    pub(crate) fn parse(args: Vec<OsString>) -> Result<Mode, String> {
        let mut args = args.into_iter();
        match args.next().as_ref().and_then(|mode| mode.to_str()) {
            Some(name @ ("run" | "bridge")) => {
                let bridge = name == "bridge";
                let (mut socket, mut http_port, mut allow_unix) = (None, None, Vec::new());
                let mut host_abstract_unix = false;
                let mut forward = Vec::new();
                loop {
                    let arg = args
                        .next()
                        .ok_or_else(|| format!("{name} needs `--` and a command"))?;
                    match arg.to_str() {
                        Some("--socket") if bridge => {
                            socket = args.next().map(PathBuf::from);
                        }
                        Some("--http") if bridge => http_port = Some(port(args.next())?),
                        Some("--forward") if bridge => forward.push(port(args.next())?),
                        Some("--allow-unix") => allow_unix.push(
                            args.next()
                                .map(PathBuf::from)
                                .filter(|path| path.is_absolute())
                                .ok_or("--allow-unix needs an absolute path")?,
                        ),
                        Some("--host-abstract-unix") if !bridge => host_abstract_unix = true,
                        Some("--") => break,
                        _ => return Err(format!("unexpected argument {arg:?}")),
                    }
                }
                let argv: Vec<OsString> = args.collect();
                if argv.is_empty() {
                    return Err(format!("{name} needs a command after `--`"));
                }
                if !bridge {
                    return Ok(Mode::Run {
                        allow_unix,
                        host_abstract_unix,
                        argv,
                    });
                }
                Ok(Mode::Bridge {
                    socket: socket.ok_or("--socket needs a path")?,
                    http_port: http_port.ok_or("--http needs a port")?,
                    forward,
                    allow_unix,
                    argv,
                })
            }
            Some("connect") => {
                let rest: Vec<OsString> = args.collect();
                let [flag, proxy, host, target_port] = rest.as_slice() else {
                    return Err("connect needs --proxy <host:port> <host> <port>".to_string());
                };
                if flag != "--proxy" {
                    return Err(format!("unexpected argument {flag:?}"));
                }
                let text = |value: &OsString| {
                    value
                        .to_str()
                        .map(str::to_string)
                        .ok_or_else(|| format!("{value:?} is not UTF-8"))
                };
                Ok(Mode::Connect {
                    proxy: text(proxy)?,
                    host: text(host)?,
                    port: port(Some(target_port.clone()))?,
                })
            }
            _ => Err("the mode is `run`, `bridge` or `connect`".to_string()),
        }
    }

    fn port(value: Option<OsString>) -> Result<u16, String> {
        value
            .as_ref()
            .and_then(|value| value.to_str())
            .and_then(|value| value.parse::<u16>().ok())
            .filter(|port| *port != 0)
            .ok_or_else(|| format!("{value:?} is not a port"))
    }

    /// Runs the command in this process's place.
    fn run(argv: &[OsString]) -> i32 {
        let error = Command::new(&argv[0]).args(&argv[1..]).exec();
        eprintln!("clai-sandbox-helper: cannot run {:?}: {error}", argv[0]);
        if error.kind() == io::ErrorKind::NotFound {
            127
        } else {
            126
        }
    }

    fn bridge(socket: PathBuf, http_port: u16, forward: &[u16], argv: &[OsString]) -> i32 {
        let listener = match TcpListener::bind(("127.0.0.1", http_port)) {
            Ok(listener) => listener,
            Err(error) => {
                eprintln!("clai-sandbox-helper: cannot listen on 127.0.0.1:{http_port}: {error}");
                return 125;
            }
        };
        for &port in forward {
            if port == http_port {
                eprintln!(
                    "clai-sandbox-helper: localhost:{port} is CLAI's proxy port inside the sandbox and is not forwarded"
                );
                continue;
            }
            forward_local_port(&socket, port);
        }
        thread::spawn(move || {
            for client in listener.incoming().flatten() {
                let socket = socket.clone();
                thread::spawn(move || match UnixStream::connect(&socket) {
                    Ok(proxy) => pipe(client, proxy),
                    Err(error) => eprintln!("clai-sandbox-helper: network proxy: {error}"),
                });
            }
        });
        let status = match Command::new(&argv[0]).args(&argv[1..]).status() {
            Ok(status) => status,
            Err(error) => {
                eprintln!("clai-sandbox-helper: cannot run {:?}: {error}", argv[0]);
                return if error.kind() == io::ErrorKind::NotFound {
                    127
                } else {
                    126
                };
            }
        };
        match status.signal() {
            // Die the same way, so the caller reports the signal as it would
            // without the helper in between.
            Some(signal) => reraise(signal),
            None => status.code().unwrap_or(1),
        }
    }

    /// Listens on 127.0.0.1 and [::1] at `port` and, for each connection,
    /// asks the proxy at `socket` for a tunnel to `localhost:<port>`: the
    /// proxy decides whether the run may reach it. An address that cannot be
    /// bound is reported and skipped, so the command still runs; [::1] is
    /// skipped silently where the sandbox has no IPv6.
    fn forward_local_port(socket: &Path, port: u16) {
        for ip in [
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            IpAddr::V6(Ipv6Addr::LOCALHOST),
        ] {
            let listener = match TcpListener::bind((ip, port)) {
                Ok(listener) => listener,
                Err(error) if ip.is_ipv6() && error.kind() == io::ErrorKind::AddrNotAvailable => {
                    continue;
                }
                Err(error) => {
                    let address = SocketAddr::new(ip, port);
                    eprintln!("clai-sandbox-helper: cannot forward localhost:{port} at {address}: {error}");
                    continue;
                }
            };
            let socket = socket.to_path_buf();
            thread::spawn(move || {
                for client in listener.incoming().flatten() {
                    let socket = socket.clone();
                    thread::spawn(move || {
                        let tunnel = UnixStream::connect(&socket)
                            .map_err(|error| format!("network proxy: {error}"))
                            .and_then(|proxy| request_tunnel(proxy, "localhost", port));
                        match tunnel {
                            Ok(proxy) => pipe(client, proxy),
                            Err(error) => eprintln!("clai-sandbox-helper: {error}"),
                        }
                    });
                }
            });
        }
    }

    fn reraise(signal: i32) -> i32 {
        // SAFETY: restoring the default action and raising a signal have no
        // memory effects; for a fatal signal the process ends here.
        unsafe {
            libc::signal(signal, libc::SIG_DFL);
            libc::raise(signal);
        }
        128 + signal
    }

    /// Copies both ways until each side has finished sending.
    fn pipe(client: TcpStream, proxy: UnixStream) {
        let (Ok(client_reader), Ok(proxy_reader)) = (client.try_clone(), proxy.try_clone()) else {
            return;
        };
        thread::spawn(move || {
            relay(&mut &client_reader, &mut &proxy);
            let _ = proxy.shutdown(Shutdown::Write);
        });
        relay(&mut &proxy_reader, &mut &client);
        let _ = client.shutdown(Shutdown::Write);
    }

    /// Writes each chunk as soon as it is read. Not `io::copy`: on Linux it
    /// splices from the tunnel socket into ssh's stdout pipe, and part of a
    /// reply stayed in the socket until more data arrived, so ssh, which
    /// waits for each reply, stalled in its key exchange.
    fn relay(reader: &mut impl Read, writer: &mut impl Write) {
        let mut buffer = [0_u8; 16 * 1024];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => return,
                Ok(read) => {
                    if writer.write_all(&buffer[..read]).is_err() {
                        return;
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => return,
            }
        }
    }

    fn connect(proxy: &str, host: &str, port: u16) -> i32 {
        let tunnel = match open_tunnel(proxy, host, port) {
            Ok(tunnel) => tunnel,
            Err(error) => {
                eprintln!("clai-sandbox-helper: {error}");
                return CONNECT_FAILED;
            }
        };
        // Unbuffered copies of stdin and stdout: ssh needs every byte now.
        let (Ok(stdin), Ok(stdout), Ok(upstream)) = (
            io::stdin().as_fd().try_clone_to_owned(),
            io::stdout().as_fd().try_clone_to_owned(),
            tunnel.try_clone(),
        ) else {
            return CONNECT_FAILED;
        };
        thread::spawn(move || {
            relay(&mut File::from(stdin), &mut &upstream);
            let _ = upstream.shutdown(Shutdown::Write);
        });
        relay(&mut &tunnel, &mut File::from(stdout));
        0
    }

    /// Asks the proxy at `proxy` for a tunnel to `host:port`.
    pub(crate) fn open_tunnel(proxy: &str, host: &str, port: u16) -> Result<TcpStream, String> {
        let stream = TcpStream::connect(proxy)
            .map_err(|error| format!("cannot reach the network proxy at {proxy}: {error}"))?;
        request_tunnel(stream, host, port)
    }

    /// Asks the proxy at the other end of `stream` for a tunnel to
    /// `host:port`. A refusal comes back as the proxy's status line and the
    /// reason in its body.
    fn request_tunnel<S: Read + Write>(mut stream: S, host: &str, port: u16) -> Result<S, String> {
        let authority = if host.contains(':') {
            format!("[{host}]:{port}")
        } else {
            format!("{host}:{port}")
        };
        write!(
            stream,
            "CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n\r\n"
        )
        .map_err(|error| format!("network proxy: {error}"))?;
        let head = read_head(&mut stream)?;
        let status = head.lines().next().unwrap_or_default();
        if status.split(' ').nth(1) == Some("200") {
            return Ok(stream);
        }
        let mut body = String::new();
        let _ = stream.take(MAX_HEAD_BYTES as u64).read_to_string(&mut body);
        Err(format!(
            "network proxy refused {authority}: {status}\n{}",
            body.trim_end()
        ))
    }

    /// Reads one byte at a time so nothing past the head is consumed: those
    /// bytes belong to the tunnel.
    fn read_head(stream: &mut impl Read) -> Result<String, String> {
        let mut head = Vec::new();
        let mut byte = [0_u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            match stream.read(&mut byte) {
                Ok(1) if head.len() < MAX_HEAD_BYTES => head.push(byte[0]),
                Ok(1) => return Err("the network proxy's answer is too large".to_string()),
                _ => return Err("the network proxy closed the connection".to_string()),
            }
        }
        Ok(String::from_utf8_lossy(&head).into_owned())
    }

    /// Landlock through its system calls, as `linux/landlock.h` defines them.
    pub(crate) mod landlock {
        use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
        use std::os::unix::ffi::OsStrExt;
        use std::path::{Path, PathBuf};

        const CREATE_RULESET_VERSION: libc::c_uint = 1;
        const RULE_PATH_BENEATH: libc::c_int = 1;
        const ACCESS_FS_REFER: u64 = 1 << 13;
        /// ABI 9: connecting to a pathname socket created outside the domain.
        const ACCESS_FS_RESOLVE_UNIX: u64 = 1 << 16;
        /// ABI 6: abstract sockets and signals outside the domain.
        const SCOPE_ABSTRACT_UNIX_SOCKET: u64 = 1;
        const SCOPE_SIGNAL: u64 = 1 << 1;

        #[repr(C)]
        #[derive(Debug, PartialEq, Eq)]
        pub(crate) struct RulesetAttr {
            pub(crate) handled_access_fs: u64,
            pub(crate) handled_access_net: u64,
            pub(crate) scoped: u64,
        }

        #[repr(C, packed)]
        struct PathBeneathAttr {
            allowed_access: u64,
            parent_fd: i32,
        }

        /// The kernel's Landlock ABI version, 0 without Landlock.
        pub(crate) fn abi() -> i64 {
            // SAFETY: with no attribute and size 0 the call only reports the
            // version.
            let version = unsafe {
                libc::syscall(
                    libc::SYS_landlock_create_ruleset,
                    std::ptr::null::<RulesetAttr>(),
                    0_usize,
                    CREATE_RULESET_VERSION,
                )
            };
            version.max(0)
        }

        /// What a domain confines at `abi`, `None` when it confines nothing;
        /// host abstract sockets only with `scope_abstract`.
        /// Refer is handled with the socket right because a domain that
        /// handles a filesystem right refuses a rename or link across
        /// directories unless refer is allowed there; it is allowed everywhere.
        pub(crate) fn ruleset(abi: i64, scope_abstract: bool) -> Option<RulesetAttr> {
            let handled_access_fs = if abi >= 9 {
                ACCESS_FS_RESOLVE_UNIX | ACCESS_FS_REFER
            } else {
                0
            };
            let scoped = match (abi >= 6, scope_abstract) {
                (false, _) => 0,
                (true, true) => SCOPE_ABSTRACT_UNIX_SOCKET | SCOPE_SIGNAL,
                (true, false) => SCOPE_SIGNAL,
            };
            (handled_access_fs != 0 || scoped != 0).then_some(RulesetAttr {
                handled_access_fs,
                handled_access_net: 0,
                scoped,
            })
        }

        /// Confines this thread and everything it starts. An allowed path
        /// that cannot be opened is skipped: its socket is not there.
        pub(crate) fn confine_unix_sockets(
            allowed: &[PathBuf],
            scope_abstract: bool,
        ) -> Result<(), String> {
            let Some(attr) = ruleset(abi(), scope_abstract) else {
                return Ok(());
            };
            // SAFETY: `attr` is a ruleset attribute of the size passed.
            let fd = unsafe {
                libc::syscall(
                    libc::SYS_landlock_create_ruleset,
                    &attr as *const RulesetAttr,
                    std::mem::size_of::<RulesetAttr>(),
                    0_u32,
                )
            };
            if fd < 0 {
                return Err(format!(
                    "landlock_create_ruleset: {}",
                    std::io::Error::last_os_error()
                ));
            }
            // SAFETY: the kernel returned a new descriptor nothing else owns.
            let ruleset = unsafe { OwnedFd::from_raw_fd(fd as i32) };
            if attr.handled_access_fs != 0 {
                let root = open_path(Path::new("/")).map_err(|error| format!("/: {error}"))?;
                add_rule(&ruleset, &root, ACCESS_FS_REFER)?;
                for path in allowed {
                    if let Ok(socket) = open_path(path) {
                        add_rule(&ruleset, &socket, ACCESS_FS_RESOLVE_UNIX)?;
                    }
                }
            }
            // SAFETY: plain prctl and landlock calls on a descriptor we own.
            unsafe {
                if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
                    return Err(format!("no_new_privs: {}", std::io::Error::last_os_error()));
                }
                if libc::syscall(libc::SYS_landlock_restrict_self, ruleset.as_raw_fd(), 0_u32) != 0
                {
                    return Err(format!(
                        "landlock_restrict_self: {}",
                        std::io::Error::last_os_error()
                    ));
                }
            }
            Ok(())
        }

        fn open_path(path: &Path) -> std::io::Result<OwnedFd> {
            let path = std::ffi::CString::new(path.as_os_str().as_bytes())?;
            // SAFETY: `path` is a valid C string; the result is checked.
            let fd = unsafe { libc::open(path.as_ptr(), libc::O_PATH | libc::O_CLOEXEC) };
            if fd < 0 {
                return Err(std::io::Error::last_os_error());
            }
            // SAFETY: a new descriptor nothing else owns.
            Ok(unsafe { OwnedFd::from_raw_fd(fd) })
        }

        fn add_rule(ruleset: &OwnedFd, path: &OwnedFd, access: u64) -> Result<(), String> {
            let attr = PathBeneathAttr {
                allowed_access: access,
                parent_fd: path.as_raw_fd(),
            };
            // SAFETY: `attr` is a path-beneath attribute with an open descriptor.
            let added = unsafe {
                libc::syscall(
                    libc::SYS_landlock_add_rule,
                    ruleset.as_raw_fd(),
                    RULE_PATH_BENEATH,
                    &attr as *const PathBeneathAttr,
                    0_u32,
                )
            };
            if added != 0 {
                return Err(format!(
                    "landlock_add_rule: {}",
                    std::io::Error::last_os_error()
                ));
            }
            Ok(())
        }

        #[cfg(test)]
        mod tests {
            use super::*;
            use std::os::linux::net::SocketAddrExt;
            use std::os::unix::net::{SocketAddr, UnixListener, UnixStream};

            #[test]
            fn what_each_abi_confines() {
                assert_eq!(ruleset(0, true), None);
                assert_eq!(ruleset(5, true), None);
                assert_eq!(ruleset(5, false), None);
                let scopes = SCOPE_ABSTRACT_UNIX_SOCKET | SCOPE_SIGNAL;
                let fs = ACCESS_FS_RESOLVE_UNIX | ACCESS_FS_REFER;
                let confines = |abi, scope_abstract| {
                    ruleset(abi, scope_abstract).map(|r| (r.handled_access_fs, r.scoped))
                };
                assert_eq!(confines(6, true), Some((0, scopes)));
                assert_eq!(confines(9, true), Some((fs, scopes)));
                // Host network: signals only, not host abstract sockets.
                assert_eq!(confines(6, false), Some((0, SCOPE_SIGNAL)));
                assert_eq!(confines(9, false), Some((fs, SCOPE_SIGNAL)));
                assert_eq!(
                    std::mem::size_of::<PathBeneathAttr>(),
                    12,
                    "packed as in the uapi"
                );
            }

            /// Sockets made outside the domain: only the allowed one connects;
            /// renames across directories still work.
            #[test]
            fn a_confined_thread_reaches_only_allowed_sockets() {
                let abi = abi();
                let required = std::env::var("CLAI_REQUIRE_LANDLOCK_ABI")
                    .ok()
                    .and_then(|value| value.parse::<i64>().ok())
                    .unwrap_or(0);
                // This test checks what ABI 6 and ABI 9 add.
                assert!(
                    abi >= required.min(9),
                    "CLAI_REQUIRE_LANDLOCK_ABI is {required}, but this kernel has Landlock ABI {abi}"
                );
                let dir = tempfile::tempdir().unwrap();
                let allowed = dir.path().join("allowed.sock");
                let other = dir.path().join("other.sock");
                let _allowed = UnixListener::bind(&allowed).unwrap();
                let _other = UnixListener::bind(&other).unwrap();
                let name = format!("clai-helper-test-{}", std::process::id());
                let address = SocketAddr::from_abstract_name(name.as_bytes()).unwrap();
                let _abstract = UnixListener::bind_addr(&address).unwrap();
                std::fs::create_dir(dir.path().join("a")).unwrap();
                std::fs::create_dir(dir.path().join("b")).unwrap();
                std::fs::write(dir.path().join("a/file"), "x").unwrap();
                let paths = (allowed.clone(), other.clone(), dir.path().to_path_buf());
                let results = std::thread::spawn(move || {
                    let (allowed, other, dir) = paths;
                    confine_unix_sockets(std::slice::from_ref(&allowed), true).unwrap();
                    (
                        UnixStream::connect(&allowed)
                            .map(|_| ())
                            .map_err(|e| e.kind()),
                        UnixStream::connect(&other)
                            .map(|_| ())
                            .map_err(|e| e.kind()),
                        UnixStream::connect_addr(&address)
                            .map(|_| ())
                            .map_err(|e| e.kind()),
                        std::fs::rename(dir.join("a/file"), dir.join("b/file")).is_ok(),
                    )
                })
                .join()
                .unwrap();
                let (to_allowed, to_other, to_abstract, renamed) = results;
                assert_eq!(to_allowed, Ok(()));
                assert!(renamed, "rename across directories");
                if abi >= 9 {
                    assert_eq!(to_other, Err(std::io::ErrorKind::PermissionDenied));
                } else {
                    eprintln!("Landlock ABI {abi}: pathname sockets are not confined here");
                }
                if abi >= 6 {
                    assert!(to_abstract.is_err(), "{to_abstract:?}");
                }
                // This thread was not confined.
                assert!(UnixStream::connect(&other).is_ok());
            }

            /// Host network: host abstract sockets (X11, for one) stay
            /// reachable, as every service on localhost does.
            #[test]
            fn a_host_network_thread_still_reaches_host_abstract_sockets() {
                let name = format!("clai-helper-host-{}", std::process::id());
                let address = SocketAddr::from_abstract_name(name.as_bytes()).unwrap();
                let _abstract = UnixListener::bind_addr(&address).unwrap();
                let reached = std::thread::spawn(move || {
                    confine_unix_sockets(&[], false).unwrap();
                    UnixStream::connect_addr(&address).map(|_| ())
                })
                .join()
                .unwrap();
                assert!(reached.is_ok(), "{reached:?}");
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn args(values: &[&str]) -> Vec<OsString> {
            values.iter().map(OsString::from).collect()
        }

        #[test]
        fn arguments_name_a_mode_and_its_parts() {
            assert_eq!(
                parse(args(&[
                    "bridge",
                    "--socket",
                    "/run/clai/net.sock",
                    "--http",
                    "3128",
                    "--",
                    "sh",
                    "-c",
                    "true"
                ])),
                Ok(Mode::Bridge {
                    socket: PathBuf::from("/run/clai/net.sock"),
                    http_port: 3128,
                    forward: Vec::new(),
                    allow_unix: Vec::new(),
                    argv: args(&["sh", "-c", "true"]),
                })
            );
            assert_eq!(
                parse(args(&[
                    "bridge",
                    "--socket",
                    "/s",
                    "--allow-unix",
                    "/run/clai/bus",
                    "--http",
                    "3128",
                    "--",
                    "sh"
                ])),
                Ok(Mode::Bridge {
                    socket: PathBuf::from("/s"),
                    http_port: 3128,
                    forward: Vec::new(),
                    allow_unix: vec![PathBuf::from("/run/clai/bus")],
                    argv: args(&["sh"]),
                })
            );
            assert_eq!(
                parse(args(&[
                    "run",
                    "--allow-unix",
                    "/a.sock",
                    "--allow-unix",
                    "/b.sock",
                    "--",
                    "sh",
                    "--"
                ])),
                Ok(Mode::Run {
                    allow_unix: vec![PathBuf::from("/a.sock"), PathBuf::from("/b.sock")],
                    host_abstract_unix: false,
                    argv: args(&["sh", "--"]),
                })
            );
            assert_eq!(
                parse(args(&[
                    "connect",
                    "--proxy",
                    "127.0.0.1:3128",
                    "github.com",
                    "22"
                ])),
                Ok(Mode::Connect {
                    proxy: "127.0.0.1:3128".to_string(),
                    host: "github.com".to_string(),
                    port: 22,
                })
            );
            for bad in [
                &[][..],
                &["serve"],
                &["run", "--"],
                &["run", "sh"],
                &["run", "--allow-unix", "relative.sock", "--", "sh"],
                &["run", "--socket", "/s", "--", "sh"],
                &["bridge", "--socket", "/s", "--http", "3128"],
                &["bridge", "--socket", "/s", "--http", "3128", "--"],
                &["bridge", "--http", "3128", "--", "sh"],
                &["bridge", "--socket", "/s", "--http", "0", "--", "sh"],
                &["bridge", "--socket", "/s", "--port", "1", "--", "sh"],
                &["connect", "--proxy", "127.0.0.1:3128", "github.com"],
                &["connect", "--via", "127.0.0.1:3128", "github.com", "22"],
                &["connect", "--proxy", "127.0.0.1:3128", "github.com", "ssh"],
            ] {
                assert!(parse(args(bad)).is_err(), "{bad:?}");
            }
        }

        #[test]
        fn every_mode_that_starts_a_command_confines_it() {
            let confined = |argv: &[&str]| confinement(&parse(args(argv)).unwrap());
            assert_eq!(
                confined(&["run", "--allow-unix", "/a.sock", "--", "sh"]),
                Some(Confinement {
                    allowed: vec![PathBuf::from("/a.sock")],
                    scope_abstract: true,
                })
            );
            assert_eq!(
                confined(&["run", "--host-abstract-unix", "--", "sh"]),
                Some(Confinement {
                    allowed: Vec::new(),
                    scope_abstract: false,
                })
            );
            let bridge = [
                "bridge",
                "--socket",
                "/run/clai/net.sock",
                "--http",
                "3128",
                "--allow-unix",
                "/b.sock",
                "--",
                "sh",
            ];
            assert_eq!(
                confined(&bridge),
                Some(Confinement {
                    allowed: vec![
                        PathBuf::from("/run/clai/net.sock"),
                        PathBuf::from("/b.sock")
                    ],
                    scope_abstract: true,
                })
            );
            assert_eq!(
                confined(&["connect", "--proxy", "127.0.0.1:3128", "github.com", "22"]),
                None
            );
        }

        #[test]
        fn only_bridge_forwards_local_ports() {
            let bridge = |extra: &[&str]| {
                let mut argv = vec!["bridge", "--socket", "/s", "--http", "3128"];
                argv.extend_from_slice(extra);
                argv.extend(["--", "sh"]);
                parse(args(&argv))
            };
            assert_eq!(
                bridge(&["--forward", "5432", "--forward", "6379"]),
                Ok(Mode::Bridge {
                    socket: PathBuf::from("/s"),
                    http_port: 3128,
                    forward: vec![5432, 6379],
                    allow_unix: Vec::new(),
                    argv: args(&["sh"]),
                })
            );
            assert!(bridge(&["--forward", "0"]).is_err());
            assert!(bridge(&["--forward"]).is_err());
            assert!(parse(args(&["run", "--forward", "5432", "--", "sh"])).is_err());
        }

        #[test]
        fn only_run_takes_host_abstract_sockets() {
            assert_eq!(
                parse(args(&["run", "--host-abstract-unix", "--", "sh"])),
                Ok(Mode::Run {
                    allow_unix: Vec::new(),
                    host_abstract_unix: true,
                    argv: args(&["sh"]),
                })
            );
            let bridge = [
                "bridge",
                "--socket",
                "/s",
                "--http",
                "3128",
                "--host-abstract-unix",
                "--",
                "sh",
            ];
            assert!(parse(args(&bridge)).is_err());
        }

        /// A proxy that answers one CONNECT with `answer`, then echoes.
        fn proxy(answer: &'static str) -> (String, thread::JoinHandle<String>) {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap().to_string();
            let handle = thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let head = read_head(&mut stream).unwrap();
                stream.write_all(answer.as_bytes()).unwrap();
                let mut echo = [0_u8; 4];
                if answer.contains(" 200 ") && stream.read_exact(&mut echo).is_ok() {
                    stream.write_all(&echo).unwrap();
                }
                head
            });
            (addr, handle)
        }

        #[test]
        fn a_tunnel_opens_on_200_and_carries_bytes() {
            let (addr, server) = proxy("HTTP/1.1 200 Connection established\r\n\r\n");
            let mut tunnel = open_tunnel(&addr, "2001:db8::1", 22).unwrap();
            tunnel.write_all(b"SSH-").unwrap();
            let mut echo = [0_u8; 4];
            tunnel.read_exact(&mut echo).unwrap();
            assert_eq!(&echo, b"SSH-");
            assert_eq!(
                server.join().unwrap(),
                "CONNECT [2001:db8::1]:22 HTTP/1.1\r\nHost: [2001:db8::1]:22\r\n\r\n"
            );
        }

        #[test]
        fn a_refusal_carries_the_proxy_reason() {
            let (addr, _server) = proxy(
                "HTTP/1.1 403 Forbidden\r\nConnection: close\r\n\r\n`example.org` is not on this agent's network allowlist.\n",
            );
            let error = open_tunnel(&addr, "example.org", 22).unwrap_err();
            assert!(error.contains("HTTP/1.1 403 Forbidden"), "{error}");
            assert!(error.contains("`example.org` is not on"), "{error}");
        }
    }
}
