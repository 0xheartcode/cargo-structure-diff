//! PR-replay harness: run the csd gate over many merged PRs and record what it would have done.
//!
//! Where the [`crate::metrics`] sweep answers "does rename detection lie?", this module answers the
//! product question that decides adoption (see the Phase-1 mission): **when the gate goes red, is it
//! right?** For each first-parent pair (one merged PR) it runs the exact verdict path the real gate
//! runs - [`csd_diff::diff`] at the gate's rename threshold, then [`csd_lint::lint`] - under two
//! configs:
//!
//! - **zero**: [`Config::default`], what a user gets with no `.csd.toml`.
//! - **init**: the config [`cargo_structure_diff::init_config`] generates, parsed back. Using the
//!   real `init` output (not a copy) means this setup tracks any future hardening of `csd init`.
//!
//! It records, per pair: the verdict and findings under each config, the change counts, a portable
//! determinism hash (recomputed and checked), wall time, and whether any git `.rs` file rename came
//! out as delete+add noise instead of a reconciled move. The raw records (JSON) are the input to the
//! triage pass; the markdown is the human summary. Nothing here is exercised by `cargo test` beyond
//! the pure helpers, since a real run clones and extracts.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;
use std::time::Instant;

use anyhow::{Context, Result};
use csd_config::Config;
use csd_diff::{diff, DiffOptions, FileRename};
use csd_ir::{Change, Graph, NodeKind};
use csd_lint::{has_denials, lint, Finding, Severity};
use serde::Serialize;

use crate::corpus::{self, CommitPair, Repo};
use crate::materialize::{self, Cache};

/// The rename-similarity threshold the gate diffs at. MUST match the literal in
/// `cargo_structure_diff::analyze_graphs` (crates/cargo-structure-diff/src/lib.rs); the gate hardcodes
/// it, so the replay hardcodes the same value to run the identical verdict path. A `debug_assert`-style
/// guard is not possible across crates, so this const is the single documented coupling point.
const GATE_RENAME_THRESHOLD: f32 = 0.7;

/// Default first-parent pairs replayed per repo. ~50 (one per recent merged PR) is enough signal per
/// repo without a multi-hour run; the operator can pass more.
const DEFAULT_REPLAY_PAIRS: usize = 50;

/// One lint finding, flattened for serialization (csd-lint's `Finding`/`Severity` are not `Serialize`).
#[derive(Debug, Clone, Serialize)]
pub struct FindingRec {
    /// The lint rule name, for example `cycles`.
    pub rule: String,
    /// `deny` or `warn`.
    pub severity: String,
    /// The human-readable finding message.
    pub message: String,
}

impl FindingRec {
    fn from(f: &Finding) -> Self {
        Self {
            rule: f.rule.clone(),
            severity: match f.severity {
                Severity::Deny => "deny".to_string(),
                Severity::Warn => "warn".to_string(),
            },
            message: f.message.clone(),
        }
    }
}

/// The counts that summarize a delta, for quick aggregation without re-walking the changes.
#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct ChangeCounts {
    /// Nodes added.
    pub added: usize,
    /// Nodes removed.
    pub removed: usize,
    /// Nodes modified (identity matched, contents changed).
    pub modified: usize,
    /// Nodes re-parented.
    pub moved: usize,
    /// Edges added.
    pub edge_added: usize,
    /// Edges removed.
    pub edge_removed: usize,
}

/// Count each [`Change`] variant in a delta.
fn count_changes(changes: &[Change]) -> ChangeCounts {
    let mut c = ChangeCounts::default();
    for change in changes {
        match change {
            Change::Added(_) => c.added += 1,
            Change::Removed(_) => c.removed += 1,
            Change::Modified { .. } => c.modified += 1,
            Change::Moved { .. } => c.moved += 1,
            Change::EdgeAdded(_) => c.edge_added += 1,
            Change::EdgeRemoved(_) => c.edge_removed += 1,
        }
    }
    c
}

/// The gate's outcome under one config: the verdict, the findings, and a determinism hash.
#[derive(Debug, Clone, Serialize)]
pub struct GateOutcome {
    /// Whether any finding denies the build (the gate goes red).
    pub denied: bool,
    /// Count of deny-severity findings.
    pub denials: usize,
    /// Count of warn-severity findings.
    pub warnings: usize,
    /// Every finding, deny and warn, in csd-lint's stable order.
    pub findings: Vec<FindingRec>,
    /// A portable hash over the canonical findings, for the determinism check.
    pub hash: String,
}

