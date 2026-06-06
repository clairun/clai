//! System application integration — open workspace files/folders in the
//! user's editor, terminal, or system-default app.
//!
//! GNOME-style "default applications", scoped to what a dev tool needs:
//! a curated probe table of editors/terminals (each entry carries its own
//! open-file / open-dir incantation, so users never write command
//! templates), the OS's own defaults via `xdg-open` / `xdg-mime` (the OS
//! owns the MIME → app table; we never rebuild it), and a `Custom…`
//! template as the escape hatch for anything not in the table.
//!
//! Terminal resolution has no xdg-mime equivalent (a known freedesktop
//! gap), so it's a chain: explicit user choice → `xdg-terminal-exec`
//! (the emerging standard; runs a command in the preferred terminal and
//! inherits cwd) → `$TERMINAL` → first available entry in the probe
//! table.
//!
//! Flatpak: every spawn goes through `flatpak-spawn --host` (the editor
//! and terminal live on the HOST), reusing the same plumbing as provider
//! CLI detection. `--directory` carries the working directory across.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::providers::{command_exists, get_host_command, is_flatpak};

/// A probe-table entry the UI can offer in a dropdown.
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "bindings.ts")]
pub struct SystemAppEntry {
    pub id: String,
    pub name: String,
}

/// Detection result for the Settings "Applications" section.
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "bindings.ts")]
pub struct SystemAppsStatus {
    /// Editors from the probe table found on the host.
    pub editors: Vec<SystemAppEntry>,
    /// Terminals from the probe table found on the host.
    pub terminals: Vec<SystemAppEntry>,
    /// Pretty name of the xdg default handler for text/plain, when
    /// resolvable — shown as "System default (gedit)" in the dropdown.
    pub system_editor_name: Option<String>,
}

/// User selection persisted in `~/.clai/config.json`. `editor`/`terminal`
/// hold a probe-table id, the sentinel `"custom"`, or None for the
/// default behavior (xdg-open / the terminal resolution chain).
#[derive(Debug, Clone, Default, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase", default)]
#[ts(export, export_to = "bindings.ts")]
pub struct SystemAppsConfig {
    pub editor: Option<String>,
    /// Custom editor template; `{path}` is replaced with the file or
    /// directory being opened.
    pub editor_custom_command: Option<String>,
    pub terminal: Option<String>,
    /// Custom terminal template; `{dir}` is replaced with the working
    /// directory.
    pub terminal_custom_command: Option<String>,
}

/// (id, display name, binary, open-file args, open-dir args).
/// `{path}` is replaced with the target file/directory.
type EditorSpec = (
    &'static str,
    &'static str,
    &'static str,
    &'static [&'static str],
    &'static [&'static str],
);

const EDITORS: &[EditorSpec] = &[
    (
        "vscode",
        "Visual Studio Code",
        "code",
        &["--goto", "{path}"],
        &["{path}"],
    ),
    (
        "cursor",
        "Cursor",
        "cursor",
        &["--goto", "{path}"],
        &["{path}"],
    ),
    ("zed", "Zed", "zed", &["{path}"], &["{path}"]),
    ("sublime", "Sublime Text", "subl", &["{path}"], &["{path}"]),
    (
        "vscodium",
        "VSCodium",
        "codium",
        &["--goto", "{path}"],
        &["{path}"],
    ),
];

/// (id, display name, binary, args opening at a working directory).
/// `{dir}` is replaced with the directory.
const TERMINALS: &[(&str, &str, &str, &[&str])] = &[
    (
        "ptyxis",
        "Ptyxis",
        "ptyxis",
        &["--working-directory", "{dir}"],
    ),
    (
        "gnome-terminal",
        "GNOME Terminal",
        "gnome-terminal",
        &["--working-directory={dir}"],
    ),
    ("konsole", "Konsole", "konsole", &["--workdir", "{dir}"]),
    ("kitty", "kitty", "kitty", &["--directory", "{dir}"]),
    (
        "alacritty",
        "Alacritty",
        "alacritty",
        &["--working-directory", "{dir}"],
    ),
    ("foot", "foot", "foot", &["-D", "{dir}"]),
    (
        "wezterm",
        "WezTerm",
        "wezterm",
        &["start", "--cwd", "{dir}"],
    ),
];

