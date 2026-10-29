//! Splits a shell command string into the commands it will run.
//!
//! Every command the shell would execute becomes one [`Segment`]: the
//! commands joined by `|`, `||`, `|&`, `&&`, `;`, `&` or a newline, and the
//! commands inside subshells `( … )`, brace groups `{ …; }`, function bodies
//! and `if`/`while`/`until`/`for`/`select`/`case` blocks. A call to a
//! function is a command too, wherever the function was defined: its
//! arguments may be what runs (`f(){ "$@"; }; f rm -rf x`).
//!
//! Shell grammar is not a command: braces, parentheses, reserved words,
//! function headers, `for`/`case` headers and `case` patterns. It is dropped,
//! unless it redirects or embeds a substitution (`for f in $(ls)`, or
//! `done > log`, which applies to the whole loop); then it is kept as an
//! Opaque segment, because it runs or writes something.
//!
//! Segments are classified as either:
//!
//! - [`Segment::Simple`] — a plain command whose head (binary + canonical
//!   subcommand) is unambiguous from textual inspection.
//! - [`Segment::Opaque`] — a segment whose surface form includes one of
//!   command substitution (`$(...)`, backticks), test expressions, heredocs,
//!   redirects, process substitution, or an executor-style head (`bash -c`,
//!   `xargs`, `eval`, ...). The shape is harder to reason about than a plain
//!   command, but the policy layer still matches an Opaque segment's binary
//!   head against the allow/blocklist via the same prefix matcher — the
//!   `Opaque` tag mostly informs the UI ("this is more than a bare command")
//!   rather than gating persistence. The splitter is intentionally
//!   conservative: anything ambiguous becomes Opaque, surfaced to the user,
//!   and only allowlisted after an explicit "Always allow" click on the
//!   prefix.

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

/// A block whose commands are segments of their own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scope {
    /// `( … )`
    Subshell,
    /// `{ …; }`
    Group,
    /// `if … fi`
    If,
    /// `for`, `select`, `while`, `until` … `done`
    Loop,
    /// `case … esac`: its header and its item bodies.
    Case,
    /// A `case` item's pattern, from `in` or `;;` to its `)`.
    Pattern,
}

/// What a reserved word does where a command would start.
#[derive(Debug, Clone, Copy)]
enum Keyword {
    /// Opens a block whose body follows: `if`, `while`, `until`, `{`.
    Open(Scope),
    /// Opens a block with a header that is not a command: `for`,
    /// `select`, `case`.
    Header(Scope),
    /// Separates the parts of a block, or negates a pipeline: `then`,
    /// `elif`, `else`, `do`, `!`.
    Part,
    /// Closes a block: `fi`, `done`, `esac`, `}`.
    Close(Scope),
    /// `function NAME`.
    Function,
    /// `in`, which belongs only in a header.
    Misplaced,
}

fn keyword(word: &str) -> Option<Keyword> {
    Some(match word {
        "if" => Keyword::Open(Scope::If),
        "while" | "until" => Keyword::Open(Scope::Loop),
        "{" => Keyword::Open(Scope::Group),
        "for" | "select" => Keyword::Header(Scope::Loop),
        "case" => Keyword::Header(Scope::Case),
        "then" | "elif" | "else" | "do" | "!" => Keyword::Part,
        "fi" => Keyword::Close(Scope::If),
        "done" => Keyword::Close(Scope::Loop),
        "esac" => Keyword::Close(Scope::Case),
        "}" => Keyword::Close(Scope::Group),
        "function" => Keyword::Function,
        "in" => Keyword::Misplaced,
        _ => return None,
    })
}

/// True for a word that is shell grammar rather than a command name. Such a
/// word heads a segment only when the grammar redirects or substitutes, or
/// the input could not be split; either way it is no prefix to allowlist.
pub fn is_shell_syntax(word: &str) -> bool {
    keyword(word).is_some()
}

/// True if `text` runs a command of its own through a command or process
/// substitution.
pub fn has_substitution(text: &str) -> bool {
    ["$(", "`", "<(", ">("].iter().any(|s| text.contains(s))
}

/// The characters that separate words in shell syntax. A carriage return is
/// not one: bash reads it as part of a word.
fn is_blank(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n')
}

/// A heredoc whose body starts on the line after its operator.
struct Heredoc {
    /// The delimiter after quote removal.
    delim: String,
    /// `<<-`: the closing line may be indented with tabs.
    strip_tabs: bool,
    /// The segment of the command that reads it.
    owner: usize,
}

/// The commands read so far, and the one being read.
#[derive(Default)]
struct Commands {
    segments: Vec<Segment>,
    buf: String,
    /// `buf` has a construct that makes it Opaque.
    opaque: bool,
    /// `buf` is grammar, not a command: a `for`/`case` header, a `case`
    /// pattern, a function header, or a closed block and its redirects.
    grammar: bool,
    scopes: Vec<Scope>,
    /// Heredocs opened on the current line.
    heredocs: Vec<Heredoc>,
}

impl Commands {
    fn at_command_start(&self) -> bool {
        !self.grammar && self.buf.chars().all(is_blank)
    }

    /// `buf` holds only the word that closed a block. A reserved word may
    /// follow it directly, but not after a redirect: `if { a; } then`,
    /// `fi fi`, against `done > fi`.
    fn just_closed(&self) -> bool {
        let word = self.buf.trim();
        self.grammar && (word == ")" || matches!(keyword(word), Some(Keyword::Close(_))))
    }

