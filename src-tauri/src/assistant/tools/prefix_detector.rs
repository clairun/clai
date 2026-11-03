//! Suggests a permission-allowlist prefix for a Simple shell segment.
//!
//! The goal is to pick a prefix that's neither too narrow (so the user
//! doesn't get re-prompted for every variation of the same command) nor
//! too broad (so allowlisting a verb doesn't approve unrelated actions).
//! For most CLIs, the meaningful permission boundary is the subcommand:
//! `git status` ≠ `git push`, `kubectl get` ≠ `kubectl delete`.
//!
//! Algorithm:
//!
//! 1. Take the command's words as tree-sitter-bash parses them
//!    ([`command_words`]).
//! 2. Leading `VAR=value` env assignments are not words, so they're skipped.
//! 3. Take the next token as the **binary**. If it's path-shaped
//!    (`./foo`, `/usr/bin/x`, `~/bin/y`), return it as-is.
//! 4. Look up the binary in the per-binary depth table to pick a style:
//!    `None`, `OneWord`, `TwoWord`, or `Unknown` (heuristic).
//! 5. Skip the global-option tokens between the binary and its first
//!    subword (starting with `-`, but not the bare `-` or `--` markers).
//!    For binaries with a [`GLOBAL_OPTION_SPECS`] entry, an option that
//!    takes a value (`git -C <dir>`) consumes its value word too, so the
//!    value is never mistaken for the subcommand.
//! 6. For `OneWord`: take the first non-flag token as the subword.
//!    For `TwoWord`: take that token *and* the immediately-following
//!    non-flag token, but only if no flag intervenes.
//!    For `Unknown`: take the first non-flag token only if it's
//!    subcommand-shaped (`^[a-z][a-z0-9_-]{0,24}$`).
//!    For `None`: ignore subwords entirely.
//!
//! Interpreters and the sourcing builtins (`bash`, `python3`, `.`, …) are
//! their own case: the prefix is the binary plus the word that names what
//! it runs ([`interpreter_prefix`]). A lookup (`command -v rm`) suggests
//! the lookup (`command -v`), never the command it names.
//!
//! The output is always a default; the modal pre-fills it into an editable
//! field so the user can narrow or broaden before confirming.

#![allow(dead_code)] // wired into the approval flow in commit 7

