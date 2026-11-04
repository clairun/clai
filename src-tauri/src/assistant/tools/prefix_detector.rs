//! Reads a shell row the ways the ask-first matcher needs.
//!
//! A rule names a command by its leading words (`git push`, `rm`). A row
//! can run that command without starting with those words: behind
//! assignments or a path (`FOO=1 /usr/bin/rm`), behind global options
//! (`git -C repo push`), or as the command a wrapper or an argument runner
//! runs (`timeout 5 rm x`, `xargs rm`). Each function here gives one such
//! reading of a row, so the matcher can try a rule against all of them.

use crate::assistant::tools::command_splitter::{
    argument_command_words, command_words, possible_commands, row_words, with_split_strings,
};

/// Drops the global-option words between `binary` and its subcommand, and
/// returns the rest. Path-shaped binaries keep every word.
///
/// Dropping stops at the first non-option word: the table only lists
/// options documented to precede the subcommand (`git status -C .` is
/// rejected by git), while options after it belong to the subcommand's
/// own grammar (`git log -C 2` sets copy detection), so those stay
/// verbatim.
fn after_global_options<'w, 'a>(binary: &str, rest: &'w [&'a str]) -> &'w [&'a str] {
    if is_path_shaped(binary) {
        return rest;
    }
    let Some(spec) = spec_for(binary) else {
        return skip_global_options(&[], rest);
    };
    let rest = match rest {
        [first, after @ ..] if spec.toolchain && first.starts_with('+') => after,
        _ => rest,
    };
    skip_global_options(spec.options, rest)
}

/// A binary's documented value-taking global options.
struct GlobalOptionSpec {
    /// The binary's name, as it is written at the head of a command.
    binary: &'static str,
    /// The value-taking global options, each spelled as it is written
    /// (`-C`, `--git-dir`, …). A long option also matches its attached
    /// form (`--git-dir=.git`).
    options: &'static [&'static str],
    /// Whether a rustup `+toolchain` word may come first (`cargo +nightly
    /// publish`).
    toolchain: bool,
}

impl GlobalOptionSpec {
    const fn new(binary: &'static str, options: &'static [&'static str]) -> Self {
        Self {
            binary,
            options,
            toolchain: false,
        }
    }
}

/// Python's value options before the script.
const PYTHON_OPTIONS: &[&str] = &["-m", "-W", "-X"];

/// The per-binary option knowledge behind [`after_global_options`] — the
/// only place a binary is named for option-value skipping. Supporting
/// another CLI's documented, subcommand-preceding value options is an
/// entry here, not a parser change: the skipping semantics live once in
/// [`skip_global_options`], which this table merely feeds.
const GLOBAL_OPTION_SPECS: &[GlobalOptionSpec] = &[
    // From git's synopsis (`git --bogus` usage). `-C` and `-c` accept no
    // attached short form (git rejects `-Cdir`); the long options accept
    // both spellings (`--git-dir=x`, `--git-dir x`).
    GlobalOptionSpec::new(
        "git",
        &[
            "-C",
            "-c",
            "--git-dir",
            "--work-tree",
            "--namespace",
            "--config-env",
            "--attr-source",
        ],
    ),
    GlobalOptionSpec::new("python", PYTHON_OPTIONS),
    GlobalOptionSpec::new("python3", PYTHON_OPTIONS),
    GlobalOptionSpec::new("bash", &["-o", "-O", "--rcfile", "--init-file"]),
    GlobalOptionSpec::new("sh", &["-o"]),
    GlobalOptionSpec::new("dash", &["-o"]),
    GlobalOptionSpec::new("ksh", &["-o"]),
    GlobalOptionSpec::new("zsh", &["-o"]),
    // Every option of `kubectl options` that takes a value.
    GlobalOptionSpec::new(
        "kubectl",
        &[
            "--as",
            "--as-group",
            "--as-uid",
            "--as-user-extra",
            "--cache-dir",
            "--certificate-authority",
            "--client-certificate",
            "--client-key",
            "--cluster",
            "--context",
            "--kubeconfig",
            "--kuberc",
            "--log-flush-frequency",
            "-n",
            "--namespace",
            "--password",
            "--profile",
            "--profile-output",
            "--proxy-url",
            "--request-timeout",
            "-s",
            "--server",
            "--tls-server-name",
            "--token",
            "--user",
            "--username",
            "-v",
            "--v",
            "--vmodule",
        ],
    ),
    // `docker --help`.
    GlobalOptionSpec::new(
        "docker",
        &[
            "-c",
            "--context",
            "-H",
            "--host",
            "--config",
            "-l",
            "--log-level",
            "--tlscacert",
            "--tlscert",
            "--tlskey",
        ],
    ),
    GlobalOptionSpec::new("gh", &["-R", "--repo"]),
    // npm takes any of its config options before the command; these are
    // the common ones that take a value, from `npm help config`.
    GlobalOptionSpec::new(
        "npm",
        &[
            "-w",
            "--workspace",
            "-C",
            "--prefix",
            "--userconfig",
            "--registry",
            "--cache",
            "--loglevel",
            "--tag",
            "--otp",
            "--access",
            "-L",
            "--location",
            "--scope",
            "--auth-type",
        ],
    ),
    GlobalOptionSpec::new(
        "pnpm",
        &[
            "-F",
            "--filter",
            "--filter-prod",
            "-C",
            "--dir",
            "--loglevel",
            "--reporter",
        ],
    ),
    GlobalOptionSpec::new("yarn", &["--cwd"]),
    // `cargo --help`.
    GlobalOptionSpec {
        toolchain: true,
        ..GlobalOptionSpec::new("cargo", &["-Z", "-C", "--config", "--color"])
    },
];

/// `binary`'s entry in [`GLOBAL_OPTION_SPECS`], or none when its option
/// table is unknown: an unlisted binary must never have a word consumed.
fn spec_for(binary: &str) -> Option<&'static GlobalOptionSpec> {
    GLOBAL_OPTION_SPECS
        .iter()
        .find(|spec| spec.binary == binary)
}

