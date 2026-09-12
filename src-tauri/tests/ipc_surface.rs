//! Keeps the Tauri IPC surface and its consumer in sync.
//!
//! Rust cannot see either side of this boundary. A command registered in
//! `generate_handler!` is "used" as far as the compiler is concerned even when
//! no window ever invokes it, and an `invoke('typo')` in the frontend fails at
//! runtime with a `Command not found` log nobody reads — which is how the
//! AgentBridge frontend outlived its backend until `8c3f8bb4b`.
//!
//! What these tests do NOT catch: a wrapper that still contains an
//! `invoke('name')` literal but has no importers. That is unused-export
//! analysis, it lives on the TypeScript side, and it is why
//! `workspace_write_file` stayed alive long after its last caller. Read the
//! guarantee here as "the two name lists agree", not "every command is
//! reachable".
//!
//! The check is textual by necessity: it greps the registration macro and
//! every `invoke('name')` literal under `../src`. That is exact as long as
//! command names stay string literals, which `no_dynamic_invoke_call_sites`
//! enforces. It does not parse TypeScript, so an `invoke('name')` inside a
//! comment or a doc block counts as a call site — which fails safe in the
//! direction that matters: it can only make a dead command look alive, never
//! make a live one look missing.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

/// `src-tauri/`, the crate root — integration tests run with it as CWD.
fn crate_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Frontend sources: every `.ts`/`.tsx` file except unit tests and the test
/// harness, which invoke through mocks and would otherwise contribute
/// fictional command names.
fn frontend_sources() -> Vec<(PathBuf, String)> {
    fn walk(dir: &Path, out: &mut Vec<(PathBuf, String)>) {
        let entries = fs::read_dir(dir).unwrap_or_else(|e| panic!("read {}: {e}", dir.display()));
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if path.is_dir() {
                // `src/test/` holds the Tauri mocks; `__mocks__` and dot-dirs
                // are not app code either.
                if !matches!(name.as_str(), "node_modules" | "test" | "__mocks__")
                    && !name.starts_with('.')
                {
                    walk(&path, out);
                }
                continue;
            }
            let is_source = name.ends_with(".ts") || name.ends_with(".tsx");
            let is_test = name.contains(".test.") || name.contains(".spec.");
            if is_source && !is_test {
                let text = fs::read_to_string(&path).unwrap_or_default();
                out.push((path, text));
            }
        }
    }

    let root = crate_root().join("../src");
    let mut files = Vec::new();
    walk(&root, &mut files);
    assert!(!files.is_empty(), "no frontend sources found under ../src");
    files
}

/// Command names listed in `tauri::generate_handler![...]`, stripped of their
/// module path.
fn registered_commands() -> BTreeSet<String> {
    let lib = fs::read_to_string(crate_root().join("src/lib.rs")).expect("read src/lib.rs");
    let start = lib
        .find("generate_handler![")
        .expect("generate_handler! not found in src/lib.rs");
    let block = &lib[start..];
    let end = block.find("])").expect("unterminated generate_handler!");

    let commands: BTreeSet<String> = block[..end]
        .lines()
        .skip(1)
        .filter_map(|line| {
            // The trailing comma is rustfmt's doing, not a guarantee: the last
            // entry of a hand-edited macro would have none.
            let name = line.trim().trim_end_matches(',').trim();
            if name.is_empty() || name.starts_with("//") {
                return None;
            }
            Some(name.rsplit("::").next()?.to_string())
        })
        .collect();

    // A floor, not a count: it catches a parser that silently stopped working
    // without failing on every new command. 109 commands as of this test.
    assert!(
        commands.len() > 100,
        "parsed only {} commands from generate_handler!; the macro's shape probably changed",
        commands.len()
    );
    commands
}

/// Command names passed to `invoke('...')` anywhere in the frontend.
fn invoked_commands() -> BTreeSet<String> {
    let mut invoked = BTreeSet::new();
    for (_, text) in frontend_sources() {
        invoked.extend(invoked_names_in(&text));
    }
    invoked
}

/// Literal command names invoked in one source text.
fn invoked_names_in(text: &str) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for rest in invoke_call_arguments(text) {
        let quote = rest.chars().next().unwrap_or(' ');
        if quote == '\'' || quote == '"' {
            if let Some(name) = rest[1..].split(quote).next() {
                names.insert(name.to_string());
            }
        }
    }
    names
}

