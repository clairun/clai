//! Command-line launch requests, shared by cold startup and single-instance handoff.

use crate::workspace_index::{WorkspaceIndex, WorkspaceLoadFailure};
use crate::AppConfig;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Focus,
    Open(String),
    New,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchRequest {
    Focus,
    Open(String),
    New,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliError {
    pub code: i32,
    pub message: String,
}

impl CliError {
    fn usage(message: impl Into<String>) -> Self {
        Self {
            code: 2,
            message: message.into(),
        }
    }
    fn operation(message: impl Into<String>) -> Self {
        Self {
            code: 1,
            message: message.into(),
        }
    }
}

pub fn parse(args: &[String]) -> Result<Command, CliError> {
    match args {
        [] => Ok(Command::Focus),
        [open, name] if open == "open" => Ok(Command::Open(name.clone())),
        [new] if new == "new" => Ok(Command::New),
        [open] if open == "open" => Err(CliError::usage("usage: clai open <workspace>")),
        _ => Err(CliError::usage("usage: clai [open <workspace>|new]")),
    }
}

pub fn resolve(
    command: Command,
    config: &AppConfig,
    index: &WorkspaceIndex,
) -> Result<LaunchRequest, CliError> {
    match command {
        Command::Focus => Ok(LaunchRequest::Focus),
        Command::New => {
            if config.expanded_workspace_dirs().len() != 1 {
                return Err(CliError::operation("Workspace directory must be selected."));
            }
            Ok(LaunchRequest::New)
        }
        Command::Open(name) => resolve_open(&name, index).map(LaunchRequest::Open),
    }
}

fn resolve_open(name: &str, index: &WorkspaceIndex) -> Result<String, CliError> {
    let all: Vec<_> = index.by_id.values().collect();
    let starred: Vec<_> = all.iter().copied().filter(|ws| ws.starred_at > 0).collect();
    if let Some(error) = index.scan_errors.first() {
        return Err(CliError::operation(error.clone()));
    }
    // A failed config can hide its star or title. In particular, a full UUID
    // naming that directory must never be reported as an absent workspace.
    if let Some(failure) = index
        .load_failures
        .iter()
        .find(|failure| failure_dir(failure) == Some(name))
    {
        return Err(CliError::operation(format!(
            "Cannot load workspace {}",
            describe_failure(failure)
        )));
    }
    // Exact IDs take precedence over prefixes and titles.
    let exact = all.iter().copied().find(|ws| ws.id == name);
    if let Some(ws) = exact {
        return if ws.starred_at > 0 {
            Ok(ws.id.clone())
        } else {
            Err(unstarred(name))
        };
    }
    if name.len() >= 8 {
        // Workspace directories are named by id, so only a failure whose
        // directory starts with the prefix can hide a match.
        let hidden: Vec<_> = index
            .load_failures
            .iter()
            .filter(|failure| failure_dir(failure).is_some_and(|dir| dir.starts_with(name)))
            .collect();
        if !hidden.is_empty() {
            return Err(incomplete_lookup(name, &hidden));
        }
        let matches: Vec<_> = starred
            .iter()
            .copied()
            .filter(|ws| ws.id.starts_with(name))
            .collect();
        if let Some(id) = unique_or_ambiguous(name, matches)? {
            return Ok(id);
        }
    }
    // Any failed config could carry this title, so even a unique match among
    // the loaded ones may be ambiguous.
    if !index.load_failures.is_empty() {
        let failures: Vec<_> = index.load_failures.iter().collect();
        return Err(incomplete_lookup(name, &failures));
    }
    let matches: Vec<_> = starred
        .iter()
        .copied()
        .filter(|ws| ws.title.to_lowercase() == name.to_lowercase())
        .collect();
    if let Some(id) = unique_or_ambiguous(name, matches)? {
        return Ok(id);
    }
    if all.iter().any(|ws| {
        ws.starred_at <= 0
            && ((name.len() >= 8 && ws.id.starts_with(name))
                || ws.title.to_lowercase() == name.to_lowercase())
    }) {
        return Err(unstarred(name));
    }
    Err(CliError::usage(format!(
        "clai: no starred workspace matches \"{name}\"."
    )))
}

fn unique_or_ambiguous(
    name: &str,
    matches: Vec<&crate::workspace_index::WorkspaceLocator>,
) -> Result<Option<String>, CliError> {
    match matches.as_slice() {
        [] => Ok(None),
        [ws] => Ok(Some(ws.id.clone())),
        _ => {
            let mut candidates: Vec<_> = matches
                .iter()
                .map(|ws| format!("  {}  {}", ws.id, ws.title))
                .collect();
            candidates.sort();
            Err(CliError::usage(format!("clai: \"{name}\" matches {} starred workspaces:\n{}\nRun clai open <id> to choose one.", candidates.len(), candidates.join("\n"))))
        }
    }
}

fn failure_dir(failure: &WorkspaceLoadFailure) -> Option<&str> {
    failure.path.file_name().and_then(|dir| dir.to_str())
}

fn describe_failure(failure: &WorkspaceLoadFailure) -> String {
    format!("{}: {}", failure.path.display(), failure.reason)
}

fn incomplete_lookup(name: &str, failures: &[&WorkspaceLoadFailure]) -> CliError {
    const SHOWN: usize = 3;
    let mut lines: Vec<_> = failures
        .iter()
        .take(SHOWN)
        .map(|failure| format!("  {}", describe_failure(failure)))
        .collect();
    if failures.len() > SHOWN {
        lines.push(format!("  ...and {} more", failures.len() - SHOWN));
    }
    CliError::operation(format!(
        "clai: cannot resolve \"{name}\" because these workspaces failed to load:\n{}\nFix or remove them, or open by full workspace id.",
        lines.join("\n")
    ))
}

fn unstarred(name: &str) -> CliError {
    CliError::usage(format!(
        "clai: workspace \"{name}\" is not starred. Star it in CLAI to use clai open."
    ))
}

pub fn argv_request(
    args: &[String],
    config: &AppConfig,
    index: &WorkspaceIndex,
) -> Result<LaunchRequest, CliError> {
    resolve(parse(args)?, config, index)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::workspace_config::WorkspaceConfig;
    use crate::workspace_index::LoadFailureReason;
    use std::path::PathBuf;

    const A: &str = "12345678-0000-0000-0000-000000000001";
    const B: &str = "12345678-0000-0000-0000-000000000002";
    const C: &str = "87654321-0000-0000-0000-000000000003";

    fn fixture() -> (AppConfig, WorkspaceIndex) {
        let mut index = WorkspaceIndex::default();
        for (id, title, starred) in [(A, "Research", 10), (B, "research", 20), (C, "Private", 0)] {
            let mut ws = WorkspaceConfig::new(id.into(), title.into(), 1, "manager".into());
            ws.starred_at = starred;
            index.insert_config(PathBuf::from(id), &ws);
        }
        (
            AppConfig {
                workspace_dirs: vec![PathBuf::from("/tmp/workspaces")],
                ..Default::default()
            },
            index,
        )
    }

    #[test]
    fn parser_rejects_missing_and_extra_arguments() {
        assert_eq!(parse(&[]), Ok(Command::Focus));
        assert_eq!(parse(&["new".into()]), Ok(Command::New));
        assert_eq!(
            parse(&["open".into(), A.into()]),
            Ok(Command::Open(A.into()))
        );
        assert_eq!(parse(&["open".into()]).unwrap_err().code, 2);
        assert_eq!(parse(&["new".into(), "extra".into()]).unwrap_err().code, 2);
    }

    #[test]
    fn resolves_starred_id_prefix_and_title_in_order() {
        let (config, mut index) = fixture();
        assert_eq!(
            resolve(Command::Open(A.into()), &config, &index),
            Ok(LaunchRequest::Open(A.into()))
        );
        assert_eq!(
            resolve(Command::Open("12345678".into()), &config, &index)
                .unwrap_err()
                .code,
            2
        );
        assert!(resolve(Command::Open("1234567".into()), &config, &index)
            .unwrap_err()
            .message
            .contains("no starred workspace"));
        assert_eq!(
            resolve(Command::Open("87654321".into()), &config, &index)
                .unwrap_err()
                .code,
            2
        );
        assert_eq!(
            resolve(Command::Open("RESEARCH".into()), &config, &index)
                .unwrap_err()
                .code,
            2
        );
        index.by_id.remove(B);
        assert_eq!(
            resolve(
                Command::Open("12345678-0000-0000-0000-00000000000".into()),
                &config,
                &index
            ),
            Ok(LaunchRequest::Open(A.into()))
        );
        assert_eq!(
            resolve(Command::Open("research".into()), &config, &index),
            Ok(LaunchRequest::Open(A.into()))
        );
        assert_eq!(
            resolve(Command::Open(C.into()), &config, &index)
                .unwrap_err()
                .code,
            2
        );
        assert_eq!(
            resolve(Command::Open("Private".into()), &config, &index)
                .unwrap_err()
                .code,
            2
        );
    }

    #[test]
    fn title_matching_handles_unicode_case() {
        let (config, mut index) = fixture();
        let id = "aaaaaaaa-0000-0000-0000-000000000000";
        let mut ws = WorkspaceConfig::new(id.into(), "Café".into(), 1, "manager".into());
        ws.starred_at = 1;
        index.insert_config(PathBuf::from(id), &ws);
        assert_eq!(
            resolve(Command::Open("CAFÉ".into()), &config, &index),
            Ok(LaunchRequest::Open(id.into()))
        );
    }

    #[test]
    fn scan_failures_are_operational_errors() {
        let (config, mut index) = fixture();
        index.scan_errors.push("Cannot scan root".into());
        assert_eq!(
            resolve(Command::Open("missing".into()), &config, &index)
                .unwrap_err()
                .code,
            1
        );
        index.scan_errors.clear();
        index.load_failures.push(WorkspaceLoadFailure {
            path: PathBuf::from("/tmp/workspaces/aaaaaaaa-0000-0000-0000-000000000000"),
            reason: LoadFailureReason::InvalidConfig { line: 1, column: 1 },
        });
        assert_eq!(
            resolve(
                Command::Open("aaaaaaaa-0000-0000-0000-000000000000".into()),
                &config,
                &index
            )
            .unwrap_err()
            .code,
            1
        );
        assert_eq!(
            resolve(Command::Open("unknown".into()), &config, &index)
                .unwrap_err()
                .code,
            1
        );
    }

    fn failed_path(dir: &str) -> PathBuf {
        PathBuf::from("/tmp/workspaces").join(dir)
    }

    fn fail(index: &mut WorkspaceIndex, dir: &str) {
        index.load_failures.push(WorkspaceLoadFailure {
            path: failed_path(dir),
            reason: LoadFailureReason::InvalidConfig { line: 1, column: 1 },
        });
    }

    #[test]
    fn unrelated_load_failure_does_not_block_id_or_prefix() {
        let (config, mut index) = fixture();
        index.by_id.remove(B);
        fail(&mut index, "aaaaaaaa-0000-0000-0000-000000000000");
        let open = |name: &str| resolve(Command::Open(name.into()), &config, &index);
        assert_eq!(open(A), Ok(LaunchRequest::Open(A.into())));
        assert_eq!(open("12345678"), Ok(LaunchRequest::Open(A.into())));
        assert_eq!(open(C).unwrap_err().code, 2);
    }

    #[test]
    fn load_failure_under_prefix_blocks_prefix_but_not_exact_id() {
        let (config, mut index) = fixture();
        index.by_id.remove(B);
        let dir = "12345678-ffff-0000-0000-000000000000";
        fail(&mut index, dir);
        let open = |name: &str| resolve(Command::Open(name.into()), &config, &index);
        let error = open("12345678").unwrap_err();
        assert_eq!(error.code, 1);
        assert!(error
            .message
            .contains(&failed_path(dir).display().to_string()));
        assert_eq!(open(A), Ok(LaunchRequest::Open(A.into())));
    }

    #[test]
    fn load_failure_blocks_every_title_lookup() {
        let (config, mut index) = fixture();
        index.by_id.remove(B);
        fail(&mut index, "aaaaaaaa-0000-0000-0000-000000000000");
        for name in ["research", "Private", "unknown"] {
            let error = resolve(Command::Open(name.into()), &config, &index).unwrap_err();
            assert_eq!(error.code, 1, "{name}");
            assert!(error.message.contains("aaaaaaaa-0000"), "{name}");
        }
    }

    #[test]
    fn many_load_failures_are_summarised() {
        let (config, mut index) = fixture();
        for n in 0..5 {
            fail(
                &mut index,
                &format!("aaaaaaaa-0000-0000-0000-00000000000{n}"),
            );
        }
        let message = resolve(Command::Open("unknown".into()), &config, &index)
            .unwrap_err()
            .message;
        assert!(message.contains("00000000002: invalid config.json at line 1 column 1"));
        assert!(!message.contains("00000000003"));
        assert!(message.contains("...and 2 more"));
    }

    #[test]
    fn new_requires_one_root_and_focus_is_not_creation() {
        let (mut config, index) = fixture();
        assert_eq!(
            resolve(Command::Focus, &config, &index),
            Ok(LaunchRequest::Focus)
        );
        assert_eq!(
            resolve(Command::New, &config, &index),
            Ok(LaunchRequest::New)
        );
        config.workspace_dirs.push(PathBuf::from("/tmp/second"));
        assert_eq!(resolve(Command::New, &config, &index).unwrap_err().code, 1);
    }
}
