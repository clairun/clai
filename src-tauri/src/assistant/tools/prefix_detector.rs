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
//! 1. Tokenize the segment (whitespace-split, quote-aware).
//! 2. Skip leading `VAR=value` env-assignment tokens.
//! 3. Take the next token as the **binary**. If it's path-shaped
//!    (`./foo`, `/usr/bin/x`, `~/bin/y`), return it as-is.
//! 4. Look up the binary in the per-binary depth table to pick a style:
//!    `None`, `OneWord`, `TwoWord`, or `Unknown` (heuristic).
//! 5. Skip leading flag tokens (starting with `-`, but not the bare `-` or
//!    `--` markers).
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
    let tokens = tokenize(segment);
    if tokens.is_empty() {
        return segment.trim().to_string();
    }

    // 1. Skip leading env assignments.
    let mut idx = 0;
    while idx < tokens.len() && is_env_assignment(&tokens[idx]) {
        idx += 1;
    }

    // 2. Binary token, if any.
    let Some(binary) = tokens.get(idx).map(String::as_str) else {
        return segment.trim().to_string();
    };
    idx += 1;

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

    let style = lookup_style(binary);

    if matches!(style, Style::None) {
        return binary.to_string();
    }

    // 4. Skip flags between binary and first subword.
    while idx < tokens.len() && is_flag(&tokens[idx]) {
        idx += 1;
    }
    let Some(first_sub) = tokens.get(idx).cloned() else {
        return binary.to_string();
    };
    idx += 1;

    let include_first = match style {
        Style::OneWord | Style::TwoWord(_) => true,
        Style::Unknown => is_subcommand_shaped(&first_sub),
        Style::None => false,
    };
    if !include_first {
        return binary.to_string();
    }

    if let Style::TwoWord(policy) = style {
        // 5. Second subword only if the very next token is also a non-flag
        //    and looks like a subcommand. No flags allowed in between.
        //    Policy decides whether hyphenated tokens are accepted.
        if let Some(second) = tokens.get(idx) {
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

/// Normalizes a segment so the allowlist/blocklist matcher identifies a
/// command the *same way* `suggest_prefix` does when it proposes the saved
/// prefix. Mirrors steps 2–5 of the suggestion algorithm: it drops leading
/// `VAR=value` env assignments and the global flags that sit between the
/// binary and its first subcommand, while preserving every token from the
/// first subcommand onward.
///
/// Without this, the two halves disagree: `suggest_prefix("git --no-pager
/// log --oneline")` returns `git log`, but a literal prefix match of
/// `git log` against the raw `git --no-pager log …` fails — so "Always allow
/// git log" never matches the command it came from and the user is
/// re-prompted forever.
///
/// Returns `None` when nothing would be stripped (no env prefix, no
/// inter-binary flags), so the caller can skip the redundant second match and
/// keep using the raw text it already has.
pub fn canonicalize_for_match(segment: &str) -> Option<String> {
    let tokens = tokenize(segment);
    if tokens.is_empty() {
        return None;
    }

    // Step 2: skip leading env assignments.
    let mut idx = 0;
    while idx < tokens.len() && is_env_assignment(&tokens[idx]) {
        idx += 1;
    }
    let env_skipped = idx > 0;

    // Step 3: the binary.
    let Some(binary) = tokens.get(idx).cloned() else {
        // All-assignment segment (`FOO=bar`) — a benign construct, never
        // routed through prefix matching. Nothing to canonicalize.
        return None;
    };
    idx += 1;

    // Step 5: skip global flags between the binary and its first subcommand,
    // but only for binaries that actually have a subcommand layer. Path-shaped
    // binaries (`~/bin/go`) and flat-argument commands (`ls`, Style::None) are
    // returned head-only by `suggest_prefix` and skip no flags, so we mirror
    // that and leave their flags in place.
    let mut flags_skipped = false;
    if !is_path_shaped(&binary) && !matches!(lookup_style(&binary), Style::None) {
        while idx < tokens.len() && is_flag(&tokens[idx]) {
            idx += 1;
            flags_skipped = true;
        }
    }

    // Nothing was removed → identical to the raw text; let the literal match
    // handle it and avoid allocating a duplicate string.
    if !env_skipped && !flags_skipped {
        return None;
    }

    let mut rebuilt = String::with_capacity(segment.len());
    rebuilt.push_str(&binary);
    for tok in &tokens[idx..] {
        rebuilt.push(' ');
        rebuilt.push_str(tok);
    }
    Some(rebuilt)
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

/// Shell-aware whitespace tokenizer. Honors single quotes, double quotes,
/// and backslash escapes. Sufficient for breaking a Simple segment into
/// argv-shaped tokens; the segment has already passed through the splitter
/// so it has no top-level separators.
fn tokenize(input: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut buf = String::new();
    let mut in_single = false;
    let mut in_double = false;
    let mut escape = false;

    for c in input.chars() {
        if escape {
            buf.push(c);
            escape = false;
            continue;
        }
        if in_single {
            buf.push(c);
            if c == '\'' {
                in_single = false;
            }
            continue;
        }
        if in_double {
            buf.push(c);
            if c == '\\' {
                escape = true;
            } else if c == '"' {
                in_double = false;
            }
            continue;
        }
        if c == '\\' {
            buf.push(c);
            escape = true;
            continue;
        }
        if c == '\'' {
            buf.push(c);
            in_single = true;
            continue;
        }
        if c == '"' {
            buf.push(c);
            in_double = true;
            continue;
        }
        if c.is_whitespace() {
            if !buf.is_empty() {
                out.push(std::mem::take(&mut buf));
            }
            continue;
        }
        buf.push(c);
    }
    if !buf.is_empty() {
        out.push(buf);
    }
    out
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
            r#"gh pr create --title "feat: x" --body "..."#,
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
        // Pathological: just env assignments, no actual command.
        // We fall back to the trimmed original.
        check("FOO=bar", "FOO=bar");
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
    fn tokenize_handles_quotes_and_escapes() {
        assert_eq!(tokenize("a b c"), vec!["a", "b", "c"]);
        assert_eq!(tokenize(r#"a "b c" d"#), vec!["a", r#""b c""#, "d"]);
        assert_eq!(tokenize(r"a\ b c"), vec![r"a\ b", "c"]);
        assert_eq!(tokenize("a 'b c' d"), vec!["a", "'b c'", "d"]);
    }

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
    fn canonicalize_agrees_with_suggestion_for_separate_token_flag_arg() {
        // `git -c <val>` carries its argument as a separate token that looks
        // like neither a flag nor an env assignment, so flag-skipping stops
        // there — for *both* functions. They still agree (the suggested
        // prefix is a literal prefix of the canonical form), so no re-prompt
        // loop; this just documents the shared, pre-existing limitation.
        let cmd = "git -c core.pager=less show HEAD";
        let suggested = suggest_prefix(cmd);
        let canonical = canonicalize_for_match(cmd).unwrap();
        assert_eq!(suggested, "git core.pager=less");
        assert!(canonical.starts_with(&suggested));
    }

    #[test]
    fn canonicalize_returns_none_when_nothing_to_strip() {
        // Plain command with no env prefix and no inter-binary flags — the
        // literal match already handles it, so we signal "no change".
        assert_eq!(canonicalize_for_match("git log --oneline -5"), None);
    }

    #[test]
    fn canonicalize_leaves_flat_argument_commands_untouched() {
        // `ls` is Style::None — its leading flags are arguments, not a
        // pre-subcommand region, so suggest_prefix keeps them off the prefix
        // and we must not strip them here either.
        assert_eq!(canonicalize_for_match("ls -la /tmp"), None);
    }

    #[test]
    fn canonicalize_leaves_path_shaped_binary_flags_untouched() {
        // Path-shaped binaries have no subcommand layer; suggest_prefix
        // returns the binary head as-is, so no flag stripping.
        assert_eq!(canonicalize_for_match("~/go/bin/go --foo build"), None);
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
}
