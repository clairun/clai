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
//! [`Segment::Unparsed`]. So does input that names a variable bash expands
//! as code (`PS4`), or a command whose name is not a plain word (`\rm`,
//! `"$cmd"`).
//!
//! A command whose argument is code (`bash -c`, `eval`, `trap`, …) is named
//! by the words it is written with; code inside a quoted argument or a
//! script file (`source x.sh`) is not looked into. A wrapper that only
//! changes how a command runs still names the command it runs (`timeout 600
//! cargo build` runs `cargo build`), so its row keeps that command: the
//! policy can allowlist it, and the blocklist sees it. The blocklist also
//! reads the command written in the arguments of `xargs`, `exec`, `eval` and
//! `watch` ([`argument_command_words`]), and every command a misread option
//! could hide ([`possible_commands`]); their rows stay named by their head.
//!
//! A quoted heredoc body is text. Bash expands an unquoted one: the commands
//! its `$(…)`, backticks and `${…}` run become rows of their own, each
//! `$(…)` and `${…}` read as it is outside a heredoc, and the rest stays
//! text with the row.
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
/// `cargo build`). `None` when a modifier wrapper is run by path or its own
/// arguments cannot be told from the command, read as `reading` says.
pub fn command_words(row: &str, reading: Reading) -> Option<Vec<&str>> {
    Some(effective_words(&row_words(row)?, reading)?.to_vec())
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

/// A command that runs the command written after it.
struct Wrapper {
    /// The wrapper's name, as it is written.
    name: &'static str,
    /// The short options that take no value.
    flag_letters: &'static str,
    /// The short options that take a value: the rest of their word, or the
    /// next word when they end it (`-n` in `xargs -0n 1`).
    value_letters: &'static str,
    /// The short options whose value is optional, so it can only be the
    /// rest of their word (GNU `xargs -i[R]`).
    optional_value_letters: &'static str,
    /// The long options whose value is the next word, unless written
    /// `--name=value`.
    value_long: &'static [&'static str],
    /// The other long options: they never take the next word, and take a
    /// value only after `=` (GNU `xargs --max-lines[=N]`).
    flag_long: &'static [&'static str],
    /// The options, by letter or long name, whose value changes the command
    /// the wrapper runs.
    roles: &'static [(&'static str, Role)],
    /// The plain arguments the wrapper takes before the command
    /// (`timeout`'s duration, `taskset`'s mask).
    plain_args: usize,
    /// Whether a number before the command is its priority, as `chrt` reads
    /// one: when a word follows it, and then an optional `--`.
    priority: bool,
}

/// What the value of a [`Wrapper::roles`] option is to the command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    /// A string the runner replaces with its input wherever it occurs in
    /// the command's words (`xargs -I R`).
    Replace,
    /// The first words of the command (`env -S 'cmd args'`).
    Command,
}

impl Wrapper {
    const fn named(name: &'static str) -> Self {
        Wrapper {
            name,
            flag_letters: "",
            value_letters: "",
            optional_value_letters: "",
            value_long: &[],
            flag_long: &[],
            roles: &[],
            plain_args: 0,
            priority: false,
        }
    }

    fn role(&self, option: &str) -> Option<Role> {
        let (_, role) = self.roles.iter().find(|(name, _)| *name == option)?;
        Some(*role)
    }
}

