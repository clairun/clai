//! Read-only SQL access to *this workspace's* conversation history
//! database (`.clai/data.sqlite`) — and nothing else.
//!
//! This is the always-available recovery path: an agent that lost context
//! after a compaction, or that simply needs a verbatim past message, the
//! exact command that was run, or the full text of an old error, can query
//! the complete record without hitting a command-approval gate, because the
//! tool is *structurally* incapable of writing or escaping.
//!
//! Why this can be always-on where a raw `sqlite3` allowlist cannot:
//! - It uses the sqlite *library*, not the `sqlite3` CLI, so there are no
//!   dot-commands (`.shell`, `.system`, `.output`, `.import`, `.read`) and
//!   therefore no shell escape and no arbitrary file write.
//! - The connection is opened read-only (`SQLITE_OPEN_READONLY`) *and* with
//!   `PRAGMA query_only=ON`, so the engine itself refuses every write.
//! - `ATTACH` is hard-disabled on the connection (`SQLITE_LIMIT_ATTACHED` set
//!   to 0), so no statement can reach a second database file — not even a
//!   read of another workspace's `data.sqlite`. This is the structural
//!   isolation guarantee; the textual statement/keyword checks are only a
//!   friendly-error UX layer on top of it (sqlx executes every `;`-separated
//!   statement, so a textual check alone could be smuggled past).
//! - It is hard-wired to the current workspace's `data.sqlite`. It takes no
//!   path argument and can open no other file or workspace.
//!
//! Read power is otherwise complete: arbitrary `SELECT`/CTE/`json_extract`/
//! `PRAGMA table_info`/`sqlite_master` introspection over every row and
//! column, paginated with `LIMIT`/`OFFSET`.

use std::path::PathBuf;

use serde::Deserialize;
use sqlx::sqlite::{SqliteConnectOptions, SqliteRow};
use sqlx::{Column, ConnectOptions, Connection, Row, TypeInfo, ValueRef};
use tokio::time::Duration;

use super::ToolExecutionContext;

/// Default and ceiling on rows returned in one call. The agent pages
/// through larger result sets with `LIMIT`/`OFFSET` in the SQL itself.
const DEFAULT_MAX_ROWS: usize = 100;
const HARD_MAX_ROWS: usize = 1_000;
/// Byte ceiling on the serialized rows, mirroring `bash_exec`'s output cap.
const MAX_OUTPUT_CHARS: usize = 200_000;
/// Per-cell character cap so one giant `content_json` / tool-result blob
/// can't consume the whole budget; the agent narrows with SQL (`substr`,
/// `json_extract`) when it needs the full value.
const MAX_CELL_CHARS: usize = 20_000;

const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryQueryParams {
    sql: String,
    #[serde(default)]
    max_rows: Option<usize>,
}

pub async fn execute(
    context: &ToolExecutionContext,
    params: HistoryQueryParams,
) -> Result<serde_json::Value, String> {
    let workspace_root: PathBuf = context.workspace_root.clone().ok_or_else(|| {
        "history_query is unavailable because this session is not tied to an automation workspace"
            .to_string()
    })?;
    let db_path = crate::config::workspace_config::data_path(&workspace_root);
    if !db_path.exists() {
        return Err(format!(
            "Conversation database not found at {} (nothing has been recorded for this workspace yet)",
            db_path.display()
        ));
    }

    let sql = params.sql.trim();
    validate_read_only_sql(sql)?;

    let max_rows = params
        .max_rows
        .unwrap_or(DEFAULT_MAX_ROWS)
        .clamp(1, HARD_MAX_ROWS);

    run_query(&db_path, sql, max_rows).await
}

