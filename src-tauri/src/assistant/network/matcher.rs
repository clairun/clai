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

use super::addr::embedded_ipv4;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostDecision {
    Allowed,
    /// Refused by this deny-list entry, even if an allow entry covers it.
    Denied {
        entry: String,
    },
    /// Neither list covers the host, or it is not a valid host name.
    Unlisted,
    /// The deny list holds an entry that does not parse (only a hand edit
    /// stores one), so every host is refused until the user fixes it.
    InvalidDenyEntry {
        entry: String,
        reason: &'static str,
    },
}

impl HostDecision {
    /// Why `host` is refused, in the words the web tools and the network
    /// proxy give the agent; `None` when it is allowed. `can_ask` says
    /// whether `net_request_access` is offered to the agent.
    pub fn refusal(&self, host: &str, can_ask: bool) -> Option<String> {
        match self {
            Self::Allowed => None,
            Self::Denied { entry } => Some(format!(
                "`{host}` is in this agent's denied hosts (entry `{entry}`). The user can \
                 change them in the agent's Local access settings."
            )),
            Self::InvalidDenyEntry { entry, reason } => Some(format!(
                "This agent's denied hosts have an invalid entry `{entry}` ({reason}), so \
                 every host is refused until the user fixes or removes it in the agent's \
                 Local access settings."
            )),
            Self::Unlisted => {
                let way_out = if can_ask {
                    "Call `net_request_access` to ask the user, or they can add it in the \
                     agent's Local access settings."
                } else {
                    "The user can add it in the agent's Local access settings."
                };
                Some(format!(
                    "`{host}` is not in this agent's allowed hosts. {way_out}"
                ))
            }
        }
    }
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
    pub fn normalized(&self) -> String {
        match &self.host {
            Host::Domain(domain) => domain.clone(),
            Host::Ip(ip) => ip.to_string(),
        }
    }

    pub fn is_wildcard(&self) -> bool {
        has_wildcard(&self.host)
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

/// Parses a port on this computer, written `localhost:<port>`, or says why
/// text that names this computer is not one. `127.0.0.1:<port>` and
/// `[::1]:<port>` are accepted spellings of the same port: a grant is for
/// the program on the port, whichever loopback address a client tries
/// first. Every other spelling of this computer is refused: `localhost`, a
/// `*.localhost` name, or a loopback or unspecified IP, recognised after the
/// normalisation [`parse_entry`] applies (case, trailing dot, Unicode).
/// `Ok(None)` means the text is none of those, so it can only be a host
/// entry. Names that reach this computer only through name resolution, such
/// as `localhost.localdomain` or `ip6-localhost` in /etc/hosts, return
/// `Ok(None)`; the network proxy and the web tools refuse loopback addresses
/// when they connect.
pub fn parse_local_port(raw: &str) -> Result<Option<u16>, &'static str> {
    let value = raw.trim().to_lowercase();
    // `::1:5432` is itself a valid IPv6 address, so only brackets carry a port.
    if value.rsplit_once(':').is_some_and(|(ip, _)| {
        ip.parse::<Ipv6Addr>()
            .is_ok_and(|ip| is_this_computer(ip.into()))
    }) {
        return Err("put the IPv6 address in brackets, as in [::1]:5432");
    }
    let (host, ipv6, port) = if value.parse::<Ipv6Addr>().is_ok() {
        (value.as_str(), true, None)
    } else if let Some((host, rest)) = value
        .strip_prefix('[')
        .and_then(|inner| inner.split_once(']'))
    {
        match rest.strip_prefix(':') {
            Some(port) => (host, true, Some(port)),
            None if rest.is_empty() => (host, true, None),
            None => (value.as_str(), false, None),
        }
    } else {
        match value.split_once(':') {
            Some((host, port)) => (host, false, Some(port)),
            None => (value.as_str(), false, None),
        }
    };
    let alias = if ipv6 {
        host == "::1"
    } else {
        host == "localhost" || host == "127.0.0.1"
    };
    if !alias {
        return if names_this_computer(host) || names_this_computer(&value) {
            Err("write this computer as localhost, as in localhost:5432")
        } else {
            Ok(None)
        };
    }
    let port = port.unwrap_or_default();
    if port.is_empty() {
        return Err("add the port, as in localhost:5432");
    }
    if port.contains(['*', '-', ',']) {
        return Err("allow one port at a time, without ranges or wildcards");
    }
    if !port.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(PORT_RANGE);
    }
    if port.len() > 1 && port.starts_with('0') {
        return Err("write the port without leading zeros");
    }
    match port.parse() {
        Ok(0) | Err(_) => Err(PORT_RANGE),
        Ok(port) => Ok(Some(port)),
    }
}