/// Wrappers that only change how a command runs: the row names the command
/// they run (`timeout 600 cargo build` runs `cargo build`), so the policy can
/// allowlist it and the blocklist sees it.
const MODIFIER_WRAPPERS: &[Wrapper] = &[
    Wrapper {
        flag_letters: "fpv",
        value_letters: "sk",
        value_long: &["--signal", "--kill-after"],
        flag_long: &["--foreground", "--preserve-status", "--verbose"],
        plain_args: 1,
        ..Wrapper::named("timeout")
    },
    Wrapper {
        // The shell keyword's only option.
        flag_letters: "p",
        ..Wrapper::named("time")
    },
    Wrapper {
        // `nice -10` is the old form of `nice -n 10`.
        flag_letters: "0123456789",
        value_letters: "n",
        value_long: &["--adjustment"],
        ..Wrapper::named("nice")
    },
    Wrapper::named("nohup"),
    Wrapper {
        // `-c` and `-p` change how the mask, a plain argument, is read.
        flag_letters: "acp",
        flag_long: &["--all-tasks", "--cpu-list", "--pid"],
        plain_args: 1,
        ..Wrapper::named("taskset")
    },
    Wrapper {
        value_letters: "ioe",
        value_long: &["--input", "--output", "--error"],
        ..Wrapper::named("stdbuf")
    },
    Wrapper {
        flag_letters: "t",
        value_letters: "cnpPu",
        value_long: &["--class", "--classdata", "--pid", "--pgid", "--uid"],
        flag_long: &["--ignore"],
        ..Wrapper::named("ionice")
    },
    Wrapper {
        flag_letters: "abdefGimOopRrv",
        value_letters: "TPDUX",
        value_long: &[
            "--sched-runtime",
            "--sched-period",
            "--sched-deadline",
            "--clamp-min",
            "--clamp-max",
        ],
        flag_long: &[
            "--batch",
            "--deadline",
            "--deadline-overrun",
            "--ext",
            "--fifo",
            "--idle",
            "--other",
            "--rr",
            "--reset-on-fork",
            "--reclaim-grub",
            "--all-tasks",
            "--max",
            "--pid",
            "--verbose",
        ],
        priority: true,
        ..Wrapper::named("chrt")
    },
    Wrapper {
        flag_letters: "cfw",
        flag_long: &["--ctty", "--fork", "--wait"],
        ..Wrapper::named("setsid")
    },
    Wrapper {
        flag_letters: "pVv",
        ..Wrapper::named("command")
    },
    Wrapper::named("builtin"),
    Wrapper {
        flag_letters: "0iv",
        value_letters: "auCS",
        value_long: &[
            "--argv0",
            "--unset",
            "--chdir",
            "--env0-from",
            "--split-string",
        ],
        flag_long: &[
            "--ignore-environment",
            "--null",
            "--block-signal",
            "--default-signal",
            "--ignore-signal",
            "--list-signal-handling",
            "--debug",
        ],
        roles: &[("S", Role::Command), ("--split-string", Role::Command)],
        ..Wrapper::named("env")
    },
];

/// How [`wrapped_start`] reads an option its table does not list as
/// written: getopt takes any unambiguous abbreviation (`xargs --process`
/// is `--process-slot-var`), and an implementation may have options the
/// table lacks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reading {
    /// The command cannot be told: allow matching reads words this way,
    /// so a grant never covers a command found past a misread option, or
    /// one an option's value starts (`env -S`).
    Exact,
    /// As getopt would with the table: an abbreviation of one listed
    /// option is that option, anything else takes no value. The splitter
    /// and the blocklist read words this way.
    Getopt,
}

/// The command the words name, looking through the wrappers that only change
/// how it runs: `timeout 600 cargo build` names `cargo build`. A lookup
/// (`command -v rm`) names itself. `None` when a wrapper is run by path or
/// its own arguments cannot be told from the command.
pub fn effective_words<'w, 'a>(words: &'w [&'a str], reading: Reading) -> Option<&'w [&'a str]> {
    let mut words = words;
    loop {
        let (name, rest) = words.split_first()?;
        let base = name.rsplit('/').next().unwrap_or(name);
        let Some(wrapper) = MODIFIER_WRAPPERS.iter().find(|w| w.name == base) else {
            return Some(words);
        };
        if name.contains('/') {
            // A wrapper run by path: its own arguments are not the ones the
            // table describes, so the command it runs is not ours to name.
            return None;
        }
        if lookup_option(words).is_some() {
            return Some(words);
        }
        words = rest.get(wrapped_start(wrapper, rest, reading)?.at..)?;
    }
}

/// Where the command a wrapper runs starts in the words after it, and the
/// replace strings its options set.
struct Start<'a> {
    at: usize,
    replace: Vec<&'a str>,
}

