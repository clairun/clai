//! The professional memory of a shared agent definition: lessons about its
//! craft that every workspace assigning it shares, kept in
//! `<app dir>/agents/<definition-id>/memory/`.
//!
//! Agents read and write it through the shell; the sandbox exposes only the
//! running member's own directory. This module lists it for the system
//! prompt's catalog and the settings UI, and reads, writes and deletes single
//! files for the UI. Paths given to it are relative to the memory directory
//! and never leave it.

use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::time::SystemTime;

use serde::Serialize;

/// Each description in the catalog is cut to this many characters.
const MAX_DESCRIPTION_CHARS: usize = 300;
/// The catalog stops at whichever bound comes first.
const MAX_CATALOG_ENTRIES: usize = 100;
const MAX_CATALOG_BYTES: usize = 8 * 1024;
/// Bounds on the walk, so a runaway directory cannot stall a run.
const MAX_DEPTH: usize = 8;
const MAX_FILES: usize = 1_000;
/// Frontmatter must be within the start of a file.
const FRONTMATTER_BYTES: u64 = 4 * 1024;
/// The largest file the settings UI reads or writes.
const MAX_FILE_BYTES: usize = 256 * 1024;

/// One memory file, as the catalog and the settings UI list it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "bindings.ts")]
pub struct AgentMemoryFile {
    /// Relative to the memory directory, `/`-separated.
    pub path: String,
    pub description: Option<String>,
    pub updated_at: Option<String>,
    #[ts(type = "number")]
    pub size: u64,
    /// The file starts with frontmatter holding a `description` and an
    /// `updated_at` date.
    pub frontmatter_valid: bool,
    /// Newest first: `updated_at`, else the modification time, in seconds.
    #[serde(skip)]
    sort_key: i64,
}

/// The memory directory of `definition_id` under `agents_root`.
pub fn memory_dir(agents_root: &Path, definition_id: &str) -> Result<PathBuf, String> {
    validate_definition_id(definition_id)?;
    Ok(agents_root.join(definition_id).join("memory"))
}

/// Definition ids are generated UUIDs; anything that could name another
/// directory is refused.
pub fn validate_definition_id(id: &str) -> Result<(), String> {
    let valid = !id.is_empty()
        && id.len() <= 128
        && !id.starts_with('.')
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    if valid {
        Ok(())
    } else {
        Err(format!("Invalid agent definition id: {id:?}"))
    }
}

/// Creates the memory directory. A symlink in its place is refused: the
/// sandbox would expose wherever it leads.
pub fn ensure_memory_dir(dir: &Path) -> Result<(), String> {
    fs::create_dir_all(dir)
        .map_err(|e| format!("Failed to create agent memory {}: {e}", dir.display()))?;
    real_dir(dir)
}

fn real_dir(dir: &Path) -> Result<(), String> {
    match fs::symlink_metadata(dir) {
        Ok(meta) if meta.is_dir() => Ok(()),
        Ok(_) => Err(format!("Agent memory {} is not a directory", dir.display())),
        Err(e) => Err(format!(
            "Agent memory {} is unavailable: {e}",
            dir.display()
        )),
    }
}

/// Every visible `.md` file under `dir`, newest first. Hidden entries and
/// symlinks are skipped; a missing directory lists nothing.
pub fn list(dir: &Path) -> Result<Vec<AgentMemoryFile>, String> {
    if fs::symlink_metadata(dir).is_err() {
        return Ok(Vec::new());
    }
    real_dir(dir)?;
    let mut files = Vec::new();
    walk(dir, "", 0, &mut files);
    files.sort_by(|a, b| {
        b.sort_key
            .cmp(&a.sort_key)
            .then_with(|| a.path.cmp(&b.path))
    });
    Ok(files)
}

fn walk(dir: &Path, prefix: &str, depth: usize, files: &mut Vec<AgentMemoryFile>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        if files.len() >= MAX_FILES {
            return;
        }
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        if !valid_component(&name) {
            continue;
        }
        // `DirEntry::file_type` does not follow symlinks.
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let path = format!("{prefix}{name}");
        if file_type.is_dir() && depth < MAX_DEPTH {
            walk(&entry.path(), &format!("{path}/"), depth + 1, files);
        } else if file_type.is_file() && name.ends_with(".md") {
            if let Some(file) = describe(&entry.path(), path) {
                files.push(file);
            }
        }
    }
}

