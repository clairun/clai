//! Address and host-name filters for CLAI's own network clients.
//!
//! `web_fetch` refuses every non-public destination. The network proxy is
//! looser: it serves hosts the user listed, a LAN host included, and refuses
//! only the addresses no listed name should ever lead to.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

pub(crate) fn is_blocked_web_fetch_hostname(normalized_host: &str) -> bool {
    normalized_host == "localhost"
        || normalized_host.ends_with(".localhost")
        || normalized_host.ends_with(".local")
        || normalized_host.ends_with(".internal")
}

pub(crate) fn strip_ipv6_url_brackets(host: &str) -> &str {
    host.strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .unwrap_or(host)
}

pub(crate) fn reject_private_web_fetch_ip(ip: IpAddr) -> Result<(), String> {
    if is_private_web_fetch_ip(ip) {
        Err("Fetching private/local URLs is not allowed".to_string())
    } else {
        Ok(())
    }
}

pub(crate) fn reject_private_web_fetch_addrs(addrs: &[SocketAddr]) -> Result<(), String> {
    for addr in addrs {
        reject_private_web_fetch_ip(addr.ip())?;
    }
    Ok(())
}

fn is_private_web_fetch_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => is_private_web_fetch_ipv4(ip),
        IpAddr::V6(ip) => is_private_web_fetch_ipv6(ip),
    }
}

fn is_private_web_fetch_ipv4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_multicast()
        || ip.is_broadcast()
        || ip.is_documentation()
        || ip.is_unspecified()
        || (a == 100 && (64..=127).contains(&b))
        || (a == 198 && (b == 18 || b == 19))
        || (a == 192 && b == 0 && c == 0)
        || a >= 240
}

fn is_private_web_fetch_ipv6(ip: Ipv6Addr) -> bool {
    if let Some(mapped) = ip.to_ipv4_mapped() {
        return is_private_web_fetch_ipv4(mapped);
    }

    let octets = ip.octets();
    let segments = ip.segments();

    // NAT64: the well-known prefix 64:ff9b::/96 embeds a target IPv4 in the
    // low 32 bits, so `64:ff9b::7f00:1` would reach 127.0.0.1. Extract and
    // apply the IPv4 blocklist rather than trusting the v6 literal. The
    // local-use prefix 64:ff9b:1::/48 (RFC 8215) is site-internal by
    // definition, so block it wholesale.
    if segments[0] == 0x0064 && segments[1] == 0xff9b {
        if segments[2] == 0x0001 {
            return true;
        }
        if segments[2] == 0 && segments[3] == 0 && segments[4] == 0 && segments[5] == 0 {
            let embedded = std::net::Ipv4Addr::new(octets[12], octets[13], octets[14], octets[15]);
            return is_private_web_fetch_ipv4(embedded);
        }
    }

    ip.is_loopback()
        || ip.is_unspecified()
        || ip.is_multicast()
        || (octets[0] & 0xfe) == 0xfc
        || (segments[0] & 0xffc0) == 0xfe80
        || (segments[0] & 0xffc0) == 0xfec0
        || (segments[0] == 0x2001 && segments[1] == 0x0db8)
        || segments[0] == 0x2002
}

/// AWS's IPv6 instance metadata endpoint.
const AWS_IPV6_METADATA: Ipv6Addr = Ipv6Addr::new(0xfd00, 0x0ec2, 0, 0, 0, 0, 0, 0x0254);

/// Whether the network proxy refuses to connect to `ip` whatever the
/// allowlist says: loopback, link-local (cloud metadata included),
/// unspecified, multicast and AWS's IPv6 metadata address. Private LAN
/// ranges stay reachable, since the user may list a host that lives there.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "the proxy, added later, calls it")
)]
pub(crate) fn is_proxy_forbidden_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => is_proxy_forbidden_ipv4(ip),
        IpAddr::V6(ip) => match ip.to_ipv4_mapped() {
            Some(mapped) => is_proxy_forbidden_ipv4(mapped),
            None => {
                ip.is_loopback()
                    || ip.is_unspecified()
                    || ip.is_multicast()
                    || (ip.segments()[0] & 0xffc0) == 0xfe80
                    || ip == AWS_IPV6_METADATA
            }
        },
    }
}

fn is_proxy_forbidden_ipv4(ip: Ipv4Addr) -> bool {
    ip.is_loopback() || ip.is_link_local() || ip.is_multicast() || ip.octets()[0] == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn web_fetch_ip_filter_blocks_non_public_ranges() {
        for ip in [
            "0.0.0.0",
            "10.0.0.1",
            "100.64.0.1",
            "127.0.0.1",
            "169.254.169.254",
            "172.16.0.1",
            "192.168.1.1",
            "192.0.0.1",
            "198.18.0.1",
            "224.0.0.1",
            "240.0.0.1",
            "::",
            "::1",
            "::ffff:127.0.0.1",
            "fc00::1",
            "fe80::1",
            "fec0::1",
            "2001:db8::1",
            "2002:0a00:0001::1",
            // NAT64 well-known prefix embedding 127.0.0.1 (64:ff9b::7f00:1).
            "64:ff9b::7f00:1",
            // NAT64 local-use prefix 64:ff9b:1::/48 (site-internal by definition).
            "64:ff9b:1::1",
        ] {
            assert!(is_private_web_fetch_ip(ip.parse().unwrap()), "{ip}");
        }
    }

    #[test]
    fn web_fetch_ip_filter_allows_public_addresses() {
        for ip in [
            "8.8.8.8",
            "1.1.1.1",
            "2606:4700:4700::1111",
            // NAT64 well-known prefix embedding a public IPv4 (8.8.8.8) must
            // still be reachable — only the embedded IPv4 decides.
            "64:ff9b::808:808",
        ] {
            assert!(!is_private_web_fetch_ip(ip.parse().unwrap()), "{ip}");
        }
    }

    #[test]
    fn web_fetch_blocks_private_dns_results() {
        let addrs = [
            SocketAddr::new("93.184.216.34".parse().unwrap(), 80),
            SocketAddr::new("192.168.1.1".parse().unwrap(), 80),
        ];

        let err = reject_private_web_fetch_addrs(&addrs).unwrap_err();
        assert!(err.contains("private/local"));
    }

    #[test]
    fn web_fetch_hostname_filter_blocks_local_names() {
        for host in [
            "localhost",
            "api.localhost",
            "myhost.local",
            "service.internal",
        ] {
            assert!(is_blocked_web_fetch_hostname(host), "{host}");
        }
        assert!(!is_blocked_web_fetch_hostname("example.com"));
    }

    #[test]
    fn proxy_filter_refuses_local_and_metadata_addresses() {
        for ip in [
            "127.0.0.1",
            "127.1.2.3",
            "169.254.169.254",
            "169.254.0.1",
            "0.0.0.0",
            "224.0.0.1",
            "::1",
            "::",
            "::ffff:127.0.0.1",
            "::ffff:169.254.169.254",
            "fe80::1",
            "ff02::1",
            "fd00:ec2::254",
        ] {
            assert!(is_proxy_forbidden_ip(ip.parse().unwrap()), "{ip}");
        }
    }

    #[test]
    fn proxy_filter_allows_lan_and_public_addresses() {
        for ip in [
            "10.0.0.1",
            "192.168.1.1",
            "172.16.0.1",
            "100.64.0.1",
            "8.8.8.8",
            "fd00::1",
            "2606:4700:4700::1111",
            "::ffff:10.0.0.1",
        ] {
            assert!(!is_proxy_forbidden_ip(ip.parse().unwrap()), "{ip}");
        }
    }
}
