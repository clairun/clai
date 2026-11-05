//! The two requests the network proxy serves: `CONNECT host:port`, and
//! plain HTTP in absolute form, forwarded in origin form.

use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt};

pub const MAX_HEAD_BYTES: usize = 16 * 1024;
pub const HEAD_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, PartialEq, Eq)]
pub enum Request {
    Connect {
        host: String,
        port: u16,
    },
    /// `head` is the request rewritten for the origin server.
    Forward {
        host: String,
        port: u16,
        head: Vec<u8>,
    },
}

impl Request {
    pub fn target(&self) -> (&str, u16) {
        match self {
            Self::Connect { host, port } | Self::Forward { host, port, .. } => (host, *port),
        }
    }
}

/// Reads up to the blank line that ends a request head. Returns the head
/// and the bytes already read past it, which belong to the body or tunnel.
pub async fn read_head<R: AsyncRead + Unpin>(
    reader: &mut R,
) -> Result<(String, Vec<u8>), &'static str> {
    let mut buffer = Vec::with_capacity(1024);
    let mut chunk = [0_u8; 4096];
    loop {
        let read = reader
            .read(&mut chunk)
            .await
            .map_err(|_| "the request could not be read")?;
        if read == 0 {
            return Err("the request ended before its head did");
        }
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(end) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            let rest = buffer.split_off(end + 4);
            let head = String::from_utf8(buffer).map_err(|_| "the request head is not UTF-8")?;
            return Ok((head, rest));
        }
        if buffer.len() > MAX_HEAD_BYTES {
            return Err("the request head is too large");
        }
    }
}

pub fn parse_request(head: &str) -> Result<Request, &'static str> {
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or_default();
    let mut parts = request_line.split(' ');
    let (Some(method), Some(target), Some(version), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err("malformed request line");
    };
    if !version.starts_with("HTTP/1.") {
        return Err("only HTTP/1.x is served");
    }
    if method.eq_ignore_ascii_case("CONNECT") {
        let (host, port) = split_authority(target).ok_or("CONNECT needs host:port")?;
        return Ok(Request::Connect { host, port });
    }

    let url = url::Url::parse(target).map_err(|_| "the proxy serves absolute http:// URLs")?;
    if url.scheme() != "http" {
        return Err("the proxy serves absolute http:// URLs; use CONNECT for https");
    }
    let host = url.host_str().ok_or("the URL has no host")?.to_string();
    let port = url.port_or_known_default().ok_or("the URL has no port")?;
    let origin_form = &url[url::Position::BeforePath..url::Position::AfterQuery];

    // The origin sees the host the proxy decided on (RFC 9112 '3.2.2), never
    // the client's own `Host` lines.
    let mut rewritten = format!(
        "{method} {origin_form} {version}\r\nHost: {}\r\n",
        &url[url::Position::BeforeHost..url::Position::BeforePath]
    );
    for line in lines.filter(|line| !line.is_empty()) {
        let name = line.split(':').next().unwrap_or_default().trim();
        let name = name.to_ascii_lowercase();
        if name.starts_with("proxy-")
            || name == "connection"
            || name == "keep-alive"
            || name == "host"
        {
            continue;
        }
        rewritten.push_str(line);
        rewritten.push_str("\r\n");
    }
    rewritten.push_str("Connection: close\r\n\r\n");
    Ok(Request::Forward {
        host,
        port,
        head: rewritten.into_bytes(),
    })
}

/// `host:port` or `[v6]:port`; the host comes back without brackets.
fn split_authority(authority: &str) -> Option<(String, u16)> {
    let (host, port) = match authority.strip_prefix('[') {
        Some(bracketed) => {
            let (host, rest) = bracketed.split_once(']')?;
            (host, rest.strip_prefix(':')?)
        }
        None => authority.rsplit_once(':')?,
    };
    let port = port.parse::<u16>().ok().filter(|port| *port != 0)?;
    (!host.is_empty()).then(|| (host.to_string(), port))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connect_targets_name_a_host_and_port() {
        let connect = |head: &str| parse_request(head);
        assert_eq!(
            connect("CONNECT github.com:443 HTTP/1.1\r\nHost: github.com:443\r\n\r\n"),
            Ok(Request::Connect {
                host: "github.com".to_string(),
                port: 443
            })
        );
        assert_eq!(
            connect("CONNECT [2001:db8::1]:22 HTTP/1.1\r\n\r\n")
                .unwrap()
                .target(),
            ("2001:db8::1", 22)
        );
        for bad in [
            "CONNECT github.com HTTP/1.1\r\n\r\n",
            "CONNECT github.com:0 HTTP/1.1\r\n\r\n",
            "CONNECT :443 HTTP/1.1\r\n\r\n",
            "CONNECT github.com:443\r\n\r\n",
            "CONNECT github.com:443 HTTP/2\r\n\r\n",
            "GET /relative HTTP/1.1\r\n\r\n",
            "GET https://github.com/ HTTP/1.1\r\n\r\n",
            "garbage\r\n\r\n",
        ] {
            assert!(parse_request(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn absolute_form_is_forwarded_in_origin_form_without_proxy_headers() {
        let request = parse_request(
            "GET http://example.org:8080/a/b?q=1 HTTP/1.1\r\nHost: other.example\r\n\
             Proxy-Authorization: Basic eDp5\r\nProxy-Connection: keep-alive\r\n\
             Connection: keep-alive\r\nAccept: */*\r\nhost: second.example\r\n\r\n",
        )
        .unwrap();
        let Request::Forward { host, port, head } = request else {
            panic!("not forwarded");
        };
        assert_eq!((host.as_str(), port), ("example.org", 8080));
        assert_eq!(
            String::from_utf8(head).unwrap(),
            "GET /a/b?q=1 HTTP/1.1\r\nHost: example.org:8080\r\nAccept: */*\r\n\
             Connection: close\r\n\r\n"
        );

        let Request::Forward { head, .. } =
            parse_request("GET http://example.org HTTP/1.0\r\n\r\n").unwrap()
        else {
            panic!("not forwarded");
        };
        assert_eq!(
            String::from_utf8(head).unwrap(),
            "GET / HTTP/1.0\r\nHost: example.org\r\nConnection: close\r\n\r\n"
        );
    }

    #[tokio::test]
    async fn a_head_ends_at_its_blank_line_and_keeps_what_follows() {
        let mut input: &[u8] = b"CONNECT a.org:443 HTTP/1.1\r\n\r\n\x16\x03\x01";
        let (head, rest) = read_head(&mut input).await.unwrap();
        assert_eq!(head, "CONNECT a.org:443 HTTP/1.1\r\n\r\n");
        assert_eq!(rest, b"\x16\x03\x01");

        let oversized = format!(
            "GET http://a.org/ HTTP/1.1\r\nX: {}",
            "a".repeat(MAX_HEAD_BYTES)
        );
        assert!(read_head(&mut oversized.as_bytes()).await.is_err());
        assert!(read_head(&mut &b"GET http://a.org/ HTTP/1.1\r\n"[..])
            .await
            .is_err());
        assert!(read_head(&mut &b"\xff\xfe\r\n\r\n"[..]).await.is_err());
    }
}