fn describe(path: &Path, relative: String) -> Option<AgentMemoryFile> {
    let (file, meta) = open_file(path).ok()?;
    let mut head = Vec::new();
    let _ = file.take(FRONTMATTER_BYTES).read_to_end(&mut head);
    // The cut may split a character, and a file may not be UTF-8 at all:
    // only the valid start is parsed.
    let head = match std::str::from_utf8(&head) {
        Ok(head) => head,
        Err(error) => std::str::from_utf8(&head[..error.valid_up_to()]).unwrap_or_default(),
    };
    let frontmatter = Frontmatter::parse(head);
    let updated = frontmatter.updated_at.as_deref().and_then(parse_date);
    let modified = meta
        .modified()
        .ok()
        .and_then(|time| time.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map_or(0, |since| {
            i64::try_from(since.as_secs()).unwrap_or(i64::MAX)
        });
    Some(AgentMemoryFile {
        path: relative,
        frontmatter_valid: frontmatter.closed
            && frontmatter.description.is_some()
            && updated.is_some(),
        description: frontmatter.description,
        updated_at: frontmatter.updated_at,
        size: meta.len(),
        sort_key: updated.unwrap_or(modified),
    })
}

#[derive(Debug, Default, PartialEq, Eq)]
struct Frontmatter {
    description: Option<String>,
    updated_at: Option<String>,
    /// The closing `---` was found.
    closed: bool,
}

impl Frontmatter {
    fn parse(content: &str) -> Self {
        let mut lines = content.lines().map(str::trim);
        let mut frontmatter = Self::default();
        if lines.next() != Some("---") {
            return frontmatter;
        }
        for line in lines {
            if line == "---" {
                frontmatter.closed = true;
                break;
            }
            let Some((key, value)) = line.split_once(':') else {
                continue;
            };
            let value = value.trim().trim_matches('"').trim_matches('\'').trim();
            let value = (!value.is_empty()).then(|| value.to_string());
            match key.trim() {
                "description" => frontmatter.description = value,
                "updated_at" => frontmatter.updated_at = value,
                _ => {}
            }
        }
        frontmatter
    }
}

/// `YYYY-MM-DD`, optionally followed by a time, as seconds since the epoch.
fn parse_date(value: &str) -> Option<i64> {
    use chrono::{DateTime, NaiveDate, NaiveDateTime};
    if let Ok(time) = DateTime::parse_from_rfc3339(value) {
        return Some(time.timestamp());
    }
    for format in ["%Y-%m-%dT%H:%M:%S", "%Y-%m-%d %H:%M:%S", "%Y-%m-%dT%H:%M"] {
        if let Ok(time) = NaiveDateTime::parse_from_str(value, format) {
            return Some(time.and_utc().timestamp());
        }
    }
    NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .ok()
        .and_then(|date| date.and_hms_opt(0, 0, 0))
        .map(|time| time.and_utc().timestamp())
}

/// The catalog the system prompt shows: one line per file, newest first,
/// bounded. Files whose frontmatter needs fixing say so.
pub fn catalog(files: &[AgentMemoryFile]) -> String {
    let mut catalog = String::new();
    let mut shown = 0;
    for file in files {
        let description = file.description.as_deref().map(|text| {
            let mut cut: String = text.chars().take(MAX_DESCRIPTION_CHARS).collect();
            if cut.len() < text.len() {
                cut.push('…');
            }
            cut
        });
        let line = match (file.frontmatter_valid, description) {
            (true, Some(description)) => format!("- `{}` — {description}\n", file.path),
            (_, description) => format!(
                "- `{}` — FIX FRONTMATTER (needs `description` and `updated_at`){}\n",
                file.path,
                description.map(|d| format!(": {d}")).unwrap_or_default()
            ),
        };
        if shown == MAX_CATALOG_ENTRIES || catalog.len() + line.len() > MAX_CATALOG_BYTES {
            break;
        }
        catalog.push_str(&line);
        shown += 1;
    }
    if shown < files.len() {
        catalog.push_str(&format!(
            "- … {} older files not listed. Your memory has outgrown its catalog: consolidate overlapping lessons and delete stale ones.\n",
            files.len() - shown
        ));
    }
    catalog
}

/// A path component the UI may name: visible, and free of control characters.
fn valid_component(name: &str) -> bool {
    !name.is_empty() && !name.starts_with('.') && !name.chars().any(|c| c.is_control() || c == '\\')
}

/// `relative` inside `dir`, refusing anything that could leave it: absolute
/// paths, `..`, hidden components, non-`.md` files and symlinks on the way.
fn resolve_file(dir: &Path, relative: &str) -> Result<PathBuf, String> {
    let invalid = || format!("Invalid memory file path: {relative:?}");
    let path = Path::new(relative);
    if !relative.ends_with(".md") || path.is_absolute() {
        return Err(invalid());
    }
    let mut resolved = dir.to_path_buf();
    for component in path.components() {
        let Component::Normal(name) = component else {
            return Err(invalid());
        };
        let name = name
            .to_str()
            .filter(|name| valid_component(name))
            .ok_or_else(invalid)?;
        resolved.push(name);
    }
    real_dir(dir)?;
    for ancestor in resolved.ancestors().take_while(|path| *path != dir) {
        if fs::symlink_metadata(ancestor).is_ok_and(|meta| meta.file_type().is_symlink()) {
            return Err(invalid());
        }
    }
    Ok(resolved)
}

/// Opens a regular file without following a symlink at `path`, so one
/// swapped in after the directory was listed is refused rather than read.
/// Non-blocking, so a FIFO swapped in is refused rather than waited on.
fn open_file(path: &Path) -> std::io::Result<(fs::File, fs::Metadata)> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::custom_flags(
        &mut options,
        libc::O_NOFOLLOW | libc::O_NONBLOCK,
    );
    let file = options.open(path)?;
    let meta = file.metadata()?;
    if meta.is_file() {
        Ok((file, meta))
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "not a regular file",
        ))
    }
}

