//! Splits a shell command string into independently-evaluable segments.
//!
//! Top-level separators (`|`, `||`, `|&`, `&&`, `;`, `&`, newline) divide a
//! command line into pieces that the shell will run as separate processes.
//! Each piece is returned as a [`Segment`].
//!
//! Segments are classified as either:
//!
//! - [`Segment::Simple`] — a plain command whose head (binary + canonical
//!   subcommand) is unambiguous from textual inspection.
//! - [`Segment::Opaque`] — a segment whose surface form includes one of
//!   command substitution (`$(...)`, backticks), subshells, command groups,
//!   control-flow keywords, test expressions, heredocs, redirects, process
//!   substitution, or an executor-style head (`bash -c`, `xargs`, `eval`,
//!   ...). The shape is harder to reason about than a plain command, but the
//!   policy layer still matches an Opaque segment's binary head against the
//!   allow/blocklist via the same prefix matcher — the `Opaque` tag mostly
//!   informs the UI ("this is more than a bare command") rather than gating
//!   persistence. The splitter is intentionally conservative: anything
//!   ambiguous becomes Opaque, surfaced to the user, and only allowlisted
//!   after an explicit "Always allow" click on the prefix.

#![allow(dead_code)] // wired into enforce_command_policy in commit 6

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Segment {
    Simple(String),
    Opaque(String),
}

impl Segment {
    pub fn text(&self) -> &str {
        match self {
            Segment::Simple(s) | Segment::Opaque(s) => s,
        }
    }

    pub fn is_opaque(&self) -> bool {
        matches!(self, Segment::Opaque(_))
    }
}

