//! The network proxy's outbound connections: resolved here, on the host, and
//! made only to an address the proxy may reach, the one it checked.

use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use tokio::net::TcpStream;

use super::addr::is_proxy_forbidden_ip;
use super::local_listener::Loopback;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, PartialEq, Eq)]
pub enum ConnectError {
    /// Every address of the host is one the proxy never connects to.
    Forbidden,
    Failed(String),
}

/// Opens the upstream connection for an allowed host. Tests put a loopback
/// upstream behind it; the proxy itself uses [`HostConnector`].
#[async_trait::async_trait]
pub trait Connector: Send + Sync {
    async fn connect(&self, host: &str, port: u16) -> Result<TcpStream, ConnectError>;
}

pub struct HostConnector;

#[async_trait::async_trait]
impl Connector for HostConnector {
    async fn connect(&self, host: &str, port: u16) -> Result<TcpStream, ConnectError> {
        tokio::time::timeout(CONNECT_TIMEOUT, connect_checked(host, port))
            .await
            .unwrap_or_else(|_| Err(ConnectError::Failed("timed out".to_string())))
    }
}

/// Opens a connection to a port on this computer, at the loopback address
/// the listener there was checked for. The proxy calls it only once the run
/// may reach what listens on the port.
pub struct LocalConnector;

impl LocalConnector {
    pub async fn connect(&self, dial: Loopback, port: u16) -> Result<TcpStream, String> {
        match tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect((dial.ip(), port))).await {
            Ok(connected) => connected.map_err(|error| error.to_string()),
            Err(_) => Err("timed out".to_string()),
        }
    }
}

/// `host` is canonical: a lower-case name or an IP without brackets.
async fn connect_checked(host: &str, port: u16) -> Result<TcpStream, ConnectError> {
    let addrs: Vec<SocketAddr> = match host.parse::<IpAddr>() {
        Ok(ip) => vec![SocketAddr::new(ip, port)],
        Err(_) => tokio::net::lookup_host((host, port))
            .await
            .map_err(|error| ConnectError::Failed(format!("cannot resolve {host}: {error}")))?
            .collect(),
    };
    let mut last_error = None;
    for addr in addrs
        .into_iter()
        .filter(|addr| !is_proxy_forbidden_ip(addr.ip()))
    {
        match TcpStream::connect(addr).await {
            Ok(stream) => return Ok(stream),
            Err(error) => last_error = Some(error.to_string()),
        }
    }
    Err(last_error.map_or(ConnectError::Forbidden, ConnectError::Failed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn local_and_metadata_addresses_are_never_dialled() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        for host in ["127.0.0.1", "localhost", "::1", "64:ff9b::7f00:1"] {
            assert_eq!(
                HostConnector.connect(host, port).await.err(),
                Some(ConnectError::Forbidden),
                "{host}"
            );
        }
        assert_eq!(
            HostConnector.connect("169.254.169.254", 80).await.err(),
            Some(ConnectError::Forbidden)
        );
        let accepted = tokio::time::timeout(Duration::from_millis(100), listener.accept()).await;
        assert!(accepted.is_err(), "the local listener was reached");
    }
}