const PORT_RANGE: &str = "the port is a number from 1 to 65535";

/// The names a client reaches a port on this computer by, as [`canonical_host`]
/// writes them. The network proxy serves them from the run's local ports,
/// and a deny entry for any of them refuses every local port.
pub const LOCAL_NAMES: [&str; 3] = [LOCALHOST, "127.0.0.1", "::1"];

/// How CLAI names a port on this computer, whichever of [`LOCAL_NAMES`] the
/// client used: in the run's log and in a request for the port.
pub const LOCALHOST: &str = "localhost";

/// Whether `host`, canonical, is one of [`LOCAL_NAMES`].
pub fn is_local_name(host: &str) -> bool {
    LOCAL_NAMES.contains(&host)
}

/// Whether `host`, read as [`parse_entry`] reads it, is `localhost`, a
/// `*.localhost` name or an address of this computer.
fn names_this_computer(host: &str) -> bool {
    parse_entry(host).is_ok_and(|entry| match entry.host {
        Host::Domain(name) => name == "localhost" || name.ends_with(".localhost"),
        Host::Ip(ip) => is_this_computer(ip),
    })
}

/// All of 127.0.0.0/8, `::1` and the unspecified addresses, which connect
/// locally, also when an IPv6 address embeds one ([`embedded_ipv4`]).
fn is_this_computer(ip: IpAddr) -> bool {
    let ip = match ip {
        IpAddr::V6(v6) => embedded_ipv4(v6).map_or(ip, IpAddr::V4),
        IpAddr::V4(_) => ip,
    };
    ip.is_loopback() || ip.is_unspecified()
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
    ignored: Vec<IgnoredEntry>,
}

/// A stored entry [`parse_entry`] refuses. Saving a config refuses those, so
/// one on disk was edited by hand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IgnoredEntry {
    pub entry: String,
    pub reason: &'static str,
    /// On the deny list: while it is there, every host is refused.
    pub denied: bool,
}

impl HostLists {
    /// Skips invalid entries, reported by [`Self::ignored`]. Both fail
    /// closed: an invalid allow entry allows nothing, and an invalid deny
    /// entry makes [`Self::decide`] refuse every host.
    pub fn new<S: AsRef<str>>(allow: &[S], deny: &[S]) -> Self {
        let mut ignored = Vec::new();
        let mut parse = |entries: &[S], denied: bool| {
            entries
                .iter()
                .filter_map(|raw| match parse_entry(raw.as_ref()) {
                    Ok(entry) => Some(entry),
                    Err(reason) => {
                        ignored.push(IgnoredEntry {
                            entry: raw.as_ref().to_string(),
                            reason,
                            denied,
                        });
                        None
                    }
                })
                .collect()
        };
        let allow = parse(allow, false);
        let deny = parse(deny, true);
        Self {
            allow,
            deny,
            ignored,
        }
    }

    pub fn ignored(&self) -> &[IgnoredEntry] {
        &self.ignored
    }

    /// Adds an allow entry. The deny list still wins over it.
    pub fn add_allow(&mut self, entry: HostEntry) {
        if !self
            .allow
            .iter()
            .any(|existing| existing.host == entry.host)
        {
            self.allow.push(entry);
        }
    }