use crate::assistant::tools::command_splitter::{
    argument_command_words, command_words, lookup_option, row_words,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Style {
    /// Just the binary. Flat-argument commands like `ls`, `cat`, `grep`.
    None,
    /// Binary + one subword. `git status`, `docker ps`, `make build`.
    OneWord,
    /// Binary + two subwords with the given second-token policy.
    /// `kubectl get pods`, `aws s3 ls`, `gh pr create`.
    TwoWord(SecondTokenPolicy),
    /// Not in any table. Apply heuristic: include the first non-flag token
    /// if it's subcommand-shaped.
    Unknown,
}

/// When the binary uses `TwoWord` style, how do we decide whether the third
/// token (the candidate second subword) is a generic operation/resource vs.
/// a specific identifier?
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SecondTokenPolicy {
    /// Take the candidate if it matches subcommand-shape (allows hyphens).
    /// For `aws ec2 describe-instances`, `gh pr create`, `gcloud compute
    /// instances`, the hyphenated form is the operation, not an identifier.
    Permissive,
    /// Take the candidate only if it contains no hyphen. For `kubectl`,
    /// hyphens almost always signal a specific resource name (`my-pod`,
    /// `nginx-7f...`), while canonical resource types (`pods`, `nodes`,
    /// `deployments`) are bare lowercase plurals.
    NoHyphen,
}

// Commands that take flat arguments and have no meaningful subcommand layer.
// Allowlisting just the binary is the natural granularity.
const NO_SUBCOMMAND: &[&str] = &[
    // File/text utilities
    "ls",
    "cat",
    "less",
    "more",
    "head",
    "tail",
    "cp",
    "mv",
    "ln",
    "mkdir",
    "rmdir",
    "touch",
    "stat",
    "file",
    "wc",
    "sort",
    "uniq",
    "tr",
    "cut",
    "tee",
    "rev",
    "tac",
    "grep",
    "egrep",
    "fgrep",
    "rg",
    "ag",
    "ack",
    "find",
    "fd",
    "locate",
    "which",
    "whereis",
    "readlink",
    "realpath",
    "basename",
    "dirname",
    // System info
    "pwd",
    "echo",
    "printf",
    "date",
    "whoami",
    "hostname",
    "uname",
    "id",
    "uptime",
    "free",
    "df",
    "du",
    "ps",
    "kill",
    "killall",
    "pidof",
    "top",
    "htop",
    "nproc",
    // Test / trivial
    "true",
    "false",
    "sleep",
    "yes",
    "seq",
    "expr",
    // Network probes
    "ping",
    "traceroute",
    "host",
    "dig",
    "nslookup",
    "curl",
    "wget",
    "nc",
    // Diff / archive
    "diff",
    "patch",
    "tar",
    "gzip",
    "gunzip",
    "zip",
    "unzip",
    // Hashing / encoding
    "md5sum",
    "sha1sum",
    "sha256sum",
    "base64",
    "hexdump",
    "xxd",
];

// Commands where the first subword is the operation (one-level subcommand
// structure). Allowlisting `git status` vs `git push` is the natural axis.
const ONE_WORD: &[&str] = &[
    "git",
    "docker",
    "podman",
    "npm",
    "yarn",
    "pnpm",
    "cargo",
    "make",
    "go",
    "terraform",
    "tofu",
    "helm",
    "flatpak",
    "apt",
    "apt-get",
    "dpkg",
    "yum",
    "dnf",
    "pacman",
    "snap",
    "pip",
    "pip3",
    "pipx",
    "poetry",
    "uv",
    "systemctl",
    "journalctl",
    "brew",
    "rustup",
    "tsc",
    "ruff",
    "black",
    "mypy",
    "pytest",
];

// Commands where the second subword is meaningful but is usually a
// specific identifier when hyphenated (so we reject hyphenated second
// tokens to avoid allowlisting `kubectl logs my-pod`).
const TWO_WORD_NO_HYPHEN: &[&str] = &["kubectl"];

// Commands where hyphenated second subwords are legitimate operations
// (`aws ec2 describe-instances`, `gh pr create`, etc.).
const TWO_WORD_PERMISSIVE: &[&str] = &[
    "aws", "gcloud", "az", "gh", "glab", "doctl", "fly", "heroku", "vercel",
];

/// Returns a suggested allowlist prefix for the given Simple segment.
///
/// The segment must not be Opaque (this function is only meaningful for
/// segments that can be allowlisted).
pub fn suggest_prefix(segment: &str) -> String {
    // 1–2. The command's words, without leading env assignments. Anything
    // else (`> file`, `for f in …`) has no prefix to save.
    let Some(tokens) = command_words(segment) else {
        return String::new();
    };
    let binary = tokens[0];

    // A lookup names the lookup: `command rm` would also allow running `rm`.
    if let Some(option) = lookup_option(&tokens) {
        return format!("{binary} {option}");
    }
    if INTERPRETERS.contains(&binary) {
        return interpreter_prefix(binary, &tokens[1..]);
    }

    // 3. Path-shaped binaries are returned as-is (no subcommand layer).
    if is_path_shaped(binary) {
        return binary.to_string();
    }

    // A head that isn't a plausible command name has no meaningful
    // allowlist prefix: the `:` no-op builtin, or punctuation left over
    // from a malformed / over-collapsed segment (e.g. an unterminated
    // quote that swallowed the rest of the line). Returning an empty
    // string tells the modal to omit the "Save as prefix" / "Always allow"
    // affordances — the frontend gates them on a non-empty suggestedPrefix
    // — so the user only gets allow-once and isn't offered a bogus,
    // never-matching grant like `:`. Real command names start with a
    // letter, digit (`7z`), or underscore.
    let first = binary.chars().next().unwrap_or('\0');
    if !first.is_ascii_alphanumeric() && first != '_' {
        return String::new();
    }
    // Nor does a reserved word like `for` or `case`: allowlisting `for i`
    // would trust every loop over `i`, whatever its body runs.
    if crate::assistant::tools::command_splitter::is_shell_syntax(binary) {
        return String::new();
    }

    let style = lookup_style(binary);

    if matches!(style, Style::None) {
        return binary.to_string();
    }

    // 4. A config-injecting global option changes what the command can run.
    //    The allow matcher deliberately keeps it, so a subcommand-only
    //    suggestion would not authorize the command it came from. Offer
    //    allow-once instead of a persistent prefix that cannot round-trip.
    let sub = after_global_options(binary, &tokens[1..], OptionSkipping::All);
    if after_global_options(binary, &tokens[1..], OptionSkipping::AllButConfig).len() != sub.len() {
        return String::new();
    }
    let Some(&first_sub) = sub.first() else {
        return binary.to_string();
    };

    let include_first = match style {
        Style::OneWord | Style::TwoWord(_) => true,
        Style::Unknown => is_subcommand_shaped(first_sub),
        Style::None => false,
    };
    if !include_first {
        return binary.to_string();
    }

    if let Style::TwoWord(policy) = style {
        // 5. Second subword only if the very next token is also a non-flag
        //    and looks like a subcommand. No flags allowed in between.
        //    Policy decides whether hyphenated tokens are accepted.
        if let Some(second) = sub.get(1) {
            if !is_flag(second) && is_subcommand_shaped(second) {
                let hyphen_ok = match policy {
                    SecondTokenPolicy::Permissive => true,
                    SecondTokenPolicy::NoHyphen => !second.contains('-'),
                };
                if hyphen_ok {
                    return format!("{} {} {}", binary, first_sub, second);
                }
            }
        }
        return format!("{} {}", binary, first_sub);
    }

    format!("{} {}", binary, first_sub)
}

/// The words after `binary`, from its first non-option word onward: the
/// global options between the binary and its subcommand are dropped.
/// [`suggest_prefix`] and both canonical forms share this. Suggestion
/// declines commands where the allow form keeps a config-injecting option.
///
/// Value awareness is a lookup in [`GLOBAL_OPTION_SPECS`]: a listed
/// binary's documented value options (`git -C <dir>` above all) consume
/// their value word, which must never be taken for the subcommand
/// (`git -C repo status` is `git status`, never `git repo`). Every other
/// binary keeps the previous flag-token-only skipping — its option table
/// is unknown, so a value word cannot be told from a subcommand.
///
/// `skipping` splits the consumers (see [`OptionSkipping`]): subcommand
/// detection and deny matching drop every listed option with its value,
/// while allow matching stops at an option that injects configuration.
///
/// Dropping stops at the first non-option word: the table only lists
/// options documented to precede the subcommand (`git status -C .` is
/// rejected by git), while options after it belong to the subcommand's
/// own grammar (`git log -C 2` sets copy detection), so those stay
/// verbatim. Path-shaped binaries and flat-argument commands (Style::None)
/// keep every word: `suggest_prefix` returns their head alone, so no
/// option region exists.
fn after_global_options<'a>(
    binary: &str,
    rest: &'a [&'a str],
    skipping: OptionSkipping,
) -> &'a [&'a str] {
    if is_path_shaped(binary) || matches!(lookup_style(binary), Style::None) {
        return rest;
    }
    skip_global_options(value_options_for(binary), rest, skipping)
}

/// How much of the global-option region [`skip_global_options`] drops.
#[derive(Clone, Copy, PartialEq, Eq)]
enum OptionSkipping {
    /// Every listed value option leaves with its value. Subcommand
    /// detection needs the subcommand wherever it sits, and deny
    /// matching only grows stronger as the form gets shorter: both
    /// `git -C repo push` and `git -c e=v push` must read as `git push`
    /// to a `git push` block.
    All,
    /// Everything except an option whose value injects configuration:
    /// skipping stops there, keeping that option and every word after
    /// it. Allow matching runs on this form because configuration can
    /// name commands to execute (`core.fsmonitor`, `diff.external`), so
    /// a saved `git diff` must not silently cover
    /// `git -c diff.external=/bin/echo diff`.
    AllButConfig,
}