/// Open a dedicated READ-ONLY connection to `db_path`, run `sql`, and shape
/// the rows into JSON. This is the core safety guarantee: even if the
/// textual checks in `validate_read_only_sql` missed something, the engine
/// refuses every write on this handle. `read_only(true)` maps to
/// SQLITE_OPEN_READONLY; `query_only=ON` is belt-and-suspenders.
async fn run_query(
    db_path: &std::path::Path,
    sql: &str,
    max_rows: usize,
) -> Result<serde_json::Value, String> {
    let mut conn = SqliteConnectOptions::new()
        .filename(db_path)
        .read_only(true)
        .pragma("query_only", "ON")
        .busy_timeout(BUSY_TIMEOUT)
        .connect()
        .await
        .map_err(|e| format!("Failed to open conversation database read-only: {e}"))?;

    // STRUCTURAL containment: hard-disable ATTACH so no statement can reach a
    // second database file (e.g. another workspace's data.sqlite). The textual
    // gate in `validate_read_only_sql` is only a friendly-error UX layer and is
    // NOT relied upon for isolation — sqlx executes every `;`-separated
    // statement, and a comment/quote trick could smuggle one past a textual
    // check. Setting SQLITE_LIMIT_ATTACHED to 0 makes ATTACH fail at the engine
    // regardless. Combined with the read-only + query_only open above (writes)
    // and SQLite shipping with no file-reading SQL functions or extension
    // loading enabled, the connection can only ever read THIS database.
    disable_attach(&mut conn).await?;

    let result = sqlx::query(sql).fetch_all(&mut conn).await;
    let _ = conn.close().await;
    let rows = result.map_err(|e| format!("Query failed: {e}"))?;

    let columns: Vec<String> = rows
        .first()
        .map(|first| {
            first
                .columns()
                .iter()
                .map(|c| c.name().to_string())
                .collect()
        })
        .unwrap_or_default();

    let row_limit_hit = rows.len() > max_rows;
    let mut out_rows: Vec<serde_json::Value> = Vec::new();
    let mut bytes_used: usize = 0;
    let mut byte_limit_hit = false;
    for row in rows.iter().take(max_rows) {
        let obj = row_to_json(row);
        let serialized = serde_json::to_string(&obj).map(|s| s.len()).unwrap_or(0);
        // Always keep at least one row, even an oversized one (its cells are
        // already clipped), so a single huge row still returns something.
        if !out_rows.is_empty() && bytes_used + serialized > MAX_OUTPUT_CHARS {
            byte_limit_hit = true;
            break;
        }
        bytes_used += serialized;
        out_rows.push(obj);
    }

    let truncated = row_limit_hit || byte_limit_hit;
    Ok(serde_json::json!({
        "columns": columns,
        "rows": out_rows,
        "rowCount": out_rows.len(),
        "truncated": truncated,
        "note": truncation_note(row_limit_hit, byte_limit_hit, max_rows),
    }))
}

/// Set `SQLITE_LIMIT_ATTACHED` to 0 on the live connection, so `ATTACH`
/// fails at the engine ("too many attached databases"). This is the
/// structural guarantee that `history_query` cannot read any file other than
/// the database it opened.
async fn disable_attach(conn: &mut sqlx::sqlite::SqliteConnection) -> Result<(), String> {
    let mut handle = conn
        .lock_handle()
        .await
        .map_err(|e| format!("Failed to lock database handle: {e}"))?;
    let raw = handle.as_raw_handle().as_ptr();
    // SAFETY: `raw` is a valid `sqlite3*` for as long as `handle` (the lock
    // guard) is held, and `sqlite3_limit` only reads/sets a connection limit.
    unsafe {
        libsqlite3_sys::sqlite3_limit(raw, libsqlite3_sys::SQLITE_LIMIT_ATTACHED, 0);
    }
    Ok(())
}

fn truncation_note(row_limit_hit: bool, byte_limit_hit: bool, max_rows: usize) -> String {
    match (row_limit_hit, byte_limit_hit) {
        (true, _) => format!(
            "Result truncated at the {max_rows}-row limit. Page through more with LIMIT/OFFSET or raise maxRows (cap {HARD_MAX_ROWS})."
        ),
        (false, true) => {
            "Result truncated at the output byte limit. Narrow the columns (e.g. substr/json_extract) or add LIMIT.".to_string()
        }
        (false, false) => String::new(),
    }
}

fn row_to_json(row: &SqliteRow) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    for (i, col) in row.columns().iter().enumerate() {
        map.insert(col.name().to_string(), cell_to_json(row, i));
    }
    serde_json::Value::Object(map)
}

