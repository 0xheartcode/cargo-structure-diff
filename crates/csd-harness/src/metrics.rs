//! Pure metric functions over diff [`Change`] sets.
//!
//! These are the unit-tested core of the harness. Everything here is a pure function of its inputs
//! (a `Change` slice or a pair of node lists), so it can be driven by hand-built in-memory graphs
//! with no git, no network, and no filesystem.
//!
//! Three signals are produced.
//!
//! [`classify_counts`] tallies each `Change` variant.
//!
//! [`spurious_splits`] finds near-miss renames: `Removed` + `Added` pairs the matcher left split
//! even though they look like renames. These are heuristic quality signals, not precision against a
//! gold set, since git gives no type-level rename ground truth.
//!
//! [`fingerprint_matches`] re-derives the accepted fingerprint matches together with their
//! similarity score, which `csd_diff::diff` does not itself return, feeding the false-merge audit.

use csd_ir::{similarity, Change, Node, StableId};

/// The rename-threshold sweep from SPEC.md section 6: shows how sensitive rename detection is to
/// the acceptance threshold.
pub const THRESHOLD_SWEEP: &[f32] = &[0.5, 0.6, 0.7, 0.8, 0.9];

/// Similarity at or above which a still-split pair counts as a near-miss rename.
pub const NEAR_MISS_SCORE: f32 = 0.5;

/// Tally of each [`Change`] variant in a diff.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    /// `Change::Added` nodes.
    pub added: usize,
    /// `Change::Removed` nodes.
    pub removed: usize,
    /// `Change::Moved` nodes.
    pub moved: usize,
    /// `Change::Modified` nodes.
    pub modified: usize,
    /// `Change::EdgeAdded` edges.
    pub edge_added: usize,
    /// `Change::EdgeRemoved` edges.
    pub edge_removed: usize,
}

/// Tally each [`Change`] variant.
pub fn classify_counts(changes: &[Change]) -> Counts {
    let mut c = Counts::default();
    for change in changes {
        match change {
            Change::Added(_) => c.added += 1,
            Change::Removed(_) => c.removed += 1,
            Change::Moved { .. } => c.moved += 1,
            Change::Modified { .. } => c.modified += 1,
            Change::EdgeAdded(_) => c.edge_added += 1,
            Change::EdgeRemoved(_) => c.edge_removed += 1,
        }
    }
    c
}

/// A near-miss rename: a `Removed` and an `Added` node that look alike but stayed split.
#[derive(Debug, Clone, PartialEq)]
pub struct SplitCandidate {
    /// Id of the removed node.
    pub removed_id: String,
    /// Id of the added node.
    pub added_id: String,
    /// Recomputed fingerprint similarity in `[0.0, 1.0]`.
    pub score: f32,
}

/// Near-miss renames found in a diff, plus a bounded sample.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SpuriousSplits {
    /// Total qualifying `(Removed, Added)` pairs.
    pub count: usize,
    /// Highest-scoring qualifying pairs, most-likely-missed first, capped at the sample limit.
    pub sample: Vec<SplitCandidate>,
}

/// Find spurious-split candidates: `(Removed, Added)` pairs with the same kind, a fingerprint on
/// both sides, and similarity at or above `min_score`, that the matcher nonetheless left split.
///
/// The input is a real `diff` output at some threshold; only its still-`Added`/`Removed` nodes are
/// considered, so any pair here is one the matcher missed. This is a heuristic quality signal, not
/// precision against a gold set: git reports file renames, never type-level renames, so there is no
/// ground truth to score against.
pub fn spurious_splits(changes: &[Change], min_score: f32, sample_limit: usize) -> SpuriousSplits {
    let removed: Vec<&Node> = changes
        .iter()
        .filter_map(|c| match c {
            Change::Removed(n) => Some(n),
            _ => None,
        })
        .collect();
    let added: Vec<&Node> = changes
        .iter()
        .filter_map(|c| match c {
            Change::Added(n) => Some(n),
            _ => None,
        })
        .collect();

    let mut candidates: Vec<SplitCandidate> = Vec::new();
    for r in &removed {
        for a in &added {
            if r.kind != a.kind {
                continue;
            }
            let (Some(rf), Some(af)) = (&r.fingerprint, &a.fingerprint) else {
                continue;
            };
            let score = similarity(rf, af);
            if score >= min_score {
                candidates.push(SplitCandidate {
                    removed_id: r.id.as_str().to_string(),
                    added_id: a.id.as_str().to_string(),
                    score,
                });
            }
        }
    }

    candidates.sort_by(|x, y| {
        y.score
            .total_cmp(&x.score)
            .then_with(|| x.removed_id.cmp(&y.removed_id))
            .then_with(|| x.added_id.cmp(&y.added_id))
    });

    let count = candidates.len();
    candidates.truncate(sample_limit);
    SpuriousSplits {
        count,
        sample: candidates,
    }
}

