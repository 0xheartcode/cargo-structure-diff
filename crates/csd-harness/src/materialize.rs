//! Materialize a commit into a throwaway worktree, extract its IR, and cache the result by SHA.
//!
//! A SHA appears in up to two first-parent pairs (as one pair's `commit` and the next pair's
//! `parent`), so caching the extracted [`Graph`] keyed by SHA halves extraction cost across the
//! pair set (SPEC.md section 6). Worktrees are created outside the repo under the system temp dir
//! and are always torn down, even when extraction fails.
//!
//! The cache has two levels. First is the in-memory [`Cache`], which lives for one harness run.
//! Behind it sits a persistent on-disk cache: each extracted graph is serialized as JSON under
//! `<cache_dir>/ir/<sha>.json` (see [`corpus::cache_dir`]). A SHA names immutable content, so a
//! cached entry is valid forever and never needs invalidation, letting reruns skip the expensive
//! worktree checkout plus extraction entirely. Disk writes are atomic (write a temp file, then
//! rename) so a crash cannot leave a truncated file that later reads back as valid-but-empty.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use csd_ir::Graph;

use crate::corpus;

/// In-memory IR cache keyed by commit SHA.
///
/// Thin wrapper over a `HashMap<String, Graph>`. It is the first level in front of the persistent
/// on-disk cache and lives for one harness run.
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

/// Extract the IR for `sha` in `repo`, using `cache` and the on-disk cache to avoid re-extracting.
///
/// Lookups go in-memory first, then disk, then extract. On an in-memory hit the cached [`Graph`]
/// is cloned and returned. On a disk hit the JSON at `<cache_dir>/ir/<sha>.json` is deserialized,
/// promoted into `cache`, and returned, skipping the worktree checkout entirely. On a full miss
/// the commit is checked out into a detached worktree under the system temp dir, extracted,
/// written to disk, and promoted into `cache`; the worktree is always removed before returning,
/// so a failed extraction never leaks a worktree. Because [`csd_extract_rs::extract`] normalizes
/// its graph, a cached graph equals a freshly extracted one.
pub fn ir_for_sha(repo: &Path, sha: &str, cache: &mut Cache) -> Result<Graph> {
    if let Some(graph) = cache.graphs.get(sha) {
        return Ok(graph.clone());
    }
    let dir = ir_cache_dir();
    if let Some(graph) = read_from_disk(&dir, sha)? {
        cache.graphs.insert(sha.to_string(), graph.clone());
        return Ok(graph);
    }
    let graph = extract_at_sha(repo, sha)?;
    write_to_disk(&dir, sha, &graph)?;
    cache.graphs.insert(sha.to_string(), graph.clone());
    Ok(graph)
}

/// The on-disk IR cache directory, `<cache_dir>/ir` (honours `CSD_HARNESS_CACHE`).
fn ir_cache_dir() -> PathBuf {
    corpus::cache_dir().join("ir")
}

/// The JSON path for a SHA under `dir`.
fn ir_cache_path(dir: &Path, sha: &str) -> PathBuf {
    dir.join(format!("{sha}.json"))
}

/// Read and deserialize the cached graph for `sha` under `dir`, if the file exists.
///
/// Returns `Ok(None)` when nothing is cached yet. A present-but-unreadable or malformed file is an
/// error rather than a silent miss, so corruption surfaces instead of being masked by re-extraction.
fn read_from_disk(dir: &Path, sha: &str) -> Result<Option<Graph>> {
    let path = ir_cache_path(dir, sha);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(err).with_context(|| format!("failed to read IR cache {}", path.display()))
        }
    };
    let graph = serde_json::from_slice(&bytes)
        .with_context(|| format!("failed to parse IR cache {}", path.display()))?;
    Ok(Some(graph))
}

/// Serialize `graph` to `<dir>/<sha>.json` atomically: write a temp file, then rename over it.
///
/// The rename is atomic on a POSIX filesystem, so a crash mid-write leaves either the old file or
/// none, never a truncated JSON that would later deserialize as a valid-but-empty graph.
fn write_to_disk(dir: &Path, sha: &str, graph: &Graph) -> Result<()> {
    std::fs::create_dir_all(dir)
        .with_context(|| format!("failed to create IR cache dir {}", dir.display()))?;
    let json = serde_json::to_vec(graph).context("failed to serialize IR graph")?;
    // Temp name is keyed by SHA and PID so concurrent writers do not clobber each other's temp.
    let tmp = dir.join(format!(".{}.{}.tmp", sha, std::process::id()));
    std::fs::write(&tmp, &json)
        .with_context(|| format!("failed to write IR cache temp {}", tmp.display()))?;
    let final_path = ir_cache_path(dir, sha);
    std::fs::rename(&tmp, &final_path).with_context(|| {
        format!(
            "failed to rename IR cache temp into {}",
            final_path.display()
        )
    })?;
    Ok(())
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

        // Point the on-disk IR cache at the temp dir so the test never writes the real ~/.cache.
        std::env::set_var("CSD_HARNESS_CACHE", dir.join("cache"));

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

    /// A small synthetic graph exercising nodes, attrs, a fingerprint, and an edge.
    fn sample_graph() -> Graph {
        use csd_ir::{EdgeKind, Fingerprint, NodeKind, SourceSpan, StableId};

        let span = SourceSpan {
            file: "src/lib.rs".into(),
            start: 0,
            end: 1,
        };
        let mut typed = csd_ir::Node {
            id: StableId::new("crate::A"),
            kind: NodeKind::Struct,
            span: span.clone(),
            attrs: Default::default(),
            fingerprint: Some(Fingerprint {
                members: ["id", "total"].iter().map(|s| s.to_string()).collect(),
                ..Default::default()
            }),
        };
        typed.attrs.insert("vis".into(), "pub".into());
        let plain = csd_ir::Node {
            id: StableId::new("crate::B"),
            kind: NodeKind::Module,
            span: span.clone(),
            attrs: Default::default(),
            fingerprint: None,
        };
        let mut g = Graph::new();
        g.nodes = vec![typed, plain];
        g.edges = vec![csd_ir::Edge {
            from: StableId::new("crate::A"),
            to: StableId::new("crate::B"),
            kind: EdgeKind::Uses,
            span,
            ordinal: None,
        }];
        g.normalize();
        g
    }

    #[test]
    fn disk_cache_round_trips_a_graph() {
        // A dedicated temp dir stands in for `<cache_dir>/ir`; nothing touches the real ~/.cache.
        let dir = std::env::temp_dir().join(format!("csd-harness-ir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);

        let sha = "0123456789abcdef0123456789abcdef01234567";

        // A miss returns None before anything is written.
        assert!(
            read_from_disk(&dir, sha).unwrap().is_none(),
            "an empty cache must miss"
        );

        let graph = sample_graph();
        write_to_disk(&dir, sha, &graph).unwrap();

        // The file lands at <dir>/<sha>.json and no temp file is left behind.
        assert!(
            ir_cache_path(&dir, sha).is_file(),
            "the json file must exist"
        );
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .filter(|n| n.to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "no temp files should remain: {leftovers:?}"
        );

        // Read back and assert byte-for-byte equality with the written graph.
        let back = read_from_disk(&dir, sha)
            .unwrap()
            .expect("a written sha must hit");
        assert_eq!(graph, back, "the cached graph must equal what was written");

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
