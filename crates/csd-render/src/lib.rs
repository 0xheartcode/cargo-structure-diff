//! Render a graph and its delta as one annotated Mermaid diagram.
//!
//! Four views live here: [`render_module_view`] (a `flowchart` over `Module` nodes and `Uses`
//! edges), [`render_state_view`] (a `stateDiagram-v2` over `Variant` nodes and `Transitions`
//! edges, SPEC.md 3.4), [`render_type_view`] (a `classDiagram` over `Struct`/`Enum`/`Trait`
//! nodes with `Implements`/`Associates` relations, SPEC.md 3.2) and [`render_call_view`] (a
//! `sequenceDiagram` slice from an entry `Fn` over `Calls` edges, SPEC.md 3.3). All follow the
//! project convention
//! (SPEC.md section 4): the delta is
//! colour-encoded onto a single drawing rather than diffing two images (green added, red-dashed
//! removed, amber changed, gray unchanged context) and the graph is pruned to changed nodes, the
//! endpoints of changed edges, and a two-hop neighbourhood, so a large system never renders in
//! full. The colouring ([`Class`]/[`CLASS_DEFS`]) and pruning ([`prune`]) are shared.
//!
//! Output is deterministic: nodes are emitted in id order with stable handles, edges in
//! `(from, to)` order.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use csd_ir::{Change, EdgeKind, Fingerprint, Graph, NodeKind, StableId};

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

/// Render the type view of `head` with `changes` colour-encoded, as a Mermaid `classDiagram`
/// (SPEC.md 3.2).
///
/// Classes are [`NodeKind::Struct`], [`NodeKind::Enum`] and [`NodeKind::Trait`] nodes (traits carry
/// a `<<trait>>` stereotype). Relations: [`EdgeKind::Implements`] renders as a realization
/// `Type ..|> Trait` and [`EdgeKind::Associates`] as an association `Type --> Other`. Classes are
/// coloured via the `:::` operator, reusing [`CLASS_DEFS`].
///
/// `classDiagram` cannot colour individual members, so per-member delta is carried as a text prefix
/// on member lines (`+` added, `-` removed, `~` type changed, plain unchanged): see [`member_lines`].
/// DOT HTML-like labels are the future path for true per-member background colour.
///
/// Pruning is one-hop (SPEC.md 3.2): changed classes plus the classes one relation away, tighter
/// than the two-hop module/state views because a type graph fans out fast.
pub fn render_type_view(head: &Graph, changes: &[Change]) -> String {
    // Struct, Enum and Trait nodes all render as classes.
    let mut nodes = collect_nodes(head, changes, NodeKind::Struct);
    nodes.extend(collect_nodes(head, changes, NodeKind::Enum));
    nodes.extend(collect_nodes(head, changes, NodeKind::Trait));

    let node_class = classify_nodes(&nodes, changes);

    // Implements (realization) and Associates (association) relations, tagged with their kind.
    let mut relations: Vec<(&StableId, &StableId, EdgeKind, Class)> = Vec::new();
    for (from, to, class) in collect_edges(head, changes, &nodes, EdgeKind::Implements) {
        relations.push((from, to, EdgeKind::Implements, class));
    }
    for (from, to, class) in collect_edges(head, changes, &nodes, EdgeKind::Associates) {
        relations.push((from, to, EdgeKind::Associates, class));
    }
    relations.sort_by(|a, b| (a.0, a.1, a.2).cmp(&(b.0, b.1, b.2)));

    // One-hop prune: changed classes plus the classes one relation away.
    let prune_edges: Vec<(&StableId, &StableId, Class)> =
        relations.iter().map(|(f, t, _, c)| (*f, *t, *c)).collect();
    let kept = prune_within(&nodes, &node_class, &prune_edges, 1);

    let mut out = String::from("classDiagram\n");
    out.push_str(CLASS_DEFS);
    out.push('\n');

    if kept.is_empty() {
        out.push_str("    %% no structural changes in the type view\n");
        return out;
    }

    // Stable c<i> handles in id order (class ids carry `::`/`<>`, so a raw id is not a valid name).
    let handles: BTreeMap<&StableId, String> = kept
        .iter()
        .enumerate()
        .map(|(i, id)| (*id, format!("c{i}")))
        .collect();

    // Node kind (for the trait stereotype) and the before/after fingerprints of Modified classes.
    let kind_of: BTreeMap<&StableId, NodeKind> =
        head.nodes.iter().map(|n| (&n.id, n.kind)).collect();
    let modified: BTreeMap<&StableId, (&Fingerprint, &Fingerprint)> = changes
        .iter()
        .filter_map(|c| match c {
            Change::Modified { before, after } => {
                match (before.fingerprint.as_ref(), after.fingerprint.as_ref()) {
                    (Some(b), Some(a)) => Some((&after.id, (b, a))),
                    _ => None,
                }
            }
            _ => None,
        })
        .collect();

    // Declare each class, with a `<<trait>>` stereotype and (for Modified classes) member lines.
    for id in &kept {
        let handle = &handles[id];
        let is_trait = kind_of.get(id).copied() == Some(NodeKind::Trait);
        let members = modified.get(id).map(|(b, a)| member_lines(b, a));
        let has_body = is_trait || members.as_ref().is_some_and(|m| !m.is_empty());
        if has_body {
            let _ = writeln!(out, "    class {}[\"{}\"] {{", handle, id.as_str());
            if is_trait {
                out.push_str("        <<trait>>\n");
            }
            if let Some(lines) = &members {
                for line in lines {
                    let _ = writeln!(out, "        {line}");
                }
            }
            out.push_str("    }\n");
        } else {
            let _ = writeln!(out, "    class {}[\"{}\"]", handle, id.as_str());
        }
    }

    // Colour each class via the ::: operator.
    for id in &kept {
        let class = node_class.get(id).copied().unwrap_or(Class::Context);
        let _ = writeln!(out, "    class {}:::{}", handles[id], class.css());
    }

    // Relations in (from, to, kind) order; endpoints pruned away are skipped.
    for (from, to, kind, _class) in &relations {
        if !handles.contains_key(from) || !handles.contains_key(to) {
            continue;
        }
        let arrow = match kind {
            EdgeKind::Implements => "..|>",
            _ => "-->",
        };
        let _ = writeln!(out, "    {} {} {}", handles[from], arrow, handles[to]);
    }

    out
}

