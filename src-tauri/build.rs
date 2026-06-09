use std::path::Path;
use std::process::Command;

fn main() {
    // Bake a `git describe` string (e.g. "v26.6.7-38-g6148106") into the
    // binary so the About page can show how far a dev build is past the last
    // release tag. Best-effort: in tarball / vendored builds there's no `.git`
    // (or no `git`), so the var is simply absent and the command falls back to
    // the crate version.
    if let Ok(output) = Command::new("git")
        .args(["describe", "--tags", "--always", "--dirty"])
        .output()
    {
        if output.status.success() {
            let describe = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !describe.is_empty() {
                println!("cargo:rustc-env=CLAI_GIT_DESCRIBE={describe}");
            }
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