fn cell_to_json(row: &SqliteRow, i: usize) -> serde_json::Value {
    use serde_json::Value;

    // Inspect the value's *runtime* type, then drop the borrow before
    // decoding so the second `try_get` is a clean independent borrow.
    let type_name = {
        let raw = match row.try_get_raw(i) {
            Ok(raw) => raw,
            Err(_) => return Value::Null,
        };
        if raw.is_null() {
            return Value::Null;
        }
        raw.type_info().name().to_string()
    };

    match type_name.to_ascii_uppercase().as_str() {
        "INTEGER" | "INT" | "BIGINT" => row
            .try_get::<i64, _>(i)
            .map(Value::from)
            .unwrap_or(Value::Null),
        "REAL" | "FLOAT" | "DOUBLE" => row
            .try_get::<f64, _>(i)
            .map(Value::from)
            .unwrap_or(Value::Null),
        "BLOB" => row
            .try_get::<Vec<u8>, _>(i)
            .map(|bytes| Value::String(format!("<blob: {} bytes>", bytes.len())))
            .unwrap_or(Value::Null),
        // TEXT and any unusual affinity: decode as a string and clip.
        _ => match row.try_get::<String, _>(i) {
            Ok(text) => Value::String(clip_cell(&text)),
            Err(_) => Value::Null,
        },
    }
}

fn clip_cell(text: &str) -> String {
    let total = text.chars().count();
    if total <= MAX_CELL_CHARS {
        return text.to_string();
    }
    let mut clipped: String = text.chars().take(MAX_CELL_CHARS).collect();
    clipped.push_str(&format!("…[cell truncated; {total} chars total]"));
    clipped
}

/// Reject anything that isn't a single read-only statement. The read-only
/// connection is the real guarantee; these checks turn a raw engine error
/// ("attempt to write a readonly database") into a clear, self-correcting
/// message and close the `ATTACH` second-database hole.
fn validate_read_only_sql(sql: &str) -> Result<(), String> {
    // Strip comments first with a combined string/comment-aware scanner. The
    // earlier bug was that an unbalanced quote inside a comment (e.g. `/*\'*/`)
    // desynced the quote tracker in the `;`/keyword scans, letting a smuggled
    // `; ATTACH ...` slip through. Removing comments up front (without ever
    // treating comment markers inside string literals as comments) makes the
    // downstream quote-aware scans reliable. (Isolation no longer depends on
    // this gate — ATTACH is disabled at the engine — but the gate must still
    // not be trivially fooled.)
    let cleaned = strip_sql_comments(sql);
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        return Err("history_query requires a non-empty SQL statement.".to_string());
    }
    if has_multiple_statements(trimmed) {
        return Err(
            "history_query accepts a single statement; remove the extra `;`-separated statement(s)."
                .to_string(),
        );
    }
    if contains_keyword(trimmed, "attach") || contains_keyword(trimmed, "detach") {
        return Err(
            "history_query does not allow ATTACH/DETACH — it can only read this workspace's conversation database."
                .to_string(),
        );
    }
    let lead = leading_keyword(trimmed);
    const READ_LEADERS: &[&str] = &["select", "with", "explain", "pragma", "values"];
    if !READ_LEADERS.contains(&lead.as_str()) {
        return Err(format!(
            "history_query is read-only; the statement must begin with SELECT, WITH, EXPLAIN, PRAGMA, or VALUES (got `{}`). Use it to read the conversation record, not to modify it.",
            if lead.is_empty() { "<none>" } else { lead.as_str() }
        ));
    }
    Ok(())
}

/// The leading SQL keyword, lowercased. Skips leading `(` and whitespace so
/// `(SELECT ...)` and `  WITH ...` classify correctly.
fn leading_keyword(sql: &str) -> String {
    sql.trim_start_matches(|c: char| c == '(' || c.is_whitespace())
        .split(|c: char| !c.is_ascii_alphabetic())
        .next()
        .unwrap_or("")
        .to_ascii_lowercase()
}

/// True if `sql` contains a statement-terminating `;` followed by more
/// non-whitespace text (i.e. a second statement), ignoring `;` inside
/// quoted string/identifier literals.
fn has_multiple_statements(sql: &str) -> bool {
    let bytes = sql.as_bytes();
    let n = bytes.len();
    let mut in_single = false;
    let mut in_double = false;
    let mut i = 0;
    while i < n {
        let c = bytes[i] as char;
        match c {
            '\'' if !in_double => {
                // Doubled '' is an escaped quote inside a single-quoted string.
                if in_single && i + 1 < n && bytes[i + 1] == b'\'' {
                    i += 2;
                    continue;
                }
                in_single = !in_single;
            }
            '"' if !in_single => {
                if in_double && i + 1 < n && bytes[i + 1] == b'"' {
                    i += 2;
                    continue;
                }
                in_double = !in_double;
            }
            ';' if !in_single && !in_double => {
                if !sql[i + 1..].trim().is_empty() {
                    return true;
                }
            }
            _ => {}
        }
        i += 1;
    }
    false
}

