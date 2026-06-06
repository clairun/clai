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
//! Terminal editors (nvim, vim, helix) can't be spawned detached — with
//! no TTY they die silently — so their table entries are flagged
//! `in_terminal` and route through the terminal resolution, each
//! terminal entry carrying the syntax that introduces a command to run
//! inside it (`gnome-terminal -- cmd`, `konsole -e cmd`, `kitty cmd`…).
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

/// Probe-table editor. `{path}` in the arg lists is replaced with the
/// file/directory being opened. `in_terminal` editors run inside the
/// resolved terminal instead of being spawned detached.
struct EditorSpec {
    id: &'static str,
    name: &'static str,
    bin: &'static str,
    file_args: &'static [&'static str],
    dir_args: &'static [&'static str],
    in_terminal: bool,
}

const EDITORS: &[EditorSpec] = &[
    EditorSpec {
        id: "vscode",
        name: "Visual Studio Code",
        bin: "code",
        file_args: &["--goto", "{path}"],
        dir_args: &["{path}"],
        in_terminal: false,
    },
    EditorSpec {
        id: "cursor",
        name: "Cursor",
        bin: "cursor",
        file_args: &["--goto", "{path}"],
        dir_args: &["{path}"],
        in_terminal: false,
    },
    EditorSpec {
        id: "zed",
        name: "Zed",
        bin: "zed",
        file_args: &["{path}"],
        dir_args: &["{path}"],
        in_terminal: false,
    },
    EditorSpec {
        id: "sublime",
        name: "Sublime Text",
        bin: "subl",
        file_args: &["{path}"],
        dir_args: &["{path}"],
        in_terminal: false,
    },
    EditorSpec {
        id: "vscodium",
        name: "VSCodium",
        bin: "codium",
        file_args: &["--goto", "{path}"],
        dir_args: &["{path}"],
        in_terminal: false,
    },
    EditorSpec {
        id: "nvim",
        name: "Neovim",
        bin: "nvim",
        file_args: &["{path}"],
        dir_args: &["{path}"],
        in_terminal: true,
    },
    EditorSpec {
        id: "vim",
        name: "Vim",
        bin: "vim",
        file_args: &["{path}"],
        dir_args: &["{path}"],
        in_terminal: true,
    },
    EditorSpec {
        id: "helix",
        name: "Helix",
        bin: "hx",
        file_args: &["{path}"],
        dir_args: &["{path}"],
        in_terminal: true,
    },
];

/// Probe-table terminal. `dir_args` open at a working directory
/// (`{dir}` substituted); `exec_args` introduce a command to run inside
/// the terminal (appended before the command itself).
struct TerminalSpec {
    id: &'static str,
    name: &'static str,
    bin: &'static str,
    dir_args: &'static [&'static str],
    exec_args: &'static [&'static str],
}

const TERMINALS: &[TerminalSpec] = &[
    TerminalSpec {
        id: "ptyxis",
        name: "Ptyxis",
        bin: "ptyxis",
        dir_args: &["--working-directory", "{dir}"],
        exec_args: &["--"],
    },
    TerminalSpec {
        id: "gnome-terminal",
        name: "GNOME Terminal",
        bin: "gnome-terminal",
        dir_args: &["--working-directory={dir}"],
        exec_args: &["--"],
    },
    TerminalSpec {
        id: "konsole",
        name: "Konsole",
        bin: "konsole",
        dir_args: &["--workdir", "{dir}"],
        exec_args: &["-e"],
    },
    TerminalSpec {
        id: "ghostty",
        name: "Ghostty",
        bin: "ghostty",
        dir_args: &["--working-directory={dir}"],
        exec_args: &["-e"],
    },
    TerminalSpec {
        id: "kitty",
        name: "kitty",
        bin: "kitty",
        dir_args: &["--directory", "{dir}"],
        exec_args: &[],
    },
    TerminalSpec {
        id: "alacritty",
        name: "Alacritty",
        bin: "alacritty",
        dir_args: &["--working-directory", "{dir}"],
        exec_args: &["-e"],
    },
    TerminalSpec {
        id: "foot",
        name: "foot",
        bin: "foot",
        dir_args: &["-D", "{dir}"],
        exec_args: &[],
    },
    TerminalSpec {
        id: "wezterm",
        name: "WezTerm",
        bin: "wezterm",
        dir_args: &["start", "--cwd", "{dir}"],
        exec_args: &["--"],
    },
];

/// Probe the host for known editors/terminals + the xdg default editor
/// name. Spawns one `which` per table entry — call from the Settings
/// surface, not hot paths.
pub fn detect_system_apps() -> SystemAppsStatus {
    let editors = EDITORS
        .iter()
        .filter(|spec| command_exists(spec.bin))
        .map(|spec| SystemAppEntry {
            id: spec.id.to_string(),
            name: spec.name.to_string(),
        })
        .collect();
    let terminals = TERMINALS
        .iter()
        .filter(|spec| command_exists(spec.bin))
        .map(|spec| SystemAppEntry {
            id: spec.id.to_string(),
            name: spec.name.to_string(),
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
/// Terminal editors run inside the resolved terminal at the path's
/// directory.
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
            let spec = EDITORS
                .iter()
                .find(|spec| spec.id == id)
                .ok_or_else(|| format!("Unknown editor `{}` — re-select it in Settings.", id))?;
            let args = substitute(
                if is_dir {
                    spec.dir_args
                } else {
                    spec.file_args
                },
                "{path}",
                &path_str,
            );
            if spec.in_terminal {
                let dir = if is_dir {
                    path
                } else {
                    path.parent().unwrap_or(path)
                };
                let mut command = vec![spec.bin.to_string()];
                command.extend(args);
                run_in_terminal(config, dir, &command)
            } else {
                spawn_host_detached(spec.bin, &args, None)
            }
        }
    }
}

