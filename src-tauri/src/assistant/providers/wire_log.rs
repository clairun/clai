//! Opt-in wire log of every outbound provider HTTP exchange, for diagnosing
//! provider-specific failures (e.g. GLM/MiniMax rejecting or truncating a
//! request). One append-only JSONL file per launch, shared by all workspaces
//! in that launch.
//!
//! Enabled only when `CLAI_PROVIDER_WIRE_LOG` is set at app start: `1`/`true`
//! logs to `<host temp dir>/clai-provider-wire-<pid>.log` (0600, unique per
//! launch so concurrent instances never interleave), any other non-empty
//! value is taken as an explicit file path. Unset = feature off,
//! zero cost beyond one mutex peek per request. Provider HTTP runs solely in
//! the host app process — never inside a sandboxed child — so the host
//! process temp dir is the real host `/tmp`, not a sandbox view.
//!
//! Every line is one JSON event. A `request` line carries the verbatim body
//! plus a process-unique request id; later `response`, `body`, `chunk`,
//! `end`, `stream_error`, `transport_error`, and `cancelled` lines reference
//! that id, which keeps concurrent requests separable when interleaved.
//! Stream chunks are logged losslessly: as exact UTF-8 `text` when the chunk
//! is valid UTF-8 (the SSE common case), else as base64 `b64` — a chunk split
//! mid-multibyte reassembles exactly from the base64 parts. Credentials never
//! reach the file: sensitive header *values* and credential query parameters
//! are replaced with `[REDACTED]` before writing. Bodies are recorded
//! intentionally unredacted — that is the feature.
//!
//! Any logging failure is warned and swallowed: the wire log must never break
//! the request it observes. Supersedes the unmerged `CLAI_LLM_TRACE_DIR`
//! experiment, which could not capture streams.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use futures::stream::Stream;
use serde_json::{json, Map, Value};

type Bytes = axum::body::Bytes;

const ENV_WIRE_LOG: &str = "CLAI_PROVIDER_WIRE_LOG";
const DEFAULT_FILE_PREFIX: &str = "clai-provider-wire";

/// Header names whose values must never reach the wire log.
const SENSITIVE_HEADERS: &[&str] = &[
    "authorization",
    "proxy-authorization",
    "x-api-key",
    "api-key",
    "x-goog-api-key",
    "x-amz-security-token",
    "cookie",
    "set-cookie",
];

/// Query parameter names whose values must never reach the wire log.
const SENSITIVE_QUERY_KEYS: &[&str] = &[
    "key",
    "api_key",
    "api-key",
    "apikey",
    "token",
    "access_token",
    "access-token",
    "secret",
    "signature",
    "auth",
];

static LOGGER: Mutex<Option<Arc<Logger>>> = Mutex::new(None);
static REQUEST_SEQ: AtomicU64 = AtomicU64::new(0);

/// Serialize wire-log tests that swap the global logger.
#[cfg(test)]
pub(crate) static TEST_LOCK: Mutex<()> = Mutex::new(());

/// Resolve the env var and open the log file. Called once at app startup;
/// failures disable the feature with a warning, never abort startup.
pub(crate) fn init() {
    let Some(path) = resolve_from_env() else {
        return;
    };
    match Logger::open(&path) {
        Ok(logger) => {
            let logger = Arc::new(logger);
            let mut guard = LOGGER.lock().expect("wire log logger poisoned");
            *guard = Some(logger.clone());
            drop(guard);
            logger.emit_startup();
            tracing::info!(path = %path.display(), "Provider wire log enabled");
        }
        Err(error) => tracing::warn!(
            path = %path.display(),
            error = %error,
            "CLAI_PROVIDER_WIRE_LOG is set but the wire log file could not be opened; continuing without provider wire logging"
        ),
    }
}

fn resolve_from_env() -> Option<PathBuf> {
    let raw = std::env::var(ENV_WIRE_LOG).ok()?;
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    if raw == "1" || raw.eq_ignore_ascii_case("true") || raw.eq_ignore_ascii_case("on") {
        // Pid-suffixed so each launch gets its own file: concurrent app
        // instances never interleave, and a fresh launch cannot silently
        // grow a previous session's log.
        Some(std::env::temp_dir().join(format!(
            "{}-{}.log",
            DEFAULT_FILE_PREFIX,
            std::process::id()
        )))
    } else {
        Some(PathBuf::from(raw))
    }
}

