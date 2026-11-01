//! Splits a shell command string into the commands it will run.
//!
//! The input is parsed with tree-sitter-bash. Every command the shell would
//! execute becomes one [`Segment`]: the commands joined by `|`, `|&`, `&&`,
//! `||`, `;`, `&` or a newline, the commands inside subshells, brace groups,
//! function bodies and `if`/`while`/`until`/`for`/`case` blocks, and the
//! commands inside command and process substitutions, which become rows of
//! their own next to the command that holds them.
//!
//! Shell grammar is not a command: braces, reserved words, function headers,
//! `for`/`case` headers and `case` patterns. Redirects on a block are kept as
//! an Opaque segment, because they may write any file.
//!
//! Only node kinds and tokens this module names are accepted, and a node
//! accepted whole must hold nothing tree-sitter nested in it and no `$` it
//! did not parse (`Walker::leaf`). A parse error, anything else, or a
//! construct whose effect depends on runtime values bash may evaluate as code
//! (arithmetic on variables, indirect expansion) makes the whole input one
//! [`Segment::Unparsed`]. So does a command whose argument is a command or a
//! string of code that is not written out (`xargs`, `bash -c`, `trap`, …),
//! input that names a variable bash expands as code (`PS4`), or a command
//! whose name is not a plain word (`\rm`, `"$cmd"`).
//!
//! A wrapper that only changes how a command runs still names the command it
//! runs (`timeout 600 cargo build` runs `cargo build`), so its row keeps that
//! command: the policy can allowlist it, and the blocklist sees it.
//!
//! - [`Segment::Simple`] — a plain command.
//! - [`Segment::Opaque`] — a command with a redirect, heredoc, here-string,
//!   command or process substitution, or a `[`/`[[` test.
//! - [`Segment::Unparsed`] — the whole input when it cannot be split
//!   reliably. The policy layer decides it as one row of exact text.

use tree_sitter::Node;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Segment {
    Simple(String),
    Opaque(String),
    /// The whole input when shell grammar could not be parsed reliably.
    Unparsed(String),
}

impl Segment {
    pub fn text(&self) -> &str {
        match self {
            Segment::Simple(s) | Segment::Opaque(s) | Segment::Unparsed(s) => s,
        }
    }

    pub fn is_opaque(&self) -> bool {
        matches!(self, Segment::Opaque(_) | Segment::Unparsed(_))
    }
}

/// True for a word that is shell grammar rather than a command name.
pub fn is_shell_syntax(word: &str) -> bool {
    const RESERVED: &[&str] = &[
        "if", "then", "elif", "else", "fi", "while", "until", "for", "select", "in", "do", "done",
        "case", "esac", "function", "{", "}", "!", "[[", "]]",
    ];
    RESERVED.contains(&word) || word.ends_with("()") || word.ends_with("(){")
}

/// True if `text` runs a command of its own through a command or process
/// substitution.
pub fn has_substitution(text: &str) -> bool {
    ["$(", "`", "<(", ">("].iter().any(|s| text.contains(s))
}

/// Splits a shell command into [`Segment`]s, in source order.
pub fn split_command(input: &str) -> Vec<Segment> {
    split(input).unwrap_or_else(|| vec![Segment::Unparsed(input.trim().to_string())])
}

fn split(input: &str) -> Option<Vec<Segment>> {
    if !lexes_like_bash(input) || names_code_variable(input) {
        return None;
    }
    let tree = parse(input)?;
    let root = tree.root_node();
    let mut walker = Walker {
        src: input,
        out: Vec::new(),
    };
    walker.node(root)?;
    Some(walker.out)
}

fn parse(input: &str) -> Option<tree_sitter::Tree> {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_bash::LANGUAGE.into())
        .ok()?;
    let tree = parser.parse(input, None)?;
    (!tree.root_node().has_error()).then_some(tree)
}

/// The words of `row` when it holds one command: its name and arguments as
/// written, without leading assignments, redirects or heredoc bodies
/// (`A=$'\'' /bin/rm -rf x > f` → `/bin/rm`, `-rf`, `x`).
pub fn row_words(row: &str) -> Option<Vec<&str>> {
    let tree = parse(row)?;
    let root = tree.root_node();
    let mut cursor = root.walk();
    let [mut node] = root.named_children(&mut cursor).collect::<Vec<_>>()[..] else {
        return None;
    };
    if node.kind() == "redirected_statement" {
        node = node.child_by_field_name("body")?;
    }
    command_node_words(row, node)
}

/// The command `row` runs: the words of the command itself, looking through
/// the wrappers that only change how it runs (`timeout 600 cargo build` →
/// `cargo build`). `None` when the command is not written out (`bash -c
/// 'rm -rf /'`) or when the wrapper's own arguments cannot be told from the
/// command.
pub fn command_words(row: &str) -> Option<Vec<&str>> {
    effective_words(&row_words(row)?)
}

/// The name and arguments of a command node, as written in `src`.
fn command_node_words<'src>(src: &'src str, node: Node) -> Option<Vec<&'src str>> {
    let mut cursor = node.walk();
    let words: Vec<Node> = match node.kind() {
        "command" => std::iter::once(node.child_by_field_name("name")?)
            .chain(node.children_by_field_name("argument", &mut cursor))
            .collect(),
        "declaration_command" | "unset_command" => node.children(&mut cursor).collect(),
        _ => return None,
    };
    Some(words.iter().map(|n| &src[n.byte_range()]).collect())
}

/// Commands whose argument is a command or a string of code that is not
/// written out, so the command they run cannot be named. Matched on the last
/// part of the command's path.
const CODE_WRAPPERS: &[&str] = &[
    "bash",
    "sh",
    "zsh",
    "dash",
    "ksh",
    "fish",
    "xargs",
    "eval",
    "watch",
    "exec",
    "source",
    ".",
    "command",
    "builtin",
    "coproc",
    "trap",
    "alias",
    "let",
    "mapfile",
    "readarray",
    "compgen",
    "complete",
    "hash",
    "enable",
    "fc",
];