/// Case-insensitive whole-word search for `keyword`, after blanking out
/// quoted literals so `WHERE text = 'attach'` is not a false positive.
fn contains_keyword(sql: &str, keyword: &str) -> bool {
    strip_string_literals(sql)
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .any(|token| token.eq_ignore_ascii_case(keyword))
}

/// Remove SQL comments (`-- ...` to end of line, and `/* ... */`) with a
/// single combined scanner that tracks string-literal and comment state
/// together. The precedence is the same as SQLite's tokenizer: inside a
/// string literal, `--` and `/*` are ordinary characters (not comments); and
/// inside a comment, `'` and `"` are ordinary characters (not string starts).
/// String literals are preserved verbatim; comment bodies become spaces (and
/// newlines are kept so line numbers/whitespace boundaries are stable). This
/// runs before the `;`/keyword scans so an unbalanced quote inside a comment
/// can no longer desync them.
fn strip_sql_comments(sql: &str) -> String {
    let bytes = sql.as_bytes();
    let n = bytes.len();
    let mut out = String::with_capacity(n);
    let mut in_single = false;
    let mut in_double = false;
    let mut in_line = false;
    let mut in_block = false;
    let mut i = 0;
    while i < n {
        let c = bytes[i] as char;
        let next = if i + 1 < n {
            Some(bytes[i + 1] as char)
        } else {
            None
        };

        if in_line {
            if c == '\n' {
                in_line = false;
                out.push('\n');
            }
            i += 1;
            continue;
        }
        if in_block {
            if c == '*' && next == Some('/') {
                in_block = false;
                out.push(' ');
                out.push(' ');
                i += 2;
                continue;
            }
            out.push(if c == '\n' { '\n' } else { ' ' });
            i += 1;
            continue;
        }
        if in_single {
            out.push(c);
            if c == '\'' {
                if next == Some('\'') {
                    out.push('\'');
                    i += 2;
                    continue;
                }
                in_single = false;
            }
            i += 1;
            continue;
        }
        if in_double {
            out.push(c);
            if c == '"' {
                if next == Some('"') {
                    out.push('"');
                    i += 2;
                    continue;
                }
                in_double = false;
            }
            i += 1;
            continue;
        }

        // Outside any string or comment.
        if c == '-' && next == Some('-') {
            in_line = true;
            out.push(' ');
            out.push(' ');
            i += 2;
            continue;
        }
        if c == '/' && next == Some('*') {
            in_block = true;
            out.push(' ');
            out.push(' ');
            i += 2;
            continue;
        }
        if c == '\'' {
            in_single = true;
        } else if c == '"' {
            in_double = true;
        }
        out.push(c);
        i += 1;
    }
    out
}