/// How deep the call slice recurses before it stops, so a deep or recursive graph never blows the
/// stack or the diagram. Documented so the ceiling is honest, not silent.
const MAX_CALL_DEPTH: usize = 32;

/// Added-region tint (SPEC.md section 4: green added). `sequenceDiagram` colours a region with a
/// `rect rgb(...)` block, not `classDef`, so the value is inlined here. Matches the `added` classDef
/// fill `#f0fdf4`.
const RECT_ADDED: &str = "240,253,244";
/// Removed-region tint (SPEC.md section 4: red removed). Matches the `removed` classDef fill
/// `#fef2f2`.
const RECT_REMOVED: &str = "254,242,242";

/// Render a call-graph slice of `head` from `entry`, with `changes` colour-encoded, as a Mermaid
/// `sequenceDiagram` (SPEC.md 3.3).
///
/// The diagram is a slice, not the whole graph: a depth-first pre-order walk from the `entry` `Fn`
/// node following [`EdgeKind::Calls`] edges in `ordinal` (source) order, so messages read top to
/// bottom in call order. The walk carries an ancestor stack and skips any edge back to a node
/// already on it, so a cycle (`a` calls `b` calls `a`) terminates instead of recursing forever;
/// depth is also bounded by [`MAX_CALL_DEPTH`].
///
/// Participants are the type/module owning each fn, derived from the fn id by stripping the last
/// `::segment` (`crate::Widget::run` -> `crate::Widget`; `crate::helper` -> `crate`). Ids are not
/// valid Mermaid names, so each owner is declared once, in first-seen order, with a stable `p<i>`
/// alias (`participant p0 as crate::Widget`). A message is `caller ->> callee : callee_fn_name`.
///
/// Delta colouring is taken straight from the delta's edge changes: a message whose `Calls` edge
/// `(from, to, ordinal)` is a [`Change::EdgeAdded`] is wrapped in a green `rect rgb(...)`; a
/// [`Change::EdgeRemoved`] `Calls` edge is shown as a message wrapped in a red `rect rgb(...)` with
/// a ` (removed)` suffix. The precise reorder-as-move analysis lives in `csd_diff::call_tree_diff`;
/// wiring it in is a later refinement, and this renderer deliberately takes no dependency on the
/// differ, colouring only from `EdgeAdded`/`EdgeRemoved`.
///
/// Fidelity ceiling (SPEC.md 3.3): a fn with `attrs["unresolved_calls"] = N` (N > 0) carries dynamic
/// or generic callees with no statically known target. These are never fabricated as resolved
/// calls; instead a `note over <participant> : N dynamic call(s) not shown` is emitted after that
/// fn's messages.
///
/// If `entry` is not a `Fn` node, or has no outgoing calls, the output is `sequenceDiagram` plus a
/// `%% no calls from <entry>` comment.
pub fn render_call_view(head: &Graph, changes: &[Change], entry: &StableId) -> String {
    // Edges the delta added / removed, keyed by (from, to, ordinal) for exact-message matching.
    let added: BTreeSet<(&str, &str, Option<u32>)> = changes
        .iter()
        .filter_map(|c| match c {
            Change::EdgeAdded(e) if e.kind == EdgeKind::Calls => {
                Some((e.from.as_str(), e.to.as_str(), e.ordinal))
            }
            _ => None,
        })
        .collect();

    // Outgoing Calls per caller: head edges (added or plain) plus edges the delta removed. Keyed by
    // id string so `entry` and cross-graph edge endpoints compare by value.
    let mut outgoing: BTreeMap<&str, Vec<Out>> = BTreeMap::new();
    for e in &head.edges {
        if e.kind != EdgeKind::Calls {
            continue;
        }
        let class = if added.contains(&(e.from.as_str(), e.to.as_str(), e.ordinal)) {
            Class::Added
        } else {
            Class::Context
        };
        outgoing.entry(e.from.as_str()).or_default().push(Out {
            to: e.to.as_str(),
            ordinal: e.ordinal,
            class,
        });
    }
    for c in changes {
        if let Change::EdgeRemoved(e) = c {
            if e.kind == EdgeKind::Calls {
                outgoing.entry(e.from.as_str()).or_default().push(Out {
                    to: e.to.as_str(),
                    ordinal: e.ordinal,
                    class: Class::Removed,
                });
            }
        }
    }
    // Deterministic message order at each level: by ordinal, then callee id.
    for outs in outgoing.values_mut() {
        outs.sort_by(|a, b| (a.ordinal, a.to).cmp(&(b.ordinal, b.to)));
    }

    // `unresolved_calls` counts per fn (the dyn/generics fidelity ceiling).
    let unresolved: BTreeMap<&str, u32> = head
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Fn)
        .filter_map(|n| {
            n.attrs
                .get("unresolved_calls")
                .and_then(|v| v.parse::<u32>().ok())
                .filter(|n| *n > 0)
                .map(|count| (n.id.as_str(), count))
        })
        .collect();

    let is_fn = head
        .nodes
        .iter()
        .any(|n| n.kind == NodeKind::Fn && &n.id == entry);
    let has_calls = outgoing
        .get(entry.as_str())
        .is_some_and(|outs| !outs.is_empty());

    if !is_fn || !has_calls {
        let mut out = String::from("sequenceDiagram\n");
        let _ = writeln!(out, "    %% no calls from {}", entry.as_str());
        return out;
    }

    // Walk, building message lines and the first-seen participant order together.
    let mut parts = Parts { order: Vec::new() };
    let mut lines = String::new();
    let mut stack: Vec<&str> = Vec::new();
    walk_calls(
        entry.as_str(),
        0,
        &mut stack,
        &outgoing,
        &unresolved,
        &mut parts,
        &mut lines,
    );

    let mut out = String::from("sequenceDiagram\n");
    for (i, owner) in parts.order.iter().enumerate() {
        let _ = writeln!(out, "    participant p{i} as {owner}");
    }
    out.push_str(&lines);
    out
}

