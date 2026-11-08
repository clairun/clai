//! The CLAI network proxy for one sandboxed command in Allowlist mode.
//!
//! The command runs in its own network namespace; the sandbox helper
//! bridges its `HTTP(S)_PROXY` port to this session's Unix socket. Each
//! request is decided with the run's lists and the hosts the user allowed
//! for the run, logged in the run's host log, and either refused with a
//! 403 naming the host or connected through [`Connector`]. A port on this
//! computer (`localhost`, `127.0.0.1`, `[::1]`) is decided instead by
//! what listens there ([`ListenerInspector`]) and the local ports of the
//! run, and connected through [`LocalConnector`] only once allowed.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpStream, UnixListener};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

use super::connect::{ConnectError, Connector, LocalConnector};
use super::http::{parse_request, read_head, Request, HEAD_TIMEOUT};
use super::local_listener::{Listener, ListenerInspector, Loopback};
use super::matcher::{canonical_host, is_local_name, HostDecision, LOCALHOST};
use super::run_state::{LocalDecision, RefusedTargets, RunNetwork};

/// Open connections one command may hold through the proxy. Each holds
/// file descriptors in the CLAI process, so a command that opens many
/// cannot starve the rest of the app; the next one gets a 503.
const MAX_CONNECTIONS: usize = 128;

/// Listens while it lives; dropping it closes the listener, every tunnel
/// it opened, and removes the socket file.
pub struct ProxySession {
    socket: PathBuf,
    refused: Arc<Mutex<RefusedTargets>>,
    cancel: CancellationToken,
}

struct Shared {
    network: Arc<RunNetwork>,
    connector: Arc<dyn Connector>,
    inspector: Arc<dyn ListenerInspector>,
    refused: Arc<Mutex<RefusedTargets>>,
}

impl ProxySession {
    /// Binds `socket` (which must not exist) and serves it on the current
    /// tokio runtime.
    pub fn start(
        socket: &Path,
        network: Arc<RunNetwork>,
        connector: Arc<dyn Connector>,
        inspector: Arc<dyn ListenerInspector>,
    ) -> std::io::Result<Self> {
        Self::start_with_limit(socket, network, connector, inspector, MAX_CONNECTIONS)
    }

    fn start_with_limit(
        socket: &Path,
        network: Arc<RunNetwork>,
        connector: Arc<dyn Connector>,
        inspector: Arc<dyn ListenerInspector>,
        max_connections: usize,
    ) -> std::io::Result<Self> {
        let listener = UnixListener::bind(socket)?;
        let refused = Arc::new(Mutex::new(RefusedTargets::default()));
        let cancel = CancellationToken::new();
        let shared = Arc::new(Shared {
            network,
            connector,
            inspector,
            refused: Arc::clone(&refused),
        });
        let slots = Arc::new(Semaphore::new(max_connections));
        tokio::spawn(accept_loop(listener, shared, slots, cancel.clone()));
        Ok(Self {
            socket: socket.to_path_buf(),
            refused,
            cancel,
        })
    }

