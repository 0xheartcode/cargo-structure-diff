//! Corpus definition and the first-parent pair walker.
//!
//! The corpus is a fixed set of real-world Rust repos we replay merged PRs from. The walker is the
//! unit-testable core: given a cloned repo it yields the latest N `(parent, commit)` pairs along
//! the default branch first-parent chain. Cloning is runtime-only; nothing here touches the
//! network at build or test time.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};

/// A single first-parent step: the pre-change commit and the commit that produced it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitPair {
    /// The first parent, i.e. `commit^`. This is the base side of the diff.
    pub parent: String,
    /// The commit on the first-parent chain. This is the head side of the diff.
    pub commit: String,
}

/// A repo in the validation corpus.
#[derive(Debug, Clone, Copy)]
pub struct Repo {
    /// Short name, also the on-disk directory under the cache dir.
    pub name: &'static str,
    /// Clone URL.
    pub url: &'static str,
}

/// The fixed corpus: a few large, active Rust codebases with plenty of real refactors.
pub const CORPUS: &[Repo] = &[
    Repo {
        name: "tokio",
        url: "https://github.com/tokio-rs/tokio",
    },
    Repo {
        name: "ripgrep",
        url: "https://github.com/BurntSushi/ripgrep",
    },
    Repo {
        name: "bat",
        url: "https://github.com/sharkdp/bat",
    },
];

/// How many pairs we replay per repo by default (SPEC.md section 6: several hundred).
pub const DEFAULT_PAIR_COUNT: usize = 300;

/// The persistent clone cache. Honours `CSD_HARNESS_CACHE`, else `~/.cache/csd-harness`.
pub fn cache_dir() -> PathBuf {
    if let Ok(dir) = env::var("CSD_HARNESS_CACHE") {
        return PathBuf::from(dir);
    }
    let home = env::var("HOME").unwrap_or_else(|_| ".".to_string());
    Path::new(&home).join(".cache").join("csd-harness")
}

/// Ensure `repo` is cloned under `cache`, returning its working-tree path.
///
/// If a clone already exists the existing checkout is reused, so reruns are cheap. Fresh clones
/// use `--filter=blob:none` to skip historical blob content we do not need. This is the only
/// function here that hits the network, and only on a cache miss.
pub fn ensure_cloned(repo: &Repo, cache: &Path) -> Result<PathBuf> {
    let dest = cache.join(repo.name);
    if dest.join(".git").is_dir() {
        return Ok(dest);
    }
    fs::create_dir_all(cache)
        .with_context(|| format!("failed to create cache dir {}", cache.display()))?;
    let status = Command::new("git")
        .args(["clone", "--filter=blob:none", repo.url])
        .arg(&dest)
        .status()
        .with_context(|| format!("failed to spawn git clone for {}", repo.name))?;
    if !status.success() {
        bail!("git clone of {} failed", repo.url);
    }
    Ok(dest)
}

/// Yield the latest `n` first-parent `(parent, commit)` pairs reachable from `rev`.
///
/// Pairs come back most-recent first, so the first entry's `commit` is the tip of `rev`. A repo
/// with fewer than `n + 1` commits on the chain simply yields fewer pairs. `n == 0` yields none.
pub fn first_parent_pairs(repo: &Path, rev: &str, n: usize) -> Result<Vec<CommitPair>> {
    if n == 0 {
        return Ok(Vec::new());
    }
    // n pairs need n + 1 commits on the chain, since each pair links two adjacent commits.
    let limit = n.saturating_add(1).to_string();
    let out = git_stdout(repo, &["rev-list", "--first-parent", "-n", &limit, rev])?;
    let shas: Vec<&str> = out.lines().filter(|l| !l.is_empty()).collect();
    // rev-list --first-parent prints newest first and only follows first parents, so adjacent
    // entries are first-parent linked: shas[i + 1] is the parent of shas[i].
    let pairs = shas
        .windows(2)
        .map(|w| CommitPair {
            parent: w[1].to_string(),
            commit: w[0].to_string(),
        })
        .collect();
    Ok(pairs)
}

/// Run git in `repo` and return its stdout, erroring on a non-zero exit.
fn git_stdout(repo: &Path, args: &[&str]) -> Result<String> {
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

    /// Run git in `repo` and assert success.
    fn git(repo: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .status()
            .expect("failed to spawn git");
        assert!(status.success(), "git {} failed", args.join(" "));
    }

    /// Capture git stdout in `repo`, trimmed.
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

    /// Add an empty commit so we get a linear, merge-free history.
    fn commit(repo: &Path, msg: &str) {
        git(
            repo,
            &[
                "-c",
                "user.email=test@example.invalid",
                "-c",
                "user.name=test",
                "commit",
                "--allow-empty",
                "-q",
                "-m",
                msg,
            ],
        );
    }

    #[test]
    fn walks_linear_first_parent_pairs() {
        let dir = env::temp_dir().join(format!("csd-harness-walk-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        git(&dir, &["init", "-q"]);
        for msg in ["c1", "c2", "c3", "c4"] {
            commit(&dir, msg);
        }

        let head = git_out(&dir, &["rev-parse", "HEAD"]);

        // Four linear commits give three consecutive pairs, newest first.
        let all = first_parent_pairs(&dir, "HEAD", DEFAULT_PAIR_COUNT).unwrap();
        assert_eq!(all.len(), 3, "4 commits should yield 3 pairs");
        assert_eq!(all[0].commit, head, "first pair should be the tip");
        for pair in &all {
            let expected_parent = git_out(&dir, &["rev-parse", &format!("{}^", pair.commit)]);
            assert_eq!(pair.parent, expected_parent, "parent must be commit^");
        }
        // Newest first: each pair's parent is the next pair's commit.
        assert_eq!(all[0].parent, all[1].commit);
        assert_eq!(all[1].parent, all[2].commit);

        // The N limit selects the latest pairs only.
        let two = first_parent_pairs(&dir, "HEAD", 2).unwrap();
        assert_eq!(two.len(), 2);
        assert_eq!(two, all[..2]);

        let one = first_parent_pairs(&dir, "HEAD", 1).unwrap();
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].commit, head);

        assert!(first_parent_pairs(&dir, "HEAD", 0).unwrap().is_empty());

        fs::remove_dir_all(&dir).unwrap();
    }
}