/// Run `lint` for one config and summarize the outcome, hashing the canonical findings.
fn gate(head: &Graph, changes: &[Change], config: &Config) -> GateOutcome {
    let findings = lint(head, changes, config);
    let denials = findings
        .iter()
        .filter(|f| f.severity == Severity::Deny)
        .count();
    let recs: Vec<FindingRec> = findings.iter().map(FindingRec::from).collect();
    GateOutcome {
        denied: has_denials(&findings),
        denials,
        warnings: findings.len() - denials,
        hash: hash_findings(&recs),
        findings: recs,
    }
}

/// The full record for one replayed pair.
#[derive(Debug, Clone, Serialize)]
pub struct PairResult {
    /// Corpus repo name.
    pub repo: String,
    /// Base commit (`commit^`).
    pub parent: String,
    /// Head commit on the first-parent chain.
    pub commit: String,
    /// The head commit's subject line.
    pub subject: String,
    /// Delta counts, config-independent.
    pub counts: ChangeCounts,
    /// Gate outcome with no `.csd.toml`.
    pub zero: GateOutcome,
    /// Gate outcome under the config `csd init` generates (inferred layering as a warning).
    pub init: GateOutcome,
    /// Gate outcome under the `init` config with the inferred layering promoted to `deny` - the
    /// signal measurement: would enforcing the inferred architecture have failed this PR?
    pub layering_deny: GateOutcome,
    /// Git `.rs` file renames in this pair (reliable module-move ground truth).
    pub rs_renames: usize,
    /// Of those, how many came out as delete+add noise instead of a reconciled move.
    pub rename_splits: usize,
    /// Whether the verdict path is reproducible (findings hash equal on a second run).
    pub deterministic: bool,
    /// Wall time for this pair: IR for both sides (cache-warm after the first touch) plus diff and
    /// both lint passes, in milliseconds.
    pub wall_ms: u128,
}

/// Replay `pairs` of one `repo`, returning one [`PairResult`] each (newest pair first, as given).
fn replay_repo(
    repo_path: &Path,
    repo_name: &str,
    pairs: &[CommitPair],
    init_config: &Config,
    cache: &mut Cache,
) -> Result<Vec<PairResult>> {
    // The real zero-config behaviour a user gets with no .csd.toml (module view, cycles warned),
    // not a bare Config::default that would lint nothing.
    let zero_config = cargo_structure_diff::zero_config_defaults();
    // The signal measurement: the inferred layering promoted from a warning to a deny, as a team
    // would do once it trusts the layers. Everything else matches the init config.
    let mut layering_deny_config = init_config.clone();
    layering_deny_config.lint.deny = vec!["layering".to_string()];
    layering_deny_config.lint.warn = vec!["cycles".to_string()];
    let mut results = Vec::with_capacity(pairs.len());

    for pair in pairs {
        let start = Instant::now();
        let base = materialize::ir_for_sha(repo_path, &pair.parent, cache)
            .with_context(|| format!("{repo_name}: extract base {}", pair.parent))?;
        let head = materialize::ir_for_sha(repo_path, &pair.commit, cache)
            .with_context(|| format!("{repo_name}: extract head {}", pair.commit))?;
        let renames = corpus::file_renames(repo_path, &pair.parent, &pair.commit)?;

        let changes = diff(&base, &head, gate_opts(&renames));
        let zero = gate(&head, &changes, &zero_config);
        let init = gate(&head, &changes, init_config);
        let layering_deny = gate(&head, &changes, &layering_deny_config);
        let wall_ms = start.elapsed().as_millis();

        // Determinism: the verdict path is pure over normalized graphs, so a second run must agree.
        // Re-diff as well so any ordering nondeterminism in the differ would surface.
        let changes2 = diff(&base, &head, gate_opts(&renames));
        let zero2 = gate(&head, &changes2, &zero_config);
        let init2 = gate(&head, &changes2, init_config);
        let deterministic = zero.hash == zero2.hash && init.hash == init2.hash;

        let rs_renames = renames.iter().filter(|r| is_rs(&r.new_path)).count();
        let rename_splits = count_rename_splits(&changes, &renames);

        results.push(PairResult {
            repo: repo_name.to_string(),
            parent: pair.parent.clone(),
            commit: pair.commit.clone(),
            subject: corpus::commit_subject(repo_path, &pair.commit).unwrap_or_default(),
            counts: count_changes(&changes),
            zero,
            init,
            layering_deny,
            rs_renames,
            rename_splits,
            deterministic,
            wall_ms,
        });
    }
    Ok(results)
}

