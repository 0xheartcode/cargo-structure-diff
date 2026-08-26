//! Render a graph and its delta as one annotated Mermaid diagram.
//!
//! Two views live here: [`render_module_view`] (a `flowchart` over `Module` nodes and `Uses`
//! edges) and [`render_state_view`] (a `stateDiagram-v2` over `Variant` nodes and `Transitions`
//! edges, SPEC.md 3.4). Both follow the project convention (SPEC.md section 4): the delta is
//! colour-encoded onto a single drawing rather than diffing two images (green added, red-dashed
//! removed, amber changed, gray unchanged context) and the graph is pruned to changed nodes, the
//! endpoints of changed edges, and a two-hop neighbourhood, so a large system never renders in
//! full. The colouring ([`Class`]/[`CLASS_DEFS`]) and pruning ([`prune`]) are shared.
//!
//! Output is deterministic: nodes are emitted in id order with stable handles, edges in
//! `(from, to)` order.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use csd_ir::{Change, EdgeKind, Graph, NodeKind, StableId};

/// How far from a changed node a context node is still drawn.
const CONTEXT_HOPS: usize = 2;

/// The shared classDef block (SPEC.md section 4 rendering convention). `concat!` keeps the literal
/// four-space indentation; a `\`-continuation would strip it.
const CLASS_DEFS: &str = concat!(
    "    classDef added fill:#f0fdf4,stroke:#22c55e,stroke-width:2px\n",
    "    classDef removed fill:#fef2f2,stroke:#ef4444,stroke-width:2px,stroke-dasharray:4 4\n",
    "    classDef changed fill:#fffbeb,stroke:#f59e0b,stroke-width:2px\n",
    "    classDef context fill:#ffffff,stroke:#d1d5db,color:#9ca3af",
);

/// How a node or edge is coloured relative to the delta.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    Added,
    Removed,
    Changed,
    Context,
}

impl Class {
    fn css(self) -> &'static str {
        match self {
            Class::Added => "added",
            Class::Removed => "removed",
            Class::Changed => "changed",
            Class::Context => "context",
        }
    }
}

/// Render the module view of `head` with `changes` colour-encoded, as a Mermaid `flowchart LR`.
///
/// `changes` is the delta from `csd_diff::diff`. Removed nodes and edges are recovered from the
/// delta, since they are absent from `head`. Only `Module` nodes and `Uses` edges participate.
pub fn render_module_view(head: &Graph, changes: &[Change]) -> String {
    let nodes = collect_nodes(head, changes, NodeKind::Module);
    let node_class = classify_nodes(&nodes, changes);
    let edges = collect_edges(head, changes, &nodes, EdgeKind::Uses);

    let kept = prune(&nodes, &node_class, &edges);

    let mut out = String::from("flowchart LR\n");
    out.push_str(CLASS_DEFS);
    out.push('\n');

    if kept.is_empty() {
        out.push_str("    %% no structural changes in the module view\n");
        return out;
    }

    // Stable n<i> handles in id order.
    let handles: BTreeMap<&StableId, String> = kept
        .iter()
        .enumerate()
        .map(|(i, id)| (*id, format!("n{i}")))
        .collect();

    for id in &kept {
        let class = node_class.get(id).copied().unwrap_or(Class::Context);
        let _ = writeln!(
            out,
            "    {}[\"{}\"]:::{}",
            handles[id],
            id.as_str(),
            class.css()
        );
    }

    for (from, to, class) in &edges {
        if !handles.contains_key(from) || !handles.contains_key(to) {
            continue;
        }
        let (arrow, label) = match class {
            Class::Added => ("-->", "|+|"),
            Class::Removed => ("-.->", "|-|"),
            _ => ("-->", ""),
        };
        let _ = writeln!(
            out,
            "    {} {}{} {}",
            handles[from], arrow, label, handles[to]
        );
    }

    out
}