    fn top(&self) -> Option<Scope> {
        self.scopes.last().copied()
    }

    /// The keyword heading `buf` while it is a `for`, `select` or `case`
    /// header.
    fn header(&self) -> Option<&str> {
        let first = self.buf.split_whitespace().next()?;
        (self.grammar && matches!(first, "for" | "select" | "case")).then_some(first)
    }

    /// A header may break lines before its word list: `case WORD` up to
    /// `in`, and `for NAME` up to `in` or `do`.
    fn header_continues(&self) -> bool {
        match self.header() {
            Some("case") => true,
            Some(_) => self.buf.split_whitespace().count() == 2,
            None => false,
        }
    }

    /// Ends the command in `buf`. Grammar runs nothing of its own unless it
    /// redirects or substitutes; then it is kept, as Opaque.
    fn end(&mut self) {
        let text = self.buf.trim();
        let runs = !self.grammar || has_substitution(text) || text.contains(['<', '>']);
        if runs && !text.is_empty() {
            let text = text.to_string();
            self.segments
                .push(if self.opaque || self.grammar || head_is_executor(&text) {
                    Segment::Opaque(text)
                } else {
                    Segment::Simple(text)
                });
        }
        self.buf.clear();
        self.opaque = false;
        self.grammar = false;
    }

    /// Applies a reserved word read where a command would start. `None`
    /// when it is out of place, such as closing a block that is not open.
    fn keyword(&mut self, keyword: Keyword, word: &str) -> Option<()> {
        match keyword {
            Keyword::Open(scope) => self.scopes.push(scope),
            Keyword::Header(scope) => {
                self.scopes.push(scope);
                self.buf.push_str(word);
                self.grammar = true;
            }
            Keyword::Part | Keyword::Function => {}
            Keyword::Close(scope) => self.close(scope, word)?,
            Keyword::Misplaced => return None,
        }
        Some(())
    }

    /// Closes the innermost block, which must be `scope`, with `word`.
    /// What follows up to the next separator applies to the whole block,
    /// so it is grammar, shown after `word` if it redirects.
    fn close(&mut self, scope: Scope, word: &str) -> Option<()> {
        if self.scopes.pop() != Some(scope) {
            return None;
        }
        self.buf.push_str(word);
        self.grammar = true;
        Some(())
    }

    fn enter_pattern(&mut self) {
        self.scopes.push(Scope::Pattern);
        self.grammar = true;
    }

    /// Reads the bodies of the heredocs opened on the line that ended just
    /// before `i`, appends each to the command that reads it, and returns
    /// where lexing resumes. A body is data, not commands.
    fn read_heredoc_bodies(&mut self, input: &str, mut i: usize) -> usize {
        for heredoc in std::mem::take(&mut self.heredocs) {
            let start = i;
            for line in input[start..].split_inclusive('\n') {
                i += line.len();
                let line = line.strip_suffix('\n').unwrap_or(line);
                let line = if heredoc.strip_tabs {
                    line.trim_start_matches('\t')
                } else {
                    line
                };
                if line == heredoc.delim {
                    break;
                }
            }
            if let Some(Segment::Simple(text) | Segment::Opaque(text)) =
                self.segments.get_mut(heredoc.owner)
            {
                text.push('\n');
                text.push_str(input[start..i].trim_end_matches('\n'));
            }
        }
        i
    }
}

/// Splits a shell command into [`Segment`]s.
///
/// The returned vector preserves execution order. When the input cannot be
/// split reliably — an unterminated quote, an unbalanced block — separator
/// detection after that point means nothing (one stray quote silently
/// swallows later `;` / `&&` / `|`), so the whole input becomes one Opaque
/// segment the user reviews as a unit. [`is_shell_syntax`] and the junk-head
/// rule in `suggest_prefix` keep a bogus "always allow" off that blob.
pub fn split_command(input: &str) -> Vec<Segment> {
    split(input).unwrap_or_else(|| vec![Segment::Opaque(input.trim().to_string())])
}

#[derive(PartialEq, Eq)]
enum CasePart {
    Header,
    Pattern,
    Body,
}

