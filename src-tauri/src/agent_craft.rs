//! The craft of a shared agent definition: what it has learnt about how to do
//! its work, carried into every workspace that assigns it, kept in
//! `<app dir>/agents/<definition-id>/craft/`.
//!
//! Agents read and write it through the shell; the sandbox exposes only the
//! running member's own directory. This module lists it for the system
//! prompt's catalog and the settings UI, and reads, writes and deletes single
//! files for the UI by paths relative to the craft directory, refusing any
//! path `list` would not show.

use std::fs;
use std::io::{self, ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[cfg(unix)]
use rustix::fs::Mode;
use serde::Serialize;

/// Each description in the catalog is cut to this many characters.
const MAX_DESCRIPTION_CHARS: usize = 300;
/// The catalog stops at whichever bound comes first.
pub(crate) const MAX_CATALOG_ENTRIES: usize = 100;
const MAX_CATALOG_BYTES: usize = 8 * 1024;
/// Bounds on the walk, so a runaway directory cannot stall a run. Files sit
/// at most `MAX_DEPTH` folders deep.
const MAX_DEPTH: usize = 8;
pub(crate) const MAX_FILES: usize = 1_000;
/// Frontmatter must be within the start of a file, and characters are
/// counted only in a file no longer than this.
pub(crate) const HEAD_BYTES: u64 = 8 * 1024;
/// The largest file the settings UI reads or writes.
const MAX_FILE_BYTES: usize = 256 * 1024;

/// One craft file, as the catalog and the settings UI list it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "bindings.ts")]
pub struct AgentCraftFile {
    /// Relative to the craft directory, `/`-separated.
    pub path: String,
    pub description: Option<String>,
    /// As written in the frontmatter.
    pub updated_at: Option<String>,
    /// The file starts with frontmatter holding a `description` and an
    /// `updated_at` date.
    pub frontmatter_valid: bool,
    /// Newest first: `updated_at`, else the modification time, in seconds.
    #[serde(skip)]
    sort_key: i64,
    /// Characters in the file, frontmatter included; `None` when it is
    /// longer than `HEAD_BYTES` bytes, so over `HEAD_BYTES / 4` characters.
    #[serde(skip)]
    pub(crate) chars: Option<usize>,
}

/// The craft directory of `definition_id` under `agents_root`.
pub fn craft_dir(agents_root: &Path, definition_id: &str) -> Result<PathBuf, String> {
    validate_definition_id(definition_id)?;
    Ok(agents_root.join(definition_id).join("craft"))
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

/// Creates the craft directory. A symlink in its place is refused: the
/// sandbox would expose wherever it leads.
pub fn ensure_craft_dir(dir: &Path) -> Result<(), String> {
    fs::create_dir_all(dir)
        .map_err(|e| format!("Failed to create agent craft {}: {e}", dir.display()))?;
    open_root(dir).map(drop)
}

/// Every visible `.md` file under `dir`, newest first. Hidden entries and
/// symlinks are skipped; a missing directory lists nothing.
pub fn list(dir: &Path) -> Result<Vec<AgentCraftFile>, String> {
    let mut files = Vec::new();
    if let Some(root) = open_root(dir)? {
        walk(&root, "", 0, &mut files);
    }
    files.sort_by(|a, b| {
        b.sort_key
            .cmp(&a.sort_key)
            .then_with(|| a.path.cmp(&b.path))
    });
    Ok(files)
}

fn walk(folder: &Folder, prefix: &str, depth: usize, files: &mut Vec<AgentCraftFile>) {
    let Ok(mut names) = folder.names() else {
        return;
    };
    names.sort_unstable();
    for name in names {
        if files.len() >= MAX_FILES {
            return;
        }
        if !valid_component(&name) {
            continue;
        }
        let path = format!("{prefix}{name}");
        match folder.kind(&name) {
            Ok(Kind::Folder) if depth < MAX_DEPTH => {
                if let Ok(child) = folder.child(&name) {
                    walk(&child, &format!("{path}/"), depth + 1, files);
                }
            }
            Ok(Kind::File) if is_craft_file_name(&name) => {
                files.extend(describe(folder, &name, path));
            }
            _ => {}
        }
    }
}

fn describe(folder: &Folder, name: &str, relative: String) -> Option<AgentCraftFile> {
    let (file, meta) = folder.open_file(name).ok()?;
    let mut head = Vec::new();
    let _ = file.take(HEAD_BYTES).read_to_end(&mut head);
    let chars = (meta.len() <= HEAD_BYTES).then(|| String::from_utf8_lossy(&head).chars().count());
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
    Some(AgentCraftFile {
        path: relative,
        frontmatter_valid: frontmatter.closed
            && frontmatter.description.is_some()
            && updated.is_some(),
        description: frontmatter.description,
        updated_at: frontmatter.updated_at,
        sort_key: updated.unwrap_or(modified),
        chars,
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
pub fn catalog(files: &[AgentCraftFile]) -> String {
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
            "- … {} older files not listed: list the directory to see them.\n",
            files.len() - shown
        ));
    }
    catalog
}

/// A path component the catalog lists: a visible name, free of control
/// characters. Backslashes, and on Windows colons (drive prefixes, alternate
/// data streams), are refused so a name cannot become a path.
fn valid_component(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('.')
        && !name
            .chars()
            .any(|c| c.is_control() || c == '\\' || (cfg!(windows) && c == ':'))
}

fn is_craft_file_name(name: &str) -> bool {
    name.ends_with(".md")
}

/// The `/`-separated names of `relative` if `list` could return it: valid
/// components naming a Markdown file no deeper than the walk goes.
fn listable_names(relative: &str) -> Option<Vec<&str>> {
    let names: Vec<&str> = relative.split('/').collect();
    let (file, folders) = names.split_last()?;
    let listable = folders.len() <= MAX_DEPTH
        && is_craft_file_name(file)
        && names.iter().all(|name| valid_component(name));
    listable.then_some(names)
}

fn invalid_path(relative: &str) -> String {
    format!("Invalid craft file path: {relative:?}")
}

/// A file operation; only writing creates the folders on the way.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Op {
    Read,
    Write,
    Delete,
}

impl Op {
    fn failed(self, relative: &str, error: io::Error) -> String {
        let verb = match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::Delete => "delete",
        };
        format!("Failed to {verb} {relative}: {error}")
    }
}