/// Render the state-machine view of `head` with `changes` colour-encoded, as a Mermaid
/// `stateDiagram-v2` (SPEC.md 3.4).
///
/// States are [`NodeKind::Variant`] nodes; transitions are [`EdgeKind::Transitions`] edges. Removed
/// states and transitions are recovered from the delta, since they are absent from `head`. Each
/// state is declared with its variant id as the label (`state "crate::Status::Active" as s0`) and
/// coloured via the `:::` operator, reusing [`CLASS_DEFS`]. Added transitions carry a `+` label and
/// removed a `-` label; `stateDiagram-v2` has no dashed transition arrow, so the removed colouring
/// shows on the state border (via `:::removed`) rather than the edge.
pub fn render_state_view(head: &Graph, changes: &[Change]) -> String {
    let nodes = collect_nodes(head, changes, NodeKind::Variant);
    let node_class = classify_nodes(&nodes, changes);
    let edges = collect_edges(head, changes, &nodes, EdgeKind::Transitions);

    let kept = prune(&nodes, &node_class, &edges);

    let mut out = String::from("stateDiagram-v2\n");
    out.push_str(CLASS_DEFS);
    out.push('\n');

    if kept.is_empty() {
        out.push_str("    %% no structural changes in the state view\n");
        return out;
    }

    // Stable s<i> handles in id order.
    let handles: BTreeMap<&StableId, String> = kept
        .iter()
        .enumerate()
        .map(|(i, id)| (*id, format!("s{i}")))
        .collect();

    // Declare each state, labelled with its variant id.
    for id in &kept {
        let _ = writeln!(out, "    state \"{}\" as {}", id.as_str(), handles[id]);
    }
    // Colour each state via the ::: operator.
    for id in &kept {
        let class = node_class.get(id).copied().unwrap_or(Class::Context);
        let _ = writeln!(out, "    {}:::{}", handles[id], class.css());
    }
    // Transitions in (from, to) order; added/removed carry a marker label.
    for (from, to, class) in &edges {
        if !handles.contains_key(from) || !handles.contains_key(to) {
            continue;
        }
        let label = match class {
            Class::Added => " : +",
            Class::Removed => " : -",
            _ => "",
        };
        let _ = writeln!(out, "    {} --> {}{}", handles[from], handles[to], label);
    }

    out
}

use std::fmt::Write as _;

/// Nodes of `kind` to consider: those in `head` plus any removed by the delta (absent from `head`).
fn collect_nodes<'a>(
    head: &'a Graph,
    changes: &'a [Change],
    kind: NodeKind,
) -> BTreeMap<&'a StableId, ()> {
    let mut nodes: BTreeMap<&StableId, ()> = BTreeMap::new();
    for n in &head.nodes {
        if n.kind == kind {
            nodes.insert(&n.id, ());
        }
    }
    for c in changes {
        if let Change::Removed(n) = c {
            if n.kind == kind {
                nodes.insert(&n.id, ());
            }
        }
    }
    nodes
}

/// Colour each collected node from the delta. Unlisted nodes are context.
fn classify_nodes<'a>(
    nodes: &BTreeMap<&'a StableId, ()>,
    changes: &'a [Change],
) -> BTreeMap<&'a StableId, Class> {
    let mut class: BTreeMap<&StableId, Class> = BTreeMap::new();
    for c in changes {
        match c {
            Change::Added(n) if nodes.contains_key(&n.id) => {
                class.insert(&n.id, Class::Added);
            }
            Change::Removed(n) if nodes.contains_key(&n.id) => {
                class.insert(&n.id, Class::Removed);
            }
            Change::Modified { after, .. } if nodes.contains_key(&after.id) => {
                class.insert(&after.id, Class::Changed);
            }
            Change::Moved { node, .. } if nodes.contains_key(node) => {
                class.insert(node, Class::Changed);
            }
            _ => {}
        }
    }
    class
}

/// Edges of `kind` to draw: head edges (added or context) plus edges the delta removed. Only edges
/// whose endpoints are both kept nodes are considered.
fn collect_edges<'a>(
    head: &'a Graph,
    changes: &'a [Change],
    nodes: &BTreeMap<&'a StableId, ()>,
    kind: EdgeKind,
) -> Vec<(&'a StableId, &'a StableId, Class)> {
    let added: BTreeSet<(&StableId, &StableId)> = changes
        .iter()
        .filter_map(|c| match c {
            Change::EdgeAdded(e) if e.kind == kind => Some((&e.from, &e.to)),
            _ => None,
        })
        .collect();

    let mut edges: Vec<(&StableId, &StableId, Class)> = Vec::new();
    for e in &head.edges {
        if e.kind != kind {
            continue;
        }
        if !nodes.contains_key(&e.from) || !nodes.contains_key(&e.to) {
            continue;
        }
        let class = if added.contains(&(&e.from, &e.to)) {
            Class::Added
        } else {
            Class::Context
        };
        edges.push((&e.from, &e.to, class));
    }
    for c in changes {
        if let Change::EdgeRemoved(e) = c {
            if e.kind == kind && nodes.contains_key(&e.from) && nodes.contains_key(&e.to) {
                edges.push((&e.from, &e.to, Class::Removed));
            }
        }
    }
    edges.sort_by(|a, b| (a.0, a.1).cmp(&(b.0, b.1)));
    edges
}

