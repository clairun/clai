//! Command-line launch requests, shared by cold startup and single-instance handoff.

use crate::workspace_index::WorkspaceIndex;
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
        .find(|failure| failure.path.file_name().and_then(|s| s.to_str()) == Some(name))
    {
        return Err(CliError::operation(format!(
            "Cannot load workspace {}: {:?}",
            failure.path.display(),
            failure.reason
        )));
    }
    if !index.load_failures.is_empty() {
        return Err(CliError::operation("Cannot complete workspace lookup because some workspace configurations failed to load."));
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
        let matches: Vec<_> = starred
            .iter()
            .copied()
            .filter(|ws| ws.id.starts_with(name))
            .collect();
        if let Some(id) = unique_or_ambiguous(name, matches)? {
            return Ok(id);
        }
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
    use crate::workspace_index::{LoadFailureReason, WorkspaceLoadFailure};
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
            reason: LoadFailureReason::Unparseable("bad json".into()),
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