/// A binary's documented value-taking global option.
struct GlobalValueOption {
    /// The option as written between the binary and its subcommand
    /// (`-C`, `--git-dir`, …), covering both spellings when the long
    /// form also accepts `=`.
    name: &'static str,
    /// The option's value changes what this run can execute: it names
    /// configuration (`git -c <key>=<value>`) or the program to run
    /// (`python -m <module>`). It is kept out of allow matching
    /// (see [`OptionSkipping::AllButConfig`]): such an invocation can do more
    /// than the subcommand it ends in.
    injects_config: bool,
}

impl GlobalValueOption {
    /// Whether `token` is this option: the bare name (`--git-dir .git`)
    /// or the attached form long options also accept (`--git-dir=.git`).
    /// The `=` guard keeps `--git-dir2=x` a different, unknown option.
    fn matches_token(&self, token: &str) -> bool {
        token
            .strip_prefix(self.name)
            .is_some_and(|tail| tail.is_empty() || tail.starts_with('='))
    }
}

const fn option(name: &'static str, injects_config: bool) -> GlobalValueOption {
    GlobalValueOption {
        name,
        injects_config,
    }
}

/// A binary's documented value-taking global options.
struct GlobalOptionSpec {
    /// The binary's name, as it is written at the head of a command.
    binary: &'static str,
    /// The value-taking global options, each spelled as it is written.
    options: &'static [GlobalValueOption],
}

/// Python's value options before the script. `-m` names the module to run,
/// so the allow form keeps it next to its value: `python3 -m pytest -q`
/// matches a saved `python3 -m pytest`. `-W` and `-X` only set options.
const PYTHON_OPTIONS: &[GlobalValueOption] =
    &[option("-m", true), option("-W", false), option("-X", false)];

/// The per-binary option knowledge behind [`after_global_options`] — the
/// only place a binary is named for option-value skipping. Supporting
/// another CLI's documented, subcommand-preceding value options (kubectl
/// `-n`, docker `--context` are candidates) is an entry here, not a
/// parser change: the skipping semantics live once in
/// [`skip_global_options`], which this table merely feeds.
const GLOBAL_OPTION_SPECS: &[GlobalOptionSpec] = &[
    GlobalOptionSpec {
        binary: "git",
        // From git's synopsis (`git --bogus` usage). `-C` and `-c` accept
        // no attached short form (git rejects `-Cdir`); the long options
        // accept both spellings (`--git-dir=x`, `--git-dir x`). `-c` and
        // `--config-env` inject configuration, so they are marked as such;
        // the rest only relocate (`-C`, `--git-dir`, `--work-tree`) or
        // scope (`--namespace`, `--attr-source`) the run.
        options: &[
            option("-C", false),
            option("-c", true),
            option("--git-dir", false),
            option("--work-tree", false),
            option("--namespace", false),
            option("--config-env", true),
            option("--attr-source", false),
        ],
    },
    GlobalOptionSpec {
        binary: "python",
        options: PYTHON_OPTIONS,
    },
    GlobalOptionSpec {
        binary: "python3",
        options: PYTHON_OPTIONS,
    },
    GlobalOptionSpec {
        binary: "bash",
        options: &[
            option("-o", false),
            option("-O", false),
            option("--rcfile", false),
            option("--init-file", false),
        ],
    },
    GlobalOptionSpec {
        binary: "sh",
        options: &[option("-o", false)],
    },
    GlobalOptionSpec {
        binary: "dash",
        options: &[option("-o", false)],
    },
    GlobalOptionSpec {
        binary: "ksh",
        options: &[option("-o", false)],
    },
    GlobalOptionSpec {
        binary: "zsh",
        options: &[option("-o", false)],
    },
];

/// `binary`'s value-taking global options, or none when its option table
/// is unknown: an unlisted binary must never have a word consumed.
fn value_options_for(binary: &str) -> &'static [GlobalValueOption] {
    GLOBAL_OPTION_SPECS
        .iter()
        .find(|spec| spec.binary == binary)
        .map(|spec| spec.options)
        .unwrap_or(&[])
}

/// Drops the global-option words from the head of `rest` and returns the
/// words from the first non-option word onward. A listed option consumes
/// its value word too, unless the value rides inside the token
/// (`--git-dir=x`); every other option — boolean, or unknown to the
/// table — is dropped alone, keeping the next word, the subcommand
/// candidate, intact. Under [`OptionSkipping::AllButConfig`] an option
/// flagged `injects_config` is a wall: nothing is dropped from it on.
fn skip_global_options<'a>(
    value_options: &[GlobalValueOption],
    rest: &'a [&'a str],
    skipping: OptionSkipping,
) -> &'a [&'a str] {
    let mut rest = rest;
    while let [first, ..] = rest {
        if !is_flag(first) {
            break;
        }
        let Some(spec) = value_options.iter().find(|o| o.matches_token(first)) else {
            // A boolean or unknown option: the token alone goes.
            rest = rest.get(1..).unwrap_or(&[]);
            continue;
        };
        if skipping == OptionSkipping::AllButConfig && spec.injects_config {
            break;
        }
        // A value option written without `=` takes its value as the next
        // word; written with `=`, the value rides inside the token.
        let skip = if first.contains('=') { 1 } else { 2 };
        rest = rest.get(skip..).unwrap_or(&[]);
    }
    rest
}

/// Normalizes a segment for command-policy matching. For commands with
/// ordinary global options, this agrees with the saved prefix proposed
/// by `suggest_prefix`. It keeps
/// only the command's words, without leading `VAR=value` env assignments,
/// redirects and the global flags that sit between the binary and its
/// first subcommand, while preserving every word from the first
/// subcommand onward. Returns `None` when `segment` is not one command.
///
/// The two halves of the policy need different amounts of stripping, so
/// there are two forms:
///
/// - [`canonicalize_for_allow`] keeps an option whose value injects
///   configuration. Stripping it would let `git -c
///   diff.external=/bin/echo diff` match a saved `git diff` and execute
///   `/bin/echo`: configuration can name commands (`core.fsmonitor`,
///   `diff.external`), so such an invocation is not the command the user
///   saved. Keeping the option leaves the command outside every saved
///   subcommand prefix, so it asks for approval on its own.
/// - [`canonicalize_for_deny`] drops every listed option with its
///   value: the shorter form can only match more deny prefixes, so a
///   `git push` block still catches `git -c e=v push` and
///   `git -C repo push`.
///
/// Suggestion reads through ordinary options like `git -C repo status`,
/// which proposes `git status` and matches the allow form. It declines a
/// config-injecting option because the allow form must keep it.
fn canonicalize(segment: &str, skipping: OptionSkipping) -> Option<String> {
    let words = command_words(segment)?;
    let binary = words[0];
    let rest = after_global_options(binary, &words[1..], skipping);
    Some(
        std::iter::once(binary)
            .chain(rest.iter().copied())
            .collect::<Vec<_>>()
            .join(" "),
    )
}

