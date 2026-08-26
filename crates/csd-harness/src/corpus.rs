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
use csd_diff::FileRename;

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

/// The reliable module-level ground truth: git's file renames between `base` and `head`.
///
/// Runs `git diff -M50 --name-status base head` and keeps only rename (`R`) entries. Git reports
/// FILE renames only, never type-level renames, so this is trustworthy ground truth for the
/// module-move reconciliation sanity check but not for the type-level split/merge signals.
pub fn file_renames(repo: &Path, base: &str, head: &str) -> Result<Vec<FileRename>> {
    let out = git_stdout(repo, &["diff", "-M50", "--name-status", base, head])?;
    let mut renames = Vec::new();
    for line in out.lines() {
        // A rename line is `R<score>\t<old>\t<new>`; other statuses (A/M/D) are ignored.
        let mut parts = line.split('\t');
        let status = parts.next().unwrap_or("");
        if !status.starts_with('R') {
            continue;
        }
        let (Some(old_path), Some(new_path)) = (parts.next(), parts.next()) else {
            continue;
        };
        renames.push(FileRename {
            old_path: old_path.to_string(),
            new_path: new_path.to_string(),
        });
    }
    Ok(renames)
}

/// Subject keywords that mark a commit as refactor-heavy (case-insensitive substring match).
const REFACTOR_KEYWORDS: [&str; 3] = ["rename", "move", "refactor"];

/// True if a commit subject reads like a refactor. Lowercase substring check, no regex crate.
pub fn refactor_subject(subject: &str) -> bool {
    let lower = subject.to_lowercase();
    REFACTOR_KEYWORDS.iter().any(|kw| lower.contains(kw))
}

/// The commit subject line (`git log -1 --format=%s`), trimmed.
pub fn commit_subject(repo: &Path, sha: &str) -> Result<String> {
    let out = git_stdout(repo, &["log", "-1", "--format=%s", sha])?;
    Ok(out.trim().to_string())
}

/// True if `pair` is refactor-heavy: its subject matches [`refactor_subject`] OR it has at least
/// one `.rs` file rename per [`file_renames`].
pub fn is_refactor_pair(repo: &Path, pair: &CommitPair) -> Result<bool> {
    if refactor_subject(&commit_subject(repo, &pair.commit)?) {
        return Ok(true);
    }
    let renames = file_renames(repo, &pair.parent, &pair.commit)?;
    Ok(renames.iter().any(|r| r.new_path.ends_with(".rs")))
}

/// Retain only refactor-heavy pairs from `candidates`, preserving order. See [`is_refactor_pair`].
pub fn retain_refactor_pairs(repo: &Path, candidates: &[CommitPair]) -> Result<Vec<CommitPair>> {
    let mut kept = Vec::new();
    for pair in candidates {
        if is_refactor_pair(repo, pair)? {
            kept.push(pair.clone());
        }
    }
    Ok(kept)
}

/// Scan `candidates` first-parent pairs from `rev`, keep the refactor-heavy ones, and return at most
/// `want` of them (newest first). Refactor commits are sparse, so `candidates` should exceed `want`.
pub fn refactor_pairs(
    repo: &Path,
    rev: &str,
    want: usize,
    candidates: usize,
) -> Result<Vec<CommitPair>> {
    let pool = first_parent_pairs(repo, rev, candidates)?;
    let mut kept = retain_refactor_pairs(repo, &pool)?;
    kept.truncate(want);
    Ok(kept)
}

/// How many candidate pairs to scan per wanted refactor pair, since refactor commits are sparse.
pub const REFACTOR_SCAN_FACTOR: usize = 12;

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

    #[test]
    fn refactor_subject_matches_keywords_case_insensitively() {
        assert!(refactor_subject("refactor: split the parser"));
        assert!(refactor_subject("Rename X to Y"));
        assert!(refactor_subject("MOVE module into core"));
        assert!(!refactor_subject("add a feature"));
        assert!(!refactor_subject("fix a bug"));
    }

    /// Write, stage, and commit a file so a pair carries a real tree change.
    fn commit_file(repo: &Path, path: &str, contents: &str, msg: &str) {
        fs::write(repo.join(path), contents).unwrap();
        git(repo, &["add", path]);
        commit(repo, msg);
    }

    #[test]
    fn retain_refactor_pairs_keeps_only_refactor_heavy() {
        let dir = env::temp_dir().join(format!("csd-harness-refactor-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        git(&dir, &["init", "-q"]);
        // c1: seed a .rs file (plain).
        commit_file(&dir, "old.rs", "fn a() {}\n", "add feature a");
        // c2: plain add, no refactor keyword, no rename.
        commit_file(&dir, "other.txt", "hello\n", "add docs");
        // c3: refactor by subject only (no rename).
        commit_file(&dir, "extra.rs", "fn b() {}\n", "refactor: tidy modules");
        // c4: actual git mv of a .rs file with a non-refactor subject.
        git(&dir, &["mv", "old.rs", "new.rs"]);
        commit(&dir, "adjust things");

        // Newest first: [c4/c3, c3/c2, c2/c1].
        let all = first_parent_pairs(&dir, "HEAD", DEFAULT_PAIR_COUNT).unwrap();
        assert_eq!(all.len(), 3);

        let kept = retain_refactor_pairs(&dir, &all).unwrap();
        // c4 (file rename) and c3 (subject) qualify; the c2/c1 "add docs" pair does not.
        assert_eq!(kept.len(), 2, "kept: {kept:?}");

        let c4 = git_out(&dir, &["rev-parse", "HEAD"]);
        let c3 = git_out(&dir, &["rev-parse", "HEAD~1"]);
        assert_eq!(kept[0].commit, c4, "file-rename pair must be kept");
        assert_eq!(kept[1].commit, c3, "subject pair must be kept");

        // The subject pair qualifies by subject, the rename pair by file rename.
        assert!(refactor_subject(&commit_subject(&dir, &c3).unwrap()));
        assert!(!refactor_subject(&commit_subject(&dir, &c4).unwrap()));
        assert!(is_refactor_pair(&dir, &kept[0]).unwrap());

        // want caps the result even when more candidates qualify.
        let one = refactor_pairs(&dir, "HEAD", 1, DEFAULT_PAIR_COUNT).unwrap();
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].commit, c4);

        fs::remove_dir_all(&dir).unwrap();
    }
}
