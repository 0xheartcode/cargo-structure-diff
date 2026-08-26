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
//!
//! [`call_tree_diff`] is the ordered call-sequence diff of SPEC 3.3. Call order is significant, so
//! a set diff over `Calls` edges cannot see a reordering. It runs Zhang-Shasha ordered tree edit
//! distance over the call tree/forest the caller extracts and returns an [`EditScript`]. Like
//! [`member_diff`] it is additive and pure; it does not touch [`diff`]'s output.

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

// ---------------------------------------------------------------------------
// SPEC 3.3: ordered call-sequence diff (Zhang-Shasha tree edit distance).
// ---------------------------------------------------------------------------

/// A node in an ordered call tree.
///
/// `label` is the callee id. Children are the callees in ordinal order. A linear call sequence is
/// a root whose children are the calls in order; nested `children` model a real call tree. Order is
/// significant: `[a, b]` and `[b, a]` are different trees, which is the whole point of using tree
/// edit distance over a set diff (SPEC 3.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallNode {
    /// Callee id (the label compared for match/relabel).
    pub label: String,
    /// Ordered children (callees).
    pub children: Vec<CallNode>,
}

impl CallNode {
    /// A leaf with no children.
    pub fn leaf(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            children: Vec::new(),
        }
    }

    /// A node with ordered children.
    pub fn new(label: impl Into<String>, children: Vec<CallNode>) -> Self {
        Self {
            label: label.into(),
            children,
        }
    }
}

/// One step in an [`EditScript`].
///
/// Zhang-Shasha yields insert/delete/relabel plus (unemitted) matches. It has no native "move";
/// the post-pass in [`call_tree_diff`] rewrites a delete+insert of the same label into [`EditOp::Moved`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditOp {
    /// Insert a head-side node with this label.
    Insert(String),
    /// Delete a base-side node with this label.
    Delete(String),
    /// Relabel a base-side node to a head-side label.
    Relabel {
        /// Base-side label.
        from: String,
        /// Head-side label.
        to: String,
    },
    /// A base and head node aligned unchanged (zero cost). Recovered internally to trace the
    /// alignment; [`call_tree_diff`] does not emit it, so an identical tree yields an empty script.
    Match(String),
    /// A reordered call, surfaced by the move post-pass from a paired delete+insert of one label.
    Moved(String),
}

/// The recovered edit script between two call trees.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditScript {
    /// The edits, in a deterministic order.
    pub ops: Vec<EditOp>,
    /// The Zhang-Shasha edit distance (unit cost per insert/delete/relabel, zero per match). The
    /// move post-pass only relabels op pairs for readability; it does not change this distance, so
    /// a reordered call still costs its delete+insert (2).
    pub cost: u32,
}

/// Ordered tree edit distance between two call trees, with a recovered [`EditScript`].
///
/// # Algorithm (Zhang-Shasha)
///
/// 1. Postorder-flatten each tree ([`flatten_postorder`]). Each node gets a 1-based postorder
///    index; `lld[i]` is the postorder index of the leftmost leaf descendant of node `i`.
/// 2. Keyroots are the nodes that are either the root or have a left sibling, computed as the
///    largest index for each distinct `lld` value. Only keyroot pairs need a forest-distance pass.
/// 3. For each keyroot pair, fill a forest-distance DP ([`forest_dist`]) and copy its tree cells
///    into the global `treedist` table. `treedist[n][m]` is the whole-tree distance (the cost).
/// 4. Recover the script by backtracking the forest distance of the root pair ([`recover`]),
///    recursing into matched subtrees. Ties break deterministically: delete, then insert, then
///    match/relabel. That bias makes a reordering fall out as delete+insert (a move) rather than
///    two unrelated relabels.
///
/// # Move post-pass
///
/// Zhang-Shasha cannot say "moved": a reordered call `[a, b] -> [b, a]` comes back as a delete of
/// one label at its old slot and an insert of the same label at its new slot. [`detect_moves`]
/// pairs each `Delete(x)` with a matching `Insert(x)`, rewriting the delete into [`EditOp::Moved`]
/// and dropping the paired insert, so the reorder reads as a move instead of a blind add+remove.
/// The pairing count per label is `min(deletes, inserts)` and consumes ops left to right, so it is
/// deterministic. `cost` is left as the raw edit distance.
pub fn call_tree_diff(base: &CallNode, head: &CallNode) -> EditScript {
    let (labels1, lld1, keyroots1) = flatten_postorder(base);
    let (labels2, lld2, keyroots2) = flatten_postorder(head);
    let n = labels1.len() - 1;
    let m = labels2.len() - 1;

    // treedist[i][j] = edit distance between the subtree rooted at postorder i (tree1) and j (tree2).
    let mut treedist = vec![vec![0u32; m + 1]; n + 1];
    for &i in &keyroots1 {
        for &j in &keyroots2 {
            let fd = forest_dist(i, j, &labels1, &lld1, &labels2, &lld2, &treedist);
            let (li, lj) = (lld1[i], lld2[j]);
            // Copy the tree-rooted cells of this forest distance into the global table.
            for di in li..=i {
                for dj in lj..=j {
                    if lld1[di] == li && lld2[dj] == lj {
                        treedist[di][dj] = fd[di][dj];
                    }
                }
            }
        }
    }

    let cost = treedist[n][m];
    let mut ops = recover(n, m, &labels1, &lld1, &labels2, &lld2, &treedist);
    // Matches are the zero-cost baseline; drop them so an identical tree yields an empty script.
    ops.retain(|op| !matches!(op, EditOp::Match(_)));
    let ops = detect_moves(ops);
    EditScript { ops, cost }
}

