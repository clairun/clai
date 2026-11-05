//! The network state of one run: the agent's lists as the run started, the
//! hosts the user allowed for this run on top of them, and the hosts the run
//! tried to reach.

use std::sync::{Mutex, PoisonError, RwLock};

use serde::Serialize;

use super::matcher::{canonical_host, HostDecision, HostEntry, HostLists};
use crate::config::{NetworkMode, NetworkPolicy};

/// Enough for the network card's “This run” line; the oldest entry goes
/// first.
pub const MAX_HOST_LOG_ENTRIES: usize = 200;

/// One host the run tried to reach, and whether it was let through.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "bindings.ts")]
pub struct HostLogEntry {
    pub host: String,
    pub port: Option<u16>,
    pub allowed: bool,
}

#[derive(Debug, Default)]
pub struct RunNetwork {
    lists: RwLock<HostLists>,
    log: Mutex<Vec<HostLogEntry>>,
    has_ignored_entries: bool,
}

impl RunNetwork {
    /// Parses the agent's lists once for the whole run. In Allowlist, where
    /// the lists are read, an entry that does not parse is logged here once
    /// (only a hand edit can store one) and the run ends with a warning.
    pub fn new(policy: &NetworkPolicy) -> Self {
        let lists = HostLists::new(&policy.allow_domains, &policy.deny_domains);
        let in_use = policy.mode == NetworkMode::Allowlist;
        if in_use {
            for ignored in lists.ignored() {
                if ignored.denied {
                    tracing::error!(
                        "Network deny entry `{}` is invalid ({}), so every host is refused; fix it in the agent's Access settings",
                        ignored.entry,
                        ignored.reason
                    );
                } else {
                    tracing::warn!(
                        "Network allow entry `{}` is ignored ({}); fix it in the agent's Access settings",
                        ignored.entry,
                        ignored.reason
                    );
                }
            }
        }
        Self {
            has_ignored_entries: in_use && !lists.ignored().is_empty(),
            lists: RwLock::new(lists),
            log: Mutex::default(),
        }
    }

    /// Whether the run uses lists with an entry that was skipped.
    pub fn has_ignored_entries(&self) -> bool {
        self.has_ignored_entries
    }

    pub fn decide(&self, host: &str) -> HostDecision {
        self.lists
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .decide(host)
    }

    /// Allows `entry` for the rest of the run. The deny list still wins.
    pub fn allow(&self, entry: HostEntry) {
        self.lists
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .add_allow(entry);
    }

    /// Adds a host to the run's log, once per host and outcome.
    pub fn record(&self, host: &str, port: Option<u16>, allowed: bool) {
        let host = canonical_host(host).unwrap_or_else(|| host.trim().to_string());
        let mut log = self.log.lock().unwrap_or_else(PoisonError::into_inner);
        if log
            .iter()
            .any(|entry| entry.host == host && entry.allowed == allowed)
        {
            return;
        }
        if log.len() >= MAX_HOST_LOG_ENTRIES {
            log.remove(0);
        }
        log.push(HostLogEntry {
            host,
            port,
            allowed,
        });
    }

    pub fn host_log(&self) -> Vec<HostLogEntry> {
        self.log
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(mode: NetworkMode, allow: &[&str], deny: &[&str]) -> NetworkPolicy {
        NetworkPolicy {
            mode,
            allow_domains: allow.iter().map(|entry| entry.to_string()).collect(),
            deny_domains: deny.iter().map(|entry| entry.to_string()).collect(),
        }
    }

    fn logged(network: &RunNetwork) -> Vec<(String, bool)> {
        network
            .host_log()
            .into_iter()
            .map(|entry| (entry.host, entry.allowed))
            .collect()
    }

    #[test]
    fn the_run_decides_with_the_lists_it_started_with() {
        let network = RunNetwork::new(&policy(
            NetworkMode::Allowlist,
            &["*.example.org"],
            &["secret.example.org"],
        ));
        assert_eq!(network.decide("www.example.org"), HostDecision::Allowed);
        assert_eq!(
            network.decide("secret.example.org"),
            HostDecision::Denied {
                entry: "secret.example.org".to_string()
            }
        );
        assert_eq!(network.decide("example.com"), HostDecision::Unlisted);
    }

    #[test]
    fn a_host_allowed_for_the_run_never_overrides_the_deny_list() {
        let network = RunNetwork::new(&policy(
            NetworkMode::Allowlist,
            &[],
            &["*.s3.amazonaws.com"],
        ));
        let entry = |raw| crate::assistant::network::matcher::parse_entry(raw).unwrap();
        network.allow(entry("mirror.example.org"));
        network.allow(entry("*.amazonaws.com"));
        assert_eq!(network.decide("mirror.example.org"), HostDecision::Allowed);
        assert_eq!(network.decide("www.example.org"), HostDecision::Unlisted);
        assert_eq!(
            network.decide("ec2.us-east-1.amazonaws.com"),
            HostDecision::Allowed
        );
        assert_eq!(
            network.decide("bucket.s3.amazonaws.com"),
            HostDecision::Denied {
                entry: "*.s3.amazonaws.com".to_string()
            }
        );
    }

    #[test]
    fn the_log_keeps_one_entry_per_host_and_outcome() {
        let network = RunNetwork::default();
        network.record("GitHub.com.", Some(443), true);
        network.record("github.com", Some(80), true);
        network.record("github.com", Some(443), false);
        network.record("example.org", None, false);
        assert_eq!(
            logged(&network),
            [
                ("github.com".to_string(), true),
                ("github.com".to_string(), false),
                ("example.org".to_string(), false),
            ]
        );
    }

    #[test]
    fn the_log_drops_its_oldest_entry_when_full() {
        let network = RunNetwork::default();
        for index in 0..=MAX_HOST_LOG_ENTRIES {
            network.record(&format!("h{index}.example"), None, true);
        }
        let log = logged(&network);
        assert_eq!(log.len(), MAX_HOST_LOG_ENTRIES);
        assert_eq!(log[0].0, "h1.example");
        assert_eq!(
            log.last().unwrap().0,
            format!("h{MAX_HOST_LOG_ENTRIES}.example")
        );
    }

    #[test]
    fn an_invalid_deny_entry_refuses_every_host_this_run() {
        let network = RunNetwork::new(&policy(
            NetworkMode::Allowlist,
            &["github.com"],
            &["github.com:443"],
        ));
        network.allow(crate::assistant::network::matcher::parse_entry("example.org").unwrap());
        for host in ["github.com", "example.org"] {
            assert!(
                matches!(
                    network.decide(host),
                    HostDecision::InvalidDenyEntry { ref entry, .. } if entry == "github.com:443"
                ),
                "{host}"
            );
        }
    }

    #[test]
    fn invalid_entries_warn_only_where_the_lists_are_read() {
        let hand_edited = |mode| RunNetwork::new(&policy(mode, &["github.com"], &["x:443"]));
        assert!(hand_edited(NetworkMode::Allowlist).has_ignored_entries());
        assert!(!hand_edited(NetworkMode::HostNetwork).has_ignored_entries());
        assert!(!hand_edited(NetworkMode::Off).has_ignored_entries());
        assert!(
            !RunNetwork::new(&policy(NetworkMode::Allowlist, &["github.com"], &[]))
                .has_ignored_entries()
        );
    }
}