#[allow(
    clippy::cognitive_complexity,
    reason = "lint debt: cognitive complexity 65 against a budget of 25"
)]
#[expect(
    clippy::too_many_lines,
    reason = "lint debt: 354 lines against a 100-line budget; split it, do not raise the budget"
)]
fn split(input: &str) -> Option<Vec<Segment>> {
    let bytes = input.as_bytes();
    let n = bytes.len();

    let mut out = Commands::default();

    let mut in_single = false;
    let mut in_double = false;
    let mut in_backtick = false;
    let mut escape = false;
    let mut depth: u32 = 0;
    let mut double_quote_resume_depths = Vec::new();
    let mut substitution_cases: Vec<(u32, CasePart)> = Vec::new();

    let mut i = 0;
    while i < n {
        // Shell syntax is ASCII, so any other character is literal text in
        // every state: copy it whole. Read byte by byte, `—` would become
        // `â\u{80}\u{94}`. Every other step moves over ASCII bytes, so `i`
        // is always on a character boundary here.
        if !bytes[i].is_ascii() {
            let ch = input[i..]
                .chars()
                .next()
                .unwrap_or(char::REPLACEMENT_CHARACTER);
            out.buf.push(ch);
            escape = false;
            i += ch.len_utf8();
            continue;
        }
        let c = bytes[i] as char;
        let next = if i + 1 < n {
            Some(bytes[i + 1] as char)
        } else {
            None
        };

        // Escape: a backslash immediately followed by a newline is a line
        // continuation — the shell removes BOTH characters and joins the
        // lines into one logical command. Drop the backslash we already
        // buffered and swallow the newline. Any other escaped char is kept
        // literally, a carriage return too: bash joins no lines at `\`
        // before `\r\n`. (Single quotes never set `escape`, so
        // `\<newline>` inside `'...'` correctly stays literal.)
        if escape {
            escape = false;
            if c == '\n' {
                out.buf.pop(); // remove the trailing backslash
                i += 1;
                continue;
            }
            out.buf.push(c);
            i += 1;
            continue;
        }

        // Inside single quotes: only ' ends it. No escapes.
        if in_single {
            out.buf.push(c);
            if c == '\'' {
                in_single = false;
            }
            i += 1;
            continue;
        }

        // Inside double quotes: handle \, ", $(, `.
        if in_double {
            out.buf.push(c);
            if c == '\\' {
                escape = true;
            } else if c == '"' {
                in_double = false;
            } else if c == '$' && next == Some('(') {
                out.opaque = true;
                out.buf.push('(');
                depth += 1;
                double_quote_resume_depths.push(depth - 1);
                in_double = false;
                i += 2;
                continue;
            } else if c == '`' {
                out.opaque = true;
                in_backtick = true;
            }
            i += 1;
            continue;
        }

        // Inside backticks: only ` ends it. Backslash escapes the next char.
        if in_backtick {
            out.buf.push(c);
            if c == '\\' {
                escape = true;
            } else if c == '`' {
                in_backtick = false;
            }
            i += 1;
            continue;
        }

        // Inside a substitution or expansion: one word, no separators.
        if depth > 0 {
            // A case pattern's `)` belongs to the item, not the substitution.
            let word_start =
                i == 0 || is_blank(bytes[i - 1] as char) || ";&|()".contains(bytes[i - 1] as char);
            if word_start {
                match word_at(input, i) {
                    "case" => substitution_cases.push((depth, CasePart::Header)),
                    "in" => {
                        if let Some((case_depth, part @ CasePart::Header)) =
                            substitution_cases.last_mut()
                        {
                            if *case_depth == depth {
                                *part = CasePart::Pattern;
                            }
                        }
                    }
                    "esac" => {
                        if substitution_cases.last().is_some_and(|(case_depth, part)| {
                            *case_depth == depth && *part != CasePart::Header
                        }) {
                            substitution_cases.pop();
                        }
                    }
                    _ => {}
                }
            }
            if c == ';' && next == Some(';') {
                if let Some((case_depth, part @ CasePart::Body)) = substitution_cases.last_mut() {
                    if *case_depth == depth {
                        *part = CasePart::Pattern;
                    }
                }
            }
            out.buf.push(c);
            match c {
                '\\' => escape = true,
                '\'' => in_single = true,
                '"' => in_double = true,
                '`' => {
                    in_backtick = true;
                    out.opaque = true;
                }
                '(' if substitution_cases.last().is_some_and(|(case_depth, part)| {
                    *case_depth == depth && *part == CasePart::Pattern
                }) => {}
                '(' | '{' | '[' => depth += 1,
                ')' if substitution_cases
                    .last_mut()
                    .is_some_and(|(case_depth, part)| {
                        if *case_depth == depth && *part == CasePart::Pattern {
                            *part = CasePart::Body;
                            true
                        } else {
                            false
                        }
                    }) => {}
                ')' | '}' | ']' => {
                    depth -= 1;
                    if double_quote_resume_depths.last() == Some(&depth) {
                        double_quote_resume_depths.pop();
                        in_double = true;
                    }
                }
                '$' if next == Some('(') || next == Some('{') => {
                    out.buf.push(next.unwrap());
                    if next == Some('(') {
                        out.opaque = true;
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
            let at_token_start = out.buf.chars().last().map(is_blank).unwrap_or(true);
            if at_token_start {
                while i < n && bytes[i] != b'\n' {
                    i += 1;
                }
                continue;
            }
        }

        match c {
            '\\' => {
                out.buf.push(c);
                escape = true;
                i += 1;
                continue;
            }
            '\'' => {
                out.buf.push(c);
                in_single = true;
                i += 1;
                continue;
            }
            '"' => {
                out.buf.push(c);
                in_double = true;
                i += 1;
                continue;
            }
            '`' => {
                out.buf.push(c);
                in_backtick = true;
                out.opaque = true;
                i += 1;
                continue;
            }
            _ => {}
        }

        if c == '\n' {
            // In a case pattern, or before a header's word list, a newline
            // is a blank; elsewhere it ends the command.
            if out.top() == Some(Scope::Pattern) || out.header_continues() {
                out.buf.push(' ');
            } else {
                out.end();
            }
            i = out.read_heredoc_bodies(input, i + 1);
            continue;
        }

        if let Some(after) = grammar_word(input, i, &mut out)? {
            i = after;
            continue;
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
                out.buf.push_str("$((");
                depth += 2;
                i += 3;
            } else {
                // Command substitution $(  — runs arbitrary code.
                out.buf.push_str("$(");
                depth += 1;
                out.opaque = true;
                i += 2;
            }
            continue;
        }
        if c == '$' && next == Some('{') {
            out.buf.push_str("${");
            depth += 1;
            i += 2;
            continue;
        }

        if out.top() == Some(Scope::Pattern) {
            i = read_pattern(input, i, &mut out)?;
            continue;
        }

        // Process substitution: <( ... ) or >( ... )
        if (c == '<' || c == '>') && next == Some('(') {
            out.buf.push(c);
            out.buf.push('(');
            depth += 1;
            out.opaque = true;
            i += 2;
            continue;
        }

        // Here-strings (`<<<word`): single-token input, no body. Fall
        // through to the redirect handler below — they're consumed as
        // part of the `<` redirect run, like any other redirect form.
        // We only treat `<<` and `<<-` as proper heredocs that capture
        // a multi-line body.
        if c == '<' && next == Some('<') && bytes.get(i + 2) != Some(&b'<') {
            i = read_heredoc(input, i, &mut out);
            continue;
        }

        // Plain redirects: >, >>, <, plus dup forms like >&1, <&0
        if c == '>' || c == '<' {
            out.opaque = true;
            out.buf.push(c);
            i += 1;
            while i < n {
                let c2 = bytes[i] as char;
                if c2 == '>' || c2 == '<' || c2 == '&' || c2 == '|' || c2 == '-' {
                    out.buf.push(c2);
                    i += 1;
                } else {
                    break;
                }
            }
            continue;
        }

        // &> and &>> stdout+stderr redirect
        if c == '&' && next == Some('>') {
            out.opaque = true;
            out.buf.push('&');
            out.buf.push('>');
            i += 2;
            while i < n {
                let c2 = bytes[i] as char;
                if c2 == '>' || c2 == '|' || c2 == '-' {
                    out.buf.push(c2);
                    i += 1;
                } else {
                    break;
                }
            }
            continue;
        }

        if c == '(' {
            // `NAME()` or `NAME ()` defines a function; its body follows.
            if let Some(after) = empty_parens(input, i) {
                let name = out.buf.trim().to_string();
                if !out.grammar && is_function_name(&name) {
                    out.buf.clear();
                    out.opaque = false;
                    i = after;
                    continue;
                }
            }
            // A subshell. `((` opens an arithmetic command instead.
            if out.at_command_start() && next != Some('(') {
                out.scopes.push(Scope::Subshell);
                i += 1;
                continue;
            }
            // An array literal `a=(…)` or arithmetic `((…))`: one word.
            out.buf.push(c);
            depth += 1;
            out.opaque = true;
            i += 1;
            continue;
        }
        if c == ')' && out.top() == Some(Scope::Subshell) {
            out.end();
            out.close(Scope::Subshell, ")")?;
            i += 1;
            continue;
        }
        // `{` heading a command word (not a group, which is the word `{`
        // alone) is brace expansion: `{rm,-rf,x}` runs `rm -rf x`.
        if c == '{' && out.at_command_start() {
            out.opaque = true;
        }
        // Test expressions: [ ... ] or [[ ... ]] mark Opaque. We don't track
        // their depth — separators inside are unusual and would only cause
        // a benign over-split (each side still becomes Opaque from carrying
        // a `[` or `]` marker, or from the executor-head check).
        if c == '[' {
            out.opaque = true;
        }

        // Separators
        if c == ';' && out.top() == Some(Scope::Case) && matches!(next, Some(';' | '&')) {
            // `;;`, `;&` or `;;&` ends a case item; a pattern follows.
            out.end();
            out.enter_pattern();
            i += if input[i..].starts_with(";;&") { 3 } else { 2 };
            continue;
        }
        let separator_len = match (c, next) {
            ('|', Some('|' | '&')) | ('&', Some('&')) => 2,
            ('|' | '&' | ';', _) => 1,
            _ => 0,
        };
        if separator_len > 0 {
            out.end();
            i += separator_len;
            continue;
        }

        // Regular character
        out.buf.push(c);
        i += 1;
    }

    if in_single || in_double || in_backtick || depth > 0 || !out.scopes.is_empty() {
        return None;
    }
    out.end();
    Some(out.segments)
}

/// Applies the shell grammar word at `i`, if there is one, and returns
/// where lexing resumes: a reserved word where a command would start
/// (elsewhere `done` or `{` is an ordinary argument), or the word that ends
/// a block header. `None` when the word is out of place, such as a closer
/// of a block that is not open.
fn grammar_word(input: &str, i: usize, out: &mut Commands) -> Option<Option<usize>> {
    let word = word_at(input, i);
    if out.just_closed() && keyword(word).is_some() {
        out.end();
    }
    if out.at_command_start() && !word.is_empty() {
        let Some(kw) = keyword(word) else {
            return Some(None);
        };
        let mut i = i + word.len();
        if let Keyword::Function = kw {
            // `function NAME`, optionally followed by `()`, is a header.
            i = skip_blanks(input, i);
            let name = word_at(input, i);
            if name.is_empty() {
                return None;
            }
            i += name.len();
            i = empty_parens(input, i).unwrap_or(i);
        } else {
            out.keyword(kw, word)?;
        }
        return Some(Some(i));
    }
    // A header ends at `in` (`case WORD in`) or at `do` (the `for NAME do`
    // form, without a word list).
    if out.buf.ends_with(is_blank) {
        let header_ends = match (out.header(), word) {
            (Some("case"), "in") => true,
            (Some("for" | "select"), "do") => out.header_continues(),
            _ => false,
        };
        if header_ends {
            out.end();
            if word == "in" {
                out.enter_pattern();
            }
            return Some(Some(i + word.len()));
        }
    }
    Some(None)
}

/// Reads one step of a `case` pattern, which runs to its `)`: `|` in it
/// separates alternatives, not commands. `esac` may follow the last item.
/// Returns where lexing resumes.
fn read_pattern(input: &str, i: usize, out: &mut Commands) -> Option<usize> {
    let c = input.as_bytes()[i] as char;
    let blank = out.buf.chars().all(is_blank);
    if blank && word_at(input, i) == "esac" {
        out.scopes.pop();
        out.close(Scope::Case, "esac")?;
        return Some(i + "esac".len());
    }
    if c == ')' {
        out.end();
        out.scopes.pop();
    } else if !(blank && (c == '(' || is_blank(c))) {
        out.buf.push(c);
    }
    Some(i + 1)
}

/// Reads the heredoc operator at `i` (`<<`, `<<-`) and its delimiter word
/// into the command, queues the heredoc so its body is read after this
/// line, and returns where lexing resumes.
fn read_heredoc(input: &str, start: usize, out: &mut Commands) -> usize {
    out.opaque = true;
    let mut i = start + 2;
    let strip_tabs = input[i..].starts_with('-');
    if strip_tabs {
        i += 1;
    }
    i = skip_blanks(input, i);
    let (len, delim) = heredoc_delimiter(&input[i..]);
    i += len;
    out.buf.push_str(&input[start..i]);
    if !delim.is_empty() {
        out.heredocs.push(Heredoc {
            delim,
            strip_tabs,
            owner: out.segments.len(),
        });
    }
    i
}

/// The heredoc delimiter word heading `rest`: its length, and its text with
/// quotes removed. It ends at a blank or an operator outside quotes, so
/// `'EOF'`, `"E F"`, `\EOF` and `E"OF"` close at a line reading `EOF` or
/// `E F`.
fn heredoc_delimiter(rest: &str) -> (usize, String) {
    let mut delim = String::new();
    let mut chars = rest.char_indices();
    while let Some((at, c)) = chars.next() {
        match c {
            '\'' | '"' => delim.extend(chars.by_ref().map(|(_, q)| q).take_while(|&q| q != c)),
            '\\' => match chars.next() {
                Some((_, '\n')) | None => {}
                Some((_, escaped)) => delim.push(escaped),
            },
            _ if is_blank(c) || ";&|()<>".contains(c) => return (at, delim),
            _ => delim.push(c),
        }
    }
    (rest.len(), delim)
}

/// The word starting at `i`: up to a blank or an operator character.
fn word_at(input: &str, i: usize) -> &str {
    let rest = &input[i..];
    let len = rest
        .find(|c: char| is_blank(c) || ";&|()<>".contains(c))
        .unwrap_or(rest.len());
    &rest[..len]
}

fn skip_blanks(input: &str, i: usize) -> usize {
    input.len() - input[i..].trim_start_matches([' ', '\t']).len()
}

/// The position after `()` (blanks allowed inside) at `i`, if there.
fn empty_parens(input: &str, i: usize) -> Option<usize> {
    let rest = input[skip_blanks(input, i)..].strip_prefix('(')?;
    let rest = rest.trim_start_matches([' ', '\t']).strip_prefix(')')?;
    Some(input.len() - rest.len())
}

/// A word that `NAME()` defines as a function: no blanks, quoting,
/// expansion, or assignment.
fn is_function_name(word: &str) -> bool {
    !word.is_empty()
        && !word
            .chars()
            .any(|c| c.is_whitespace() || "=$'\"`\\".contains(c))
}

/// `xargs`, `eval`, `source`, and the like.
fn head_is_executor(segment: &str) -> bool {
    const EXECUTORS: &[&str] = &[
        "bash", "sh", "zsh", "dash", "ksh", "fish", "xargs", "eval", "watch", "nohup", "time",
        "timeout", "nice", "taskset", "env", "exec", "source", ".",
    ];
    segment
        .split_whitespace()
        .find(|tok| !is_env_assignment(tok))
        .is_some_and(|head| EXECUTORS.contains(&head))
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
    fn keeps_non_ascii_text_whole() {
        // The text the user approves is the command as written: `—` and `→`,
        // not their UTF-8 bytes read as Latin-1 (`â\u{80}\u{94}`).
        assert_eq!(
            split("echo \"14:11— 18:02 → 10 eval\" | tee -a notes.md"),
            vec![
                simple("echo \"14:11— 18:02 → 10 eval\""),
                simple("tee -a notes.md")
            ]
        );
        let heredoc = "cat <<'FIN' > notes.md\nswitch — 18:02 → café\nFIN";
        assert_eq!(split(heredoc), vec![opaque(heredoc)]);
        // Inside a block, too: "Confirms" holds no `fi`.
        let compound = "if true; then echo \"Confirms — the blocker\"; fi";
        assert_eq!(
            split(compound),
            vec![simple("true"), simple("echo \"Confirms — the blocker\"")]
        );
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
    fn heredoc_line_after_operator_is_still_split() {
        // The body starts on the next line; `| sh` on the operator's line
        // is a command of its own, not part of the heredoc.
        assert_eq!(
            split("cat <<EOF | sh\nrm -rf ~\nEOF\necho done"),
            vec![
                opaque("cat <<EOF\nrm -rf ~\nEOF"),
                opaque("sh"),
                simple("echo done")
            ]
        );
    }

    #[test]
    fn heredoc_body_starts_after_the_command_line() {
        // A continuation, a newline in quotes, or an operator glued to the
        // delimiter: the body starts after the line the shell reads.
        assert_eq!(
            split("cat <<EOF \\\n| sh\ntouch t\nEOF"),
            vec![opaque("cat <<EOF\ntouch t\nEOF"), opaque("sh")]
        );
        assert_eq!(
            split("cat <<EOF|sh\ntouch t\nEOF"),
            vec![opaque("cat <<EOF\ntouch t\nEOF"), opaque("sh")]
        );
        assert_eq!(
            split("cat <<EOF; echo \"a\nb\"\nx\nEOF\ntouch t"),
            vec![
                opaque("cat <<EOF\nx\nEOF"),
                simple("echo \"a\nb\""),
                simple("touch t")
            ]
        );
    }

    #[test]
    fn quoted_heredoc_delimiter_may_hold_blanks_and_operators() {
        for (input, heredoc) in [
            ("cat <<'E F'\nx\nE F\necho y #'", "cat <<'E F'\nx\nE F"),
            ("cat <<\"E F\"\nx\nE F\necho y #\"", "cat <<\"E F\"\nx\nE F"),
            ("cat <<'E;F'\nx\nE;F\necho y #'", "cat <<'E;F'\nx\nE;F"),
            ("cat <<E\\ F\nx\nE F\necho y #'", "cat <<E\\ F\nx\nE F"),
        ] {
            assert_eq!(
                split(input),
                vec![opaque(heredoc), simple("echo y")],
                "{input:?}"
            );
        }
    }

    #[test]
    fn heredoc_delimiter_has_quotes_removed_and_closes_exactly() {
        assert_eq!(
            split("cat <<\\EOF\nx\nEOF\ntouch t"),
            vec![opaque("cat <<\\EOF\nx\nEOF"), simple("touch t")]
        );
        assert_eq!(
            split("cat <<E\"OF\"\nx\nEOF\ntouch t"),
            vec![opaque("cat <<E\"OF\"\nx\nEOF"), simple("touch t")]
        );
        // `EOF ` is no closer for bash, so `touch t` is still body.
        assert_eq!(
            split("cat <<EOF\nEOF \ntouch t\nEOF"),
            vec![opaque("cat <<EOF\nEOF \ntouch t\nEOF")]
        );
    }

    #[test]
    fn two_heredocs_on_one_line_take_consecutive_bodies() {
        assert_eq!(
            split("diff <<A <<B && echo same\na; b\nA\nb; c\nB\necho next"),
            vec![
                opaque("diff <<A <<B\na; b\nA\nb; c\nB"),
                simple("echo same"),
                simple("echo next"),
            ]
        );
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
    fn subshell_yields_its_commands() {
        assert_eq!(
            split("(cd /tmp && rm -rf foo) | tar -x"),
            vec![simple("cd /tmp"), simple("rm -rf foo"), simple("tar -x")]
        );
    }

    #[test]
    fn brace_group_yields_its_commands() {
        assert_eq!(split("{ a; b; }"), vec![simple("a"), simple("b")]);
        assert_eq!(
            split("{\n  a\n  b\n} > log"),
            vec![simple("a"), simple("b"), opaque("} > log")]
        );
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
    fn brace_group_after_separator_yields_its_commands() {
        assert_eq!(
            split("ls && { echo a; echo b; }"),
            vec![simple("ls"), simple("echo a"), simple("echo b")]
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
    // Blocks: their commands are segments, their grammar is not
    // -----------------------------------------------------------------

    #[test]
    fn if_block_yields_its_commands() {
        assert_eq!(
            split("if true; then echo yes; elif false; then echo maybe; else echo no; fi"),
            vec![
                simple("true"),
                simple("echo yes"),
                simple("false"),
                simple("echo maybe"),
                simple("echo no"),
            ]
        );
    }

    #[test]
    fn for_loop_yields_body_not_header() {
        assert_eq!(
            split("for f in *.txt; do cat $f; done"),
            vec![simple("cat $f")]
        );
    }

    #[test]
    fn for_header_with_substitution_stays_opaque() {
        // The substitution in the word list runs, so the header is kept.
        assert_eq!(
            split("for f in $(ls); do cat $f; done"),
            vec![opaque("for f in $(ls)"), simple("cat $f")]
        );
    }

    #[test]
    fn for_without_word_list_ends_header_at_do() {
        // `for NAME do …` iterates "$@"; `rm` must not hide in the header.
        assert_eq!(split("for f do rm $f; done"), vec![simple("rm $f")]);
    }

    #[test]
    fn while_and_until_conditions_are_commands() {
        assert_eq!(
            split("while true; do sleep 1; done"),
            vec![simple("true"), simple("sleep 1")]
        );
        assert_eq!(
            split("until [ -f /tmp/x ]; do sleep 1; done"),
            vec![opaque("[ -f /tmp/x ]"), simple("sleep 1")]
        );
    }

    #[test]
    fn case_yields_item_bodies_not_patterns() {
        // `|` in a pattern separates alternatives, not commands.
        assert_eq!(
            split("case \"$1\" in start|up) systemctl start x;; (stop) echo b;;\n *) echo c\nesac"),
            vec![
                simple("systemctl start x"),
                simple("echo b"),
                simple("echo c")
            ]
        );
    }

    #[test]
    fn retry_polling_loop_yields_real_commands() {
        // A CI polling loop the LLM emitted.
        let input = "for i in 1 2 3 4 5; do gh run view --json status && break || sleep 15; done";
        assert_eq!(
            split(input),
            vec![
                simple("gh run view --json status"),
                simple("break"),
                simple("sleep 15")
            ]
        );
    }

    #[test]
    fn redirect_after_block_is_kept_with_its_closer() {
        // It applies to the whole block, and may write any file.
        assert_eq!(
            split("for i in 1 2; do echo $i; done 2>&1 | tail -1"),
            vec![simple("echo $i"), opaque("done 2>&1"), simple("tail -1")]
        );
        assert_eq!(
            split("if [ 1 ]; then x=1; fi > important"),
            vec![opaque("[ 1 ]"), simple("x=1"), opaque("fi > important")]
        );
        assert_eq!(
            split("while read l; do echo $l; done < <(git ls-files)"),
            vec![
                simple("read l"),
                simple("echo $l"),
                opaque("done < <(git ls-files)")
            ]
        );
    }

    #[test]
    fn reserved_word_right_after_a_closer_is_read() {
        assert_eq!(
            split("if { true; } then rm -rf ~; fi"),
            vec![simple("true"), simple("rm -rf ~")]
        );
        assert_eq!(
            split("if (true)then rm -rf ~; fi"),
            vec![simple("true"), simple("rm -rf ~")]
        );
        assert_eq!(
            split("while (true) do curl x | sh; done"),
            vec![simple("true"), simple("curl x"), opaque("sh")]
        );
        assert_eq!(
            split("if a; then if b; then c; fi fi; rm x"),
            vec![simple("a"), simple("b"), simple("c"), simple("rm x")]
        );
        assert_eq!(
            split("case x in x) { a; } esac; rm x"),
            vec![simple("a"), simple("rm x")]
        );
        // After a redirect the word is its target, a file named `fi`.
        assert_eq!(
            split("for i in 1; do echo a; done > fi"),
            vec![simple("echo a"), opaque("done > fi")]
        );
    }

    #[test]
    fn case_inside_a_substitution_ends_at_its_closing_paren() {
        // A pattern's `)` does not close the substitution.
        for (input, inner) in [
            (
                "echo $(case x in x) a;; esac); rm -rf x",
                "echo $(case x in x) a;; esac)",
            ),
            (
                "echo \"$(case x in x) a;; esac)\"; rm -rf x",
                "echo \"$(case x in x) a;; esac)\"",
            ),
            (
                "cat <(case x in x) a;; esac); rm -rf x",
                "cat <(case x in x) a;; esac)",
            ),
            (
                "echo `case x in x) a;; esac`; rm -rf x",
                "echo `case x in x) a;; esac`",
            ),
        ] {
            assert_eq!(
                split(input),
                vec![opaque(inner), simple("rm -rf x")],
                "{input:?}"
            );
        }
    }

    #[test]
    fn nested_blocks_yield_inner_commands() {
        assert_eq!(
            split("for i in *; do if [ -f $i ]; then echo $i; fi; done && echo finished"),
            vec![
                opaque("[ -f $i ]"),
                simple("echo $i"),
                simple("echo finished")
            ]
        );
    }

    #[test]
    fn malformed_block_collapses_to_single_opaque() {
        for input in [
            "for i in 1 2 3; do echo $i",
            "echo a; fi",
            "{ echo a;",
            "echo a; }",
        ] {
            assert_eq!(split(input), vec![opaque(input)], "{input:?}");
        }
    }

    #[test]
    fn reserved_words_as_arguments_are_literal() {
        // `echo for; echo done` is two echo commands, not a loop.
        assert_eq!(
            split("echo for; echo done }"),
            vec![simple("echo for"), simple("echo done }")]
        );
    }

    #[test]
    fn negation_is_grammar_and_jumps_are_commands() {
        assert_eq!(split("! grep -q x f"), vec![simple("grep -q x f")]);
        // `break` can name a function: `break() ( … ); break` runs it.
        assert_eq!(
            split("break() ( rm -rf x ); break > f"),
            vec![simple("rm -rf x"), opaque("break > f")]
        );
    }

    #[test]
    fn carriage_return_is_part_of_a_word() {
        // Bash neither ends a command nor starts a comment at a lone `\r`.
        assert_eq!(
            split(">\rbreak rm -rf y"),
            vec![opaque(">\rbreak rm -rf y")]
        );
        assert_eq!(
            split("echo x\r# ; rm -rf y"),
            vec![simple("echo x\r#"), simple("rm -rf y")]
        );
    }

    #[test]
    fn header_may_break_lines_before_its_word_list() {
        assert_eq!(
            split("case x\nin x) rm -rf y;; esac"),
            vec![simple("rm -rf y")]
        );
        assert_eq!(split("for f\ndo rm $f; done"), vec![simple("rm $f")]);
        assert_eq!(
            split("for f\nin a b\ndo rm $f; done"),
            vec![simple("rm $f")]
        );
    }

    #[test]
    fn brace_word_heading_a_command_is_opaque() {
        // `{rm,-rf,x}` is brace expansion that runs `rm -rf x`.
        assert_eq!(split("{rm,-rf,x}"), vec![opaque("{rm,-rf,x}")]);
    }

    // -----------------------------------------------------------------
    // Functions: the body is checked, the header is not a command
    // -----------------------------------------------------------------

    #[test]
    fn function_body_and_calls_are_commands() {
        assert_eq!(
            split("run(){ echo \"== $1\"; go test ./... | head -4; }\nrun a; run b | tail"),
            vec![
                simple("echo \"== $1\""),
                simple("go test ./..."),
                simple("head -4"),
                simple("run a"),
                simple("run b"),
                simple("tail")
            ]
        );
    }

    #[test]
    fn function_definition_forms() {
        let body = vec![simple("echo a"), simple("rm -rf x"), simple("f")];
        for input in [
            "f() { echo a; rm -rf x; }; f",
            "f () {\n  echo a\n  rm -rf x\n}\nf",
            "f()\n{ echo a; rm -rf x; }; f",
            "function f { echo a; rm -rf x; }; f",
            "function f() ( echo a; rm -rf x ); f",
        ] {
            assert_eq!(split(input), body, "{input:?}");
        }
    }

    #[test]
    fn call_to_function_not_defined_here_is_a_command() {
        // A function from a sourced file has a body nobody has seen.
        assert_eq!(
            split("source ./env.sh && run x"),
            vec![opaque("source ./env.sh"), simple("run x")]
        );
        // Called before its definition, `run` is whatever it was before.
        assert_eq!(
            split("run x; run(){ echo a; }"),
            vec![simple("run x"), simple("echo a")]
        );
    }

    #[test]
    fn call_runs_what_its_name_and_arguments_say() {
        // The body seen in the definition is not always what runs.
        for (input, call) in [
            ("rm(){ :; }; unset -f rm; rm -rf ~", simple("rm -rf ~")),
            ("f(){ \"$@\"; }; f rm -rf x", simple("f rm -rf x")),
            ("{rm,-rf,y}(){ :; }; {rm,-rf,y}", opaque("{rm,-rf,y}")),
            ("false && f(){ echo a; }; f", simple("f")),
        ] {
            let segs = split(input);
            assert_eq!(segs.last(), Some(&call), "{input:?} → {segs:?}");
        }
    }

    #[test]
    fn owner_command_yields_only_real_commands() {
        // The command from the bug report: a function body split at its
        // `;` produced `run(){ echo "== $1"` and a bare `}` as segments.
        // Now the body's commands and the calls are segments, the header
        // and braces are not, and the heredoc body stays with `python3`.
        let input = concat!(
            "cd /w && source ./.goenv.sh && M=$PWD/.trash/rev/mut && mkdir -p $M && ",
            "(cd cloud-devstack && git archive e34c080) | tar -x -C $M && cd $M && ",
            "cp internal/docker/diagnose.go /tmp/d.go 2>/dev/null; cp internal/docker/diagnose.go ../d.orig;\n",
            "run(){ echo \"== $1\"; go test ./internal/docker/ ./internal/ops/ -run 'Diagnose|FixFor' 2>&1 ",
            "| grep -E \"^(ok|FAIL|---)\" | head -4; cp ../d.orig internal/docker/diagnose.go; }\n",
            "sed -i 's/ || strings.Contains(lower, \"cannot connect\")//' internal/docker/diagnose.go; run \"M1 drop\"\n",
            "sed -i 's/x//' internal/docker/diagnose.go; python3 - <<'E'\n",
            "p='internal/docker/diagnose.go'; s=open(p).read()\n",
            "s=s.replace('\\tif err != nil {\\n','\\tif err != nil {\\n',1); open(p,'w').write(s)\n",
            "E\n",
            "run \"M2 drop ctx check\"\n",
        );
        assert_eq!(
            split(input),
            vec![
                simple("cd /w"),
                opaque("source ./.goenv.sh"),
                simple("M=$PWD/.trash/rev/mut"),
                simple("mkdir -p $M"),
                simple("cd cloud-devstack"),
                simple("git archive e34c080"),
                simple("tar -x -C $M"),
                simple("cd $M"),
                opaque("cp internal/docker/diagnose.go /tmp/d.go 2>/dev/null"),
                simple("cp internal/docker/diagnose.go ../d.orig"),
                simple("echo \"== $1\""),
                opaque("go test ./internal/docker/ ./internal/ops/ -run 'Diagnose|FixFor' 2>&1"),
                simple("grep -E \"^(ok|FAIL|---)\""),
                simple("head -4"),
                simple("cp ../d.orig internal/docker/diagnose.go"),
                simple("sed -i 's/ || strings.Contains(lower, \"cannot connect\")//' internal/docker/diagnose.go"),
                simple("run \"M1 drop\""),
                simple("sed -i 's/x//' internal/docker/diagnose.go"),
                opaque(concat!(
                    "python3 - <<'E'\n",
                    "p='internal/docker/diagnose.go'; s=open(p).read()\n",
                    "s=s.replace('\\tif err != nil {\\n','\\tif err != nil {\\n',1); open(p,'w').write(s)\n",
                    "E",
                )),
                simple("run \"M2 drop ctx check\""),
            ]
        );
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
    fn backslash_crlf_is_not_a_continuation() {
        // Bash escapes the `\r` and ends the line at `\n`.
        assert_eq!(
            split("echo hi \\\r\nrm -rf x"),
            vec![simple("echo hi \\"), simple("rm -rf x")]
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