fn current_logger() -> Option<Arc<Logger>> {
    LOGGER.lock().expect("wire log logger poisoned").clone()
}

struct Logger {
    path: PathBuf,
    file: Mutex<std::fs::File>,
}

impl Logger {
    fn open(path: &Path) -> std::io::Result<Self> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        #[cfg(unix)]
        let file = {
            use std::os::unix::fs::OpenOptionsExt;
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .mode(0o600)
                .open(path)?
        };
        #[cfg(not(unix))]
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        // Defensive: a pre-existing file may carry looser permissions.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
        }
        Ok(Self {
            path: path.to_path_buf(),
            file: Mutex::new(file),
        })
    }

    fn emit_startup(&self) {
        self.emit(json!({
            "event": "startup",
            "ts": now_iso(),
            "pid": std::process::id(),
            "version": env!("CARGO_PKG_VERSION"),
            "path": self.path.display().to_string(),
        }));
    }

    /// Append one JSON line. Never fails the caller: errors are warned once
    /// per event (disk-full and friends are rare; silent failure is worse).
    fn emit(&self, event: Value) {
        // The logger-level mutex serializes concurrent writers; failures stay
        // local to this event.
        let result = {
            let mut guard = self.file.lock().expect("wire log file poisoned");
            let mut line = match serde_json::to_vec(&event) {
                Ok(bytes) => bytes,
                Err(error) => {
                    tracing::warn!(error = %error, "failed to serialize provider wire log event");
                    return;
                }
            };
            line.push(b'\n');
            guard.write_all(&line)
        };
        if let Err(error) = result {
            tracing::warn!(error = %error, "failed to write provider wire log event");
        }
    }
}

fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn is_sensitive_header(name: &str) -> bool {
    SENSITIVE_HEADERS
        .iter()
        .any(|sensitive| name.eq_ignore_ascii_case(sensitive))
}

fn is_sensitive_query_key(key: &str) -> bool {
    SENSITIVE_QUERY_KEYS
        .iter()
        .any(|sensitive| key.eq_ignore_ascii_case(sensitive))
}

/// Replace credential query parameter values, keeping the URL structure.
fn redact_url(url: &str) -> String {
    let Some((base, query)) = url.split_once('?') else {
        return url.to_string();
    };
    let redacted = query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| match pair.split_once('=') {
            Some((key, _value)) if is_sensitive_query_key(key) => {
                format!("{key}=[REDACTED]")
            }
            _ => pair.to_string(),
        })
        .collect::<Vec<_>>()
        .join("&");
    if redacted.is_empty() {
        base.to_string()
    } else {
        format!("{base}?{redacted}")
    }
}

fn redacted_headers(headers: &[(String, String)]) -> Map<String, Value> {
    headers
        .iter()
        .map(|(name, value)| {
            let logged = if is_sensitive_header(name) {
                "[REDACTED]".to_string()
            } else {
                value.clone()
            };
            (name.clone(), Value::String(logged))
        })
        .collect()
}

/// One outbound provider HTTP request, as sent. `headers` mirror exactly what
/// goes on the wire (sensitive values are redacted when logged).
pub(crate) struct Request<'a> {
    /// "completion" | "models" | "key_validation"
    pub kind: &'static str,
    pub provider_id: &'a str,
    pub protocol_id: &'a str,
    pub model_id: Option<&'a str>,
    pub run_id: Option<&'a str>,
    pub session_id: Option<&'a str>,
    pub method: &'static str,
    pub url: &'a str,
    pub headers: &'a [(String, String)],
    pub body: &'a Value,
}

/// Per-request logging handle. Inert (`None`) when the wire log is disabled;
/// every method is a no-op then.
#[derive(Clone, Default)]
pub(crate) struct Scope(Option<Arc<ScopeState>>);

struct ScopeState {
    id: u64,
    logger: Arc<Logger>,
}