/// Open `path` with the OS default application for its type (`xdg-open`).
pub fn open_with_system(path: &Path) -> Result<(), String> {
    spawn_host_detached("xdg-open", &[path.display().to_string()], None)
}

/// Open a terminal at `dir`.
pub fn open_terminal(config: &SystemAppsConfig, dir: &Path) -> Result<(), String> {
    run_in_terminal(config, dir, &[])
}

/// Open the resolved terminal at `dir`, optionally running `command`
/// inside it (terminal editors). Resolution chain: explicit choice →
/// `xdg-terminal-exec` → `$TERMINAL` → first available probe entry.
fn run_in_terminal(
    config: &SystemAppsConfig,
    dir: &Path,
    command: &[String],
) -> Result<(), String> {
    let dir_str = dir.display().to_string();
    match config.terminal.as_deref() {
        Some("custom") => {
            let template = config
                .terminal_custom_command
                .as_deref()
                .filter(|t| !t.trim().is_empty())
                .ok_or_else(|| "Custom terminal command is not configured.".to_string())?;
            let (bin, mut args) = parse_custom_template(template, "{dir}", &dir_str)?;
            args.extend_from_slice(command);
            return spawn_host_detached(&bin, &args, Some(dir));
        }
        Some(id) if id != "auto" => {
            let spec = TERMINALS
                .iter()
                .find(|spec| spec.id == id)
                .ok_or_else(|| format!("Unknown terminal `{}` — re-select it in Settings.", id))?;
            return spawn_terminal_spec(spec, &dir_str, command);
        }
        _ => {}
    }

    // Auto chain. xdg-terminal-exec opens the user's preferred terminal,
    // inherits the working directory, and takes the command verbatim.
    if command_exists("xdg-terminal-exec") {
        return spawn_host_detached("xdg-terminal-exec", command, Some(dir));
    }
    if let Ok(term) = std::env::var("TERMINAL") {
        let term = term.trim().to_string();
        if !term.is_empty() && command_exists(&term) {
            // Use the probe entry's syntax when $TERMINAL is a known
            // terminal (matched by binary name); otherwise fall back to
            // the de-facto `-e` convention (xterm, urxvt, st, …).
            let basename = term.rsplit('/').next().unwrap_or(&term);
            if let Some(spec) = TERMINALS.iter().find(|spec| spec.bin == basename) {
                return spawn_terminal_spec(spec, &dir_str, command);
            }
            let mut args: Vec<String> = Vec::new();
            if !command.is_empty() {
                args.push("-e".to_string());
                args.extend_from_slice(command);
            }
            return spawn_host_detached(&term, &args, Some(dir));
        }
    }
    for spec in TERMINALS {
        if command_exists(spec.bin) {
            return spawn_terminal_spec(spec, &dir_str, command);
        }
    }
    Err("No terminal emulator found. Pick one in Settings → Applications.".to_string())
}

/// Build a probe-table terminal invocation: working-directory args, then
/// the exec introducer + command when one should run inside.
fn spawn_terminal_spec(
    spec: &TerminalSpec,
    dir_str: &str,
    command: &[String],
) -> Result<(), String> {
    let mut args = substitute(spec.dir_args, "{dir}", dir_str);
    if !command.is_empty() {
        args.extend(spec.exec_args.iter().map(|a| a.to_string()));
        args.extend_from_slice(command);
    }
    spawn_host_detached(spec.bin, &args, None)
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

    #[test]
    fn terminal_editors_are_flagged() {
        for id in ["nvim", "vim", "helix"] {
            let spec = EDITORS.iter().find(|spec| spec.id == id).unwrap();
            assert!(spec.in_terminal, "{id} must run inside a terminal");
        }
        let code = EDITORS.iter().find(|spec| spec.id == "vscode").unwrap();
        assert!(!code.in_terminal);
    }

    #[test]
    fn terminal_spec_invocation_includes_exec_introducer() {
        // gnome-terminal: dir flag, `--`, then the command.
        let spec = TERMINALS
            .iter()
            .find(|spec| spec.id == "gnome-terminal")
            .unwrap();
        let mut args = substitute(spec.dir_args, "{dir}", "/w");
        args.extend(spec.exec_args.iter().map(|a| a.to_string()));
        args.extend_from_slice(&["nvim".to_string(), "/w/f.md".to_string()]);
        assert_eq!(
            args,
            vec!["--working-directory=/w", "--", "nvim", "/w/f.md"]
        );

        // kitty: command appended directly, no introducer.
        let spec = TERMINALS.iter().find(|spec| spec.id == "kitty").unwrap();
        assert!(spec.exec_args.is_empty());
    }
}
