//! `create_vega_chart` — the one sanctioned way for an agent to produce a
//! chart.
//!
//! The model hands over a Vega-Lite spec (or names an existing `.vl.json`
//! file); the tool validates it against the Vega-Lite JSON schema and, when
//! valid, stores it in the workspace as a `.vl.json` artifact the frontend
//! renders as an interactive chart (`VegaChart`: chat, `.md` reports via
//! `![title](charts/x.vl.json)`, and the artifacts panel).
//!
//! Why a tool instead of `fs_write`: a spec written blind renders as an
//! error card the model never sees. Validating here turns that into a tool
//! error listing the offending JSON paths, so the model repairs the spec in
//! the same turn. The schema is the compact copy of
//! `vega-lite/build/vega-lite-schema.json` vendored at
//! `embedded/vega-lite-schema.json` (a vitest keeps it in sync with the npm
//! package the renderer uses) and is compiled once per process.

use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;

use jsonschema::error::ValidationErrorKind;
use jsonschema::{ValidationError, Validator};
use serde::Deserialize;

use super::ToolExecutionContext;

/// Directory (relative to the workspace root) inline specs are saved to.
pub const CHARTS_DIR: &str = "charts";
/// `$schema` written into a saved spec that has none; also the version the
/// renderer (`vega-lite` npm package) and the vendored schema implement.
const VEGA_LITE_SCHEMA_URL: &str = "https://vega.github.io/schema/vega-lite/v6.json";
const SPEC_EXTENSION: &str = ".vl.json";
/// Ceiling on an inline spec. Data belongs in a workspace file referenced
/// by `data.url`; a spec this large is almost always inlined rows.
const MAX_SPEC_BYTES: usize = 1_000_000;
/// Above this many inline `data.values` rows the result carries a warning
/// steering the model to `data.url`. Not an error: small tables are fine.
const INLINE_ROWS_WARNING_THRESHOLD: usize = 200;
/// How many schema violations are reported. The deepest ones are kept; the
/// rest is noise from the other branches of the top-level `anyOf`.
const MAX_REPORTED_VIOLATIONS: usize = 8;
const MAX_SLUG_CHARS: usize = 60;

static VEGA_LITE_SCHEMA_JSON: &str = include_str!("../../../embedded/vega-lite-schema.json");

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateVegaChartParams {
    pub title: String,
    /// The spec as a JSON object, or as a string holding JSON (models
    /// sometimes stringify nested objects).
    #[serde(default)]
    pub spec: Option<serde_json::Value>,
    /// Workspace-relative `.vl.json` path: the destination when `spec` is
    /// given, the file to validate otherwise.
    #[serde(default)]
    pub path: Option<String>,
    /// Show the chart inline in the chat as this call's result card
    /// (default true). `false` for charts that only belong in a report.
    #[serde(default = "default_display")]
    pub display: bool,
}

fn default_display() -> bool {
    true
}

pub async fn execute(
    context: &ToolExecutionContext,
    params: CreateVegaChartParams,
) -> Result<serde_json::Value, String> {
    let workspace_root = context.workspace_root.clone().ok_or_else(|| {
        "create_vega_chart is unavailable because this session is not tied to an automation workspace"
            .to_string()
    })?;
    let title = params.title.trim();
    let display = params.display;
    if title.is_empty() {
        return Err("`title` must not be empty".to_string());
    }

    let outcome = match (params.spec, params.path) {
        (Some(spec), path) => {
            let relative = match path {
                Some(path) => spec_relative_path(&path)?,
                None => Path::new(CHARTS_DIR).join(format!("{}{}", slugify(title), SPEC_EXTENSION)),
            };
            let mut spec = parse_spec_value(spec)?;
            validate_spec(&spec)?;
            let warnings = spec_warnings(&spec);
            if let Some(object) = spec.as_object_mut() {
                object
                    .entry("$schema")
                    .or_insert_with(|| serde_json::Value::String(VEGA_LITE_SCHEMA_URL.to_string()));
            }
            write_spec(&workspace_root, &relative, &spec)?;
            (relative, warnings, "written")
        }
        (None, Some(path)) => {
            let relative = spec_relative_path(&path)?;
            let absolute = workspace_root.join(&relative);
            let text = read_spec_file(&absolute, &relative)?;
            let spec = parse_spec_text(&text)?;
            validate_spec(&spec)?;
            (relative, spec_warnings(&spec), "validated")
        }
        (None, None) => {
            return Err(
                "Provide `spec` (a Vega-Lite spec to validate and save) or `path` (an existing .vl.json file to validate)"
                    .to_string(),
            )
        }
    };

    let (relative, warnings, action) = outcome;
    let path_string = relative_path_string(&relative);
    let mut result = serde_json::json!({
        "ok": true,
        "action": action,
        "path": path_string,
        "title": title,
        "display": display,
        "markdown": format!("![{}]({})", title.replace(']', "\\]"), path_string),
    });
    if !warnings.is_empty() {
        result["warnings"] = serde_json::Value::Array(
            warnings
                .into_iter()
                .map(serde_json::Value::String)
                .collect(),
        );
    }
    Ok(result)
}