/// Text following each `invoke(` / `invoke<T>(` call site, with leading
/// whitespace trimmed.
fn invoke_call_arguments(text: &str) -> Vec<&str> {
    let mut sites = Vec::new();
    for (index, _) in text.match_indices("invoke") {
        let before = &text[..index];
        if before
            .chars()
            .next_back()
            .is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '.')
        {
            continue; // `revoke(`, `obj.invoke(`, … — no separator before it
        }
        // `function invoke(cmd, …)` declares the thing; it does not call it.
        // Scoped to the current line, or a comment ending in the word
        // "function" would swallow the call on the line below it.
        let current_line = before.rsplit('\n').next().unwrap_or(before);
        if current_line.trim_end().ends_with("function") {
            continue;
        }
        let mut rest = text[index + "invoke".len()..].trim_start();
        if rest.starts_with('<') {
            // Generic argument, possibly nested: `invoke<Record<string, X>>(`.
            let mut depth = 0usize;
            let mut end = None;
            for (offset, ch) in rest.char_indices() {
                match ch {
                    '<' => depth += 1,
                    '>' => {
                        depth -= 1;
                        if depth == 0 {
                            end = Some(offset + ch.len_utf8());
                            break;
                        }
                    }
                    _ => {}
                }
            }
            let Some(end) = end else { continue };
            rest = rest[end..].trim_start();
        }
        let Some(args) = rest.strip_prefix('(') else {
            continue;
        };
        sites.push(args.trim_start());
    }
    sites
}

#[test]
fn every_registered_command_is_invoked_by_the_frontend() {
    let unused: Vec<String> = registered_commands()
        .difference(&invoked_commands())
        .cloned()
        .collect();
    assert!(
        unused.is_empty(),
        "commands registered in generate_handler! but named by no invoke() in ../src — \
         delete the command (and its request type and resolver) or wire it up: {unused:?}"
    );
}

#[test]
fn every_invoked_command_is_registered() {
    let missing: Vec<String> = invoked_commands()
        .difference(&registered_commands())
        .cloned()
        .collect();
    assert!(
        missing.is_empty(),
        "invoke() targets with no command in generate_handler! — these fail at runtime with \
         `Command not found`: {missing:?}"
    );
}

#[test]
fn no_dynamic_invoke_call_sites() {
    let mut dynamic = Vec::new();
    for (path, text) in frontend_sources() {
        for rest in invoke_call_arguments(&text) {
            let first = rest.chars().next().unwrap_or(' ');
            if first != '\'' && first != '"' {
                let head: String = rest.chars().take(24).collect();
                dynamic.push(format!("{}: invoke({head}…", path.display()));
            }
        }
    }
    assert!(
        dynamic.is_empty(),
        "invoke() called with a non-literal command name, which makes the two checks above \
         blind to it; keep command names literal: {dynamic:#?}"
    );
}

/// The two checks above are only as good as this parser, and a parser that
/// silently collects nothing would make them pass forever.
#[test]
fn the_call_site_parser_sees_real_calls_and_only_those() {
    let text = r#"
        import { invoke } from '@tauri-apps/api/core';
        export function invoke(cmd: string, args?: unknown) {}
        await invoke<Record<string, string[]>>('workspace_list', {});
        const x = invoke("workspace_create");
        obj.invoke('someone_elses_method');
        revoke('not_an_invoke');
    "#;

    let names = invoked_names_in(text);
    assert!(
        invoked_names_in("// a helper function\ninvoke('workspace_list');")
            .contains("workspace_list"),
        "the `function` skip must be scoped to its own line"
    );
    assert!(
        names.contains("workspace_list"),
        "nested generic arguments must not hide a call site: {names:?}"
    );
    assert!(names.contains("workspace_create"), "{names:?}");
    assert!(!names.contains("someone_elses_method"), "{names:?}");
    assert!(!names.contains("not_an_invoke"), "{names:?}");
    assert_eq!(names.len(), 2, "{names:?}");

    // The declaration's parameter list must not read as a dynamic call site.
    assert!(invoke_call_arguments(text)
        .iter()
        .all(|rest| rest.starts_with('\'') || rest.starts_with('"')));
}