/// Splits a shell command into [`Segment`]s.
///
/// The returned vector preserves separator order. Empty (whitespace-only)
/// segments produced by adjacent separators are discarded.
pub fn split_command(input: &str) -> Vec<Segment> {
    let bytes = input.as_bytes();
    let n = bytes.len();

    let mut segments: Vec<Segment> = Vec::new();
    let mut buf = String::new();
    let mut opaque = false;

    let mut in_single = false;
    let mut in_double = false;
    let mut in_backtick = false;
    let mut escape = false;
    let mut depth: u32 = 0;

    let mut i = 0;
    while i < n {
        let c = bytes[i] as char;
        let next = if i + 1 < n {
            Some(bytes[i + 1] as char)
        } else {
            None
        };

        // Escape: a backslash immediately followed by a newline is a line
        // continuation — the shell removes BOTH characters and joins the
        // lines into one logical command. Drop the backslash we already
        // buffered and swallow the newline (and a paired `\r\n`). Any other
        // escaped char is kept literally. (Single quotes never set `escape`,
        // so `\<newline>` inside `'...'` correctly stays literal.)
        if escape {
            escape = false;
            if c == '\n' || c == '\r' {
                buf.pop(); // remove the trailing backslash
                if c == '\r' && next == Some('\n') {
                    i += 2;
                } else {
                    i += 1;
                }
                continue;
            }
            buf.push(c);
            i += 1;
            continue;
        }

        // Inside single quotes: only ' ends it. No escapes.
        if in_single {
            buf.push(c);
            if c == '\'' {
                in_single = false;
            }
            i += 1;
            continue;
        }

        // Inside double quotes: handle \, ", $(, `.
        if in_double {
            buf.push(c);
            if c == '\\' {
                escape = true;
            } else if c == '"' {
                in_double = false;
            } else if c == '$' && next == Some('(') {
                opaque = true;
                buf.push('(');
                depth += 1;
                i += 2;
                continue;
            } else if c == '`' {
                opaque = true;
                in_backtick = true;
            }
            i += 1;
            continue;
        }

        // Inside backticks: only ` ends it. Backslash escapes the next char.
        if in_backtick {
            buf.push(c);
            if c == '\\' {
                escape = true;
            } else if c == '`' {
                in_backtick = false;
            }
            i += 1;
            continue;
        }

        // Outside quotes; depth>0 suppresses separator detection.
        if depth > 0 {
            buf.push(c);
            match c {
                '\\' => escape = true,
                '\'' => in_single = true,
                '"' => in_double = true,
                '`' => {
                    in_backtick = true;
                    opaque = true;
                }
                '(' | '{' | '[' => depth += 1,
                ')' | '}' | ']' => {
                    depth = depth.saturating_sub(1);
                }
                '$' if next == Some('(') || next == Some('{') => {
                    buf.push(next.unwrap());
                    if next == Some('(') {
                        opaque = true;
                    }
                    depth += 1;
                    i += 2;
                    continue;
                }
                _ => {}
            }
            i += 1;
            continue;
        }

        // depth == 0, outside all quotes: full logic applies.

        // Shell comment: `#` at token start (preceded by whitespace or at
        // the start of input/segment) consumes the rest of the line. Bare
        // `#` mid-token (e.g. `foo#bar`) is a literal — the previous-char
        // check filters that out. We DON'T consume the trailing `\n` so
        // the newline-as-separator handling below still flushes any
        // command that preceded the inline comment (`ls # listing` →
        // segment "ls", comment dropped).
        if c == '#' {
            let at_token_start = buf
                .chars()
                .last()
                .map(|ch| ch.is_whitespace())
                .unwrap_or(true);
            if at_token_start {
                while i < n && bytes[i] != b'\n' {
                    i += 1;
                }
                continue;
            }
        }

        match c {
            '\\' => {
                buf.push(c);
                escape = true;
                i += 1;
                continue;
            }
            '\'' => {
                buf.push(c);
                in_single = true;
                i += 1;
                continue;
            }
            '"' => {
                buf.push(c);
                in_double = true;
                i += 1;
                continue;
            }
            '`' => {
                buf.push(c);
                in_backtick = true;
                opaque = true;
                i += 1;
                continue;
            }
            _ => {}
        }

        // Substitution and grouping triggers
        if c == '$' && next == Some('(') {
            let third = if i + 2 < n {
                Some(bytes[i + 2] as char)
            } else {
                None
            };
            if third == Some('(') {
                // Arithmetic expansion $((  — just text, not a substitution.
                // Track both parens so the matching `))` brings depth back.
                buf.push_str("$((");
                depth += 2;
                i += 3;
            } else {
                // Command substitution $(  — runs arbitrary code.
                buf.push_str("$(");
                depth += 1;
                opaque = true;
                i += 2;
            }
            continue;
        }
        if c == '$' && next == Some('{') {
            buf.push_str("${");
            depth += 1;
            i += 2;
            continue;
        }

        // Process substitution: <( ... ) or >( ... )
        if (c == '<' || c == '>') && next == Some('(') {
            buf.push(c);
            buf.push('(');
            depth += 1;
            opaque = true;
            i += 2;
            continue;
        }

        // Here-strings (`<<<word`): single-token input, no body. Fall
        // through to the redirect handler below — they're consumed as
        // part of the `<` redirect run, like any other redirect form.
        // We only treat `<<` and `<<-` as proper heredocs that capture
        // a multi-line body.
        if c == '<' && next == Some('<') && bytes.get(i + 2) != Some(&b'<') {
            opaque = true;
            buf.push_str("<<");
            i += 2;
            // <<- variant strips leading tabs in body; same delimiter
            if bytes.get(i) == Some(&b'-') {
                buf.push('-');
                i += 1;
            }
            // Skip whitespace before the delimiter token.
            while i < n && (bytes[i] == b' ' || bytes[i] == b'\t') {
                buf.push(bytes[i] as char);
                i += 1;
            }
            // Parse the delimiter — may be quoted ('EOF' / "EOF") or
            // bare (EOF).
            let mut delim = String::new();
            if i < n && (bytes[i] == b'\'' || bytes[i] == b'"') {
                let quote = bytes[i];
                buf.push(bytes[i] as char);
                i += 1;
                while i < n && bytes[i] != quote {
                    delim.push(bytes[i] as char);
                    buf.push(bytes[i] as char);
                    i += 1;
                }
                if i < n {
                    buf.push(bytes[i] as char);
                    i += 1; // closing quote
                }
            } else {
                while i < n && !(bytes[i] as char).is_ascii_whitespace() {
                    delim.push(bytes[i] as char);
                    buf.push(bytes[i] as char);
                    i += 1;
                }
            }

            // Malformed (no delimiter parsed) — leave it as Opaque and
            // let the outer loop handle whatever comes next. This is
            // the conservative default; we've already set opaque=true.
            if delim.is_empty() {
                continue;
            }

            // The rest of the current line is part of the line that
            // *started* the heredoc (e.g. trailing pipes or whatever
            // follows the delimiter on the same line). We consume it
            // into the segment buffer without doing any separator
            // splitting, then move on to the body.
            while i < n && bytes[i] != b'\n' {
                buf.push(bytes[i] as char);
                i += 1;
            }
            if i < n {
                buf.push('\n');
                i += 1;
            }

            // Consume body lines until we see the delimiter alone on a
            // line. Per POSIX shell semantics newlines inside the body
            // are NOT command separators — they're literal data. We
            // must therefore skip all of our own separator/quote
            // logic until the closer.
            while i < n {
                let line_start = i;
                while i < n && bytes[i] != b'\n' {
                    i += 1;
                }
                let line_bytes = &bytes[line_start..i];
                let trimmed = std::str::from_utf8(line_bytes).unwrap_or("").trim();
                // Push the line content verbatim.
                for &b in line_bytes {
                    buf.push(b as char);
                }
                let is_closer = trimmed == delim;
                if i < n {
                    buf.push('\n');
                    i += 1;
                }
                if is_closer {
                    break;
                }
            }
            // Heredoc is a self-contained command — flush this segment
            // now so anything that follows (`echo done` on the next
            // line, etc.) starts a fresh segment. Without this the
            // outer loop would keep appending to the same Opaque buf.
            push_segment(&mut segments, &buf, opaque);
            buf.clear();
            opaque = false;
            continue;
        }

        // Plain redirects: >, >>, <, plus dup forms like >&1, <&0
        if c == '>' || c == '<' {
            opaque = true;
            buf.push(c);
            i += 1;
            while i < n {
                let c2 = bytes[i] as char;
                if c2 == '>' || c2 == '<' || c2 == '&' || c2 == '|' || c2 == '-' {
                    buf.push(c2);
                    i += 1;
                } else {
                    break;
                }
            }
            continue;
        }

        // &> and &>> stdout+stderr redirect
        if c == '&' && next == Some('>') {
            opaque = true;
            buf.push('&');
            buf.push('>');
            i += 2;
            while i < n {
                let c2 = bytes[i] as char;
                if c2 == '>' || c2 == '|' || c2 == '-' {
                    buf.push(c2);
                    i += 1;
                } else {
                    break;
                }
            }
            continue;
        }

        // Subshell / command group at segment top level
        if c == '(' {
            buf.push(c);
            depth += 1;
            opaque = true;
            i += 1;
            continue;
        }
        if c == '{' {
            // `{` is ambiguous between brace expansion (`a{b,c}d` inside
            // an argument) and a command group (`{ cmd1; cmd2; }`). The
            // shell distinguishes by tokenisation: a command group is its
            // own token, so `{` is preceded by whitespace or starts the
            // input. If `{` is glued to the preceding token, it's brace
            // expansion — treat it as a literal so file-glob patterns
            // like `docs/00{08,09,10}*.md` don't get flagged Opaque.
            let glued = buf
                .chars()
                .last()
                .map(|ch| !ch.is_whitespace())
                .unwrap_or(false);
            if glued {
                buf.push(c);
                i += 1;
                continue;
            }
            buf.push(c);
            depth += 1;
            opaque = true;
            i += 1;
            continue;
        }
        // Test expressions: [ ... ] or [[ ... ]] mark Opaque. We don't track
        // their depth — separators inside are unusual and would only cause
        // a benign over-split (each side still becomes Opaque from carrying
        // a `[` or `]` marker, or from the executor-head check).
        if c == '[' {
            buf.push(c);
            opaque = true;
            i += 1;
            continue;
        }
        if c == ']' || c == ')' || c == '}' {
            buf.push(c);
            i += 1;
            continue;
        }

        // Separators
        if c == '|' && next == Some('|') {
            push_segment(&mut segments, &buf, opaque);
            buf.clear();
            opaque = false;
            i += 2;
            continue;
        }
        if c == '&' && next == Some('&') {
            push_segment(&mut segments, &buf, opaque);
            buf.clear();
            opaque = false;
            i += 2;
            continue;
        }
        if c == '|' && next == Some('&') {
            // |& : pipe with stderr included. Same separation as |.
            push_segment(&mut segments, &buf, opaque);
            buf.clear();
            opaque = false;
            i += 2;
            continue;
        }
        if c == '|' {
            push_segment(&mut segments, &buf, opaque);
            buf.clear();
            opaque = false;
            i += 1;
            continue;
        }
        if c == '&' {
            push_segment(&mut segments, &buf, opaque);
            buf.clear();
            opaque = false;
            i += 1;
            continue;
        }
        if c == ';' || c == '\n' || c == '\r' {
            push_segment(&mut segments, &buf, opaque);
            buf.clear();
            opaque = false;
            i += 1;
            continue;
        }

        // Regular character
        buf.push(c);
        i += 1;
    }

    push_segment(&mut segments, &buf, opaque);

    // If parsing ended inside an unterminated quote, backtick, or bracket
    // group, separator detection was unreliable for everything after the
    // opening delimiter — a single stray quote silently swallows later
    // `;` / `&&` / `|`, collapsing unrelated commands into one mis-identified
    // segment. Rather than emit a misleading partial segmentation, surface
    // the whole command as one Opaque segment so the user reviews it as a
    // unit. `suggest_prefix` declines to offer a prefix for a junk head like
    // `:`, so no bogus "always allow" is proposed for the blob.
    if in_single || in_double || in_backtick || depth > 0 {
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return Vec::new();
        }
        return vec![Segment::Opaque(trimmed.to_string())];
    }

    // Post-process: reclassify Simple segments whose head is an executor
    // command or a control-flow keyword as Opaque. These run other programs
    // (xargs, bash -c, ...) or open scoped scopes that aren't safely
    // allowlistable.
    for seg in segments.iter_mut() {
        if let Segment::Simple(s) = seg {
            if head_is_opaque_trigger(s) {
                *seg = Segment::Opaque(s.clone());
            }
        }
    }

    // Collapse compound shell constructs (`for/while/until/select … done`,
    // `if … fi`, `case … esac`) into a single Opaque segment. Splitting on
    // every `;` / `&&` / `||` inside a loop produces meaningless approval
    // fragments like `do gh run view …`, `break`, `done` — none of which
    // the user can sensibly decide on in isolation. One construct = one
    // approval card.
    merge_compound_constructs(input, segments)
}

