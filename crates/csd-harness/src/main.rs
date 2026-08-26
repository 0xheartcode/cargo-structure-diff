//! Validation harness (M0): clone a corpus, walk first-parent commit pairs, extract and diff
//! each pair across a rename-threshold sweep, and report spurious-split rate and a false-merge
//! audit. See `SPEC.md` section 6 and the backlog (area `harness`).
//!
//! The pure metric functions live in [`metrics`] and are unit-tested on synthetic graphs. The
//! `sweep` subcommand wires them to a real corpus repo; it clones and extracts, so it is never
//! exercised by tests (no network in tests).
//!
//! Edge precision/recall is deliberately NOT computed: the extractor emits no edges yet (the
//! use-path resolver, backlog bb67bbb, is unfinished), so any edge P/R number would be fabricated.

mod corpus;
mod materialize;
mod metrics;

use std::fmt::Write as _;
use std::path::Path;

use anyhow::{Context, Result};
use csd_diff::{diff, DiffOptions, FileRename};
use csd_ir::{Change, Graph, Node};

use metrics::{
    classify_counts, false_merge_audit, fingerprint_matches, spurious_splits, AcceptedMatch,
    Counts, SplitCandidate, NEAR_MISS_SCORE, THRESHOLD_SWEEP,
};

/// Threshold used for the detailed audit samples in the report (the differ's default).
const AUDIT_THRESHOLD: f32 = 0.7;