/// The folder holding `relative`'s file, and the file's name; `None` when a
/// folder on the way does not exist and `op` does not create it. Refused
/// unless `list` could show the path: every folder on the way is a real
/// folder, and the file, if it exists, is a regular one.
fn locate<'a>(dir: &Path, relative: &'a str, op: Op) -> Result<Option<(Folder, &'a str)>, String> {
    let names = listable_names(relative).ok_or_else(|| invalid_path(relative))?;
    let (file, folders) = names.split_last().expect("listable paths have a file");
    let Some(mut folder) = open_root(dir)? else {
        return Ok(None);
    };
    let failed = |e: io::Error| op.failed(relative, e);
    for name in folders {
        folder = match folder.kind(name) {
            Ok(Kind::Folder) => folder.child(name),
            Ok(_) => return Err(invalid_path(relative)),
            Err(e) if e.kind() != ErrorKind::NotFound => return Err(failed(e)),
            Err(_) if op == Op::Write => folder.child_or_create(name),
            Err(_) => return Ok(None),
        }
        .map_err(failed)?;
    }
    match folder.kind(file) {
        Ok(Kind::File) => Ok(Some((folder, file))),
        Ok(_) => Err(invalid_path(relative)),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(Some((folder, file))),
        Err(e) => Err(failed(e)),
    }
}