    /// The `host:port` targets this session refused.
    pub fn refused(&self) -> RefusedTargets {
        self.refused
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl Drop for ProxySession {
    fn drop(&mut self) {
        self.cancel.cancel();
        let _ = std::fs::remove_file(&self.socket);
    }
}

async fn accept_loop(
    listener: UnixListener,
    shared: Arc<Shared>,
    slots: Arc<Semaphore>,
    cancel: CancellationToken,
) {
    loop {
        let accepted = tokio::select! {
            () = cancel.cancelled() => return,
            accepted = listener.accept() => accepted,
        };
        match accepted {
            Ok((mut stream, _)) => {
                let Ok(slot) = Arc::clone(&slots).try_acquire_owned() else {
                    // Read the head first so the client gets the answer, not a
                    // reset; the bound keeps a silent client from stalling accept.
                    let _ = tokio::time::timeout(REFUSAL_TIMEOUT, async {
                        let _ = read_head(&mut stream).await;
                        respond(&mut stream, "503 Service Unavailable", TOO_MANY).await;
                    })
                    .await;
                    continue;
                };
                let shared = Arc::clone(&shared);
                let cancel = cancel.clone();
                tokio::spawn(async move {
                    let _slot = slot;
                    tokio::select! {
                        () = cancel.cancelled() => {}
                        () = serve(stream, &shared) => {}
                    }
                });
            }
            Err(error) => {
                tracing::warn!("Network proxy could not accept a connection: {error}");
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        }
    }
}

async fn serve<S: AsyncRead + AsyncWrite + Unpin>(mut client: S, shared: &Shared) {
    let (head, rest) = match tokio::time::timeout(HEAD_TIMEOUT, read_head(&mut client)).await {
        Ok(Ok(head)) => head,
        Ok(Err(reason)) => return respond(&mut client, "400 Bad Request", reason).await,
        Err(_) => return respond(&mut client, "408 Request Timeout", "no request").await,
    };
    let request = match parse_request(&head) {
        Ok(request) => request,
        Err(reason) => return respond(&mut client, "400 Bad Request", reason).await,
    };
    let (host, port) = request.target();
    let host = canonical_host(host).unwrap_or_else(|| host.to_string());
    let upstream = if is_local_name(&host) {
        local_upstream(&mut client, shared, &host, port).await
    } else {
        host_upstream(&mut client, shared, &host, port).await
    };
    let Some(mut upstream) = upstream else {
        return;
    };
    let opened = match &request {
        Request::Connect { .. } => client
            .write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
            .await
            .is_ok(),
        Request::Forward { head, .. } => upstream.write_all(head).await.is_ok(),
    };
    if opened && upstream.write_all(&rest).await.is_ok() {
        let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
    }
}

/// The connection to `host`, once the lists allow it; `None` once the
/// client has its answer.
async fn host_upstream<S: AsyncWrite + Unpin>(
    client: &mut S,
    shared: &Shared,
    host: &str,
    port: u16,
) -> Option<TcpStream> {
    let decision = shared.network.decide(host);
    shared
        .network
        .record(host, Some(port), decision == HostDecision::Allowed);
    if let Some(refusal) = decision.refusal(host, true) {
        shared.refuse(target_text(host, port));
        respond(client, "403 Forbidden", &refusal).await;
        return None;
    }
    match shared.connector.connect(host, port).await {
        Ok(upstream) => Some(upstream),
        Err(ConnectError::Forbidden) => {
            let reason = format!(
                "`{host}` resolves only to loopback, link-local or other local addresses, \
                 which the CLAI network proxy never connects to."
            );
            respond(client, "403 Forbidden", &reason).await;
            None
        }
        Err(ConnectError::Failed(error)) => {
            let reason = format!("Could not connect to {}: {error}", target_text(host, port));
            respond(client, "502 Bad Gateway", &reason).await;
            None
        }
    }
}

/// The connection to `port` on this computer, once the deny list and the
/// local ports of the run allow what listens there. Nothing is dialled
/// before that; `None` once the client has its answer.
async fn local_upstream<S: AsyncWrite + Unpin>(
    client: &mut S,
    shared: &Shared,
    host: &str,
    port: u16,
) -> Option<TcpStream> {
    let target = format!("{LOCALHOST}:{port}");
    if let Some((name, denied)) = shared.network.local_denial() {
        shared.network.record(LOCALHOST, Some(port), false);
        shared.refuse(target);
        let refusal = denied.refusal(name, false).unwrap_or_default();
        respond(client, "403 Forbidden", &refusal).await;
        return None;
    }
    let inspector = Arc::clone(&shared.inspector);
    let programs = shared.network.local_programs(port);
    let dials = dials(host);
    let found =
        tokio::task::spawn_blocking(move || find_listener(&*inspector, port, dials, &programs))
            .await
            .unwrap_or_else(|error| Err(io::Error::other(error)));
    let (dial, decision, served_by) = match found {
        Ok(Found::Confirmed(dial)) => (dial, LocalDecision::Allowed, String::new()),
        Ok(Found::Scanned(dial, listener)) => (
            dial,
            shared.network.decide_local(port, &listener),
            served_by(&listener),
        ),
        Err(error) => {
            let reason = format!(
                "CLAI could not check what listens on port {port} on this computer: {error}"
            );
            respond(client, "502 Bad Gateway", &reason).await;
            return None;
        }
    };
    let refusal = match decision {
        LocalDecision::Allowed => {
            shared.network.record(LOCALHOST, Some(port), true);
            return match LocalConnector.connect(dial, port).await {
                Ok(upstream) => Some(upstream),
                Err(error) => {
                    let reason = format!("Could not connect to {target}: {error}");
                    respond(client, "502 Bad Gateway", &reason).await;
                    None
                }
            };
        }
        LocalDecision::Nothing => {
            let reason = format!("Nothing is listening on port {port} on this computer.");
            respond(client, "502 Bad Gateway", &reason).await;
            return None;
        }
        LocalDecision::NotGranted => format!(
            "`{target}` (port {port} on this computer) is not in this agent's allowed \
             local ports. Call `net_request_access` with host `{target}` to ask the user."
        ),
        LocalDecision::Changed => format!(
            "Port {port} on this computer is now served by {served_by}, not by what the \
             user allowed. Call `net_request_access` with host `{target}` to ask again."
        ),
        LocalDecision::Clai => super::local_listener::served_by_clai(port),
    };
    shared.network.record(LOCALHOST, Some(port), false);
    shared.refuse(target);
    respond(client, "403 Forbidden", &refusal).await;
    None
}

/// The loopback addresses tried for a local name, in order: `localhost`
/// also reaches a listener on IPv6 only.
pub(crate) fn dials(host: &str) -> &'static [Loopback] {
    match host {
        "127.0.0.1" => &[Loopback::V4],
        "::1" => &[Loopback::V6],
        _ => &[Loopback::V4, Loopback::V6],
    }
}

enum Found {
    /// The program the last full scan found on the port still serves it,
    /// and the run allows that program.
    Confirmed(Loopback),
    Scanned(Loopback, Listener),
}

/// What listens on `port` at the first of `dials` where anything does.
/// The cheap check can only confirm one of `programs`, which the run
/// allows; anything else is decided from a full scan.
fn find_listener(
    inspector: &dyn ListenerInspector,
    port: u16,
    dials: &[Loopback],
    programs: &[String],
) -> io::Result<Found> {
    for &dial in dials {
        if inspector.still_serves(port, dial, programs) {
            return Ok(Found::Confirmed(dial));
        }
        let listener = inspector.inspect(port, dial)?;
        if listener != Listener::Nothing {
            return Ok(Found::Scanned(dial, listener));
        }
    }
    Ok(Found::Scanned(Loopback::V4, Listener::Nothing))
}

/// What the agent is told listens on a port the user allowed for something
/// else. Never the command line, which only the request card shows.
fn served_by(listener: &Listener) -> String {
    match listener {
        Listener::Program(program) => format!("`{}`", program.exe),
        Listener::OtherUser { name, .. } => format!("a program of user `{name}`"),
        _ => "a program CLAI cannot name".to_string(),
    }
}

impl Shared {
    fn refuse(&self, target: String) {
        self.refused
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .add(target);
    }
}

const TOO_MANY: &str = "This command has too many connections open through the CLAI network \
                        proxy; close some before opening more.";
const REFUSAL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);

fn target_text(host: &str, port: u16) -> String {
    if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

async fn respond<W: AsyncWrite + Unpin>(client: &mut W, status: &str, body: &str) {
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}\n",
        body.len() + 1
    );
    let _ = client.write_all(response.as_bytes()).await;
    let _ = client.shutdown().await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assistant::network::connect::HostConnector;
    use crate::assistant::network::local_listener::{Program, RunOnly};
    use crate::assistant::network::matcher::LOCAL_NAMES;
    use crate::assistant::network::run_state::{HostLogEntry, LocalBinding, LocalRule};
    use crate::config::types::LocalPortGrant;
    use crate::config::{NetworkMode, NetworkPolicy};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::AsyncReadExt;
    use tokio::net::{TcpListener, UnixStream};