/// A wrapper that only changes how a command runs: the command it runs is
/// written out after it, so the policy can still name it.
struct ModifierWrapper {
    /// The wrapper's name, as it is written.
    name: &'static str,
    /// The options whose value is the next argument.
    value_options: &'static [&'static str],
    /// The plain arguments the wrapper takes before the command
    /// (`timeout`'s duration).
    plain_args: usize,
}

const MODIFIER_WRAPPERS: &[ModifierWrapper] = &[
    ModifierWrapper {
        name: "timeout",
        value_options: &["-s", "--signal", "-k", "--kill-after"],
        plain_args: 1,
    },
    ModifierWrapper {
        name: "time",
        value_options: &[],
        plain_args: 0,
    },
    ModifierWrapper {
        name: "nice",
        value_options: &["-n", "--adjustment"],
        plain_args: 0,
    },
    ModifierWrapper {
        name: "nohup",
        value_options: &[],
        plain_args: 0,
    },
    ModifierWrapper {
        name: "taskset",
        value_options: &["-c", "--cpu-list", "-p", "--pid"],
        plain_args: 0,
    },
    ModifierWrapper {
        name: "stdbuf",
        value_options: &["-i", "--input", "-o", "--output", "-e", "--error"],
        plain_args: 0,
    },
    ModifierWrapper {
        name: "ionice",
        value_options: &["-c", "--class", "-n", "--classdata"],
        plain_args: 0,
    },
    ModifierWrapper {
        name: "chrt",
        value_options: &["--rr"],
        plain_args: 0,
    },
    ModifierWrapper {
        name: "setsid",
        value_options: &[],
        plain_args: 0,
    },
    ModifierWrapper {
        name: "env",
        value_options: &["-u", "--unset", "-C", "--chdir"],
        plain_args: 0,
    },
];

/// The command the words name, looking through the wrappers that only change
/// how it runs: `timeout 600 cargo build` names `cargo build`. `None` when
/// the words name a command that is not written out (`bash -c 'rm -rf /'`),
/// or when the wrapper's own arguments cannot be told from the command.
fn effective_words<'a>(words: &[&'a str]) -> Option<Vec<&'a str>> {
    let (name, rest) = words.split_first()?;
    let base = name.rsplit('/').next().unwrap_or(name);
    if CODE_WRAPPERS.contains(&base) {
        return None;
    }
    let Some(wrapper) = MODIFIER_WRAPPERS.iter().find(|w| w.name == base) else {
        return Some(words.to_vec());
    };
    if name.contains('/') {
        // A wrapper run by path: its own arguments are not the ones the
        // table describes, so the command it runs is not ours to name.
        return None;
    }
    // The wrapper's own options come first.
    let mut at = 0;
    while let Some(arg) = rest.get(at) {
        if *arg == "--" {
            at += 1;
            break;
        }
        if !arg.starts_with('-') || *arg == "-" {
            break;
        }
        if arg.contains('=') {
            at += 1;
        } else if wrapper.value_options.contains(arg) {
            at += 2;
        } else {
            at += 1;
        }
    }
    // Then the arguments the wrapper takes before the command.
    for _ in 0..wrapper.plain_args {
        let arg = *rest.get(at)?;
        if arg.starts_with('-') && arg != "-" {
            return None;
        }
        at += 1;
    }
    // `env` takes assignments before the command.
    while rest.get(at).is_some_and(|arg| is_assignment(arg)) {
        at += 1;
    }
    effective_words(rest.get(at..)?)
}