/// An accepted fingerprint match, carrying the score `csd_diff::diff` does not return.
#[derive(Debug, Clone, PartialEq)]
pub struct AcceptedMatch {
    /// Id of the base-side (removed) node.
    pub removed_id: String,
    /// Id of the head-side (added) node.
    pub added_id: String,
    /// Fingerprint similarity in `[0.0, 1.0]` at which the pair was accepted.
    pub score: f32,
    /// True when the parent module differs (a `Moved`), false for a same-parent `Modified`.
    pub moved: bool,
}

/// Re-derive the accepted fingerprint matches and their scores from a pure set-diff node split.
///
/// This mirrors the greedy matcher in `csd_diff::detect_renames` (same candidate rule, same
/// `score desc, removed id, added id` ordering, same one-to-one assignment) so the accepted pairs
/// match what `diff` produced, while additionally keeping the similarity score. That score is how
/// the harness recovers the false-merge audit input without modifying `csd-diff`.
///
/// Feed it the `Removed`/`Added` nodes of the pure set diff (threshold off). Modules carry no
/// fingerprint and are skipped, exactly as in the differ.
pub fn fingerprint_matches(removed: &[Node], added: &[Node], threshold: f32) -> Vec<AcceptedMatch> {
    let mut candidates: Vec<(f32, usize, usize)> = Vec::new();
    for (ri, r) in removed.iter().enumerate() {
        for (ai, a) in added.iter().enumerate() {
            if r.kind != a.kind {
                continue;
            }
            let (Some(rf), Some(af)) = (&r.fingerprint, &a.fingerprint) else {
                continue;
            };
            let score = similarity(rf, af);
            if score >= threshold {
                candidates.push((score, ri, ai));
            }
        }
    }

    candidates.sort_by(|x, y| {
        y.0.total_cmp(&x.0)
            .then_with(|| removed[x.1].id.cmp(&removed[y.1].id))
            .then_with(|| added[x.2].id.cmp(&added[y.2].id))
    });

    let mut removed_taken = vec![false; removed.len()];
    let mut added_taken = vec![false; added.len()];
    let mut out: Vec<AcceptedMatch> = Vec::new();
    for (score, ri, ai) in candidates {
        if removed_taken[ri] || added_taken[ai] {
            continue;
        }
        removed_taken[ri] = true;
        added_taken[ai] = true;
        out.push(AcceptedMatch {
            removed_id: removed[ri].id.as_str().to_string(),
            added_id: added[ai].id.as_str().to_string(),
            score,
            moved: parent(&removed[ri].id) != parent(&added[ai].id),
        });
    }
    out
}

/// False-merge audit: the accepted matches sorted ASCENDING by score, riskiest first, capped.
///
/// The lowest-scoring accepted moves are the ones most likely to have merged two genuinely
/// different items. Frame these as hand-review candidates, not a measured false-merge rate: with no
/// type-level rename ground truth from git, "wrong" here is a human judgement.
pub fn false_merge_audit(matches: &[AcceptedMatch], sample_limit: usize) -> Vec<AcceptedMatch> {
    let mut sorted = matches.to_vec();
    sorted.sort_by(|a, b| {
        a.score
            .total_cmp(&b.score)
            .then_with(|| a.removed_id.cmp(&b.removed_id))
            .then_with(|| a.added_id.cmp(&b.added_id))
    });
    sorted.truncate(sample_limit);
    sorted
}