    pub fn decide(&self, host: &str) -> HostDecision {
        if let Some(invalid) = self.ignored.iter().find(|ignored| ignored.denied) {
            return HostDecision::InvalidDenyEntry {
                entry: invalid.entry.clone(),
                reason: invalid.reason,
            };
        }
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

/// `raw` in the form the matcher compares (lower case, punycode, an IPv6
/// address without brackets), or `None` when it is not a host name.
pub fn canonical_host(raw: &str) -> Option<String> {
    match normalize(raw)? {
        Host::Domain(domain) => Some(domain),
        Host::Ip(ip) => Some(ip.to_string()),
    }
}

/// The wider entry the network card offers for `host`: `*.` and the host
/// without its first label, when [`approved_entry`] accepts it.
pub fn parent_wildcard(host: &str) -> Option<String> {
    let Some(Host::Domain(domain)) = normalize(host) else {
        return None;
    };
    let (_, parent) = domain.split_once('.')?;
    approved_entry(&domain, &format!("*.{parent}"))
        .ok()
        .map(|entry| entry.normalized())
}

/// The entry a user may add to the allow list when asked about `host`: the
/// host itself, or `*.P` covering it where `P` is a registrable domain or a
/// name under one (by the public suffix list), so a wildcard never spans a
/// public suffix such as `*.com`, `*.co.uk` or `*.github.io`. IP addresses
/// have no wildcard. The settings form is not held to this rule.
pub fn approved_entry(host: &str, entry: &str) -> Result<HostEntry, String> {
    let host_value = normalize(host)
        .filter(|host| !has_wildcard(host))
        .ok_or_else(|| format!("`{host}` is not a host name"))?;
    let approved = parse_entry(entry)
        .map_err(|reason| format!("`{entry}` is not a valid host entry: {reason}"))?;
    if approved.host == host_value {
        return Ok(approved);
    }
    let parent = match &approved.host {
        Host::Domain(domain) => domain.strip_prefix("*."),
        Host::Ip(_) => None,
    };
    let narrow_wildcard = parent
        .is_some_and(|parent| !parent.contains('*') && psl::domain(parent.as_bytes()).is_some());
    if narrow_wildcard && approved.matches(&host_value) {
        return Ok(approved);
    }
    Err(format!(
        "`{entry}` is neither `{host}` nor `*.` and a parent domain of it that is not a \
         public suffix"
    ))
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
    use crate::assistant::network::default_lists::DEFAULT_ALLOW_DOMAINS;
    use HostDecision::{Allowed, Unlisted};

    fn decide(allow: &[&str], deny: &[&str], host: &str) -> HostDecision {
        HostLists::new(allow, deny).decide(host)
    }

    fn with_defaults(host: &str) -> HostDecision {
        decide(DEFAULT_ALLOW_DOMAINS, &[], host)
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
            // No whole-provider wildcard: other cloud hosts, storage and
            // API gateways included, are asked for.
            ("ec2.us-east-1.amazonaws.com", Unlisted),
            ("abc123.execute-api.us-east-1.amazonaws.com", Unlisted),
            ("www.googleapis.com", Unlisted),
            ("s3.amazonaws.com", Unlisted),
            ("bucket.s3.amazonaws.com", Unlisted),
            ("bucket.s3.us-east-1.amazonaws.com", Unlisted),
            ("bucket.storage.googleapis.com", Unlisted),
            ("example.org", Unlisted),
            ("evilgithub.com", Unlisted),
            ("github.com.evil.org", Unlisted),
            ("duckduckgo.com", Unlisted),
        ] {
            assert_eq!(with_defaults(host), expected, "{host}");
        }
    }

    #[test]
    fn default_allow_list_has_no_duplicates_and_every_entry_parses() {
        let mut seen = std::collections::HashSet::new();
        for entry in DEFAULT_ALLOW_DOMAINS {
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
        // An interior wildcard in a deny entry, under a broad allow entry.
        let allow = ["*.amazonaws.com"];
        let deny = ["*.s3.*.amazonaws.com"];
        assert_eq!(
            decide(&allow, &deny, "bucket.s3.us-east-1.amazonaws.com"),
            denied("*.s3.*.amazonaws.com")
        );
        assert_eq!(
            decide(&allow, &deny, "ec2.us-east-1.amazonaws.com"),
            Allowed
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
    /// The settings form's `parseHostEntry` runs the same cases.
    #[test]
    fn entries_are_parsed_by_one_rule() {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Cases {
            accepted: Vec<Accepted>,
            rejected: Vec<String>,
            allow_only_rejected: Vec<String>,
        }
        #[derive(serde::Deserialize)]
        struct Accepted {
            entry: String,
            matched: String,
        }
        let cases: Cases = serde_json::from_str(include_str!(
            "../../../../src/components/Settings/hostEntryCases.json"
        ))
        .expect("host entry cases");
        for Accepted { entry, matched } in cases.accepted {
            assert_eq!(
                parse_entry(&entry).map(|entry| entry.normalized()),
                Ok(matched),
                "{entry:?}"
            );
        }
        for entry in cases.rejected {
            assert!(parse_entry(&entry).is_err(), "{entry:?}");
        }
        // The form refuses these on the allowed hosts only (`parseAllowEntry`,
        // AgentAccess.test.ts): denied, they refuse this computer.
        for entry in cases.allow_only_rejected {
            assert!(parse_entry(&entry).is_ok(), "{entry:?}");
            assert!(parse_local_port(&entry).is_err(), "{entry:?}");
        }
    }

    // Host entries must never parse as local ports, so the host cases run here too.
    #[test]
    fn local_ports_are_parsed_by_one_rule() {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Cases {
            accepted: Vec<HostCase>,
            local_ports: LocalPorts,
        }
        #[derive(serde::Deserialize)]
        struct HostCase {
            entry: String,
        }
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct LocalPorts {
            accepted: Vec<Accepted>,
            rejected: Vec<String>,
            not_local: Vec<String>,
        }
        #[derive(serde::Deserialize)]
        struct Accepted {
            entry: String,
            port: u16,
        }
        let cases: Cases = serde_json::from_str(include_str!(
            "../../../../src/components/Settings/hostEntryCases.json"
        ))
        .expect("host entry cases");
        let local = cases.local_ports;
        for Accepted { entry, port } in local.accepted {
            assert_eq!(parse_local_port(&entry), Ok(Some(port)), "{entry:?}");
        }
        for entry in local.rejected {
            assert!(parse_local_port(&entry).is_err(), "{entry:?}");
        }
        let hosts = cases.accepted.into_iter().map(|case| case.entry);
        for entry in local.not_local.into_iter().chain(hosts) {
            assert_eq!(parse_local_port(&entry), Ok(None), "{entry:?}");
        }
    }

    #[test]
    fn local_port_errors_say_what_to_write() {
        let no_port = "add the port, as in localhost:5432";
        let range = "the port is a number from 1 to 65535";
        let one_port = "allow one port at a time, without ranges or wildcards";
        let use_localhost = "write this computer as localhost, as in localhost:5432";
        let brackets = "put the IPv6 address in brackets, as in [::1]:5432";
        for (entry, reason) in [
            ("localhost", no_port),
            ("[::1]:", no_port),
            ("localhost:0", range),
            ("localhost:65536", range),
            ("localhost:05432", "write the port without leading zeros"),
            ("localhost:1-9", one_port),
            ("localhost:*", one_port),
            ("::1:5432", brackets),
            (":::80", brackets),
            ("64:ff9b::7f00:1:80", brackets),
            ("127.0.0.1.", use_localhost),
            ("[::1].", use_localhost),
            ("[0:0:0:0:0:0:0:1]:80", use_localhost),
            ("127.0.0.2:80", use_localhost),
            ("0.0.0.0:80", use_localhost),
            ("*.localhost:80", use_localhost),
            ("localhost.:80", use_localhost),
        ] {
            assert_eq!(parse_local_port(entry), Err(reason), "{entry:?}");
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
    }

    #[test]
    fn an_invalid_deny_entry_refuses_every_host() {
        let invalid = HostDecision::InvalidDenyEntry {
            entry: "github.com:443".to_string(),
            reason: "leave out the port: an entry matches every port",
        };
        for host in ["github.com", "example.org", "10.0.0.5"] {
            assert_eq!(
                decide(&["github.com", "*.org"], &["github.com:443"], host),
                invalid,
                "{host}"
            );
        }
        assert_eq!(decide(&["x/y"], &[], "github.com"), Unlisted);
    }

    #[test]
    fn invalid_entries_are_reported_with_their_list() {
        let lists = HostLists::new(&["github.com", "x/y"], &["github.com:443"]);
        let reported: Vec<(&str, bool)> = lists
            .ignored()
            .iter()
            .map(|ignored| (ignored.entry.as_str(), ignored.denied))
            .collect();
        assert_eq!(reported, [("x/y", false), ("github.com:443", true)]);
        assert!(HostLists::new(&["github.com"], &["*.s3.amazonaws.com"])
            .ignored()
            .is_empty());
    }

    #[test]
    fn the_card_offers_the_parent_domain_unless_it_is_a_public_suffix() {
        for (host, offer) in [
            ("mirror.example.org", Some("*.example.org")),
            ("a.b.example.org", Some("*.b.example.org")),
            ("Mirror.Example.ORG.", Some("*.example.org")),
            ("www.bücher.example", Some("*.xn--bcher-kva.example")),
            ("api.stripe.com", Some("*.stripe.com")),
            ("www.bbc.co.uk", Some("*.bbc.co.uk")),
            ("a.alice.github.io", Some("*.alice.github.io")),
            ("bbc.co.uk", None),
            ("alice.github.io", None),
            ("app.herokuapp.com", None),
            // On the list's private section, as GitHub registered it.
            ("raw.githubusercontent.com", None),
            ("bucket.s3.amazonaws.com", None),
            ("example.org", None),
            ("org", None),
            ("10.0.0.5", None),
            ("2001:db8::1", None),
            ("", None),
        ] {
            assert_eq!(parent_wildcard(host).as_deref(), offer, "{host:?}");
        }
    }

    #[test]
    fn an_approved_entry_is_the_host_or_a_parent_wildcard_below_a_public_suffix() {
        let host = "mirror.example.org";
        for (entry, normalized) in [
            ("mirror.example.org", "mirror.example.org"),
            ("MIRROR.example.org.", "mirror.example.org"),
            ("*.example.org", "*.example.org"),
        ] {
            assert_eq!(
                approved_entry(host, entry).map(|entry| entry.normalized()),
                Ok(normalized.to_string()),
                "{entry}"
            );
        }
        for entry in [
            "*.org",
            "*.other.org",
            "example.org",
            "*.*.org",
            "*.mirror.example.org",
            "other.example.org",
            "https://mirror.example.org",
        ] {
            assert!(approved_entry(host, entry).is_err(), "{entry}");
        }
        assert!(approved_entry("a.b.example.org", "*.example.org").is_ok());
        assert!(approved_entry("www.bbc.co.uk", "*.bbc.co.uk").is_ok());
        for (host, entry) in [
            ("www.bbc.co.uk", "*.co.uk"),
            ("alice.github.io", "*.github.io"),
            ("app.herokuapp.com", "*.herokuapp.com"),
            ("example.com", "*.com"),
        ] {
            assert!(approved_entry(host, entry).is_err(), "{entry}");
        }
        assert!(approved_entry("10.0.0.5", "10.0.0.5").is_ok());
        assert!(approved_entry("10.0.0.5", "10.0.0.*").is_err());
        assert!(approved_entry("10.0.0.5", "10.0.0.6").is_err());
        assert!(approved_entry("*.example.org", "*.example.org").is_err());
    }

    #[test]
    fn canonical_hosts_are_what_the_matcher_compares() {
        assert_eq!(canonical_host("GitHub.COM.").as_deref(), Some("github.com"));
        assert_eq!(
            canonical_host("bücher.example").as_deref(),
            Some("xn--bcher-kva.example")
        );
        assert_eq!(
            canonical_host("[2001:DB8::1]").as_deref(),
            Some("2001:db8::1")
        );
        assert_eq!(canonical_host("a..com"), None);
        assert_eq!(canonical_host(""), None);
    }

    #[test]
    fn a_refusal_names_the_host_and_the_way_out() {
        assert_eq!(HostDecision::Allowed.refusal("github.com", true), None);
        let unlisted = HostDecision::Unlisted.refusal("example.org", true).unwrap();
        assert!(unlisted.contains("`example.org`") && unlisted.contains("net_request_access"));
        let elsewhere = HostDecision::Unlisted
            .refusal("example.org", false)
            .unwrap();
        assert!(!elsewhere.contains("net_request_access"), "{elsewhere}");
        assert!(elsewhere.contains("Local access settings"), "{elsewhere}");
        let denied = HostDecision::Denied {
            entry: "*.s3.amazonaws.com".to_string(),
        }
        .refusal("b.s3.amazonaws.com", true)
        .unwrap();
        assert!(
            denied.contains("denied hosts (entry `*.s3.amazonaws.com`)"),
            "{denied}"
        );
        assert!(!denied.contains("net_request_access"), "{denied}");
    }
}