/// A `NAME=value` token, as `env` takes before the command it runs.
fn is_assignment(token: &str) -> bool {
    let Some(eq) = token.find('=') else {
        return false;
    };
    if eq == 0 {
        return false;
    }
    let mut chars = token[..eq].chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_alphabetic() && first != '_' {
        return false;
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Variables whose value bash, or a bash it starts, expands as code.
const CODE_VARIABLES: &[&str] = &["PS4", "PROMPT_COMMAND", "BASH_ENV", "ENV"];

/// True if `input` names a [`CODE_VARIABLES`] entry anywhere, with quotes and
/// backslashes ignored, since any word may be or build an assignment to it
/// (`export 'PS4=$(cmd)'`, `PS4[0]=…`, `read PS4`).
fn names_code_variable(input: &str) -> bool {
    let unquoted: String = input
        .chars()
        .filter(|c| !matches!(c, '\'' | '"' | '\\'))
        .collect();
    unquoted
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .any(|name| CODE_VARIABLES.contains(&name))
}

/// Tokens that only sequence or group commands.
const GRAMMAR_TOKENS: &[&str] = &[
    "&&", "||", "|", "|&", ";", "&", "\n", "(", ")", "{", "}", "!", "if", "then", "elif", "else",
    "fi", "while", "until", "for", "select", "in", "do", "done", "case", "esac", ";;", "function",
];

/// Operators of `${…}` that read a variable without evaluating its value.
const EXPANSION_TOKENS: &[&str] = &[
    "${", "}", "#", "##", "%", "%%", "/", "//", "/#", "/%", ":-", "-", ":=", "=", ":+", "+", ":?",
    "?", "^", "^^", ",", ",,", "[", "]",
];

/// Operators that compare their operands as arithmetic, evaluating a
/// variable's value as an expression (`a[$(cmd)]` runs `cmd`).
const ARITHMETIC_TESTS: &[&str] = &["-eq", "-ne", "-lt", "-le", "-gt", "-ge", "-v"];

struct Walker<'src> {
    src: &'src str,
    out: Vec<Segment>,
}

impl<'src> Walker<'src> {
    fn text(&self, node: Node) -> &'src str {
        &self.src[node.byte_range()]
    }

    fn children<'tree>(node: Node<'tree>) -> Vec<Node<'tree>> {
        let mut cursor = node.walk();
        node.children(&mut cursor).collect()
    }

    /// Walks a node. `None` when it holds anything not proven understood.
    fn node(&mut self, node: Node) -> Option<()> {
        match node.kind() {
            "comment" => self.starts_word(node.start_byte()).then_some(()),
            "program"
            | "list"
            | "pipeline"
            | "subshell"
            | "compound_statement"
            | "negated_command"
            | "do_group"
            | "if_statement"
            | "elif_clause"
            | "else_clause"
            | "while_statement"
            | "for_statement"
            | "case_statement"
            | "case_item"
            | "function_definition" => self.grammar(node),
            "command" | "declaration_command" | "unset_command" => self.command(node, node),
            "variable_assignment" | "variable_assignments" => {
                self.word(node)?;
                self.out
                    .push(Segment::Simple(self.row_text(node, node.start_byte())));
                Some(())
            }
            "test_command" => self.test(node),
            "redirected_statement" => self.redirected(node),
            _ => self.word(node),
        }
    }

    /// Grammar that only sequences commands: walk what it holds.
    fn grammar(&mut self, node: Node) -> Option<()> {
        if node.kind() == "compound_statement" && self.text(node).starts_with("((") {
            return self.arithmetic(node);
        }
        for child in Self::children(node) {
            if child.is_named() {
                self.node(child)?;
            } else if !GRAMMAR_TOKENS.contains(&child.kind()) {
                return None;
            }
        }
        Some(())
    }

    /// A command; `row` is the node whose text the user approves.
    fn command(&mut self, node: Node, row: Node) -> Option<()> {
        let mut wrapped = false;
        if node.kind() == "command" {
            let name = node.child_by_field_name("name")?;
            let word = name.named_child(0)?;
            let text = self.text(word);
            let basename = text.rsplit('/').next().unwrap_or(text);
            if word.kind() != "word" || !is_plain_name(text) || CODE_WRAPPERS.contains(&basename) {
                return None;
            }
            // A wrapper runs the command written after it: the row is opaque
            // and the command it runs is the one the policy names.
            wrapped = MODIFIER_WRAPPERS.iter().any(|w| w.name == basename);
            // A redirect before the name would head the row, hiding the
            // command from prefix rules: `2>&1 rm -rf x`.
            let mut cursor = node.walk();
            if node
                .children(&mut cursor)
                .take_while(|c| *c != name)
                .any(|c| c.kind().ends_with("redirect"))
            {
                return None;
            }
        }
        let start = self.out.len();
        let opaque = self.words(node)? || wrapped;
        self.out.insert(start, Segment::Simple(String::new()));
        let text = self.row_text(row, node.start_byte());
        // Policy reads the command from the row text alone, so it must name
        // the same command.
        let row_command = command_words(&text)?;
        let node_command = effective_words(&command_node_words(self.src, node)?)?;
        if row_command != node_command {
            return None;
        }
        self.out[start] = if opaque || row != node {
            Segment::Opaque(text)
        } else {
            Segment::Simple(text)
        };
        Some(())
    }

    /// `[ … ]` or `[[ … ]]`, always Opaque.
    fn test(&mut self, node: Node) -> Option<()> {
        let mut stack = vec![node];
        while let Some(n) = stack.pop() {
            if n.kind() == "binary_expression" || n.kind() == "unary_expression" {
                let op = n.child_by_field_name("operator").map(|o| self.text(o));
                if op.is_some_and(|op| ARITHMETIC_TESTS.contains(&op)) {
                    for operand in Self::children(n).into_iter().filter(|c| c.is_named()) {
                        if operand.kind() != "test_operator" && !self.is_constant(operand) {
                            return None;
                        }
                    }
                }
            }
            stack.extend(Self::children(n));
        }
        let start = self.out.len();
        self.parts(node, &["[", "]", "[[", "]]"])?;
        self.out
            .insert(start, Segment::Opaque(self.text(node).trim().to_string()));
        Some(())
    }

    /// A command or block with redirects.
    fn redirected(&mut self, node: Node) -> Option<()> {
        // Tree-sitter reads the words after a leading redirect as its target
        // (`> f touch x`), where bash runs them.
        let body = node.child_by_field_name("body")?;
        self.redirected_body(body, node)
    }

    /// `body` followed by the redirects of `node`. Tree-sitter hangs them on
    /// a whole list or pipeline, where bash binds them to its last command
    /// (`cd x && cat > f`).
    fn redirected_body(&mut self, body: Node, node: Node) -> Option<()> {
        match body.kind() {
            "command" => {
                self.command(body, node)?;
                self.redirects(node)
            }
            "list" | "pipeline" => {
                let children = Self::children(body);
                let (last, rest) = children.split_last()?;
                for child in rest {
                    if child.is_named() {
                        self.node(*child)?;
                    } else if !GRAMMAR_TOKENS.contains(&child.kind()) {
                        return None;
                    }
                }
                self.redirected_body(*last, node)
            }
            _ => {
                self.node(body)?;
                let start = self.out.len();
                self.redirects(node)?;
                let redirects = self.src[body.end_byte()..node.end_byte()]
                    .trim()
                    .to_string();
                self.out.insert(start, Segment::Opaque(redirects));
                Some(())
            }
        }
    }

    fn redirects(&mut self, node: Node) -> Option<()> {
        let body = node.child_by_field_name("body");
        for child in Self::children(node)
            .into_iter()
            .filter(|c| Some(*c) != body)
        {
            self.word(child)?;
        }
        Some(())
    }

    /// Walks the words of a command, pushing the commands they substitute.
    /// Returns true if any makes the command Opaque.
    fn words(&mut self, node: Node) -> Option<bool> {
        let mut opaque = false;
        for child in Self::children(node) {
            if child.is_named() {
                opaque |= matches!(
                    child.kind(),
                    "file_redirect" | "heredoc_redirect" | "herestring_redirect"
                );
                let before = self.out.len();
                self.word(child)?;
                opaque |= self.out.len() > before;
            } else if !matches!(
                child.kind(),
                "export"
                    | "declare"
                    | "local"
                    | "readonly"
                    | "typeset"
                    | "unset"
                    | "unsetenv"
                    | "="
                    | "+="
            ) {
                return None;
            }
        }
        Some(opaque)
    }

    /// A word, or part of one. Substituted commands are pushed as segments.
    fn word(&mut self, node: Node) -> Option<()> {
        match node.kind() {
            "word"
            | "number"
            | "raw_string"
            | "ansi_c_string"
            | "string_content"
            | "variable_name"
            | "special_variable_name"
            | "extglob_pattern"
            | "regex"
            | "test_operator"
            | "file_descriptor"
            | "heredoc_content"
            | "heredoc_end" => self.leaf(node),
            "command_name"
            | "string"
            | "concatenation"
            | "simple_expansion"
            | "array"
            | "variable_assignment"
            | "variable_assignments"
            | "file_redirect"
            | "herestring_redirect"
            | "binary_expression"
            | "unary_expression"
            | "parenthesized_expression"
            | "heredoc_body" => self.parts(
                node,
                &[
                    "\"", "$", "(", ")", "=", "+=", "!", "<", ">", ">>", "&>", "&>>", "<&", ">&",
                    ">|", "<>", "<<<", ">&-", "<&-", "==", "=~", "!=", "&&", "||", "-a", "-o",
                ],
            ),
            // Bash reads `$((` as arithmetic where tree-sitter may read a
            // subshell (`${x:-$((i))}`).
            "command_substitution" if self.text(node).starts_with("$((") => None,
            "command_substitution" | "process_substitution" => {
                for child in Self::children(node) {
                    if child.is_named() {
                        self.node(child)?;
                    } else if !matches!(child.kind(), "$(" | ")" | "`" | "<(" | ">(") {
                        return None;
                    }
                }
                Some(())
            }
            "expansion" => self.parts(node, EXPANSION_TOKENS),
            "subscript" => {
                let index = node.child_by_field_name("index")?;
                (self.is_constant(index) || matches!(self.text(index), "@" | "*")).then_some(())
            }
            "arithmetic_expansion" => self.arithmetic(node),
            "heredoc_redirect" => self.heredoc(node),
            _ => None,
        }
    }

    /// A node accepted whole: nothing is nested in it, and unless it is
    /// quoted literally its text holds no `$`, backtick, `<(` or `>(`, which
    /// tree-sitter would otherwise have parsed as an expansion of its own
    /// (`16#$(cmd)`, `${x#$(cmd)}`, `${x#${a[i]}}`, nested backticks).
    fn leaf(&self, node: Node) -> Option<()> {
        let literal = matches!(node.kind(), "raw_string" | "ansi_c_string");
        let text = self.text(node);
        (node.child_count() == 0 && (literal || !text.contains('$') && !has_substitution(text)))
            .then_some(())
    }

    fn parts(&mut self, node: Node, tokens: &[&str]) -> Option<()> {
        for child in Self::children(node) {
            if child.is_named() {
                if matches!(
                    child.kind(),
                    "pipeline" | "list" | "command" | "redirected_statement"
                ) {
                    self.node(child)?;
                } else {
                    self.word(child)?;
                }
            } else if !tokens.contains(&child.kind()) {
                return None;
            }
        }
        Some(())
    }

    /// `$(( … ))` or `(( … ))`: accepted only on constants, since bash
    /// evaluates a variable's value as an expression that may run commands.
    fn arithmetic(&mut self, node: Node) -> Option<()> {
        let constant = Self::children(node)
            .into_iter()
            .filter(|c| c.is_named())
            .all(|c| self.is_constant(c));
        if !constant {
            return None;
        }
        if node.kind() == "compound_statement" {
            self.out.push(Segment::Opaque(self.text(node).to_string()));
        }
        Some(())
    }

    /// Numbers and arithmetic on numbers.
    fn is_constant(&self, node: Node) -> bool {
        match node.kind() {
            "number" => self.leaf(node).is_some(),
            "word" => {
                self.leaf(node).is_some() && self.text(node).bytes().all(|b| b.is_ascii_digit())
            }
            "simple_expansion" => matches!(self.text(node), "$?" | "$#" | "$$" | "$!"),
            "binary_expression" | "unary_expression" | "parenthesized_expression" => {
                Self::children(node).into_iter().all(|c| {
                    if c.is_named() {
                        self.is_constant(c)
                    } else {
                        c.kind().bytes().all(|b| b"+-*/%()<>=!&|^~".contains(&b))
                    }
                })
            }
            _ => false,
        }
    }

    /// `<<DELIM`: only delimiters bash and tree-sitter read alike, and a
    /// body that ends at the first line bash would end it at.
    fn heredoc(&mut self, node: Node) -> Option<()> {
        let mut delim = None;
        let mut strip_tabs = false;
        let mut body_start = None;
        for child in Self::children(node) {
            match child.kind() {
                "<<" => {}
                "<<-" => strip_tabs = true,
                "heredoc_start" => {
                    delim = Some(heredoc_delimiter(self.text(child))?);
                    body_start =
                        Some(child.end_byte() + self.src[child.end_byte()..].find('\n')? + 1);
                }
                "heredoc_body" => {
                    let (_, quoted) = delim.as_ref()?;
                    let text = self.text(child);
                    // Tree-sitter starts the body after any blank lines.
                    let moved = self
                        .src
                        .get(body_start?..child.start_byte())
                        .is_none_or(|skipped| skipped.bytes().any(|b| b != b'\n'));
                    if moved && !text.is_empty()
                        || !quoted && (text.contains("\\\n") || text.contains('`'))
                    {
                        return None;
                    }
                    // A quoted delimiter keeps the body literal.
                    if !quoted {
                        self.word(child)?;
                    }
                }
                "heredoc_end" => {
                    let (word, _) = delim.as_ref()?;
                    let closes = |line: &str| {
                        let line = if strip_tabs {
                            line.trim_start_matches('\t')
                        } else {
                            line
                        };
                        line == word
                    };
                    let line_start = self.src[..child.start_byte()].rfind('\n')? + 1;
                    let line_end = self.src[child.end_byte()..]
                        .find('\n')
                        .map_or(self.src.len(), |i| child.end_byte() + i);
                    let body = self.src.get(body_start?..line_start)?;
                    if !closes(&self.src[line_start..line_end]) || body.lines().any(closes) {
                        return None;
                    }
                }
                "pipeline" | "list" | "command" | "redirected_statement" => self.node(child)?,
                "file_redirect" | "herestring_redirect" => self.word(child)?,
                "&&" | "||" | "|" | "|&" | ";" | "&" => {}
                _ => return None,
            }
        }
        Some(())
    }

    /// The text of `row` from `start`, without the commands tree-sitter
    /// nests in a heredoc redirect (`cat <<EOF | sh` keeps `sh` a row of its
    /// own).
    fn row_text(&self, row: Node, start: usize) -> String {
        let mut text = String::new();
        let mut at = start;
        let mut stack = vec![row];
        while let Some(n) = stack.pop() {
            if n.end_byte() <= start {
                continue;
            }
            if n.kind() == "heredoc_redirect" {
                for c in Self::children(n) {
                    if matches!(
                        c.kind(),
                        "pipeline"
                            | "list"
                            | "command"
                            | "redirected_statement"
                            | "&&"
                            | "||"
                            | "|"
                            | "|&"
                            | ";"
                            | "&"
                    ) && c.start_byte() >= at
                    {
                        text.push_str(&self.src[at..c.start_byte()]);
                        at = c.end_byte();
                    }
                }
            } else {
                stack.extend(Self::children(n).into_iter().rev());
            }
        }
        text.push_str(&self.src[at..row.end_byte()]);
        text.trim().to_string()
    }

    /// True if bash starts a word at `at`, so a `#` there is a comment.
    fn starts_word(&self, at: usize) -> bool {
        let before = &self.src.as_bytes()[..at];
        match before.last() {
            None | Some(b';' | b'&' | b'|' | b'(' | b')') => true,
            Some(b' ' | b'\t' | b'\n') => {
                let escapes = before[..before.len() - 1]
                    .iter()
                    .rev()
                    .take_while(|b| **b == b'\\')
                    .count();
                escapes % 2 == 0
            }
            Some(_) => false,
        }
    }
}