/// Accept the spec as an object or as a string containing JSON.
fn parse_spec_value(spec: serde_json::Value) -> Result<serde_json::Value, String> {
    match spec {
        serde_json::Value::String(text) => parse_spec_text(&text),
        serde_json::Value::Object(_) => Ok(spec),
        other => Err(format!(
            "`spec` must be a JSON object (or a string containing one), got {}",
            json_type_name(&other)
        )),
    }
}

fn parse_spec_text(text: &str) -> Result<serde_json::Value, String> {
    let value: serde_json::Value =
        serde_json::from_str(text).map_err(|e| format!("`spec` is not valid JSON: {e}"))?;
    if !value.is_object() {
        return Err(format!(
            "A Vega-Lite spec must be a JSON object, got {}",
            json_type_name(&value)
        ));
    }
    Ok(value)
}

fn json_type_name(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "a boolean",
        serde_json::Value::Number(_) => "a number",
        serde_json::Value::String(_) => "a string",
        serde_json::Value::Array(_) => "an array",
        serde_json::Value::Object(_) => "an object",
    }
}

/// The compiled Vega-Lite schema, built on first use. A compile failure is
/// a programming error (the vendored file is corrupt); it is reported on
/// every call rather than panicking the app.
fn schema_validator() -> Result<&'static Validator, String> {
    static VALIDATOR: OnceLock<Result<Validator, String>> = OnceLock::new();
    VALIDATOR
        .get_or_init(|| {
            let mut schema: serde_json::Value = serde_json::from_str(VEGA_LITE_SCHEMA_JSON)
                .map_err(|e| format!("embedded Vega-Lite schema is not valid JSON: {e}"))?;
            percent_encode_refs(&mut schema);
            jsonschema::validator_for(&schema)
                .map_err(|e| format!("embedded Vega-Lite schema failed to compile: {e}"))
        })
        .as_ref()
        .map_err(|e| format!("Chart validation is unavailable: {e}"))
}

/// Vega-Lite names generic definitions literally — `"$ref":
/// "#/definitions/MarkPropDef<(Gradient|string|null)>"` — and `<`, `>`,
/// `|`, `"`, `[`, `]` are not legal in a URI reference, so `jsonschema`
/// (which parses every `$ref` as one) refuses to compile the schema as
/// shipped. Percent-encoding the local fragments fixes that without
/// touching the vendored file (kept identical to the npm copy so the sync
/// test stays a plain comparison): the resolver percent-decodes a JSON
/// pointer before walking it, so the encoded refs land on the same
/// definitions. Only the schema's own references are rewritten; Vega-Lite
/// has none pointing outside the document.
fn percent_encode_refs(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, child) in map.iter_mut() {
                if key == "$ref" {
                    if let serde_json::Value::String(reference) = child {
                        if let Some(fragment) = reference.strip_prefix('#') {
                            *reference = format!("#{}", percent_encode_fragment(fragment));
                        }
                    }
                } else {
                    percent_encode_refs(child);
                }
            }
        }
        serde_json::Value::Array(items) => items.iter_mut().for_each(percent_encode_refs),
        _ => {}
    }
}

