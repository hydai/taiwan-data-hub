//! Embeds the short git SHA of the working tree as `GATEWAY_GIT_SHA`
//! so /healthz and /readyz can report it. Falls back to "unknown"
//! when git is unavailable (e.g. when building from a release tarball).

use std::process::Command;

fn main() {
    // Re-run only when HEAD moves; the contents-of-tree case is handled
    // by the dependency on .git/HEAD's pointer file when on a branch.
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/index");

    let sha = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map_or_else(|| "unknown".to_owned(), |s| s.trim().to_owned());

    println!("cargo:rustc-env=GATEWAY_GIT_SHA={sha}");
}