pub fn read(dir: &Path, relative: &str) -> Result<String, String> {
    let path = resolve_file(dir, relative)?;
    let failed = |e: std::io::Error| format!("Failed to read {relative}: {e}");
    let (file, meta) = open_file(&path).map_err(failed)?;
    if meta.len() > MAX_FILE_BYTES as u64 {
        return Err(format!("{relative} is larger than {MAX_FILE_BYTES} bytes"));
    }
    let mut content = String::new();
    file.take(MAX_FILE_BYTES as u64)
        .read_to_string(&mut content)
        .map_err(failed)?;
    Ok(content)
}

/// Writes through a new file renamed into place, so a symlink planted at the
/// path is replaced rather than followed.
pub fn write(dir: &Path, relative: &str, content: &str) -> Result<(), String> {
    if content.len() > MAX_FILE_BYTES {
        return Err(format!(
            "Memory files are limited to {MAX_FILE_BYTES} bytes"
        ));
    }
    let path = resolve_file(dir, relative)?;
    let parent = path
        .parent()
        .ok_or_else(|| format!("Invalid memory file path: {relative:?}"))?;
    fs::create_dir_all(parent).map_err(|e| format!("Failed to write {relative}: {e}"))?;
    if fs::symlink_metadata(&path).is_ok_and(|meta| meta.is_dir()) {
        return Err(format!("{relative} is a directory"));
    }
    let temp = parent.join(format!(".{}.tmp", uuid::Uuid::new_v4()));
    let result = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)
        .and_then(|mut file| std::io::Write::write_all(&mut file, content.as_bytes()))
        .and_then(|()| fs::rename(&temp, &path));
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result.map_err(|e| format!("Failed to write {relative}: {e}"))
}

