//! Diffing between two [`csd_ir::Graph`]s.
//!
//! Two layers, tracked in the backlog (area `diff`):
//! 1. a flat set diff over nodes and edges, and
//! 2. a rename/move matcher that rewrites `Added` + `Removed` pairs into `Moved`/`Modified` when
//!    structural fingerprints are similar enough.
//!
//! This file implements both layers. Layer 2 ([`detect_renames`]) only runs when
//! `DiffOptions::rename_threshold` is `Some`; with `None` the output is the pure set diff.

use std::collections::BTreeMap;

use csd_ir::{similarity, Change, Edge, EdgeKind, Graph, Node, StableId};

/// Options controlling rename/move detection.
#[derive(Debug, Clone, Copy)]
pub struct DiffOptions {
    /// Similarity in `[0.0, 1.0]` at or above which a candidate pair is accepted as a move.
    /// `None` disables rename detection (pure set diff).
    pub rename_threshold: Option<f32>,
}

impl Default for DiffOptions {
    fn default() -> Self {
        Self {
            rename_threshold: Some(0.7),
        }
    }
}

/// Identity of an edge for set-diff purposes. `span` is deliberately excluded: it shifts on any
/// edit above the edge and would spuriously flip every edge to added/removed.
type EdgeId<'a> = (&'a StableId, &'a StableId, EdgeKind, Option<u32>);

/// Compute the delta from `base` to `head`.
///
/// Flat, identity-keyed set diff. Nodes are keyed by [`StableId`]; edges by
/// `(from, to, kind, ordinal)`. Output is sorted into a stable order so golden tests do not flake.
/// Input ordering is not relied upon.
pub fn diff(base: &Graph, head: &Graph, opts: DiffOptions) -> Vec<Change> {
    let mut changes = Vec::new();

    let base_nodes: BTreeMap<&StableId, &Node> = base.nodes.iter().map(|n| (&n.id, n)).collect();
    let head_nodes: BTreeMap<&StableId, &Node> = head.nodes.iter().map(|n| (&n.id, n)).collect();

    for (id, after) in &head_nodes {
        match base_nodes.get(id) {
            None => changes.push(Change::Added((*after).clone())),
            Some(before) => {
                if content_changed(before, after) {
                    changes.push(Change::Modified {
                        before: (*before).clone(),
                        after: (*after).clone(),
                    });
                }
            }
        }
    }

    for (id, before) in &base_nodes {
        if !head_nodes.contains_key(id) {
            changes.push(Change::Removed((*before).clone()));
        }
    }

    let base_edges: BTreeMap<EdgeId, &Edge> = base.edges.iter().map(|e| (edge_id(e), e)).collect();
    let head_edges: BTreeMap<EdgeId, &Edge> = head.edges.iter().map(|e| (edge_id(e), e)).collect();

    for (id, edge) in &head_edges {
        if !base_edges.contains_key(id) {
            changes.push(Change::EdgeAdded((*edge).clone()));
        }
    }
    for (id, edge) in &base_edges {
        if !head_edges.contains_key(id) {
            changes.push(Change::EdgeRemoved((*edge).clone()));
        }
    }

    // Layer 2: rewrite matched Added + Removed pairs into Moved/Modified. Skipped entirely when
    // no threshold is set, so `diff` returns exactly the pure set diff in that case.
    if let Some(threshold) = opts.rename_threshold {
        changes = detect_renames(changes, threshold);
    }

    changes.sort_by(|a, b| order_key(a).cmp(&order_key(b)));
    changes
}

