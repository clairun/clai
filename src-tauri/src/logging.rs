//! Durable application logging.
//!
//! The app previously configured `tracing_subscriber::fmt()` with no file sink,
//! so all output went to stdout. journald only captures that when the app is
//! launched from its `.desktop` entry — a terminal-launched run left no durable
//! record at all, and any after-the-fact log review had nothing to read.
//!
//! This module adds a daily-rotating file sink alongside stdout, implemented on
//! `std` plus `chrono` rather than pulling in `tracing-appender`. The rotation
//! and retention rules are the only interesting part, and keeping them here
//! makes them unit-testable, which a third-party appender would not have been.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use tracing_subscriber::fmt::MakeWriter;

/// How many daily log files to keep. The oldest are deleted beyond this.
pub const LOG_RETENTION_DAYS: usize = 14;

const FILE_PREFIX: &str = "clai-";
const FILE_SUFFIX: &str = ".log";

/// Returns the log file name for a given `YYYY-MM-DD` stamp.
fn file_name_for(stamp: &str) -> String {
    format!("{FILE_PREFIX}{stamp}{FILE_SUFFIX}")
}

/// Today's stamp in local time.
///
/// Local rather than UTC deliberately: these logs are read by a human
/// correlating them against when they were sitting at the machine, so the file
/// boundary should match their day, not UTC's.
fn today_stamp() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

/// Picks which log file names to delete, given every candidate name in the
/// directory and how many to retain.
///
/// Names are `clai-YYYY-MM-DD.log`, which sorts lexicographically in date
/// order, so "newest" is just "last". Anything not matching the pattern is
/// ignored rather than deleted — this directory belongs to the user and may
/// hold files we did not write.
///
/// Split out as a pure function because it is the part with the off-by-one:
/// retaining `n` files while *writing* the `n`th must not delete the file
/// currently open.
fn names_to_prune(mut candidates: Vec<String>, retain: usize) -> Vec<String> {
    candidates.retain(|name| is_log_file_name(name));
    candidates.sort();
    if candidates.len() <= retain {
        return Vec::new();
    }
    let cut = candidates.len() - retain;
    candidates.truncate(cut);
    candidates
}

/// True when `name` is one of our own daily log files.
///
/// The date is *parsed*, not merely length-checked: `clai-not-a-date.log` has
/// exactly the same length as `clai-2026-08-01.log`, and because it sorts after
/// every real date it would otherwise be treated as the newest file and consume
/// a retention slot, deleting a real log instead.
fn is_log_file_name(name: &str) -> bool {
    name.strip_prefix(FILE_PREFIX)
        .and_then(|rest| rest.strip_suffix(FILE_SUFFIX))
        .is_some_and(|stamp| chrono::NaiveDate::parse_from_str(stamp, "%Y-%m-%d").is_ok())
}

/// Deletes log files beyond the retention window. Best-effort: a file we cannot
/// remove is left alone rather than failing the write that triggered the sweep.
fn prune(dir: &Path, retain: usize) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let names: Vec<String> = entries
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect();
    for name in names_to_prune(names, retain) {
        let _ = std::fs::remove_file(dir.join(name));
    }
}

/// The currently open log file and the day it belongs to.
struct OpenLog {
    stamp: String,
    file: File,
}

/// A `MakeWriter` that appends to `clai-<today>.log`, switching files when the
/// local date changes.
pub struct DailyFile {
    dir: PathBuf,
    retain: usize,
    open: Mutex<Option<OpenLog>>,
}

impl DailyFile {
    /// Creates the sink, creating the directory if needed.
    ///
    /// Opens today's file eagerly so that a permissions or disk problem is
    /// discovered here — at startup, where the caller can fall back to
    /// stdout-only — rather than silently on the first log line.
    pub fn new(dir: PathBuf, retain: usize) -> io::Result<Self> {
        std::fs::create_dir_all(&dir)?;
        let stamp = today_stamp();
        let file = open_append(&dir, &stamp)?;
        prune(&dir, retain);
        Ok(Self {
            dir,
            retain,
            open: Mutex::new(Some(OpenLog { stamp, file })),
        })
    }
}

fn open_append(dir: &Path, stamp: &str) -> io::Result<File> {
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(file_name_for(stamp)))
}

/// Write handle holding the sink lock for the duration of one event.
///
/// `tracing`'s `fmt` layer may issue several `write` calls per event (message
/// then newline). Holding the lock across the whole returned writer keeps
/// concurrent events from interleaving mid-line, which is the same reason
/// `tracing_subscriber` ships a mutex-guard writer for `File`.
pub struct DailyFileWriter<'a> {
    guard: MutexGuard<'a, Option<OpenLog>>,
    dir: &'a Path,
    retain: usize,
}

