//! Checks the splitter against the commands bash runs.
//!
//! `bash_runs.json` records, for each input, the names of the commands bash
//! 5.3 ran for it with an empty `PATH`, logged by `command_not_found_handle`.
//! Builtins and functions are not logged.

use crate::assistant::tools::local::{evaluate_command_policy, PolicyResult};
use crate::config::types::ShellCapabilityConfig;
use crate::config::{ExecutionCapabilityConfig, ShellAccessMode};

#[derive(serde::Deserialize)]
struct Case {
    input: String,
    runs: Vec<String>,
}

fn cases() -> Vec<Case> {
    serde_json::from_str(include_str!("bash_runs.json")).expect("bash_runs.json")
}

/// Every command bash runs is blocked by a blocklist entry naming it, or the
/// input is asked as one exact row.
#[test]
fn blocklist_sees_every_command_bash_runs() {
    let mut missed = Vec::new();
    for case in cases() {
        for name in &case.runs {
            let exec = ExecutionCapabilityConfig {
                shell: ShellCapabilityConfig {
                    mode: ShellAccessMode::Full,
                    allowed_command_prefixes: vec![],
                    blocked_command_prefixes: vec![name.clone()],
                },
                ..Default::default()
            };
            let seen = match evaluate_command_policy(&exec, &case.input, &[], &[]) {
                PolicyResult::Block { .. } => true,
                PolicyResult::NeedsApproval(rows) => {
                    matches!(rows.as_slice(), [row] if row.text == case.input.trim())
                }
                PolicyResult::Allow => false,
            };
            if !seen {
                missed.push(format!("{:?} runs {name:?}", case.input));
            }
        }
    }
    assert!(missed.is_empty(), "{missed:#?}");
}

/// Re-runs every recorded input in the local bash and compares what it runs.
/// Run with `cargo test --lib bash_runs -- --ignored`.
#[test]
#[ignore = "needs bash"]
fn recorded_runs_match_live_bash() {
    let bash = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|dir| dir.join("bash"))
        .find(|path| path.is_file())
        .expect("bash on PATH");
    let mut differ = Vec::new();
    for case in cases() {
        let dir = tempfile::tempdir().unwrap();
        // Commands write files relative to `cwd`, away from the log.
        let cwd = dir.path().join("cwd");
        std::fs::create_dir(&cwd).unwrap();
        let log = dir.path().join("log");
        let env = dir.path().join("env.sh");
        std::fs::write(
            &env,
            format!(
                "command_not_found_handle() {{ printf '%s\\0' \"$1\" >> '{}'; }}\n",
                log.display()
            ),
        )
        .unwrap();
        let mut child = std::process::Command::new(&bash)
            .args(["--norc", "--noprofile", "-c", &case.input])
            .current_dir(&cwd)
            .env_clear()
            .env("PATH", dir.path().join("empty"))
            .env("HOME", &cwd)
            .env("BASH_ENV", &env)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        // Stderr closes once bash and every command it started in the
        // background, such as a process substitution, have exited.
        let mut stderr = child.stderr.take().unwrap();
        let (done, exited) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = std::io::copy(&mut stderr, &mut std::io::sink());
            let _ = done.send(());
        });
        let _ = exited.recv_timeout(std::time::Duration::from_secs(2));
        let _ = child.kill();
        let _ = child.wait();
        let logged = std::fs::read(&log).unwrap_or_default();
        let mut runs: Vec<String> = String::from_utf8_lossy(&logged)
            .split('\0')
            .filter(|name| !name.is_empty())
            .map(String::from)
            .collect();
        runs.sort();
        runs.dedup();
        if runs != case.runs {
            differ.push(format!(
                "{:?}: recorded {:?}, bash {runs:?}",
                case.input, case.runs
            ));
        }
    }
    assert!(differ.is_empty(), "{differ:#?}");
}