/// Where the command `wrapper` runs starts in `rest`, the words after it:
/// past the wrapper's own options, read as getopt reads them, the plain
/// arguments it takes before the command, and the assignments `env` takes.
/// `None` under [`Reading::Exact`] when the table does not list an option
/// as written, or an option's value starts the command.
fn wrapped_start<'a>(wrapper: &Wrapper, rest: &[&'a str], reading: Reading) -> Option<Start<'a>> {
    let mut at = 0;
    let mut replace = Vec::new();
    while let Some(&arg) = rest.get(at) {
        if arg == "--" {
            at += 1;
            break;
        }
        if arg == "-" && wrapper.name == "env" {
            // `env -` is `env -i`.
            at += 1;
            continue;
        }
        if !arg.starts_with('-') || arg == "-" {
            break;
        }
        let option = option_word(wrapper, arg, reading)?;
        let value = if option.takes_next {
            rest.get(at + 1).copied()
        } else {
            option.value
        };
        match option.role {
            Some(Role::Replace) => replace.push(value.unwrap_or("{}")),
            Some(Role::Command) if reading == Reading::Exact => return None,
            _ => {}
        }
        at += 1 + usize::from(option.takes_next);
    }
    for _ in 0..wrapper.plain_args {
        let arg = *rest.get(at)?;
        if arg.starts_with('-') && arg != "-" {
            return None;
        }
        at += 1;
    }
    if wrapper.priority {
        if rest.get(at).is_some_and(|arg| is_number(arg)) && at + 1 < rest.len() {
            at += 1;
        }
        if rest.get(at) == Some(&"--") {
            at += 1;
        }
    }
    while rest.get(at).is_some_and(|arg| is_assignment(arg)) {
        at += 1;
    }
    Some(Start { at, replace })
}

/// An option word, as getopt reads it.
struct OptionWord<'a> {
    /// Whether its value is the next word.
    takes_next: bool,
    /// Its value, when written in the word (`-n1`, `--max-args=1`).
    value: Option<&'a str>,
    role: Option<Role>,
}

/// `arg`, an option word of `wrapper`, as getopt reads it: the letters of a
/// short-option cluster until one that takes a value, whose value is the
/// rest of the cluster, or the next word when none is left; a long option
/// by its name. `None` under [`Reading::Exact`] when the table does not
/// list the option as written.
fn option_word<'a>(wrapper: &Wrapper, arg: &'a str, reading: Reading) -> Option<OptionWord<'a>> {
    let flag = OptionWord {
        takes_next: false,
        value: None,
        role: None,
    };
    if arg.starts_with("--") {
        let (name, value) = match arg.split_once('=') {
            Some((name, value)) => (name, Some(value)),
            None => (arg, None),
        };
        let Some((name, takes_value)) = long_option(wrapper, name, reading) else {
            // getopt rejects it, so it takes nothing.
            return (reading == Reading::Getopt).then_some(OptionWord { value, ..flag });
        };
        return Some(OptionWord {
            takes_next: takes_value && value.is_none(),
            value,
            role: wrapper.role(name),
        });
    }
    let cluster = arg.strip_prefix('-')?;
    for (at, letter) in cluster.char_indices() {
        let optional = wrapper.optional_value_letters.contains(letter);
        if optional || wrapper.value_letters.contains(letter) {
            let (letter, value) = cluster[at..].split_at(letter.len_utf8());
            return Some(OptionWord {
                takes_next: !optional && value.is_empty(),
                value: (!value.is_empty()).then_some(value),
                role: wrapper.role(letter),
            });
        }
        if reading == Reading::Exact && !wrapper.flag_letters.contains(letter) {
            return None;
        }
    }
    Some(flag)
}

/// The long option of `wrapper` that `name` names, and whether it takes a
/// value: `name` itself, or under [`Reading::Getopt`] the one option it
/// abbreviates.
fn long_option(wrapper: &Wrapper, name: &str, reading: Reading) -> Option<(&'static str, bool)> {
    let value = wrapper.value_long.iter().map(|option| (*option, true));
    let flag = wrapper.flag_long.iter().map(|option| (*option, false));
    let options = value.chain(flag);
    if let Some(option) = options.clone().find(|(option, _)| *option == name) {
        return Some(option);
    }
    if reading == Reading::Exact {
        return None;
    }
    let mut abbreviated = options.filter(|(option, _)| option.starts_with(name));
    match (abbreviated.next(), abbreviated.next()) {
        (Some(option), None) => Some(option),
        _ => None,
    }
}

/// A word of digits, as `chrt` reads a priority.
fn is_number(word: &str) -> bool {
    !word.is_empty() && word.bytes().all(|b| b.is_ascii_digit())
}

/// The short-option letters that make a wrapper look a command up instead
/// of running it: `command -v rm` prints where `rm` is.
const LOOKUP_OPTIONS: &[(&str, &[char])] = &[("command", &['v', 'V'])];