/// The craft directory, or `None` when it does not exist.
fn open_root(dir: &Path) -> Result<Option<Folder>, String> {
    match Folder::open(dir) {
        Ok(folder) => Ok(Some(folder)),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("Agent craft {} is unavailable: {e}", dir.display())),
    }
}

/// What a folder entry is, without following a symlink.
enum Kind {
    File,
    Folder,
    Other,
}

/// A folder inside a craft directory. On Unix it is an open handle reached
/// from the craft directory one folder at a time without following a
/// symlink, and every operation, listing included, is relative to it, so
/// swapping a folder for a symlink midway cannot lead one outside the craft
/// directory.
#[cfg(unix)]
struct Folder(std::os::fd::OwnedFd);

#[cfg(unix)]
impl Folder {
    const FLAGS: rustix::fs::OFlags = rustix::fs::OFlags::RDONLY
        .union(rustix::fs::OFlags::DIRECTORY)
        .union(rustix::fs::OFlags::NOFOLLOW)
        .union(rustix::fs::OFlags::CLOEXEC);

    fn open(dir: &Path) -> io::Result<Self> {
        Ok(Self(rustix::fs::open(dir, Self::FLAGS, Mode::empty())?))
    }

    fn child(&self, name: &str) -> io::Result<Self> {
        Ok(Self(rustix::fs::openat(
            &self.0,
            name,
            Self::FLAGS,
            Mode::empty(),
        )?))
    }

    fn child_or_create(&self, name: &str) -> io::Result<Self> {
        match rustix::fs::mkdirat(&self.0, name, Mode::from_raw_mode(0o777)) {
            Ok(()) | Err(rustix::io::Errno::EXIST) => self.child(name),
            Err(e) => Err(e.into()),
        }
    }

    fn names(&self) -> io::Result<Vec<String>> {
        Ok(rustix::fs::Dir::read_from(&self.0)?
            .flatten()
            .filter_map(|entry| entry.file_name().to_str().ok().map(str::to_string))
            .collect())
    }

    fn kind(&self, name: &str) -> io::Result<Kind> {
        use rustix::fs::{statat, AtFlags, FileType};
        let stat = statat(&self.0, name, AtFlags::SYMLINK_NOFOLLOW)?;
        Ok(match FileType::from_raw_mode(stat.st_mode) {
            FileType::RegularFile => Kind::File,
            FileType::Directory => Kind::Folder,
            _ => Kind::Other,
        })
    }

    /// Non-blocking, so a FIFO swapped in is refused rather than waited on.
    fn open_file(&self, name: &str) -> io::Result<(fs::File, fs::Metadata)> {
        use rustix::fs::OFlags;
        let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
        let fd = rustix::fs::openat(&self.0, name, flags, Mode::empty())?;
        regular_file(fd.into())
    }

    fn create_new(&self, name: &str) -> io::Result<fs::File> {
        use rustix::fs::OFlags;
        let flags =
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        let fd = rustix::fs::openat(&self.0, name, flags, Mode::from_raw_mode(0o666))?;
        Ok(fd.into())
    }

    fn rename(&self, from: &str, to: &str) -> io::Result<()> {
        Ok(rustix::fs::renameat(&self.0, from, &self.0, to)?)
    }

    fn remove(&self, name: &str) -> io::Result<()> {
        Ok(rustix::fs::unlinkat(
            &self.0,
            name,
            rustix::fs::AtFlags::empty(),
        )?)
    }
}

/// Elsewhere the folder is a checked path, so a folder or file swapped for a
/// symlink between the check and the use is followed. Agents' shells run unsandboxed
/// there, so the race reaches nothing they cannot already reach.
#[cfg(not(unix))]
struct Folder(PathBuf);

#[cfg(not(unix))]
impl Folder {
    fn open(dir: &Path) -> io::Result<Self> {
        Self::real_dir(dir.to_path_buf())
    }

    fn child(&self, name: &str) -> io::Result<Self> {
        Self::real_dir(self.0.join(name))
    }