/// Whether `token` is the value option `name`: the bare name (`--git-dir
/// .git`) or the attached form long options also accept (`--git-dir=.git`).
/// The `=` guard keeps `--git-dir2=x` a different, unknown option.
fn is_option(name: &str, token: &str) -> bool {
    token
        .strip_prefix(name)
        .is_some_and(|tail| tail.is_empty() || tail.starts_with('='))
}

/// Drops the global-option words from the head of `rest` and returns the
/// words from the first non-option word onward. A listed option consumes
/// its value word too, unless the value rides inside the token
/// (`--git-dir=x`); every other option — boolean, or unknown to the
/// table — is dropped alone, keeping the next word, the subcommand
/// candidate, intact.
fn skip_global_options<'w, 'a>(value_options: &[&str], rest: &'w [&'a str]) -> &'w [&'a str] {
    let mut rest = rest;
    while let [first, ..] = rest {
        if !is_flag(first) {
            break;
        }
        // A value option written without `=` takes its value as the next
        // word; a boolean or unknown option, or one with `=`, goes alone.
        let takes_next =
            value_options.iter().any(|name| is_option(name, first)) && !first.contains('=');
        let skip = if takes_next { 2 } else { 1 };
        rest = rest.get(skip..).unwrap_or(&[]);
    }
    rest
}

/// The form a rule matches against besides the row as written: the
/// command's words without leading `VAR=value` assignments, redirects and
/// the global options between the binary and its first subcommand, so a
/// rule cannot be dodged through the option region: `git -c e=v push` and
/// `git -C repo push` read as `git push`. Every word from the subcommand
/// on is kept. `None` when `segment` is not one command.
pub fn canonicalize_for_deny(segment: &str) -> Option<String> {
    let words = command_words(segment)?;
    let (binary, rest) = words.split_first()?;
    Some(
        std::iter::once(*binary)
            .chain(after_global_options(binary, rest).iter().copied())
            .collect::<Vec<_>>()
            .join(" "),
    )
}