/// The option that makes `words` a lookup of the command they name rather
/// than a run of it: `-v` in `command -v rm`.
pub fn lookup_option<'a>(words: &[&'a str]) -> Option<&'a str> {
    let (name, rest) = words.split_first()?;
    let (_, letters) = LOOKUP_OPTIONS.iter().find(|(n, _)| n == name)?;
    rest.iter()
        .take_while(|arg| arg.starts_with('-') && !matches!(**arg, "-" | "--"))
        .find(|arg| !arg.starts_with("--") && arg.contains(*letters))
        .copied()
}

/// Commands that run the command written in their arguments (`xargs rm`,
/// `exec rm`, `eval rm`, `watch rm`) but whose row names them: they also run
/// without a command (`exec >log 2>&1`, bare `xargs`), and `eval` and `watch`
/// read their arguments as code. The blocklist looks through all of them, in
/// [`argument_command_words`]; allow prefixes only through the ones that run
/// the command directly ([`argument_run`]).
const ARGUMENT_RUNNERS: &[Wrapper] = &[
    Wrapper {
        flag_letters: "cl",
        value_letters: "a",
        ..Wrapper::named("exec")
    },
    Wrapper::named("eval"),
    Wrapper {
        // GNU's options, and BSD's `-J`, `-R` and `-S`.
        flag_letters: "0oprtx",
        value_letters: "adEILnPsJRS",
        optional_value_letters: "eil",
        value_long: &[
            "--arg-file",
            "--delimiter",
            "--max-args",
            "--max-procs",
            "--max-chars",
            "--process-slot-var",
        ],
        flag_long: &[
            "--null",
            "--eof",
            "--replace",
            "--max-lines",
            "--open-tty",
            "--interactive",
            "--no-run-if-empty",
            "--show-limits",
            "--verbose",
            "--exit",
        ],
        roles: &[
            ("I", Role::Replace),
            ("i", Role::Replace),
            ("J", Role::Replace),
            ("--replace", Role::Replace),
        ],
        ..Wrapper::named("xargs")
    },
    Wrapper {
        flag_letters: "bcCefgprtwx",
        value_letters: "nqs",
        optional_value_letters: "d",
        value_long: &["--interval", "--equexit", "--shotsdir"],
        flag_long: &[
            "--beep",
            "--color",
            "--no-color",
            "--differences",
            "--errexit",
            "--follow",
            "--chgexit",
            "--precise",
            "--no-rerun",
            "--no-title",
            "--no-wrap",
            "--exec",
        ],
        ..Wrapper::named("watch")
    },
];

/// What the [`ARGUMENT_RUNNERS`] head of some words runs.
pub struct ArgumentRun<'w, 'a> {
    /// The runner reads its arguments as shell code (`eval`, and `watch`,
    /// which passes them to `sh -c`), so they name no single command.
    pub reads_code: bool,
    /// The words after the runner's own options, as written: `grep foo` in
    /// `xargs -n 1 grep foo`. Empty when they name no command (`exec >log
    /// 2>&1`, bare `xargs`); `None` when the runner's options, read as
    /// [`argument_run`] was asked to, do not tell where they start.
    pub command: Option<&'w [&'a str]>,
    /// The replace strings the runner's options set (`R` in `xargs -I R`).
    pub replace: Vec<&'a str>,
}

impl<'w, 'a> ArgumentRun<'w, 'a> {
    /// The words of [`Self::command`] the runner's input cannot change:
    /// with a replace string, those before the first word it may occur in
    /// (`git` in `xargs -I push git push`). `None` when that leaves no word.
    pub fn literal_command(&self) -> Option<&'w [&'a str]> {
        let command = self.command?;
        if self.replace.is_empty() {
            return Some(command);
        }
        let end = command
            .iter()
            .position(|word| {
                self.replace
                    .iter()
                    .any(|replace| may_contain(word, replace))
            })
            .unwrap_or(command.len());
        (end > 0).then_some(&command[..end])
    }
}

/// Whether `word` may hold `pattern` once the shell expands both: quotes
/// are read away, and an expansion may hold anything.
fn may_contain(word: &str, pattern: &str) -> bool {
    let literal =
        |text: &str| (!text.contains(['$', '`'])).then(|| text.replace(['\'', '"', '\\'], ""));
    match (literal(word), literal(pattern)) {
        (Some(word), Some(pattern)) => word.contains(&pattern),
        _ => true,
    }
}

