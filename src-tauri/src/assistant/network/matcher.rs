//! The one host matcher behind the network allowlist.
//!
//! The web tools and the network proxy call it, and only in Allowlist mode.
//! Entries are host names; a leading `*` label stands for one or more labels
//! (`*.github.com` matches `api.github.com`, not `github.com`), an interior
//! `*` label for exactly one (`s3.*.amazonaws.com`). An IP host matches only
//! the identical IP entry. Ports are not part of an entry.
//!
//! The deny list always wins: it is checked first, and nothing granted later,
//! a host allowed for one run included, may override it. Grants only ever
//! add to the allow side.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

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

#[derive(Debug, Clone, PartialEq, Eq)]
enum Host {
    Domain(String),
    Ip(IpAddr),
}

/// One allow or deny entry, validated by [`parse_entry`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostEntry {
    /// As the user wrote it, for messages.
    text: String,
    host: Host,
}

impl HostEntry {
    /// The form the matcher compares: lower case, punycode, IPv6 without
    /// brackets.
    #[cfg(test)]
    fn normalized(&self) -> String {
        match &self.host {
            Host::Domain(domain) => domain.clone(),
            Host::Ip(ip) => ip.to_string(),
        }
    }

    fn matches(&self, host: &Host) -> bool {
        match (&self.host, host) {
            (Host::Ip(entry), Host::Ip(host)) => entry == host,
            (Host::Domain(entry), Host::Domain(host)) => {
                let entry: Vec<&str> = entry.split('.').collect();
                let host: Vec<&str> = host.split('.').collect();
                domain_matches(&entry, &host)
            }
            _ => false,
        }
    }
}

/// Parses an allow or deny entry, or says why it is not one. This is the
/// rule for what an entry may be: config saves refuse what it refuses, and
/// the settings form mirrors it in `parseHostEntry` (`AgentAccess.tsx`).
pub fn parse_entry(raw: &str) -> Result<HostEntry, &'static str> {
    let text = raw.trim();
    let value = text.strip_suffix('.').unwrap_or(text).to_lowercase();
    let entry = |host| HostEntry {
        text: text.to_string(),
        host,
    };
    if value.is_empty() {
        return Err("enter a host name");
    }
    if value.contains(char::is_whitespace) {
        return Err("a host name has no spaces");
    }
    if value.contains("://") {
        return Err("enter the host only, without http:// or https://");
    }
    if value.contains('/') {
        return Err("enter the host only, without a path");
    }
    if value.contains('@') {
        return Err("enter the host only, without a user name");
    }
    let bracketed = value
        .strip_prefix('[')
        .and_then(|inner| inner.strip_suffix(']'));
    let unbracketed = bracketed.unwrap_or(&value);
    if unbracketed.contains(':') {
        return match unbracketed.parse::<Ipv6Addr>() {
            Ok(ip) => Ok(entry(Host::Ip(ip.into()))),
            Err(_) if bracketed.is_some() => Err("not a valid IPv6 address"),
            Err(_) => Err("leave out the port: an entry matches every port"),
        };
    }
    // url::Host::parse percent-decodes, so `%2a` would become a wildcard.
    if value.contains('%') {
        return Err(INVALID_CHARACTERS);
    }
    let labels: Vec<&str> = value.split('.').collect();
    if labels.iter().any(|label| label.is_empty()) {
        return Err("a host name has no empty labels");
    }
    if labels.last().is_some_and(|label| ends_in_a_number(label)) {
        if value.contains('*') {
            return Err("a wildcard cannot cover IP addresses");
        }
        return value
            .parse::<Ipv4Addr>()
            .map(|ip| entry(Host::Ip(ip.into())))
            .map_err(|_| "not a valid IPv4 address; write it as four numbers, such as 10.0.0.5");
    }
    let Ok(url::Host::Domain(domain)) = url::Host::parse(&value) else {
        return Err(INVALID_CHARACTERS);
    };
    let labels: Vec<&str> = domain.split('.').collect();
    if labels.iter().any(|label| label.is_empty()) {
        return Err("a host name has no empty labels");
    }
    if labels
        .iter()
        .any(|label| label.contains('*') && *label != "*")
    {
        return Err("use * only as a whole label, as in *.example.com");
    }
    if labels.iter().all(|label| *label == "*") {
        return Err("it would allow every host; choose Host network instead");
    }
    if !labels.iter().all(|label| {
        *label == "*"
            || label
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    }) {
        return Err(INVALID_CHARACTERS);
    }
    Ok(entry(Host::Domain(domain)))
}

const INVALID_CHARACTERS: &str = "use only letters, digits, hyphens and dots";

/// A last label that makes URL parsers read the name as IPv4 (WHATWG
/// "ends in a number"): decimal digits, or `0x` and hex digits.
fn ends_in_a_number(label: &str) -> bool {
    let hex = label.strip_prefix("0x");
    match hex {
        Some(digits) => digits.bytes().all(|byte| byte.is_ascii_hexdigit()),
        None => !label.is_empty() && label.bytes().all(|byte| byte.is_ascii_digit()),
    }
}

/// An agent's allow and deny lists, parsed once so a host check compares
/// labels instead of re-parsing every entry.
#[derive(Debug, Clone, Default)]
pub struct HostLists {
    allow: Vec<HostEntry>,
    deny: Vec<HostEntry>,
}