impl Scope {
    /// Emit the `request` event and open a scope for everything that follows.
    pub(crate) fn begin(request: &Request<'_>) -> Self {
        let Some(logger) = current_logger() else {
            return Self(None);
        };
        let id = REQUEST_SEQ.fetch_add(1, Ordering::Relaxed) + 1;
        logger.emit(json!({
            "event": "request",
            "ts": now_iso(),
            "pid": std::process::id(),
            "id": format!("wr-{id}"),
            "kind": request.kind,
            "provider_id": request.provider_id,
            "protocol_id": request.protocol_id,
            "model_id": request.model_id,
            "run_id": request.run_id,
            "session_id": request.session_id,
            "method": request.method,
            "url": redact_url(request.url),
            "headers": redacted_headers(request.headers),
            "body": request.body,
        }));
        Self(Some(Arc::new(ScopeState { id, logger })))
    }

    fn emit(&self, event: Value) {
        let Some(state) = &self.0 else {
            return;
        };
        stamp(state, event);
    }

    /// The send itself failed (connection error, timeout, …): nothing came back.
    pub(crate) fn transport_error(&self, error: &str) {
        self.emit(json!({ "event": "transport_error", "error": error }));
    }

    /// Response headers arrived. `status` is the HTTP status code.
    pub(crate) fn response(&self, status: u16, headers: &reqwest::header::HeaderMap) {
        let logged: Map<String, Value> = headers
            .iter()
            .map(|(name, value)| {
                let logged = if is_sensitive_header(name.as_str()) {
                    "[REDACTED]".to_string()
                } else {
                    value
                        .to_str()
                        .map(|v| v.to_string())
                        .unwrap_or_else(|_| format!("<{} bytes>", value.len()))
                };
                (name.as_str().to_string(), Value::String(logged))
            })
            .collect();
        self.emit(json!({ "event": "response", "status": status, "headers": logged }));
    }

    /// Full non-stream response body (also used for non-2xx error bodies).
    pub(crate) fn body(&self, text: &str) {
        self.emit(json!({ "event": "body", "bytes": text.len(), "text": text }));
    }

    /// Tee a response byte stream through the wire log: every chunk, the
    /// clean end, stream errors, and early drop (cancellation) are recorded,
    /// while items pass through unchanged.
    pub(crate) fn wrap_stream(
        &self,
        stream: impl Stream<Item = reqwest::Result<Bytes>> + Send + 'static,
    ) -> WireStream {
        let state = self.0.clone().map(|scope| StreamState {
            scope,
            chunks: 0,
            bytes: 0,
            done: false,
        });
        WireStream {
            inner: Box::pin(stream),
            state,
        }
    }
}

/// Pass-through stream wrapper that records every raw chunk. Dropping it
/// before the stream ends logs `cancelled` — that is how aborted runs show up.
pub(crate) struct WireStream {
    inner: Pin<Box<dyn Stream<Item = reqwest::Result<Bytes>> + Send>>,
    state: Option<StreamState>,
}

struct StreamState {
    scope: Arc<ScopeState>,
    chunks: u64,
    bytes: u64,
    done: bool,
}

impl Stream for WireStream {
    type Item = reqwest::Result<Bytes>;

    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        let poll = self.inner.as_mut().poll_next(cx);
        if let Some(state) = &mut self.state {
            match &poll {
                std::task::Poll::Ready(Some(Ok(chunk))) => {
                    state.chunks += 1;
                    state.bytes += chunk.len() as u64;
                    // Lossless either way: exact text when valid, base64 when
                    // a multibyte character is split across chunks.
                    let payload = match std::str::from_utf8(chunk.as_ref()) {
                        Ok(text) => json!({ "text": text }),
                        Err(_) => {
                            use base64::Engine as _;
                            json!({ "b64": base64::engine::general_purpose::STANDARD.encode(chunk.as_ref()) })
                        }
                    };
                    let event = json!({
                        "event": "chunk",
                        "n": state.chunks,
                        "bytes": chunk.len(),
                        "payload": payload,
                    });
                    stamp(&state.scope, event);
                }
                std::task::Poll::Ready(Some(Err(error))) => {
                    state.done = true;
                    stamp(
                        &state.scope,
                        json!({ "event": "stream_error", "error": error.to_string() }),
                    );
                }
                std::task::Poll::Ready(None) => {
                    state.done = true;
                    stamp(
                        &state.scope,
                        json!({ "event": "end", "chunks": state.chunks, "bytes": state.bytes }),
                    );
                }
                std::task::Poll::Pending => {}
            }
        }
        poll
    }
}

