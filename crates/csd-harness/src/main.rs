//! Validation harness (M0): clone a corpus, walk first-parent commit pairs, extract and diff
//! each pair across a rename-threshold sweep, and report spurious-split rate, false-merge audit,
//! and edge precision/recall. See `SPEC.md` section 6 and the backlog (area `harness`).
//!
//! Only the corpus clone and first-parent pair walker are implemented at v0.0.0; the measurement
//! pipeline is not.

mod corpus;

use std::path::Path;

use anyhow::{Context, Result};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("pairs") => run_pairs(&args[2..]),
        Some("corpus") => run_corpus(),
        _ => {
            eprintln!(
                "csd-harness v0.0.0: not yet implemented. See BACKLOG.md (area `harness`) and SPEC.md section 6."
            );
            Ok(())
        }
    }
}

/// `csd-harness pairs <repo-path> [count]`: print the latest first-parent pairs of a clone.
fn run_pairs(rest: &[String]) -> Result<()> {
    let repo = rest
        .first()
        .context("usage: csd-harness pairs <repo-path> [count]")?;
    let count = match rest.get(1) {
        Some(s) => s.parse().context("count must be a non-negative integer")?,
        None => corpus::DEFAULT_PAIR_COUNT,
    };
    let pairs = corpus::first_parent_pairs(Path::new(repo), "HEAD", count)?;
    for pair in &pairs {
        println!("{} {}", pair.parent, pair.commit);
    }
    Ok(())
}

/// `csd-harness corpus`: ensure every corpus repo is cloned into the cache and print its path.
fn run_corpus() -> Result<()> {
    let cache = corpus::cache_dir();
    for repo in corpus::CORPUS {
        let path = corpus::ensure_cloned(repo, &cache)?;
        println!("{}\t{}", repo.name, path.display());
    }
    Ok(())
}