fn push_segment(out: &mut Vec<Segment>, buf: &str, opaque: bool) {
    let trimmed = buf.trim();
    if trimmed.is_empty() {
        return;
    }
    let s = trimmed.to_string();
    if opaque {
        out.push(Segment::Opaque(s));
    } else {
        out.push(Segment::Simple(s));
    }
}

/// True if the first non-env-assignment token is an executor command or a
/// control-flow keyword.
fn head_is_opaque_trigger(segment: &str) -> bool {
    // Tokens whose presence at the head means the segment runs arbitrary
    // other code, or alters control flow such that the segment isn't a
    // simple "this command, these args" shape.
    const EXECUTORS: &[&str] = &[
        "bash", "sh", "zsh", "dash", "ksh", "fish", "xargs", "eval", "watch", "nohup", "time",
        "timeout", "nice", "taskset", "env", "exec", "source", ".",
    ];
    const CONTROL_FLOW: &[&str] = &[
        "if", "then", "elif", "else", "fi", "for", "while", "until", "do", "done", "case", "esac",
        "select", "function", "in", "break", "continue", "return",
    ];

    let mut tokens = segment.split_whitespace();
    let mut head = tokens.next();
    while let Some(tok) = head {
        if is_env_assignment(tok) {
            head = tokens.next();
        } else {
            break;
        }
    }
    let Some(head) = head else {
        return false;
    };
    EXECUTORS.contains(&head) || CONTROL_FLOW.contains(&head)
}

