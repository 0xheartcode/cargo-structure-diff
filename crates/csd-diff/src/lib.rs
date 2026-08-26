//! Diffing between two [`csd_ir::Graph`]s.
//!
//! A flat, identity-keyed set diff over nodes and edges, followed by two move-detection passes.
//!
//! [`seed_module_moves`] reconciles module `Added` + `Removed` using git's file-rename signal,
//! since modules carry no fingerprint and the similarity matcher cannot catch them; it runs only
//! when `DiffOptions::file_renames` is non-empty. [`detect_renames`] is the fingerprint similarity
//! matcher for the remaining items and runs only when `DiffOptions::rename_threshold` is `Some`.
//! With neither signal, the output is the pure set diff.
//!
//! [`member_diff`] is the member level of the SPEC 3.2 three-level diff (node add/remove, then
//! member matching within surviving nodes, then signature comparison). It is an additive helper
//! for the renderer and CLI; it does not change [`diff`]'s output.

use std::collections::{BTreeMap, BTreeSet};

use csd_ir::{similarity, Change, Edge, EdgeKind, Graph, Node, NodeKind, StableId};

/// A file rename reported by git (`git diff -M --name-status`). The harness feeds these in so the
/// differ can recover module moves precisely: modules carry no fingerprint, so the similarity
/// matcher can never catch them on its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRename {
    /// Path on the base side.
    pub old_path: String,
    /// Path on the head side.
    pub new_path: String,
}

/// Options controlling rename/move detection.
#[derive(Debug, Clone)]
pub struct DiffOptions {
    /// Similarity in `[0.0, 1.0]` at or above which a candidate pair is accepted as a move.
    /// `None` disables fingerprint rename detection (pure set diff).
    pub rename_threshold: Option<f32>,
    /// Git file renames used to seed module-level moves. Empty means no git signal is available.
    pub file_renames: Vec<FileRename>,
}

impl Default for DiffOptions {
    fn default() -> Self {
        Self {
            rename_threshold: Some(0.7),
            file_renames: Vec::new(),
        }
    }
}

/// Member-level delta between two matched type nodes, the member level of the SPEC 3.2 three-level
/// diff. All fields are sorted for deterministic output.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MemberDelta {
    /// Member names present only on the head side.
    pub added: Vec<String>,
    /// Member names present only on the base side.
    pub removed: Vec<String>,
    /// Member names present on both sides whose type appears to have changed. See [`member_diff`]
    /// for the exact (and limited) meaning given the current [`Fingerprint`] shape.
    pub changed: Vec<String>,
}