/// The method `gh api` requests in `reading`, in both spellings a rule may
/// name it with (`gh api -X DELETE`, `gh api --method DELETE`). gh takes
/// the option anywhere among its arguments, as `-X M`, `-XM`, `--method M`
/// or `--method=M`, more places and spellings than a prefix rule can list:
/// `gh api repos/o/r -XDELETE` gives both forms with `DELETE`.
pub fn gh_api_method_forms(reading: &str) -> Vec<String> {
    let Some(canonical) = canonicalize_for_deny(reading) else {
        return Vec::new();
    };
    let words: Vec<&str> = canonical.split_whitespace().collect();
    let ["gh", "api", args @ ..] = words.as_slice() else {
        return Vec::new();
    };
    let mut forms = Vec::new();
    for (at, word) in args.iter().enumerate() {
        let method = match *word {
            "-X" | "--method" => args.get(at + 1).copied(),
            _ => word
                .strip_prefix("--method=")
                .or_else(|| word.strip_prefix("-X")),
        };
        let Some(method) = method
            .map(|method| method.trim_start_matches('=').trim_matches(['\'', '"']))
            .filter(|method| !method.is_empty())
        else {
            continue;
        };
        let method = method.to_ascii_uppercase();
        forms.push(format!("gh api -X {method}"));
        forms.push(format!("gh api --method {method}"));
    }
    forms
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
pub fn by_argument_command(segment: &str) -> Option<String> {
    Some(by_name(&argument_command_words(segment)?))
}

/// The `find` actions that run a command for each match.
const FIND_ACTIONS: &[&str] = &["-exec", "-execdir", "-ok", "-okdir"];

/// The commands a `find` row runs for its matches, one per action, joined
/// like [`by_run_command`]: `find . -name x -exec rm {} +` → `rm {}`.
/// Empty when `segment` is not a `find` command.
pub fn by_find_action(segment: &str) -> Vec<String> {
    let Some(words) = command_words(segment) else {
        return Vec::new();
    };
    let Some((name, args)) = words.split_first() else {
        return Vec::new();
    };
    if name.rsplit('/').next() != Some("find") {
        return Vec::new();
    }
    args.iter()
        .enumerate()
        .filter(|(_, word)| FIND_ACTIONS.contains(word))
        .filter_map(|(at, _)| {
            let command = &args[at + 1..];
            let end = command
                .iter()
                .position(|word| matches!(*word, ";" | "\\;" | "';'" | "\";\"" | "+"))
                .unwrap_or(command.len());
            (end > 0).then(|| by_name(&command[..end]))
        })
        .collect()
}

/// The commands `segment` may run under every reading of its wrappers'
/// and runners' options and of the strings `env -S` splits
/// ([`possible_commands`]), each joined like [`by_run_command`], once.
pub fn by_any_reading(segment: &str) -> Vec<String> {
    let Some(words) = row_words(segment) else {
        return Vec::new();
    };
    let split = with_split_strings(&words);
    let split: Vec<&str> = split.iter().map(String::as_str).collect();
    let mut commands: Vec<String> = possible_commands(&words)
        .into_iter()
        .chain(possible_commands(&split))
        .map(by_name)
        .collect();
    commands.sort_unstable();
    commands.dedup();
    commands
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

#[cfg(test)]
mod tests {
    use super::*;

    fn deny_form(segment: &str) -> Option<String> {
        canonicalize_for_deny(segment)
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

    #[test]
    fn deny_form_drops_assignments_and_global_options() {
        for (segment, form) in [
            ("git log --oneline -5", "git log --oneline -5"),
            ("git --no-pager log --oneline -5", "git log --oneline -5"),
            ("FOO=bar BAZ=1 git log --oneline", "git log --oneline"),
            ("GIT_PAGER=cat git --no-pager show HEAD", "git show HEAD"),
            ("git -c core.pager=less show HEAD", "git show HEAD"),
            ("git -C repo status --short", "git status --short"),
            ("git --git-dir=.git log --oneline", "git log --oneline"),
            ("git --git-dir .git log --oneline", "git log --oneline"),
            ("git --no-pager -C repo status -s", "git status -s"),
            ("git --config-env=core.fsmonitor=FSMON status", "git status"),
            ("git --config-env core.fsmonitor=FSMON status", "git status"),
            ("FOO=bar git -C repo status", "git status"),
            (
                "timeout 600 git -C repo status --short",
                "git status --short",
            ),
            ("timeout 600 cargo build 2>&1", "cargo build"),
            ("FOO=bar ~/go/bin/go build", "~/go/bin/go build"),
        ] {
            assert_eq!(deny_form(segment).as_deref(), Some(form), "{segment}");
        }
    }

    #[test]
    fn deny_form_keeps_what_is_not_a_global_option() {
        // Options after the subcommand belong to it: `git log -C 2` sets
        // copy detection, and git rejects `git status -C .`.
        assert_eq!(
            deny_form("git status -C repo").as_deref(),
            Some("git status -C repo")
        );
        // A path-shaped binary has no option region.
        assert_eq!(
            deny_form("~/go/bin/go --foo build").as_deref(),
            Some("~/go/bin/go --foo build")
        );
        // An unlisted binary's option table is unknown: the word after an
        // option could be its value or its subcommand, so only the option
        // token goes.
        assert_eq!(
            deny_form("tool -C repo status").as_deref(),
            Some("tool repo status")
        );
        assert_eq!(deny_form("FOO=bar BAZ=1"), None);
    }

    #[test]
    fn spec_entries_alone_drive_option_skipping() {
        // The extensibility contract of GLOBAL_OPTION_SPECS: the next
        // documented CLI is an entry of the same shape, not a parser
        // change.
        let docker: &[&str] = &["--context", "-H", "--log-config"];
        assert_eq!(
            skip_global_options(docker, &["--context", "prod", "ps", "-a"]),
            &["ps", "-a"][..]
        );
        assert_eq!(
            skip_global_options(docker, &["--debug", "--context=prod", "ps"]),
            &["ps"][..]
        );
        assert_eq!(
            skip_global_options(docker, &["--log-config", "/bin/evil", "ps"]),
            &["ps"][..]
        );
        assert_eq!(
            skip_global_options(docker, &["--context", "prod"]),
            &[] as &[&str]
        );
        // `--context2=x` is a different, unknown option: it goes alone.
        assert_eq!(
            skip_global_options(docker, &["--context2", "prod", "ps"]),
            &["prod", "ps"][..]
        );
        // A binary without an entry has an unknown option table: no word
        // is ever consumed.
        assert!(spec_for("no-such-tool").is_none());
    }

    #[test]
    fn deny_form_drops_the_global_options_of_listed_clis() {
        for (segment, form) in [
            ("kubectl -n ns delete pod x", "kubectl delete pod x"),
            (
                "kubectl --context prod delete pod x",
                "kubectl delete pod x",
            ),
            ("kubectl -n ns get pods", "kubectl get pods"),
            (
                "kubectl --certificate-authority ca.crt delete pod x",
                "kubectl delete pod x",
            ),
            (
                "kubectl --kuberc f --tls-server-name n delete pod x",
                "kubectl delete pod x",
            ),
            ("docker --context x push img", "docker push img"),
            ("docker -H tcp://x push img", "docker push img"),
            ("gh -R o/r release create v1", "gh release create v1"),
            ("gh --repo=o/r release list", "gh release list"),
            ("npm --prefix x publish", "npm publish"),
            ("npm -w pkg publish", "npm publish"),
            ("npm --tag beta --otp 123456 publish", "npm publish"),
            ("npm --access public -L project publish", "npm publish"),
            ("pnpm --filter x publish", "pnpm publish"),
            ("pnpm -C dir publish", "pnpm publish"),
            ("yarn --cwd dir npm publish", "yarn npm publish"),
            ("cargo +nightly publish", "cargo publish"),
            ("cargo -Z unstable publish", "cargo publish"),
            ("cargo +nightly -Z unstable publish", "cargo publish"),
        ] {
            assert_eq!(deny_form(segment).as_deref(), Some(form), "{segment}");
        }
        // Only cargo takes a `+toolchain` word.
        assert_eq!(deny_form("date +%s").as_deref(), Some("date +%s"));
        assert_eq!(deny_form("git +x push").as_deref(), Some("git +x push"));
    }

    #[test]
    fn gh_api_method_is_read_in_every_spelling_and_place() {
        let delete = vec![
            "gh api -X DELETE".to_string(),
            "gh api --method DELETE".to_string(),
        ];
        for reading in [
            "gh api -X DELETE repos/o/r",
            "gh api -XDELETE repos/o/r",
            "gh api --method DELETE repos/o/r",
            "gh api --method=DELETE repos/o/r",
            "gh api -X=DELETE repos/o/r",
            "gh api repos/o/r -X DELETE",
            "gh api repos/o/r --method delete",
            "gh -R o/r api repos/o/r -X 'DELETE'",
        ] {
            assert_eq!(gh_api_method_forms(reading), delete, "{reading}");
        }
        assert!(gh_api_method_forms("gh api repos/o/r").is_empty());
        assert!(gh_api_method_forms("gh pr view -X DELETE").is_empty());
    }

    #[test]
    fn code_argument_is_named_by_its_written_words() {
        // The code inside the argument is not looked into.
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
    fn by_find_action_reads_the_command_find_runs() {
        for (segment, commands) in [
            ("find . -exec rm {} +", &["rm {}"][..]),
            ("find . -name x -execdir rm -f {} \\;", &["rm -f {}"]),
            ("/usr/bin/find . -ok rm {} ';'", &["rm {}"]),
            ("timeout 5 find . -exec /bin/rm {} +", &["rm {}"]),
            (
                "find . -exec chmod 600 {} + -exec rm {} +",
                &["chmod 600 {}", "rm {}"],
            ),
            ("find . -name x", &[]),
            ("find . -exec", &[]),
            ("echo -exec rm", &[]),
        ] {
            assert_eq!(by_find_action(segment), commands, "{segment}");
        }
    }

    #[test]
    fn by_argument_command_reads_through_runners() {
        assert_eq!(
            by_argument_command("xargs -0 rm -f").as_deref(),
            Some("rm -f")
        );
        assert_eq!(by_argument_command("ls -la"), None);
    }
}
