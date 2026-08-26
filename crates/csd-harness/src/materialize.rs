//! Materialize a commit into a throwaway worktree, extract its IR, and cache the result by SHA.
//!
//! A SHA appears in up to two first-parent pairs (as one pair's `commit` and the next pair's
//! `parent`), so caching the extracted [`Graph`] keyed by SHA halves extraction cost across the
//! pair set (SPEC.md section 6). Worktrees are created outside the repo under the system temp dir
//! and are always torn down, even when extraction fails.
//!
//! The cache is in-memory for M0. An on-disk IR cache keyed by SHA (surviving across runs) is a
//! later option and is not implemented here.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use csd_ir::Graph;

/// In-memory IR cache keyed by commit SHA.
///
/// Thin wrapper over a `HashMap<String, Graph>`. Lives for one harness run; nothing is persisted.
#[derive(Debug, Default)]
pub struct Cache {
    graphs: HashMap<String, Graph>,
}

impl Cache {
    /// An empty cache.
    pub fn new() -> Self {
        Self::default()
    }
}

/// Extract the IR for `sha` in `repo`, using `cache` to avoid re-extracting a repeated SHA.
///
/// On a cache hit the cached [`Graph`] is cloned and returned (cheap relative to a worktree
/// checkout plus extraction). On a miss the commit is checked out into a detached worktree under
/// the system temp dir, extracted, then the worktree is always removed before returning, so a
/// failed extraction never leaks a worktree.
pub fn ir_for_sha(repo: &Path, sha: &str, cache: &mut Cache) -> Result<Graph> {
    if let Some(graph) = cache.graphs.get(sha) {
        return Ok(graph.clone());
    }
    let graph = extract_at_sha(repo, sha)?;
    cache.graphs.insert(sha.to_string(), graph.clone());
    Ok(graph)
}

/// Check out `sha` into a fresh detached worktree, extract it, and always clean up afterwards.
fn extract_at_sha(repo: &Path, sha: &str) -> Result<Graph> {
    let worktree = worktree_path(sha);
    git(
        repo,
        &[
            "worktree",
            "add",
            "--detach",
            &worktree.to_string_lossy(),
            sha,
        ],
    )
    .with_context(|| format!("failed to add worktree for {sha}"))?;

    // Extract with cleanup guaranteed: capture the result, tear the worktree down, then surface it.
    let result = csd_extract_rs::extract(&worktree)
        .with_context(|| format!("failed to extract IR for {sha}"));
    cleanup(repo, &worktree);
    result
}

/// Remove the worktree from git's registry and delete its directory. Best-effort: cleanup runs on
/// the error path too, so failures here must not mask the original error and are ignored.
fn cleanup(repo: &Path, worktree: &Path) {
    let _ = git(
        repo,
        &["worktree", "remove", "--force", &worktree.to_string_lossy()],
    );
    let _ = std::fs::remove_dir_all(worktree);
}

/// A unique worktree path under the system temp dir. Keyed by SHA and PID so concurrent runs and
/// repeated SHAs within a run do not collide, and it lives outside the repo by construction.
fn worktree_path(sha: &str) -> PathBuf {
    std::env::temp_dir().join(format!("csd-harness-wt-{}-{}", std::process::id(), sha))
}

/// Run git in `repo` and return its stdout, erroring on a non-zero exit.
fn git(repo: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .with_context(|| format!("failed to spawn git in {}", repo.display()))?;
    if !out.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    String::from_utf8(out.stdout).context("git output was not utf-8")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Run git in `repo` with an inline throwaway identity and assert success.
    fn git_ok(repo: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args([
                "-c",
                "user.email=test@example.invalid",
                "-c",
                "user.name=test",
            ])
            .args(args)
            .status()
            .expect("failed to spawn git");
        assert!(status.success(), "git {} failed", args.join(" "));
    }

    /// Capture trimmed git stdout in `repo`.
    fn git_out(repo: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .output()
            .expect("failed to spawn git");
        assert!(out.status.success(), "git {} failed", args.join(" "));
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    /// Write `src/lib.rs` with the given body under `repo`.
    fn write_lib(repo: &Path, body: &str) {
        std::fs::create_dir_all(repo.join("src")).unwrap();
        std::fs::write(repo.join("src").join("lib.rs"), body).unwrap();
    }

    fn ids(graph: &Graph) -> Vec<String> {
        graph
            .nodes
            .iter()
            .map(|n| n.id.as_str().to_string())
            .collect()
    }

    #[test]
    fn materializes_and_caches_ir_per_sha() {
        let dir = std::env::temp_dir().join(format!("csd-harness-mat-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        git_ok(&dir, &["init", "-q"]);

        write_lib(&dir, "pub struct A;\n");
        git_ok(&dir, &["add", "-A"]);
        git_ok(&dir, &["commit", "-q", "-m", "add A"]);
        let sha1 = git_out(&dir, &["rev-parse", "HEAD"]);

        write_lib(&dir, "pub struct A;\npub struct B;\n");
        git_ok(&dir, &["add", "-A"]);
        git_ok(&dir, &["commit", "-q", "-m", "add B"]);
        let sha2 = git_out(&dir, &["rev-parse", "HEAD"]);

        let mut cache = Cache::new();
        assert!(cache.graphs.is_empty(), "a fresh cache is empty");

        // First SHA: A present, B absent.
        let g1 = ir_for_sha(&dir, &sha1, &mut cache).unwrap();
        let g1_ids = ids(&g1);
        assert!(g1_ids.iter().any(|i| i == "crate::A"), "expected crate::A");
        assert!(
            !g1_ids.iter().any(|i| i == "crate::B"),
            "crate::B must not exist at sha1"
        );

        // Second SHA: both A and B present.
        let g2 = ir_for_sha(&dir, &sha2, &mut cache).unwrap();
        let g2_ids = ids(&g2);
        assert!(g2_ids.iter().any(|i| i == "crate::A"), "expected crate::A");
        assert!(g2_ids.iter().any(|i| i == "crate::B"), "expected crate::B");

        assert_eq!(cache.graphs.len(), 2, "two distinct SHAs should be cached");

        // Cache hit: same SHA returns an equal graph and adds no worktree.
        let g1_again = ir_for_sha(&dir, &sha1, &mut cache).unwrap();
        assert_eq!(g1, g1_again, "cached graph must equal the first extraction");
        assert_eq!(
            cache.graphs.len(),
            2,
            "a repeat SHA must not grow the cache"
        );

        // No dangling worktrees: only the main tree remains.
        let list = git_out(&dir, &["worktree", "list"]);
        assert_eq!(
            list.lines().count(),
            1,
            "only the main worktree should remain, got:\n{list}"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
