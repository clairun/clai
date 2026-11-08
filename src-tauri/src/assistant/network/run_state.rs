//! The network state of one run: the agent's lists as the run started, the
//! hosts the user allowed for this run on top of them, and the hosts the run
//! tried to reach.

use std::sync::{Mutex, PoisonError, RwLock};

use serde::Serialize;

#[cfg(target_os = "linux")]
use super::local_listener::Listener;
#[cfg(target_os = "linux")]
use super::matcher::LOCAL_NAMES;
use super::matcher::{canonical_host, is_local_name, HostDecision, HostEntry, HostLists};
#[cfg(target_os = "linux")]
use crate::config::types::LocalPortGrant;
use crate::config::{NetworkMode, NetworkPolicy};

/// Enough for the network card's “This run” line; the oldest entry goes
/// first.
pub const MAX_HOST_LOG_ENTRIES: usize = 200;

/// The `host:port` targets the network proxy refused for one command, once
/// each and in order. Past [`MAX_HOST_LOG_ENTRIES`] refusals are only
/// counted, so a crawler cannot fill the `bash_exec` result.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RefusedTargets {
    pub targets: Vec<String>,
    /// Refusals of unlisted targets after the list was full; a target
    /// refused twice then counts twice.
    pub more: usize,
}

impl RefusedTargets {
    #[cfg_attr(
        not(any(test, target_os = "linux")),
        expect(dead_code, reason = "only the Linux network proxy calls it")
    )]
    pub fn add(&mut self, target: String) {
        if self.targets.contains(&target) {
            return;
        }
        if self.targets.len() < MAX_HOST_LOG_ENTRIES {
            self.targets.push(target);
        } else {
            self.more += 1;
        }
    }

    pub fn is_empty(&self) -> bool {
        self.targets.is_empty()
    }
}

/// One host the run tried to reach, and whether it was let through.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "bindings.ts")]
pub struct HostLogEntry {
    pub host: String,
    pub port: Option<u16>,
    pub allowed: bool,
}

/// What must listen on a local port for the run to reach it.
#[cfg(target_os = "linux")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalBinding {
    /// The program at this path, as a full scan of `/proc` names it.
    Program(String),
    /// Any program of this other user, whose processes CLAI cannot see.
    User(u32),
    /// This listening socket, by inode, for one run: a listener that
    /// restarts has a new one.
    Socket(u64),
}

/// A local port the run may reach, and what must listen there.
#[cfg(target_os = "linux")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalRule {
    pub port: u16,
    pub binding: LocalBinding,
}

#[cfg(target_os = "linux")]
impl From<&LocalPortGrant> for LocalRule {
    fn from(grant: &LocalPortGrant) -> Self {
        let (port, binding) = match grant {
            LocalPortGrant::Program { port, program } => {
                (*port, LocalBinding::Program(program.clone()))
            }
            LocalPortGrant::User { port, uid, .. } => (*port, LocalBinding::User(*uid)),
        };
        Self { port, binding }
    }
}

/// Whether the run may reach a local port, by what listens there.
#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalDecision {
    Allowed,
    /// No rule of the run names the port.
    NotGranted,
    /// Rules name the port, but none covers what listens there now.
    Changed,
    /// CLAI, or a process it started, listens there: never reached.
    Clai,
    /// Nothing listens there.
    Nothing,
}