/// False for bytes tree-sitter-bash reads as blanks or line continuations
/// where bash does not, which would let a comment or a word hide a command:
/// control characters other than tab and newline (`echo x\r# ; rm -rf y`),
/// an escaped tab, and a line continuation not preceded by a blank.
fn lexes_like_bash(input: &str) -> bool {
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\t' | b'\n' => {}
            0..=0x1f | 0x7f => return false,
            b'\\' => {
                match bytes.get(i + 1) {
                    Some(b'\t' | 0..=0x08 | 0x0b..=0x1f | 0x7f) => return false,
                    Some(b'\n') => {
                        let blank = i > 0 && matches!(bytes[i - 1], b' ' | b'\t');
                        if !blank || (i > 1 && bytes[i - 2] == b'\\') {
                            return false;
                        }
                    }
                    _ => {}
                }
                i += 1;
            }
            _ => {}
        }
        i += 1;
    }
    true
}

/// A command name with no quoting, escaping, expansion or globbing.
fn is_plain_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-./:+@%,~=".contains(&b) || b >= 0x80)
}

/// A heredoc delimiter as bash and tree-sitter both read it: a word of
/// letters, digits and `_`, bare, quoted whole, or after one backslash.
/// Returns the word and whether it was quoted.
fn heredoc_delimiter(raw: &str) -> Option<(String, bool)> {
    let (word, quoted) = if let Some(w) = raw.strip_prefix('\\') {
        (w, true)
    } else if let Some(w) = raw.strip_prefix('\'').and_then(|w| w.strip_suffix('\'')) {
        (w, true)
    } else if let Some(w) = raw.strip_prefix('"').and_then(|w| w.strip_suffix('"')) {
        (w, true)
    } else {
        (raw, false)
    };
    let plain = !word.is_empty() && word.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_');
    plain.then(|| (word.to_string(), quoted))
}