/// The form a saved allow prefix matches against: global options
/// stripped, except an option that injects configuration — and every
/// word after it — which stays verbatim so it can never widen a grant
/// silently. `None` when `segment` is not one command.
pub fn canonicalize_for_allow(segment: &str) -> Option<String> {
    canonicalize(segment, OptionSkipping::AllButConfig)
}

/// The form a deny prefix matches against: every documented value option
/// stripped with its value, so a block cannot be dodged through the
/// option region. `None` when `segment` is not one command.
pub fn canonicalize_for_deny(segment: &str) -> Option<String> {
    canonicalize(segment, OptionSkipping::All)
}

/// The head of `segment` as the shell runs it, joined by single spaces, with
/// the directory dropped from the command name: `FOO='a b' /usr/bin/rm\t-rf
/// x` → `rm -rf x`. `None` when `segment` is not one command.
pub fn by_head_name(segment: &str) -> Option<String> {
    Some(by_name(&row_words(segment)?))
}

/// The command `segment` runs, joined by single spaces, with the directory
/// dropped from the command name: `timeout 60 rm -rf x` → `rm -rf x`. `None`
/// when a modifier wrapper is run by path or its own arguments cannot be
/// told from the command.
pub fn by_run_command(segment: &str) -> Option<String> {
    Some(by_name(&command_words(segment)?))
}

/// The command `segment` runs from the arguments of `xargs`, `exec`, `eval`
/// or `watch`, joined like [`by_run_command`]: `xargs -0 rm -f` → `rm -f`.
/// Deny matching only: the row itself stays named by its head.
pub fn by_argument_command(segment: &str) -> Option<String> {
    Some(by_name(&argument_command_words(segment)?))
}