/// Rewrite matched `Added` + `Removed` node pairs into `Moved`/`Modified`.
///
/// A candidate pair is a (removed, added) pair with the SAME [`NodeKind`] and a fingerprint on
/// BOTH sides; anything else can never rename-match (this is the false-merge guard). Each candidate
/// scoring at or above `threshold` is kept, candidates are ordered `(score desc, removed.id asc,
/// added.id asc)`, and assignment is greedy: a removed or added node already used is skipped, so no
/// node is ever reused. Unmatched `Added`/`Removed` and every non-node change pass through.
fn detect_renames(changes: Vec<Change>, threshold: f32) -> Vec<Change> {
    let mut removed: Vec<Node> = Vec::new();
    let mut added: Vec<Node> = Vec::new();
    let mut out: Vec<Change> = Vec::new();
    for change in changes {
        match change {
            Change::Removed(n) => removed.push(n),
            Change::Added(n) => added.push(n),
            other => out.push(other),
        }
    }

    // Scored candidate pairs, held as `(score, removed index, added index)`.
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

    // Deterministic order: best score first, then by removed then added id.
    candidates.sort_by(|x, y| {
        y.0.total_cmp(&x.0)
            .then_with(|| removed[x.1].id.cmp(&removed[y.1].id))
            .then_with(|| added[x.2].id.cmp(&added[y.2].id))
    });

    let mut removed_taken = vec![false; removed.len()];
    let mut added_taken = vec![false; added.len()];
    for (_score, ri, ai) in candidates {
        if removed_taken[ri] || added_taken[ai] {
            continue;
        }
        removed_taken[ri] = true;
        added_taken[ai] = true;
        let r = &removed[ri];
        let a = &added[ai];
        let (from, to) = (parent(&r.id), parent(&a.id));
        if from != to {
            out.push(Change::Moved {
                node: a.id.clone(),
                from,
                to,
            });
        } else {
            out.push(Change::Modified {
                before: r.clone(),
                after: a.clone(),
            });
        }
    }

    for (ri, r) in removed.into_iter().enumerate() {
        if !removed_taken[ri] {
            out.push(Change::Removed(r));
        }
    }
    for (ai, a) in added.into_iter().enumerate() {
        if !added_taken[ai] {
            out.push(Change::Added(a));
        }
    }
    out
}

/// Parent module id: the node's id with the last `::segment` stripped.
///
/// A node with no `::` (a crate-root item) has the empty [`StableId`] as its parent; that empty id
/// is the documented crate-root sentinel, so two such nodes share a parent and read as a rename.
fn parent(id: &StableId) -> StableId {
    match id.as_str().rfind("::") {
        Some(pos) => StableId::new(&id.as_str()[..pos]),
        None => StableId::new(""),
    }
}

/// The set-diff identity of an edge (`span` excluded, same reasoning as [`EdgeId`]).
fn edge_id(e: &Edge) -> EdgeId<'_> {
    (&e.from, &e.to, e.kind, e.ordinal)
}

/// Whether two nodes matched by id represent a real change.
///
/// Compares `kind`, `attrs`, and `fingerprint`. `span` is ignored on purpose: it shifts on any
/// edit above the node and would flag everything as Modified.
fn content_changed(before: &Node, after: &Node) -> bool {
    before.kind != after.kind
        || before.attrs != after.attrs
        || before.fingerprint != after.fingerprint
}

