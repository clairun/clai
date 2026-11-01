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
//! The output is always a default; the modal pre-fills it into an editable
//! field so the user can narrow or broaden before confirming.

#![allow(dead_code)] // wired into the approval flow in commit 7

use crate::assistant::tools::command_splitter::{command_words, row_words};

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
    "deno",
    "bun",
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

    // 4. Skip the global options between binary and first subword; for
    //    git, an option's value word is skipped with it.
    let sub = after_global_options(binary, &tokens[1..]);
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
/// [`suggest_prefix`] and [`canonicalize_for_match`] share this so the
/// prefix one proposes is the exact form the other matches against.
///
/// Value awareness is a lookup in [`GLOBAL_OPTION_SPECS`]: a listed
/// binary's documented value options (`git -C <dir>` above all) consume
/// their value word, which must never be taken for the subcommand
/// (`git -C repo status` is `git status`, never `git repo`). Every other
/// binary keeps the previous flag-token-only skipping — its option table
/// is unknown, so a value word cannot be told from a subcommand.
///
/// Dropping stops at the first non-option word: the table only lists
/// options documented to precede the subcommand (`git status -C .` is
/// rejected by git), while options after it belong to the subcommand's
/// own grammar (`git log -C 2` sets copy detection), so those stay
/// verbatim. Path-shaped binaries and flat-argument commands (Style::None)
/// keep every word: `suggest_prefix` returns their head alone, so no
/// option region exists.
fn after_global_options<'a>(binary: &str, rest: &'a [&'a str]) -> &'a [&'a str] {
    if is_path_shaped(binary) || matches!(lookup_style(binary), Style::None) {
        return rest;
    }
    skip_global_options(value_options_for(binary), rest)
}

/// A binary's global options whose value is a separate word when not
/// attached with `=`, as the binary's own synopsis documents them.
struct GlobalOptionSpec {
    /// The binary's name, as it is written at the head of a command.
    binary: &'static str,
    /// The value-taking global options, each spelled as it is written.
    value_options: &'static [&'static str],
}

/// The per-binary option knowledge behind [`after_global_options`] — the
/// only place a binary is named for option-value skipping. Supporting
/// another CLI's documented, subcommand-preceding value options (kubectl
/// `-n`, docker `--context` are candidates) is an entry here, not a
/// parser change: the skipping semantics live once in
/// [`skip_global_options`], which this table merely feeds.
const GLOBAL_OPTION_SPECS: &[GlobalOptionSpec] = &[GlobalOptionSpec {
    binary: "git",
    // From git's synopsis (`git --bogus` usage). `-C` and `-c` accept
    // no attached short form (git rejects `-Cdir`); the long options
    // accept both spellings (`--git-dir=x`, `--git-dir x`).
    value_options: &[
        "-C",
        "-c",
        "--git-dir",
        "--work-tree",
        "--namespace",
        "--config-env",
        "--attr-source",
    ],
}];

/// `binary`'s value-taking global options, or none when its option table
/// is unknown: an unlisted binary must never have a word consumed.
fn value_options_for(binary: &str) -> &'static [&'static str] {
    GLOBAL_OPTION_SPECS
        .iter()
        .find(|spec| spec.binary == binary)
        .map(|spec| spec.value_options)
        .unwrap_or(&[])
}