pub fn delete(dir: &Path, relative: &str) -> Result<(), String> {
    let path = resolve_file(dir, relative)?;
    fs::remove_file(&path).map_err(|e| format!("Failed to delete {relative}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, description: Option<&str>, valid: bool) -> AgentMemoryFile {
        AgentMemoryFile {
            path: path.to_string(),
            description: description.map(str::to_string),
            updated_at: None,
            size: 0,
            frontmatter_valid: valid,
            sort_key: 0,
        }
    }

    fn lesson(description: &str, updated_at: &str) -> String {
        format!("---\ndescription: {description}\nupdated_at: {updated_at}\n---\nBody\n")
    }

    #[test]
    fn frontmatter_needs_both_fields_and_a_closing_line() {
        let parsed =
            Frontmatter::parse("---\ndescription: \"Use when x\"\nupdated_at: 2026-11-10\n---\n");
        assert_eq!(
            parsed,
            Frontmatter {
                description: Some("Use when x".into()),
                updated_at: Some("2026-11-10".into()),
                closed: true,
            }
        );
        assert!(!Frontmatter::parse("---\ndescription: Use when x\n").closed);
        assert_eq!(
            Frontmatter::parse("# no frontmatter\n"),
            Frontmatter::default()
        );
        // Keys after the closing line are body text.
        assert_eq!(
            Frontmatter::parse("---\n---\ndescription: x\n").description,
            None
        );
        assert_eq!(parse_date("2026-11-10"), parse_date("2026-11-10T00:00:00"));
        assert!(parse_date("2026-11-10T08:00:00+01:00").is_some());
        assert_eq!(parse_date("yesterday"), None);
    }

    #[test]
    fn list_orders_newest_first_flags_broken_files_and_skips_the_rest() {
        let temp = tempfile::tempdir().unwrap();
        let dir = &temp.path().join("memory");
        fs::create_dir_all(dir.join("rust/.hidden")).unwrap();
        fs::write(dir.join("old.md"), lesson("Use when old", "2025-01-01")).unwrap();
        fs::write(
            dir.join("rust/new.md"),
            lesson("Use when new", "2026-11-10T09:00:00"),
        )
        .unwrap();
        fs::write(dir.join("broken.md"), "---\ndescription: Use when\n---\n").unwrap();
        fs::write(dir.join("bad-date.md"), lesson("Use when dated", "soon")).unwrap();
        fs::write(dir.join("notes.txt"), "not markdown").unwrap();
        fs::write(dir.join(".draft.md"), lesson("hidden", "2026-01-01")).unwrap();
        fs::write(
            dir.join("rust/.hidden/x.md"),
            lesson("hidden", "2026-01-01"),
        )
        .unwrap();
        #[cfg(unix)]
        {
            let outside = temp.path().join("outside");
            fs::create_dir_all(&outside).unwrap();
            fs::write(outside.join("leak.md"), lesson("leak", "2026-01-01")).unwrap();
            std::os::unix::fs::symlink(&outside, dir.join("linked")).unwrap();
            std::os::unix::fs::symlink(dir.join("old.md"), dir.join("alias.md")).unwrap();
        }

        let files = list(dir).unwrap();
        let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
        // Broken files have no date and sort by mtime: now, so first.
        assert_eq!(&paths[2..], ["rust/new.md", "old.md"]);
        let mut broken = paths[..2].to_vec();
        broken.sort_unstable();
        assert_eq!(broken, ["bad-date.md", "broken.md"]);
        let valid: Vec<bool> = files.iter().map(|f| f.frontmatter_valid).collect();
        assert_eq!(valid, [false, false, true, true]);
        assert_eq!(files[2].description.as_deref(), Some("Use when new"));
        assert!(list(&dir.join("missing")).unwrap().is_empty());
    }

    #[test]
    fn frontmatter_survives_a_character_split_at_the_read_limit() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("long.md");
        let mut content = lesson("Use when long", "2026-11-10");
        content.push_str(&"x".repeat(FRONTMATTER_BYTES as usize - content.len() - 1));
        content.push_str("é and more");
        assert!(!content.is_char_boundary(FRONTMATTER_BYTES as usize));
        fs::write(&path, &content).unwrap();
        let file = describe(&path, "long.md".into()).unwrap();
        assert!(file.frontmatter_valid);
        assert_eq!(file.description.as_deref(), Some("Use when long"));
    }

    #[test]
    fn catalog_flags_cuts_and_bounds_its_lines() {
        let long = "x".repeat(MAX_DESCRIPTION_CHARS + 50);
        let text = catalog(&[
            file("a.md", Some("Use when a"), true),
            file("b.md", None, false),
            file("c.md", Some(&long), true),
        ]);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "- `a.md` — Use when a");
        assert!(lines[1].starts_with("- `b.md` — FIX FRONTMATTER"));
        assert!(lines[2].ends_with(&format!("{}…", "x".repeat(MAX_DESCRIPTION_CHARS))));
        assert_eq!(lines.len(), 3);

        let many: Vec<_> = (0..MAX_CATALOG_ENTRIES + 5)
            .map(|i| file(&format!("{i}.md"), Some("Use when"), true))
            .collect();
        let text = catalog(&many);
        assert_eq!(text.lines().count(), MAX_CATALOG_ENTRIES + 1);
        assert!(text.ends_with("- … 5 older files not listed. Your memory has outgrown its catalog: consolidate overlapping lessons and delete stale ones.\n"));

        let wide: Vec<_> = (0..60)
            .map(|i| file(&format!("{i}.md"), Some(&long), true))
            .collect();
        let text = catalog(&wide);
        assert!(text.len() <= MAX_CATALOG_BYTES + 200);
        assert!(text.contains("older files not listed"));
        assert!(text.starts_with("- `0.md`"));
        assert!(catalog(&[]).is_empty());
    }

    #[test]
    fn definition_ids_and_file_paths_cannot_leave_the_memory_dir() {
        for id in ["", ".", "..", "../x", "a/b", ".hidden", "a b"] {
            assert!(memory_dir(Path::new("/agents"), id).is_err(), "{id:?}");
        }
        assert_eq!(
            memory_dir(Path::new("/agents"), "8d3bb9d1-a92a_x").unwrap(),
            Path::new("/agents/8d3bb9d1-a92a_x/memory")
        );

        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("memory");
        ensure_memory_dir(&dir).unwrap();
        for path in [
            "",
            "/etc/x.md",
            "../x.md",
            "a/../../x.md",
            "./x.md",
            ".x.md",
            "a/.b/x.md",
            "x.txt",
            "x.md/",
            "a\\b.md",
            "a\nb.md",
        ] {
            assert!(write(&dir, path, "x").is_err(), "{path:?}");
            assert!(read(&dir, path).is_err(), "{path:?}");
        }
        write(&dir, "craft/lesson.md", "hello").unwrap();
        assert_eq!(read(&dir, "craft/lesson.md").unwrap(), "hello");
        write(&dir, "craft/lesson.md", "again").unwrap();
        assert_eq!(read(&dir, "craft/lesson.md").unwrap(), "again");
        assert!(write(&dir, "big.md", &"x".repeat(MAX_FILE_BYTES + 1)).is_err());
        delete(&dir, "craft/lesson.md").unwrap();
        assert!(read(&dir, "craft/lesson.md").is_err());
        // No temp file is left behind.
        assert_eq!(fs::read_dir(dir.join("craft")).unwrap().count(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_never_followed() {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("memory");
        let outside = temp.path().join("outside");
        fs::create_dir_all(&dir).unwrap();
        fs::create_dir_all(&outside).unwrap();
        let secret = lesson("Use when leaking", "2026-01-01");
        fs::write(outside.join("secret.md"), &secret).unwrap();
        symlink(&outside, dir.join("sub")).unwrap();
        symlink(outside.join("secret.md"), dir.join("link.md")).unwrap();
        // A symlink swapped in after the listing saw a regular file.
        assert_eq!(describe(&dir.join("link.md"), "link.md".into()), None);

        assert!(read(&dir, "sub/secret.md").is_err());
        assert!(write(&dir, "sub/secret.md", "x").is_err());
        assert!(read(&dir, "link.md").is_err());
        assert!(delete(&dir, "sub/secret.md").is_err());
        assert_eq!(
            fs::read_to_string(outside.join("secret.md")).unwrap(),
            secret
        );

        let linked = temp.path().join("linked-memory");
        symlink(&outside, &linked).unwrap();
        assert!(ensure_memory_dir(&linked).is_err());
        assert!(list(&linked).is_err());
        assert!(read(&linked, "secret.md").is_err());
    }
}