/// Replace the contents of quoted string/identifier literals with spaces so
/// keyword scanning never trips over literal text. Errs toward blanking.
fn strip_string_literals(sql: &str) -> String {
    let mut out = String::with_capacity(sql.len());
    let mut in_single = false;
    let mut in_double = false;
    for c in sql.chars() {
        match c {
            '\'' if !in_double => {
                in_single = !in_single;
                out.push(' ');
            }
            '"' if !in_single => {
                in_double = !in_double;
                out.push(' ');
            }
            _ => out.push(if in_single || in_double { ' ' } else { c }),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_read_statements() {
        for sql in [
            "SELECT * FROM assistant_messages",
            "  select 1",
            "WITH x AS (SELECT 1) SELECT * FROM x",
            "EXPLAIN QUERY PLAN SELECT 1",
            "PRAGMA table_info(assistant_messages)",
            "VALUES (1, 2)",
            "select * from t where body = 'has ; semicolon';",
        ] {
            assert!(validate_read_only_sql(sql).is_ok(), "should accept: {sql}");
        }
    }

    #[test]
    fn rejects_writes_by_leading_keyword() {
        for sql in [
            "DELETE FROM assistant_messages",
            "UPDATE assistant_runs SET status = 'x'",
            "INSERT INTO t VALUES (1)",
            "DROP TABLE t",
            "CREATE TABLE t (a)",
            "VACUUM",
        ] {
            assert!(validate_read_only_sql(sql).is_err(), "should reject: {sql}");
        }
    }

    #[test]
    fn rejects_multiple_statements() {
        assert!(validate_read_only_sql("SELECT 1; DROP TABLE t").is_err());
        assert!(validate_read_only_sql("SELECT 1; SELECT 2").is_err());
        // A single trailing semicolon is fine.
        assert!(validate_read_only_sql("SELECT 1;").is_ok());
        assert!(validate_read_only_sql("SELECT 1;   ").is_ok());
    }

    #[test]
    fn rejects_attach_detach() {
        assert!(validate_read_only_sql("ATTACH DATABASE 'x.db' AS evil").is_err());
        assert!(validate_read_only_sql("DETACH DATABASE evil").is_err());
    }

    #[test]
    fn attach_inside_string_literal_is_not_flagged() {
        // The word "attach" only appears inside a string literal, so it must
        // not be treated as the ATTACH statement.
        assert!(validate_read_only_sql(
            "SELECT * FROM assistant_messages WHERE content_json LIKE '%attach%'"
        )
        .is_ok());
    }

    #[test]
    fn rejects_comment_smuggled_attach_bypass() {
        // The exact bypass an independent review verified: an unbalanced quote
        // inside a block comment used to desync the quote tracker so the real
        // `;` and `ATTACH` were treated as if inside a string literal.
        let evil = "SELECT 1 /*\'*/; ATTACH DATABASE \'/tmp/x.db\' AS o; SELECT * FROM o.secrets";
        assert!(validate_read_only_sql(evil).is_err());
        // Line-comment variant.
        let evil2 = "SELECT 1 --\'\n; ATTACH DATABASE \'/tmp/x.db\' AS o";
        assert!(validate_read_only_sql(evil2).is_err());
    }

    #[test]
    fn strip_sql_comments_respects_strings_and_comments() {
        // Block + line comments removed.
        assert!(!strip_sql_comments("SELECT 1 /* c */ FROM t").contains("/*"));
        assert!(!strip_sql_comments("SELECT 1 -- tail\n").contains("--"));
        // A comment marker INSIDE a string literal is preserved (not a comment).
        let kept = strip_sql_comments("SELECT '/* not a comment */' AS x");
        assert!(kept.contains("/* not a comment */"));
        // A quote INSIDE a comment does not open a string: the real `;` after
        // the comment stays visible to the multi-statement scan.
        let cleaned = strip_sql_comments("SELECT 1 /*\'*/; SELECT 2");
        assert!(has_multiple_statements(cleaned.trim()));
    }

    #[test]
    fn detects_multiple_statements_ignoring_quoted_semicolons() {
        assert!(!has_multiple_statements("SELECT ';' AS x"));
        assert!(has_multiple_statements("SELECT 1 ; SELECT 2"));
        assert!(!has_multiple_statements("SELECT 1 ;"));
    }

    #[test]
    fn leading_keyword_skips_parens_and_space() {
        assert_eq!(leading_keyword("  (SELECT 1)"), "select");
        assert_eq!(leading_keyword("WITH x AS (SELECT 1) SELECT *"), "with");
        assert_eq!(leading_keyword("pragma table_info(t)"), "pragma");
    }

    async fn seed_db(tmp: &tempfile::TempDir) -> std::path::PathBuf {
        let db_path = tmp.path().join("data.sqlite");
        let mut conn = SqliteConnectOptions::new()
            .filename(&db_path)
            .create_if_missing(true)
            .connect()
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE t (id INTEGER, ratio REAL, label TEXT, payload BLOB, maybe TEXT)",
        )
        .execute(&mut conn)
        .await
        .unwrap();
        for i in 1..=5 {
            sqlx::query("INSERT INTO t VALUES (?, ?, ?, ?, ?)")
                .bind(i)
                .bind(i as f64 + 0.5)
                .bind(format!("row-{i}"))
                .bind(vec![0u8, 1, 2, 3])
                .bind(Option::<String>::None)
                .execute(&mut conn)
                .await
                .unwrap();
        }
        conn.close().await.unwrap();
        db_path
    }

    #[tokio::test]
    async fn run_query_decodes_every_column_type() {
        let tmp = tempfile::tempdir().unwrap();
        let db = seed_db(&tmp).await;
        let out = run_query(&db, "SELECT * FROM t ORDER BY id LIMIT 1", 100)
            .await
            .unwrap();
        assert_eq!(out["columns"][0], "id");
        let row = &out["rows"][0];
        assert_eq!(row["id"], 1);
        assert_eq!(row["ratio"], 1.5);
        assert_eq!(row["label"], "row-1");
        assert_eq!(row["payload"], "<blob: 4 bytes>");
        assert!(row["maybe"].is_null());
        assert_eq!(out["truncated"], false);
    }

    #[tokio::test]
    async fn run_query_enforces_row_cap_and_flags_truncation() {
        let tmp = tempfile::tempdir().unwrap();
        let db = seed_db(&tmp).await;
        let out = run_query(&db, "SELECT id FROM t ORDER BY id", 2)
            .await
            .unwrap();
        assert_eq!(out["rowCount"], 2);
        assert_eq!(out["rows"].as_array().unwrap().len(), 2);
        assert_eq!(out["truncated"], true);
        assert!(out["note"].as_str().unwrap().contains("LIMIT/OFFSET"));
    }

    #[tokio::test]
    async fn run_query_connection_rejects_writes_even_without_validation() {
        // Bypass validate_read_only_sql to prove the *connection* itself is
        // read-only: a write must fail at the engine, not just at the gate.
        let tmp = tempfile::tempdir().unwrap();
        let db = seed_db(&tmp).await;
        let err = run_query(&db, "DELETE FROM t", 100).await.unwrap_err();
        assert!(
            err.to_lowercase().contains("readonly")
                || err.to_lowercase().contains("read-only")
                || err.to_lowercase().contains("read only"),
            "expected a read-only engine error, got: {err}"
        );
        // And the data is intact.
        let out = run_query(&db, "SELECT count(*) AS n FROM t", 100)
            .await
            .unwrap();
        assert_eq!(out["rows"][0]["n"], 5);
    }

    #[tokio::test]
    async fn run_query_reads_a_wal_database() {
        // Production data.sqlite is always WAL (see db::init_workspace_db).
        // Opening a WAL DB read-only has sharp edges, so exercise it directly.
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("data.sqlite");
        let mut conn = SqliteConnectOptions::new()
            .filename(&db_path)
            .create_if_missing(true)
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
            .connect()
            .await
            .unwrap();
        sqlx::query("CREATE TABLE t (id INTEGER, label TEXT)")
            .execute(&mut conn)
            .await
            .unwrap();
        sqlx::query("INSERT INTO t VALUES (7, 'seven')")
            .execute(&mut conn)
            .await
            .unwrap();
        // Keep the writer connection OPEN so the read-only handle must read a
        // live WAL database with an active -wal/-shm, as in production.
        let out = run_query(&db_path, "SELECT label FROM t WHERE id = 7", 100)
            .await
            .unwrap();
        assert_eq!(out["rows"][0]["label"], "seven");
        conn.close().await.unwrap();
    }

    #[tokio::test]
    async fn run_query_attach_is_blocked_at_the_engine() {
        // Bypass validate_read_only_sql entirely (call run_query directly) to
        // prove the ENGINE refuses ATTACH, not just the textual gate. Create a
        // separate "secret" database, then try to attach and read it.
        let tmp = tempfile::tempdir().unwrap();
        let main_db = seed_db(&tmp).await;

        let secret_path = tmp.path().join("secret.sqlite");
        let mut sconn = SqliteConnectOptions::new()
            .filename(&secret_path)
            .create_if_missing(true)
            .connect()
            .await
            .unwrap();
        sqlx::query("CREATE TABLE secrets (api_key TEXT)")
            .execute(&mut sconn)
            .await
            .unwrap();
        sqlx::query("INSERT INTO secrets VALUES ('SECRET-TOKEN-XYZ')")
            .execute(&mut sconn)
            .await
            .unwrap();
        sconn.close().await.unwrap();

        let attach = format!("ATTACH DATABASE '{}' AS o", secret_path.display());
        let err = run_query(&main_db, &attach, 100).await.unwrap_err();
        assert!(
            err.to_lowercase().contains("attach") || err.to_lowercase().contains("too many"),
            "expected ATTACH to be refused at the engine, got: {err}"
        );
    }

    #[tokio::test]
    async fn run_query_schema_introspection_works() {
        let tmp = tempfile::tempdir().unwrap();
        let db = seed_db(&tmp).await;
        let out = run_query(
            &db,
            "SELECT name FROM sqlite_master WHERE type='table'",
            100,
        )
        .await
        .unwrap();
        assert_eq!(out["rows"][0]["name"], "t");
    }

    #[test]
    fn clip_cell_caps_long_values() {
        let long = "x".repeat(MAX_CELL_CHARS + 50);
        let clipped = clip_cell(&long);
        assert!(clipped.contains("cell truncated"));
        assert!(clipped.chars().count() < long.chars().count() + 64);
        // Short values pass through untouched.
        assert_eq!(clip_cell("short"), "short");
    }
}