    fn child_or_create(&self, name: &str) -> io::Result<Self> {
        match fs::create_dir(self.0.join(name)) {
            Err(e) if e.kind() != ErrorKind::AlreadyExists => Err(e),
            _ => self.child(name),
        }
    }

    fn real_dir(path: PathBuf) -> io::Result<Self> {
        if fs::symlink_metadata(&path)?.is_dir() {
            Ok(Self(path))
        } else {
            Err(io::Error::new(ErrorKind::InvalidInput, "not a directory"))
        }
    }

    fn names(&self) -> io::Result<Vec<String>> {
        Ok(fs::read_dir(&self.0)?
            .flatten()
            .filter_map(|entry| entry.file_name().into_string().ok())
            .collect())
    }

    fn kind(&self, name: &str) -> io::Result<Kind> {
        let meta = fs::symlink_metadata(self.0.join(name))?;
        Ok(if meta.is_file() {
            Kind::File
        } else if meta.is_dir() {
            Kind::Folder
        } else {
            Kind::Other
        })
    }

    fn open_file(&self, name: &str) -> io::Result<(fs::File, fs::Metadata)> {
        regular_file(fs::File::open(self.0.join(name))?)
    }

    fn create_new(&self, name: &str) -> io::Result<fs::File> {
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(self.0.join(name))
    }

    fn rename(&self, from: &str, to: &str) -> io::Result<()> {
        fs::rename(self.0.join(from), self.0.join(to))
    }

    fn remove(&self, name: &str) -> io::Result<()> {
        fs::remove_file(self.0.join(name))
    }
}

fn regular_file(file: fs::File) -> io::Result<(fs::File, fs::Metadata)> {
    let meta = file.metadata()?;
    if meta.is_file() {
        Ok((file, meta))
    } else {
        Err(io::Error::new(
            ErrorKind::InvalidInput,
            "not a regular file",
        ))
    }
}

pub fn read(dir: &Path, relative: &str) -> Result<String, String> {
    let failed = |e| Op::Read.failed(relative, e);
    let (folder, name) =
        locate(dir, relative, Op::Read)?.ok_or_else(|| failed(ErrorKind::NotFound.into()))?;
    let (file, meta) = folder.open_file(name).map_err(failed)?;
    if meta.len() > MAX_FILE_BYTES as u64 {
        return Err(format!("{relative} is larger than {MAX_FILE_BYTES} bytes"));
    }
    let mut content = String::new();
    file.take(MAX_FILE_BYTES as u64)
        .read_to_string(&mut content)
        .map_err(failed)?;
    Ok(content)
}

/// Creates or replaces the file, and any folders on the way.
pub fn write(dir: &Path, relative: &str, content: &str) -> Result<(), String> {
    if content.len() > MAX_FILE_BYTES {
        return Err(format!("Craft files are limited to {MAX_FILE_BYTES} bytes"));
    }
    let failed = |e| Op::Write.failed(relative, e);
    let (folder, name) =
        locate(dir, relative, Op::Write)?.ok_or_else(|| failed(ErrorKind::NotFound.into()))?;
    replace(&folder, name, content).map_err(failed)
}

/// Writes a new file and renames it into place, so a failed write leaves the
/// old file intact and a symlink swapped in at `name` is replaced, not
/// followed.
fn replace(folder: &Folder, name: &str, content: &str) -> io::Result<()> {
    // Hidden, so `list` skips it if it is ever left behind.
    let temp = format!(".{}.tmp", uuid::Uuid::new_v4());
    let result = folder
        .create_new(&temp)
        .and_then(|mut file| {
            file.write_all(content.as_bytes())?;
            file.sync_all()
        })
        .and_then(|()| folder.rename(&temp, name));
    if result.is_err() {
        let _ = folder.remove(&temp);
    }
    result
}