/// The [`ArgumentRun`] of `words`, or `None` when they are not headed by an
/// [`ARGUMENT_RUNNERS`] entry.
pub fn argument_run<'w, 'a>(words: &'w [&'a str], reading: Reading) -> Option<ArgumentRun<'w, 'a>> {
    let (name, rest) = words.split_first()?;
    let base = name.rsplit('/').next().unwrap_or(name);
    let runner = ARGUMENT_RUNNERS.iter().find(|r| r.name == base)?;
    let start = wrapped_start(runner, rest, reading);
    Some(ArgumentRun {
        reads_code: matches!(runner.name, "eval" | "watch"),
        command: start
            .as_ref()
            .map(|start| rest.get(start.at..).unwrap_or(&[])),
        replace: start.map(|start| start.replace).unwrap_or_default(),
    })
}

/// The command an [`ARGUMENT_RUNNERS`] head runs from its arguments, looking
/// through further runners and modifier wrappers: `xargs -0 timeout 5 rm`
/// → `rm`. `None` when `row` is not headed by one, or its arguments name no
/// command (`exec >log 2>&1`).
pub fn argument_command_words(row: &str) -> Option<Vec<&str>> {
    let row = row_words(row)?;
    let mut words = effective_words(&row, Reading::Getopt)?;
    let mut ran = false;
    while let Some(run) = argument_run(words, Reading::Getopt) {
        words = effective_words(run.command?, Reading::Getopt)?;
        ran = true;
    }
    ran.then(|| words.to_vec())
}

/// The most words of a command the blocklist reads when the command was
/// found under a reading other than getopt's: more than any blocked prefix
/// and the options before its subcommand, and few enough that a row read
/// many ways costs time linear in its words.
const POSSIBLE_COMMAND_WORDS: usize = 32;

/// The commands `words` may run, under every reading of the options of the
/// wrappers and runners heading them: the word after an option that may
/// take a value, by the table or by its absence from it, is also read as
/// the start of the command (`rm` in `xargs -0I rm echo`). A wrapper or
/// runner is given by its name, any other command by its first
/// [`POSSIBLE_COMMAND_WORDS`] words. Deny matching only: every reading can
/// only block more.
pub fn possible_commands<'w, 'a>(words: &'w [&'a str]) -> Vec<&'w [&'a str]> {
    const { assert!(MODIFIER_WRAPPERS.len() + ARGUMENT_RUNNERS.len() <= 32) };
    let wrappers: Vec<&Wrapper> = MODIFIER_WRAPPERS.iter().chain(ARGUMENT_RUNNERS).collect();
    // Each start is read once, and each word as an option of each wrapper
    // once, so the readings cost time linear in the words.
    let mut started = vec![false; words.len()];
    let mut read_as_option = vec![0u32; words.len()];
    let mut starts = vec![0];
    let mut commands = Vec::new();
    while let Some(start) = starts.pop() {
        if start >= words.len() || std::mem::replace(&mut started[start], true) {
            continue;
        }
        let command = &words[start..];
        let base = command[0].rsplit('/').next().unwrap_or(command[0]);
        let Some((index, wrapper)) = wrappers.iter().enumerate().find(|(_, w)| w.name == base)
        else {
            commands.push(&command[..command.len().min(POSSIBLE_COMMAND_WORDS)]);
            continue;
        };
        commands.push(&command[..1]);
        if lookup_option(command).is_some() {
            continue;
        }
        let wrapper_bit = 1 << index;
        // Whether the word at `at`, and the word after it, may be read as
        // an option of `wrapper` or as its first positional word.
        let (mut here, mut next) = (true, false);
        for at in start + 1..words.len() {
            if !here && !next {
                break;
            }
            let word = words[at];
            let is_option =
                word.starts_with('-') && word != "--" && (word != "-" || wrapper.name == "env");
            (here, next) = if !here || (is_option && read_as_option[at] & wrapper_bit != 0) {
                (next, false)
            } else if is_option {
                read_as_option[at] |= wrapper_bit;
                let may_take_value = option_word(wrapper, word, Reading::Exact)
                    .is_none_or(|option| option.takes_next);
                (true, may_take_value)
            } else {
                let positional = at + usize::from(word == "--");
                push_command_starts(wrapper, words, positional, &mut starts);
                (next, false)
            };
        }
    }
    commands
}

