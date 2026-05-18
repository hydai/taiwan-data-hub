//! Embeds the short git SHA of the working tree as `GATEWAY_GIT_SHA`
//! so /healthz and /readyz can report it. Falls back to "unknown"
//! when git is unavailable (e.g. when building from a release tarball).

use std::process::Command;

fn main() {
    // `cargo:rerun-if-changed` paths are resolved relative to the
    // package root (crates/gateway/), so we step up two levels to the
    // workspace root where .git lives. Two files cover the cases:
    //   - .git/HEAD       → branch switches (HEAD itself moves)
    //   - .git/logs/HEAD  → new commits on any branch (the HEAD reflog
    //                       gets an entry; .git/HEAD itself is unchanged
    //                       when committing on a checked-out branch)
    // Together they bust the cache exactly when the embedded SHA would
    // become stale, without triggering a rerun on unrelated edits.
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/logs/HEAD");

    let sha = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map_or_else(|| "unknown".to_owned(), |s| s.trim().to_owned());

    println!("cargo:rustc-env=GATEWAY_GIT_SHA={sha}");
}