#[derive(Debug, Default)]
pub struct RunNetwork {
    lists: RwLock<HostLists>,
    /// The local ports the run may reach: the saved ones in Allowlist, and
    /// those the user allowed for the run.
    #[cfg(target_os = "linux")]
    local: RwLock<Vec<LocalRule>>,
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
                        "Denied hosts entry `{}` is invalid ({}), so every host is refused; fix it in the agent's Local access settings",
                        ignored.entry,
                        ignored.reason
                    );
                } else {
                    tracing::warn!(
                        "Allowed hosts entry `{}` is ignored ({}); fix it in the agent's Local access settings",
                        ignored.entry,
                        ignored.reason
                    );
                }
            }
        }
        Self {
            has_ignored_entries: in_use && !lists.ignored().is_empty(),
            lists: RwLock::new(lists),
            #[cfg(target_os = "linux")]
            local: RwLock::new(if in_use {
                policy.local_ports.iter().map(LocalRule::from).collect()
            } else {
                Vec::new()
            }),
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

    /// The deny decision that refuses every local port, with the name it
    /// refuses: an entry for `localhost`, `127.0.0.1` or `::1`, or an
    /// invalid entry. The proxy checks it before it looks at the port.
    #[cfg(target_os = "linux")]
    pub fn local_denial(&self) -> Option<(&'static str, HostDecision)> {
        let lists = self.lists.read().unwrap_or_else(PoisonError::into_inner);
        LOCAL_NAMES.into_iter().find_map(|name| {
            let decision = lists.decide(name);
            matches!(
                decision,
                HostDecision::Denied { .. } | HostDecision::InvalidDenyEntry { .. }
            )
            .then_some((name, decision))
        })
    }

    /// Allows `rule` for the rest of the run. The deny list still wins.
    #[cfg(target_os = "linux")]
    pub fn allow_local(&self, rule: LocalRule) {
        let mut local = self.local.write().unwrap_or_else(PoisonError::into_inner);
        if !local.contains(&rule) {
            local.push(rule);
        }
    }

    /// The local ports the run may reach now, in order.
    #[cfg(target_os = "linux")]
    pub fn local_ports(&self) -> Vec<u16> {
        let mut ports: Vec<u16> = self
            .local
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .map(|rule| rule.port)
            .collect();
        ports.sort_unstable();
        ports.dedup();
        ports
    }

    /// The programs the run may reach on `port`.
    #[cfg(target_os = "linux")]
    pub fn local_programs(&self, port: u16) -> Vec<String> {
        self.local_bindings(port)
            .into_iter()
            .filter_map(|binding| match binding {
                LocalBinding::Program(program) => Some(program),
                _ => None,
            })
            .collect()
    }

    /// Decides `port` by what listens there, after
    /// [`local_denial`](Self::local_denial). A program must be one a rule
    /// names by path. Sockets CLAI cannot tie to a program must each be one
    /// allowed for this run, or belong to the other user a rule names, never
    /// to the user of CLAI. While the user database cannot be read, another
    /// user is not named, so the listener is run-only and a user rule fails
    /// closed: the agent is refused and can ask for this run.
    #[cfg(target_os = "linux")]
    pub fn decide_local(&self, port: u16, listener: &Listener) -> LocalDecision {
        let rules = self.local_bindings(port);
        let allows_sockets = |sockets: &[u64]| {
            !sockets.is_empty()
                && sockets
                    .iter()
                    .all(|inode| rules.contains(&LocalBinding::Socket(*inode)))
        };
        let allowed = match listener {
            Listener::Nothing => return LocalDecision::Nothing,
            Listener::Clai => return LocalDecision::Clai,
            Listener::Program(program) => rules
                .iter()
                .any(|rule| matches!(rule, LocalBinding::Program(path) if *path == program.exe)),
            Listener::OtherUser { uid, sockets, .. } => {
                // SAFETY: geteuid has no preconditions and cannot fail.
                let own = *uid == unsafe { libc::geteuid() };
                (!own && rules.contains(&LocalBinding::User(*uid))) || allows_sockets(sockets)
            }
            Listener::RunOnly { sockets, .. } => allows_sockets(sockets),
        };
        if allowed {
            LocalDecision::Allowed
        } else if rules.is_empty() {
            LocalDecision::NotGranted
        } else {
            LocalDecision::Changed
        }
    }

    #[cfg(target_os = "linux")]
    fn local_bindings(&self, port: u16) -> Vec<LocalBinding> {
        self.local
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .filter(|rule| rule.port == port)
            .map(|rule| rule.binding.clone())
            .collect()
    }

    /// Adds a host to the run's log, once per host and outcome, and for a
    /// name of this computer once per port too.
    pub fn record(&self, host: &str, port: Option<u16>, allowed: bool) {
        let host = canonical_host(host).unwrap_or_else(|| host.trim().to_string());
        let per_port = is_local_name(&host);
        let mut log = self.log.lock().unwrap_or_else(PoisonError::into_inner);
        if log.iter().any(|entry| {
            entry.host == host && entry.allowed == allowed && (!per_port || entry.port == port)
        }) {
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
            local_ports: Vec::new(),
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
    fn the_log_keeps_one_entry_per_local_port() {
        let network = RunNetwork::default();
        network.record("localhost", Some(5432), true);
        network.record("localhost", Some(6379), true);
        network.record("LOCALHOST.", Some(5432), true);
        network.record("localhost", Some(5432), false);
        network.record("[::1]", Some(5432), true);
        network.record("::1", Some(6379), true);
        let log: Vec<(String, Option<u16>, bool)> = network
            .host_log()
            .into_iter()
            .map(|entry| (entry.host, entry.port, entry.allowed))
            .collect();
        let entry = |host: &str, port, allowed| (host.to_string(), Some(port), allowed);
        assert_eq!(
            log,
            [
                entry("localhost", 5432, true),
                entry("localhost", 6379, true),
                entry("localhost", 5432, false),
                entry("::1", 5432, true),
                entry("::1", 6379, true),
            ]
        );
    }

    #[cfg(target_os = "linux")]
    mod local {
        use super::*;
        use crate::assistant::network::local_listener::{Holder, Listener, Program, RunOnly};
        use crate::assistant::network::matcher::LOCAL_NAMES;
        use crate::config::types::LocalPortGrant;
        use LocalDecision::{Allowed, Changed, NotGranted};

        const PORT: u16 = 5432;
        const SSH: &str = "/usr/bin/ssh";
        const PYTHON: &str = "/usr/bin/python3";

        fn network(mode: NetworkMode, grants: Vec<LocalPortGrant>, deny: &[&str]) -> RunNetwork {
            RunNetwork::new(&NetworkPolicy {
                local_ports: grants,
                ..policy(mode, &[], deny)
            })
        }

        fn program_grant(program: &str) -> LocalPortGrant {
            LocalPortGrant::Program {
                port: PORT,
                program: program.to_string(),
            }
        }

        fn user_grant(uid: u32) -> LocalPortGrant {
            LocalPortGrant::User {
                port: PORT,
                uid,
                user: None,
            }
        }

        fn program(exe: &str) -> Listener {
            Listener::Program(Program {
                exe: exe.to_string(),
                pid: 300,
                cmdline: String::new(),
                interpreter: false,
                started_by: None,
            })
        }

        fn other_user(uid: u32, sockets: &[u64]) -> Listener {
            Listener::OtherUser {
                uid,
                name: "postgres".to_string(),
                sockets: sockets.to_vec(),
            }
        }

        fn run_only(reason: RunOnly, sockets: &[u64], holders: &[&str]) -> Listener {
            Listener::RunOnly {
                reason,
                sockets: sockets.to_vec(),
                holders: holders
                    .iter()
                    .map(|exe| Holder {
                        pid: 300,
                        exe: exe.to_string(),
                        cmdline: String::new(),
                    })
                    .collect(),
                started_by: None,
            }
        }

        fn assert_decisions<const N: usize>(
            network: &RunNetwork,
            cases: [(&str, u16, Listener, LocalDecision); N],
        ) {
            for (name, port, listener, expected) in cases {
                assert_eq!(network.decide_local(port, &listener), expected, "{name}");
            }
        }

        #[test]
        fn a_program_rule_allows_exactly_its_program_on_its_port() {
            let network = network(NetworkMode::Allowlist, vec![program_grant(SSH)], &[]);
            assert_eq!(network.local_programs(PORT), [SSH]);
            assert!(network.local_programs(PORT + 1).is_empty());
            assert_decisions(
                &network,
                [
                    ("the program", PORT, program(SSH), Allowed),
                    ("another program", PORT, program(PYTHON), Changed),
                    ("a longer path", PORT, program("/usr/bin/ssh2"), Changed),
                    (
                        "the program on another port",
                        PORT + 1,
                        program(SSH),
                        NotGranted,
                    ),
                    (
                        "the program beside another one",
                        PORT,
                        run_only(RunOnly::SeveralPrograms, &[11, 12], &[SSH, PYTHON]),
                        Changed,
                    ),
                    (
                        "the program in another namespace",
                        PORT,
                        run_only(RunOnly::OtherNamespace, &[11], &[SSH]),
                        Changed,
                    ),
                    ("another user", PORT, other_user(26, &[11]), Changed),
                    ("CLAI", PORT, Listener::Clai, LocalDecision::Clai),
                    ("nothing", PORT, Listener::Nothing, LocalDecision::Nothing),
                ],
            );
        }

        #[test]
        fn a_user_rule_allows_that_other_user_on_its_port_only() {
            // SAFETY: geteuid has no preconditions and cannot fail.
            let euid = unsafe { libc::geteuid() };
            let network = network(
                NetworkMode::Allowlist,
                vec![user_grant(26), user_grant(euid)],
                &[],
            );
            assert!(network.local_programs(PORT).is_empty());
            assert_decisions(
                &network,
                [
                    ("the user", PORT, other_user(26, &[11]), Allowed),
                    ("another user", PORT, other_user(27, &[11]), Changed),
                    (
                        "the user on another port",
                        PORT + 1,
                        other_user(26, &[11]),
                        NotGranted,
                    ),
                    ("a program CLAI can see", PORT, program(SSH), Changed),
                    ("the user of CLAI", PORT, other_user(euid, &[11]), Changed),
                    (
                        "the user, while its name cannot be looked up",
                        PORT,
                        run_only(RunOnly::Unseen, &[11], &[]),
                        Changed,
                    ),
                ],
            );
        }

        #[test]
        fn a_socket_rule_allows_only_while_every_socket_is_allowed() {
            let network = network(NetworkMode::Allowlist, Vec::new(), &[]);
            for inode in [11, 12] {
                network.allow_local(LocalRule {
                    port: PORT,
                    binding: LocalBinding::Socket(inode),
                });
            }
            let unseen = |sockets: &[u64]| run_only(RunOnly::Unseen, sockets, &[]);
            assert_decisions(
                &network,
                [
                    ("the socket", PORT, unseen(&[11]), Allowed),
                    ("both sockets", PORT, unseen(&[11, 12]), Allowed),
                    (
                        "another user on the socket",
                        PORT,
                        other_user(26, &[12]),
                        Allowed,
                    ),
                    ("a new socket", PORT, unseen(&[13]), Changed),
                    (
                        "an allowed socket beside a new one",
                        PORT,
                        unseen(&[11, 13]),
                        Changed,
                    ),
                    ("no socket", PORT, unseen(&[]), Changed),
                    (
                        "the socket on another port",
                        PORT + 1,
                        unseen(&[11]),
                        NotGranted,
                    ),
                    ("a program", PORT, program(SSH), Changed),
                ],
            );
        }

        #[test]
        fn saved_local_ports_apply_only_in_allowlist() {
            for (mode, expected) in [
                (NetworkMode::Allowlist, Allowed),
                (NetworkMode::HostNetwork, NotGranted),
                (NetworkMode::Off, NotGranted),
            ] {
                let network = network(mode, vec![program_grant(SSH)], &[]);
                assert_eq!(
                    network.decide_local(PORT, &program(SSH)),
                    expected,
                    "{mode:?}"
                );
            }
        }

        #[test]
        fn the_local_ports_are_listed_once_each_in_order() {
            let redis = LocalPortGrant::Program {
                port: 6379,
                program: "/usr/bin/redis-server".to_string(),
            };
            let network = network(NetworkMode::Allowlist, vec![redis, program_grant(SSH)], &[]);
            network.allow_local(LocalRule {
                port: PORT,
                binding: LocalBinding::Program(PYTHON.to_string()),
            });
            assert_eq!(network.local_ports(), [5432, 6379]);
        }

        #[test]
        fn a_deny_entry_for_any_local_name_refuses_every_local_port() {
            for name in LOCAL_NAMES {
                let network = network(NetworkMode::Allowlist, vec![program_grant(SSH)], &[name]);
                assert_eq!(
                    network.local_denial(),
                    Some((
                        name,
                        HostDecision::Denied {
                            entry: name.to_string()
                        }
                    )),
                    "{name}"
                );
            }
            let invalid = network(NetworkMode::Allowlist, Vec::new(), &["github.com:443"]);
            assert!(matches!(
                invalid.local_denial(),
                Some(("localhost", HostDecision::InvalidDenyEntry { .. }))
            ));
            let other = network(
                NetworkMode::Allowlist,
                Vec::new(),
                &["example.org", "127.0.0.2"],
            );
            assert_eq!(other.local_denial(), None);
        }
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
    fn refused_targets_are_listed_once_each_and_counted_past_the_cap() {
        let mut refused = RefusedTargets::default();
        refused.add("a.example:443".to_string());
        refused.add("a.example:443".to_string());
        assert_eq!(refused.targets, ["a.example:443"]);
        for index in 1..MAX_HOST_LOG_ENTRIES + 2 {
            refused.add(format!("h{index}.example:443"));
        }
        assert_eq!(refused.targets.len(), MAX_HOST_LOG_ENTRIES);
        assert_eq!(refused.more, 2);
        refused.add("a.example:443".to_string());
        assert_eq!(refused.more, 2, "a listed target is not counted again");
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
