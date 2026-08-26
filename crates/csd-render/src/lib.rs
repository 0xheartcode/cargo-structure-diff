//! Render the module view as one annotated Mermaid `flowchart`.
//!
//! Following the project convention (SPEC.md section 4) the delta is colour-encoded onto a single
//! drawing rather than diffing two images: green added, red-dashed removed, amber changed, gray
//! unchanged context. The graph is pruned to changed nodes (and the endpoints of changed edges)
//! plus a two-hop neighbourhood, so a large system never renders in full.
//!
//! Output is deterministic: nodes are emitted in id order with stable `n<i>` handles, edges in
//! `(from, to)` order.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use csd_ir::{Change, Graph, NodeKind, StableId};

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
    let nodes = collect_nodes(head, changes);
    let node_class = classify_nodes(&nodes, changes);
    let edges = collect_edges(head, changes, &nodes);

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

use std::fmt::Write as _;

/// Module nodes to consider: those in `head` plus any removed by the delta (absent from `head`).
fn collect_nodes<'a>(head: &'a Graph, changes: &'a [Change]) -> BTreeMap<&'a StableId, ()> {
    let mut nodes: BTreeMap<&StableId, ()> = BTreeMap::new();
    for n in &head.nodes {
        if n.kind == NodeKind::Module {
            nodes.insert(&n.id, ());
        }
    }
    for c in changes {
        if let Change::Removed(n) = c {
            if n.kind == NodeKind::Module {
                nodes.insert(&n.id, ());
            }
        }
    }
    nodes
}

/// Colour each module node from the delta. Unlisted nodes are context.
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

/// `Uses` edges to draw: head edges (added or context) plus edges the delta removed. Only edges
/// whose endpoints are both module nodes are kept.
fn collect_edges<'a>(
    head: &'a Graph,
    changes: &'a [Change],
    nodes: &BTreeMap<&'a StableId, ()>,
) -> Vec<(&'a StableId, &'a StableId, Class)> {
    let added: BTreeSet<(&StableId, &StableId)> = changes
        .iter()
        .filter_map(|c| match c {
            Change::EdgeAdded(e) if e.kind == csd_ir::EdgeKind::Uses => Some((&e.from, &e.to)),
            _ => None,
        })
        .collect();

    let mut edges: Vec<(&StableId, &StableId, Class)> = Vec::new();
    for e in &head.edges {
        if e.kind != csd_ir::EdgeKind::Uses {
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
            if e.kind == csd_ir::EdgeKind::Uses
                && nodes.contains_key(&e.from)
                && nodes.contains_key(&e.to)
            {
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
}