/// The gate's diff options: rename detection at the gate threshold, seeded with git file renames.
fn gate_opts(renames: &[FileRename]) -> DiffOptions {
    DiffOptions {
        rename_threshold: Some(GATE_RENAME_THRESHOLD),
        file_renames: renames.to_vec(),
    }
}

/// Whether a path is a Rust source file.
fn is_rs(path: &str) -> bool {
    path.ends_with(".rs")
}

/// Count git `.rs` file renames that the diff split into a Removed+Added module pair instead of
/// reconciling into a move. This is the gate-relevant "delete+add noise" the mission forbids: a pure
/// file move that lights up as a deletion and an addition.
///
/// A rename `old -> new` is a split when a Module node was Removed at `old` AND a Module node was
/// Added at `new`. A reconciled rename (a `Moved`, or a `Modified` whose before/after files differ)
/// is not counted. Non-module nodes are ignored: the module graph is what the gate's lints read.
fn count_rename_splits(changes: &[Change], renames: &[FileRename]) -> usize {
    let mut removed_files: BTreeMap<&str, ()> = BTreeMap::new();
    let mut added_files: BTreeMap<&str, ()> = BTreeMap::new();
    for c in changes {
        match c {
            Change::Removed(n) if n.kind == NodeKind::Module => {
                removed_files.insert(n.span.file.as_str(), ());
            }
            Change::Added(n) if n.kind == NodeKind::Module => {
                added_files.insert(n.span.file.as_str(), ());
            }
            _ => {}
        }
    }
    renames
        .iter()
        .filter(|r| is_rs(&r.new_path))
        .filter(|r| {
            removed_files.contains_key(r.old_path.as_str())
                && added_files.contains_key(r.new_path.as_str())
        })
        .count()
}

/// FNV-1a over a canonical string of the findings: `rule\x1fseverity\x1fmessage\n` per finding, in
/// csd-lint's already-stable order. Portable (not `DefaultHasher`, whose output is unspecified), so
/// the same findings hash identically on every OS - the cross-platform determinism check the mission
/// requires.
fn hash_findings(findings: &[FindingRec]) -> String {
    let mut canon = String::new();
    for f in findings {
        let _ = writeln!(canon, "{}\u{1f}{}\u{1f}{}", f.rule, f.severity, f.message);
    }
    format!("{:016x}", fnv1a(canon.as_bytes()))
}

/// 64-bit FNV-1a. Small, dependency-free, and stable across platforms and runs.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Build the `init` setup's config: run the real `csd init` inference over the oldest head in the
/// range (the commit a user would adopt the tool at), then parse it back into a [`Config`].
fn init_config_for(repo_path: &Path, oldest_sha: &str, cache: &mut Cache) -> Result<Config> {
    let head = materialize::ir_for_sha(repo_path, oldest_sha, cache)
        .with_context(|| format!("extract oldest head {oldest_sha} for init inference"))?;
    let toml = cargo_structure_diff::init_config(&head);
    Config::parse(&toml).context("csd init generated a config that does not parse")
}