impl HostLists {
    /// Saving a config refuses invalid entries, so one here was edited by
    /// hand. It is skipped: an allow entry then allows nothing, which fails
    /// closed; a deny entry refuses nothing, so that is logged as an error.
    pub fn new<S: AsRef<str>>(allow: &[S], deny: &[S]) -> Self {
        let parse = |entries: &[S], denied: bool| {
            entries
                .iter()
                .filter_map(|raw| match parse_entry(raw.as_ref()) {
                    Ok(entry) => Some(entry),
                    Err(reason) if denied => {
                        tracing::error!(
                            "Network deny entry `{}` is ignored and refuses nothing ({reason}); fix it in the agent's Access settings",
                            raw.as_ref()
                        );
                        None
                    }
                    Err(reason) => {
                        tracing::warn!(
                            "Network allow entry `{}` is ignored ({reason})",
                            raw.as_ref()
                        );
                        None
                    }
                })
                .collect()
        };
        Self {
            allow: parse(allow, false),
            deny: parse(deny, true),
        }
    }

    pub fn decide(&self, host: &str) -> HostDecision {
        let Some(host) = normalize(host).filter(|host| !has_wildcard(host)) else {
            return HostDecision::Unlisted;
        };
        if let Some(entry) = self.deny.iter().find(|entry| entry.matches(&host)) {
            return HostDecision::Denied {
                entry: entry.text.clone(),
            };
        }
        if self.allow.iter().any(|entry| entry.matches(&host)) {
            return HostDecision::Allowed;
        }
        HostDecision::Unlisted
    }
}

/// A host as a URL or a CONNECT line carries it: lower case, one trailing
/// dot dropped, IDNA to punycode, IP literals (with or without IPv6
/// brackets) parsed. `None` for an empty or invalid host.
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
        HostLists::new(allow, deny).decide(host)
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

    /// A user who adds a whole provider still cannot reach the storage
    /// endpoints the deny list names (best effort, see its doc).
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
            ("s3-fips.us-east-1.amazonaws.com", "s3-fips.*.amazonaws.com"),
            (
                "bucket.s3-fips.us-east-1.amazonaws.com",
                "*.s3-fips.*.amazonaws.com",
            ),
            (
                "s3-fips.dualstack.us-east-1.amazonaws.com",
                "s3-fips.dualstack.*.amazonaws.com",
            ),
            (
                "bucket.s3-fips.dualstack.us-east-1.amazonaws.com",
                "*.s3-fips.dualstack.*.amazonaws.com",
            ),
            (
                "ap-123456789012.s3-accesspoint.us-east-1.amazonaws.com",
                "*.s3-accesspoint.*.amazonaws.com",
            ),
            (
                "ap-123456789012.s3-accesspoint.dualstack.us-east-1.amazonaws.com",
                "*.s3-accesspoint.dualstack.*.amazonaws.com",
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
            assert!(parse_entry(entry).is_ok(), "{entry}");
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

    // The same cases as `parses host entries` in AgentBehaviorForm.test.tsx.
    #[test]
    fn entries_are_parsed_by_one_rule() {
        for (raw, normalized) in [
            ("github.com", "github.com"),
            (" GitHub.COM. ", "github.com"),
            ("*.github.com", "*.github.com"),
            ("s3.*.amazonaws.com", "s3.*.amazonaws.com"),
            ("*.com", "*.com"),
            ("_dmarc.example.com", "_dmarc.example.com"),
            ("bücher.example", "xn--bcher-kva.example"),
            ("10.0.0.5", "10.0.0.5"),
            ("[2001:db8::1]", "2001:db8::1"),
            ("2001:0db8:0:0::1", "2001:db8::1"),
        ] {
            assert_eq!(
                parse_entry(raw).map(|entry| entry.normalized()),
                Ok(normalized.to_string()),
                "{raw:?}"
            );
        }
        for raw in [
            "",
            "*",
            "*.*",
            "*.*.*",
            "github.com:443",
            "ab:443",
            "abc:def:1",
            "[foo]",
            "[::1]:443",
            "git@github.com",
            "github.com?x",
            "github.com#x",
            "a|b.com",
            "a^b.com",
            "a!b.com",
            "%2a.github.com",
            "999.1.1.1",
            "example.123",
            "example.0x1",
            "2130706433",
            "010.0.0.5",
            "*.0.0.5",
            "https://github.com",
            "github.com/x",
            "git hub.com",
            "a..com",
            "gi*thub.com",
        ] {
            assert!(parse_entry(raw).is_err(), "{raw:?}");
        }
    }

    #[test]
    fn invalid_entries_match_nothing() {
        for entry in [
            "",
            "*",
            "*.*",
            "https://github.com",
            "github.com/x",
            "github.com:443",
            "git hub.com",
            "git@github.com",
        ] {
            assert_eq!(decide(&[entry], &[], "github.com"), Unlisted, "{entry:?}");
        }
        // An invalid deny entry refuses nothing (it is logged instead).
        assert_eq!(
            decide(&["github.com"], &["github.com:443"], "github.com"),
            Allowed
        );
    }
}