/// Deleting a file that is already gone, or whose folder is, succeeds.
pub fn delete(dir: &Path, relative: &str) -> Result<(), String> {
    let Some((folder, name)) = locate(dir, relative, Op::Delete)? else {
        return Ok(());
    };
    match folder.remove(name) {
        Err(e) if e.kind() != ErrorKind::NotFound => Err(Op::Delete.failed(relative, e)),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, description: Option<&str>, valid: bool) -> AgentCraftFile {
        AgentCraftFile {
            path: path.to_string(),
            description: description.map(str::to_string),
            updated_at: None,
            frontmatter_valid: valid,
            sort_key: 0,
            chars: None,
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
        let dir = &temp.path().join("craft");
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
        content.push_str(&"x".repeat(HEAD_BYTES as usize - content.len() - 1));
        content.push_str("é and more");
        assert!(!content.is_char_boundary(HEAD_BYTES as usize));
        fs::write(&path, &content).unwrap();
        let folder = Folder::open(temp.path()).unwrap();
        let file = describe(&folder, "long.md", "long.md".into()).unwrap();
        assert!(file.frontmatter_valid);
        assert_eq!(file.description.as_deref(), Some("Use when long"));
    }

    #[test]
    fn files_are_measured_in_characters_up_to_the_read_limit() {
        let temp = tempfile::tempdir().unwrap();
        let folder = Folder::open(temp.path()).unwrap();
        let chars = |content: &str| {
            fs::write(temp.path().join("f.md"), content).unwrap();
            describe(&folder, "f.md", "f.md".into()).unwrap().chars
        };
        assert_eq!(chars("ñandú\n"), Some(6));
        let limit = HEAD_BYTES as usize;
        assert_eq!(chars(&"é".repeat(limit / 2)), Some(limit / 2));
        assert_eq!(chars(&"x".repeat(limit + 1)), None);
        // Each invalid byte counts as one replacement character.
        fs::write(temp.path().join("f.md"), b"\xff\xfea").unwrap();
        let file = describe(&folder, "f.md", "f.md".into()).unwrap();
        assert_eq!(file.chars, Some(3));
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
        assert!(text.ends_with("- … 5 older files not listed: list the directory to see them.\n"));

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
    fn definition_ids_cannot_leave_the_agents_root() {
        for id in ["", ".", "..", "../x", "a/b", ".hidden", "a b"] {
            assert!(craft_dir(Path::new("/agents"), id).is_err(), "{id:?}");
        }
        assert_eq!(
            craft_dir(Path::new("/agents"), "8d3bb9d1-a92a_x").unwrap(),
            Path::new("/agents/8d3bb9d1-a92a_x/craft")
        );
    }

    #[test]
    fn ensure_craft_dir_is_idempotent_and_starts_empty() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("craft");
        ensure_craft_dir(&dir).unwrap();
        ensure_craft_dir(&dir).unwrap();
        assert!(list(&dir).unwrap().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_never_followed() {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("craft");
        let outside = temp.path().join("outside");
        fs::create_dir_all(&dir).unwrap();
        fs::create_dir_all(&outside).unwrap();
        let secret = lesson("Use when leaking", "2026-01-01");
        fs::write(outside.join("secret.md"), &secret).unwrap();
        symlink(&outside, dir.join("sub")).unwrap();
        symlink(outside.join("secret.md"), dir.join("link.md")).unwrap();
        // A symlink swapped in after the listing saw a regular file.
        let folder = Folder::open(&dir).unwrap();
        assert_eq!(describe(&folder, "link.md", "link.md".into()), None);
        assert!(list(&dir).unwrap().is_empty());

        let linked = temp.path().join("linked-craft");
        symlink(&outside, &linked).unwrap();
        assert!(ensure_craft_dir(&linked).is_err());
        assert!(list(&linked).is_err());
    }

    /// A FIFO swapped in for a file is refused at once: opening it does not
    /// wait for a writer, and it is not read as a file.
    #[cfg(unix)]
    #[test]
    fn a_fifo_is_refused_without_blocking() {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("craft");
        fs::create_dir_all(&dir).unwrap();
        let fifo = dir.join("pipe.md");
        let c_path = CString::new(fifo.as_os_str().as_bytes()).unwrap();
        // SAFETY: `c_path` is a valid NUL-terminated path.
        assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);

        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let folder = Folder::open(&dir).unwrap();
            let _ = sender.send((
                folder
                    .open_file("pipe.md")
                    .map(|_| ())
                    .map_err(|e| e.kind()),
                describe(&folder, "pipe.md", "pipe.md".into()),
                list(&dir).map(|files| files.len()),
            ));
        });
        let (opened, described, listed) = receiver
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("opening a FIFO blocked");
        assert_eq!(opened, Err(std::io::ErrorKind::InvalidInput));
        assert_eq!(described, None);
        assert_eq!(listed, Ok(0));
    }

    #[test]
    fn files_round_trip_through_write_read_and_delete() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("craft");
        ensure_craft_dir(&dir).unwrap();
        let content = lesson("Use when nested", "2026-11-15");

        write(&dir, "rust/async/lesson.md", &content).unwrap();
        let listed: Vec<String> = list(&dir).unwrap().into_iter().map(|f| f.path).collect();
        assert_eq!(listed, ["rust/async/lesson.md"]);
        assert_eq!(read(&dir, &listed[0]).unwrap(), content);
        assert_eq!(
            list(&dir).unwrap()[0].updated_at.as_deref(),
            Some("2026-11-15")
        );

        write(&dir, "rust/async/lesson.md", "replaced").unwrap();
        assert_eq!(read(&dir, "rust/async/lesson.md").unwrap(), "replaced");
        // Only the file itself: no temporary file is left beside it.
        assert_eq!(
            fs::read_dir(dir.join("rust").join("async"))
                .unwrap()
                .count(),
            1
        );

        delete(&dir, "rust/async/lesson.md").unwrap();
        assert!(list(&dir).unwrap().is_empty());
        delete(&dir, "rust/async/lesson.md").unwrap();
        delete(&dir, "missing/lesson.md").unwrap();
        delete(&temp.path().join("no-craft"), "lesson.md").unwrap();
        assert!(read(&dir, "rust/async/lesson.md")
            .unwrap_err()
            .starts_with("Failed to read rust/async/lesson.md"));
    }

    #[test]
    fn paths_list_would_not_show_are_refused() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("craft");
        ensure_craft_dir(&dir).unwrap();
        fs::write(dir.join("notes.txt"), "x").unwrap();
        fs::write(dir.join(".hidden.md"), "x").unwrap();
        fs::create_dir(dir.join("folder.md")).unwrap();
        let absolute = temp.path().join("outside.md");
        let too_deep = format!("{}x.md", "d/".repeat(MAX_DEPTH + 1));
        let deepest = format!("{}x.md", "d/".repeat(MAX_DEPTH));
        let refused = [
            "",
            absolute.to_str().unwrap(),
            "../x.md",
            "a/../../x.md",
            "./x.md",
            "a//x.md",
            "x.md/",
            ".hidden.md",
            "a/.b/x.md",
            "notes.txt",
            "notes.txt/x.md",
            "folder.md",
            "a\\b.md",
            "a\nb.md",
            &too_deep,
        ];
        for path in refused {
            assert_eq!(read(&dir, path), Err(invalid_path(path)), "{path:?}");
            assert_eq!(write(&dir, path, "x"), Err(invalid_path(path)), "{path:?}");
            assert_eq!(delete(&dir, path), Err(invalid_path(path)), "{path:?}");
        }
        assert!(!absolute.exists());
        assert_eq!(fs::read_to_string(dir.join(".hidden.md")).unwrap(), "x");
        assert!(dir.join("folder.md").is_dir());
        assert!(!dir.join("a").exists());

        // The deepest file `list` still shows is accepted; one deeper is not
        // listed either.
        write(&dir, &deepest, &lesson("Use when deep", "2026-11-15")).unwrap();
        fs::create_dir_all(dir.join(&too_deep).parent().unwrap()).unwrap();
        fs::write(dir.join(&too_deep), lesson("Use when deep", "2026-11-15")).unwrap();
        let listed: Vec<String> = list(&dir).unwrap().into_iter().map(|f| f.path).collect();
        assert_eq!(listed, [deepest]);
    }

    /// A failed replacement removes its temporary file.
    #[test]
    fn a_failed_replace_leaves_no_temporary_file() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir(temp.path().join("x.md")).unwrap();
        let folder = Folder::open(temp.path()).unwrap();
        assert!(replace(&folder, "x.md", "x").is_err());
        let names: Vec<_> = fs::read_dir(temp.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names, ["x.md"]);
    }

    /// I/O errors are reported as such, not as an invalid path.
    #[cfg(unix)]
    #[test]
    fn io_errors_are_not_reported_as_invalid_paths() {
        use std::os::unix::fs::PermissionsExt;
        // SAFETY: geteuid has no preconditions and cannot fail.
        if unsafe { libc::geteuid() } == 0 {
            return; // Root ignores the permissions this test relies on.
        }
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("craft");
        ensure_craft_dir(&dir).unwrap();
        let locked = dir.join("locked");
        fs::create_dir(&locked).unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o500)).unwrap();
        let created = write(&dir, "locked/sub/x.md", "x");
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        let results = [
            read(&dir, "locked/x.md").map(drop),
            delete(&dir, "locked/x.md"),
        ];
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o700)).unwrap();

        let denied = || io::Error::from_raw_os_error(libc::EACCES);
        assert_eq!(created, Err(Op::Write.failed("locked/sub/x.md", denied())));
        assert_eq!(
            results,
            [
                Err(Op::Read.failed("locked/x.md", denied())),
                Err(Op::Delete.failed("locked/x.md", denied())),
            ]
        );
    }

    #[test]
    fn oversized_content_is_refused_and_the_old_file_kept() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("craft");
        ensure_craft_dir(&dir).unwrap();
        write(&dir, "a.md", "old").unwrap();
        assert!(write(&dir, "a.md", &"x".repeat(MAX_FILE_BYTES + 1)).is_err());
        assert_eq!(read(&dir, "a.md").unwrap(), "old");

        fs::write(dir.join("big.md"), "x".repeat(MAX_FILE_BYTES + 1)).unwrap();
        assert!(read(&dir, "big.md").unwrap_err().contains("larger than"));
    }

    #[cfg(unix)]
    #[test]
    fn file_operations_never_follow_symlinks() {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("craft");
        let outside = temp.path().join("outside");
        fs::create_dir_all(&dir).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("secret.md"), "secret").unwrap();
        symlink(&outside, dir.join("sub")).unwrap();
        symlink(outside.join("secret.md"), dir.join("link.md")).unwrap();

        for path in [
            "sub/secret.md",
            "sub/new.md",
            "sub/deeper/new.md",
            "link.md",
        ] {
            assert_eq!(read(&dir, path), Err(invalid_path(path)), "{path:?}");
            assert_eq!(write(&dir, path, "x"), Err(invalid_path(path)), "{path:?}");
            assert_eq!(delete(&dir, path), Err(invalid_path(path)), "{path:?}");
        }
        assert_eq!(
            fs::read_to_string(outside.join("secret.md")).unwrap(),
            "secret"
        );
        assert_eq!(fs::read_dir(&outside).unwrap().count(), 1);
        assert!(fs::symlink_metadata(dir.join("link.md")).is_ok());

        let linked = temp.path().join("linked-craft");
        symlink(&outside, &linked).unwrap();
        assert!(read(&linked, "secret.md").is_err());
        assert!(write(&linked, "new.md", "x").is_err());
        assert!(delete(&linked, "secret.md").is_err());
        assert_eq!(fs::read_dir(&outside).unwrap().count(), 1);
    }

    /// File operations on a FIFO are refused at once, without waiting for a
    /// writer or reader.
    #[cfg(unix)]
    #[test]
    fn file_operations_refuse_a_fifo_without_blocking() {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::fs::FileTypeExt;
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("craft");
        fs::create_dir_all(&dir).unwrap();
        let fifo = dir.join("pipe.md");
        let c_path = CString::new(fifo.as_os_str().as_bytes()).unwrap();
        // SAFETY: `c_path` is a valid NUL-terminated path.
        assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);

        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = sender.send([
                read(&dir, "pipe.md").map(|_| ()),
                write(&dir, "pipe.md", "x"),
                delete(&dir, "pipe.md"),
            ]);
        });
        let results = receiver
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("a FIFO blocked");
        let refused = Err(invalid_path("pipe.md"));
        assert_eq!(results, [refused.clone(), refused.clone(), refused]);
        assert!(fs::symlink_metadata(&fifo).unwrap().file_type().is_fifo());
    }

    /// An agent swaps a folder for a symlink after CLAI opened it: the
    /// operations still act on the folder that was opened.
    #[cfg(unix)]
    #[test]
    fn a_folder_swapped_for_a_symlink_after_opening_is_not_followed() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("craft");
        let outside = temp.path().join("outside");
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("secret.md"), "secret").unwrap();
        ensure_craft_dir(&dir).unwrap();
        write(&dir, "x/lesson.md", "lesson").unwrap();

        let (folder, name) = locate(&dir, "x/lesson.md", Op::Read).unwrap().unwrap();
        fs::rename(dir.join("x"), dir.join("moved")).unwrap();
        std::os::unix::fs::symlink(&outside, dir.join("x")).unwrap();

        replace(&folder, "secret.md", "agent").unwrap();
        folder.remove("secret.md").unwrap();
        replace(&folder, name, "edited").unwrap();
        assert_eq!(
            fs::read_to_string(outside.join("secret.md")).unwrap(),
            "secret"
        );
        assert_eq!(fs::read_dir(&outside).unwrap().count(), 1);
        assert_eq!(
            fs::read_to_string(dir.join("moved").join(name)).unwrap(),
            "edited"
        );
    }

    /// An agent swaps a folder for a symlink after `list` opened it: the walk
    /// goes on in the folder that was opened, and nothing outside is listed.
    #[cfg(unix)]
    #[test]
    fn list_does_not_follow_a_folder_swapped_for_a_symlink() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("craft");
        let outside = temp.path().join("outside");
        fs::create_dir_all(&outside).unwrap();
        fs::write(
            outside.join("leak.md"),
            lesson("Use when leaking", "2026-01-01"),
        )
        .unwrap();
        ensure_craft_dir(&dir).unwrap();
        write(
            &dir,
            "x/lesson.md",
            &lesson("Use when inside", "2026-01-01"),
        )
        .unwrap();

        let folder = Folder::open(&dir).unwrap().child("x").unwrap();
        fs::rename(dir.join("x"), dir.join("moved")).unwrap();
        std::os::unix::fs::symlink(&outside, dir.join("x")).unwrap();

        let mut files = Vec::new();
        walk(&folder, "x/", 1, &mut files);
        let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, ["x/lesson.md"]);
        assert_eq!(files[0].description.as_deref(), Some("Use when inside"));
        let paths: Vec<String> = list(&dir).unwrap().into_iter().map(|f| f.path).collect();
        assert_eq!(paths, ["moved/lesson.md"]);
    }

    #[cfg(windows)]
    #[test]
    fn windows_drive_prefixes_and_streams_are_refused() {
        for path in ["C:x.md", "a.txt:x.md"] {
            assert_eq!(listable_names(path), None, "{path:?}");
        }
    }
}
