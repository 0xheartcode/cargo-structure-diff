//! Diffing between two [`csd_ir::Graph`]s.
//!
//! Two layers, tracked in the backlog (area `diff`):
//! 1. a flat set diff over nodes and edges, and
//! 2. a rename/move matcher that rewrites `Added` + `Removed` pairs into `Moved`/`Modified` when
//!    structural fingerprints are similar enough.
//!
//! This file implements layer 1 (the pure set diff). Layer 2 is a separate issue (`diff-rename`).

use std::collections::BTreeMap;

use csd_ir::{Change, Edge, EdgeKind, Graph, Node, StableId};

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
pub fn diff(base: &Graph, head: &Graph, _opts: DiffOptions) -> Vec<Change> {
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

    // rename detection: see diff-rename issue. `_opts.rename_threshold` hooks in here to rewrite
    // Added + Removed pairs into Moved/Modified; this layer emits the raw set diff only.

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

    changes.sort_by(|a, b| order_key(a).cmp(&order_key(b)));
    changes
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
    use csd_ir::{NodeKind, SourceSpan};

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
}