/// Percent-encode everything a URI fragment may not contain verbatim
/// (RFC 3986: fragment = pchar / "/" / "?"). `%` is encoded too: the schema
/// carries no pre-encoded escapes, so a literal `%` must not be mistaken for one.
fn percent_encode_fragment(fragment: &str) -> String {
    let mut out = String::with_capacity(fragment.len());
    for byte in fragment.bytes() {
        let keep = byte.is_ascii_alphanumeric() || b"-._~!$&'()*+,;=:@/?".contains(&byte);
        if keep {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// Validate against the Vega-Lite schema. On failure the error lists the
/// most specific violations with their JSON paths so the model can fix the
/// spec and retry.
pub fn validate_spec(spec: &serde_json::Value) -> Result<(), String> {
    let validator = schema_validator()?;
    if spec
        .get("$schema")
        .is_some_and(|s| s.is_string() && !spec_schema_is_supported(spec))
    {
        return Err(format!(
            "Unsupported `$schema` {}: write a Vega-Lite spec for {} (or omit `$schema`)",
            spec["$schema"], VEGA_LITE_SCHEMA_URL
        ));
    }
    let errors: Vec<ValidationError<'_>> = validator.iter_errors(spec).collect();
    if errors.is_empty() {
        return Ok(());
    }
    Err(format!(
        "The spec is not a valid Vega-Lite ({}) chart:\n{}\nFix the spec and call create_vega_chart again.",
        VEGA_LITE_SCHEMA_URL,
        describe_violations(&errors).join("\n")
    ))
}

/// A spec declaring a Vega-Lite `$schema` must target the version we
/// render and validate against (v5 specs render fine under v6, so both are
/// accepted). Other URLs (plain Vega, unknown) fail with a clear message
/// instead of a wall of `anyOf` violations.
fn spec_schema_is_supported(spec: &serde_json::Value) -> bool {
    match spec.get("$schema").and_then(|s| s.as_str()) {
        None => true,
        Some(url) => {
            url.starts_with("https://vega.github.io/schema/vega-lite/v6")
                || url.starts_with("https://vega.github.io/schema/vega-lite/v5")
        }
    }
}

/// Turn the validator's error tree into a short list of human-readable
/// lines. The top level of the Vega-Lite schema is an `anyOf` over the spec
/// kinds (unit, layer, facet, concat, …), so a typo in `encoding.x.type`
/// surfaces as "not valid under any of the schemas" at the root, with the
/// real cause buried in the branch contexts. The deepest violations (by
/// JSON-pointer depth) are the ones that name the actual mistake; ties are
/// deduplicated and capped.
fn describe_violations(errors: &[ValidationError<'_>]) -> Vec<String> {
    let mut leaves: Vec<(usize, String)> = Vec::new();
    for error in errors {
        collect_leaf_violations(error, &mut leaves);
    }
    let deepest = leaves.iter().map(|(depth, _)| *depth).max().unwrap_or(0);
    let mut lines: Vec<String> = Vec::new();
    for (depth, line) in leaves {
        if depth == deepest && !lines.contains(&line) {
            lines.push(line);
        }
    }
    let total = lines.len();
    lines.truncate(MAX_REPORTED_VIOLATIONS);
    if total > MAX_REPORTED_VIOLATIONS {
        lines.push(format!("- … and {} more", total - MAX_REPORTED_VIOLATIONS));
    }
    lines
}

fn collect_leaf_violations(error: &ValidationError<'_>, out: &mut Vec<(usize, String)>) {
    match &error.kind {
        ValidationErrorKind::AnyOf { context } | ValidationErrorKind::OneOfNotValid { context } => {
            let before = out.len();
            for branch in context {
                for nested in branch {
                    collect_leaf_violations(nested, out);
                }
            }
            if out.len() == before {
                out.push(describe_leaf(error));
            }
        }
        _ => out.push(describe_leaf(error)),
    }
}

fn describe_leaf(error: &ValidationError<'_>) -> (usize, String) {
    let pointer = error.instance_path.to_string();
    let depth = pointer.matches('/').count();
    let location = if pointer.is_empty() {
        "(root)".to_string()
    } else {
        pointer
    };
    (depth, format!("- at `{location}`: {error}"))
}

/// Advice attached to a valid spec. Inline tables beyond a couple hundred
/// rows bloat the spec, the transcript and every later prompt; the renderer
/// reads `data.url` from the workspace, so a CSV/JSON file is the better
/// home for them.
fn spec_warnings(spec: &serde_json::Value) -> Vec<String> {
    let mut warnings = Vec::new();
    if let Some(rows) = spec
        .get("data")
        .and_then(|d| d.get("values"))
        .and_then(|v| v.as_array())
    {
        if rows.len() > INLINE_ROWS_WARNING_THRESHOLD {
            warnings.push(format!(
                "data.values inlines {} rows; write the data to a CSV or JSON file in the workspace (fs_write) and reference it with data.url instead (a leading `/` is workspace-root-relative, e.g. `/data/sales.csv`)",
                rows.len()
            ));
        }
    }
    warnings
}

/// Normalize a model-supplied spec path: workspace-relative, no `..`, ends
/// in `.vl.json` (the extension is what routes the file to the chart viewer
/// and the chart image-link renderer).
pub fn spec_relative_path(input: &str) -> Result<PathBuf, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err("`path` must not be empty".to_string());
    }
    if !trimmed.to_ascii_lowercase().ends_with(SPEC_EXTENSION) {
        return Err(format!(
            "`path` must end in `{SPEC_EXTENSION}` (got `{trimmed}`): that extension is what makes CLAI render the file as a chart"
        ));
    }
    let raw = Path::new(trimmed.strip_prefix('/').unwrap_or(trimmed));
    let mut relative = PathBuf::new();
    for component in raw.components() {
        match component {
            Component::Normal(part) => relative.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(format!(
                    "`path` must stay inside the workspace (no `..` or absolute paths): `{trimmed}`"
                ))
            }
        }
    }
    if relative.as_os_str().is_empty() {
        return Err(format!("`path` names no file: `{trimmed}`"));
    }
    Ok(relative)
}

/// `charts/x.vl.json` with `/` separators on every platform; this string is
/// what the model pastes into markdown.
fn relative_path_string(relative: &Path) -> String {
    relative
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

/// File-name slug of a chart title: lowercase, runs of anything that is not
/// a letter or digit collapse to one `-`, trimmed and capped. Two charts with
/// the same title map to the same file, so re-creating a chart updates it
/// rather than accumulating copies.
pub fn slugify(title: &str) -> String {
    let mut slug = String::new();
    let mut pending_dash = false;
    for ch in title.chars() {
        if ch.is_alphanumeric() {
            if pending_dash && !slug.is_empty() {
                slug.push('-');
            }
            pending_dash = false;
            slug.extend(ch.to_lowercase());
        } else {
            pending_dash = true;
        }
        if slug.chars().count() >= MAX_SLUG_CHARS {
            break;
        }
    }
    let slug = slug.trim_end_matches('-').to_string();
    if slug.is_empty() {
        "chart".to_string()
    } else {
        slug
    }
}

fn write_spec(
    workspace_root: &Path,
    relative: &Path,
    spec: &serde_json::Value,
) -> Result<(), String> {
    let text =
        serde_json::to_string_pretty(spec).map_err(|e| format!("Failed to serialize spec: {e}"))?;
    if text.len() > MAX_SPEC_BYTES {
        return Err(format!(
            "The spec is {} bytes; the limit is {} bytes. Move inline data.values to a workspace CSV/JSON file and reference it with data.url",
            text.len(),
            MAX_SPEC_BYTES
        ));
    }
    let absolute = workspace_root.join(relative);
    if let Some(parent) = absolute.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create {}: {e}", parent.display()))?;
    }
    std::fs::write(&absolute, text.as_bytes())
        .map_err(|e| format!("Failed to write {}: {e}", absolute.display()))
}

fn read_spec_file(absolute: &Path, relative: &Path) -> Result<String, String> {
    let metadata = std::fs::metadata(absolute)
        .map_err(|e| format!("Cannot read `{}`: {e}", relative_path_string(relative)))?;
    if !metadata.is_file() {
        return Err(format!(
            "`{}` is not a file",
            relative_path_string(relative)
        ));
    }
    if metadata.len() > MAX_SPEC_BYTES as u64 {
        return Err(format!(
            "`{}` is {} bytes; the limit is {} bytes",
            relative_path_string(relative),
            metadata.len(),
            MAX_SPEC_BYTES
        ));
    }
    std::fs::read_to_string(absolute)
        .map_err(|e| format!("Cannot read `{}`: {e}", relative_path_string(relative)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn bar_spec() -> serde_json::Value {
        serde_json::json!({
            "data": {"values": [{"a": "A", "b": 28}, {"a": "B", "b": 55}]},
            "mark": "bar",
            "encoding": {
                "x": {"field": "a", "type": "nominal"},
                "y": {"field": "b", "type": "quantitative"}
            }
        })
    }

    fn context_for(workspace_root: Option<PathBuf>) -> ToolExecutionContext {
        ToolExecutionContext {
            session_id: "s".to_string(),
            run_id: "r".to_string(),
            tool_call_id: None,
            cancel_token: Default::default(),
            workspace_id: None,
            space_id: None,
            room_id: None,
            mcp_server_ids: vec![],
            agent_workspace_id: workspace_root.as_ref().map(|_| "ws".to_string()),
            workspace_root,
            automation_id: None,
            workspace_agents: vec![],
            inter_agent_call_depth: None,
            execution: Default::default(),
            notices: Arc::new(Mutex::new(vec![])),
            session_grants: Arc::new(Mutex::new(vec![])),
            session_allowed_command_prefixes: Arc::new(Mutex::new(vec![])),
            session_blocked_command_prefixes: Arc::new(Mutex::new(vec![])),
        }
    }

    #[test]
    fn embedded_schema_compiles() {
        schema_validator().expect("vendored Vega-Lite schema must compile");
    }

    #[test]
    fn percent_encodes_uri_hostile_characters_in_local_refs_only() {
        let mut schema = serde_json::json!({
            "properties": {
                "color": {"$ref": "#/definitions/MarkPropDef<(Gradient|string|null)>"},
                "items": {"$ref": "#/definitions/Vector2<number>"},
                "plain": {"$ref": "#/definitions/Mark"},
                "remote": {"$ref": "https://example.com/schema.json#/x"}
            },
            "list": [{"$ref": "#/definitions/Dict<\"a\">"}]
        });
        percent_encode_refs(&mut schema);
        assert_eq!(
            schema["properties"]["color"]["$ref"],
            "#/definitions/MarkPropDef%3C(Gradient%7Cstring%7Cnull)%3E"
        );
        assert_eq!(
            schema["properties"]["items"]["$ref"],
            "#/definitions/Vector2%3Cnumber%3E"
        );
        assert_eq!(schema["properties"]["plain"]["$ref"], "#/definitions/Mark");
        assert_eq!(
            schema["properties"]["remote"]["$ref"],
            "https://example.com/schema.json#/x"
        );
        assert_eq!(schema["list"][0]["$ref"], "#/definitions/Dict%3C%22a%22%3E");
        // The encoded refs must still resolve: a color channel goes through
        // `MarkPropDef<(Gradient|string|null)>`.
        let mut spec = bar_spec();
        spec["encoding"]["color"] = serde_json::json!({"value": "steelblue"});
        validate_spec(&spec).unwrap();
        spec["encoding"]["color"] = serde_json::json!({"value": 42});
        assert!(validate_spec(&spec)
            .unwrap_err()
            .contains("/encoding/color"));
    }

    #[test]
    fn accepts_a_valid_unit_spec_and_a_layered_spec() {
        validate_spec(&bar_spec()).unwrap();
        let layered = serde_json::json!({
            "data": {"url": "/data/stocks.csv"},
            "layer": [
                {"mark": "line", "encoding": {"x": {"field": "date", "type": "temporal"}, "y": {"field": "price", "type": "quantitative"}}},
                {"mark": "point", "encoding": {"x": {"field": "date", "type": "temporal"}, "y": {"field": "price", "type": "quantitative"}}}
            ]
        });
        validate_spec(&layered).unwrap();
    }

    /// The load-bearing claim of the tool: a typo deep in the spec is
    /// reported at its JSON path, not as an opaque root-level `anyOf`.
    #[test]
    fn reports_the_deepest_violation_with_its_path() {
        let mut spec = bar_spec();
        spec["encoding"]["y"]["type"] = serde_json::json!("quantitive");
        let error = validate_spec(&spec).unwrap_err();
        assert!(error.contains("/encoding/y/type"), "{error}");
        assert!(error.contains("quantitive"), "{error}");
        assert!(
            !error.contains("(root)"),
            "root anyOf noise leaked: {error}"
        );
        assert!(error.contains("call create_vega_chart again"), "{error}");
    }

    #[test]
    fn reports_unknown_marks_and_unknown_properties() {
        let mut spec = bar_spec();
        spec["mark"] = serde_json::json!("barr");
        let error = validate_spec(&spec).unwrap_err();
        assert!(error.contains("/mark"), "{error}");

        let mut spec = bar_spec();
        spec["encoding"]["x"]["feild"] = serde_json::json!("a");
        let error = validate_spec(&spec).unwrap_err();
        assert!(error.contains("feild"), "{error}");
    }

    #[test]
    fn caps_the_number_of_reported_violations() {
        let mut spec = bar_spec();
        for i in 0..12 {
            spec["encoding"][format!("bogus{i}")] = serde_json::json!({});
        }
        let error = validate_spec(&spec).unwrap_err();
        let lines = error.lines().filter(|l| l.starts_with("- ")).count();
        assert!(lines <= MAX_REPORTED_VIOLATIONS + 1, "{error}");
    }

    #[test]
    fn rejects_non_vega_lite_schema_urls_plainly() {
        let mut spec = bar_spec();
        spec["$schema"] = serde_json::json!("https://vega.github.io/schema/vega/v5.json");
        let error = validate_spec(&spec).unwrap_err();
        assert!(error.contains("Unsupported `$schema`"), "{error}");
        // v5 Vega-Lite specs render under the v6 runtime and are accepted.
        spec["$schema"] = serde_json::json!("https://vega.github.io/schema/vega-lite/v5.json");
        validate_spec(&spec).unwrap();
    }

    #[test]
    fn slugifies_titles_into_stable_file_names() {
        assert_eq!(slugify("Q3 Revenue by Region"), "q3-revenue-by-region");
        assert_eq!(slugify("  ***  "), "chart");
        assert_eq!(slugify("CPU % (last 24h)"), "cpu-last-24h");
        assert_eq!(slugify("Ventas – España"), "ventas-españa");
        assert!(slugify(&"x".repeat(200)).chars().count() <= MAX_SLUG_CHARS);
    }

    #[test]
    fn spec_paths_stay_inside_the_workspace_and_keep_the_extension() {
        assert_eq!(
            spec_relative_path("reports/q3/revenue.vl.json").unwrap(),
            PathBuf::from("reports/q3/revenue.vl.json")
        );
        assert_eq!(
            spec_relative_path("/charts/a.VL.JSON").unwrap(),
            PathBuf::from("charts/a.VL.JSON")
        );
        assert_eq!(
            spec_relative_path("./charts/./a.vl.json").unwrap(),
            PathBuf::from("charts/a.vl.json")
        );
        assert!(spec_relative_path("../escape.vl.json")
            .unwrap_err()
            .contains("inside the workspace"));
        assert!(spec_relative_path("charts/../../x.vl.json").is_err());
        assert!(spec_relative_path("charts/a.json")
            .unwrap_err()
            .contains(".vl.json"));
        assert!(spec_relative_path("   ").is_err());
    }

    #[tokio::test]
    async fn writes_a_valid_inline_spec_under_charts_and_returns_the_markdown() {
        let dir = tempfile::tempdir().unwrap();
        let context = context_for(Some(dir.path().to_path_buf()));
        let result = execute(
            &context,
            CreateVegaChartParams {
                title: "Q3 Revenue by Region".to_string(),
                display: true,
                spec: Some(bar_spec()),
                path: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(result["ok"], true);
        assert_eq!(result["action"], "written");
        assert_eq!(result["path"], "charts/q3-revenue-by-region.vl.json");
        assert_eq!(
            result["markdown"],
            "![Q3 Revenue by Region](charts/q3-revenue-by-region.vl.json)"
        );
        assert_eq!(result["display"], true);
        assert!(result.get("warnings").is_none());

        let written: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.path().join("charts/q3-revenue-by-region.vl.json"))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(written["$schema"], VEGA_LITE_SCHEMA_URL);
        assert_eq!(written["mark"], "bar");
    }

    #[tokio::test]
    async fn accepts_a_stringified_spec_and_an_explicit_destination() {
        let dir = tempfile::tempdir().unwrap();
        let context = context_for(Some(dir.path().to_path_buf()));
        let result = execute(
            &context,
            CreateVegaChartParams {
                title: "Latency".to_string(),
                display: false,
                spec: Some(serde_json::Value::String(bar_spec().to_string())),
                path: Some("reports/latency.vl.json".to_string()),
            },
        )
        .await
        .unwrap();
        assert_eq!(result["path"], "reports/latency.vl.json");
        assert_eq!(result["display"], false);
        assert!(dir.path().join("reports/latency.vl.json").is_file());
    }

    #[tokio::test]
    async fn an_invalid_spec_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let context = context_for(Some(dir.path().to_path_buf()));
        let mut spec = bar_spec();
        spec["mark"] = serde_json::json!("barr");
        let error = execute(
            &context,
            CreateVegaChartParams {
                title: "Broken".to_string(),
                display: true,
                spec: Some(spec),
                path: None,
            },
        )
        .await
        .unwrap_err();
        assert!(error.contains("/mark"), "{error}");
        assert!(!dir.path().join("charts").exists());
    }

    #[tokio::test]
    async fn validates_an_existing_file_without_rewriting_it() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("charts/existing.vl.json");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        // Compact, no $schema: must be left byte-for-byte alone.
        let original = bar_spec().to_string();
        std::fs::write(&file, &original).unwrap();
        let context = context_for(Some(dir.path().to_path_buf()));
        let result = execute(
            &context,
            CreateVegaChartParams {
                title: "Existing".to_string(),
                display: true,
                spec: None,
                path: Some("charts/existing.vl.json".to_string()),
            },
        )
        .await
        .unwrap();
        assert_eq!(result["action"], "validated");
        assert_eq!(result["path"], "charts/existing.vl.json");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), original);

        let error = execute(
            &context,
            CreateVegaChartParams {
                title: "Missing".to_string(),
                display: true,
                spec: None,
                path: Some("charts/missing.vl.json".to_string()),
            },
        )
        .await
        .unwrap_err();
        assert!(error.contains("charts/missing.vl.json"), "{error}");
    }

    #[tokio::test]
    async fn warns_about_large_inline_tables() {
        let dir = tempfile::tempdir().unwrap();
        let context = context_for(Some(dir.path().to_path_buf()));
        let mut spec = bar_spec();
        spec["data"]["values"] = serde_json::Value::Array(
            (0..INLINE_ROWS_WARNING_THRESHOLD + 1)
                .map(|i| serde_json::json!({"a": i.to_string(), "b": i}))
                .collect(),
        );
        let result = execute(
            &context,
            CreateVegaChartParams {
                title: "Big".to_string(),
                display: true,
                spec: Some(spec),
                path: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(result["ok"], true);
        let warning = result["warnings"][0].as_str().unwrap();
        assert!(warning.contains("data.url"), "{warning}");
    }

    #[tokio::test]
    async fn requires_a_workspace_a_title_and_one_of_spec_or_path() {
        let no_workspace = context_for(None);
        let error = execute(
            &no_workspace,
            CreateVegaChartParams {
                title: "t".to_string(),
                display: true,
                spec: Some(bar_spec()),
                path: None,
            },
        )
        .await
        .unwrap_err();
        assert!(
            error.contains("not tied to an automation workspace"),
            "{error}"
        );

        let dir = tempfile::tempdir().unwrap();
        let context = context_for(Some(dir.path().to_path_buf()));
        let error = execute(
            &context,
            CreateVegaChartParams {
                title: "  ".to_string(),
                display: true,
                spec: Some(bar_spec()),
                path: None,
            },
        )
        .await
        .unwrap_err();
        assert!(error.contains("`title`"), "{error}");

        let error = execute(
            &context,
            CreateVegaChartParams {
                title: "t".to_string(),
                display: true,
                spec: None,
                path: None,
            },
        )
        .await
        .unwrap_err();
        assert!(
            error.contains("`spec`") && error.contains("`path`"),
            "{error}"
        );
    }
}
