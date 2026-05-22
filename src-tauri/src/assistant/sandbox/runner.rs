use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Stdio;

use tokio::io::AsyncReadExt;
use tokio::process::{Child, Command};
use tokio::time::{timeout, Duration};

use super::SandboxProfile;

#[derive(Debug, Clone)]
pub struct SandboxCommand {
    pub argv: Vec<OsString>,
    pub cwd: PathBuf,
    pub timeout_ms: u64,
    pub max_output_chars: usize,
    pub profile: SandboxProfile,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxCommandOutput {
    pub cwd: PathBuf,
    pub exit_code: Option<i32>,
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

pub async fn run_command(command: SandboxCommand) -> Result<SandboxCommandOutput, String> {
    if command.argv.is_empty() {
        return Err("Sandbox command argv cannot be empty".to_string());
    }
    if !command.cwd.is_dir() {
        return Err(format!(
            "Shell cwd does not exist or is not a directory: {}",
            command.cwd.display()
        ));
    }

    #[cfg(target_os = "linux")]
    {
        super::linux_bwrap::run(command).await
    }

    #[cfg(not(target_os = "linux"))]
    {
        super::unsupported::run(command).await
    }
}

pub(crate) async fn run_spawned_child(
    mut child: Child,
    cwd: PathBuf,
    timeout_ms: u64,
    output_limit: usize,
    timeout_label: &str,
) -> Result<SandboxCommandOutput, String> {
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "Failed to capture command stdout".to_string())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "Failed to capture command stderr".to_string())?;

    let stdout_task = tokio::spawn(async move {
        let mut reader = tokio::io::BufReader::new(stdout);
        let mut buf = Vec::new();
        reader.read_to_end(&mut buf).await.map(|_| buf)
    });
    let stderr_task = tokio::spawn(async move {
        let mut reader = tokio::io::BufReader::new(stderr);
        let mut buf = Vec::new();
        reader.read_to_end(&mut buf).await.map(|_| buf)
    });

    let status = match timeout(Duration::from_millis(timeout_ms), child.wait()).await {
        Ok(result) => result.map_err(|e| format!("Shell command failed: {}", e))?,
        Err(_) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            let _ = stdout_task.await;
            let _ = stderr_task.await;
            return Err(format!(
                "{} timed out after {} ms",
                timeout_label, timeout_ms
            ));
        }
    };

    let stdout_bytes = stdout_task
        .await
        .map_err(|e| format!("Failed to collect stdout: {}", e))?
        .map_err(|e| format!("Failed to read stdout: {}", e))?;
    let stderr_bytes = stderr_task
        .await
        .map_err(|e| format!("Failed to collect stderr: {}", e))?
        .map_err(|e| format!("Failed to read stderr: {}", e))?;

    Ok(SandboxCommandOutput {
        cwd,
        exit_code: status.code(),
        success: status.success(),
        stdout: truncate_string(
            String::from_utf8_lossy(&stdout_bytes).into_owned(),
            output_limit,
        ),
        stderr: truncate_string(
            String::from_utf8_lossy(&stderr_bytes).into_owned(),
            output_limit,
        ),
    })
}

pub(crate) fn prepare_stdio(command: &mut Command) {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
}

fn truncate_string(text: String, limit: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= limit {
        return text;
    }
    chars[..limit].iter().collect::<String>() + "\n...[truncated]"
}