/// Pushes to `starts` where the command `wrapper` runs may start, when its
/// positional words start at `at`: past its plain arguments and
/// assignments, and with or without a priority.
fn push_command_starts(wrapper: &Wrapper, words: &[&str], at: usize, starts: &mut Vec<usize>) {
    let mut at = at + wrapper.plain_args;
    while words.get(at).is_some_and(|word| is_assignment(word)) {
        at += 1;
    }
    starts.push(at);
    let may_be_number =
        |word: &str| word.contains(['$', '`']) || is_number(&word.replace(['\'', '"'], ""));
    if wrapper.priority && words.get(at).is_some_and(|word| may_be_number(word)) {
        starts.push(at + 1);
        if words.get(at + 1) == Some(&"--") {
            starts.push(at + 2);
        }
    }
}

/// `words` with each string `env -S` may split read as the words it splits
/// into, wherever it is written: `env -S'rm -rf' x` → `env rm -rf x`, as
/// `env` reads on from them. Deny matching only: a word read as a split
/// string by mistake only adds commands to check.
pub fn with_split_strings<'a>(words: &[&'a str]) -> Vec<&'a str> {
    let mut out = Vec::with_capacity(words.len());
    // The words still to read, last first: each with whether it is a split
    // string, and whether it is known to hold no quote or blank, so each
    // character is read a bounded number of times.
    let mut todo: Vec<(&'a str, bool, bool)> = words
        .iter()
        .rev()
        .map(|word| (*word, false, false))
        .collect();
    while let Some((word, is_split, plain)) = todo.pop() {
        if is_split {
            if plain {
                todo.push((word, false, true));
            } else {
                let pieces =
                    word.split(|c: char| c.is_whitespace() || matches!(c, '\'' | '"' | '\\'));
                todo.extend(
                    pieces
                        .filter(|piece| !piece.is_empty())
                        .rev()
                        .map(|piece| (piece, false, true)),
                );
            }
            continue;
        }
        match split_string_option(word) {
            Some(Some(value)) => todo.push((value, true, plain)),
            Some(None) => {
                if let Some((value, _, plain)) = todo.pop() {
                    todo.push((value, true, plain));
                }
            }
            None => out.push(word),
        }
    }
    out
}