/// `csd-harness replay [repo-name] [num-pairs] [--out <dir>]`: replay the gate over the corpus (or one
/// repo) and write per-pair JSON plus a markdown summary. Clones and extracts, so it is manual-only.
pub fn run_replay(rest: &[String]) -> Result<()> {
    let out_dir = flag_value(rest, "--out");
    let positional: Vec<&String> = rest
        .iter()
        .enumerate()
        .filter(|(i, a)| !a.starts_with("--") && !prev_is_valued_flag(rest, *i))
        .map(|(_, a)| a)
        .collect();

    let (only_repo, num_pairs) = resolve_args(&positional)?;

    let repos: Vec<&Repo> = match only_repo {
        Some(name) => vec![corpus::CORPUS
            .iter()
            .find(|r| r.name == name)
            .with_context(|| {
                let names: Vec<&str> = corpus::CORPUS.iter().map(|r| r.name).collect();
                format!("unknown repo {name:?}; known: {}", names.join(", "))
            })?],
        None => corpus::CORPUS.iter().collect(),
    };

    let cache_dir = corpus::cache_dir();
    let mut cache = Cache::new();
    let mut all: Vec<PairResult> = Vec::new();

    for repo in repos {
        let path = corpus::ensure_cloned(repo, &cache_dir)?;
        let pairs = corpus::first_parent_pairs(&path, repo.pin, num_pairs)?;
        if pairs.is_empty() {
            eprintln!(
                "csd-harness replay: {} has no pairs at pin, skipping",
                repo.name
            );
            continue;
        }
        // The oldest commit in the range is the base of the oldest (last) pair.
        let oldest = &pairs.last().expect("non-empty").parent;
        let init_config = init_config_for(&path, oldest, &mut cache)?;
        eprintln!(
            "csd-harness replay: {} ({} pairs)...",
            repo.name,
            pairs.len()
        );
        let results = replay_repo(&path, repo.name, &pairs, &init_config, &mut cache)?;
        all.extend(results);
    }

    if let Some(dir) = out_dir {
        write_json(&all, Path::new(&dir))?;
    }
    print!("{}", summary_markdown(&all));
    Ok(())
}

/// Serialize every pair result to `<dir>/replay.json` (created if missing).
fn write_json(results: &[PairResult], dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir)
        .with_context(|| format!("failed to create replay out dir {}", dir.display()))?;
    let json =
        serde_json::to_string_pretty(results).context("failed to serialize replay results")?;
    let path = dir.join("replay.json");
    std::fs::write(&path, json).with_context(|| format!("failed to write {}", path.display()))?;
    eprintln!("csd-harness replay: wrote {}", path.display());
    Ok(())
}

/// Aggregate totals over a set of pair results, per config and overall.
#[derive(Default)]
struct Totals {
    pairs: usize,
    zero_red: usize,
    zero_warnings: usize,
    init_red: usize,
    init_warnings: usize,
    layering_red: usize,
    nondeterministic: usize,
    rs_renames: usize,
    rename_splits: usize,
}

fn totals_of(results: &[PairResult]) -> Totals {
    let mut t = Totals::default();
    for r in results {
        t.pairs += 1;
        t.zero_red += usize::from(r.zero.denied);
        t.init_red += usize::from(r.init.denied);
        t.zero_warnings += r.zero.warnings;
        t.init_warnings += r.init.warnings;
        t.layering_red += usize::from(r.layering_deny.denied);
        t.nondeterministic += usize::from(!r.deterministic);
        t.rs_renames += r.rs_renames;
        t.rename_splits += r.rename_splits;
    }
    t
}