/// Collapses runs of segments belonging to a single compound shell construct
/// (`for…done`, `while…done`, `until…done`, `select…done`, `if…fi`,
/// `case…esac`) into one Opaque segment carrying the verbatim original text.
///
/// Uses head-token matching only (not a scan of every word) so that
/// `echo for; echo done` is *not* falsely treated as a loop. The trade-off:
/// constructs nested without a separator between keywords (e.g.
/// `if cond; then if other; then x; fi; fi`) merge in two passes rather than
/// one. That's a minor regression on a pathological shape, not on the
/// common shapes the LLM actually emits (CI polling loops, retry loops,
/// case-based dispatch).
fn merge_compound_constructs(input: &str, segments: Vec<Segment>) -> Vec<Segment> {
    if !segments
        .iter()
        .any(|s| is_compound_opener(first_token(s.text())))
    {
        return segments;
    }
    let ranges = locate_segments(input, &segments);

    let mut result: Vec<Segment> = Vec::with_capacity(segments.len());
    let mut i = 0;
    while i < segments.len() {
        let head = first_token(segments[i].text());
        let Some(opener_kw) = compound_opener_kw(head) else {
            result.push(segments[i].clone());
            i += 1;
            continue;
        };
        let mut stack: Vec<&'static str> = vec![opener_kw];
        let mut end: Option<usize> = None;
        for (j, seg) in segments.iter().enumerate().skip(i + 1) {
            let h = first_token(seg.text());
            if let Some(kw) = compound_opener_kw(h) {
                stack.push(kw);
            } else if let Some(closes) = closer_targets(h) {
                if let Some(top) = stack.last() {
                    if closes.contains(top) {
                        stack.pop();
                    }
                }
            }
            if stack.is_empty() {
                end = Some(j);
                break;
            }
        }
        match end {
            Some(end_idx) => {
                let start_b = ranges[i].start;
                let end_b = ranges[end_idx].end;
                let merged = input[start_b..end_b].trim().to_string();
                result.push(Segment::Opaque(merged));
                i = end_idx + 1;
            }
            None => {
                // Malformed: opener with no matching closer in this input.
                // Fall through to per-segment behavior so the user still
                // sees something rather than silently dropping work.
                result.push(segments[i].clone());
                i += 1;
            }
        }
    }
    result
}

fn is_compound_opener(tok: &str) -> bool {
    compound_opener_kw(tok).is_some()
}

fn compound_opener_kw(tok: &str) -> Option<&'static str> {
    match tok {
        "for" => Some("for"),
        "while" => Some("while"),
        "until" => Some("until"),
        "select" => Some("select"),
        "if" => Some("if"),
        "case" => Some("case"),
        _ => None,
    }
}

fn closer_targets(tok: &str) -> Option<&'static [&'static str]> {
    match tok {
        "done" => Some(&["for", "while", "until", "select"]),
        "fi" => Some(&["if"]),
        "esac" => Some(&["case"]),
        _ => None,
    }
}

fn first_token(s: &str) -> &str {
    s.split_whitespace().next().unwrap_or("")
}

/// Finds each segment's byte range in the original input. Segment text was
/// produced from `input` by the splitter (with at most leading/trailing
/// whitespace trimmed), so a forward-only substring search reliably
/// recovers each segment's position. Used by [`merge_compound_constructs`]
/// to slice the verbatim original text (separators included) when merging.
fn locate_segments(input: &str, segments: &[Segment]) -> Vec<std::ops::Range<usize>> {
    let mut ranges = Vec::with_capacity(segments.len());
    let mut search_from = 0;
    for seg in segments {
        let text = seg.text();
        if let Some(rel) = input[search_from..].find(text) {
            let start = search_from + rel;
            let end = start + text.len();
            ranges.push(start..end);
            search_from = end;
        } else {
            // Unreachable: a segment came from this input. Defensive
            // fallback keeps indices aligned with `segments`.
            ranges.push(0..0);
        }
    }
    ranges
}