/// `words` joined by single spaces, with the directory dropped from the
/// command name.
fn by_name(words: &[&str]) -> String {
    let name = words[0].rsplit('/').next().unwrap_or(words[0]);
    std::iter::once(name)
        .chain(words[1..].iter().copied())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Interpreters whose first non-option word is the script they run, and
/// the builtins that source a script. A saved prefix keeps that word, so a
/// grant names the script (`bash tools/check-links.sh`, `. ./setup.sh`
/// save as themselves); code passed inline is not written out, and neither
/// is the stdin form, so those suggest the binary alone.
const INTERPRETERS: &[&str] = &[
    "bash", "sh", "zsh", "dash", "ksh", "fish", "python", "python3", "node", "deno", "bun", "ruby",
    "perl", "php", "source", ".",
];

/// True if `flag` makes `binary` run code passed inline: the code flag
/// alone or ending a cluster of short options (`bash -lc`, `perl -ne`). A
/// token led by a value option holds that option's value (`-Wdefault`).
fn runs_inline_code(binary: &str, flag: &str) -> bool {
    let Some(cluster) = flag.strip_prefix('-') else {
        return false;
    };
    let Some(last) = cluster.chars().last() else {
        return false;
    };
    if !cluster.bytes().all(|b| b.is_ascii_alphabetic())
        || value_options_for(binary)
            .iter()
            .any(|o| o.name.len() == 2 && cluster.starts_with(&o.name[1..]))
    {
        return false;
    }
    matches!(
        (binary, last),
        ("bash" | "sh" | "zsh" | "dash" | "ksh" | "fish", 'c')
            | ("python" | "python3", 'c')
            | ("node" | "ruby" | "perl", 'e')
            | ("php", 'r')
    )
}

/// `binary` plus the first word that names what it runs: the script
/// word, or a module option with its value. The binary's value options
/// ([`GLOBAL_OPTION_SPECS`]) skip their value; other options are skipped
/// alone, as the allow form skips them, so the prefix matches the command
/// it came from.
fn interpreter_prefix(binary: &str, rest: &[&str]) -> String {
    let options = value_options_for(binary);
    let mut at = 0;
    while let Some(&token) = rest.get(at) {
        if !is_flag(token) {
            break;
        }
        if runs_inline_code(binary, token) {
            return binary.to_string();
        }
        match options.iter().find(|o| o.matches_token(token)) {
            // The option names the program (`python3 -m pytest`).
            Some(o) if o.injects_config => {
                return match rest.get(at + 1) {
                    Some(module) if !token.contains('=') => format!("{binary} {token} {module}"),
                    _ => binary.to_string(),
                };
            }
            Some(_) if !token.contains('=') => at += 2,
            _ => at += 1,
        }
    }
    match rest.get(at) {
        // `-` and `--` read the program from stdin.
        Some(&script) if !matches!(script, "-" | "--") => format!("{binary} {script}"),
        _ => binary.to_string(),
    }
}

fn lookup_style(binary: &str) -> Style {
    if NO_SUBCOMMAND.contains(&binary) {
        Style::None
    } else if ONE_WORD.contains(&binary) {
        Style::OneWord
    } else if TWO_WORD_NO_HYPHEN.contains(&binary) {
        Style::TwoWord(SecondTokenPolicy::NoHyphen)
    } else if TWO_WORD_PERMISSIVE.contains(&binary) {
        Style::TwoWord(SecondTokenPolicy::Permissive)
    } else {
        Style::Unknown
    }
}

/// Path-shaped means the binary token references the filesystem directly
/// (`./foo`, `../foo`, `/usr/bin/x`, `~/bin/y`, or any token containing `/`).
fn is_path_shaped(token: &str) -> bool {
    token.contains('/') || token.starts_with("~")
}

fn is_flag(token: &str) -> bool {
    if !token.starts_with('-') {
        return false;
    }
    // Bare `-` (stdin), `--` (end-of-options) are not flags for our purposes.
    !matches!(token, "-" | "--")
}

/// Looks like a subcommand name: lowercase ASCII, optional digits/hyphens/
/// underscores, length 1-25, leading letter.
fn is_subcommand_shaped(token: &str) -> bool {
    if token.is_empty() || token.len() > 25 {
        return false;
    }
    let mut chars = token.chars();
    let first = chars.next().unwrap();
    if !first.is_ascii_lowercase() {
        return false;
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(input: &str, expected: &str) {
        let got = suggest_prefix(input);
        assert_eq!(
            got, expected,
            "suggest_prefix({input:?}) → {got:?}, expected {expected:?}"
        );
    }

    // -----------------------------------------------------------------
    // Interpreters — the prefix names the script, not the code
    // -----------------------------------------------------------------

    #[test]
    fn interpreter_script_word_is_the_prefix() {
        check("bash tools/check-links.sh", "bash tools/check-links.sh");
        check("sh script.sh", "sh script.sh");
        check("zsh run.zsh", "zsh run.zsh");
        check("dash d.sh", "dash d.sh");
        check("ksh k.ksh", "ksh k.ksh");
        check("fish f.fish", "fish f.fish");
        check("python main.py", "python main.py");
        check("python3 run.py arg", "python3 run.py");
        check("node server.js", "node server.js");
        check("ruby app.rb", "ruby app.rb");
        check("perl scan.pl", "perl scan.pl");
        check("php unit.php", "php unit.php");
        check("bun install.js", "bun install.js");
        check("deno run mod.ts", "deno run");
    }

    #[test]
    fn interpreter_inline_code_stops_at_the_binary() {
        check("bash -c \"echo hi\"", "bash");
        check("python3 -c \"print(1)\"", "python3");
        check("node -e \"console.log(1)\"", "node");
        check("ruby -e \"puts 1\"", "ruby");
        check("perl -e \"print 1\"", "perl");
        check("php -r \"echo 1;\"", "php");
    }

    #[test]
    fn interpreter_stdin_forms_stop_at_the_binary() {
        check("python3 -", "python3");
        check("bash -", "bash");
    }

    #[test]
    fn interpreter_env_assignment_is_skipped() {
        check("FOO=1 bash tools/x.sh", "bash tools/x.sh");
    }

    #[test]
    fn interpreter_options_are_skipped_before_the_script() {
        check("python3 -q -u tools/x.py", "python3 tools/x.py");
        check("bash -x tools/run.sh", "bash tools/run.sh");
    }

    #[test]
    fn python_module_flag_stays_in_the_prefix_and_round_trips() {
        check("python3 -m pytest -q", "python3 -m pytest");
        check("python -m http.server", "python -m http.server");
        check("python3 -m", "python3");
        let prefix = suggest_prefix("python3 -m pytest -q");
        let allow = canonicalize_for_allow("python3 -m pytest -q").unwrap();
        assert!(
            allow.starts_with(&prefix),
            "allow form {allow:?} does not cover prefix {prefix:?}"
        );
    }

    // -----------------------------------------------------------------
    // OneWord style — git, docker, npm, etc.
    // -----------------------------------------------------------------

    #[test]
    fn git_status() {
        check("git status", "git status");
    }

    #[test]
    fn git_status_with_flag() {
        check("git status -s", "git status");
    }

    #[test]
    fn git_log_with_args() {
        check("git log --oneline -5", "git log");
    }

    #[test]
    fn git_with_global_flag() {
        check("git --no-pager log --oneline", "git log");
    }

    #[test]
    fn docker_ps() {
        check("docker ps -a", "docker ps");
    }

    #[test]
    fn npm_install() {
        check("npm install foo", "npm install");
    }

    #[test]
    fn make_build() {
        check("make build", "make build");
    }

    #[test]
    fn terraform_apply() {
        check("terraform apply -auto-approve", "terraform apply");
    }

    #[test]
    fn cargo_test_with_args() {
        check("cargo test --release -- --nocapture", "cargo test");
    }

    #[test]
    fn systemctl_status() {
        check("systemctl status nginx", "systemctl status");
    }

    // -----------------------------------------------------------------
    // TwoWord style — kubectl, aws, gh, etc.
    // -----------------------------------------------------------------

    #[test]
    fn kubectl_logs() {
        check("kubectl logs my-pod -f", "kubectl logs");
    }

    #[test]
    fn kubectl_get_pods() {
        check("kubectl get pods -n kube-system", "kubectl get pods");
    }

    #[test]
    fn kubectl_delete_pod() {
        check(
            "kubectl --context=prod delete pod foo",
            "kubectl delete pod",
        );
    }

    #[test]
    fn kubectl_with_flag_between_subwords() {
        // Flag between `get` and `pods` → stop at first subword.
        check("kubectl get -o yaml pods", "kubectl get");
    }

    #[test]
    fn aws_s3_ls() {
        check("aws s3 ls s3://bucket", "aws s3 ls");
    }

    #[test]
    fn aws_s3_cp() {
        check("aws s3 cp file.txt s3://bucket/", "aws s3 cp");
    }

    #[test]
    fn aws_with_global_flag() {
        check("aws --profile=prod s3 ls s3://bucket", "aws s3 ls");
    }

    #[test]
    fn gh_pr_create() {
        check(
            r#"gh pr create --title "feat: x" --body "...""#,
            "gh pr create",
        );
    }

    #[test]
    fn gh_auth_login() {
        check("gh auth login", "gh auth login");
    }

    #[test]
    fn aws_hyphenated_subcommand() {
        // aws uses hyphens in canonical subcommand names — `describe-instances`
        // is the operation, not a specific identifier. Permissive policy takes
        // it as the second subword.
        check(
            "aws ec2 describe-instances --region us-east-1",
            "aws ec2 describe-instances",
        );
    }

    #[test]
    fn kubectl_logs_with_specific_pod_name() {
        // kubectl's NoHyphen policy: `my-pod` is rejected as a specific
        // identifier, so we stop at the verb.
        check("kubectl logs my-pod -f", "kubectl logs");
    }

    #[test]
    fn kubectl_describe_resource_then_name() {
        // `pod` (no hyphen) is a resource type → take it.
        check("kubectl describe pod my-app-7f8d", "kubectl describe pod");
    }

    #[test]
    fn kubectl_delete_with_hyphenated_resource_name() {
        // `deployment` (no hyphen) is the resource type.
        check(
            "kubectl delete deployment my-app",
            "kubectl delete deployment",
        );
    }

    // -----------------------------------------------------------------
    // None style — flat-argument commands
    // -----------------------------------------------------------------

    #[test]
    fn cat_file() {
        check("cat /etc/hosts", "cat");
    }

    #[test]
    fn ls_with_flags_and_path() {
        check("ls -la /tmp", "ls");
    }

    #[test]
    fn grep_pattern() {
        check("grep -i ERROR /var/log/syslog", "grep");
    }

    #[test]
    fn echo_args() {
        check("echo hello world", "echo");
    }

    #[test]
    fn curl_url() {
        check("curl -s https://example.com/foo", "curl");
    }

    // -----------------------------------------------------------------
    // Unknown binary — heuristic
    // -----------------------------------------------------------------

    #[test]
    fn unknown_binary_with_subword_shape() {
        check("mytool subcmd --opt", "mytool subcmd");
    }

    #[test]
    fn unknown_binary_with_non_subword_arg() {
        // Argument doesn't look like a subcommand → just the binary.
        check("mytool /some/path", "mytool");
    }

    #[test]
    fn unknown_binary_with_uppercase_arg() {
        check("mytool MIXED-case", "mytool");
    }

    #[test]
    fn unknown_binary_with_only_flags() {
        check("mytool --foo --bar", "mytool");
    }

    // -----------------------------------------------------------------
    // Path-shaped binaries
    // -----------------------------------------------------------------

    #[test]
    fn dot_slash_script() {
        check("./scripts/deploy.sh prod", "./scripts/deploy.sh");
    }

    #[test]
    fn absolute_path() {
        check("/usr/local/bin/foo --bar", "/usr/local/bin/foo");
    }

    #[test]
    fn tilde_path() {
        check("~/bin/foo arg", "~/bin/foo");
    }

    #[test]
    fn relative_parent_path() {
        check("../sibling/cmd subcmd", "../sibling/cmd");
    }

    // -----------------------------------------------------------------
    // Env-assignment prefix
    // -----------------------------------------------------------------

    #[test]
    fn env_prefix_skipped_before_binary() {
        check("FOO=bar git status", "git status");
    }

    #[test]
    fn multiple_env_prefixes_skipped() {
        check("A=1 B=2 git log", "git log");
    }

    #[test]
    fn env_prefix_then_path() {
        check("FOO=1 ./scripts/deploy.sh", "./scripts/deploy.sh");
    }

    // -----------------------------------------------------------------
    // Junk heads → no prefix (Fix #3)
    // -----------------------------------------------------------------

    #[test]
    fn colon_noop_has_no_prefix() {
        // The `:` builtin is a no-op; allowlisting it is meaningless and
        // never matches a re-run. Return empty so the modal hides persist.
        check(":", "");
    }

    #[test]
    fn reserved_word_head_has_no_prefix() {
        // `for i` would trust every loop over `i`, whatever its body runs.
        check("for f in $(ls)", "");
        check("case $(uname) in", "");
        check("function f", "");
    }

    #[test]
    fn colon_with_redirect_has_no_prefix() {
        // `: > file` — the over-collapsed shape from the real bug report.
        check(": > /tmp/out.jsonl", "");
    }

    #[test]
    fn punctuation_head_has_no_prefix() {
        // Leftover punctuation from a scrambled segment isn't a command.
        check("> file", "");
        check("&& echo hi", "");
    }

    #[test]
    fn digit_leading_binary_still_gets_prefix() {
        // `7z` is a real binary — must NOT be rejected as junk. (Unknown
        // style, so the subcommand-shaped `x` is included as a subword.)
        check("7z x archive.7z", "7z x");
    }

    #[test]
    fn env_only_no_binary() {
        // Just env assignments, no command: nothing to save as a prefix.
        check("FOO=bar", "");
    }

    // -----------------------------------------------------------------
    // Quoted args don't confuse tokenization
    // -----------------------------------------------------------------

    #[test]
    fn quoted_arg_preserved_in_tokenization() {
        check(r#"git commit -m "hello world""#, "git commit");
    }

    #[test]
    fn single_quoted_subcommand() {
        // Edge: someone quoted the subcommand. Shape check would reject
        // (leading `'`), so we'd fall back to binary-only for Unknown.
        // For OneWord we don't shape-check, so we'd take the quoted token.
        // This is a fringe case; either behavior is defensible.
        check(r"git 'status'", "git 'status'");
    }

    // -----------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------

    #[test]
    fn is_subcommand_shaped_examples() {
        assert!(is_subcommand_shaped("logs"));
        assert!(is_subcommand_shaped("get"));
        assert!(is_subcommand_shaped("pr-create"));
        assert!(is_subcommand_shaped("s3"));
        assert!(!is_subcommand_shaped("S3")); // uppercase
        assert!(!is_subcommand_shaped("/etc"));
        assert!(!is_subcommand_shaped("123abc")); // leading digit
        assert!(!is_subcommand_shaped(""));
        assert!(!is_subcommand_shaped("a".repeat(26).as_str())); // too long
    }

    #[test]
    fn is_flag_examples() {
        assert!(is_flag("-f"));
        assert!(is_flag("--foo"));
        assert!(is_flag("--foo=bar"));
        assert!(!is_flag("-"));
        assert!(!is_flag("--"));
        assert!(!is_flag("foo"));
        assert!(!is_flag(""));
    }

    #[test]
    fn is_path_shaped_examples() {
        assert!(is_path_shaped("./foo"));
        assert!(is_path_shaped("/usr/bin/x"));
        assert!(is_path_shaped("~/bin/y"));
        assert!(is_path_shaped("../sibling/cmd"));
        assert!(is_path_shaped("a/b"));
        assert!(!is_path_shaped("foo"));
        assert!(!is_path_shaped("foo.sh")); // no slash → not path
    }

    // -----------------------------------------------------------------
    // canonicalize_for_allow / canonicalize_for_deny — the matcher-side
    // normalization; the deny form agrees with the suggestion it pairs
    // with, the allow form additionally keeps config options verbatim.
    // -----------------------------------------------------------------

    #[test]
    fn canonicalize_strips_global_flag_so_match_agrees_with_suggestion() {
        // The motivating bug: suggest_prefix maps this to `git log`, so the
        // canonical form must start with `git log` for the saved prefix to
        // match the command it came from.
        assert_eq!(
            canonicalize_for_deny("git --no-pager log --oneline -5").as_deref(),
            Some("git log --oneline -5")
        );
    }

    #[test]
    fn canonicalize_strips_leading_env_assignments() {
        assert_eq!(
            canonicalize_for_deny("FOO=bar BAZ=1 git log --oneline").as_deref(),
            Some("git log --oneline")
        );
    }

    #[test]
    fn canonicalize_strips_env_and_global_flag_together() {
        assert_eq!(
            canonicalize_for_deny("GIT_PAGER=cat git --no-pager show HEAD").as_deref(),
            Some("git show HEAD")
        );
    }

    #[test]
    fn config_value_option_splits_the_allow_form_from_the_deny_form() {
        // `git -c <val>` carries its value as a separate token that looks
        // like neither a flag nor an env assignment — it is not the
        // subcommand. Deny matching reads through it, but allow matching
        // keeps it: configuration can name commands to execute. A bare
        // subcommand suggestion would not match the invocation.
        let cmd = "git -c core.pager=less show HEAD";
        assert_eq!(suggest_prefix(cmd), "");
        assert_eq!(canonicalize_for_deny(cmd).as_deref(), Some("git show HEAD"));
        assert_eq!(
            canonicalize_for_allow(cmd).as_deref(),
            Some("git -c core.pager=less show HEAD")
        );
    }

    #[test]
    fn canonicalize_keeps_a_plain_command() {
        assert_eq!(
            canonicalize_for_deny("git log --oneline -5").as_deref(),
            Some("git log --oneline -5")
        );
    }

    #[test]
    fn canonicalize_leaves_flat_argument_commands_untouched() {
        // `ls` is Style::None — its leading flags are arguments, not a
        // pre-subcommand region, so suggest_prefix keeps them off the prefix
        // and we must not strip them here either.
        assert_eq!(
            canonicalize_for_deny("ls -la /tmp").as_deref(),
            Some("ls -la /tmp")
        );
    }

    #[test]
    fn canonicalize_leaves_path_shaped_binary_flags_untouched() {
        // Path-shaped binaries have no subcommand layer; suggest_prefix
        // returns the binary head as-is, so no flag stripping.
        assert_eq!(
            canonicalize_for_deny("~/go/bin/go --foo build").as_deref(),
            Some("~/go/bin/go --foo build")
        );
    }

    #[test]
    fn canonicalize_path_shaped_binary_still_strips_env() {
        assert_eq!(
            canonicalize_for_deny("FOO=bar ~/go/bin/go build").as_deref(),
            Some("~/go/bin/go build")
        );
    }

    #[test]
    fn canonicalize_all_assignment_segment_returns_none() {
        assert_eq!(canonicalize_for_deny("FOO=bar BAZ=1"), None);
        assert_eq!(canonicalize_for_allow("FOO=bar BAZ=1"), None);
    }

    // -----------------------------------------------------------------
    // Git global options that carry a value (-C, -c, --git-dir, …)
    // -----------------------------------------------------------------

    #[test]
    fn git_dash_c_value_is_not_the_subcommand() {
        check("git -C repo status --short", "git status");
        check("git -C ../wt-live-task-thinking status", "git status");
        assert_eq!(
            canonicalize_for_deny("git -C repo status --short").as_deref(),
            Some("git status --short")
        );
        // `-C` relocates the run; it configures nothing, so the allow
        // form strips it too and a saved `git status` covers it.
        assert_eq!(
            canonicalize_for_allow("git -C repo status --short").as_deref(),
            Some("git status --short")
        );
    }

    #[test]
    fn git_value_options_attached_forms() {
        // git rejects `-Cdir` as an option spelling, but the normalizer
        // still treats the whole token as one option — its tail is a
        // value, not a subcommand. `--opt=value` is the real attached
        // form and carries its value inside the token.
        check("git -Crepo status", "git status");
        check("git --git-dir=.git log", "git log");
        assert_eq!(
            canonicalize_for_deny("git --git-dir=.git log --oneline").as_deref(),
            Some("git log --oneline")
        );
    }

    #[test]
    fn git_long_value_options_separate_form() {
        // `--git-dir x` and `--git-dir=x` are both valid git; the matcher
        // must read them the same way.
        check("git --git-dir .git log", "git log");
        assert_eq!(
            canonicalize_for_deny("git --git-dir .git log --oneline").as_deref(),
            Some("git log --oneline")
        );
    }

    #[test]
    fn git_global_options_reorder() {
        check("git --no-pager -C repo status", "git status");
        check("git -c core.pager=cat -C repo log --oneline", "");
        assert_eq!(
            canonicalize_for_deny("git --no-pager -C repo status -s").as_deref(),
            Some("git status -s")
        );
        assert_eq!(
            canonicalize_for_allow("git --no-pager -C repo status -s").as_deref(),
            Some("git status -s")
        );
    }

    #[test]
    fn git_value_option_without_subcommand_falls_back_to_binary() {
        // `git -C repo` names no subcommand; the honest prefix is `git`,
        // matching `git --no-pager` (flag-only, no subcommand).
        check("git -C repo", "git");
    }

    #[test]
    fn git_options_after_subcommand_belong_to_it() {
        // `-C` is global only before the subcommand (git rejects
        // `git status -C .`); `git log -C 2` sets copy detection.
        // Nothing after the subcommand is dropped.
        check("git log -C 2 --oneline", "git log");
        assert_eq!(
            canonicalize_for_deny("git status -C repo").as_deref(),
            Some("git status -C repo")
        );
    }

    #[test]
    fn git_value_options_behind_env_and_wrapper() {
        check("FOO=bar git -C repo status", "git status");
        check("timeout 600 git -C repo status --short", "git status");
        assert_eq!(
            canonicalize_for_deny("FOO=bar git -C repo status").as_deref(),
            Some("git status")
        );
        assert_eq!(
            canonicalize_for_deny("timeout 600 git -C repo status --short").as_deref(),
            Some("git status --short")
        );
    }

    #[test]
    fn config_injection_stops_the_allow_form_in_both_spellings() {
        // `-c` and `--config-env` turn the run into something a saved
        // subcommand prefix never described; whether the value rides in
        // the next word or inside the token, the allow form keeps the
        // whole command verbatim. The deny form still reads through to
        // the subcommand.
        for cmd in [
            "git -c diff.external=/bin/echo diff HEAD~1 HEAD",
            "git --config-env core.fsmonitor=FSMON status",
            "git --config-env=core.fsmonitor=FSMON status",
        ] {
            assert_eq!(
                canonicalize_for_allow(cmd).as_deref(),
                Some(cmd),
                "`{cmd}` must keep its config option for allow matching"
            );
        }
        assert_eq!(
            canonicalize_for_deny("git --config-env=core.fsmonitor=FSMON status").as_deref(),
            Some("git status")
        );
        assert_eq!(
            canonicalize_for_deny("git --config-env core.fsmonitor=FSMON status").as_deref(),
            Some("git status")
        );
    }

    #[test]
    fn unlisted_binary_never_consumes_a_value_word() {
        // A binary without a GLOBAL_OPTION_SPECS entry has an unknown
        // option table: the word after an option could be its value or
        // its subcommand, and guessing wrong would grant or block on a
        // misread subcommand. Only the option token is dropped, so
        // `tool -C repo status` suggests the (editable, inert) `tool
        // repo` rather than risking a wrong `tool status`.
        check("tool -C repo status", "tool repo");
        check("tool --context prod ps", "tool prod");
        assert_eq!(
            canonicalize_for_deny("tool -C repo status").as_deref(),
            Some("tool repo status")
        );
        assert_eq!(
            canonicalize_for_allow("tool -C repo status").as_deref(),
            Some("tool repo status")
        );
    }

    #[test]
    fn spec_entries_alone_drive_both_skipping_scopes() {
        // The extensibility contract of GLOBAL_OPTION_SPECS: the next
        // documented CLI is an entry of the same shape, not a parser
        // change. A hypothetical docker entry shows the whole contract —
        // benign value options in both spellings, an option flagged
        // config-injecting, and a binary with no entry at all.
        let docker: &[GlobalValueOption] = &[
            option("--context", false),
            option("-H", false),
            option("--log-config", true),
        ];
        let benign = ["--context", "prod", "ps", "-a"];
        assert_eq!(
            skip_global_options(docker, &benign, OptionSkipping::All),
            &["ps", "-a"][..]
        );
        assert_eq!(
            skip_global_options(docker, &benign, OptionSkipping::AllButConfig),
            &["ps", "-a"][..]
        );
        let attached = ["--debug", "--context=prod", "ps"];
        assert_eq!(
            skip_global_options(docker, &attached, OptionSkipping::All),
            &["ps"][..]
        );
        let config = ["--log-config", "/bin/evil", "ps"];
        assert_eq!(
            skip_global_options(docker, &config, OptionSkipping::All),
            &["ps"][..]
        );
        assert_eq!(
            skip_global_options(docker, &config, OptionSkipping::AllButConfig),
            &config[..]
        );
        assert_eq!(
            skip_global_options(docker, &["--context", "prod"], OptionSkipping::All),
            &[] as &[&str]
        );
        // A binary without an entry has an unknown option table: no word
        // is ever consumed, under either scope.
        assert!(value_options_for("no-such-tool").is_empty());
    }

    // -----------------------------------------------------------------
    // Wrappers that only change how a command runs
    // -----------------------------------------------------------------

    #[test]
    fn wrapper_suggests_the_command_it_runs() {
        assert_eq!(suggest_prefix("timeout 600 cargo build"), "cargo build");
        assert_eq!(suggest_prefix("time make build"), "make build");
        assert_eq!(suggest_prefix("env FOO=1 cargo build"), "cargo build");
    }

    #[test]
    fn wrapper_canonicalizes_to_the_command_it_runs() {
        // The prefix the user saves must match the row it came from, or the
        // grant never covers it and the user is asked again.
        assert_eq!(
            canonicalize_for_deny("timeout 600 cargo build 2>&1").as_deref(),
            Some("cargo build")
        );
        // The policy matches a row, not the whole command: the row of the
        // wrapper is the command with its redirect.
        assert_eq!(
            canonicalize_for_deny("timeout 600 cargo build 2>&1").as_deref(),
            Some("cargo build")
        );
    }

    #[test]
    fn code_argument_is_named_by_its_written_words() {
        // The code inside the argument is not looked into, so the prefix
        // names the binary alone and deny matching stays outside it.
        assert_eq!(suggest_prefix("bash -c 'rm -rf /'"), "bash");
        assert_eq!(
            by_run_command("bash -c 'rm -rf /'").as_deref(),
            Some("bash -c 'rm -rf /'")
        );
    }

    #[test]
    fn by_head_name_keeps_the_wrapper_and_by_run_command_the_command() {
        assert_eq!(
            by_head_name("/usr/bin/timeout 2 rm -rf x").as_deref(),
            Some("timeout 2 rm -rf x")
        );
        assert_eq!(
            by_run_command("timeout 60 rm -rf x").as_deref(),
            Some("rm -rf x")
        );
        assert_eq!(
            by_head_name("FOO=1 /usr/bin/rm -rf x").as_deref(),
            Some("rm -rf x")
        );
    }

    #[test]
    fn interpreter_flag_cluster_ending_in_the_code_flag_is_inline_code() {
        check("bash -lc 'echo hi'", "bash");
        check("bash -ec 'echo hi'", "bash");
        check("perl -ne 'print'", "perl");
        check("perl -lne 'print'", "perl");
        check("ruby -ne 'puts'", "ruby");
        check("python3 -Bc 'print(1)'", "python3");
    }

    #[test]
    fn interpreter_value_options_consume_their_value() {
        check("python3 -W ignore x.py", "python3 x.py");
        check("python3 -X dev x.py", "python3 x.py");
        check("bash -o pipefail s.sh", "bash s.sh");
        check("bash -O extglob s.sh", "bash s.sh");
        check("python3 -W ignore -m pytest -q", "python3 -m pytest");
    }

    #[test]
    fn sourcing_names_the_script() {
        check(". ./setup.sh", ". ./setup.sh");
        check("source ./setup.sh", "source ./setup.sh");
    }

    #[test]
    fn command_lookup_suggests_the_lookup() {
        // Not `command rm`, which would also allow running `rm`.
        check("command -v rm", "command -v");
        check("command -V git", "command -V");
    }
}