/// Render the human-facing markdown summary: a per-repo table, the noise/determinism headline, and
/// every denying finding listed so a reader can start triage without opening the JSON.
fn summary_markdown(results: &[PairResult]) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "# csd gate replay");
    let _ = writeln!(out);

    let overall = totals_of(results);
    let _ = writeln!(out, "Pairs replayed: {}", overall.pairs);
    let _ = writeln!(
        out,
        "Nondeterministic pairs: {} (must be 0)",
        overall.nondeterministic
    );
    let _ = writeln!(
        out,
        "Rename splits (pure .rs move shown as delete+add): {} of {} .rs renames (must be 0)",
        overall.rename_splits, overall.rs_renames
    );
    let _ = writeln!(
        out,
        "Signal - PRs the inferred layering would have failed if promoted to deny: {} of {}",
        overall.layering_red, overall.pairs
    );
    let _ = writeln!(out);

    // Per-repo table. `lay red` is the signal column: reds if the inferred layering were enforced.
    let _ = writeln!(
        out,
        "| repo | pairs | zero red | zero warns | init red | init warns | lay red | nondet | rename splits |"
    );
    let _ = writeln!(
        out,
        "| --- | --- | --- | --- | --- | --- | --- | --- | --- |"
    );
    let mut repos: Vec<&str> = results.iter().map(|r| r.repo.as_str()).collect();
    repos.sort_unstable();
    repos.dedup();
    for repo in &repos {
        let slice: Vec<PairResult> = results
            .iter()
            .filter(|r| r.repo == *repo)
            .cloned()
            .collect();
        let t = totals_of(&slice);
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} |",
            repo,
            t.pairs,
            t.zero_red,
            t.zero_warnings,
            t.init_red,
            t.init_warnings,
            t.layering_red,
            t.nondeterministic,
            t.rename_splits
        );
    }
    let _ = writeln!(out);

    // Every denying finding under the init config: the triage worklist.
    let _ = writeln!(
        out,
        "## Denials under the `csd init` config (triage worklist)"
    );
    let _ = writeln!(out);
    let mut any = false;
    for r in results {
        let denials: Vec<&FindingRec> = r
            .init
            .findings
            .iter()
            .filter(|f| f.severity == "deny")
            .collect();
        if denials.is_empty() {
            continue;
        }
        any = true;
        let _ = writeln!(
            out,
            "- **{}** `{}` ({})",
            r.repo,
            short(&r.commit),
            r.subject
        );
        for f in denials {
            let _ = writeln!(out, "    - `{}`: {}", f.rule, f.message);
        }
    }
    if !any {
        let _ = writeln!(
            out,
            "_No denials fired across the replay._ By design the gate denies only on the layering \
edges a repo declares in `.csd.toml`; the generated `init` config declares none and keeps `cycles` \
a warning (Rust module graphs are naturally cyclic), so out of the box the gate is informational and \
cannot produce a false red. Denials appear here only once a repo declares layers - which is where \
the gate's signal comes from."
        );
    }
    let _ = writeln!(out);

    // The signal worklist: PRs the inferred layering would have failed if promoted to deny. Each is
    // a candidate real catch to hand-audit (real upward dependency vs an inference artefact).
    let _ = writeln!(
        out,
        "## Signal: PRs the inferred layering would deny if promoted (audit worklist)"
    );
    let _ = writeln!(out);
    let mut any_signal = false;
    for r in results {
        if !r.layering_deny.denied {
            continue;
        }
        any_signal = true;
        let _ = writeln!(
            out,
            "- **{}** `{}` ({})",
            r.repo,
            short(&r.commit),
            r.subject
        );
        for f in r
            .layering_deny
            .findings
            .iter()
            .filter(|f| f.severity == "deny")
        {
            let _ = writeln!(out, "    - `{}`: {}", f.rule, f.message);
        }
    }
    if !any_signal {
        let _ = writeln!(
            out,
            "_None._ Across the replayed PRs, no change introduced an upward (lower-tier -> \
higher-tier) crate dependency, so even enforcing the inferred layering as a deny would have failed \
nothing. Crate-level dependency inversions are rare in a blind recent sample (as renames were); the \
layering gate is a guardrail against a specific regression, not a frequent catcher. Intra-crate \
layering - where architecture usually erodes - is not inferred yet and is the place to grow signal."
        );
    }
    out
}

/// The first 10 chars of a SHA, for compact display.
fn short(sha: &str) -> &str {
    &sha[..sha.len().min(10)]
}

/// Resolve `[repo-name] [num-pairs]` from the positional args, both optional. A leading positional
/// that parses as a number is num-pairs over all repos (so `replay 50` means 50 pairs each), not a
/// repo named "50"; otherwise the first positional is the repo name and the second is num-pairs.
fn resolve_args<'a>(positional: &[&'a String]) -> Result<(Option<&'a str>, usize)> {
    match positional.first().map(|s| s.as_str()) {
        Some(first) if first.parse::<usize>().is_ok() => Ok((None, first.parse().unwrap())),
        Some(name) => {
            let n = match positional.get(1) {
                Some(s) => s
                    .parse()
                    .context("num-pairs must be a non-negative integer")?,
                None => DEFAULT_REPLAY_PAIRS,
            };
            Ok((Some(name), n))
        }
        None => Ok((None, DEFAULT_REPLAY_PAIRS)),
    }
}

