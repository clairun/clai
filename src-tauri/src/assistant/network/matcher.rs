//! The one host matcher behind the network allowlist.
//!
//! The web tools and the network proxy call it, and only in Allowlist mode.
//! Entries are host names; a leading `*` label stands for one or more labels
//! (`*.github.com` matches `api.github.com`, not `github.com`), an interior
//! `*` label for exactly one (`s3.*.amazonaws.com`). An IP host matches only
//! the identical IP entry. Ports are not part of an entry. The deny list is
//! checked first.

use std::net::IpAddr;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostDecision {
    Allowed,
    /// Refused by this deny-list entry, even if an allow entry covers it.
    Denied {
        entry: String,
    },
    /// Neither list covers the host, or it is not a valid host name.
    Unlisted,
}

#[derive(Debug, PartialEq, Eq)]
enum Host {
    Domain(String),
    Ip(IpAddr),
}

pub fn host_decision<S: AsRef<str>>(allow: &[S], deny: &[S], host: &str) -> HostDecision {
    let Some(host) = normalize(host).filter(|host| !has_wildcard(host)) else {
        return HostDecision::Unlisted;
    };
    if let Some(entry) = deny
        .iter()
        .find(|entry| entry_matches(entry.as_ref(), &host))
    {
        return HostDecision::Denied {
            entry: entry.as_ref().to_string(),
        };
    }
    if allow
        .iter()
        .any(|entry| entry_matches(entry.as_ref(), &host))
    {
        return HostDecision::Allowed;
    }
    HostDecision::Unlisted
}

/// Lower case, one trailing dot dropped, IDNA to punycode, IP literals (with
/// or without IPv6 brackets) parsed. `None` for an empty or invalid host.
fn normalize(raw: &str) -> Option<Host> {
    let raw = raw.trim();
    let raw = raw.strip_suffix('.').unwrap_or(raw);
    if raw.is_empty() {
        return None;
    }
    let unbracketed = raw
        .strip_prefix('[')
        .and_then(|inner| inner.strip_suffix(']'))
        .unwrap_or(raw);
    if let Ok(ip) = unbracketed.parse::<IpAddr>() {
        return Some(Host::Ip(ip));
    }
    match url::Host::parse(raw).ok()? {
        url::Host::Domain(domain) if domain.split('.').all(|label| !label.is_empty()) => {
            Some(Host::Domain(domain))
        }
        url::Host::Domain(_) => None,
        url::Host::Ipv4(ip) => Some(Host::Ip(ip.into())),
        url::Host::Ipv6(ip) => Some(Host::Ip(ip.into())),
    }
}

fn has_wildcard(host: &Host) -> bool {
    matches!(host, Host::Domain(domain) if domain.contains('*'))
}

fn entry_matches(entry: &str, host: &Host) -> bool {
    match (normalize(entry), host) {
        (Some(Host::Ip(entry)), Host::Ip(host)) => entry == *host,
        (Some(Host::Domain(entry)), Host::Domain(host)) => {
            let entry: Vec<&str> = entry.split('.').collect();
            let host: Vec<&str> = host.split('.').collect();
            domain_matches(&entry, &host)
        }
        _ => false,
    }
}

fn domain_matches(entry: &[&str], host: &[&str]) -> bool {
    match entry {
        ["*", rest @ ..] => {
            host.len() > rest.len() && labels_match(rest, &host[host.len() - rest.len()..])
        }
        _ => labels_match(entry, host),
    }
}