/// Drops the global-option words from the head of `rest` and returns the
/// words from the first non-option word onward. An option listed in
/// `value_options` consumes its value word too, unless the value rides
/// inside the token (`--git-dir=x`); every other option — boolean, or
/// unknown to the table — is dropped alone, keeping the next word, the
/// subcommand candidate, intact.
fn skip_global_options<'a>(value_options: &[&str], rest: &'a [&'a str]) -> &'a [&'a str] {
    let mut rest = rest;
    while let [first, ..] = rest {
        if !is_flag(first) {
            break;
        }
        // A value option written without `=` takes its value as the next
        // word; written with `=`, the value rides inside the token.
        let skip = if !first.contains('=') && value_options.contains(&first) {
            2
        } else {
            1
        };
        rest = rest.get(skip..).unwrap_or(&[]);
    }
    rest
}

/// Normalizes a segment so the allowlist/blocklist matcher identifies a
/// command the *same way* `suggest_prefix` does when it proposes the saved
/// prefix. Mirrors steps 2–5 of the suggestion algorithm: it keeps only the
/// command's words, without leading `VAR=value` env assignments, redirects
/// and the global flags that sit between the binary and its first
/// subcommand, while preserving every word from the first subcommand onward.
///
/// Without this, the two halves disagree: `suggest_prefix("git --no-pager
/// log --oneline")` returns `git log`, but a literal prefix match of
/// `git log` against the raw `git --no-pager log …` fails — so "Always allow
/// git log" never matches the command it came from and the user is
/// re-prompted forever.
///
/// Returns `None` when `segment` is not one command.
pub fn canonicalize_for_match(segment: &str) -> Option<String> {
    let words = command_words(segment)?;
    let binary = words[0];
    let rest = after_global_options(binary, &words[1..]);
    Some(
        std::iter::once(binary)
            .chain(rest.iter().copied())
            .collect::<Vec<_>>()
            .join(" "),
    )
}

/// The head of `segment` as the shell runs it, joined by single spaces, with
/// the directory dropped from the command name: `FOO='a b' /usr/bin/rm\t-rf
/// x` → `rm -rf x`. `None` when `segment` is not one command.
pub fn by_head_name(segment: &str) -> Option<String> {
    let words = row_words(segment)?;
    let name = words[0].rsplit('/').next().unwrap_or(words[0]);
    Some(
        std::iter::once(name)
            .chain(words[1..].iter().copied())
            .collect::<Vec<_>>()
            .join(" "),
    )
}

/// The command `segment` runs, joined by single spaces, with the directory
/// dropped from the command name: `timeout 60 rm -rf x` → `rm -rf x`. `None`
/// when the command is not written out (`bash -c 'rm -rf /'`), so the
/// blocklist cannot see into it.
pub fn by_run_command(segment: &str) -> Option<String> {
    let words = command_words(segment)?;
    let name = words[0].rsplit('/').next().unwrap_or(words[0]);
    Some(
        std::iter::once(name)
            .chain(words[1..].iter().copied())
            .collect::<Vec<_>>()
            .join(" "),
    )
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
    // canonicalize_for_match — the matcher-side normalization that must
    // agree with the suggestion it pairs with.
    // -----------------------------------------------------------------

    #[test]
    fn canonicalize_strips_global_flag_so_match_agrees_with_suggestion() {
        // The motivating bug: suggest_prefix maps this to `git log`, so the
        // canonical form must start with `git log` for the saved prefix to
        // match the command it came from.
        assert_eq!(
            canonicalize_for_match("git --no-pager log --oneline -5").as_deref(),
            Some("git log --oneline -5")
        );
    }

    #[test]
    fn canonicalize_strips_leading_env_assignments() {
        assert_eq!(
            canonicalize_for_match("FOO=bar BAZ=1 git log --oneline").as_deref(),
            Some("git log --oneline")
        );
    }

    #[test]
    fn canonicalize_strips_env_and_global_flag_together() {
        assert_eq!(
            canonicalize_for_match("GIT_PAGER=cat git --no-pager show HEAD").as_deref(),
            Some("git show HEAD")
        );
    }

    #[test]
    fn git_dash_c_value_option_agrees_between_suggestion_and_match() {
        // `git -c <val>` carries its value as a separate token that looks
        // like neither a flag nor an env assignment. The value must be
        // consumed with the option — it is not the subcommand — so the
        // suggestion and the canonical form both land on `git show`.
        let cmd = "git -c core.pager=less show HEAD";
        assert_eq!(suggest_prefix(cmd), "git show");
        assert_eq!(
            canonicalize_for_match(cmd).as_deref(),
            Some("git show HEAD")
        );
    }

    #[test]
    fn canonicalize_keeps_a_plain_command() {
        assert_eq!(
            canonicalize_for_match("git log --oneline -5").as_deref(),
            Some("git log --oneline -5")
        );
    }

    #[test]
    fn canonicalize_leaves_flat_argument_commands_untouched() {
        // `ls` is Style::None — its leading flags are arguments, not a
        // pre-subcommand region, so suggest_prefix keeps them off the prefix
        // and we must not strip them here either.
        assert_eq!(
            canonicalize_for_match("ls -la /tmp").as_deref(),
            Some("ls -la /tmp")
        );
    }

    #[test]
    fn canonicalize_leaves_path_shaped_binary_flags_untouched() {
        // Path-shaped binaries have no subcommand layer; suggest_prefix
        // returns the binary head as-is, so no flag stripping.
        assert_eq!(
            canonicalize_for_match("~/go/bin/go --foo build").as_deref(),
            Some("~/go/bin/go --foo build")
        );
    }

    #[test]
    fn canonicalize_path_shaped_binary_still_strips_env() {
        assert_eq!(
            canonicalize_for_match("FOO=bar ~/go/bin/go build").as_deref(),
            Some("~/go/bin/go build")
        );
    }

    #[test]
    fn canonicalize_all_assignment_segment_returns_none() {
        assert_eq!(canonicalize_for_match("FOO=bar BAZ=1"), None);
    }

    // -----------------------------------------------------------------
    // Git global options that carry a value (-C, -c, --git-dir, …)
    // -----------------------------------------------------------------

    #[test]
    fn git_dash_c_value_is_not_the_subcommand() {
        check("git -C repo status --short", "git status");
        check("git -C ../wt-live-task-thinking status", "git status");
        assert_eq!(
            canonicalize_for_match("git -C repo status --short").as_deref(),
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
            canonicalize_for_match("git --git-dir=.git log --oneline").as_deref(),
            Some("git log --oneline")
        );
    }

    #[test]
    fn git_long_value_options_separate_form() {
        // `--git-dir x` and `--git-dir=x` are both valid git; the matcher
        // must read them the same way.
        check("git --git-dir .git log", "git log");
        assert_eq!(
            canonicalize_for_match("git --git-dir .git log --oneline").as_deref(),
            Some("git log --oneline")
        );
    }

    #[test]
    fn git_global_options_reorder() {
        check("git --no-pager -C repo status", "git status");
        check("git -c core.pager=cat -C repo log --oneline", "git log");
        assert_eq!(
            canonicalize_for_match("git --no-pager -C repo status -s").as_deref(),
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
            canonicalize_for_match("git status -C repo").as_deref(),
            Some("git status -C repo")
        );
    }

    #[test]
    fn git_value_options_behind_env_and_wrapper() {
        check("FOO=bar git -C repo status", "git status");
        check("timeout 600 git -C repo status --short", "git status");
        assert_eq!(
            canonicalize_for_match("FOO=bar git -C repo status").as_deref(),
            Some("git status")
        );
        assert_eq!(
            canonicalize_for_match("timeout 600 git -C repo status --short").as_deref(),
            Some("git status --short")
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
            canonicalize_for_match("tool -C repo status").as_deref(),
            Some("tool repo status")
        );
    }

    #[test]
    fn a_table_entry_alone_makes_a_binary_value_aware() {
        // Extensibility contract of GLOBAL_OPTION_SPECS: the next
        // documented CLI needs data only. With docker's `--context <ctx>`
        // and `-H <host>` as a hypothetical entry, the shared skipper
        // must parse boolean globals, value globals, `=`-attached forms,
        // and run out of words — with no parser change.
        let docker: &[&str] = &["--context", "-H"];
        assert_eq!(
            skip_global_options(docker, &["--context", "prod", "ps", "-a"]),
            &["ps", "-a"][..]
        );
        assert_eq!(
            skip_global_options(docker, &["--debug", "--context=prod", "ps"]),
            &["ps"][..]
        );
        assert_eq!(
            skip_global_options(docker, &["-H", "tcp://d", "--context", "prod", "images"]),
            &["images"][..]
        );
        assert_eq!(
            skip_global_options(docker, &["--context", "prod"]),
            &[] as &[&str]
        );
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
            canonicalize_for_match("timeout 600 cargo build 2>&1").as_deref(),
            Some("cargo build")
        );
        // The policy matches a row, not the whole command: the row of the
        // wrapper is the command with its redirect.
        assert_eq!(
            canonicalize_for_match("timeout 600 cargo build 2>&1").as_deref(),
            Some("cargo build")
        );
    }

    #[test]
    fn wrapper_of_code_has_no_command_to_name() {
        assert_eq!(suggest_prefix("bash -c 'rm -rf /'"), "");
        assert_eq!(by_run_command("bash -c 'rm -rf /'"), None);
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
}