impl Drop for WireStream {
    fn drop(&mut self) {
        if let Some(state) = &mut self.state {
            if !state.done {
                state.done = true;
                stamp(
                    &state.scope,
                    json!({ "event": "cancelled", "chunks": state.chunks, "bytes": state.bytes }),
                );
            }
        }
    }
}

/// Add the correlation stamp (`ts`, `id`) to an event and append it.
fn stamp(scope: &ScopeState, mut event: Value) {
    let object = event.as_object_mut().expect("events are JSON objects");
    object.insert("ts".into(), Value::String(now_iso()));
    object.insert("id".into(), Value::String(format!("wr-{}", scope.id)));
    scope.logger.emit(event);
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;

    /// Point the global logger at `path` for the duration of one test.
    /// Hold [`TEST_LOCK`] around the whole test: the logger is process-global.
    pub(crate) fn install(path: &Path) {
        let logger = Arc::new(Logger::open(path).expect("open wire log for test"));
        *LOGGER.lock().expect("wire log logger poisoned") = Some(logger);
    }

    pub(crate) fn disable() {
        *LOGGER.lock().expect("wire log logger poisoned") = None;
    }
}

#[cfg(test)]
mod tests {
    use super::testing;
    use super::*;
    use futures::StreamExt;
    use std::io::BufRead;