/// Probe the host for known editors/terminals + the xdg default editor
/// name. Spawns one `which` per table entry — call from the Settings
/// surface, not hot paths.
pub fn detect_system_apps() -> SystemAppsStatus {
    let editors = EDITORS
        .iter()
        .filter(|(_, _, bin, _, _)| command_exists(bin))
        .map(|(id, name, _, _, _)| SystemAppEntry {
            id: (*id).to_string(),
            name: (*name).to_string(),
        })
        .collect();
    let terminals = TERMINALS
        .iter()
        .filter(|(_, _, bin, _)| command_exists(bin))
        .map(|(id, name, _, _)| SystemAppEntry {
            id: (*id).to_string(),
            name: (*name).to_string(),
        })
        .collect();
    SystemAppsStatus {
        editors,
        terminals,
        system_editor_name: xdg_default_editor_name(),
    }
}

/// `xdg-mime query default text/plain` → "org.gnome.gedit.desktop" →
/// "gedit". Display-only; opening goes through `xdg-open`, which applies
/// the real association.
fn xdg_default_editor_name() -> Option<String> {
    let output = get_host_command("xdg-mime")
        .args(["query", "default", "text/plain"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let desktop = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if desktop.is_empty() {
        return None;
    }
    // "org.gnome.gedit.desktop" → "gedit"; "code.desktop" → "code".
    let stem = desktop.strip_suffix(".desktop").unwrap_or(&desktop);
    Some(stem.rsplit('.').next().unwrap_or(stem).to_string())
}

fn substitute(args: &[&str], placeholder: &str, value: &str) -> Vec<String> {
    args.iter()
        .map(|arg| arg.replace(placeholder, value))
        .collect()
}

/// Spawn a host command detached (fire and forget). Under Flatpak the
/// working directory is forwarded with `--directory` (plain
/// `current_dir` would only move flatpak-spawn itself).
fn spawn_host_detached(bin: &str, args: &[String], dir: Option<&Path>) -> Result<(), String> {
    let mut command: Command;
    if is_flatpak() {
        command = Command::new("flatpak-spawn");
        if let Some(dir) = dir {
            command.arg(format!("--directory={}", dir.display()));
        }
        command.arg("--host").arg(bin);
    } else {
        command = Command::new(bin);
        if let Some(dir) = dir {
            command.current_dir(dir);
        }
    }
    command.args(args);
    command
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("Failed to launch `{}`: {}", bin, e))
}

/// Split a custom template into (binary, args) with the placeholder
/// substituted. Whitespace-split on purpose: templates are simple
/// command lines, not shell scripts (no quoting/expansion surface).
fn parse_custom_template(
    template: &str,
    placeholder: &str,
    value: &str,
) -> Result<(String, Vec<String>), String> {
    let mut parts = template.split_whitespace();
    let bin = parts
        .next()
        .ok_or_else(|| "Custom command is empty.".to_string())?
        .to_string();
    let args: Vec<String> = parts.map(|p| p.replace(placeholder, value)).collect();
    Ok((bin, args))
}

/// Open `path` (file or directory) in the configured editor. Falls back
/// to the system default (`xdg-open`) when nothing is configured.
pub fn open_in_editor(config: &SystemAppsConfig, path: &Path, is_dir: bool) -> Result<(), String> {
    let path_str = path.display().to_string();
    match config.editor.as_deref() {
        None | Some("system") => open_with_system(path),
        Some("custom") => {
            let template = config
                .editor_custom_command
                .as_deref()
                .filter(|t| !t.trim().is_empty())
                .ok_or_else(|| "Custom editor command is not configured.".to_string())?;
            let (bin, args) = parse_custom_template(template, "{path}", &path_str)?;
            spawn_host_detached(&bin, &args, None)
        }
        Some(id) => {
            let entry = EDITORS
                .iter()
                .find(|(eid, _, _, _, _)| *eid == id)
                .ok_or_else(|| format!("Unknown editor `{}` — re-select it in Settings.", id))?;
            let (_, _, bin, file_args, dir_args) = entry;
            let args = substitute(
                if is_dir { dir_args } else { file_args },
                "{path}",
                &path_str,
            );
            spawn_host_detached(bin, &args, None)
        }
    }
}

/// Open `path` with the OS default application for its type (`xdg-open`).
pub fn open_with_system(path: &Path) -> Result<(), String> {
    spawn_host_detached("xdg-open", &[path.display().to_string()], None)
}

/// Open a terminal at `dir`. Resolution chain: explicit choice →
/// `xdg-terminal-exec` → `$TERMINAL` → first available probe entry.
pub fn open_terminal(config: &SystemAppsConfig, dir: &Path) -> Result<(), String> {
    let dir_str = dir.display().to_string();
    match config.terminal.as_deref() {
        Some("custom") => {
            let template = config
                .terminal_custom_command
                .as_deref()
                .filter(|t| !t.trim().is_empty())
                .ok_or_else(|| "Custom terminal command is not configured.".to_string())?;
            let (bin, args) = parse_custom_template(template, "{dir}", &dir_str)?;
            return spawn_host_detached(&bin, &args, Some(dir));
        }
        Some(id) if id != "auto" => {
            let entry = TERMINALS
                .iter()
                .find(|(tid, _, _, _)| *tid == id)
                .ok_or_else(|| format!("Unknown terminal `{}` — re-select it in Settings.", id))?;
            let (_, _, bin, dir_args) = entry;
            return spawn_host_detached(bin, &substitute(dir_args, "{dir}", &dir_str), None);
        }
        _ => {}
    }

    // Auto chain. xdg-terminal-exec opens the user's preferred terminal
    // and inherits the working directory.
    if command_exists("xdg-terminal-exec") {
        return spawn_host_detached("xdg-terminal-exec", &[], Some(dir));
    }
    if let Ok(term) = std::env::var("TERMINAL") {
        let term = term.trim().to_string();
        if !term.is_empty() && command_exists(&term) {
            return spawn_host_detached(&term, &[], Some(dir));
        }
    }
    for (_, _, bin, dir_args) in TERMINALS {
        if command_exists(bin) {
            return spawn_host_detached(bin, &substitute(dir_args, "{dir}", &dir_str), None);
        }
    }
    Err("No terminal emulator found. Pick one in Settings → Applications.".to_string())
}

/// Resolve `rel_path` inside `root`, refusing anything that escapes it
/// (symlinks included — both sides are canonicalized). `None` resolves
/// to the root itself.
pub fn resolve_contained_path(root: &Path, rel_path: Option<&str>) -> Result<PathBuf, String> {
    let root = root
        .canonicalize()
        .map_err(|e| format!("Workspace root not accessible: {}", e))?;
    let Some(rel) = rel_path.filter(|r| !r.trim().is_empty()) else {
        return Ok(root);
    };
    let candidate = root.join(rel);
    let resolved = candidate
        .canonicalize()
        .map_err(|e| format!("Path not found: {}", e))?;
    if !resolved.starts_with(&root) {
        return Err("Path escapes the workspace root.".to_string());
    }
    Ok(resolved)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substitute_replaces_placeholder() {
        assert_eq!(
            substitute(&["--goto", "{path}"], "{path}", "/tmp/x.rs"),
            vec!["--goto".to_string(), "/tmp/x.rs".to_string()]
        );
    }

    #[test]
    fn custom_template_parses_bin_and_args() {
        let (bin, args) = parse_custom_template("code --goto {path}", "{path}", "/w/f.md").unwrap();
        assert_eq!(bin, "code");
        assert_eq!(args, vec!["--goto".to_string(), "/w/f.md".to_string()]);
    }

    #[test]
    fn custom_template_rejects_empty() {
        assert!(parse_custom_template("   ", "{path}", "x").is_err());
    }

    #[test]
    fn contained_path_resolves_root_and_children() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub/f.txt"), "x").unwrap();

        let root = resolve_contained_path(dir.path(), None).unwrap();
        assert_eq!(root, dir.path().canonicalize().unwrap());

        let file = resolve_contained_path(dir.path(), Some("sub/f.txt")).unwrap();
        assert!(file.ends_with("sub/f.txt"));
    }

    #[test]
    fn contained_path_refuses_escape() {
        let dir = tempfile::tempdir().unwrap();
        assert!(resolve_contained_path(dir.path(), Some("../outside")).is_err());
        assert!(resolve_contained_path(dir.path(), Some("../../etc/passwd")).is_err());
    }

    #[test]
    fn unknown_editor_id_errors() {
        let config = SystemAppsConfig {
            editor: Some("emacs-on-a-toaster".to_string()),
            ..Default::default()
        };
        let err = open_in_editor(&config, Path::new("/tmp"), true).unwrap_err();
        assert!(err.contains("Unknown editor"), "{err}");
    }
}