/// Label by label; an entry label `*` stands for exactly one host label.
fn labels_match(entry: &[&str], host: &[&str]) -> bool {
    entry.len() == host.len()
        && entry
            .iter()
            .zip(host)
            .all(|(entry, host)| *entry == "*" || entry == host)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assistant::network::default_lists::{DEFAULT_ALLOW_DOMAINS, DEFAULT_DENY_DOMAINS};
    use HostDecision::{Allowed, Unlisted};

    fn decide(allow: &[&str], deny: &[&str], host: &str) -> HostDecision {
        host_decision(allow, deny, host)
    }

    fn with_defaults(host: &str) -> HostDecision {
        decide(DEFAULT_ALLOW_DOMAINS, DEFAULT_DENY_DOMAINS, host)
    }

    fn denied(entry: &str) -> HostDecision {
        HostDecision::Denied {
            entry: entry.to_string(),
        }
    }

    #[test]
    fn default_lists_decide_the_documented_hosts() {
        for (host, expected) in [
            ("github.com", Allowed),
            ("api.github.com", Allowed),
            ("raw.githubusercontent.com", Allowed),
            ("crates.io", Allowed),
            ("index.crates.io", Allowed),
            ("generativelanguage.googleapis.com", Allowed),
            // No whole-provider wildcard: other cloud hosts are asked for.
            ("ec2.us-east-1.amazonaws.com", Unlisted),
            (
                "abc123.execute-api.us-east-1.amazonaws.com",
                denied("*.execute-api.*.amazonaws.com"),
            ),
            ("www.googleapis.com", Unlisted),
            ("s3-us-west-2.amazonaws.com", Unlisted),
            ("s3.amazonaws.com", denied("s3.amazonaws.com")),
            ("bucket.s3.amazonaws.com", denied("*.s3.amazonaws.com")),
            ("s3.us-east-1.amazonaws.com", denied("s3.*.amazonaws.com")),
            (
                "bucket.s3.us-east-1.amazonaws.com",
                denied("*.s3.*.amazonaws.com"),
            ),
            ("storage.googleapis.com", denied("storage.googleapis.com")),
            (
                "bucket.storage.googleapis.com",
                denied("*.storage.googleapis.com"),
            ),
            ("example.org", Unlisted),
            ("evilgithub.com", Unlisted),
            ("github.com.evil.org", Unlisted),
            ("duckduckgo.com", Unlisted),
        ] {
            assert_eq!(with_defaults(host), expected, "{host}");
        }
    }

    /// A user who adds a whole provider still cannot reach its storage.
    #[test]
    fn the_default_deny_list_covers_s3_and_gcs_under_a_broad_allow_entry() {
        let allow = ["*.amazonaws.com", "*.googleapis.com"];
        for (host, entry) in [
            ("s3.amazonaws.com", "s3.amazonaws.com"),
            ("bucket.s3.amazonaws.com", "*.s3.amazonaws.com"),
            ("s3.us-east-1.amazonaws.com", "s3.*.amazonaws.com"),
            ("bucket.s3.us-east-1.amazonaws.com", "*.s3.*.amazonaws.com"),
            (
                "s3.dualstack.us-east-1.amazonaws.com",
                "s3.dualstack.*.amazonaws.com",
            ),
            (
                "bucket.s3.dualstack.us-east-1.amazonaws.com",
                "*.s3.dualstack.*.amazonaws.com",
            ),
            ("s3-accelerate.amazonaws.com", "s3-accelerate.amazonaws.com"),
            (
                "bucket.s3-accelerate.amazonaws.com",
                "*.s3-accelerate.amazonaws.com",
            ),
            (
                "bucket.s3-accelerate.dualstack.amazonaws.com",
                "*.s3-accelerate.dualstack.amazonaws.com",
            ),
            (
                "abc123.execute-api.us-east-1.amazonaws.com",
                "*.execute-api.*.amazonaws.com",
            ),
            ("storage.googleapis.com", "storage.googleapis.com"),
            ("bucket.storage.googleapis.com", "*.storage.googleapis.com"),
        ] {
            assert_eq!(
                decide(&allow, DEFAULT_DENY_DOMAINS, host),
                denied(entry),
                "{host}"
            );
        }
        assert_eq!(
            decide(&allow, DEFAULT_DENY_DOMAINS, "ec2.us-east-1.amazonaws.com"),
            Allowed
        );
    }

    #[test]
    fn default_allow_list_has_no_duplicates_and_every_entry_parses() {
        let mut seen = std::collections::HashSet::new();
        for entry in DEFAULT_ALLOW_DOMAINS.iter().chain(DEFAULT_DENY_DOMAINS) {
            assert!(seen.insert(*entry), "duplicate {entry}");
            assert!(normalize(entry).is_some(), "{entry}");
        }
        assert!(DEFAULT_ALLOW_DOMAINS.len() >= 100);
    }

    #[test]
    fn a_leading_wildcard_matches_subdomains_only() {
        assert_eq!(decide(&["*.github.com"], &[], "api.github.com"), Allowed);
        assert_eq!(decide(&["*.github.com"], &[], "a.b.github.com"), Allowed);
        assert_eq!(decide(&["*.github.com"], &[], "github.com"), Unlisted);
        assert_eq!(decide(&["github.com"], &[], "github.com"), Allowed);
        assert_eq!(decide(&["github.com"], &[], "api.github.com"), Unlisted);
        assert_eq!(decide(&["*.com"], &[], "a.com"), Allowed);
        assert_eq!(decide(&["*.com"], &[], "com"), Unlisted);
    }

    #[test]
    fn a_wildcard_respects_label_boundaries() {
        assert_eq!(decide(&["*.github.com"], &[], "evilgithub.com"), Unlisted);
        assert_eq!(
            decide(&["*.github.com"], &[], "api.evilgithub.com"),
            Unlisted
        );
        assert_eq!(decide(&["github.com"], &[], "evilgithub.com"), Unlisted);
    }

    #[test]
    fn an_interior_wildcard_matches_exactly_one_label() {
        let entry = ["s3.*.amazonaws.com"];
        assert_eq!(decide(&entry, &[], "s3.eu-west-1.amazonaws.com"), Allowed);
        assert_eq!(decide(&entry, &[], "s3.amazonaws.com"), Unlisted);
        assert_eq!(decide(&entry, &[], "s3.a.b.amazonaws.com"), Unlisted);
        assert_eq!(decide(&entry, &[], "x.s3.a.amazonaws.com"), Unlisted);
    }

    #[test]
    fn the_deny_list_wins_over_the_allow_list() {
        assert_eq!(
            decide(
                &["*.amazonaws.com"],
                &["*.s3.amazonaws.com"],
                "bucket.s3.amazonaws.com"
            ),
            denied("*.s3.amazonaws.com")
        );
        assert_eq!(
            decide(&["example.org"], &["example.org"], "example.org"),
            denied("example.org")
        );
    }

    #[test]
    fn hosts_and_entries_are_normalised() {
        assert_eq!(decide(&["github.com"], &[], "GitHub.COM."), Allowed);
        assert_eq!(decide(&["GitHub.com."], &[], "github.com"), Allowed);
        assert_eq!(decide(&["*.GitHub.com"], &[], "API.github.com."), Allowed);
        assert_eq!(decide(&["github.com"], &[], " github.com "), Allowed);
        assert_eq!(decide(&["github.com"], &[], "github.com.."), Unlisted);
    }

    #[test]
    fn internationalised_names_match_their_punycode_form() {
        assert_eq!(
            decide(&["xn--bcher-kva.example"], &[], "bücher.example"),
            Allowed
        );
        assert_eq!(
            decide(&["bücher.example"], &[], "xn--bcher-kva.example"),
            Allowed
        );
        assert_eq!(
            decide(&["*.bücher.example"], &[], "www.xn--bcher-kva.example"),
            Allowed
        );
    }

    #[test]
    fn ip_hosts_match_only_identical_ip_entries() {
        assert_eq!(with_defaults("140.82.112.3"), Unlisted);
        assert_eq!(with_defaults("[2606:4700::1111]"), Unlisted);
        assert_eq!(decide(&["10.0.0.5"], &[], "10.0.0.5"), Allowed);
        assert_eq!(decide(&["10.0.0.5"], &[], "10.0.0.6"), Unlisted);
        assert_eq!(decide(&["2001:db8::1"], &[], "[2001:db8::1]"), Allowed);
        assert_eq!(decide(&["[2001:db8::1]"], &[], "2001:0db8:0:0::1"), Allowed);
        assert_eq!(decide(&["*.0.0.5"], &[], "10.0.0.5"), Unlisted);
        assert_eq!(decide(&["*"], &[], "10.0.0.5"), Unlisted);
    }

    #[test]
    fn empty_invalid_and_wildcard_hosts_are_unlisted() {
        for host in [
            "",
            " ",
            ".",
            "*.github.com",
            "*",
            "a b.com",
            "github.com:443",
            "a..com",
        ] {
            assert_eq!(
                decide(&["*.github.com", "*.com", "*"], &[], host),
                Unlisted,
                "{host:?}"
            );
        }
    }

    #[test]
    fn invalid_entries_match_nothing() {
        for entry in [
            "",
            "https://github.com",
            "github.com/x",
            "github.com:443",
            "git hub.com",
        ] {
            assert_eq!(decide(&[entry], &[], "github.com"), Unlisted, "{entry:?}");
        }
    }
}