    fn guard() -> std::sync::MutexGuard<'static, ()> {
        TEST_LOCK.lock().expect("test lock poisoned")
    }

    fn scope_for(url: &str, headers: &[(String, String)]) -> Scope {
        let body = json!({ "model": "glm-4.7", "messages": [
            { "role": "user", "content": "pruéba € full prompt" }
        ]});
        Scope::begin(&Request {
            kind: "completion",
            provider_id: "z-ai",
            protocol_id: "openai",
            model_id: Some("glm-4.7"),
            run_id: Some("run-1"),
            session_id: Some("sess-1"),
            method: "POST",
            url,
            headers,
            body: &body,
        })
    }

    /// Parse the JSONL file into (event, id) pairs, asserting every line is
    /// valid JSON.
    fn read_events(path: &Path) -> Vec<Value> {
        let file = std::fs::File::open(path).expect("wire log exists");
        std::io::BufReader::new(file)
            .lines()
            .map(|line| serde_json::from_str(&line.expect("readable line")).expect("valid JSONL"))
            .collect()
    }

    fn events_of<'a>(events: &'a [Value], name: &str) -> Vec<&'a Value> {
        events.iter().filter(|e| e["event"] == name).collect()
    }

    /// Guard held across awaits on purpose: swaps the process-global logger.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn disabled_by_default_logs_nothing() {
        let _guard = guard();
        testing::disable();
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("wire.log");
        // Env not touched: Scope must be inert with no logger installed.
        let scope = scope_for("https://api.example/v1/chat/completions", &[]);
        scope.transport_error("boom");
        scope.response(500, &reqwest::header::HeaderMap::new());
        let stream = scope.wrap_stream(futures::stream::iter(vec![Ok(Bytes::from("data: x\n\n"))]));
        stream.count().await; // drive to end
        assert!(!path.exists());
    }

    #[test]
    fn request_body_verbatim_and_credentials_redacted() {
        let _guard = guard();
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("wire.log");
        testing::install(&path);

        let headers = vec![
            ("Content-Type".to_string(), "application/json".to_string()),
            (
                "Authorization".to_string(),
                "Bearer sk-secret-123".to_string(),
            ),
            ("X-API-Key".to_string(), "anthropic-secret".to_string()),
            ("x-custom".to_string(), "keep-me".to_string()),
        ];
        let url = "https://api.example/v1/chat/completions?api_key=abc123&limit=5";
        let scope = scope_for(url, &headers);
        drop(scope);

        let events = read_events(&path);
        let request = events_of(&events, "request")[0];
        assert_eq!(
            request["url"],
            "https://api.example/v1/chat/completions?api_key=[REDACTED]&limit=5"
        );
        assert_eq!(request["headers"]["Authorization"], "[REDACTED]");
        assert_eq!(request["headers"]["X-API-Key"], "[REDACTED]");
        assert_eq!(request["headers"]["x-custom"], "keep-me");
        assert_eq!(request["headers"]["Content-Type"], "application/json");
        // Full prompt preserved verbatim, unicode included.
        assert_eq!(
            request["body"]["messages"][0]["content"],
            "pruéba € full prompt"
        );
        assert_eq!(request["model_id"], "glm-4.7");
        assert_eq!(request["run_id"], "run-1");
        assert_eq!(request["session_id"], "sess-1");
        assert_eq!(request["kind"], "completion");
        assert!(request["id"].as_str().expect("id").starts_with("wr-"));
    }

    /// Guard held across awaits on purpose: swaps the process-global logger.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn stream_records_chunks_end_and_cancellation() {
        let _guard = guard();
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("wire.log");
        testing::install(&path);

        // Completed stream: chunks + end.
        let scope = scope_for("https://api.example/v1/chat/completions", &[]);
        // é (U+00E9) split across chunks: first ends mid-sequence, second
        // completes it — the exact case where exact-UTF-8 logging is wrong.
        let chunks = vec![
            Ok(Bytes::from_static(b"data: {\"delta\":\"h\xc3")),
            Ok(Bytes::from_static(b"\xa9i\"}\n\n")),
        ];
        let stream = scope.wrap_stream(futures::stream::iter(chunks));
        let collected = stream.map(|item| item.expect("chunk ok")).count().await;
        assert_eq!(collected, 2);
        drop(scope);

        let events = read_events(&path);
        let chunk_events = events_of(&events, "chunk");
        assert_eq!(chunk_events.len(), 2);
        // Both sides of a chunk boundary that splits a multibyte character are
        // invalid UTF-8 (the second starts with a continuation byte), so both
        // are base64 — and reassemble to the exact original bytes.
        use base64::Engine as _;
        let reassembled: Vec<u8> = [0, 1]
            .iter()
            .flat_map(|&i| {
                let b64 = chunk_events[i]["payload"]["b64"]
                    .as_str()
                    .expect("b64 payload for invalid-UTF-8 chunk");
                assert!(chunk_events[i]["payload"].get("text").is_none());
                base64::engine::general_purpose::STANDARD
                    .decode(b64)
                    .expect("valid b64")
            })
            .collect();
        assert_eq!(reassembled, b"data: {\"delta\":\"h\xc3\xa9i\"}\n\n");
        assert_eq!(chunk_events[1]["n"], 2);
        let end = events_of(&events, "end")[0];
        assert_eq!(end["chunks"], 2);
        assert_eq!(end["bytes"], 18 + 6);
        assert!(events_of(&events, "cancelled").is_empty());

        // Dropped stream: cancelled instead of end.
        let scope2 = scope_for("https://api.example/v1/chat/completions", &[]);
        let pending = scope2.wrap_stream(futures::stream::pending());
        drop(pending); // dropped before any item
        let events = read_events(&path);
        let cancelled = events_of(&events, "cancelled")[0];
        assert_eq!(cancelled["chunks"], 0);
    }

    /// Guard held across awaits on purpose: swaps the process-global logger.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn stream_and_transport_errors_are_recorded() {
        let _guard = guard();
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("wire.log");
        testing::install(&path);

        let scope = scope_for("https://api.example/v1/chat/completions", &[]);
        scope.transport_error("connection reset by peer");

        // A real transport error: nothing is listening on this port.
        let error = reqwest::get("http://127.0.0.1:1").await.unwrap_err();
        let stream = scope.wrap_stream(futures::stream::iter(vec![Err(error)]));
        stream.count().await; // drive to error
        drop(scope);

        let events = read_events(&path);
        assert!(!events_of(&events, "transport_error").is_empty());
        let stream_error = events_of(&events, "stream_error")[0];
        assert!(!stream_error["error"].as_str().expect("msg").is_empty());
        // An errored stream is done: no cancelled on drop.
        assert!(events_of(&events, "cancelled").is_empty());
    }

    #[test]
    fn response_and_body_events_recorded() {
        let _guard = guard();
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("wire.log");
        testing::install(&path);

        let scope = scope_for("https://api.example/v1/models", &[]);
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("content-type", "application/json".parse().unwrap());
        headers.insert("set-cookie", "session=secret".parse().unwrap());
        scope.response(401, &headers);
        scope.body("{\"error\":{\"message\":\"invalid api key\"}}");
        drop(scope);

        let events = read_events(&path);
        let response = events_of(&events, "response")[0];
        assert_eq!(response["status"], 401);
        assert_eq!(response["headers"]["content-type"], "application/json");
        assert_eq!(response["headers"]["set-cookie"], "[REDACTED]");
        let body = events_of(&events, "body")[0];
        assert_eq!(
            body["text"],
            "{\"error\":{\"message\":\"invalid api key\"}}"
        );
    }

    /// Guard held across awaits on purpose: swaps the process-global logger.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn concurrent_scopes_keep_distinct_ids() {
        let _guard = guard();
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("wire.log");
        testing::install(&path);

        let a = scope_for("https://api.example/v1/chat/completions", &[]);
        let b = scope_for("https://api.example/v1/chat/completions", &[]);
        let id_a = format!("wr-{}", REQUEST_SEQ.load(Ordering::Relaxed) - 1);
        let id_b = format!("wr-{}", REQUEST_SEQ.load(Ordering::Relaxed));
        let sa = a.wrap_stream(futures::stream::iter(vec![Ok(Bytes::from_static(b"one"))]));
        let sb = b.wrap_stream(futures::stream::iter(vec![Ok(Bytes::from_static(b"two"))]));
        sa.chain(sb).count().await;
        drop((a, b));

        let events = read_events(&path);
        let ids: Vec<&str> = events
            .iter()
            .map(|e| e["id"].as_str().expect("id"))
            .collect();
        assert!(ids.contains(&id_a.as_str()));
        assert!(ids.contains(&id_b.as_str()));
        assert_ne!(id_a, id_b);
        // Chunk lines reference the request that produced them.
        assert!(events
            .iter()
            .any(|e| e["event"] == "chunk" && e["id"] == id_a && e["payload"]["text"] == "one"));
        assert!(events
            .iter()
            .any(|e| e["event"] == "chunk" && e["id"] == id_b && e["payload"]["text"] == "two"));
    }

    #[test]
    fn default_path_is_unique_per_launch_and_explicit_path_wins() {
        let _guard = guard();
        // `resolve_from_env` reads the process env, which is global: the
        // guard serializes this against other env-mutating tests (none yet).
        let key = "CLAI_PROVIDER_WIRE_LOG";
        let previous = std::env::var(key).ok();
        std::env::remove_var(key);
        assert_eq!(resolve_from_env(), None);
        std::env::set_var(key, "  ");
        assert_eq!(resolve_from_env(), None, "blank disables");
        std::env::set_var(key, "1");
        assert_eq!(
            resolve_from_env(),
            Some(
                std::env::temp_dir().join(format!("clai-provider-wire-{}.log", std::process::id()))
            )
        );
        std::env::set_var(key, "TRUE");
        assert!(resolve_from_env().is_some());
        std::env::set_var(key, "/tmp/my-wire.log");
        assert_eq!(resolve_from_env(), Some(PathBuf::from("/tmp/my-wire.log")));
        match previous {
            Some(value) => std::env::set_var(key, value),
            None => std::env::remove_var(key),
        }
    }

    #[cfg(unix)]
    #[test]
    fn log_file_is_created_with_owner_only_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let _guard = guard();
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("wire.log");
        testing::install(&path);
        let scope = scope_for("https://api.example/v1/models", &[]);
        drop(scope);

        let mode = std::fs::metadata(&path)
            .expect("file exists")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}