/// The value of `word` read as an `env -S` option: `Some(None)` when it is
/// the next word, `None` when `word` is no such option.
fn split_string_option(word: &str) -> Option<Option<&str>> {
    let word = word.trim_start_matches(['\'', '"']);
    let value = if let Some(long) = word.strip_prefix("--") {
        let (name, value) = match long.split_once('=') {
            Some((name, value)) => (name, Some(value)),
            None => (long, None),
        };
        if name.is_empty() || !"split-string".starts_with(name) {
            return None;
        }
        value
    } else {
        Some(word.strip_prefix('-')?.split_once('S')?.1)
    };
    Some(value.filter(|value| !value.trim_matches(['\'', '"']).is_empty()))
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
            if word.kind() != "word" || !is_plain_name(text) {
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
        let row_command = command_words(&text, Reading::Getopt)?;
        let node_words = command_node_words(self.src, node)?;
        let node_command = effective_words(&node_words, Reading::Getopt)?;
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
                    if moved && !text.is_empty() || !quoted && text.contains("\\\n") {
                        return None;
                    }
                    // A quoted delimiter keeps the body literal; an
                    // unquoted one is walked for the commands it expands.
                    if !quoted {
                        self.heredoc_body_walk(child)?;
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

    /// An unquoted heredoc body. Bash expands its `$(…)`, backtick and `${…}`
    /// text, so each command those run becomes a row of its own; the rest of
    /// the body is text kept with the row.
    fn heredoc_body_walk(&mut self, body: Node) -> Option<()> {
        let mut expansions = Vec::new();
        Self::expansions(body, &mut expansions);
        let mut expansions = expansions.into_iter().peekable();
        let bytes = self.src.as_bytes();
        let mut at = body.start_byte();
        while at < body.end_byte() {
            // Tree-sitter does not parse backticks or escapes in a heredoc
            // body, so an expansion it found inside either is text.
            while expansions.next_if(|e| e.start_byte() < at).is_some() {}
            if let Some(expansion) = expansions.next_if(|e| e.start_byte() == at) {
                self.heredoc_expansion(expansion)?;
                at = expansion.end_byte();
                continue;
            }
            match bytes[at] {
                b'\\' => at += 2,
                b'`' => {
                    let (code, len) = backtick_code(&self.src[at + 1..body.end_byte()])?;
                    self.out.extend(split(&code)?);
                    at += len + 2;
                }
                _ => at += 1,
            }
        }
        Some(())
    }

    /// The `$(…)` and `${…}` nodes tree-sitter parsed in `node`, outermost
    /// first.
    fn expansions<'tree>(node: Node<'tree>, out: &mut Vec<Node<'tree>>) {
        for child in Self::children(node) {
            if matches!(child.kind(), "command_substitution" | "expansion") {
                out.push(child);
            } else {
                Self::expansions(child, out);
            }
        }
    }

    /// A `$(…)` or `${…}` in a heredoc body, read as it is outside one.
    /// Tree-sitter parses less inside a heredoc body: it reads every `$((`
    /// there as a subshell, where bash reads arithmetic first, and leaves
    /// backticks and some `${…}` operands unparsed.
    fn heredoc_expansion(&mut self, node: Node) -> Option<()> {
        let segments = split(&format!(": {}", self.text(node)))?;
        // The first row is the `:` that holds the expansion.
        self.out.extend(segments.into_iter().skip(1));
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

/// The command in a backtick substitution, from `rest`, the text after its
/// opening backtick: with the backslashes bash removes inside it (before
/// `` ` ``, `$` and `\`), and the length of `rest` up to the closing
/// backtick. `None` when it is not closed.
fn backtick_code(rest: &str) -> Option<(String, usize)> {
    let mut code = String::new();
    let mut chars = rest.char_indices();
    while let Some((i, c)) = chars.next() {
        match c {
            '`' => return Some((code, i)),
            '\\' => {
                let (_, escaped) = chars.next()?;
                if !matches!(escaped, '`' | '$' | '\\') {
                    code.push(c);
                }
                code.push(escaped);
            }
            _ => code.push(c),
        }
    }
    None
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
    fn modifier_wrappers_run_by_path_are_unparsed() {
        // A modifier wrapper run by path cannot be told from the command
        // it wraps, so the input is asked as its exact text. A command
        // whose argument is code is named even when run by path.
        for input in [
            "/usr/bin/env touch PWN",
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
    // A command whose argument is code names the head it runs directly
    // -----------------------------------------------------------------

    #[test]
    fn commands_whose_argument_is_code_are_named() {
        // The words name the command bash runs directly; the code inside
        // the argument is not looked into.
        for input in [
            "bash -c \"rm -rf /\"",
            "sh -c \"echo hi\"",
            "eval \"$cmd\"",
            "watch -n 1 'ls'",
            "exec touch PWN",
            "source ./setup.sh",
            ". ./setup.sh",
            "xargs rm",
            "/bin/sh -c 'touch PWN'",
        ] {
            assert_eq!(split(input), vec![simple(input)], "{input:?}");
        }
        assert_eq!(
            split("find . -name '*.tmp' | xargs rm"),
            vec![simple("find . -name '*.tmp'"), simple("xargs rm")]
        );
        assert_eq!(
            split("/usr/bin/xargs touch <<< PWN"),
            vec![opaque("/usr/bin/xargs touch <<< PWN")]
        );
        // `command` and `builtin` write the command they run out, so the
        // row looks through them to it (an Opaque row, like the `timeout`
        // rows).
        assert_eq!(
            split("command touch PWN"),
            vec![opaque("command touch PWN")]
        );
        assert_eq!(split("builtin cd /x"), vec![opaque("builtin cd /x")]);
        // A command whose name is not a plain word still cannot be named.
        for input in ["\\touch PWN", "\"touch\" PWN"] {
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
                command_words(input, Reading::Getopt).map(|words| words.join(" ")),
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
        // `source` names the script it runs; the file's contents are not
        // looked into. The function body splits at its `;` into real
        // commands, the header and braces are not segments, and the
        // heredoc body stays with `python3`.
        assert_eq!(
            split(input),
            vec![
                simple("cd /w"),
                simple("source ./.goenv.sh"),
                simple("M=$PWD/.trash/rev/mut"),
                simple("mkdir -p $M"),
                simple("cd cloud-devstack"),
                simple("git archive e34c080"),
                simple("tar -x -C $M"),
                simple("cd $M"),
                opaque("cp internal/docker/diagnose.go /tmp/d.go 2>/dev/null"),
                simple("cp internal/docker/diagnose.go ../d.orig"),
                simple("echo \"== $1\""),
                opaque("go test ./internal/docker/ ./internal/ops/ -run \'Diagnose|FixFor\' 2>&1"),
                simple("grep -E \"^(ok|FAIL|---)\""),
                simple("head -4"),
                simple("cp ../d.orig internal/docker/diagnose.go"),
                simple("sed -i \'s/ || strings.Contains(lower, \"cannot connect\")//\' internal/docker/diagnose.go"),
                simple("run \"M1 drop\""),
                simple("sed -i \'s/x//\' internal/docker/diagnose.go"),
                opaque("python3 - <<\'E\'\np=\'internal/docker/diagnose.go\'; s=open(p).read()\ns=s.replace(\'\\tif err != nil {\\n\',\'\\tif err != nil {\\n\',1); open(p,\'w\').write(s)\nE"),
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

    // -----------------------------------------------------------------
    // Unquoted heredoc bodies name every command bash expands
    // -----------------------------------------------------------------

    #[test]
    fn unquoted_heredoc_body_names_nested_substitutions() {
        let input = "cat <<EOF\n${x:-$(rm -rf ~)}\nEOF";
        assert_eq!(split(input), vec![opaque(input), simple("rm -rf ~")]);
    }

    #[test]
    fn unquoted_heredoc_body_names_backtick_commands() {
        let input = "cat <<EOF > notes.md\nuse `ls` here\nEOF";
        assert_eq!(split(input), vec![opaque(input), simple("ls")]);
        let input = "cat <<EOF\n${x:-$(rm a)} `pwd`\nEOF";
        assert_eq!(
            split(input),
            vec![opaque(input), simple("rm a"), simple("pwd")]
        );
        // A `${…}` is read as it is outside a heredoc.
        for input in ["cat <<EOF\n${x:-`rm a`}\nEOF", "echo ${x:-`rm a`}"] {
            assert_eq!(split(input), vec![unparsed(input)], "{input:?}");
        }
        // Inside backticks, an escaped backtick nests another substitution.
        let input = "cat <<EOF\n`echo \\`ls\\``\nEOF";
        assert_eq!(
            split(input),
            vec![opaque(input), opaque("echo `ls`"), simple("ls")]
        );
        // An escaped backtick or `$` is text.
        for input in ["cat <<EOF\na \\`b\\` c\nEOF", "cat <<EOF\na \\$(rm x)\nEOF"] {
            assert_eq!(split(input), vec![opaque(input)], "{input:?}");
        }
    }

    #[test]
    fn quoted_heredoc_body_names_nothing() {
        let input = "cat <<'EOF'\n`rm x` $(rm y)\nEOF";
        assert_eq!(split(input), vec![opaque(input)]);
    }

    #[test]
    fn heredoc_arithmetic_is_read_as_it_is_outside_a_heredoc() {
        // Tree-sitter reads every `$((` in a heredoc body as a subshell;
        // bash reads arithmetic first, as the splitter does elsewhere.
        let input = "cat <<EOF\n$((1+2))\nEOF";
        assert_eq!(split(input), vec![opaque(input)]);
        for input in [
            "cat <<EOF\n$((i))\nEOF",
            "cat <<EOF\n$((rm x) )\nEOF",
            "echo $((i))",
            "echo $((rm x) )",
        ] {
            assert_eq!(split(input), vec![unparsed(input)], "{input:?}");
        }
    }

    #[test]
    fn argument_runners_stay_named_rows() {
        for input in ["exec >log 2>&1", "xargs grep foo", "watch -n 2 ls"] {
            assert!(
                !matches!(split(input).as_slice(), [Segment::Unparsed(_)]),
                "{input}"
            );
        }
        assert_eq!(
            argument_command_words("find_x | xargs -I {} -n 1 rm -f {}"),
            None
        );
        assert_eq!(
            argument_command_words("xargs -0 -I {} -n 1 rm -f {}"),
            Some(vec!["rm", "-f", "{}"])
        );
        assert_eq!(
            argument_command_words("watch -d -n 2 timeout 5 rm x"),
            Some(vec!["rm", "x"])
        );
        assert_eq!(argument_command_words("exec >log 2>&1"), None);
        assert_eq!(argument_command_words("ls -la"), None);
    }
}
