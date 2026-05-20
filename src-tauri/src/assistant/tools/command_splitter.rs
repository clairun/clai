//! Splits a shell command string into independently-evaluable segments.
//!
//! Top-level separators (`|`, `||`, `|&`, `&&`, `;`, `&`, newline) divide a
//! command line into pieces that the shell will run as separate processes.
//! Each piece is returned as a [`Segment`].
//!
//! Segments are classified as either:
//!
//! - [`Segment::Simple`] — a plain command whose prefix can be matched against
//!   the allow/blocklist with the usual word-boundary prefix matcher.
//! - [`Segment::Opaque`] — a segment that cannot be safely allowlisted. This
//!   covers command substitution (`$(...)`, backticks), subshells, command
//!   groups, control-flow keywords, test expressions, heredocs, redirects,
//!   process substitution, and executor-style heads (`bash -c`, `xargs`,
//!   `eval`, ...). The caller must require fresh approval for Opaque
//!   segments and may not persist a prefix derived from them.
//!
//! The splitter is intentionally conservative: anything ambiguous becomes
//! Opaque, which is safe (it only blocks persistent allowlisting, not the
//! one-shot approval flow).

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

        // Escape: consume one literal char and clear the flag.
        if escape {
            buf.push(c);
            escape = false;
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

        // Heredocs: <<, <<-, <<<
        if c == '<' && next == Some('<') {
            opaque = true;
            buf.push('<');
            buf.push('<');
            i += 2;
            while i < n {
                let c2 = bytes[i] as char;
                if c2 == '<' || c2 == '-' {
                    buf.push(c2);
                    i += 1;
                } else {
                    break;
                }
            }
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

    segments
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
        "select", "function", "in",
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
    fn if_then_fi_each_opaque() {
        let segs = split("if true; then echo yes; fi");
        assert_eq!(segs.len(), 3);
        for seg in &segs {
            assert!(seg.is_opaque(), "{:?} should be Opaque", seg);
        }
    }

    #[test]
    fn for_loop_opaque() {
        let segs = split("for f in *.txt; do cat $f; done");
        // 'for' head, 'do' head, 'done' head all opaque
        assert!(segs.iter().any(|s| s.text().starts_with("for")));
        for seg in &segs {
            assert!(seg.is_opaque(), "{:?} should be Opaque", seg);
        }
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
}
