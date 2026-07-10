//! Build script: embeds a build identifier into the binary so the running build
//! is always identifiable (shown in the tray menu).
//!
//! Resolution order:
//! 1. `DRIVE_SYNC_BUILD` env at build time (set by the deploy step — works even
//!    when the source was rsync'd without a `.git`, e.g. the Arch build tree).
//! 2. `git rev-parse --short HEAD` (+ `-dirty`) when built inside a git repo.
//! 3. `unknown` as a last resort.

use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=DRIVE_SYNC_BUILD");

    let build = std::env::var("DRIVE_SYNC_BUILD")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(git_describe)
        .unwrap_or_else(|| "unknown".to_string());

    println!("cargo:rustc-env=DRIVE_SYNC_BUILD={build}");
}

fn git_describe() -> Option<String> {
    let out = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let hash = String::from_utf8(out.stdout).ok()?.trim().to_string();
    if hash.is_empty() {
        return None;
    }
    let dirty = Command::new("git")
        .args(["status", "--porcelain"])
        .output()
        .ok()
        .is_some_and(|o| !o.stdout.is_empty());
    Some(if dirty { format!("{hash}-dirty") } else { hash })
}