    /// Sends every allowed host to one loopback upstream.
    struct Upstream {
        addr: std::net::SocketAddr,
        dials: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl Connector for Upstream {
        async fn connect(&self, _host: &str, _port: u16) -> Result<TcpStream, ConnectError> {
            self.dials.fetch_add(1, Ordering::SeqCst);
            TcpStream::connect(self.addr)
                .await
                .map_err(|error| ConnectError::Failed(error.to_string()))
        }
    }

    fn network(allow: &[&str], deny: &[&str]) -> Arc<RunNetwork> {
        Arc::new(RunNetwork::new(&NetworkPolicy {
            mode: NetworkMode::Allowlist,
            allow_domains: allow.iter().map(|entry| entry.to_string()).collect(),
            deny_domains: deny.iter().map(|entry| entry.to_string()).collect(),
            local_ports: Vec::new(),
        }))
    }

    /// An upstream that answers each connection with what `reply` makes of
    /// the bytes it received (an echo when it returns them).
    async fn upstream(reply: fn(Vec<u8>) -> Vec<u8>) -> Arc<Upstream> {
        upstream_at("127.0.0.1:0", reply).await
    }

    async fn upstream_at(addr: &str, reply: fn(Vec<u8>) -> Vec<u8>) -> Arc<Upstream> {
        let listener = TcpListener::bind(addr).await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let mut buffer = vec![0; 4096];
                    let read = stream.read(&mut buffer).await.unwrap_or(0);
                    buffer.truncate(read);
                    let _ = stream.write_all(&reply(buffer)).await;
                });
            }
        });
        Arc::new(Upstream {
            addr,
            dials: AtomicUsize::new(0),
        })
    }

    struct Running {
        session: ProxySession,
        _dir: tempfile::TempDir,
    }

    /// Says what listens on every local port, and records the dial of each
    /// full scan.
    struct FakeInspector {
        listener: Mutex<Listener>,
        /// The only dial `listener` is found at; every dial when none.
        at: Option<Loopback>,
        /// What the cheap check answers when the port has program rules.
        confirms: bool,
        scanned: Mutex<Vec<Loopback>>,
    }

    impl FakeInspector {
        fn new(listener: Listener) -> Self {
            Self {
                listener: Mutex::new(listener),
                at: None,
                confirms: false,
                scanned: Mutex::default(),
            }
        }

        fn scans(&self) -> usize {
            self.scanned.lock().unwrap().len()
        }
    }

    impl ListenerInspector for FakeInspector {
        fn inspect(&self, _port: u16, dial: Loopback) -> io::Result<Listener> {
            self.scanned.lock().unwrap().push(dial);
            if self.at.is_some_and(|at| at != dial) {
                return Ok(Listener::Nothing);
            }
            Ok(self.listener.lock().unwrap().clone())
        }

        fn still_serves(&self, _port: u16, _dial: Loopback, programs: &[String]) -> bool {
            self.confirms && !programs.is_empty()
        }
    }

    const SSH: &str = "/usr/bin/ssh";
    const PYTHON: &str = "/usr/bin/python3.12";

    fn listening(listener: Listener) -> Arc<FakeInspector> {
        Arc::new(FakeInspector::new(listener))
    }

    fn program(exe: &str) -> Listener {
        Listener::Program(Program {
            exe: exe.to_string(),
            pid: 300,
            cmdline: format!("{exe} -N -L 5432:db:5432 secret-host"),
            interpreter: false,
            started_by: None,
        })
    }

    fn ssh() -> Listener {
        program(SSH)
    }

    fn grant(port: u16, program: &str) -> LocalPortGrant {
        LocalPortGrant::Program {
            port,
            program: program.to_string(),
        }
    }

    fn local_network(grants: Vec<LocalPortGrant>, deny: &[&str]) -> Arc<RunNetwork> {
        Arc::new(RunNetwork::new(&NetworkPolicy {
            mode: NetworkMode::Allowlist,
            allow_domains: Vec::new(),
            deny_domains: deny.iter().map(|entry| entry.to_string()).collect(),
            local_ports: grants,
        }))
    }

    /// Whether no connection reached `listener`. A dial the proxy made
    /// before answering is already queued, so the wait is only a bound.
    async fn never_reached(listener: &TcpListener) -> bool {
        tokio::time::timeout(std::time::Duration::from_millis(100), listener.accept())
            .await
            .is_err()
    }

    fn start(network: Arc<RunNetwork>, connector: Arc<dyn Connector>) -> Running {
        start_with(network, connector, listening(Listener::Nothing))
    }

    fn start_with(
        network: Arc<RunNetwork>,
        connector: Arc<dyn Connector>,
        inspector: Arc<dyn ListenerInspector>,
    ) -> Running {
        let dir = tempfile::tempdir().unwrap();
        let session =
            ProxySession::start(&dir.path().join("net.sock"), network, connector, inspector)
                .unwrap();
        Running { session, _dir: dir }
    }

    /// Sends `request`, then `then`, and returns everything read until the
    /// proxy closes.
    async fn exchange(running: &Running, request: &str, then: &[u8]) -> String {
        let mut client = UnixStream::connect(&running.session.socket).await.unwrap();
        client.write_all(request.as_bytes()).await.unwrap();
        client.write_all(then).await.unwrap();
        let mut response = Vec::new();
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            client.read_to_end(&mut response),
        )
        .await
        .expect("the proxy closed the connection")
        .unwrap();
        String::from_utf8_lossy(&response).into_owned()
    }

    #[tokio::test]
    async fn an_allowed_connect_tunnels_bytes_both_ways() {
        let echo = upstream(|bytes| bytes).await;
        let running = start(network(&["*.github.com"], &[]), echo.clone());
        let response = exchange(
            &running,
            "CONNECT api.github.com:443 HTTP/1.1\r\nHost: api.github.com:443\r\n\r\n",
            b"ping",
        )
        .await;
        assert_eq!(response, "HTTP/1.1 200 Connection established\r\n\r\nping");
        assert!(running.session.refused().is_empty());
    }

    #[tokio::test]
    async fn an_unlisted_or_denied_host_gets_a_403_naming_it() {
        let echo = upstream(|bytes| bytes).await;
        let running = start(
            network(&["*.example.org"], &["secret.example.org"]),
            echo.clone(),
        );

        let unlisted = exchange(&running, "CONNECT example.com:443 HTTP/1.1\r\n\r\n", b"").await;
        assert!(
            unlisted.starts_with("HTTP/1.1 403 Forbidden\r\n"),
            "{unlisted}"
        );
        assert!(
            unlisted.ends_with(
                "`example.com` is not in this agent's allowed hosts. Call \
                 `net_request_access` to ask the user, or they can add it in the agent's \
                 Local access settings.\n"
            ),
            "{unlisted}"
        );

        let denied = exchange(
            &running,
            "GET http://secret.example.org/ HTTP/1.1\r\n\r\n",
            b"",
        )
        .await;
        assert!(denied.starts_with("HTTP/1.1 403 Forbidden\r\n"), "{denied}");
        assert!(
            denied.contains("denied hosts (entry `secret.example.org`)"),
            "{denied}"
        );

        assert_eq!(
            running.session.refused().targets,
            ["example.com:443", "secret.example.org:80"]
        );
        assert_eq!(echo.dials.load(Ordering::SeqCst), 0, "nothing was dialled");
    }

    #[tokio::test]
    async fn an_invalid_deny_entry_refuses_every_host() {
        let echo = upstream(|bytes| bytes).await;
        let running = start(network(&["github.com"], &["github.com:443"]), echo.clone());
        let response = exchange(&running, "CONNECT github.com:443 HTTP/1.1\r\n\r\n", b"").await;
        assert!(
            response.contains("invalid entry `github.com:443`"),
            "{response}"
        );
        assert_eq!(echo.dials.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn an_ungranted_local_port_is_refused_without_a_dial() {
        let local = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = local.local_addr().unwrap().port();
        let inspector = listening(ssh());
        let running = start_with(
            network(&["localhost", "127.0.0.1", "127.0.0.2"], &[]),
            Arc::new(HostConnector),
            inspector,
        );
        for request in [
            format!("CONNECT localhost:{port} HTTP/1.1\r\n\r\n"),
            format!("CONNECT 127.0.0.1:{port} HTTP/1.1\r\n\r\n"),
            format!("CONNECT [::1]:{port} HTTP/1.1\r\n\r\n"),
            format!("CONNECT LOCALHOST.:{port} HTTP/1.1\r\n\r\n"),
            format!("GET http://localhost:{port}/ HTTP/1.1\r\n\r\n"),
        ] {
            let response = exchange(&running, &request, b"").await;
            assert!(
                response.starts_with("HTTP/1.1 403 Forbidden\r\n"),
                "{response}"
            );
            assert!(
                response.ends_with(&format!(
                    "`localhost:{port}` (port {port} on this computer) is not in this \
                     agent's allowed local ports. Call `net_request_access` with host \
                     `localhost:{port}` to ask the user.\n"
                )),
                "{response}"
            );
        }
        // Another loopback address is a host, which is never connected to.
        let response = exchange(
            &running,
            &format!("CONNECT 127.0.0.2:{port} HTTP/1.1\r\n\r\n"),
            b"",
        )
        .await;
        assert!(response.contains("never connects to"), "{response}");
        assert_eq!(
            running.session.refused().targets,
            [format!("localhost:{port}")]
        );
        assert!(never_reached(&local).await);
    }

    #[tokio::test]
    async fn an_allowed_local_port_tunnels_to_the_program_listening_there() {
        let echo = upstream(|bytes| bytes).await;
        let port = echo.addr.port();
        let inspector = listening(ssh());
        let network = local_network(vec![grant(port, SSH)], &[]);
        let running = start_with(Arc::clone(&network), echo.clone(), inspector.clone());
        for target in ["localhost", "127.0.0.1"] {
            let response = exchange(
                &running,
                &format!("CONNECT {target}:{port} HTTP/1.1\r\n\r\n"),
                b"ping",
            )
            .await;
            assert_eq!(response, "HTTP/1.1 200 Connection established\r\n\r\nping");
        }
        let response = exchange(
            &running,
            &format!("GET http://localhost:{port}/x HTTP/1.1\r\nHost: localhost:{port}\r\n\r\n"),
            b"",
        )
        .await;
        assert_eq!(
            response,
            format!("GET /x HTTP/1.1\r\nHost: localhost:{port}\r\nConnection: close\r\n\r\n")
        );
        assert!(running.session.refused().is_empty());
        assert_eq!(inspector.scans(), 3);
        assert_eq!(
            echo.dials.load(Ordering::SeqCst),
            0,
            "not through the host connector"
        );
        assert_eq!(
            network.host_log(),
            [HostLogEntry {
                host: "localhost".to_string(),
                port: Some(port),
                allowed: true
            }]
        );
    }

    /// Each name is checked, then dialled, at its own address; `localhost`
    /// at `::1` when nothing listens at 127.0.0.1.
    #[tokio::test]
    async fn each_local_name_is_checked_and_dialled_at_its_own_address() {
        let v4 = upstream(|bytes| bytes).await;
        let v6 = upstream_at("[::1]:0", |bytes| bytes).await;
        for (target, echo, at, scanned) in [
            ("127.0.0.1", &v4, Loopback::V4, &[Loopback::V4][..]),
            ("[::1]", &v6, Loopback::V6, &[Loopback::V6][..]),
            (
                "localhost",
                &v6,
                Loopback::V6,
                &[Loopback::V4, Loopback::V6][..],
            ),
        ] {
            let port = echo.addr.port();
            let inspector = Arc::new(FakeInspector {
                at: Some(at),
                ..FakeInspector::new(ssh())
            });
            let running = start_with(
                local_network(vec![grant(port, SSH)], &[]),
                echo.clone(),
                inspector.clone(),
            );
            let request = format!("CONNECT {target}:{port} HTTP/1.1\r\n\r\n");
            let response = exchange(&running, &request, b"ping").await;
            assert_eq!(
                response, "HTTP/1.1 200 Connection established\r\n\r\nping",
                "{target}"
            );
            assert_eq!(*inspector.scanned.lock().unwrap(), scanned, "{target}");
        }
    }

    #[tokio::test]
    async fn a_program_still_serving_its_allowed_port_is_not_scanned_again() {
        let echo = upstream(|bytes| bytes).await;
        let port = echo.addr.port();
        let inspector = Arc::new(FakeInspector {
            confirms: true,
            ..FakeInspector::new(ssh())
        });
        let running = start_with(
            local_network(vec![grant(port, SSH)], &[]),
            echo.clone(),
            inspector.clone(),
        );
        let request = format!("CONNECT localhost:{port} HTTP/1.1\r\n\r\n");
        let response = exchange(&running, &request, b"ping").await;
        assert_eq!(response, "HTTP/1.1 200 Connection established\r\n\r\nping");
        assert_eq!(inspector.scans(), 0);

        // Only a program rule of the port can be confirmed without a scan.
        let running = start_with(
            local_network(vec![grant(port + 1, SSH)], &[]),
            echo.clone(),
            inspector.clone(),
        );
        let response = exchange(&running, &request, b"").await;
        assert!(
            response.starts_with("HTTP/1.1 403 Forbidden\r\n"),
            "{response}"
        );
        assert_eq!(inspector.scans(), 1);
    }

    #[tokio::test]
    async fn another_program_on_an_allowed_port_is_refused_without_a_dial() {
        let local = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = local.local_addr().unwrap().port();
        let running = start_with(
            local_network(vec![grant(port, SSH)], &[]),
            Arc::new(HostConnector),
            listening(program(PYTHON)),
        );
        let response = exchange(
            &running,
            &format!("CONNECT 127.0.0.1:{port} HTTP/1.1\r\n\r\n"),
            b"",
        )
        .await;
        assert!(
            response.starts_with("HTTP/1.1 403 Forbidden\r\n"),
            "{response}"
        );
        assert!(
            response.ends_with(&format!(
                "Port {port} on this computer is now served by `{PYTHON}`, not by what the \
                 user allowed. Call `net_request_access` with host `localhost:{port}` to \
                 ask again.\n"
            )),
            "{response}"
        );
        assert!(
            !response.contains("secret-host"),
            "the command line is for the card only"
        );
        assert_eq!(
            running.session.refused().targets,
            [format!("localhost:{port}")]
        );
        assert!(never_reached(&local).await);
    }

    #[tokio::test]
    async fn a_deny_entry_for_any_local_name_wins_over_a_grant() {
        let local = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = local.local_addr().unwrap().port();
        for entry in LOCAL_NAMES {
            let inspector = listening(ssh());
            let running = start_with(
                local_network(vec![grant(port, SSH)], &[entry]),
                Arc::new(HostConnector),
                inspector.clone(),
            );
            let response = exchange(
                &running,
                &format!("CONNECT localhost:{port} HTTP/1.1\r\n\r\n"),
                b"",
            )
            .await;
            assert!(
                response.contains(&format!("denied hosts (entry `{entry}`)")),
                "{entry}: {response}"
            );
            assert_eq!(
                running.session.refused().targets,
                [format!("localhost:{port}")]
            );
            assert_eq!(inspector.scans(), 0, "{entry}");
        }
        assert!(never_reached(&local).await);
    }

    #[tokio::test]
    async fn a_socket_allowed_for_the_run_is_refused_once_its_listener_restarts() {
        let local = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = local.local_addr().unwrap().port();
        let network = local_network(Vec::new(), &[]);
        network.allow_local(LocalRule {
            port,
            binding: LocalBinding::Socket(11),
        });
        let unseen = |inode| Listener::RunOnly {
            reason: RunOnly::Unseen,
            sockets: vec![inode],
            holders: Vec::new(),
            started_by: None,
        };
        let inspector = listening(unseen(11));
        let running = start_with(network, Arc::new(HostConnector), inspector.clone());
        let request = format!("CONNECT localhost:{port} HTTP/1.1\r\n\r\n");

        let mut client = UnixStream::connect(&running.session.socket).await.unwrap();
        client.write_all(request.as_bytes()).await.unwrap();
        let mut established = [0_u8; 39];
        client.read_exact(&mut established).await.unwrap();
        assert_eq!(&established, b"HTTP/1.1 200 Connection established\r\n\r\n");
        drop(local.accept().await.unwrap());

        *inspector.listener.lock().unwrap() = unseen(12);
        let response = exchange(&running, &request, b"").await;
        assert!(
            response.contains("now served by a program CLAI cannot name"),
            "{response}"
        );
        *inspector.listener.lock().unwrap() = Listener::OtherUser {
            uid: 26,
            name: "postgres".to_string(),
            sockets: vec![13],
        };
        let response = exchange(&running, &request, b"").await;
        assert!(
            response.contains("now served by a program of user `postgres`, not by"),
            "{response}"
        );
        assert!(never_reached(&local).await);
    }

    #[tokio::test]
    async fn clai_is_refused_and_an_empty_port_is_not_a_refusal() {
        let local = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = local.local_addr().unwrap().port();
        let request = format!("CONNECT localhost:{port} HTTP/1.1\r\n\r\n");
        let network = local_network(vec![grant(port, SSH)], &[]);

        let running = start_with(
            Arc::clone(&network),
            Arc::new(HostConnector),
            listening(Listener::Nothing),
        );
        let response = exchange(&running, &request, b"").await;
        assert!(
            response.starts_with("HTTP/1.1 502 Bad Gateway\r\n"),
            "{response}"
        );
        assert!(
            response.ends_with(&format!(
                "Nothing is listening on port {port} on this computer.\n"
            )),
            "{response}"
        );
        assert!(running.session.refused().is_empty());
        assert!(network.host_log().is_empty());

        let running = start_with(network, Arc::new(HostConnector), listening(Listener::Clai));
        let response = exchange(&running, &request, b"").await;
        assert!(
            response.ends_with(&format!(
                "Port {port} on this computer is served by CLAI itself or by a program CLAI \
                 runs for its own work, such as an agent engine, which agents never connect \
                 to.\n"
            )),
            "{response}"
        );
        assert_eq!(
            running.session.refused().targets,
            [format!("localhost:{port}")]
        );
        assert!(never_reached(&local).await);
    }

    #[tokio::test]
    async fn plain_http_reaches_the_origin_in_origin_form() {
        let mirror = upstream(|head| {
            let head = String::from_utf8(head).unwrap();
            format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{head}",
                head.len()
            )
            .into_bytes()
        })
        .await;
        let running = start(network(&["example.org"], &[]), mirror);
        let response = exchange(
            &running,
            "GET http://example.org/x?y=1 HTTP/1.1\r\nHost: example.org\r\n\
             Proxy-Authorization: Basic eDp5\r\n\r\n",
            b"",
        )
        .await;
        assert!(
            response.ends_with(
                "\r\n\r\nGET /x?y=1 HTTP/1.1\r\nHost: example.org\r\nConnection: close\r\n\r\n"
            ),
            "{response}"
        );
    }

    #[tokio::test]
    async fn malformed_and_oversized_heads_are_closed_with_a_400() {
        let echo = upstream(|bytes| bytes).await;
        let running = start(network(&["github.com"], &[]), echo.clone());
        let malformed = exchange(&running, "hello\r\n\r\n", b"").await;
        assert!(
            malformed.starts_with("HTTP/1.1 400 Bad Request\r\n"),
            "{malformed}"
        );
        let oversized = format!(
            "GET http://github.com/ HTTP/1.1\r\nX: {}",
            "a".repeat(20_000)
        );
        let response = exchange(&running, &oversized, b"").await;
        assert!(
            response.starts_with("HTTP/1.1 400 Bad Request\r\n"),
            "{response}"
        );
        assert_eq!(echo.dials.load(Ordering::SeqCst), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn a_client_that_sends_no_head_is_dropped_after_the_head_timeout() {
        let echo = upstream(|bytes| bytes).await;
        let shared = Shared {
            network: network(&["github.com"], &[]),
            connector: echo,
            inspector: listening(Listener::Nothing),
            refused: Arc::default(),
        };
        let (client, mut proxy_side) = tokio::io::duplex(1024);
        serve(&mut proxy_side, &shared).await;
        drop(proxy_side);
        let mut response = String::new();
        let mut client = client;
        client.read_to_string(&mut response).await.unwrap();
        assert!(
            response.starts_with("HTTP/1.1 408 Request Timeout\r\n"),
            "{response}"
        );
    }

    #[tokio::test]
    async fn the_run_log_gets_one_entry_per_host_and_outcome() {
        let echo = upstream(|bytes| bytes).await;
        let network = network(&["github.com"], &[]);
        let running = start(Arc::clone(&network), echo);
        for request in [
            "CONNECT github.com:443 HTTP/1.1\r\n\r\n",
            "CONNECT GitHub.com.:443 HTTP/1.1\r\n\r\n",
            "CONNECT example.org:443 HTTP/1.1\r\n\r\n",
            "GET http://example.org/ HTTP/1.1\r\n\r\n",
        ] {
            exchange(&running, request, b"x").await;
        }
        let log: Vec<(String, bool)> = network
            .host_log()
            .into_iter()
            .map(|entry| (entry.host, entry.allowed))
            .collect();
        assert_eq!(
            log,
            [
                ("github.com".to_string(), true),
                ("example.org".to_string(), false)
            ]
        );
        assert_eq!(
            running.session.refused().targets,
            ["example.org:443", "example.org:80"]
        );
    }

    #[tokio::test]
    async fn past_its_connection_limit_a_command_gets_a_503_until_a_tunnel_closes() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let session = ProxySession::start_with_limit(
            &dir.path().join("net.sock"),
            network(&["github.com"], &[]),
            Arc::new(Upstream {
                addr,
                dials: AtomicUsize::new(0),
            }),
            listening(Listener::Nothing),
            2,
        )
        .unwrap();
        let running = Running { session, _dir: dir };
        let mut tunnels = Vec::new();
        for _ in 0..2 {
            let mut client = UnixStream::connect(&running.session.socket).await.unwrap();
            client
                .write_all(b"CONNECT github.com:443 HTTP/1.1\r\n\r\n")
                .await
                .unwrap();
            let mut established = [0_u8; 39];
            client.read_exact(&mut established).await.unwrap();
            let (upstream_side, _) = listener.accept().await.unwrap();
            tunnels.push((client, upstream_side));
        }

        // Every request needs a slot, a refused one too.
        let unlisted = "CONNECT example.com:443 HTTP/1.1\r\n\r\n";
        let full = exchange(&running, unlisted, b"").await;
        assert!(
            full.starts_with("HTTP/1.1 503 Service Unavailable\r\n"),
            "{full}"
        );
        assert!(full.contains("too many connections"), "{full}");
        assert!(running.session.refused().is_empty());

        // A client that sends nothing while the proxy is full must not stall
        // the accept loop for the connections behind it.
        let _silent = UnixStream::connect(&running.session.socket).await.unwrap();
        let behind = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            exchange(&running, unlisted, b""),
        )
        .await
        .expect("a silent client stalled the accept loop");
        assert!(
            behind.starts_with("HTTP/1.1 503 Service Unavailable\r\n"),
            "{behind}"
        );

        drop(tunnels.pop());
        let freed = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let response = exchange(&running, unlisted, b"").await;
                if !response.starts_with("HTTP/1.1 503") {
                    return response;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the closed tunnel gave its slot back");
        assert!(freed.starts_with("HTTP/1.1 403 Forbidden\r\n"), "{freed}");
    }

    #[tokio::test]
    async fn dropping_the_session_closes_its_tunnels_and_socket() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let held = tokio::spawn(async move { listener.accept().await.unwrap().0 });
        let running = start(
            network(&["github.com"], &[]),
            Arc::new(Upstream {
                addr,
                dials: AtomicUsize::new(0),
            }),
        );
        let socket = running.session.socket.clone();
        let mut client = UnixStream::connect(&socket).await.unwrap();
        client
            .write_all(b"CONNECT github.com:443 HTTP/1.1\r\n\r\n")
            .await
            .unwrap();
        let mut established = [0_u8; 39];
        client.read_exact(&mut established).await.unwrap();
        let _upstream_side = held.await.unwrap();

        drop(running.session);
        let mut rest = Vec::new();
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            client.read_to_end(&mut rest),
        )
        .await
        .expect("the tunnel was closed")
        .unwrap();
        assert!(rest.is_empty());
        assert!(!socket.exists());
        assert!(UnixStream::connect(&socket).await.is_err());
    }
}