/// One outgoing call from a fn, tagged with its delta colour.
struct Out<'a> {
    to: &'a str,
    ordinal: Option<u32>,
    class: Class,
}

/// First-seen participant registry mapping an owner id to a stable `p<i>` alias.
struct Parts<'a> {
    order: Vec<&'a str>,
}

impl<'a> Parts<'a> {
    /// The `p<i>` alias for `owner`, assigning a new index on first sight.
    fn alias(&mut self, owner: &'a str) -> String {
        let idx = match self.order.iter().position(|o| *o == owner) {
            Some(i) => i,
            None => {
                self.order.push(owner);
                self.order.len() - 1
            }
        };
        format!("p{idx}")
    }
}

/// The owner (participant) of a fn id: everything before the last `::segment`, or the whole id.
fn call_owner(id: &str) -> &str {
    match id.rfind("::") {
        Some(i) => &id[..i],
        None => id,
    }
}

/// The fn's own name: the last `::segment`, or the whole id.
fn call_name(id: &str) -> &str {
    match id.rfind("::") {
        Some(i) => &id[i + 2..],
        None => id,
    }
}

/// Depth-first pre-order emit of one fn's messages, then its dyn note, recursing into callees.
///
/// `stack` holds the ancestors on the current path; an edge back to any of them is skipped so cycles
/// terminate. Recursion also stops at [`MAX_CALL_DEPTH`].
#[allow(clippy::too_many_arguments)]
fn walk_calls<'a>(
    node: &'a str,
    depth: usize,
    stack: &mut Vec<&'a str>,
    outgoing: &BTreeMap<&'a str, Vec<Out<'a>>>,
    unresolved: &BTreeMap<&'a str, u32>,
    parts: &mut Parts<'a>,
    lines: &mut String,
) {
    stack.push(node);
    if let Some(outs) = outgoing.get(node) {
        for out in outs {
            let caller = parts.alias(call_owner(node));
            let callee = parts.alias(call_owner(out.to));
            let name = call_name(out.to);
            match out.class {
                // Added region: green rect (SPEC.md section 4 convention).
                Class::Added => {
                    let _ = writeln!(lines, "    rect rgb({RECT_ADDED})");
                    let _ = writeln!(lines, "        {caller} ->> {callee} : {name}");
                    lines.push_str("    end\n");
                }
                // Removed region: red rect with a `(removed)` suffix (SPEC.md section 4 convention).
                Class::Removed => {
                    let _ = writeln!(lines, "    rect rgb({RECT_REMOVED})");
                    let _ = writeln!(lines, "        {caller} ->> {callee} : {name} (removed)");
                    lines.push_str("    end\n");
                }
                _ => {
                    let _ = writeln!(lines, "    {caller} ->> {callee} : {name}");
                }
            }
            // Recurse into the callee unless it is an ancestor (cycle) or we hit the depth ceiling.
            if depth + 1 < MAX_CALL_DEPTH
                && !stack.contains(&out.to)
                && outgoing.contains_key(out.to)
            {
                walk_calls(out.to, depth + 1, stack, outgoing, unresolved, parts, lines);
            }
        }
    }
    // Honest dyn ceiling: note the unresolved callees after this fn's messages, never as a call.
    if let Some(&count) = unresolved.get(node) {
        let p = parts.alias(call_owner(node));
        let _ = writeln!(
            lines,
            "    note over {p} : {count} dynamic call(s) not shown"
        );
    }
    stack.pop();
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

/// Keep changed nodes, the endpoints of changed edges, and everything within [`CONTEXT_HOPS`] of
/// them.
fn prune<'a>(
    nodes: &BTreeMap<&'a StableId, ()>,
    class: &BTreeMap<&'a StableId, Class>,
    edges: &[(&'a StableId, &'a StableId, Class)],
) -> Vec<&'a StableId> {
    prune_within(nodes, class, edges, CONTEXT_HOPS)
}