#[cfg(test)]
mod bash_runs;

#[cfg(test)]
mod tests {
    use super::*;

    fn simple(s: &str) -> Segment {
        Segment::Simple(s.to_string())
    }

    fn opaque(s: &str) -> Segment {
        Segment::Opaque(s.to_string())
    }

    fn unparsed(s: &str) -> Segment {
        Segment::Unparsed(s.to_string())
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
    fn heredoc_without_a_body_is_unparsed() {
        assert_eq!(split("cat <<EOF"), vec![unparsed("cat <<EOF")]);
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
    fn wrappers_run_by_path_are_unparsed() {
        for input in [
            "/usr/bin/env touch PWN",
            "/bin/sh -c 'touch PWN'",
            "/usr/bin/xargs touch <<< PWN",
            "/usr/bin/timeout 2 touch PWN",
            "FOO=1 ./nice touch PWN",
        ] {
            assert_eq!(split(input), vec![unparsed(input)], "{input}");
        }
    }

    #[test]
    fn heredoc_body_may_start_with_blank_lines() {
        for input in ["cat >> f <<'EOF'\n\nhi\nEOF", "cat >> f <<EOF\n\n\nhi\nEOF"] {
            assert_eq!(split(input), vec![opaque(input)], "{input:?}");
        }
    }

    #[test]
    fn redirect_after_a_list_stays_with_its_last_command() {
        assert_eq!(
            split("cd x && cat > f"),
            vec![simple("cd x"), opaque("cat > f")]
        );
        assert_eq!(
            split("cd x && cat <<'EOF'\nhi\nEOF"),
            vec![simple("cd x"), opaque("cat <<'EOF'\nhi\nEOF")]
        );
        assert_eq!(
            split("a | b 2>&1 || c d >> log"),
            vec![simple("a"), opaque("b 2>&1"), opaque("c d >> log")]
        );
        assert_eq!(
            split("a && b > $(touch PWN)"),
            vec![simple("a"), opaque("b > $(touch PWN)"), simple("touch PWN")]
        );
        assert_eq!(
            split("a && { b; } > f"),
            vec![simple("a"), simple("b"), opaque("> f")]
        );
    }

    #[test]
    fn quoted_heredoc_body_runs_nothing() {
        assert_eq!(
            split("cat <<'EOF'\n$(rm x) `rm y`\nEOF"),
            vec![opaque("cat <<'EOF'\n$(rm x) `rm y`\nEOF")]
        );
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
        // The body starts on the next line; `| wc -l` on the operator's
        // line is a command of its own, not part of the heredoc.
        assert_eq!(
            split("cat <<EOF | wc -l\nrm -rf ~\nEOF\necho done"),
            vec![
                opaque("cat <<EOF \nrm -rf ~\nEOF"),
                simple("wc -l"),
                simple("echo done")
            ]
        );
    }

    #[test]
    fn heredoc_body_after_a_continued_or_glued_line_is_unparsed() {
        // A continuation, a newline in quotes, or an operator glued to the
        // delimiter: tree-sitter does not start the body where bash does.
        for input in [
            "cat <<EOF \\\n| wc -l\ntouch t\nEOF",
            "cat <<EOF|wc -l\ntouch t\nEOF",
            "cat <<EOF; echo \"a\nb\"\nx\nEOF\ntouch t",
        ] {
            assert_eq!(split(input), vec![unparsed(input)], "{input:?}");
        }
    }

    #[test]
    fn heredoc_delimiter_with_blanks_or_operators_is_unparsed() {
        for input in [
            "cat <<'E F'\nx\nE F\necho y #'",
            "cat <<\"E F\"\nx\nE F\necho y #\"",
            "cat <<'E;F'\nx\nE;F\necho y #'",
            "cat <<E\\ F\nx\nE F\necho y #'",
        ] {
            assert_eq!(split(input), vec![unparsed(input)], "{input:?}");
        }
    }

    #[test]
    fn heredoc_delimiter_closes_exactly() {
        assert_eq!(
            split("cat <<\\EOF\nx\nEOF\ntouch t"),
            vec![opaque("cat <<\\EOF\nx\nEOF"), simple("touch t")]
        );
        // A partly quoted delimiter, and `EOF ` (no closer for bash).
        for input in [
            "cat <<E\"OF\"\nx\nEOF\ntouch t",
            "cat <<EOF\nEOF \ntouch t\nEOF",
        ] {
            assert_eq!(split(input), vec![unparsed(input)], "{input:?}");
        }
    }

    #[test]
    fn two_heredocs_on_one_line_are_unparsed() {
        let input = "diff <<A <<B && echo same\na; b\nA\nb; c\nB\necho next";
        assert_eq!(split(input), vec![unparsed(input)]);
    }

    #[test]
    fn heredoc_chained_with_logical_and_does_not_overflow() {
        // `cd dir && cat > file << 'EOF' ... EOF` followed by another
        // command: the body lines are not commands. Tree-sitter attaches the
        // redirects to the list, so they are a row of their own.
        let input = "cd dir && cat > file << 'EOF'\n//! a\n//! b\nEOF\ncat file";
        assert_eq!(
            split(input),
            vec![
                simple("cd dir"),
                opaque("cat > file << 'EOF'\n//! a\n//! b\nEOF"),
                simple("cat file")
            ]
        );
    }

    // -----------------------------------------------------------------
    // Command substitution / subshell / grouping → Opaque
    // -----------------------------------------------------------------

    #[test]
    fn dollar_paren_substitution_is_opaque_and_its_commands_are_rows() {
        assert_eq!(
            split("echo $(date | tr a A)"),
            vec![
                opaque("echo $(date | tr a A)"),
                simple("date"),
                simple("tr a A")
            ]
        );
    }

    #[test]
    fn backtick_substitution_is_opaque_and_its_command_a_row() {
        assert_eq!(
            split("echo `date`"),
            vec![opaque("echo `date`"), simple("date")]
        );
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
            vec![simple("a"), simple("b"), opaque("> log")]
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
    fn process_substitution_is_opaque_and_its_commands_are_rows() {
        assert_eq!(
            split("diff <(a) <(b)"),
            vec![opaque("diff <(a) <(b)"), simple("a"), simple("b")]
        );
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
    fn command_that_runs_its_arguments_is_unparsed() {
        // The command these run is not written out, so the row cannot name
        // it and is asked as its exact text.
        for input in [
            r#"bash -c "rm -rf /""#,
            r#"sh -c "echo hi""#,
            "find . -name '*.tmp' | xargs rm",
            r#"eval "$cmd""#,
            "watch -n 1 'ls'",
            "command touch PWN",
            "exec touch PWN",
            "source ./setup.sh",
            ". ./setup.sh",
            "\\touch PWN",
            "\"touch\" PWN",
        ] {
            assert_eq!(split(input), vec![unparsed(input)], "{input:?}");
        }
    }

    #[test]
    fn wrapper_that_only_changes_how_a_command_runs_keeps_the_command() {
        // The command the wrapper runs is written out, so the row names it
        // and the policy can allowlist it.
        for (input, command) in [
            ("timeout 60 cargo test", "cargo test"),
            ("timeout --signal=KILL 60 cargo test", "cargo test"),
            ("timeout -s KILL 60 cargo test", "cargo test"),
            ("timeout 60 cargo test --lib", "cargo test --lib"),
            ("time make build", "make build"),
            ("nice -n 5 cargo build", "cargo build"),
            ("nohup cargo build", "cargo build"),
            ("env FOO=1 cargo build", "cargo build"),
            ("env -i cargo build", "cargo build"),
            ("taskset -c 0 cargo build", "cargo build"),
        ] {
            assert_eq!(split(input), vec![opaque(input)], "{input:?}");
            assert_eq!(
                command_words(input).map(|words| words.join(" ")),
                Some(command.to_string()),
                "{input:?}"
            );
        }
    }

    #[test]
    fn wrapper_without_a_command_written_out_stays_unparsed() {
        for input in ["timeout 60", "env", "env -i", "nice -n", "timeout --signal"] {
            assert_eq!(split(input), vec![unparsed(input)], "{input:?}");
        }
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
    fn for_header_substitution_is_a_row() {
        assert_eq!(
            split("for f in $(ls); do cat $f; done"),
            vec![simple("ls"), simple("cat $f")]
        );
    }

    #[test]
    fn for_without_word_list_ends_header_at_do() {
        // `for NAME do …` iterates "$@"; `rm` must not hide in the header.
        assert_eq!(split("for f\ndo rm $f; done"), vec![simple("rm $f")]);
        // Tree-sitter does not parse it on one line.
        let one_line = "for f do rm $f; done";
        assert_eq!(split(one_line), vec![unparsed(one_line)]);
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
    fn redirect_after_block_is_a_row() {
        // It applies to the whole block, and may write any file.
        assert_eq!(
            split("for i in 1 2; do echo $i; done 2>&1 | tail -1"),
            vec![simple("echo $i"), opaque("2>&1"), simple("tail -1")]
        );
        assert_eq!(
            split("if [ 1 ]; then x=1; fi > important"),
            vec![opaque("[ 1 ]"), simple("x=1"), opaque("> important")]
        );
        assert_eq!(
            split("while read l; do echo $l; done < <(git ls-files)"),
            vec![
                simple("read l"),
                simple("echo $l"),
                opaque("< <(git ls-files)"),
                simple("git ls-files")
            ]
        );
    }

    #[test]
    fn reserved_word_right_after_a_closer_is_read() {
        assert_eq!(
            split("case x in x) { a; } esac; rm x"),
            vec![simple("a"), simple("rm x")]
        );
        // After a redirect the word is its target, a file named `fi`.
        assert_eq!(
            split("for i in 1; do echo a; done > fi"),
            vec![simple("echo a"), opaque("> fi")]
        );
        // Tree-sitter does not parse these.
        for input in [
            "if { true; } then rm -rf ~; fi",
            "if (true)then rm -rf ~; fi",
            "while (true) do curl x | wc; done",
            "if a; then if b; then c; fi fi; rm x",
        ] {
            assert_eq!(split(input), vec![unparsed(input)], "{input:?}");
        }
    }

    #[test]
    fn case_inside_backticks_ends_at_the_closing_backtick() {
        assert_eq!(
            split("echo `case x in x) a;; esac`; rm -rf x"),
            vec![
                opaque("echo `case x in x) a;; esac`"),
                simple("a"),
                simple("rm -rf x")
            ]
        );
    }

    #[test]
    fn substitution_syntax_that_moves_its_end_hides_no_command() {
        // A hand lexer misplaces the end of these substitutions. Bash runs
        // the `touch PWN` or `rm -rf x` in each: it must head a row, or the
        // input must be Unparsed.
        for input in [
            "echo $(case x in x) a;; esac); rm -rf x",
            "echo \"$(case x in x) a;; esac)\"; rm -rf x",
            "cat <(case x in x) a;; esac); rm -rf x",
            "echo $(case a in a) echo;& b) echo;; esac); rm -rf x",
            "echo $(case a in @(a|b)) echo ok;; esac); rm -rf x",
            "echo $(echo case in a); rm -rf x",
            "echo $(ls >& case in a)\ntouch PWN\n;; esac)",
            "echo $(ls &> case in a)\ntouch PWN\n;; esac)",
            "echo $(ls &>> case in a)\ntouch PWN\n;; esac)",
            "echo $(ls >| case in a)\ntouch PWN\n;; esac)",
            "echo $(cat <& 0 case in a </dev/null)\ntouch PWN\n;; esac)",
            "echo \"$(ls >& case in a)\"\ntouch PWN\n;; esac)",
            "echo $(echo a # (\n) ; touch PWN # )",
            "echo $(cat <<E\n(\nE\n) ; touch PWN # )",
            "echo $(cat <<E\n(\nE\n) ; touch PWN ; cat <<F\n)\nF",
            "echo $(printf %s $'\\'') ; touch PWN ; echo \"')'\" \\'",
            "echo $(echo }) ; touch PWN ; echo ${x:-)}",
            "echo ${x:-)} ; touch PWN ; echo $(echo })",
            "echo $(echo {) ; touch PWN ; echo }",
            "echo ${x/(/y} ; touch PWN # )",
            "echo \"${x:-'\"'}\" ; touch PWN # \"",
            "echo $'\\'' ; touch PWN ; echo \\'",
        ] {
            let segs = split(input);
            let seen = segs == vec![unparsed(input)]
                || segs
                    .iter()
                    .any(|s| s.text() == "touch PWN" || s.text() == "rm -rf x");
            assert!(seen, "{input:?} → {segs:?}");
        }
        assert_eq!(
            split("echo ) ; touch PWN"),
            vec![unparsed("echo ) ; touch PWN")]
        );
    }

    #[test]
    fn heredoc_delimiter_bash_closes_elsewhere_is_unparsed() {
        // bash runs the `touch PWN` in each: an empty delimiter closes at the
        // first empty line, 0x01 and 0x7f do not match themselves, `<<-`
        // matches a quoted leading tab before stripping tabs, and a `\` in
        // double quotes escapes the next character.
        for input in [
            "cat <<''\n'\n\ntouch PWN\n'",
            "cat <<\"\"\n'\n\ntouch PWN\n'",
            "cat <<''\n\ntouch PWN",
            "cat <<'E\u{1}'\nE\u{1}\n'\nE\u{1}\u{1}\ntouch PWN\n'",
            "cat <<'E\u{7f}'\nE\u{7f}\n'\nE\u{1}\u{7f}\ntouch PWN\n'",
            "cat <<-\"\tE\"\n\tE\ntouch PWN",
            "cat <<-'\tE'\n\tE\ntouch PWN",
            "cat <<-\\\tE\n\tE\ntouch PWN",
            "cat <<\"E\\\\\"\nE\\\ntouch PWN\nE\\\\",
            "cat <<\"E\\\"F\"\nE\"F\ntouch PWN",
            "cat <<\"E\\$\"\nE$\ntouch PWN\nE\\$",
            "cat <<\"E\\`\"\nE`\ntouch PWN\nE\\`",
            "cat <<E\"\\\\\"\nE\\\ntouch PWN\nE\\\\",
            "cat <<-\"E\\\\\"\n\tE\\\ntouch PWN\nE\\\\",
            "[ x <<\"E\\\\\" ]\nE\\\ntouch PWN\nE\\\\",
        ] {
            assert_eq!(split(input), vec![unparsed(input)], "{input:?}");
        }
    }

    #[test]
    fn syntax_whose_end_bash_reads_differently_is_unparsed() {
        // bash runs the `touch PWN` in each: it reads a heredoc delimiter
        // holding `$`, a backtick or a line continuation differently, and
        // re-reads a `((` whose `)`s are not adjacent as subshells.
        for input in [
            "cat <<$'E'\nE\ntouch PWN",
            "cat <<$\"E\"\nE\ntouch PWN",
            "cat <<$\\\n'E'\nE\ntouch PWN",
            "cat <<$(x)\n$(x)\ntouch PWN\n$",
            "cat <<`a`\n`a`\ntouch PWN",
            "cat <<E\\\nX\nEX\\\n\ntouch PWN\nEX",
            "echo x\n((echo a) ; touch PWN )",
            "((touch PWN) )",
            "echo $((echo a) ; touch PWN )",
        ] {
            assert_eq!(split(input), vec![unparsed(input)], "{input:?}");
        }
        assert_eq!(
            split("(( (1+2) * 3 )) ; echo $((1+2))"),
            vec![opaque("(( (1+2) * 3 ))"), simple("echo $((1+2))")]
        );
    }

    #[test]
    fn unquoted_heredoc_with_a_continued_line_is_unparsed() {
        // Bash joins `\`-newline in an unquoted body before it looks for the
        // closer; tree-sitter does not. Its end of a quoted one differs too.
        for input in [
            "cat <<EOF > Dockerfile\nRUN apt-get update && \\\n    apt-get install -y curl\nEOF",
            "cat <<E\nE\\\n\ntouch PWN\nE",
            "cat <<-E\n\tE\\\n\ntouch PWN\nE",
            "cat <<-E\n\t\\\n\tE\ntouch PWN",
            "cat <<E\na\\\\\\\nE\ntouch PWN\nE",
            "cat <<E\nhi\\\nE",
            "cat <<E\nhi\\",
            "cat <<\"E\"\nE\\\n\ntouch PWN\nE",
        ] {
            assert_eq!(split(input), vec![unparsed(input)], "{input:?}");
        }
    }

    #[test]
    fn hash_after_an_escaped_blank_is_part_of_the_word() {
        assert_eq!(
            split("echo a\\ #; touch PWN"),
            vec![simple("echo a\\ #"), simple("touch PWN")]
        );
        assert_eq!(split("echo \\\\ #; touch PWN"), vec![simple("echo \\\\")]);
        // Tree-sitter reads these `#` as a comment, which bash does not.
        for input in ["echo a\\\t#; touch PWN", "echo a\\ \\\n#; touch PWN"] {
            assert_eq!(split(input), vec![unparsed(input)], "{input:?}");
        }
    }

    #[test]
    fn substitutions_without_such_syntax_still_split() {
        assert_eq!(
            split("echo $(echo [) ; touch PWN ; echo $(echo ])"),
            vec![
                opaque("echo $(echo [)"),
                simple("echo ["),
                simple("touch PWN"),
                opaque("echo $(echo ])"),
                simple("echo ]")
            ]
        );
        let words = r#"echo "${HOME}/x" ${a[0]} $((1+2)) <(ls) "$(echo ")")" $(f {} \;)"#;
        assert_eq!(
            split(&format!("{words}; ls")),
            vec![
                opaque(words),
                simple("ls"),
                simple(r#"echo ")""#),
                simple(r"f {} \;"),
                simple("ls")
            ]
        );
    }

    #[test]
    fn extglob_case_pattern_keeps_body_as_command() {
        assert_eq!(
            split("shopt -s extglob\ncase a in @(a|b)) rm -rf y;; esac"),
            vec![simple("shopt -s extglob"), simple("rm -rf y")]
        );
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
    fn malformed_block_collapses_to_single_unparsed() {
        for input in [
            "for i in 1 2 3; do echo $i",
            "{ echo a;",
            "echo a; }",
            "a ;;; b",
        ] {
            assert_eq!(split(input), vec![unparsed(input)], "{input:?}");
        }
        // Bash rejects this as a whole; tree-sitter reads `fi` as a command.
        assert_eq!(split("echo a; fi"), vec![simple("echo a"), simple("fi")]);
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
    fn carriage_return_is_unparsed() {
        // Bash reads a lone `\r` as part of a word; tree-sitter as a blank.
        for input in [
            ">\rbreak rm -rf y",
            "echo x\r# ; rm -rf y",
            "echo hi \\\r\nrm -rf x",
        ] {
            assert_eq!(split(input), vec![unparsed(input)], "{input:?}");
        }
    }

    #[test]
    fn header_may_break_lines_before_its_word_list() {
        assert_eq!(
            split("case x\nin x) rm -rf y;; esac"),
            vec![simple("rm -rf y")]
        );
        let for_in = "for f\nin a b\ndo rm $f; done";
        assert_eq!(split(for_in), vec![unparsed(for_in)]);
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
        // Called before its definition, `run` is whatever it was before.
        assert_eq!(
            split("run x; run(){ echo a; }"),
            vec![simple("run x"), simple("echo a")]
        );
    }

    #[test]
    fn call_runs_what_its_name_and_arguments_say() {
        // The body seen in the definition is not always what runs.
        assert_eq!(
            split("rm(){ :; }; unset -f rm; rm -rf ~").last(),
            Some(&simple("rm -rf ~"))
        );
        assert_eq!(
            split("false && f(){ echo a; }; f").last(),
            Some(&simple("f"))
        );
        // A name that is not a plain word: `"$@"` and brace expansion.
        for input in [
            "f(){ \"$@\"; }; f rm -rf x",
            "{rm,-rf,y}(){ :; }; {rm,-rf,y}",
            "{rm,-rf,x}",
        ] {
            assert_eq!(split(input), vec![unparsed(input)], "{input:?}");
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
        // `source` runs a file nobody has seen.
        assert_eq!(split(input), vec![unparsed(input.trim())]);
        assert_eq!(
            split(&input.replace("source ./.goenv.sh && ", "")),
            vec![
                simple("cd /w"),
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
    // Backslash-newline line continuation (Fix #1)
    // -----------------------------------------------------------------

    #[test]
    fn backslash_newline_continuation_joins_lines() {
        // One command; the row keeps the text as written.
        assert_eq!(
            split("git log \\\n  --oneline"),
            vec![simple("git log \\\n  --oneline")]
        );
        assert_eq!(
            split("echo a \\\necho b"),
            vec![simple("echo a \\\necho b")]
        );
        assert_eq!(split("echo \"a \\\nb\""), vec![simple("echo \"a \\\nb\"")]);
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
    fn unterminated_single_quote_collapses_to_single_unparsed() {
        let segs = split("echo 'oops ; ls");
        assert_eq!(segs, vec![unparsed("echo 'oops ; ls")]);
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