/// Postorder-flatten a tree into 1-based `(labels, lld, keyroots)`.
///
/// Index 0 is an unused sentinel so the DP can address `i - 1` at the boundary. `labels[i]` is the
/// label of postorder node `i`; `lld[i]` is the postorder index of its leftmost leaf descendant
/// (itself, for a leaf). Keyroots are sorted ascending.
fn flatten_postorder(root: &CallNode) -> (Vec<String>, Vec<usize>, Vec<usize>) {
    let mut labels = vec![String::new()];
    let mut lld = vec![0usize];

    // Returns the postorder index assigned to `node`.
    fn visit(node: &CallNode, labels: &mut Vec<String>, lld: &mut Vec<usize>) -> usize {
        let mut first_child_lld = None;
        for (idx, child) in node.children.iter().enumerate() {
            let ci = visit(child, labels, lld);
            if idx == 0 {
                first_child_lld = Some(lld[ci]);
            }
        }
        labels.push(node.label.clone());
        let me = labels.len() - 1;
        // Leftmost leaf descendant: that of the first child, or self when a leaf.
        lld.push(first_child_lld.unwrap_or(me));
        me
    }
    visit(root, &mut labels, &mut lld);

    // Keyroot for each distinct lld value is its largest postorder index; iterating ascending and
    // overwriting keeps that maximum.
    let mut kr_by_lld: BTreeMap<usize, usize> = BTreeMap::new();
    for (i, &l) in lld.iter().enumerate().skip(1) {
        kr_by_lld.insert(l, i);
    }
    let mut keyroots: Vec<usize> = kr_by_lld.into_values().collect();
    keyroots.sort_unstable();
    (labels, lld, keyroots)
}

/// Fill the forest-distance DP for keyroots `i` (tree1) and `j` (tree2).
///
/// Returns the full matrix; only cells `[li - 1..=i][lj - 1..=j]` are meaningful, where
/// `li = lld1[i]` and `lj = lld2[j]`. Tree-rooted cells (both nodes on the leftmost path) use a
/// relabel step; other cells fold in the precomputed `treedist` of the aligned subtrees.
fn forest_dist(
    i: usize,
    j: usize,
    labels1: &[String],
    lld1: &[usize],
    labels2: &[String],
    lld2: &[usize],
    treedist: &[Vec<u32>],
) -> Vec<Vec<u32>> {
    let (li, lj) = (lld1[i], lld2[j]);
    let mut fd = vec![vec![0u32; j + 1]; i + 1];

    // Empty-forest borders: delete the whole base prefix, or insert the whole head prefix.
    for di in li..=i {
        fd[di][lj - 1] = fd[di - 1][lj - 1] + 1;
    }
    for dj in lj..=j {
        fd[li - 1][dj] = fd[li - 1][dj - 1] + 1;
    }

    for di in li..=i {
        for dj in lj..=j {
            let del = fd[di - 1][dj] + 1;
            let ins = fd[di][dj - 1] + 1;
            if lld1[di] == li && lld2[dj] == lj {
                // Both nodes root a subtree of this forest: relabel/match them directly.
                let relabel = u32::from(labels1[di] != labels2[dj]);
                fd[di][dj] = del.min(ins).min(fd[di - 1][dj - 1] + relabel);
            } else {
                // Otherwise align the two subtrees, then continue with the forests left of them.
                let sub = fd[lld1[di] - 1][lld2[dj] - 1] + treedist[di][dj];
                fd[di][dj] = del.min(ins).min(sub);
            }
        }
    }
    fd
}