/// Like [`prune`] but with a caller-chosen neighbourhood radius. The type view passes `hops = 1`.
fn prune_within<'a>(
    nodes: &BTreeMap<&'a StableId, ()>,
    class: &BTreeMap<&'a StableId, Class>,
    edges: &[(&'a StableId, &'a StableId, Class)],
    hops: usize,
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

    // Undirected adjacency over the edges.
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
        if depth == hops {
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

/// The member name of a fingerprint member entry: the text before the first `:` (a `name: Type`
/// signature), or the whole entry for a bare name.
fn member_name(entry: &str) -> &str {
    entry.split(':').next().unwrap_or(entry).trim()
}

/// Per-member delta lines for a Modified class, as Mermaid member lines with a text prefix.
///
/// `classDiagram` cannot colour individual members (SPEC.md 3.2), so the delta is carried inline:
/// `+name` added, `-name` removed, `~name` type changed, plain `name` unchanged. Members are keyed
/// by [`member_name`] and emitted in sorted order for determinism. A persisting member is `~` when
/// its signature (name plus type) differs; for bare untyped members, which carry no per-member
/// type, the aggregate `field_types` multiset is the best-effort change signal.
fn member_lines(before: &Fingerprint, after: &Fingerprint) -> Vec<String> {
    let index = |set: &BTreeSet<String>| -> BTreeMap<String, String> {
        set.iter()
            .map(|e| (member_name(e).to_string(), e.clone()))
            .collect()
    };
    let b = index(&before.members);
    let a = index(&after.members);
    let field_types_changed = before.field_types != after.field_types;

    let mut names: BTreeSet<&String> = BTreeSet::new();
    names.extend(b.keys());
    names.extend(a.keys());

    let mut lines = Vec::new();
    for name in names {
        let line = match (b.get(name), a.get(name)) {
            (None, Some(_)) => format!("+{name}"),
            (Some(_), None) => format!("-{name}"),
            (Some(be), Some(ae)) => {
                let typed = be.contains(':') || ae.contains(':');
                let changed = if typed { be != ae } else { field_types_changed };
                if changed {
                    format!("~{name}")
                } else {
                    name.clone()
                }
            }
            // `names` is the union of the two key sets, so at least one side is present.
            (None, None) => continue,
        };
        lines.push(line);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use csd_ir::{Edge, EdgeKind, Fingerprint, Node, SourceSpan};

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

    fn type_node(id: &str, kind: NodeKind, fp: Option<Fingerprint>) -> Node {
        Node {
            id: StableId::new(id),
            kind,
            span: span(),
            attrs: BTreeMap::new(),
            fingerprint: fp,
        }
    }

    fn fingerprint(members: &[&str], fields: &[(&str, u32)]) -> Fingerprint {
        Fingerprint {
            members: members.iter().map(|s| s.to_string()).collect(),
            field_types: fields.iter().map(|(t, c)| (t.to_string(), *c)).collect(),
            neighbors: BTreeSet::new(),
            doc_hash: 0,
        }
    }

    fn relation(from: &str, to: &str, kind: EdgeKind) -> Edge {
        Edge {
            from: StableId::new(from),
            to: StableId::new(to),
            kind,
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

    #[test]
    fn type_empty_delta_renders_no_changes() {
        let head = Graph {
            nodes: vec![type_node("crate::Order", NodeKind::Struct, None)],
            edges: vec![],
        };
        let out = render_type_view(&head, &[]);
        assert!(out.starts_with("classDiagram\n"));
        assert!(out.contains("classDef added"));
        assert!(out.contains("%% no structural changes in the type view"));
        // Nothing was changed, so no class lines are drawn.
        assert!(!out.contains(":::"));
        assert!(!out.contains("class c"));
    }

    #[test]
    fn type_delta_golden() {
        // Order (struct) gains an `Implements` realization of the Billable trait, and is Modified:
        // a member added (`customer`), one unchanged (`id`), one type-changed (`total`).
        let before = fingerprint(&["id: u64", "total: Money"], &[("u64", 1), ("Money", 1)]);
        let after = fingerprint(
            &["customer: CustomerId", "id: u64", "total: Cents"],
            &[("u64", 1), ("Cents", 1), ("CustomerId", 1)],
        );
        let head = Graph {
            nodes: vec![
                type_node("crate::Order", NodeKind::Struct, Some(after.clone())),
                type_node("crate::Billable", NodeKind::Trait, None),
            ],
            edges: vec![relation(
                "crate::Order",
                "crate::Billable",
                EdgeKind::Implements,
            )],
        };
        let changes = vec![
            Change::Modified {
                before: type_node("crate::Order", NodeKind::Struct, Some(before)),
                after: type_node("crate::Order", NodeKind::Struct, Some(after)),
            },
            Change::EdgeAdded(relation(
                "crate::Order",
                "crate::Billable",
                EdgeKind::Implements,
            )),
        ];
        let out = render_type_view(&head, &changes);
        // Ids sort Billable < Order, so c0 is the trait and c1 the struct.
        let expected = concat!(
            "classDiagram\n",
            "    classDef added fill:#f0fdf4,stroke:#22c55e,stroke-width:2px\n",
            "    classDef removed fill:#fef2f2,stroke:#ef4444,stroke-width:2px,stroke-dasharray:4 4\n",
            "    classDef changed fill:#fffbeb,stroke:#f59e0b,stroke-width:2px\n",
            "    classDef context fill:#ffffff,stroke:#d1d5db,color:#9ca3af\n",
            "    class c0[\"crate::Billable\"] {\n",
            "        <<trait>>\n",
            "    }\n",
            "    class c1[\"crate::Order\"] {\n",
            "        +customer\n",
            "        id\n",
            "        ~total\n",
            "    }\n",
            "    class c0:::context\n",
            "    class c1:::changed\n",
            "    c1 ..|> c0\n",
        );
        assert_eq!(out, expected);
    }

    #[test]
    fn type_far_context_is_pruned() {
        // A modified; A --> B --> C via Associates. One-hop pruning keeps B, drops C.
        let head = Graph {
            nodes: vec![
                type_node("crate::A", NodeKind::Struct, None),
                type_node("crate::B", NodeKind::Struct, None),
                type_node("crate::C", NodeKind::Struct, None),
            ],
            edges: vec![
                relation("crate::A", "crate::B", EdgeKind::Associates),
                relation("crate::B", "crate::C", EdgeKind::Associates),
            ],
        };
        let changes = vec![Change::Modified {
            before: type_node("crate::A", NodeKind::Struct, None),
            after: type_node("crate::A", NodeKind::Struct, None),
        }];
        let out = render_type_view(&head, &changes);
        assert!(out.contains("[\"crate::A\"]"));
        assert!(out.contains("class c0:::changed") || out.contains(":::changed"));
        assert!(
            out.contains("crate::B"),
            "1-hop context must be kept:\n{out}"
        );
        assert!(
            !out.contains("crate::C"),
            "2-hop class must be pruned by one-hop rule:\n{out}"
        );
    }

    #[test]
    fn member_lines_bare_names_use_field_types_signal() {
        // Untyped members: names unchanged but the field-type multiset changed -> `~`.
        let before = fingerprint(&["amount"], &[("Money", 1)]);
        let after = fingerprint(&["amount"], &[("Cents", 1)]);
        assert_eq!(member_lines(&before, &after), vec!["~amount".to_string()]);
        // Field types unchanged -> plain.
        let same = fingerprint(&["amount"], &[("Money", 1)]);
        assert_eq!(member_lines(&before, &same), vec!["amount".to_string()]);
    }

    fn fn_node(id: &str) -> Node {
        Node {
            id: StableId::new(id),
            kind: NodeKind::Fn,
            span: span(),
            attrs: BTreeMap::new(),
            fingerprint: None,
        }
    }

    fn fn_node_dyn(id: &str, unresolved: u32) -> Node {
        let mut node = fn_node(id);
        node.attrs
            .insert("unresolved_calls".into(), unresolved.to_string());
        node
    }

    fn calls(from: &str, to: &str, ordinal: u32) -> Edge {
        Edge {
            from: StableId::new(from),
            to: StableId::new(to),
            kind: EdgeKind::Calls,
            span: span(),
            ordinal: Some(ordinal),
        }
    }

    #[test]
    fn call_entry_not_found_renders_no_calls() {
        let head = Graph {
            nodes: vec![fn_node("crate::App::run")],
            edges: vec![],
        };
        let out = render_call_view(&head, &[], &StableId::new("crate::orphan"));
        assert_eq!(out, "sequenceDiagram\n    %% no calls from crate::orphan\n");
    }

    #[test]
    fn call_entry_without_calls_renders_no_calls() {
        // A real Fn node, but with no outgoing Calls edges.
        let head = Graph {
            nodes: vec![fn_node("crate::App::run")],
            edges: vec![],
        };
        let out = render_call_view(&head, &[], &StableId::new("crate::App::run"));
        assert_eq!(
            out,
            "sequenceDiagram\n    %% no calls from crate::App::run\n"
        );
    }

    #[test]
    fn call_added_message_golden() {
        // run calls init (unchanged) then helper (newly added). ordinal order drives the sequence.
        let head = Graph {
            nodes: vec![
                fn_node("crate::Widget::run"),
                fn_node("crate::Widget::init"),
                fn_node("crate::helper"),
            ],
            edges: vec![
                calls("crate::Widget::run", "crate::Widget::init", 0),
                calls("crate::Widget::run", "crate::helper", 1),
            ],
        };
        let changes = vec![Change::EdgeAdded(calls(
            "crate::Widget::run",
            "crate::helper",
            1,
        ))];
        let out = render_call_view(&head, &changes, &StableId::new("crate::Widget::run"));
        let expected = concat!(
            "sequenceDiagram\n",
            "    participant p0 as crate::Widget\n",
            "    participant p1 as crate\n",
            "    p0 ->> p0 : init\n",
            "    rect rgb(240,253,244)\n",
            "        p0 ->> p1 : helper\n",
            "    end\n",
        );
        assert_eq!(out, expected);
    }

    #[test]
    fn call_removed_message_wrapped_in_red_rect() {
        // The removed call is absent from head; it is recovered from the delta.
        let head = Graph {
            nodes: vec![fn_node("crate::A::f"), fn_node("crate::A::g")],
            edges: vec![],
        };
        let changes = vec![Change::EdgeRemoved(calls("crate::A::f", "crate::A::g", 0))];
        let out = render_call_view(&head, &changes, &StableId::new("crate::A::f"));
        let expected = concat!(
            "sequenceDiagram\n",
            "    participant p0 as crate::A\n",
            "    rect rgb(254,242,242)\n",
            "        p0 ->> p0 : g (removed)\n",
            "    end\n",
        );
        assert_eq!(out, expected);
    }

    #[test]
    fn call_dyn_note_appears_for_unresolved_calls() {
        // f has two dynamic callees with no static target: they must be noted, never faked as calls.
        let head = Graph {
            nodes: vec![fn_node_dyn("crate::A::f", 2), fn_node("crate::A::g")],
            edges: vec![calls("crate::A::f", "crate::A::g", 0)],
        };
        let out = render_call_view(&head, &[], &StableId::new("crate::A::f"));
        assert!(
            out.contains("    p0 ->> p0 : g\n"),
            "resolved call missing:\n{out}"
        );
        assert!(
            out.contains("    note over p0 : 2 dynamic call(s) not shown\n"),
            "dyn note missing:\n{out}"
        );
        // The note follows the message.
        let msg = out.find("p0 ->> p0 : g").unwrap();
        let note = out.find("note over p0").unwrap();
        assert!(note > msg, "note must come after the message:\n{out}");
    }

    #[test]
    fn call_cycle_guard_terminates() {
        // start -> step -> start: the back edge to an ancestor is skipped, so the walk terminates.
        let head = Graph {
            nodes: vec![fn_node("crate::A::start"), fn_node("crate::B::step")],
            edges: vec![
                calls("crate::A::start", "crate::B::step", 0),
                calls("crate::B::step", "crate::A::start", 0),
            ],
        };
        let out = render_call_view(&head, &[], &StableId::new("crate::A::start"));
        let expected = concat!(
            "sequenceDiagram\n",
            "    participant p0 as crate::A\n",
            "    participant p1 as crate::B\n",
            "    p0 ->> p1 : step\n",
            "    p1 ->> p0 : start\n",
        );
        assert_eq!(out, expected);
    }
}