fn is_env_assignment(tok: &str) -> bool {
    let Some(eq) = tok.find('=') else {
        return false;
    };
    if eq == 0 {
        return false;
    }
    let name = &tok[..eq];
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_alphabetic() && first != '_' {
        return false;
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn simple(s: &str) -> Segment {
        Segment::Simple(s.to_string())
    }

    fn opaque(s: &str) -> Segment {
        Segment::Opaque(s.to_string())
    }

    fn split(s: &str) -> Vec<Segment> {
        split_command(s)
    }

    // -----------------------------------------------------------------
    // Empty / trivial
    // -----------------------------------------------------------------

    #[test]
    fn empty_input_yields_no_segments() {
        assert_eq!(split(""), vec![]);
    }

    #[test]
    fn whitespace_only_yields_no_segments() {
        assert_eq!(split("   \t  "), vec![]);
    }

    #[test]
    fn single_simple_command() {
        assert_eq!(split("git status"), vec![simple("git status")]);
    }

    #[test]
    fn leading_and_trailing_whitespace_trimmed() {
        assert_eq!(split("   git status   "), vec![simple("git status")]);
    }

    // -----------------------------------------------------------------
    // Separators
    // -----------------------------------------------------------------

    #[test]
    fn pipe_splits() {
        assert_eq!(
            split("git log | head"),
            vec![simple("git log"), simple("head")]
        );
    }

    #[test]
    fn logical_and_splits() {
        assert_eq!(split("a && b"), vec![simple("a"), simple("b")]);
    }

    #[test]
    fn logical_or_splits() {
        assert_eq!(split("a || b"), vec![simple("a"), simple("b")]);
    }

    #[test]
    fn semicolon_splits() {
        assert_eq!(
            split("a; b; c"),
            vec![simple("a"), simple("b"), simple("c")]
        );
    }

    #[test]
    fn background_amp_splits() {
        assert_eq!(split("cmd1 & cmd2"), vec![simple("cmd1"), simple("cmd2")]);
    }

    #[test]
    fn pipe_stderr_splits() {
        assert_eq!(split("a |& b"), vec![simple("a"), simple("b")]);
    }

    #[test]
    fn newline_splits() {
        assert_eq!(
            split("a\nb\nc"),
            vec![simple("a"), simple("b"), simple("c")]
        );
    }

    #[test]
    fn repeated_separators_no_empty_segments() {
        assert_eq!(split("a ;;; b"), vec![simple("a"), simple("b")]);
    }

    #[test]
    fn three_pipe_chain() {
        assert_eq!(
            split("git log --oneline -5 | head -3 | wc -l"),
            vec![
                simple("git log --oneline -5"),
                simple("head -3"),
                simple("wc -l"),
            ]
        );
    }

    // -----------------------------------------------------------------
    // Quoting
    // -----------------------------------------------------------------

    #[test]
    fn double_quoted_pipe_not_split() {
        assert_eq!(split(r#"echo "a | b""#), vec![simple(r#"echo "a | b""#)]);
    }

    #[test]
    fn single_quoted_pipe_not_split() {
        assert_eq!(split("echo 'a | b'"), vec![simple("echo 'a | b'")]);
    }

    #[test]
    fn escaped_pipe_not_split() {
        assert_eq!(split(r"echo a\|b"), vec![simple(r"echo a\|b")]);
    }

    #[test]
    fn escaped_double_quote_inside_double_quote() {
        // Pipe must NOT split because we're still inside the double quote.
        assert_eq!(
            split(r#"echo "a\" | still_in" out"#),
            vec![simple(r#"echo "a\" | still_in" out"#)]
        );
    }

    // -----------------------------------------------------------------
    // Redirects → Opaque
    // -----------------------------------------------------------------

    #[test]
    fn redirect_out_marks_opaque() {
        assert_eq!(split("cat foo > bar"), vec![opaque("cat foo > bar")]);
    }

    #[test]
    fn append_redirect_marks_opaque() {
        assert_eq!(split("echo hi >> log"), vec![opaque("echo hi >> log")]);
    }

    #[test]
    fn stderr_redirect_marks_opaque() {
        assert_eq!(split("cmd 2> errlog"), vec![opaque("cmd 2> errlog")]);
    }

    #[test]
    fn fd_dup_does_not_split_on_amp() {
        // `2>&1` contains `&` but it's part of the redirect — must not split.
        assert_eq!(
            split("cmd 2>&1 | grep err"),
            vec![opaque("cmd 2>&1"), simple("grep err")]
        );
    }

    #[test]
    fn ampersand_gt_redirect_opaque() {
        assert_eq!(split("cmd &> log"), vec![opaque("cmd &> log")]);
    }

    #[test]
    fn here_string_opaque() {
        assert_eq!(split("cmd <<< hello"), vec![opaque("cmd <<< hello")]);
    }

    #[test]
    fn heredoc_opaque() {
        assert_eq!(split("cat <<EOF"), vec![opaque("cat <<EOF")]);
    }

    #[test]
    fn heredoc_body_does_not_split_on_newlines() {
        // The whole heredoc — including the body lines — is one
        // Opaque segment. Newlines inside the body are NOT command
        // separators (POSIX shell semantics).
        let input = "cat <<EOF\nline one\nline two\nEOF";
        let segs = split(input);
        assert_eq!(segs.len(), 1);
        assert!(segs[0].is_opaque(), "got {:?}", segs[0]);
        let text = segs[0].text();
        assert!(text.contains("line one"));
        assert!(text.contains("line two"));
    }

    #[test]
    fn heredoc_with_quoted_delimiter_does_not_split() {
        // `<< 'EOF'` style with a body that includes shell-meaningful
        // chars (// comments, semicolons) that would otherwise split.
        let input = "cat > file << 'EOF'\n//! comment\npub mod x;\nEOF";
        let segs = split(input);
        assert_eq!(segs.len(), 1);
        assert!(segs[0].is_opaque());
        assert!(segs[0].text().contains("//! comment"));
        assert!(segs[0].text().contains("pub mod x"));
    }

    #[test]
    fn heredoc_then_followup_command_splits_after_closer() {
        // After the closing delimiter the next newline IS a separator.
        let input = "cat <<EOF\nbody\nEOF\necho done";
        let segs = split(input);
        assert_eq!(segs.len(), 2);
        assert!(segs[0].is_opaque());
        assert!(segs[0].text().contains("body"));
        assert_eq!(segs[1], simple("echo done"));
    }

    #[test]
    fn heredoc_with_leading_tab_strip_delimiter() {
        // `<<-EOF` is the tab-stripping variant; same delimiter rules.
        let input = "cat <<-EOF\n\tindented body\n\tEOF";
        let segs = split(input);
        assert_eq!(segs.len(), 1);
        assert!(segs[0].is_opaque());
        assert!(segs[0].text().contains("indented body"));
    }

    #[test]
    fn heredoc_chained_with_logical_and_does_not_overflow() {
        // The motivating real-world bug from the screenshot: the LLM
        // emits `cd dir && cat > file << 'EOF' ... EOF` followed by
        // another command. Before this fix the heredoc body lines were
        // each treated as separate segments.
        let input = "cd dir && cat > file << 'EOF'\n//! a\n//! b\nEOF\ncat file";
        let segs = split(input);
        // Three logical commands: cd dir, the cat-heredoc Opaque, cat file
        assert_eq!(segs.len(), 3, "got {:?}", segs);
        assert_eq!(segs[0], simple("cd dir"));
        assert!(segs[1].is_opaque());
        assert!(segs[1].text().contains("//! a"));
        assert!(segs[1].text().contains("//! b"));
        assert_eq!(segs[2], simple("cat file"));
    }

    // -----------------------------------------------------------------
    // Command substitution / subshell / grouping → Opaque
    // -----------------------------------------------------------------

    #[test]
    fn dollar_paren_substitution_opaque_and_no_inner_split() {
        // The `|` inside $(...) must not cause a split.
        assert_eq!(
            split("echo $(date | tr a A)"),
            vec![opaque("echo $(date | tr a A)")]
        );
    }

    #[test]
    fn backtick_substitution_opaque() {
        assert_eq!(split("echo `date`"), vec![opaque("echo `date`")]);
    }

    #[test]
    fn subshell_opaque() {
        assert_eq!(
            split("(cd /tmp && rm -rf foo)"),
            vec![opaque("(cd /tmp && rm -rf foo)")]
        );
    }

    #[test]
    fn brace_group_opaque() {
        assert_eq!(split("{ a; b; }"), vec![opaque("{ a; b; }")]);
    }

    #[test]
    fn brace_expansion_in_argument_stays_simple() {
        // The motivating real-world case: file-glob brace expansion
        // glued to a path-shaped argument should NOT be treated as a
        // command group. The current splitter previously marked
        // `wc -l docs/00{08,09}*.md` Opaque, forcing fresh approval on
        // every run.
        assert_eq!(
            split("wc -l docs/00{08,09,10}*.md"),
            vec![simple("wc -l docs/00{08,09,10}*.md")]
        );
    }

    #[test]
    fn brace_expansion_with_pipe_after_stays_simple() {
        assert_eq!(
            split("wc -l docs/00{08,09}*.md | tail -10"),
            vec![simple("wc -l docs/00{08,09}*.md"), simple("tail -10"),]
        );
    }

    // -----------------------------------------------------------------
    // Shell comments — discarded at top level
    // -----------------------------------------------------------------

    #[test]
    fn full_line_comment_dropped() {
        // A line that is only a comment yields no segment.
        assert_eq!(split("# this is a comment"), vec![]);
    }

    #[test]
    fn full_line_comment_between_commands() {
        // The LLM frequently emits multi-line scripts with section
        // header comments. Each comment line should disappear; the
        // surrounding commands stay intact.
        assert_eq!(
            split("# Phase 1\nls docs/\n# Phase 2\nwc -l docs/*.md"),
            vec![simple("ls docs/"), simple("wc -l docs/*.md")]
        );
    }

    #[test]
    fn trailing_inline_comment_dropped() {
        // Inline `# …` after a command, separated by whitespace, is a
        // comment — drop it; the command segment stays.
        assert_eq!(split("ls -la # listing"), vec![simple("ls -la")]);
    }

    #[test]
    fn hash_inside_token_is_literal() {
        // `#` glued to the previous char is part of the token, not a
        // comment start (e.g. an anchor in a URL).
        assert_eq!(
            split("curl https://example.com/path#anchor"),
            vec![simple("curl https://example.com/path#anchor")]
        );
    }

    #[test]
    fn hash_inside_quotes_is_literal() {
        // Inside a quoted string `#` is a plain char.
        assert_eq!(
            split(r#"echo "value has # in it""#),
            vec![simple(r#"echo "value has # in it""#)]
        );
    }

    #[test]
    fn comment_does_not_consume_following_newline_separator() {
        // The trailing newline must still act as a separator between
        // the (post-comment-strip) leading content and the next command.
        // Without preserving the newline, `ls\n# c\necho` would merge.
        let segs = split("ls\n# c\necho hi");
        assert_eq!(segs, vec![simple("ls"), simple("echo hi")]);
    }

    #[test]
    fn brace_at_token_start_still_opaque() {
        // Regression guard: `{` preceded by whitespace is still a
        // command-group opener and must remain Opaque.
        assert_eq!(
            split("ls && { echo a; echo b; }"),
            vec![simple("ls"), opaque("{ echo a; echo b; }")]
        );
    }

    #[test]
    fn process_substitution_opaque() {
        assert_eq!(split("diff <(a) <(b)"), vec![opaque("diff <(a) <(b)")]);
    }

    #[test]
    fn parameter_expansion_is_simple() {
        // ${VAR} is variable expansion, not opaque.
        assert_eq!(split("echo ${HOME}/bin"), vec![simple("echo ${HOME}/bin")]);
    }

    #[test]
    fn arithmetic_expansion_is_simple() {
        // $((1+2)) is arithmetic, not opaque.
        assert_eq!(split("echo $((1+2))"), vec![simple("echo $((1+2))")]);
    }

    // -----------------------------------------------------------------
    // Test expressions → Opaque
    // -----------------------------------------------------------------

    #[test]
    fn bracket_test_opaque() {
        // The `;` inside likely splits, but each part still ends up Opaque.
        let segs = split("[ -f foo ] && echo yes");
        // First segment is opaque due to `[`, second is Simple ("echo yes").
        assert_eq!(segs.len(), 2);
        assert!(segs[0].is_opaque(), "got {:?}", segs[0]);
        assert_eq!(segs[1], simple("echo yes"));
    }

    #[test]
    fn double_bracket_test_opaque() {
        let segs = split("[[ -f foo ]]");
        assert_eq!(segs.len(), 1);
        assert!(segs[0].is_opaque());
    }

    // -----------------------------------------------------------------
    // Executor commands at head → Opaque
    // -----------------------------------------------------------------

    #[test]
    fn bash_dash_c_opaque() {
        assert_eq!(
            split(r#"bash -c "rm -rf /""#),
            vec![opaque(r#"bash -c "rm -rf /""#)]
        );
    }

    #[test]
    fn sh_dash_c_opaque() {
        assert_eq!(
            split(r#"sh -c "echo hi""#),
            vec![opaque(r#"sh -c "echo hi""#)]
        );
    }

    #[test]
    fn xargs_in_pipe_opaque() {
        assert_eq!(
            split("find . -name '*.tmp' | xargs rm"),
            vec![simple("find . -name '*.tmp'"), opaque("xargs rm")]
        );
    }

    #[test]
    fn eval_opaque() {
        assert_eq!(split(r#"eval "$cmd""#), vec![opaque(r#"eval "$cmd""#)]);
    }

    #[test]
    fn watch_opaque() {
        assert_eq!(split("watch -n 1 'ls'"), vec![opaque("watch -n 1 'ls'")]);
    }

    #[test]
    fn time_opaque() {
        assert_eq!(split("time make build"), vec![opaque("time make build")]);
    }

    #[test]
    fn env_opaque() {
        // env runs another binary; conservative Opaque.
        assert_eq!(split("env -i mycmd"), vec![opaque("env -i mycmd")]);
    }

    #[test]
    fn source_opaque() {
        assert_eq!(
            split("source ./setup.sh"),
            vec![opaque("source ./setup.sh")]
        );
    }

    #[test]
    fn dot_source_opaque() {
        assert_eq!(split(". ./setup.sh"), vec![opaque(". ./setup.sh")]);
    }

    // -----------------------------------------------------------------
    // Env-assignment prefix does NOT make Opaque
    // -----------------------------------------------------------------

    #[test]
    fn env_prefix_remains_simple() {
        // `FOO=bar mycmd` is a shell construct, not a call to env.
        // The head after stripping env-assignments is `mycmd`, so still Simple.
        assert_eq!(
            split("FOO=bar mycmd --opt"),
            vec![simple("FOO=bar mycmd --opt")]
        );
    }

    #[test]
    fn multi_env_prefix_remains_simple() {
        assert_eq!(split("A=1 B=2 git log"), vec![simple("A=1 B=2 git log")]);
    }

    // -----------------------------------------------------------------
    // Control-flow keywords → Opaque
    // -----------------------------------------------------------------

    #[test]
    fn if_then_fi_collapses_to_single_opaque() {
        let segs = split("if true; then echo yes; fi");
        assert_eq!(segs.len(), 1);
        assert!(segs[0].is_opaque());
        assert_eq!(segs[0].text(), "if true; then echo yes; fi");
    }

    #[test]
    fn for_loop_collapses_to_single_opaque() {
        let segs = split("for f in *.txt; do cat $f; done");
        assert_eq!(segs.len(), 1);
        assert!(segs[0].is_opaque());
        assert_eq!(segs[0].text(), "for f in *.txt; do cat $f; done");
    }

    #[test]
    fn while_loop_collapses_to_single_opaque() {
        let segs = split("while true; do sleep 1; done");
        assert_eq!(segs.len(), 1);
        assert!(segs[0].is_opaque());
    }

    #[test]
    fn until_loop_collapses_to_single_opaque() {
        let segs = split("until [ -f /tmp/x ]; do sleep 1; done");
        assert_eq!(segs.len(), 1);
        assert!(segs[0].is_opaque());
    }

    #[test]
    fn case_block_collapses_to_single_opaque() {
        let segs = split("case x in a) echo a;; b) echo b;; esac");
        assert_eq!(segs.len(), 1);
        assert!(segs[0].is_opaque());
        assert!(segs[0].text().contains("esac"));
    }

    #[test]
    fn retry_polling_loop_collapses_verbatim() {
        // Motivating real-world case: a CI polling loop the LLM emitted to
        // monitor a workflow. Before this fix it produced 4-5 meaningless
        // approval cards (`for i in 1..5`, `do gh run view ...`, `break`,
        // `sleep 15`, `done`); now it's one Opaque carrying the original
        // text verbatim — including `&&` and `||`, not just `;`.
        let input = "for i in 1 2 3 4 5; do gh run view --json status && break || sleep 15; done";
        let segs = split(input);
        assert_eq!(segs.len(), 1);
        assert!(segs[0].is_opaque());
        assert_eq!(segs[0].text(), input);
    }

    #[test]
    fn trailing_command_after_loop_stays_separate() {
        let segs = split("for i in 1 2; do echo $i; done && echo finished");
        assert_eq!(segs.len(), 2);
        assert!(segs[0].is_opaque());
        assert!(segs[0].text().contains("for i in 1 2"));
        assert!(segs[0].text().contains("done"));
        assert!(!segs[0].text().contains("finished"));
        assert_eq!(segs[1], simple("echo finished"));
    }

    #[test]
    fn two_sequential_loops_each_collapse_separately() {
        let segs = split("for i in 1; do echo a; done; for j in 2; do echo b; done");
        assert_eq!(segs.len(), 2);
        assert!(segs[0].is_opaque());
        assert!(segs[0].text().contains("for i in 1"));
        assert!(segs[1].is_opaque());
        assert!(segs[1].text().contains("for j in 2"));
    }

    #[test]
    fn nested_if_inside_for_collapses_at_outer_done() {
        let segs = split("for i in *; do if [ -f $i ]; then echo $i; fi; done");
        assert_eq!(segs.len(), 1);
        assert!(segs[0].is_opaque());
        assert!(segs[0].text().ends_with("done"));
    }

    #[test]
    fn malformed_loop_without_closer_falls_through() {
        // No matching `done`. We don't hang; we keep the segments split.
        let segs = split("for i in 1 2 3; do echo $i");
        assert!(!segs.is_empty());
        for seg in &segs {
            assert!(seg.is_opaque(), "{:?} should be Opaque", seg);
        }
    }

    #[test]
    fn echo_with_kw_substring_not_treated_as_loop() {
        // `echo for; echo done` is two echo commands, not a loop. Head
        // tokens are `echo`, not the loop keywords — must not collapse.
        let segs = split("echo for; echo done");
        assert_eq!(segs.len(), 2);
        assert_eq!(segs[0], simple("echo for"));
        assert_eq!(segs[1], simple("echo done"));
    }

    #[test]
    fn break_is_opaque_not_simple_with_prefix() {
        // `break` is a loop builtin — meaningless outside a loop and
        // unsafe to allowlist (`break-something` would match). Even
        // when it survives compound-construct merging (e.g. malformed
        // input), it must not be Simple.
        let segs = split("break");
        assert_eq!(segs.len(), 1);
        assert!(segs[0].is_opaque());
    }

    // -----------------------------------------------------------------
    // Compound real-world commands
    // -----------------------------------------------------------------

    #[test]
    fn mixed_pipeline() {
        assert_eq!(
            split("git log | obscure-tool | grep Running"),
            vec![
                simple("git log"),
                simple("obscure-tool"),
                simple("grep Running"),
            ]
        );
    }

    #[test]
    fn and_chain_with_simple_segments() {
        assert_eq!(
            split("git pull && make build && make test"),
            vec![
                simple("git pull"),
                simple("make build"),
                simple("make test"),
            ]
        );
    }

    #[test]
    fn pipe_with_redirect_marks_only_redirect_segment_opaque() {
        let segs = split("git log > /tmp/log | grep foo");
        // First segment has `>`, opaque. Second is simple.
        assert_eq!(segs.len(), 2);
        assert!(segs[0].is_opaque());
        assert_eq!(segs[1], simple("grep foo"));
    }

    #[test]
    fn closes_pipe_bypass_foot_gun() {
        // The motivating attack: a saved `git log` prefix would
        // word-boundary-match the whole string today, silently approving
        // `rm -rf ~/`. Under per-segment evaluation, the `rm` segment
        // stands alone and (a) gets blocklisted by the default blocklist
        // upstream, or (b) requires fresh approval if the user removed
        // the default. Either way, the saved `git log` prefix never
        // approves the `rm` segment.
        let segs = split("git log | rm -rf ~/");
        assert_eq!(segs, vec![simple("git log"), simple("rm -rf ~/")]);
    }

    // -----------------------------------------------------------------
    // Segment helpers
    // -----------------------------------------------------------------

    #[test]
    fn segment_text_accessor() {
        assert_eq!(simple("a").text(), "a");
        assert_eq!(opaque("b").text(), "b");
    }

    #[test]
    fn segment_is_opaque_predicate() {
        assert!(!simple("a").is_opaque());
        assert!(opaque("b").is_opaque());
    }

    // -----------------------------------------------------------------
    // is_env_assignment / head_is_opaque_trigger
    // -----------------------------------------------------------------

    #[test]
    fn is_env_assignment_recognizes_valid() {
        assert!(is_env_assignment("FOO=bar"));
        assert!(is_env_assignment("_FOO=bar"));
        assert!(is_env_assignment("a1=2"));
        assert!(is_env_assignment("X="));
    }

    #[test]
    fn is_env_assignment_rejects_invalid() {
        assert!(!is_env_assignment("FOO"));
        assert!(!is_env_assignment("=bar"));
        assert!(!is_env_assignment("1FOO=bar")); // can't start with digit
        assert!(!is_env_assignment("foo-bar=x")); // dash not allowed
    }

    // -----------------------------------------------------------------
    // Backslash-newline line continuation (Fix #1)
    // -----------------------------------------------------------------

    #[test]
    fn backslash_newline_continuation_joins_lines() {
        // The shell removes `\<newline>` entirely: this is one `git log`,
        // not a segment carrying a literal backslash + embedded newline.
        assert_eq!(
            split("git log \\\n  --oneline"),
            vec![simple("git log   --oneline")]
        );
    }

    #[test]
    fn backslash_newline_continuation_does_not_create_extra_segment() {
        // Without continuation handling the bare newline would split this
        // into two segments; the `\` must suppress that.
        assert_eq!(split("echo a \\\necho b"), vec![simple("echo a echo b")]);
    }

    #[test]
    fn backslash_crlf_continuation_joins_lines() {
        assert_eq!(
            split("git log \\\r\n  --oneline"),
            vec![simple("git log   --oneline")]
        );
    }

    #[test]
    fn backslash_newline_continuation_inside_double_quotes() {
        // Line continuation is also removed inside double quotes.
        assert_eq!(split("echo \"a \\\nb\""), vec![simple("echo \"a b\"")]);
    }

    #[test]
    fn backslash_before_normal_char_still_literal() {
        // Only `\<newline>` is special; `\;` stays a literal escaped char.
        assert_eq!(split(r"echo a\;b"), vec![simple(r"echo a\;b")]);
    }

    // -----------------------------------------------------------------
    // Unterminated quote / group defense (Fix #3)
    // -----------------------------------------------------------------

    #[test]
    fn unterminated_double_quote_collapses_to_single_opaque() {
        // A stray unclosed quote desyncs separator detection. Rather than
        // emit a scrambled partial segmentation, the whole command becomes
        // one Opaque segment for review.
        let segs = split("echo \"unterminated ; rm -rf / && cd /a");
        assert_eq!(segs.len(), 1);
        assert!(segs[0].is_opaque());
        assert_eq!(segs[0].text(), "echo \"unterminated ; rm -rf / && cd /a");
    }

    #[test]
    fn unterminated_single_quote_collapses_to_single_opaque() {
        let segs = split("echo 'oops ; ls");
        assert_eq!(segs, vec![opaque("echo 'oops ; ls")]);
    }

    #[test]
    fn unbalanced_paren_group_collapses_to_single_opaque() {
        let segs = split("( cd /a && echo hi");
        assert_eq!(segs.len(), 1);
        assert!(segs[0].is_opaque());
    }

    #[test]
    fn balanced_quotes_after_fix_still_split_normally() {
        // Guard: the defense must not trip on well-formed input.
        assert_eq!(
            split(r#"echo "a ; b" && ls"#),
            vec![simple(r#"echo "a ; b""#), simple("ls")]
        );
    }
}