/// Backtrack the forest distance of keyroot pair `(i, j)` into ops in forward order.
///
/// Recurses into matched subtrees. Ties break deterministically as delete, then insert, then
/// match/relabel, so a reordering surfaces as delete+insert for the move post-pass to pair.
fn recover(
    i: usize,
    j: usize,
    labels1: &[String],
    lld1: &[usize],
    labels2: &[String],
    lld2: &[usize],
    treedist: &[Vec<u32>],
) -> Vec<EditOp> {
    let fd = forest_dist(i, j, labels1, lld1, labels2, lld2, treedist);
    let (li, lj) = (lld1[i], lld2[j]);
    let mut rev: Vec<EditOp> = Vec::new();
    let (mut di, mut dj) = (i, j);

    while di >= li || dj >= lj {
        if di < li {
            // Base forest exhausted: only inserts remain.
            rev.push(EditOp::Insert(labels2[dj].clone()));
            dj -= 1;
        } else if dj < lj {
            // Head forest exhausted: only deletes remain.
            rev.push(EditOp::Delete(labels1[di].clone()));
            di -= 1;
        } else if lld1[di] == li && lld2[dj] == lj {
            // Tree-rooted cell: delete, insert, or match/relabel.
            let del = fd[di - 1][dj] + 1;
            let ins = fd[di][dj - 1] + 1;
            let relabel = u32::from(labels1[di] != labels2[dj]);
            let cur = fd[di][dj];
            if cur == del {
                rev.push(EditOp::Delete(labels1[di].clone()));
                di -= 1;
            } else if cur == ins {
                rev.push(EditOp::Insert(labels2[dj].clone()));
                dj -= 1;
            } else if relabel == 0 {
                rev.push(EditOp::Match(labels1[di].clone()));
                di -= 1;
                dj -= 1;
            } else {
                rev.push(EditOp::Relabel {
                    from: labels1[di].clone(),
                    to: labels2[dj].clone(),
                });
                di -= 1;
                dj -= 1;
            }
        } else {
            // Forest cell: delete, insert, or descend into the aligned subtree pair.
            let del = fd[di - 1][dj] + 1;
            let ins = fd[di][dj - 1] + 1;
            let cur = fd[di][dj];
            if cur == del {
                rev.push(EditOp::Delete(labels1[di].clone()));
                di -= 1;
            } else if cur == ins {
                rev.push(EditOp::Insert(labels2[dj].clone()));
                dj -= 1;
            } else {
                let sub = recover(di, dj, labels1, lld1, labels2, lld2, treedist);
                for op in sub.into_iter().rev() {
                    rev.push(op);
                }
                di = lld1[di] - 1;
                dj = lld2[dj] - 1;
            }
        }
    }

    rev.reverse();
    rev
}