/// Parent module id: the node's id with the last `::segment` stripped. Matches the differ's rule so
/// the moved/modified split here agrees with `diff`.
fn parent(id: &StableId) -> StableId {
    match id.as_str().rfind("::") {
        Some(pos) => StableId::new(&id.as_str()[..pos]),
        None => StableId::new(""),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use csd_ir::{Edge, EdgeKind, Fingerprint, NodeKind, SourceSpan};
    use std::collections::{BTreeMap, BTreeSet};

    fn span() -> SourceSpan {
        SourceSpan {
            file: "src/lib.rs".into(),
            start: 0,
            end: 1,
        }
    }

    /// A fingerprint carrying just members and field types; enough to drive similarity.
    fn fp(members: &[&str], fields: &[(&str, u32)]) -> Fingerprint {
        Fingerprint {
            members: members.iter().map(|s| s.to_string()).collect(),
            field_types: fields.iter().map(|(t, c)| (t.to_string(), *c)).collect(),
            member_types: BTreeMap::new(),
            neighbors: BTreeSet::new(),
            doc_hash: 0,
        }
    }

    fn node(id: &str, kind: NodeKind, fingerprint: Option<Fingerprint>) -> Node {
        Node {
            id: StableId::new(id),
            kind,
            span: span(),
            attrs: BTreeMap::new(),
            fingerprint,
        }
    }

    fn edge(from: &str, to: &str) -> Edge {
        Edge {
            from: StableId::new(from),
            to: StableId::new(to),
            kind: EdgeKind::Uses,
            span: span(),
            ordinal: None,
        }
    }

    #[test]
    fn classify_counts_tallies_every_variant() {
        let changes = vec![
            Change::Added(node("m::A", NodeKind::Struct, None)),
            Change::Added(node("m::B", NodeKind::Struct, None)),
            Change::Removed(node("m::C", NodeKind::Struct, None)),
            Change::Modified {
                before: node("m::D", NodeKind::Struct, None),
                after: node("m::D", NodeKind::Struct, None),
            },
            Change::Moved {
                node: StableId::new("x::E"),
                from: StableId::new("y"),
                to: StableId::new("x"),
            },
            Change::EdgeAdded(edge("a", "b")),
            Change::EdgeRemoved(edge("c", "d")),
            Change::EdgeRemoved(edge("e", "f")),
        ];
        let c = classify_counts(&changes);
        assert_eq!(
            c,
            Counts {
                added: 2,
                removed: 1,
                moved: 1,
                modified: 1,
                edge_added: 1,
                edge_removed: 2,
            }
        );
    }

    #[test]
    fn clean_rename_is_not_a_spurious_split() {
        // A clean rename is accepted by the matcher, so at any real threshold it leaves no
        // Added/Removed pair behind and cannot be a spurious split.
        let f = fp(&["id", "total"], &[("u64", 1)]);
        let changes = vec![Change::Modified {
            before: node("m::Old", NodeKind::Struct, Some(f.clone())),
            after: node("m::New", NodeKind::Struct, Some(f)),
        }];
        let out = spurious_splits(&changes, NEAR_MISS_SCORE, 20);
        assert_eq!(out.count, 0);
        assert!(out.sample.is_empty());
    }

    #[test]
    fn genuine_add_and_remove_is_not_flagged() {
        // Disjoint members and field types keep similarity below the near-miss floor.
        let changes = vec![
            Change::Removed(node(
                "m::Alpha",
                NodeKind::Struct,
                Some(fp(&["a", "b", "c"], &[("u64", 1)])),
            )),
            Change::Added(node(
                "m::Beta",
                NodeKind::Struct,
                Some(fp(&["x", "y", "z"], &[("String", 1)])),
            )),
        ];
        let out = spurious_splits(&changes, NEAR_MISS_SCORE, 20);
        assert_eq!(
            out.count, 0,
            "low-similarity pair is a real change, not a split"
        );
    }

    #[test]
    fn near_miss_split_is_detected() {
        // Structurally close pair (score >= 0.5) left as Added + Removed: the matcher missed a
        // likely rename, e.g. because the threshold was set above the pair score.
        let changes = vec![
            Change::Removed(node(
                "m::Old",
                NodeKind::Struct,
                Some(fp(&["id", "total", "note"], &[("u64", 2)])),
            )),
            Change::Added(node(
                "m::New",
                NodeKind::Struct,
                Some(fp(&["id", "total"], &[("u64", 2)])),
            )),
        ];
        let out = spurious_splits(&changes, NEAR_MISS_SCORE, 20);
        assert_eq!(out.count, 1);
        assert_eq!(out.sample.len(), 1);
        assert_eq!(out.sample[0].removed_id, "m::Old");
        assert_eq!(out.sample[0].added_id, "m::New");
        assert!(
            out.sample[0].score >= NEAR_MISS_SCORE,
            "score {} must clear the near-miss floor",
            out.sample[0].score
        );
    }

    #[test]
    fn different_kinds_are_never_split_candidates() {
        // Identical structure but Struct vs Enum: not a rename candidate at all.
        let f = fp(&["Read", "Write"], &[("u8", 1)]);
        let changes = vec![
            Change::Removed(node("m::Mode", NodeKind::Struct, Some(f.clone()))),
            Change::Added(node("m::Mode2", NodeKind::Enum, Some(f))),
        ];
        let out = spurious_splits(&changes, NEAR_MISS_SCORE, 20);
        assert_eq!(out.count, 0);
    }

    #[test]
    fn split_sample_is_sorted_by_score_descending_and_capped() {
        // Two removeds, each close to a distinct added: a strong pair and a weaker pair. The sample
        // should list the stronger one first, and honour the cap.
        let strong = fp(&["id", "total"], &[("u64", 2)]);
        let weak_r = fp(&["id", "total", "a", "b"], &[("u64", 2)]);
        let weak_a = fp(&["id", "total"], &[("u64", 2), ("String", 2)]);
        let changes = vec![
            Change::Removed(node("m::StrongOld", NodeKind::Struct, Some(strong.clone()))),
            Change::Added(node("m::StrongNew", NodeKind::Struct, Some(strong))),
            Change::Removed(node("m::WeakOld", NodeKind::Enum, Some(weak_r))),
            Change::Added(node("m::WeakNew", NodeKind::Enum, Some(weak_a))),
        ];
        let out = spurious_splits(&changes, NEAR_MISS_SCORE, 1);
        assert!(out.count >= 2, "both pairs qualify, got {}", out.count);
        assert_eq!(out.sample.len(), 1, "capped to one sample");
        assert_eq!(out.sample[0].removed_id, "m::StrongOld");
    }

    #[test]
    fn fingerprint_matches_recovers_score_and_move_flag() {
        // Same structure, different parent module: an accepted Moved with score 1.0.
        let f = fp(&["id", "total"], &[("u64", 1)]);
        let removed = vec![node("billing::Order", NodeKind::Struct, Some(f.clone()))];
        let added = vec![node("payments::Order", NodeKind::Struct, Some(f))];
        let matches = fingerprint_matches(&removed, &added, 0.7);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].removed_id, "billing::Order");
        assert_eq!(matches[0].added_id, "payments::Order");
        assert_eq!(matches[0].score, 1.0);
        assert!(matches[0].moved, "different parent module is a Moved");
    }

    #[test]
    fn fingerprint_matches_same_parent_is_not_moved() {
        let f = fp(&["id", "total"], &[("u64", 1)]);
        let removed = vec![node("m::Old", NodeKind::Struct, Some(f.clone()))];
        let added = vec![node("m::New", NodeKind::Struct, Some(f))];
        let matches = fingerprint_matches(&removed, &added, 0.7);
        assert_eq!(matches.len(), 1);
        assert!(!matches[0].moved, "same parent module is a Modified");
    }

    #[test]
    fn fingerprint_matches_skips_below_threshold_and_modules() {
        // One near pair below threshold, plus a module pair (no fingerprint): neither matches.
        let removed = vec![
            node(
                "m::Old",
                NodeKind::Struct,
                Some(fp(&["a", "b", "c", "d"], &[("u64", 1)])),
            ),
            node("crate::billing", NodeKind::Module, None),
        ];
        let added = vec![
            node(
                "m::New",
                NodeKind::Struct,
                Some(fp(&["a"], &[("String", 1)])),
            ),
            node("crate::payments", NodeKind::Module, None),
        ];
        let matches = fingerprint_matches(&removed, &added, 0.7);
        assert!(matches.is_empty(), "got {matches:?}");
    }

    #[test]
    fn false_merge_audit_orders_riskiest_first_and_caps() {
        // A strong (1.0), a medium, and a weak accepted match; the audit lists the weakest first.
        let strong = fp(&["id", "total"], &[("u64", 2)]);
        let medium_r = fp(&["id", "total"], &[("u64", 2)]);
        let medium_a = fp(&["id", "amount"], &[("u64", 2)]);
        let weak_r = fp(&["id", "total"], &[("u64", 4)]);
        let weak_a = fp(&["id", "extra"], &[("u64", 2), ("String", 2)]);
        let removed = vec![
            node("m::S_Old", NodeKind::Struct, Some(strong.clone())),
            node("m::M_Old", NodeKind::Trait, Some(medium_r)),
            node("m::W_Old", NodeKind::Enum, Some(weak_r)),
        ];
        let added = vec![
            node("m::S_New", NodeKind::Struct, Some(strong)),
            node("m::M_New", NodeKind::Trait, Some(medium_a)),
            node("m::W_New", NodeKind::Enum, Some(weak_a)),
        ];
        let matches = fingerprint_matches(&removed, &added, 0.4);
        assert_eq!(matches.len(), 3, "all three pairs clear 0.4: {matches:?}");

        let audit = false_merge_audit(&matches, 2);
        assert_eq!(audit.len(), 2, "capped to two");
        assert!(
            audit[0].score <= audit[1].score,
            "ascending by score: {} then {}",
            audit[0].score,
            audit[1].score
        );
        assert_eq!(
            audit[0].removed_id, "m::W_Old",
            "weakest match audited first"
        );
    }
}
