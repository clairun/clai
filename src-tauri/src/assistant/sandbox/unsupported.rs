use tokio::process::Command;

use super::runner::{prepare_stdio, run_spawned_child};
use super::SandboxCommand;
use crate::windows_console::HideConsoleWindow;

pub async fn run(command: SandboxCommand) -> Result<super::SandboxCommandOutput, String> {
    let mut argv = command.argv.iter();
    let program = argv
        .next()
        .ok_or_else(|| "Sandbox command argv cannot be empty".to_string())?;
    let mut child_command = Command::new(program);
    child_command.args(argv).current_dir(&command.cwd);
    // Shell commands run headless with piped stdio; on Windows an unflagged
    // console child would flash a console window per bash_exec invocation.
    child_command.hide_console_window();
    prepare_stdio(&mut child_command);

    let child = child_command
        .spawn()
        .map_err(|e| format!("Failed to start shell command: {}", e))?;

    run_spawned_child(
        child,
        command.cwd,
        command.timeout_ms,
        command.max_output_chars,
        "Shell command",
    )
    .await
}
