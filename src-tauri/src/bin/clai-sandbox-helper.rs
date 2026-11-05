//! The helper that runs inside the Linux sandbox in the Allowlist network
//! mode.
//!
//! A command in Allowlist has its own network namespace with only a
//! loopback interface. CLAI binds its network proxy's Unix socket into the
//! sandbox, and this helper joins the two:
//!
//! - `bridge --socket <path> --http <port> -- <command>…` listens on
//!   127.0.0.1:<port>, where `HTTP(S)_PROXY` point, forwards each
//!   connection to the socket, runs the command and exits as it does.
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
    use std::net::{Shutdown, TcpListener, TcpStream};
    use std::os::fd::AsFd;
    use std::os::unix::net::UnixStream;
    use std::os::unix::process::ExitStatusExt;
    use std::path::PathBuf;
    use std::process::Command;
    use std::thread;

    const USAGE: &str = "usage: clai-sandbox-helper bridge --socket <path> --http <port> -- <command>...\n       clai-sandbox-helper connect --proxy <host:port> <host> <port>";
    const MAX_HEAD_BYTES: usize = 16 * 1024;
    /// What ssh reports for a ProxyCommand that could not connect.
    const CONNECT_FAILED: i32 = 255;

    #[derive(Debug, PartialEq, Eq)]
    pub(crate) enum Mode {
        Bridge {
            socket: PathBuf,
            http_port: u16,
            argv: Vec<OsString>,
        },
        Connect {
            proxy: String,
            host: String,
            port: u16,
        },
    }

    pub fn main(args: Vec<OsString>) -> i32 {
        match parse(args) {
            Ok(Mode::Bridge {
                socket,
                http_port,
                argv,
            }) => bridge(socket, http_port, &argv),
            Ok(Mode::Connect { proxy, host, port }) => connect(&proxy, &host, port),
            Err(error) => {
                eprintln!("clai-sandbox-helper: {error}\n{USAGE}");
                2
            }
        }
    }

    pub(crate) fn parse(args: Vec<OsString>) -> Result<Mode, String> {
        let mut args = args.into_iter();
        match args.next().as_ref().and_then(|mode| mode.to_str()) {
            Some("bridge") => {
                let (mut socket, mut http_port) = (None, None);
                loop {
                    let arg = args.next().ok_or("bridge needs `--` and a command")?;
                    match arg.to_str() {
                        Some("--socket") => {
                            socket = args.next().map(PathBuf::from);
                        }
                        Some("--http") => http_port = Some(port(args.next())?),
                        Some("--") => break,
                        _ => return Err(format!("unexpected argument {arg:?}")),
                    }
                }
                let argv: Vec<OsString> = args.collect();
                if argv.is_empty() {
                    return Err("bridge needs a command after `--`".to_string());
                }
                Ok(Mode::Bridge {
                    socket: socket.ok_or("--socket needs a path")?,
                    http_port: http_port.ok_or("--http needs a port")?,
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
            _ => Err("the mode is `bridge` or `connect`".to_string()),
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

    fn bridge(socket: PathBuf, http_port: u16, argv: &[OsString]) -> i32 {
        let listener = match TcpListener::bind(("127.0.0.1", http_port)) {
            Ok(listener) => listener,
            Err(error) => {
                eprintln!("clai-sandbox-helper: cannot listen on 127.0.0.1:{http_port}: {error}");
                return 125;
            }
        };
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
            let _ = io::copy(&mut &client_reader, &mut &proxy);
            let _ = proxy.shutdown(Shutdown::Write);
        });
        let _ = io::copy(&mut &proxy_reader, &mut &client);
        let _ = client.shutdown(Shutdown::Write);
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
            let _ = io::copy(&mut File::from(stdin), &mut &upstream);
            let _ = upstream.shutdown(Shutdown::Write);
        });
        let _ = io::copy(&mut &tunnel, &mut File::from(stdout));
        0
    }

    /// Asks the proxy for a tunnel to `host:port`. A refusal comes back as
    /// the proxy's status line and the reason in its body.
    pub(crate) fn open_tunnel(proxy: &str, host: &str, port: u16) -> Result<TcpStream, String> {
        let mut stream = TcpStream::connect(proxy)
            .map_err(|error| format!("cannot reach the network proxy at {proxy}: {error}"))?;
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
    fn read_head(stream: &mut TcpStream) -> Result<String, String> {
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
                    argv: args(&["sh", "-c", "true"]),
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