/// Rewrite paired `Delete(x)` + `Insert(x)` into `Moved(x)`.
///
/// Zhang-Shasha has no move op, so a reordered call reads as a delete of a label at its old slot
/// plus an insert of the same label at its new slot. For each label the number of moves is
/// `min(deletes, inserts)`; the first that many deletes become [`EditOp::Moved`] and the first that
/// many inserts are dropped. Left-to-right consumption keeps it deterministic.
fn detect_moves(ops: Vec<EditOp>) -> Vec<EditOp> {
    let mut del_counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut ins_counts: BTreeMap<String, usize> = BTreeMap::new();
    for op in &ops {
        match op {
            EditOp::Delete(x) => *del_counts.entry(x.clone()).or_default() += 1,
            EditOp::Insert(x) => *ins_counts.entry(x.clone()).or_default() += 1,
            _ => {}
        }
    }

    // Remaining deletes to convert / inserts to drop, per label.
    let mut del_budget: BTreeMap<String, usize> = BTreeMap::new();
    let mut ins_drop: BTreeMap<String, usize> = BTreeMap::new();
    for (label, &dc) in &del_counts {
        let moves = dc.min(ins_counts.get(label).copied().unwrap_or(0));
        if moves > 0 {
            del_budget.insert(label.clone(), moves);
            ins_drop.insert(label.clone(), moves);
        }
    }

    let mut out = Vec::with_capacity(ops.len());
    for op in ops {
        match op {
            EditOp::Delete(x) if del_budget.get(&x).copied().unwrap_or(0) > 0 => {
                *del_budget.get_mut(&x).unwrap() -= 1;
                out.push(EditOp::Moved(x));
            }
            EditOp::Insert(x) if ins_drop.get(&x).copied().unwrap_or(0) > 0 => {
                *ins_drop.get_mut(&x).unwrap() -= 1;
                // Paired with the delete above; drop it.
            }
            other => out.push(other),
        }
    }
    out
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

    // --- call_tree_diff (SPEC 3.3) ---

    /// A root labelled `entry` whose children are the given leaf calls in order.
    fn seq(labels: &[&str]) -> CallNode {
        CallNode::new("entry", labels.iter().map(|l| CallNode::leaf(*l)).collect())
    }

    #[test]
    fn identical_trees_yield_empty_script() {
        let base = seq(&["a", "b", "c"]);
        let head = seq(&["a", "b", "c"]);
        let script = call_tree_diff(&base, &head);
        assert_eq!(script.cost, 0);
        assert!(script.ops.is_empty());
    }

    #[test]
    fn inserted_leaf_is_a_single_insert() {
        let base = seq(&["a", "b"]);
        let head = seq(&["a", "b", "c"]);
        let script = call_tree_diff(&base, &head);
        assert_eq!(script.cost, 1);
        assert_eq!(script.ops, vec![EditOp::Insert("c".to_string())]);
    }

    #[test]
    fn deleted_leaf_is_a_single_delete() {
        let base = seq(&["a", "b", "c"]);
        let head = seq(&["a", "b"]);
        let script = call_tree_diff(&base, &head);
        assert_eq!(script.cost, 1);
        assert_eq!(script.ops, vec![EditOp::Delete("c".to_string())]);
    }

    #[test]
    fn relabelled_leaf_is_a_relabel() {
        let base = seq(&["a", "b"]);
        let head = seq(&["a", "c"]);
        let script = call_tree_diff(&base, &head);
        assert_eq!(script.cost, 1);
        assert_eq!(
            script.ops,
            vec![EditOp::Relabel {
                from: "b".to_string(),
                to: "c".to_string(),
            }]
        );
    }

    #[test]
    fn reordered_pair_surfaces_as_a_move() {
        // [a, b] -> [b, a]: Zhang-Shasha produces a delete + insert of the same label, which the
        // move post-pass pairs into a single Moved rather than two unrelated edits.
        let base = seq(&["a", "b"]);
        let head = seq(&["b", "a"]);
        let script = call_tree_diff(&base, &head);
        assert_eq!(script.cost, 2, "a reorder is delete+insert = distance 2");
        let moved: Vec<_> = script
            .ops
            .iter()
            .filter(|op| matches!(op, EditOp::Moved(_)))
            .collect();
        assert_eq!(
            moved.len(),
            1,
            "exactly one call reads as moved: {:?}",
            script.ops
        );
        // No blind add/remove survives the post-pass for a pure reorder.
        assert!(
            !script
                .ops
                .iter()
                .any(|op| matches!(op, EditOp::Insert(_) | EditOp::Delete(_))),
            "reorder must not leave an unrelated add+remove: {:?}",
            script.ops
        );
    }

    #[test]
    fn nested_call_tree_relabel_is_found() {
        // A real (nested) call tree, not just a flat sequence.
        let base = CallNode::new(
            "entry",
            vec![
                CallNode::new("a", vec![CallNode::leaf("x")]),
                CallNode::leaf("b"),
            ],
        );
        let head = CallNode::new(
            "entry",
            vec![
                CallNode::new("a", vec![CallNode::leaf("y")]),
                CallNode::leaf("b"),
            ],
        );
        let script = call_tree_diff(&base, &head);
        assert_eq!(script.cost, 1);
        assert_eq!(
            script.ops,
            vec![EditOp::Relabel {
                from: "x".to_string(),
                to: "y".to_string(),
            }]
        );
    }

    #[test]
    fn call_tree_diff_is_deterministic() {
        let base = seq(&["a", "b", "c", "d"]);
        let head = seq(&["b", "a", "d", "e"]);
        let first = call_tree_diff(&base, &head);
        let second = call_tree_diff(&base, &head);
        assert_eq!(first, second);
    }
}