/// Report member-level changes between two nodes matched at the node level.
///
/// Members are matched by NAME using [`Fingerprint::members`]. A name only on the head side is
/// `added`; a name only on the base side is `removed`.
///
/// `changed` is the honest limit of the current [`Fingerprint`] shape. The fingerprint stores
/// member names as a set and field types as a separate `field_types` multiset (type name to
/// count); it does NOT map a member to its type, so a per-member retype is not directly
/// recoverable. The best available signal: when the member-name set is identical on both sides
/// (no adds or removes) yet the `field_types` multiset differs, at least one surviving member was
/// retyped. Which one is not recoverable, so every common member is reported as `changed`. When
/// names were added or removed, the `field_types` delta is attributed to those adds/removes and
/// `changed` stays empty, avoiding a false retype signal.
///
/// A node with no fingerprint on either side yields an empty [`MemberDelta`].
pub fn member_diff(before: &Node, after: &Node) -> MemberDelta {
    let (Some(bf), Some(af)) = (&before.fingerprint, &after.fingerprint) else {
        return MemberDelta::default();
    };

    let added: Vec<String> = af.members.difference(&bf.members).cloned().collect();
    let removed: Vec<String> = bf.members.difference(&af.members).cloned().collect();

    let changed = if added.is_empty() && removed.is_empty() && bf.field_types != af.field_types {
        // Names unchanged but the type multiset moved: a surviving member was retyped.
        bf.members.intersection(&af.members).cloned().collect()
    } else {
        Vec::new()
    };

    // BTreeSet iteration is already sorted; keep it explicit so the contract does not rely on it.
    MemberDelta {
        added,
        removed,
        changed,
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

    // Layer 2a: use git's file-rename signal to reconcile module Added + Removed into Moved/
    // Modified. Modules have no fingerprint, so the similarity matcher below cannot catch them.
    if !opts.file_renames.is_empty() {
        changes = seed_module_moves(changes, &opts.file_renames);
    }

    // Layer 2b: rewrite the remaining matched Added + Removed pairs into Moved/Modified via
    // fingerprint similarity. Skipped when no threshold is set, so `diff` returns the pure set diff.
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

/// Reconcile module `Added` + `Removed` into `Moved`/`Modified` using git file renames.
///
/// A renamed file implies its module moved (modules correspond to files). For each rename we look
/// for exactly one `Removed` module node on `old_path` and exactly one `Added` module node on
/// `new_path`; more than one on either side is ambiguous (a file with inline submodules) and is
/// left untouched, keeping precision over recall. A matched pair becomes `Moved` when the parent
/// module differs, else `Modified`.
fn seed_module_moves(changes: Vec<Change>, renames: &[FileRename]) -> Vec<Change> {
    let mut removed_by_file: BTreeMap<String, Vec<Node>> = BTreeMap::new();
    let mut added_by_file: BTreeMap<String, Vec<Node>> = BTreeMap::new();
    let mut out: Vec<Change> = Vec::new();
    for change in changes {
        match change {
            Change::Removed(n) if n.kind == NodeKind::Module => {
                removed_by_file
                    .entry(n.span.file.clone())
                    .or_default()
                    .push(n);
            }
            Change::Added(n) if n.kind == NodeKind::Module => {
                added_by_file
                    .entry(n.span.file.clone())
                    .or_default()
                    .push(n);
            }
            other => out.push(other),
        }
    }

    let mut consumed_removed: BTreeSet<String> = BTreeSet::new();
    let mut consumed_added: BTreeSet<String> = BTreeSet::new();
    for rename in renames {
        let (Some(rem), Some(add)) = (
            removed_by_file.get(&rename.old_path),
            added_by_file.get(&rename.new_path),
        ) else {
            continue;
        };
        if rem.len() != 1 || add.len() != 1 {
            continue;
        }
        let before = &rem[0];
        let after = &add[0];
        if before.id == after.id {
            continue;
        }
        let (from, to) = (parent(&before.id), parent(&after.id));
        if from != to {
            out.push(Change::Moved {
                node: after.id.clone(),
                from,
                to,
            });
        } else {
            out.push(Change::Modified {
                before: before.clone(),
                after: after.clone(),
            });
        }
        consumed_removed.insert(rename.old_path.clone());
        consumed_added.insert(rename.new_path.clone());
    }

    // Re-emit every module change the renames did not consume.
    for (file, nodes) in removed_by_file {
        if consumed_removed.contains(&file) {
            continue;
        }
        out.extend(nodes.into_iter().map(Change::Removed));
    }
    for (file, nodes) in added_by_file {
        if consumed_added.contains(&file) {
            continue;
        }
        out.extend(nodes.into_iter().map(Change::Added));
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
                file_renames: Vec::new(),
            },
        );
        assert_eq!(out, vec![Change::Added(after), Change::Removed(before)]);
    }

    fn mod_node(id: &str, file: &str) -> Node {
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

    fn renames(pairs: &[(&str, &str)]) -> DiffOptions {
        DiffOptions {
            rename_threshold: Some(0.7),
            file_renames: pairs
                .iter()
                .map(|(o, n)| FileRename {
                    old_path: (*o).to_string(),
                    new_path: (*n).to_string(),
                })
                .collect(),
        }
    }

    #[test]
    fn module_file_move_across_parents_is_moved() {
        let base = graph(
            vec![mod_node("crate::api::billing", "src/api/billing.rs")],
            vec![],
        );
        let head = graph(
            vec![mod_node("crate::infra::billing", "src/infra/billing.rs")],
            vec![],
        );
        let out = diff(
            &base,
            &head,
            renames(&[("src/api/billing.rs", "src/infra/billing.rs")]),
        );
        assert_eq!(
            out,
            vec![Change::Moved {
                node: StableId::new("crate::infra::billing"),
                from: StableId::new("crate::api"),
                to: StableId::new("crate::infra"),
            }]
        );
    }

    #[test]
    fn module_file_rename_same_parent_is_modified() {
        let before = mod_node("crate::billing", "src/billing.rs");
        let after = mod_node("crate::payments", "src/payments.rs");
        let out = diff(
            &graph(vec![before.clone()], vec![]),
            &graph(vec![after.clone()], vec![]),
            renames(&[("src/billing.rs", "src/payments.rs")]),
        );
        assert_eq!(out, vec![Change::Modified { before, after }]);
    }

    #[test]
    fn ambiguous_file_with_two_modules_is_not_merged() {
        // Two modules share src/m.rs (a file module plus an inline submodule), so the rename is
        // ambiguous and must be left as Added + Removed rather than guessed.
        let m = mod_node("crate::m", "src/m.rs");
        let inner = mod_node("crate::m::inner", "src/m.rs");
        let n = mod_node("crate::n", "src/n.rs");
        let out = diff(
            &graph(vec![m.clone(), inner.clone()], vec![]),
            &graph(vec![n.clone()], vec![]),
            renames(&[("src/m.rs", "src/n.rs")]),
        );
        assert_eq!(
            out,
            vec![Change::Added(n), Change::Removed(m), Change::Removed(inner),]
        );
    }

    #[test]
    fn without_git_signal_module_rename_stays_add_remove() {
        // No file_renames and modules have no fingerprint, so nothing reconciles them.
        let before = mod_node("crate::billing", "src/billing.rs");
        let after = mod_node("crate::payments", "src/payments.rs");
        let out = diff(
            &graph(vec![before.clone()], vec![]),
            &graph(vec![after.clone()], vec![]),
            DiffOptions::default(),
        );
        assert_eq!(out, vec![Change::Added(after), Change::Removed(before)]);
    }

    #[test]
    fn member_added_is_reported_as_added() {
        let before = fp_node("m::T", NodeKind::Struct, fp(&["a"], &[("u64", 1)]));
        let after = fp_node("m::T", NodeKind::Struct, fp(&["a", "b"], &[("u64", 2)]));
        let d = member_diff(&before, &after);
        assert_eq!(d.added, vec!["b".to_string()]);
        assert!(d.removed.is_empty());
        assert!(d.changed.is_empty());
    }

    #[test]
    fn member_removed_is_reported_as_removed() {
        let before = fp_node("m::T", NodeKind::Struct, fp(&["a", "b"], &[("u64", 2)]));
        let after = fp_node("m::T", NodeKind::Struct, fp(&["a"], &[("u64", 1)]));
        let d = member_diff(&before, &after);
        assert_eq!(d.removed, vec!["b".to_string()]);
        assert!(d.added.is_empty());
        assert!(d.changed.is_empty());
    }

    #[test]
    fn retyped_member_is_reported_as_changed() {
        // Same name set, field_types multiset differs: a surviving member was retyped.
        let before = fp_node("m::T", NodeKind::Struct, fp(&["a"], &[("u64", 1)]));
        let after = fp_node("m::T", NodeKind::Struct, fp(&["a"], &[("String", 1)]));
        let d = member_diff(&before, &after);
        assert_eq!(d.changed, vec!["a".to_string()]);
        assert!(d.added.is_empty());
        assert!(d.removed.is_empty());
    }

    #[test]
    fn identical_members_yield_empty_delta() {
        let f = fp(&["a", "b"], &[("u64", 2)]);
        let before = fp_node("m::T", NodeKind::Struct, f.clone());
        let after = fp_node("m::T", NodeKind::Struct, f);
        assert_eq!(member_diff(&before, &after), MemberDelta::default());
    }

    #[test]
    fn no_fingerprint_yields_empty_delta() {
        let before = node("a");
        let after = node("a");
        assert_eq!(member_diff(&before, &after), MemberDelta::default());
    }

    #[test]
    fn member_delta_is_sorted_and_deterministic() {
        let before = fp_node("m::T", NodeKind::Struct, fp(&["a", "c"], &[("u64", 2)]));
        let after = fp_node(
            "m::T",
            NodeKind::Struct,
            fp(&["a", "z", "b"], &[("u64", 3)]),
        );
        let d = member_diff(&before, &after);
        assert_eq!(d.added, vec!["b".to_string(), "z".to_string()]);
        assert_eq!(d.removed, vec!["c".to_string()]);
        assert!(d.changed.is_empty());
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
