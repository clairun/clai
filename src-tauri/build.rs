use std::path::Path;
use std::process::Command;

fn main() {
    // Bake a `git describe` string (e.g. "v26.6.7-38-g6148106") into the
    // binary so the About page can show how far a dev build is past the last
    // release tag. Best-effort: in tarball / vendored builds there's no `.git`
    // (or no `git`), so the var is simply absent and the command falls back to
    // the crate version.
    let described = describe_head();
    if let Ok(describe) = &described {
        println!("cargo:rustc-env=CLAI_GIT_DESCRIBE={describe}");
    }

    // Report the outcome in CI logs. v26.8.1 shipped a Windows installer
    // stamped `26.8.1-dirty` while its Linux and macOS binaries baked no
    // describe string at all — one tag, three platforms, three results — and
    // nothing in the build log said which of those a job had produced, so the
    // difference only surfaced by unpacking the published artifacts. Gated on
    // `CI` to keep contributor builds quiet.
    if std::env::var_os("CI").is_some() {
        match &described {
            Ok(describe) => println!("cargo:warning=CLAI_GIT_DESCRIBE={describe}"),
            Err(reason) => println!("cargo:warning=CLAI_GIT_DESCRIBE unset: {reason}"),
        }
    }

    // Re-run when the checkout's HEAD/refs move so the baked value tracks the
    // current commit. Only watch these when `.git` exists — pointing
    // rerun-if-changed at a missing path would force the script to re-run on
    // every build (e.g. vendored source trees).
    if Path::new("../.git/HEAD").exists() {
        println!("cargo:rerun-if-changed=../.git/HEAD");
        println!("cargo:rerun-if-changed=../.git/refs");
        println!("cargo:rerun-if-changed=../.git/packed-refs");
    }

    tauri_build::build()
}

/// `git describe` for HEAD, or the reason it produced nothing usable.
///
/// The error side exists purely so the `CI` branch above can log *why* no
/// version was baked; every failure mode is a legitimate build configuration
/// (no git, no `.git`, no commits) and none of them is fatal.
fn describe_head() -> Result<String, String> {
    let output = Command::new("git")
        .args(["describe", "--tags", "--always", "--dirty"])
        .output()
        .map_err(|err| format!("could not run git: {err}"))?;
    if !output.status.success() {
        return Err(format!(
            "git describe failed ({}): {}",
            output.status,
            one_line(&String::from_utf8_lossy(&output.stderr))
        ));
    }
    let describe = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if describe.is_empty() {
        return Err("git describe printed nothing".to_string());
    }
    Ok(describe)
}

/// Collapse whitespace so multi-line git stderr survives the trip through
/// `cargo:warning`: cargo shows only the first line of the value and drops the
/// rest without `-vv`.
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}