/// Keep changed nodes, the endpoints of changed edges, and everything within two hops of them.
fn prune<'a>(
    nodes: &BTreeMap<&'a StableId, ()>,
    class: &BTreeMap<&'a StableId, Class>,
    edges: &[(&'a StableId, &'a StableId, Class)],
) -> Vec<&'a StableId> {
    // Seed: changed nodes plus endpoints of added/removed edges.
    let mut seed: BTreeSet<&StableId> = class.keys().copied().collect();
    for (from, to, cls) in edges {
        if *cls != Class::Context {
            seed.insert(from);
            seed.insert(to);
        }
    }
    if seed.is_empty() {
        return Vec::new();
    }

    // Undirected adjacency over the module edges.
    let mut adj: BTreeMap<&StableId, Vec<&StableId>> = BTreeMap::new();
    for (from, to, _) in edges {
        adj.entry(from).or_default().push(to);
        adj.entry(to).or_default().push(from);
    }

    let mut kept: BTreeSet<&StableId> = BTreeSet::new();
    let mut queue: VecDeque<(&StableId, usize)> = seed.iter().map(|id| (*id, 0usize)).collect();
    while let Some((id, depth)) = queue.pop_front() {
        if !kept.insert(id) {
            continue;
        }
        if depth == CONTEXT_HOPS {
            continue;
        }
        if let Some(neighbours) = adj.get(id) {
            for n in neighbours {
                if !kept.contains(n) {
                    queue.push_back((n, depth + 1));
                }
            }
        }
    }

    // Only render nodes we actually know about.
    kept.into_iter()
        .filter(|id| nodes.contains_key(id))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use csd_ir::{Edge, EdgeKind, Node, SourceSpan};

    fn span() -> SourceSpan {
        SourceSpan {
            file: "src/lib.rs".into(),
            start: 0,
            end: 1,
        }
    }

    fn module(id: &str) -> Node {
        Node {
            id: StableId::new(id),
            kind: NodeKind::Module,
            span: span(),
            attrs: BTreeMap::new(),
            fingerprint: None,
        }
    }

    fn uses(from: &str, to: &str) -> Edge {
        Edge {
            from: StableId::new(from),
            to: StableId::new(to),
            kind: EdgeKind::Uses,
            span: span(),
            ordinal: None,
        }
    }

    fn variant(id: &str) -> Node {
        Node {
            id: StableId::new(id),
            kind: NodeKind::Variant,
            span: span(),
            attrs: BTreeMap::new(),
            fingerprint: None,
        }
    }

    fn transition(from: &str, to: &str) -> Edge {
        Edge {
            from: StableId::new(from),
            to: StableId::new(to),
            kind: EdgeKind::Transitions,
            span: span(),
            ordinal: None,
        }
    }

    #[test]
    fn empty_delta_renders_no_changes() {
        let head = Graph {
            nodes: vec![module("crate"), module("crate::a")],
            edges: vec![],
        };
        let out = render_module_view(&head, &[]);
        assert!(out.starts_with("flowchart LR\n"));
        assert!(out.contains("classDef added"));
        assert!(out.contains("%% no structural changes in the module view"));
        // Nothing was changed, so no node lines are drawn.
        assert!(!out.contains(":::"));
    }

    #[test]
    fn added_edge_pulls_in_its_endpoints_as_context() {
        // A newly added Uses edge (the layering-violation shape) with no node changes.
        let head = Graph {
            nodes: vec![module("crate::api"), module("crate::infra")],
            edges: vec![uses("crate::api", "crate::infra")],
        };
        let changes = vec![Change::EdgeAdded(uses("crate::api", "crate::infra"))];
        let out = render_module_view(&head, &changes);
        let expected = concat!(
            "flowchart LR\n",
            "    classDef added fill:#f0fdf4,stroke:#22c55e,stroke-width:2px\n",
            "    classDef removed fill:#fef2f2,stroke:#ef4444,stroke-width:2px,stroke-dasharray:4 4\n",
            "    classDef changed fill:#fffbeb,stroke:#f59e0b,stroke-width:2px\n",
            "    classDef context fill:#ffffff,stroke:#d1d5db,color:#9ca3af\n",
            "    n0[\"crate::api\"]:::context\n",
            "    n1[\"crate::infra\"]:::context\n",
            "    n0 -->|+| n1\n",
        );
        assert_eq!(out, expected);
    }

    #[test]
    fn changed_node_and_two_hop_context() {
        // b modified; a and c are within two hops via a->b->c, so both are drawn as context.
        let head = Graph {
            nodes: vec![module("crate::a"), module("crate::b"), module("crate::c")],
            edges: vec![uses("crate::a", "crate::b"), uses("crate::b", "crate::c")],
        };
        let changes = vec![Change::Modified {
            before: module("crate::b"),
            after: module("crate::b"),
        }];
        let out = render_module_view(&head, &changes);
        assert!(out.contains("[\"crate::b\"]:::changed"));
        assert!(out.contains("[\"crate::a\"]:::context"));
        assert!(out.contains("[\"crate::c\"]:::context"));
    }

    #[test]
    fn far_context_is_pruned() {
        // d is three hops from the changed node a, so it is dropped.
        let head = Graph {
            nodes: vec![
                module("crate::a"),
                module("crate::b"),
                module("crate::c"),
                module("crate::d"),
            ],
            edges: vec![
                uses("crate::a", "crate::b"),
                uses("crate::b", "crate::c"),
                uses("crate::c", "crate::d"),
            ],
        };
        let changes = vec![Change::Added(module("crate::a"))];
        let out = render_module_view(&head, &changes);
        assert!(out.contains("[\"crate::a\"]:::added"));
        assert!(out.contains("crate::b"));
        assert!(out.contains("crate::c"));
        assert!(
            !out.contains("crate::d"),
            "3-hop node must be pruned:\n{out}"
        );
    }

    #[test]
    fn state_empty_delta_renders_no_changes() {
        let head = Graph {
            nodes: vec![
                variant("crate::Status::Active"),
                variant("crate::Status::Closed"),
            ],
            edges: vec![],
        };
        let out = render_state_view(&head, &[]);
        assert!(out.starts_with("stateDiagram-v2\n"));
        assert!(out.contains("classDef added"));
        assert!(out.contains("%% no structural changes in the state view"));
        // Nothing was changed, so no state lines are drawn.
        assert!(!out.contains(":::"));
        assert!(!out.contains("state \""));
    }

    #[test]
    fn state_added_transition_golden() {
        // A newly added transition Active -> Closed; both states already exist in head.
        let head = Graph {
            nodes: vec![
                variant("crate::Status::Active"),
                variant("crate::Status::Closed"),
            ],
            edges: vec![transition("crate::Status::Active", "crate::Status::Closed")],
        };
        let changes = vec![Change::EdgeAdded(transition(
            "crate::Status::Active",
            "crate::Status::Closed",
        ))];
        let out = render_state_view(&head, &changes);
        let expected = concat!(
            "stateDiagram-v2\n",
            "    classDef added fill:#f0fdf4,stroke:#22c55e,stroke-width:2px\n",
            "    classDef removed fill:#fef2f2,stroke:#ef4444,stroke-width:2px,stroke-dasharray:4 4\n",
            "    classDef changed fill:#fffbeb,stroke:#f59e0b,stroke-width:2px\n",
            "    classDef context fill:#ffffff,stroke:#d1d5db,color:#9ca3af\n",
            "    state \"crate::Status::Active\" as s0\n",
            "    state \"crate::Status::Closed\" as s1\n",
            "    s0:::context\n",
            "    s1:::context\n",
            "    s0 --> s1 : +\n",
        );
        assert_eq!(out, expected);
    }

    #[test]
    fn state_removed_transition_recovered_from_delta() {
        // The removed transition is absent from head; it is recovered from the delta.
        let head = Graph {
            nodes: vec![
                variant("crate::Status::Active"),
                variant("crate::Status::Closed"),
            ],
            edges: vec![],
        };
        let changes = vec![Change::EdgeRemoved(transition(
            "crate::Status::Active",
            "crate::Status::Closed",
        ))];
        let out = render_state_view(&head, &changes);
        assert!(out.contains("state \"crate::Status::Active\" as s0"));
        assert!(out.contains("state \"crate::Status::Closed\" as s1"));
        // Removed transitions carry a `-` marker (stateDiagram-v2 has no dashed arrow).
        assert!(
            out.contains("    s0 --> s1 : -"),
            "removed marker missing:\n{out}"
        );
    }

    #[test]
    fn state_far_context_is_pruned() {
        // d is three hops from the changed state a, so it is dropped.
        let head = Graph {
            nodes: vec![
                variant("crate::S::a"),
                variant("crate::S::b"),
                variant("crate::S::c"),
                variant("crate::S::d"),
            ],
            edges: vec![
                transition("crate::S::a", "crate::S::b"),
                transition("crate::S::b", "crate::S::c"),
                transition("crate::S::c", "crate::S::d"),
            ],
        };
        let changes = vec![Change::Added(variant("crate::S::a"))];
        let out = render_state_view(&head, &changes);
        assert!(out.contains("state \"crate::S::a\" as s0"));
        assert!(out.contains(":::added"));
        assert!(out.contains("crate::S::b"));
        assert!(out.contains("crate::S::c"));
        assert!(
            !out.contains("crate::S::d"),
            "3-hop state must be pruned:\n{out}"
        );
    }
}
