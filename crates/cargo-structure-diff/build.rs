//! Embed the git commit and its date into the binary for the version string.
//!
//! Both are deterministic per commit (the commit date, not the wall-clock build time), so a given
//! commit always produces the same version line. When `.git` is absent (for example a crates.io
//! tarball), the values are empty and the version falls back to the bare package version.

use std::process::Command;

fn main() {
    // Rebuild when HEAD moves so the embedded commit stays current. Best effort: the paths are
    // relative to this crate dir (two levels below the repo root) and simply may not exist.
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/refs");

    let sha = git(&["rev-parse", "--short", "HEAD"]);
    let date = git(&["show", "-s", "--format=%cs", "HEAD"]);
    println!("cargo:rustc-env=CSD_GIT_SHA={sha}");
    println!("cargo:rustc-env=CSD_GIT_DATE={date}");
}

/// Run git and return trimmed stdout, or an empty string on any failure.
fn git(args: &[&str]) -> String {
    Command::new("git")
        .args(args)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}