/// Default pairs to replay in a manual `sweep` run. Small so a hand run is quick; SPEC.md section 6
/// wants several hundred for a real go/no-go measurement, which the operator passes explicitly.
const DEFAULT_SWEEP_PAIRS: usize = 25;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("pairs") => run_pairs(&args[2..]),
        Some("corpus") => run_corpus(),
        Some("ir") => run_ir(&args[2..]),
        Some("sweep") => run_sweep(&args[2..]),
        _ => {
            eprintln!(
                "csd-harness: usage: pairs <repo> [n] | corpus | ir <repo> <sha> | sweep [repo-name] [n]. See SPEC.md section 6."
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

/// `csd-harness ir <repo-path> <sha>`: materialize a commit and print its IR node count.
fn run_ir(rest: &[String]) -> Result<()> {
    let repo = rest
        .first()
        .context("usage: csd-harness ir <repo-path> <sha>")?;
    let sha = rest
        .get(1)
        .context("usage: csd-harness ir <repo-path> <sha>")?;
    let mut cache = materialize::Cache::new();
    let graph = materialize::ir_for_sha(Path::new(repo), sha, &mut cache)?;
    println!("{} nodes", graph.nodes.len());
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

/// `csd-harness sweep [repo-name] [num-pairs]`: replay N pairs of a corpus repo across the rename
/// threshold sweep and print a markdown report. Clones and extracts, so it is manual-only.
fn run_sweep(rest: &[String]) -> Result<()> {
    let repo_name = rest
        .first()
        .map(String::as_str)
        .unwrap_or(corpus::CORPUS[0].name);
    let num_pairs = match rest.get(1) {
        Some(s) => s
            .parse()
            .context("num-pairs must be a non-negative integer")?,
        None => DEFAULT_SWEEP_PAIRS,
    };
    let repo = corpus::CORPUS
        .iter()
        .find(|r| r.name == repo_name)
        .with_context(|| {
            let names: Vec<&str> = corpus::CORPUS.iter().map(|r| r.name).collect();
            format!("unknown repo {repo_name:?}; known: {}", names.join(", "))
        })?;

    let cache_dir = corpus::cache_dir();
    let path = corpus::ensure_cloned(repo, &cache_dir)?;
    let pairs = corpus::first_parent_pairs(&path, "HEAD", num_pairs)?;

    let report = sweep_report(&path, repo.name, &pairs)?;
    print!("{report}");
    Ok(())
}

/// Per-threshold running totals over the pair set.
#[derive(Clone, Copy, Default)]
struct ThresholdTotals {
    counts: Counts,
    spurious: usize,
}

/// Run the sweep over `pairs` and render the markdown report.
fn sweep_report(repo: &Path, repo_name: &str, pairs: &[corpus::CommitPair]) -> Result<String> {
    let mut cache = materialize::Cache::new();

    let mut baseline = Counts::default();
    let mut totals: Vec<ThresholdTotals> = vec![ThresholdTotals::default(); THRESHOLD_SWEEP.len()];
    let mut audit_matches: Vec<AcceptedMatch> = Vec::new();
    let mut audit_splits: Vec<SplitCandidate> = Vec::new();

    let mut rs_renames = 0usize;
    let mut module_reconciled = 0usize;
    let mut analyzed = 0usize;

    for pair in pairs {
        let base = materialize::ir_for_sha(repo, &pair.parent, &mut cache)?;
        let head = materialize::ir_for_sha(repo, &pair.commit, &mut cache)?;
        let renames = corpus::file_renames(repo, &pair.parent, &pair.commit)?;
        analyzed += 1;

        rs_renames += renames
            .iter()
            .filter(|r| r.new_path.ends_with(".rs"))
            .count();
        module_reconciled += module_reconciliations(&base, &head, &renames);

        // Rename detection OFF baseline (pure set diff plus git module seeding only).
        let off = diff(&base, &head, off_opts(&renames));
        baseline = add_counts(baseline, classify_counts(&off));

        // The pure set-diff node split feeds the harness-side matcher for score recovery.
        let (removed, added) = set_diff_nodes(&base, &head);

        for (i, &threshold) in THRESHOLD_SWEEP.iter().enumerate() {
            let changes = diff(&base, &head, on_opts(threshold, &renames));
            totals[i].counts = add_counts(totals[i].counts, classify_counts(&changes));
            let splits = spurious_splits(&changes, NEAR_MISS_SCORE, usize::MAX);
            totals[i].spurious += splits.count;

            if (threshold - AUDIT_THRESHOLD).abs() < f32::EPSILON {
                audit_matches.extend(fingerprint_matches(&removed, &added, threshold));
                audit_splits.extend(splits.sample);
            }
        }
    }

    Ok(render_markdown(RenderInput {
        repo_name,
        analyzed,
        baseline,
        totals: &totals,
        rs_renames,
        module_reconciled,
        audit_matches,
        audit_splits,
    }))
}

/// Diff options with rename detection off (module seeding still runs when git has renames).
fn off_opts(renames: &[FileRename]) -> DiffOptions {
    DiffOptions {
        rename_threshold: None,
        file_renames: renames.to_vec(),
    }
}

/// Diff options with fingerprint rename detection on at `threshold`.
fn on_opts(threshold: f32, renames: &[FileRename]) -> DiffOptions {
    DiffOptions {
        rename_threshold: Some(threshold),
        file_renames: renames.to_vec(),
    }
}

/// The `Removed`/`Added` nodes of the pure set diff (no rename detection, no seeding).
fn set_diff_nodes(base: &Graph, head: &Graph) -> (Vec<Node>, Vec<Node>) {
    let changes = diff(
        base,
        head,
        DiffOptions {
            rename_threshold: None,
            file_renames: Vec::new(),
        },
    );
    let mut removed = Vec::new();
    let mut added = Vec::new();
    for change in changes {
        match change {
            Change::Removed(n) => removed.push(n),
            Change::Added(n) => added.push(n),
            _ => {}
        }
    }
    (removed, added)
}

/// Count module moves git reconciled: run with seeding on but fingerprint matching off, then count
/// `Moved` plus renamed `Modified` (before id differs from after id). This is the reliable
/// module-level sanity check.
fn module_reconciliations(base: &Graph, head: &Graph, renames: &[FileRename]) -> usize {
    diff(base, head, off_opts(renames))
        .iter()
        .filter(|c| match c {
            Change::Moved { .. } => true,
            Change::Modified { before, after } => before.id != after.id,
            _ => false,
        })
        .count()
}

/// Sum two [`Counts`].
fn add_counts(a: Counts, b: Counts) -> Counts {
    Counts {
        added: a.added + b.added,
        removed: a.removed + b.removed,
        moved: a.moved + b.moved,
        modified: a.modified + b.modified,
        edge_added: a.edge_added + b.edge_added,
        edge_removed: a.edge_removed + b.edge_removed,
    }
}

/// Everything the renderer needs; grouped to keep the signature narrow.
struct RenderInput<'a> {
    repo_name: &'a str,
    analyzed: usize,
    baseline: Counts,
    totals: &'a [ThresholdTotals],
    rs_renames: usize,
    module_reconciled: usize,
    audit_matches: Vec<AcceptedMatch>,
    audit_splits: Vec<SplitCandidate>,
}

/// Render the markdown report: sweep tables, audit samples, and the honesty notes.
fn render_markdown(input: RenderInput) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "# csd-harness sweep report: {}", input.repo_name);
    let _ = writeln!(out);
    let _ = writeln!(out, "Pairs analyzed: {}", input.analyzed);
    let _ = writeln!(out);

    // Honesty notes up front so no reader mistakes these for gold-standard precision/recall.
    let _ = writeln!(out, "## Scope and honesty notes");
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "- Edge precision/recall is PENDING and not reported: the extractor emits no edges yet, awaiting the use-path resolver (backlog bb67bbb). Any edge P/R here would be fabricated."
    );
    let _ = writeln!(
        out,
        "- Spurious-split and false-merge numbers are heuristic quality signals, NOT precision/recall against a gold set. Git reports FILE renames only, never type-level renames, so there is no type-level rename ground truth to score against."
    );
    let _ = writeln!(
        out,
        "- Git file renames ARE reliable module-level ground truth and are used only for the module-move reconciliation sanity check below."
    );
    let _ = writeln!(out);

    // Module-move reconciliation sanity check.
    let _ = writeln!(out, "## Module-move reconciliation (git ground truth)");
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "- Git `.rs` file renames across all pairs: {}",
        input.rs_renames
    );
    let _ = writeln!(
        out,
        "- Reconciled to a module Moved/Modified by csd: {}",
        input.module_reconciled
    );
    let _ = writeln!(out);

    // Threshold sweep table.
    let _ = writeln!(out, "## Threshold sweep (rename detection off vs on)");
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "| threshold | added | removed | moved | modified | edge+ | edge- | spurious splits |"
    );
    let _ = writeln!(out, "| --- | --- | --- | --- | --- | --- | --- | --- |");
    let b = input.baseline;
    let _ = writeln!(
        out,
        "| off | {} | {} | {} | {} | {} | {} | n/a |",
        b.added, b.removed, b.moved, b.modified, b.edge_added, b.edge_removed
    );
    for (i, &threshold) in THRESHOLD_SWEEP.iter().enumerate() {
        let t = input.totals[i];
        let c = t.counts;
        let _ = writeln!(
            out,
            "| {:.1} | {} | {} | {} | {} | {} | {} | {} |",
            threshold,
            c.added,
            c.removed,
            c.moved,
            c.modified,
            c.edge_added,
            c.edge_removed,
            t.spurious
        );
    }
    let _ = writeln!(out);

    // False-merge audit sample.
    let audit = false_merge_audit(&input.audit_matches, 20);
    let _ = writeln!(
        out,
        "## False-merge audit at threshold {AUDIT_THRESHOLD:.1} (lowest scores first, hand-review)"
    );
    let _ = writeln!(out);
    if audit.is_empty() {
        let _ = writeln!(out, "No accepted fingerprint matches at this threshold.");
    } else {
        let _ = writeln!(out, "| score | kind | removed id | added id |");
        let _ = writeln!(out, "| --- | --- | --- | --- |");
        for m in &audit {
            let kind = if m.moved { "moved" } else { "modified" };
            let _ = writeln!(
                out,
                "| {:.3} | {} | `{}` | `{}` |",
                m.score, kind, m.removed_id, m.added_id
            );
        }
    }
    let _ = writeln!(out);

    // Spurious-split sample: highest-scoring missed renames.
    let mut splits = input.audit_splits;
    splits.sort_by(|x, y| {
        y.score
            .total_cmp(&x.score)
            .then_with(|| x.removed_id.cmp(&y.removed_id))
            .then_with(|| x.added_id.cmp(&y.added_id))
    });
    splits.truncate(20);
    let _ = writeln!(
        out,
        "## Spurious-split sample at threshold {AUDIT_THRESHOLD:.1} (near-miss renames, highest score first)"
    );
    let _ = writeln!(out);
    if splits.is_empty() {
        let _ = writeln!(out, "No near-miss splits at this threshold.");
    } else {
        let _ = writeln!(out, "| score | removed id | added id |");
        let _ = writeln!(out, "| --- | --- | --- |");
        for s in &splits {
            let _ = writeln!(
                out,
                "| {:.3} | `{}` | `{}` |",
                s.score, s.removed_id, s.added_id
            );
        }
    }
    let _ = writeln!(out);

    out
}
