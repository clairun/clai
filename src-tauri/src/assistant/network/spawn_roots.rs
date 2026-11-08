//! The programs CLAI starts for the user: a shell in its terminal, an app
//! opened from it, an agent's command and an MCP server. A port that one of
//! them, or a process under it, listens on can be asked for, and the card
//! says who started it (see `local_listener`). Every other process under
//! CLAI, such as an agent engine, does CLAI's own work and is never reached.
//!
//! Only Linux has `/proc/<pid>/stat`, so elsewhere nothing is recorded.

use std::collections::HashMap;
use std::fs;
use std::future::Future;
use std::sync::{LazyLock, Mutex, MutexGuard, PoisonError};

/// Who started a program that listens on a port. For the card only: never
/// part of a grant or of what a grant matches.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(
    not(target_os = "linux"),
    allow(dead_code, reason = "only Linux finds what listens on a port")
)]
pub enum StartedBy {
    /// A shell in the CLAI terminal.
    Terminal,
    /// An app opened from CLAI, by program name.
    App(String),
    /// An agent's shell command, by agent name.
    Agent(Option<String>),
    /// An MCP server CLAI runs, by its configured name.
    McpServer(String),
}

type Programs = HashMap<u32, (u64, StartedBy)>;

/// By pid: the start time and who started the program.
static RECORDED: LazyLock<Mutex<Programs>> = LazyLock::new(Mutex::default);

tokio::task_local! {
    /// The agent whose shell command the current task runs.
    static AGENT: Option<String>;
}

fn programs() -> MutexGuard<'static, Programs> {
    RECORDED.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Keeps a program recorded until dropped, which its owner does when it reaps
/// the program or lets it go. A later program given the same pid keeps its
/// own record.
#[must_use = "dropping it forgets the program"]
pub struct Recorded(Option<(u32, u64)>);

/// Records `pid`, a child CLAI just started, as started by `by`.
pub fn record(pid: Option<u32>, by: StartedBy) -> Recorded {
    let Some((pid, start)) = pid.and_then(|pid| Some((pid, start_time(pid)?))) else {
        return Recorded(None);
    };
    programs().insert(pid, (start, by));
    Recorded(Some((pid, start)))
}

impl Drop for Recorded {
    fn drop(&mut self) {
        let Some((pid, start)) = self.0 else {
            return;
        };
        let mut programs = programs();
        if programs
            .get(&pid)
            .is_some_and(|(recorded, _)| *recorded == start)
        {
            programs.remove(&pid);
        }
    }
}

/// Runs `run` with the shell commands it starts recorded as agent `name`'s.
pub async fn as_agent<F: Future>(name: Option<String>, run: F) -> F::Output {
    AGENT.scope(name, run).await
}

/// Records `pid` as a shell command of the agent [`as_agent`] names.
#[cfg_attr(
    not(target_os = "linux"),
    allow(dead_code, reason = "only Linux finds what listens on a port")
)]
pub fn record_agent_command(pid: Option<u32>) -> Recorded {
    let agent = AGENT.try_with(Clone::clone).ok().flatten();
    record(pid, StartedBy::Agent(agent))
}

/// The program recorded at `pid`: its start time and who started it.
#[cfg_attr(
    not(target_os = "linux"),
    allow(dead_code, reason = "only Linux finds what listens on a port")
)]
pub fn recorded(pid: u32) -> Option<(u64, StartedBy)> {
    programs().get(&pid).cloned()
}

/// The pids recorded as started by `by`.
#[cfg(all(test, target_os = "linux"))]
pub fn started_by(by: &StartedBy) -> Vec<u32> {
    programs()
        .iter()
        .filter(|(_, (_, recorded))| recorded == by)
        .map(|(pid, _)| *pid)
        .collect()
}

/// Whether `check` holds within five seconds.
#[cfg(all(test, target_os = "linux"))]
pub async fn eventually(mut check: impl FnMut() -> bool) -> bool {
    for _ in 0..250 {
        if check() {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    check()
}

/// When `pid` started, which tells it from a later process given its pid.
pub fn start_time(pid: u32) -> Option<u64> {
    start_time_in(&fs::read_to_string(format!("/proc/{pid}/stat")).ok()?)
}

/// Field 22 of `pid (comm) state ppid …`, where comm may hold spaces and `)`.
fn start_time_in(stat: &str) -> Option<u64> {
    let (_, rest) = stat.rsplit_once(")")?;
    rest.split_whitespace().nth(19)?.parse().ok()
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use std::process::{Child, Command};

    use super::*;

    fn sleep() -> Child {
        Command::new("sleep").arg("30").spawn().unwrap()
    }

    fn stop(mut child: Child) {
        child.kill().unwrap();
        child.wait().unwrap();
    }

    #[test]
    fn the_start_time_is_field_22_after_the_last_parenthesis() {
        let stat = "42 (a) b) S 1 42 42 0 -1 4194560 100 0 0 0 1 2 0 0 20 0 1 0 98765 1000 10";
        assert_eq!(start_time_in(stat), Some(98765));
        assert_eq!(start_time_in("42 (a) S 1 42"), None);
    }

    #[test]
    fn a_program_is_recorded_until_its_record_drops() {
        let child = sleep();
        let pid = child.id();
        let start = start_time(pid).expect("a start time");
        let kept = record(Some(pid), StartedBy::Terminal);
        assert_eq!(recorded(pid), Some((start, StartedBy::Terminal)));
        drop(kept);
        assert_eq!(recorded(pid), None);
        stop(child);
    }

    #[test]
    fn a_late_drop_keeps_a_later_program_given_the_same_pid() {
        let child = sleep();
        let pid = child.id();
        let old = record(Some(pid), StartedBy::Terminal);
        let later = start_time(pid).expect("a start time") + 1;
        let app = || StartedBy::App("kitty".to_string());
        programs().insert(pid, (later, app()));
        drop(old);
        assert_eq!(recorded(pid), Some((later, app())));
        drop(Recorded(Some((pid, later))));
        assert_eq!(recorded(pid), None);
        stop(child);
    }

    #[tokio::test]
    async fn an_agent_command_is_recorded_with_the_agent_name() {
        let child = sleep();
        let pid = child.id();
        let agent = |name: Option<&str>| Some(StartedBy::Agent(name.map(str::to_string)));
        let named = as_agent(Some("Ana".to_string()), async {
            record_agent_command(Some(pid))
        })
        .await;
        assert_eq!(recorded(pid).map(|(_, by)| by), agent(Some("Ana")));
        drop(named);
        let unnamed = record_agent_command(Some(pid));
        assert_eq!(recorded(pid).map(|(_, by)| by), agent(None));
        drop(unnamed);
        stop(child);
    }
}