/// The value following `flag` in `args`, if present (`--out dir`).
fn flag_value(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

/// Whether the arg at `idx` is the value consumed by a preceding valued flag (so it is not a
/// positional). Only `--out` takes a value here.
fn prev_is_valued_flag(args: &[String], idx: usize) -> bool {
    idx > 0 && args[idx - 1] == "--out"
}

#[cfg(test)]
mod tests {
    use super::*;
    use csd_ir::{Node, SourceSpan, StableId};

    fn module(id: &str, file: &str) -> Node {
        Node {
            id: StableId::new(id),
            kind: NodeKind::Module,
            span: SourceSpan {
                file: file.into(),
                start: 0,
                end: 1,
            },
            attrs: BTreeMap::new(),
            fingerprint: None,
        }
    }

    #[test]
    fn fnv1a_is_stable_and_distinguishes() {
        // Known FNV-1a 64 vector: empty string is the offset basis.
        assert_eq!(format!("{:016x}", fnv1a(b"")), "cbf29ce484222325");
        assert_ne!(fnv1a(b"a"), fnv1a(b"b"));
        // Deterministic across calls.
        assert_eq!(fnv1a(b"cycles\x1fdeny\x1fx"), fnv1a(b"cycles\x1fdeny\x1fx"));
    }

    #[test]
    fn hash_findings_is_order_sensitive_and_portable() {
        let a = FindingRec {
            rule: "cycles".into(),
            severity: "deny".into(),
            message: "A -> B -> A".into(),
        };
        let b = FindingRec {
            rule: "cycles".into(),
            severity: "deny".into(),
            message: "C -> D -> C".into(),
        };
        let h1 = hash_findings(&[a.clone(), b.clone()]);
        let h2 = hash_findings(&[b, a]);
        assert_ne!(h1, h2, "order changes the hash");
        // Empty findings hash to the FNV offset basis, and are stable.
        assert_eq!(hash_findings(&[]), "cbf29ce484222325");
    }

    #[test]
    fn count_changes_tallies_each_variant() {
        let changes = vec![
            Change::Added(module("crate::a", "src/a.rs")),
            Change::Removed(module("crate::b", "src/b.rs")),
            Change::Modified {
                before: module("crate::c", "src/c.rs"),
                after: module("crate::c", "src/c2.rs"),
            },
            Change::Moved {
                node: StableId::new("crate::d::T"),
                from: StableId::new("crate::d"),
                to: StableId::new("crate::e"),
            },
        ];
        let c = count_changes(&changes);
        assert_eq!((c.added, c.removed, c.modified, c.moved), (1, 1, 1, 1));
    }

    #[test]
    fn rename_split_is_counted_only_on_delete_plus_add() {
        let renames = vec![FileRename {
            old_path: "src/old.rs".into(),
            new_path: "src/new.rs".into(),
        }];

        // Split: the old module is Removed and the new one Added -> delete+add noise.
        let split = vec![
            Change::Removed(module("crate::old", "src/old.rs")),
            Change::Added(module("crate::new", "src/new.rs")),
        ];
        assert_eq!(count_rename_splits(&split, &renames), 1);

        // Reconciled: a single Modified bridging the two files is not a split.
        let reconciled = vec![Change::Modified {
            before: module("crate::old", "src/old.rs"),
            after: module("crate::new", "src/new.rs"),
        }];
        assert_eq!(count_rename_splits(&reconciled, &renames), 0);

        // A non-.rs rename is ignored even if split.
        let non_rs = vec![FileRename {
            old_path: "src/old.toml".into(),
            new_path: "src/new.toml".into(),
        }];
        assert_eq!(count_rename_splits(&split, &non_rs), 0);
    }

    fn owned(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn resolve_args_reads_leading_number_as_pairs_over_all_repos() {
        let a = owned(&["50"]);
        let p: Vec<&String> = a.iter().collect();
        assert_eq!(resolve_args(&p).unwrap(), (None, 50));

        let b = owned(&["ripgrep", "20"]);
        let p: Vec<&String> = b.iter().collect();
        assert_eq!(resolve_args(&p).unwrap(), (Some("ripgrep"), 20));

        let c = owned(&["tokio"]);
        let p: Vec<&String> = c.iter().collect();
        assert_eq!(
            resolve_args(&p).unwrap(),
            (Some("tokio"), DEFAULT_REPLAY_PAIRS)
        );

        let empty: Vec<&String> = Vec::new();
        assert_eq!(resolve_args(&empty).unwrap(), (None, DEFAULT_REPLAY_PAIRS));
    }

    #[test]
    fn positional_parsing_skips_out_flag_and_its_value() {
        let args: Vec<String> = ["ripgrep", "20", "--out", "/tmp/x"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(flag_value(&args, "--out").as_deref(), Some("/tmp/x"));
        let positional: Vec<&String> = args
            .iter()
            .enumerate()
            .filter(|(i, a)| !a.starts_with("--") && !prev_is_valued_flag(&args, *i))
            .map(|(_, a)| a)
            .collect();
        assert_eq!(positional.len(), 2, "repo + num-pairs only: {positional:?}");
        assert_eq!(positional[0], "ripgrep");
        assert_eq!(positional[1], "20");
    }
}
