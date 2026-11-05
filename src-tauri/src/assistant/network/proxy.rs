//! The CLAI network proxy for one sandboxed command in Allowlist mode.
//!
//! The command runs in its own network namespace; the sandbox helper
//! bridges its `HTTP(S)_PROXY` port to this session's Unix socket. Each
//! request is decided with the run's lists and the hosts the user allowed
//! for the run, logged in the run's host log, and either refused with a
//! 403 naming the host or connected through [`Connector`].

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::net::UnixListener;
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

use super::connect::{ConnectError, Connector};
use super::http::{parse_request, read_head, Request, HEAD_TIMEOUT};
use super::matcher::{canonical_host, HostDecision};
use super::run_state::{RefusedTargets, RunNetwork};

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
    refused: Arc<Mutex<RefusedTargets>>,
}

impl ProxySession {
    /// Binds `socket` (which must not exist) and serves it on the current
    /// tokio runtime.
    pub fn start(
        socket: &Path,
        network: Arc<RunNetwork>,
        connector: Arc<dyn Connector>,
    ) -> std::io::Result<Self> {
        Self::start_with_limit(socket, network, connector, MAX_CONNECTIONS)
    }

    fn start_with_limit(
        socket: &Path,
        network: Arc<RunNetwork>,
        connector: Arc<dyn Connector>,
        max_connections: usize,
    ) -> std::io::Result<Self> {
        let listener = UnixListener::bind(socket)?;
        let refused = Arc::new(Mutex::new(RefusedTargets::default()));
        let cancel = CancellationToken::new();
        let shared = Arc::new(Shared {
            network,
            connector,
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
    let decision = shared.network.decide(&host);
    shared
        .network
        .record(&host, Some(port), decision == HostDecision::Allowed);
    if let Some(refusal) = decision.refusal(&host, true) {
        shared.refuse(target_text(&host, port));
        return respond(&mut client, "403 Forbidden", &refusal).await;
    }

    let mut upstream = match shared.connector.connect(&host, port).await {
        Ok(upstream) => upstream,
        Err(ConnectError::Forbidden) => {
            let reason = format!(
                "`{host}` resolves only to loopback, link-local or other local addresses, \
                 which the CLAI network proxy never connects to."
            );
            return respond(&mut client, "403 Forbidden", &reason).await;
        }
        Err(ConnectError::Failed(error)) => {
            let reason = format!("Could not connect to {}: {error}", target_text(&host, port));
            return respond(&mut client, "502 Bad Gateway", &reason).await;
        }
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
    use crate::config::{NetworkMode, NetworkPolicy};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::AsyncReadExt;
    use tokio::net::{TcpListener, TcpStream, UnixStream};

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
        }))
    }

    /// An upstream that answers each connection with what `reply` makes of
    /// the bytes it received (an echo when it returns them).
    async fn upstream(reply: fn(Vec<u8>) -> Vec<u8>) -> Arc<Upstream> {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
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

    fn start(network: Arc<RunNetwork>, connector: Arc<dyn Connector>) -> Running {
        let dir = tempfile::tempdir().unwrap();
        let session =
            ProxySession::start(&dir.path().join("net.sock"), network, connector).unwrap();
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
    async fn a_listed_host_on_a_local_address_is_still_refused() {
        let local = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = local.local_addr().unwrap().port();
        let running = start(
            network(&["localhost", "127.0.0.1"], &[]),
            Arc::new(HostConnector),
        );
        for target in ["127.0.0.1", "localhost"] {
            let response = exchange(
                &running,
                &format!("CONNECT {target}:{port} HTTP/1.1\r\n\r\n"),
                b"",
            )
            .await;
            assert!(
                response.starts_with("HTTP/1.1 403 Forbidden\r\n"),
                "{response}"
            );
            assert!(response.contains("never connects to"), "{response}");
        }
        let reached =
            tokio::time::timeout(std::time::Duration::from_millis(100), local.accept()).await;
        assert!(
            reached.is_err(),
            "the proxy connected to a loopback listener"
        );
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