/// A total, deterministic sort key over changes: `(variant rank, ids/edge tuple)`.
fn order_key(c: &Change) -> (u8, &StableId, &StableId, EdgeKind, Option<u32>) {
    // Placeholders for the edge-only fields when the change is node-shaped.
    let pad = EdgeKind::Uses;
    match c {
        Change::Added(n) => (0, &n.id, &n.id, pad, None),
        Change::Removed(n) => (1, &n.id, &n.id, pad, None),
        Change::Modified { before, .. } => (2, &before.id, &before.id, pad, None),
        Change::Moved { node, from, .. } => (3, node, from, pad, None),
        Change::EdgeAdded(e) => (4, &e.from, &e.to, e.kind, e.ordinal),
        Change::EdgeRemoved(e) => (5, &e.from, &e.to, e.kind, e.ordinal),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use csd_ir::{Fingerprint, NodeKind, SourceSpan};
    use std::collections::BTreeSet;

    fn span(start: u32) -> SourceSpan {
        SourceSpan {
            file: "src/lib.rs".into(),
            start,
            end: start + 1,
        }
    }

    fn node(id: &str) -> Node {
        Node {
            id: StableId::new(id),
            kind: NodeKind::Struct,
            span: span(0),
            attrs: BTreeMap::new(),
            fingerprint: None,
        }
    }

    /// A fingerprint carrying just members and field types; enough to drive similarity.
    fn fp(members: &[&str], fields: &[(&str, u32)]) -> Fingerprint {
        Fingerprint {
            members: members.iter().map(|s| s.to_string()).collect(),
            field_types: fields.iter().map(|(t, c)| (t.to_string(), *c)).collect(),
            neighbors: BTreeSet::new(),
            doc_hash: 0,
        }
    }

    /// A node with an explicit kind and fingerprint.
    fn fp_node(id: &str, kind: NodeKind, fingerprint: Fingerprint) -> Node {
        Node {
            id: StableId::new(id),
            kind,
            span: span(0),
            attrs: BTreeMap::new(),
            fingerprint: Some(fingerprint),
        }
    }

    fn edge(from: &str, to: &str) -> Edge {
        Edge {
            from: StableId::new(from),
            to: StableId::new(to),
            kind: EdgeKind::Uses,
            span: span(0),
            ordinal: None,
        }
    }

    fn graph(nodes: Vec<Node>, edges: Vec<Edge>) -> Graph {
        Graph { nodes, edges }
    }

    #[test]
    fn node_only_in_head_is_added() {
        let base = graph(vec![], vec![]);
        let head = graph(vec![node("a")], vec![]);
        let out = diff(&base, &head, DiffOptions::default());
        assert_eq!(out, vec![Change::Added(node("a"))]);
    }

    #[test]
    fn node_only_in_base_is_removed() {
        let base = graph(vec![node("a")], vec![]);
        let head = graph(vec![], vec![]);
        let out = diff(&base, &head, DiffOptions::default());
        assert_eq!(out, vec![Change::Removed(node("a"))]);
    }

    #[test]
    fn changed_attrs_is_modified() {
        let before = node("a");
        let mut after = node("a");
        after.attrs.insert("vis".into(), "pub".into());
        let out = diff(
            &graph(vec![before.clone()], vec![]),
            &graph(vec![after.clone()], vec![]),
            DiffOptions::default(),
        );
        assert_eq!(out, vec![Change::Modified { before, after }]);
    }

    #[test]
    fn changed_fingerprint_is_modified() {
        let before = node("a");
        let mut after = node("a");
        after.kind = NodeKind::Enum;
        let out = diff(
            &graph(vec![before.clone()], vec![]),
            &graph(vec![after.clone()], vec![]),
            DiffOptions::default(),
        );
        assert_eq!(out, vec![Change::Modified { before, after }]);
    }

    #[test]
    fn span_only_change_emits_nothing() {
        let before = node("a");
        let mut after = node("a");
        after.span = span(999);
        let out = diff(
            &graph(vec![before], vec![]),
            &graph(vec![after], vec![]),
            DiffOptions::default(),
        );
        assert!(out.is_empty());
    }

    #[test]
    fn edge_added_and_removed_detected() {
        let base = graph(vec![], vec![edge("a", "b")]);
        let head = graph(vec![], vec![edge("c", "d")]);
        let out = diff(&base, &head, DiffOptions::default());
        assert_eq!(
            out,
            vec![
                Change::EdgeAdded(edge("c", "d")),
                Change::EdgeRemoved(edge("a", "b")),
            ]
        );
    }

    #[test]
    fn edge_span_only_change_emits_nothing() {
        let mut moved = edge("a", "b");
        moved.span = span(500);
        let out = diff(
            &graph(vec![], vec![edge("a", "b")]),
            &graph(vec![], vec![moved]),
            DiffOptions::default(),
        );
        assert!(out.is_empty());
    }

    #[test]
    fn output_is_deterministic() {
        let base = graph(
            vec![node("keep"), node("gone"), node("mod")],
            vec![edge("x", "y")],
        );
        let mut modded = node("mod");
        modded.attrs.insert("k".into(), "v".into());
        let head = graph(
            vec![node("new"), node("keep"), modded],
            vec![edge("p", "q")],
        );
        let first = diff(&base, &head, DiffOptions::default());
        let second = diff(&base, &head, DiffOptions::default());
        assert_eq!(first, second);
    }

    #[test]
    fn pure_rename_same_module_is_modified() {
        let f = fp(&["id", "total"], &[("u64", 1)]);
        let before = fp_node("m::Old", NodeKind::Struct, f.clone());
        let after = fp_node("m::New", NodeKind::Struct, f);
        let out = diff(
            &graph(vec![before.clone()], vec![]),
            &graph(vec![after.clone()], vec![]),
            DiffOptions::default(),
        );
        assert_eq!(out, vec![Change::Modified { before, after }]);
    }

    #[test]
    fn identical_struct_moved_to_new_module_is_moved() {
        let f = fp(&["id", "total"], &[("u64", 1)]);
        let before = fp_node("billing::Order", NodeKind::Struct, f.clone());
        let after = fp_node("payments::Order", NodeKind::Struct, f);
        let out = diff(
            &graph(vec![before], vec![]),
            &graph(vec![after], vec![]),
            DiffOptions::default(),
        );
        assert_eq!(
            out,
            vec![Change::Moved {
                node: StableId::new("payments::Order"),
                from: StableId::new("billing"),
                to: StableId::new("payments"),
            }]
        );
    }

    #[test]
    fn genuinely_different_pair_is_not_merged() {
        // Disjoint members and disjoint field types keep the score well under the threshold.
        let before = fp_node(
            "m::Alpha",
            NodeKind::Struct,
            fp(&["a", "b", "c"], &[("u64", 1)]),
        );
        let after = fp_node(
            "m::Beta",
            NodeKind::Struct,
            fp(&["x", "y", "z"], &[("String", 1)]),
        );
        let out = diff(
            &graph(vec![before.clone()], vec![]),
            &graph(vec![after.clone()], vec![]),
            DiffOptions::default(),
        );
        assert_eq!(
            out,
            vec![Change::Added(after), Change::Removed(before)],
            "low-similarity pair must stay Added + Removed"
        );
    }

    #[test]
    fn different_kinds_never_match() {
        // Identical structure but Struct vs Enum: not a candidate, so no merge.
        let f = fp(&["Read", "Write"], &[("u8", 1)]);
        let before = fp_node("m::Mode", NodeKind::Struct, f.clone());
        let after = fp_node("m::Mode2", NodeKind::Enum, f);
        let out = diff(
            &graph(vec![before.clone()], vec![]),
            &graph(vec![after.clone()], vec![]),
            DiffOptions::default(),
        );
        assert_eq!(out, vec![Change::Added(after), Change::Removed(before)]);
    }

    #[test]
    fn one_to_one_assignment_pairs_each_node_once() {
        // R1/A1 share one structure, R2/A2 another; cross pairs are disjoint and score too low to
        // be candidates, so greedy assignment pairs each removed with exactly one added.
        let f1 = fp(&["a", "b"], &[("u64", 1)]);
        let f2 = fp(&["c", "d"], &[("String", 1)]);
        let r1 = fp_node("m::Old1", NodeKind::Struct, f1.clone());
        let r2 = fp_node("m::Old2", NodeKind::Struct, f2.clone());
        let a1 = fp_node("m::New1", NodeKind::Struct, f1);
        let a2 = fp_node("m::New2", NodeKind::Struct, f2);
        let out = diff(
            &graph(vec![r1.clone(), r2.clone()], vec![]),
            &graph(vec![a1.clone(), a2.clone()], vec![]),
            DiffOptions::default(),
        );
        assert_eq!(
            out,
            vec![
                Change::Modified {
                    before: r1,
                    after: a1,
                },
                Change::Modified {
                    before: r2,
                    after: a2,
                },
            ]
        );
    }

    #[test]
    fn threshold_none_is_pure_set_diff() {
        let f = fp(&["id", "total"], &[("u64", 1)]);
        let before = fp_node("m::Old", NodeKind::Struct, f.clone());
        let after = fp_node("m::New", NodeKind::Struct, f);
        let out = diff(
            &graph(vec![before.clone()], vec![]),
            &graph(vec![after.clone()], vec![]),
            DiffOptions {
                rename_threshold: None,
            },
        );
        assert_eq!(out, vec![Change::Added(after), Change::Removed(before)]);
    }

    #[test]
    fn rename_detection_is_deterministic() {
        let f1 = fp(&["a", "b"], &[("u64", 1)]);
        let f2 = fp(&["c", "d"], &[("String", 1)]);
        let base = graph(
            vec![
                fp_node("m::Old1", NodeKind::Struct, f1.clone()),
                fp_node("billing::Order", NodeKind::Struct, f2.clone()),
            ],
            vec![],
        );
        let head = graph(
            vec![
                fp_node("m::New1", NodeKind::Struct, f1),
                fp_node("payments::Order", NodeKind::Struct, f2),
            ],
            vec![],
        );
        let first = diff(&base, &head, DiffOptions::default());
        let second = diff(&base, &head, DiffOptions::default());
        assert_eq!(first, second);
    }
}