impl io::Write for DailyFileWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let stamp = today_stamp();

        // Rotate when the date moved on. A failure to open the new file keeps
        // the old handle: continuing to write yesterday's file is far better
        // than dropping log lines.
        let needs_rotation = self
            .guard
            .as_ref()
            .map(|open| open.stamp != stamp)
            .unwrap_or(true);
        if needs_rotation {
            if let Ok(file) = open_append(self.dir, &stamp) {
                *self.guard = Some(OpenLog {
                    stamp: stamp.clone(),
                    file,
                });
                prune(self.dir, self.retain);
            }
        }

        match self.guard.as_mut() {
            Some(open) => open.file.write(buf),
            // Only reachable if the very first open failed *and* every
            // subsequent rotation attempt also failed. Report the byte count as
            // consumed so `tracing` does not spin retrying.
            None => Ok(buf.len()),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self.guard.as_mut() {
            Some(open) => open.file.flush(),
            None => Ok(()),
        }
    }
}

impl<'a> MakeWriter<'a> for DailyFile {
    type Writer = DailyFileWriter<'a>;

    fn make_writer(&'a self) -> Self::Writer {
        DailyFileWriter {
            // A poisoned lock means a previous logging thread panicked mid-write.
            // Recover the guard and carry on: losing logging because logging
            // once failed is the worst available outcome.
            guard: self.open.lock().unwrap_or_else(|e| e.into_inner()),
            dir: &self.dir,
            retain: self.retain,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn names_to_prune_keeps_the_newest_and_drops_the_rest() {
        let names = vec![
            "clai-2026-08-01.log".to_string(),
            "clai-2026-08-03.log".to_string(),
            "clai-2026-08-02.log".to_string(),
        ];
        assert_eq!(
            names_to_prune(names, 2),
            vec!["clai-2026-08-01.log".to_string()]
        );
    }

    #[test]
    fn names_to_prune_keeps_everything_within_the_window() {
        let names = vec![
            "clai-2026-08-01.log".to_string(),
            "clai-2026-08-02.log".to_string(),
        ];
        assert!(names_to_prune(names.clone(), 2).is_empty());
        assert!(names_to_prune(names, 14).is_empty());
    }

    #[test]
    fn names_to_prune_never_deletes_the_only_file() {
        // The file being written must survive its own retention sweep.
        let names = vec!["clai-2026-08-01.log".to_string()];
        assert!(names_to_prune(names, 1).is_empty());
    }

    #[test]
    fn names_to_prune_ignores_foreign_files() {
        // The log directory is under the user's home; never delete what we did
        // not write, however many files are present.
        let names = vec![
            "notes.txt".to_string(),
            "clai.log".to_string(),
            "clai-2026-08-01.log.gz".to_string(),
            "clai-not-a-date.log".to_string(),
            "clai-2026-08-01.log".to_string(),
            "clai-2026-08-02.log".to_string(),
        ];
        assert_eq!(
            names_to_prune(names, 1),
            vec!["clai-2026-08-01.log".to_string()]
        );
    }

    #[test]
    fn names_to_prune_rejects_same_length_non_dates() {
        // `clai-not-a-date.log` is byte-for-byte the same length as a real
        // dated name and sorts after every date, so a length-only filter would
        // treat it as the newest file and prune a real log in its place.
        let names = vec![
            "clai-not-a-date.log".to_string(),
            "clai-2026-08-02.log".to_string(),
        ];
        assert!(is_log_file_name("clai-2026-08-02.log"));
        assert!(!is_log_file_name("clai-not-a-date.log"));
        assert!(names_to_prune(names, 1).is_empty());
    }

    #[test]
    fn writes_land_in_todays_file() {
        let dir = std::env::temp_dir().join(format!("clai-log-test-{}", std::process::id()));
        let sink = DailyFile::new(dir.clone(), 3).expect("sink");

        {
            let mut writer = sink.make_writer();
            writer.write_all(b"hello\n").expect("write");
            writer.flush().expect("flush");
        }

        let path = dir.join(file_name_for(&today_stamp()));
        let body = std::fs::read_to_string(&path).expect("read back");
        assert!(body.contains("hello"), "log line missing from {path:?}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn new_creates_the_directory_and_prunes_on_startup() {
        let dir = std::env::temp_dir().join(format!("clai-log-prune-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("pre-create");

        // Two stale files plus whatever today's stamp is; retaining 1 must leave
        // exactly today's.
        for stamp in ["2020-01-01", "2020-01-02"] {
            std::fs::write(dir.join(file_name_for(stamp)), b"old").expect("seed");
        }

        let _sink = DailyFile::new(dir.clone(), 1).expect("sink");

        let remaining: Vec<String> = std::fs::read_dir(&dir)
            .expect("read dir")
            .filter_map(|e| e.ok())
            .filter_map(|e| e.file_name().into_string().ok())
            .collect();
        assert_eq!(
            remaining,
            vec![file_name_for(&today_stamp())],
            "startup prune should leave only today's file"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
