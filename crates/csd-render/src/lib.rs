//! Render a graph and its delta as one annotated Mermaid diagram.
//!
//! Seven views live here: [`render_module_view`] (a `flowchart` over `Module` nodes and `Uses`
//! edges), [`render_state_view`] (a `stateDiagram-v2` over `Variant` nodes and `Transitions`
//! edges, SPEC.md 3.4), [`render_type_view`] (a `classDiagram` over `Struct`/`Enum`/`Trait`
//! nodes with `Implements`/`Associates` relations plus their methods as members, SPEC.md 3.2),
//! [`render_call_view`] (a `sequenceDiagram` slice from an entry `Fn` over `Calls` edges,
//! SPEC.md 3.3), the global call-graph `flowchart` ([`View::CallGraph`], one box per `Fn`,
//! grouped into per-owner subgraphs) and the schema `erDiagram` ([`View::Schema`], over
//! `Table` nodes and `ForeignKey` edges, SPEC.md 3.5) and the combined schema `flowchart`
//! ([`View::Overview`], modules as subgraphs holding their items with every edge kind). All follow
//! the project convention
//! (SPEC.md section 4): the delta is
//! colour-encoded onto a single drawing rather than diffing two images (green added, red-dashed
//! removed, amber changed, gray unchanged context) and the graph is pruned to changed nodes, the
//! endpoints of changed edges, and a two-hop neighbourhood, so a large system never renders in
//! full. The colouring ([`Class`]/[`CLASS_DEFS`]) and pruning ([`prune_within`]) are shared.
//!
//! Output is deterministic: nodes are emitted in id order with stable handles, edges in
//! `(from, to)` order.
//!
//! # Options ([`RenderOpts`], [`render`])
//!
//! Each view is also reachable through [`render`] plus a [`View`] selector, honouring
//! [`RenderOpts`]:
//!
//! - `full`: render the whole graph, not just the pruned delta. Every collected node is drawn
//!   (unchanged ones as `context`) with the delta colour on top; the changed-node pruning is
//!   skipped. The state view still applies its real-state-machine filter; `full` only disables the
//!   neighbourhood prune.
//! - `scope`: a list of path globs (`glob::Pattern`, e.g. `dir/**`) matched against
//!   [`csd_ir::SourceSpan::file`]. Only in-scope nodes render. An edge that crosses the boundary
//!   (one endpoint in scope, one out) draws its out-of-scope endpoint as a collapsed external
//!   **stub**: a `context`-classed node whose label is suffixed ` (external)`, so the crossing arrow
//!   is visible without expanding the far side. Edges wholly outside scope are dropped. Scope
//!   composes with `full` and with the delta.
//! - `entry`: the entry `Fn` for the call view (see [`render_call_view`]).
//!
//! The four `render_*_view` fns are thin wrappers over [`render`] with default options, so existing
//! callers are unchanged.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use csd_ir::{Change, EdgeKind, Fingerprint, Graph, NodeKind, StableId};

mod boxes;
mod svg;

/// Which view [`render`] produces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    /// Module `flowchart` (see [`render_module_view`]).
    Modules,
    /// State-machine `stateDiagram-v2` (see [`render_state_view`]).
    States,
    /// Type `classDiagram` (see [`render_type_view`]).
    Types,
    /// Call `sequenceDiagram` slice (see [`render_call_view`]).
    Calls,
    /// Global call-graph `flowchart` (see [`call_graph_view`]): one box per `Fn`, `Calls` edges as
    /// arrows, grouped into per-owner subgraphs. Distinct from [`View::Calls`], which is a slice.
    CallGraph,
    /// Schema `erDiagram` (see [`schema_view`]) over `Table` nodes and `ForeignKey` edges.
    Schema,
    /// Combined "code schema" `flowchart` (see [`overview_view`]): one subgraph per `Module`
    /// holding its `Struct`/`Enum`/`Trait`/`Fn` items, with every non-module edge kind
    /// (`Implements`/`Associates`/`Calls`) drawn among the items and `Uses` between the subgraphs.
    Overview,
}

/// Which output syntax [`render`] emits.
///
/// [`Default`] is [`Format::Mermaid`], the classic behaviour. [`Format::Dot`], [`Format::Ascii`] and
/// [`Format::Boxes`] are JS-free alternatives for the DAG-shaped views (see [`render`]); they reuse
/// the same delta selection and differ only in emission.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Format {
    /// Mermaid diagram source (the default, one syntax per view).
    #[default]
    Mermaid,
    /// Graphviz DOT `digraph` for every graph-shaped view (all but the Calls sequence view).
    Dot,
    /// cargo-tree-style ASCII text for every graph-shaped view (all but the Calls sequence view).
    Ascii,
    /// Native layered ASCII boxes-and-arrows for every graph-shaped view (all but the Calls
    /// sequence view). Pure Rust, no external tools; see [`boxes`] and [`boxes_view`].
    Boxes,
    /// A rendered SVG image via the pure-Rust layout crate (no Node/Chromium). Every graph-shaped
    /// view (all but the Calls sequence view).
    Svg,
}

/// Render options shared by every view.
///
/// See the crate docs for `full`, `scope` and `entry`. [`Default`] reproduces the classic
/// pruned-delta Mermaid behaviour (no full, no scope, no entry, `Format::Mermaid`).
#[derive(Debug, Clone, Default)]
pub struct RenderOpts {
    /// Render the whole graph (unchanged nodes as context), skipping the changed-node prune.
    pub full: bool,
    /// Path globs matched against `Node.span.file`; empty means every node is in scope.
    pub scope: Vec<String>,
    /// Entry `Fn` id for the call view; ignored by the other views.
    pub entry: Option<StableId>,
    /// Output syntax; [`Format::Mermaid`] by default. `full`/`scope` compose with every format.
    pub format: Format,
}

/// Render `view` of `head` with `changes` colour-encoded, honouring `opts`.
///
/// The per-view node/edge/`Class` selection is shared across formats (see [`select`] and
/// [`select_graph`]); only the final emission differs. The Mermaid path keeps one bespoke syntax per
/// view. The Dot and Ascii paths cover the graph/DAG views and degrade unsupported views to a
/// one-line comment rather than crashing.
pub fn render(view: View, head: &Graph, changes: &[Change], opts: &RenderOpts) -> String {
    match opts.format {
        Format::Mermaid => match view {
            View::Modules => module_view(head, changes, opts),
            View::States => state_view(head, changes, opts),
            View::Types => type_view(head, changes, opts),
            View::Calls => call_view(head, changes, opts),
            View::CallGraph => call_graph_view(head, changes, opts),
            View::Schema => schema_view(head, changes, opts),
            View::Overview => overview_view(head, changes, opts),
        },
        Format::Dot => dot_view(view, head, changes, opts),
        Format::Ascii => ascii_view(view, head, changes, opts),
        Format::Boxes => boxes_view(view, head, changes, opts),
        Format::Svg => svg_view(view, head, changes, opts),
    }
}

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
    module_view(head, changes, &RenderOpts::default())
}

fn module_view(head: &Graph, changes: &[Change], opts: &RenderOpts) -> String {
    let nodes = collect_nodes(head, changes, NodeKind::Module);
    let node_class = classify_nodes(&nodes, changes);
    let edges = collect_edges(head, changes, &nodes, EdgeKind::Uses);
    let files = file_index(head, changes);
    let patterns = compile_scope(&opts.scope);

    let sel = select(
        &nodes,
        &node_class,
        &edges,
        &files,
        &patterns,
        opts.full,
        CONTEXT_HOPS,
    );

    let mut out = String::from("flowchart LR\n");
    out.push_str(CLASS_DEFS);
    out.push('\n');

    if sel.render_ids.is_empty() {
        out.push_str("    %% no structural changes in the module view\n");
        return out;
    }

    // Stable n<i> handles in id order.
    let handles: BTreeMap<&StableId, String> = sel
        .render_ids
        .iter()
        .enumerate()
        .map(|(i, id)| (*id, format!("n{i}")))
        .collect();

    for id in &sel.render_ids {
        let (label, class) = sel.label_class(id, &node_class);
        let _ = writeln!(out, "    {}[\"{}\"]:::{}", handles[id], label, class.css());
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
    state_view(head, changes, &RenderOpts::default())
}

fn state_view(head: &Graph, changes: &[Change], opts: &RenderOpts) -> String {
    let mut nodes = collect_nodes(head, changes, NodeKind::Variant);
    let edges = collect_edges(head, changes, &nodes, EdgeKind::Transitions);

    // Only real state machines: an enum with at least one transition. Data enums (their variants
    // never appear on a Transitions edge) are dropped so the view is not flooded with noise. `full`
    // disables the changed-node prune but not this machine filter.
    let machines: BTreeSet<String> = edges
        .iter()
        .flat_map(|(from, to, _)| [enum_of(from), enum_of(to)])
        .collect();
    nodes.retain(|id, _| machines.contains(&enum_of(id)));

    let node_class = classify_nodes(&nodes, changes);
    let files = file_index(head, changes);
    let patterns = compile_scope(&opts.scope);
    let sel = select(
        &nodes,
        &node_class,
        &edges,
        &files,
        &patterns,
        opts.full,
        CONTEXT_HOPS,
    );

    let mut out = String::from("stateDiagram-v2\n");
    out.push_str(CLASS_DEFS);
    out.push('\n');

    if sel.render_ids.is_empty() {
        out.push_str("    %% no state machines (no enum has transitions)\n");
        return out;
    }

    // Stable s<i> handles in id order.
    let handles: BTreeMap<&StableId, String> = sel
        .render_ids
        .iter()
        .enumerate()
        .map(|(i, id)| (*id, format!("s{i}")))
        .collect();

    // Declare each state, labelled with its variant id (stubs suffixed ` (external)`).
    for id in &sel.render_ids {
        let (label, _) = sel.label_class(id, &node_class);
        let _ = writeln!(out, "    state \"{}\" as {}", label, handles[id]);
    }
    // Colour each state via the ::: operator.
    for id in &sel.render_ids {
        let (_, class) = sel.label_class(id, &node_class);
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
    type_view(head, changes, &RenderOpts::default())
}

fn type_view(head: &Graph, changes: &[Change], opts: &RenderOpts) -> String {
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

    // One-hop prune (`select` skips it under `full`): changed classes plus one relation away.
    let prune_edges: Vec<(&StableId, &StableId, Class)> =
        relations.iter().map(|(f, t, _, c)| (*f, *t, *c)).collect();
    let files = file_index(head, changes);
    let patterns = compile_scope(&opts.scope);
    let sel = select(
        &nodes,
        &node_class,
        &prune_edges,
        &files,
        &patterns,
        opts.full,
        1,
    );

    let mut out = String::from("classDiagram\n");
    out.push_str(CLASS_DEFS);
    out.push('\n');

    if sel.render_ids.is_empty() {
        out.push_str("    %% no structural changes in the type view\n");
        return out;
    }

    // Stable c<i> handles in id order (class ids carry `::`/`<>`, so a raw id is not a valid name).
    let handles: BTreeMap<&StableId, String> = sel
        .render_ids
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

    // Per-method (Fn) delta class, keyed by fn id: an `Added`/`Removed` fn or one whose
    // `member_types` signature changed. A fn whose id is exactly `<class>::<method>` is listed as a
    // member of that class below.
    let method_status = method_status(head, changes);

    // Declare each class, with a `<<trait>>` stereotype and, for non-stub classes, its member
    // lines: fields first (from a Modified fingerprint), then methods (from `Fn` nodes), each
    // deterministically ordered. Methods carry a `()` suffix to distinguish them from fields. A
    // scope stub renders as a bare external class with no body.
    for id in &sel.render_ids {
        let handle = &handles[id];
        let is_stub = sel.stubs.contains(id);
        let is_trait = !is_stub && kind_of.get(id).copied() == Some(NodeKind::Trait);
        let (fields, methods) = if is_stub {
            (Vec::new(), Vec::new())
        } else {
            let fields = modified.get(id).map(|(b, a)| member_lines(b, a));
            (
                fields.unwrap_or_default(),
                method_lines(id.as_str(), &method_status),
            )
        };
        let (label, _) = sel.label_class(id, &node_class);
        let has_body = is_trait || !fields.is_empty() || !methods.is_empty();
        if has_body {
            let _ = writeln!(out, "    class {}[\"{}\"] {{", handle, label);
            if is_trait {
                out.push_str("        <<trait>>\n");
            }
            for line in fields.iter().chain(methods.iter()) {
                let _ = writeln!(out, "        {line}");
            }
            out.push_str("    }\n");
        } else {
            let _ = writeln!(out, "    class {}[\"{}\"]", handle, label);
        }
    }

    // Colour each class via the ::: operator.
    for id in &sel.render_ids {
        let (_, class) = sel.label_class(id, &node_class);
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
    call_view(
        head,
        changes,
        &RenderOpts {
            entry: Some(entry.clone()),
            ..RenderOpts::default()
        },
    )
}

/// Call-view core (see [`render_call_view`]). `opts.entry` names the entry `Fn`; `opts.scope`
/// collapses the walk at out-of-scope callees (their subtree is not expanded); `opts.full` is a
/// no-op here (the slice is already the whole reachable call tree, bounded by depth and cycles).
fn call_view(head: &Graph, changes: &[Change], opts: &RenderOpts) -> String {
    let entry = match &opts.entry {
        Some(e) => e,
        None => return String::from("sequenceDiagram\n    %% no entry fn specified\n"),
    };
    // Scope predicate over fn source paths; empty scope keeps every callee.
    let files = file_index(head, changes);
    let patterns = compile_scope(&opts.scope);
    let in_scope = |id: &str| -> bool {
        patterns.is_empty()
            || files
                .get(&StableId::new(id))
                .is_some_and(|f| matches_scope(f, &patterns))
    };

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
    let unresolved = unresolved_calls(head);

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
        &in_scope,
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

/// The dyn/generics fidelity ceiling per fn id: `attrs["unresolved_calls"] = N` (N > 0) means the fn
/// has N dynamic or generic call sites with no statically known callee. Keyed by fn id string so the
/// value compares across `head` and the delta. Fns with no unresolved calls are absent, so an empty
/// map means the whole graph is statically resolved. Shared by every view that draws calls, so the
/// ceiling is surfaced honestly and identically rather than recomputed per emitter.
fn unresolved_calls(head: &Graph) -> BTreeMap<&str, u32> {
    head.nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Fn)
        .filter_map(|n| {
            n.attrs
                .get("unresolved_calls")
                .and_then(|v| v.parse::<u32>().ok())
                .filter(|c| *c > 0)
                .map(|c| (n.id.as_str(), c))
        })
        .collect()
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
    in_scope: &dyn Fn(&str) -> bool,
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
            // Recurse into the callee unless it is an ancestor (cycle), out of scope (collapsed as
            // an external leaf), or we hit the depth ceiling.
            if depth + 1 < MAX_CALL_DEPTH
                && !stack.contains(&out.to)
                && outgoing.contains_key(out.to)
                && in_scope(out.to)
            {
                walk_calls(
                    out.to,
                    depth + 1,
                    stack,
                    outgoing,
                    unresolved,
                    in_scope,
                    parts,
                    lines,
                );
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

/// The enum id owning a variant id: the id with its last `::segment` stripped
/// (`crate::Status::Active` -> `crate::Status`).
fn enum_of(variant: &StableId) -> String {
    let s = variant.as_str();
    match s.rfind("::") {
        Some(pos) => s[..pos].to_string(),
        None => s.to_string(),
    }
}

/// Source path per node id: from `head` plus nodes the delta removed (absent from `head`).
fn file_index<'a>(head: &'a Graph, changes: &'a [Change]) -> BTreeMap<&'a StableId, &'a str> {
    let mut files: BTreeMap<&StableId, &str> = BTreeMap::new();
    for n in &head.nodes {
        files.insert(&n.id, n.span.file.as_str());
    }
    for c in changes {
        if let Change::Removed(n) = c {
            files.entry(&n.id).or_insert(n.span.file.as_str());
        }
    }
    files
}

/// Compile the scope globs, dropping any that fail to parse. An empty result means "no scope".
fn compile_scope(scope: &[String]) -> Vec<glob::Pattern> {
    scope
        .iter()
        .filter_map(|s| glob::Pattern::new(s).ok())
        .collect()
}

/// Whether `file` matches any scope glob.
fn matches_scope(file: &str, patterns: &[glob::Pattern]) -> bool {
    patterns.iter().any(|p| p.matches(file))
}

/// The nodes a view draws plus the out-of-scope stub endpoints, after `full` and `scope`.
struct Selection<'a> {
    /// Ids to render, in id order (kept in-scope nodes plus external stubs).
    render_ids: Vec<&'a StableId>,
    /// Subset of `render_ids` that are collapsed external stubs (out of scope, edge endpoints).
    stubs: BTreeSet<&'a StableId>,
}

impl<'a> Selection<'a> {
    /// The label and colour for a rendered id. A stub is `context` with an ` (external)` suffix;
    /// otherwise the delta class (unlisted -> context) and the raw id.
    fn label_class(
        &self,
        id: &StableId,
        node_class: &BTreeMap<&StableId, Class>,
    ) -> (String, Class) {
        if self.stubs.contains(id) {
            (format!("{} (external)", id.as_str()), Class::Context)
        } else {
            let class = node_class.get(id).copied().unwrap_or(Class::Context);
            (id.as_str().to_string(), class)
        }
    }
}

/// Pick the nodes to render, honouring `full` (no prune) and `scope` (path globs + boundary stubs).
///
/// Without scope and without full this is exactly the classic pruned neighbourhood. `full` swaps the
/// prune for "every collected node". `scope` then keeps only in-scope nodes and, for each edge that
/// crosses into a kept node, records the out-of-scope endpoint as a stub so the arrow stays visible.
fn select<'a>(
    nodes: &BTreeMap<&'a StableId, ()>,
    node_class: &BTreeMap<&'a StableId, Class>,
    edges: &[(&'a StableId, &'a StableId, Class)],
    files: &BTreeMap<&'a StableId, &'a str>,
    patterns: &[glob::Pattern],
    full: bool,
    hops: usize,
) -> Selection<'a> {
    let scoped = !patterns.is_empty();
    let in_scope = |id: &StableId| -> bool {
        !scoped || files.get(id).is_some_and(|f| matches_scope(f, patterns))
    };

    // Candidate set before scope: the whole graph under `full`, else the pruned neighbourhood.
    let candidates: Vec<&StableId> = if full {
        nodes.keys().copied().collect()
    } else {
        prune_within(nodes, node_class, edges, hops)
    };

    // Keep the in-scope candidates.
    let kept: BTreeSet<&StableId> = candidates.into_iter().filter(|id| in_scope(id)).collect();

    // Stubs: out-of-scope endpoints of edges that touch a kept node.
    let mut stubs: BTreeSet<&StableId> = BTreeSet::new();
    if scoped {
        for (from, to, _) in edges {
            if kept.contains(from) && !in_scope(to) {
                stubs.insert(to);
            }
            if kept.contains(to) && !in_scope(from) {
                stubs.insert(from);
            }
        }
    }

    let mut render_ids: Vec<&StableId> =
        kept.iter().copied().chain(stubs.iter().copied()).collect();
    render_ids.sort();
    Selection { render_ids, stubs }
}

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

/// Keep changed nodes, the endpoints of changed edges, and everything within a caller-chosen
/// neighbourhood radius. The module and state views pass [`CONTEXT_HOPS`]; the type view passes 1.
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

/// Per-method delta class keyed by fn id, over the `Fn` nodes in `head` plus any the delta removed.
///
/// A fn is `Added`/`Removed` from the matching [`Change`]; a `Modified` fn is `Changed` only when
/// its [`Fingerprint::member_types`] signature differs (a real retype), else `Context`.
fn method_status<'a>(head: &'a Graph, changes: &'a [Change]) -> BTreeMap<&'a str, Class> {
    let mut status: BTreeMap<&str, Class> = BTreeMap::new();
    for n in &head.nodes {
        if n.kind == NodeKind::Fn {
            status.insert(n.id.as_str(), Class::Context);
        }
    }
    for c in changes {
        match c {
            Change::Added(n) if n.kind == NodeKind::Fn => {
                status.insert(n.id.as_str(), Class::Added);
            }
            Change::Removed(n) if n.kind == NodeKind::Fn => {
                status.insert(n.id.as_str(), Class::Removed);
            }
            Change::Modified { before, after } if after.kind == NodeKind::Fn => {
                let changed = matches!(
                    (before.fingerprint.as_ref(), after.fingerprint.as_ref()),
                    (Some(b), Some(a)) if b.member_types != a.member_types
                );
                status.insert(
                    after.id.as_str(),
                    if changed {
                        Class::Changed
                    } else {
                        Class::Context
                    },
                );
            }
            _ => {}
        }
    }
    status
}

/// Member lines for the methods of `class_id`: the `Fn` ids whose owner (id minus the last
/// `::segment`) is exactly `class_id`, rendered `+name()`/`-name()`/`~name()`/`name()` by delta
/// class. `status` is sorted by fn id, so lines come out in method-name order (deterministic).
fn method_lines(class_id: &str, status: &BTreeMap<&str, Class>) -> Vec<String> {
    status
        .iter()
        .filter(|(id, _)| call_owner(id) == class_id)
        .map(|(id, class)| {
            let prefix = match class {
                Class::Added => "+",
                Class::Removed => "-",
                Class::Changed => "~",
                Class::Context => "",
            };
            format!("{prefix}{}()", call_name(id))
        })
        .collect()
}

/// Render the global call-graph view of `head` with `changes` colour-encoded, as a Mermaid
/// `flowchart LR` (SPEC.md 3.3, the codebase-oriented "functions and flow" picture).
///
/// Unlike [`render_call_view`], which slices a `sequenceDiagram` from one entry and groups callers
/// into per-owner lifelines, this draws **one box per [`NodeKind::Fn`]** so a crate of flat free
/// functions never collapses to a single node: each `Calls` edge is an arrow between two distinct
/// boxes. Functions are grouped into `subgraph` blocks by owning module/type (fn id minus its last
/// `::segment`) purely for readability; the boxes stay separate. Nodes and edges are delta-coloured
/// via [`Class`]/[`CLASS_DEFS`]. A fn with `attrs["unresolved_calls"] > 0` gets a dashed edge to a
/// small `:::context` `dyn` stub, so the dynamic/generic fidelity ceiling is shown, never faked.
///
/// `full`/`scope` are honoured through the shared [`select`] machinery: `full` draws every fn,
/// `scope` keeps in-scope fns and renders a crossing callee/caller as an external stub.
fn call_graph_view(head: &Graph, changes: &[Change], opts: &RenderOpts) -> String {
    let nodes = collect_nodes(head, changes, NodeKind::Fn);
    let node_class = classify_nodes(&nodes, changes);
    let edges = collect_edges(head, changes, &nodes, EdgeKind::Calls);
    let files = file_index(head, changes);
    let patterns = compile_scope(&opts.scope);
    let sel = select(
        &nodes,
        &node_class,
        &edges,
        &files,
        &patterns,
        opts.full,
        CONTEXT_HOPS,
    );

    let mut out = String::from("flowchart LR\n");
    out.push_str(CLASS_DEFS);
    out.push('\n');

    if sel.render_ids.is_empty() {
        out.push_str("    %% no structural changes in the call graph\n");
        return out;
    }

    // Stable f<i> handles in id order.
    let handles: BTreeMap<&StableId, String> = sel
        .render_ids
        .iter()
        .enumerate()
        .map(|(i, id)| (*id, format!("f{i}")))
        .collect();

    // `unresolved_calls` counts per fn (the dyn/generics fidelity ceiling).
    let unresolved = unresolved_calls(head);

    // Group the rendered fns by owner (sorted), one subgraph each. render_ids is already sorted, so
    // fns within a subgraph stay in id order.
    let mut by_owner: BTreeMap<&str, Vec<&StableId>> = BTreeMap::new();
    for id in &sel.render_ids {
        by_owner
            .entry(call_owner(id.as_str()))
            .or_default()
            .push(id);
    }
    for (i, (owner, ids)) in by_owner.iter().enumerate() {
        let _ = writeln!(out, "    subgraph sg{i}[\"{owner}\"]");
        for id in ids {
            let is_stub = sel.stubs.contains(id);
            let class = if is_stub {
                Class::Context
            } else {
                node_class.get(id).copied().unwrap_or(Class::Context)
            };
            let name = call_name(id.as_str());
            let label = if is_stub {
                format!("{name} (external)")
            } else {
                name.to_string()
            };
            let _ = writeln!(
                out,
                "        {}[\"{}\"]:::{}",
                handles[id],
                label,
                class.css()
            );
        }
        out.push_str("    end\n");
    }

    // Call edges as arrows; added/removed carry a +/- marker (same convention as the module view).
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

    // Honest dyn ceiling: a dashed edge to a `dyn` stub for each rendered fn with unresolved calls.
    let mut dyn_idx = 0;
    for id in &sel.render_ids {
        if sel.stubs.contains(id) {
            continue;
        }
        if unresolved.contains_key(id.as_str()) {
            let _ = writeln!(out, "    dyn{dyn_idx}[\"dyn\"]:::context");
            let _ = writeln!(out, "    {} -.-> dyn{dyn_idx}", handles[id]);
            dyn_idx += 1;
        }
    }

    out
}

/// The columns of a table node, parsed from `attrs["columns"]` (`"id: Int4, name: Varchar"`), as
/// erDiagram attribute lines `Type name` (`"id: Int4"` -> `Int4 id`). Entries without a `:` are
/// skipped. Best-effort: whitespace is trimmed, non-name chars are not otherwise validated.
fn column_lines(columns: Option<&str>) -> Vec<String> {
    let Some(cols) = columns else {
        return Vec::new();
    };
    cols.split(',')
        .filter_map(|c| {
            let (name, ty) = c.split_once(':')?;
            let (name, ty) = (name.trim(), ty.trim());
            if name.is_empty() || ty.is_empty() {
                None
            } else {
                Some(format!("{} {}", sanitize_ident(ty), sanitize_ident(name)))
            }
        })
        .collect()
}

/// A Mermaid-safe identifier: every char outside `[A-Za-z0-9_]` becomes `_`. erDiagram entity and
/// attribute names cannot carry `::` or spaces, so ids are projected through this.
fn sanitize_ident(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

/// Render the schema view of `head` with `changes`, as a Mermaid `erDiagram` (SPEC.md 3.5).
///
/// Entities are [`NodeKind::Table`] nodes; their columns come from `attrs["columns"]` (see
/// [`column_lines`]). Relationships are [`EdgeKind::ForeignKey`] edges, drawn `child }o--|| parent`.
/// `erDiagram` supports neither `classDef` colouring nor per-member colour, so the delta is carried
/// as **text markers**, not colour: an added/removed table gets a leading `csd_delta added` /
/// `csd_delta removed` attribute row, and an added/removed relationship gets a `(added)`/`(removed)`
/// suffix on its label. `full`/`scope` are honoured through [`select`] as in the type view. Empty
/// case: the header plus a `%% no schema tables` comment.
fn schema_view(head: &Graph, changes: &[Change], opts: &RenderOpts) -> String {
    let nodes = collect_nodes(head, changes, NodeKind::Table);
    let node_class = classify_nodes(&nodes, changes);
    let edges = collect_edges(head, changes, &nodes, EdgeKind::ForeignKey);
    let files = file_index(head, changes);
    let patterns = compile_scope(&opts.scope);
    let sel = select(
        &nodes,
        &node_class,
        &edges,
        &files,
        &patterns,
        opts.full,
        CONTEXT_HOPS,
    );

    let mut out = String::from("erDiagram\n");
    if sel.render_ids.is_empty() {
        out.push_str("    %% no schema tables\n");
        return out;
    }

    // Column source per table id: head node attrs plus removed nodes recovered from the delta.
    let mut columns: BTreeMap<&StableId, &str> = BTreeMap::new();
    for n in &head.nodes {
        if n.kind == NodeKind::Table {
            if let Some(c) = n.attrs.get("columns") {
                columns.insert(&n.id, c.as_str());
            }
        }
    }
    for c in changes {
        if let Change::Removed(n) = c {
            if n.kind == NodeKind::Table {
                if let Some(c) = n.attrs.get("columns") {
                    columns.entry(&n.id).or_insert(c.as_str());
                }
            }
        }
    }

    let rendered: BTreeSet<&StableId> = sel.render_ids.iter().copied().collect();
    let ent = |id: &StableId| sanitize_ident(id.as_str());

    // Declare each entity, delta marker (if any) first, then its columns.
    for id in &sel.render_ids {
        let is_stub = sel.stubs.contains(id);
        let class = if is_stub {
            Class::Context
        } else {
            node_class.get(id).copied().unwrap_or(Class::Context)
        };
        let marker = match class {
            Class::Added => Some("added"),
            Class::Removed => Some("removed"),
            _ => None,
        };
        let cols = if is_stub {
            Vec::new()
        } else {
            column_lines(columns.get(id).copied())
        };
        if marker.is_some() || !cols.is_empty() {
            let _ = writeln!(out, "    {} {{", ent(id));
            if let Some(m) = marker {
                let _ = writeln!(out, "        csd_delta {m}");
            }
            for line in &cols {
                let _ = writeln!(out, "        {line}");
            }
            out.push_str("    }\n");
        } else {
            let _ = writeln!(out, "    {}", ent(id));
        }
    }

    // Foreign-key relationships in (from, to) order; endpoints pruned away are skipped.
    for (from, to, class) in &edges {
        if !rendered.contains(from) || !rendered.contains(to) {
            continue;
        }
        let label = match class {
            Class::Added => "references (added)",
            Class::Removed => "references (removed)",
            _ => "references",
        };
        let _ = writeln!(out, "    {} }}o--|| {} : \"{}\"", ent(from), ent(to), label);
    }

    out
}

/// Render the combined overview ("code schema") of `head` with `changes` colour-encoded, as a
/// Mermaid `flowchart LR` (issue view-overview: the SchemaSpy/drawDB analogue for code).
///
/// One diagram for the whole structure. Every `Struct`/`Enum`/`Trait`/`Fn` item is a node, grouped
/// into one `subgraph` per owning module (owner = id minus its last `::segment`), and every
/// non-module edge kind is drawn together among the items: `Implements`, `Associates` and `Calls`,
/// each disambiguated by a short edge label (`impl`/`assoc`/`calls`). Module-level `Uses` edges are
/// drawn between the subgraphs that are rendered (label `uses`). `flowchart` has no realization
/// arrow (`..|>` is `classDiagram`-only), so relations are told apart by their label, not arrow
/// glyph. Types render as rectangles, fns as rounded boxes, so the two item kinds read apart.
///
/// Delta colouring is identical in meaning to the other views: nodes via [`classify_nodes`]
/// (added green, removed red-dashed, changed amber/`Moved` amber, context gray) and edges via
/// [`collect_edges`], with the same arrow/marker convention as [`module_view`] (`-->|... +|` added,
/// `-.->|... -|` removed). Removed items and removed edges are recovered from `changes`. `flowchart`
/// cannot colour a subgraph box, so a module whose `Module` node is entirely added/removed is marked
/// with a ` (added)`/` (removed)` title suffix. `full`/`scope` compose through the shared [`select`]
/// machinery (2-hop neighbourhood by default); scope is the primary comprehension use case.
///
/// Empty case: the header plus `%% no structural changes in the overview` (or `%% empty overview`
/// for `full` on an empty graph).
fn overview_view(head: &Graph, changes: &[Change], opts: &RenderOpts) -> String {
    // Item nodes: Struct/Enum/Trait/Fn.
    let mut items = collect_nodes(head, changes, NodeKind::Struct);
    items.extend(collect_nodes(head, changes, NodeKind::Enum));
    items.extend(collect_nodes(head, changes, NodeKind::Trait));
    items.extend(collect_nodes(head, changes, NodeKind::Fn));
    let node_class = classify_nodes(&items, changes);

    // All item-level edge kinds together, tagged with their kind.
    let mut kinded: Vec<(&StableId, &StableId, EdgeKind, Class)> = Vec::new();
    for (from, to, class) in collect_edges(head, changes, &items, EdgeKind::Implements) {
        kinded.push((from, to, EdgeKind::Implements, class));
    }
    for (from, to, class) in collect_edges(head, changes, &items, EdgeKind::Associates) {
        kinded.push((from, to, EdgeKind::Associates, class));
    }
    for (from, to, class) in collect_edges(head, changes, &items, EdgeKind::Calls) {
        kinded.push((from, to, EdgeKind::Calls, class));
    }
    kinded.sort_by(|a, b| (a.0, a.1, a.2).cmp(&(b.0, b.1, b.2)));

    // Prune over the union of item edges (kind dropped), then honour `full`/`scope`.
    let prune_edges: Vec<(&StableId, &StableId, Class)> =
        kinded.iter().map(|(f, t, _, c)| (*f, *t, *c)).collect();
    let files = file_index(head, changes);
    let patterns = compile_scope(&opts.scope);
    let sel = select(
        &items,
        &node_class,
        &prune_edges,
        &files,
        &patterns,
        opts.full,
        CONTEXT_HOPS,
    );

    let mut out = String::from("flowchart LR\n");
    out.push_str(CLASS_DEFS);
    out.push('\n');

    if sel.render_ids.is_empty() {
        if opts.full {
            out.push_str("    %% empty overview\n");
        } else {
            out.push_str("    %% no structural changes in the overview\n");
        }
        return out;
    }

    // Stable n<i> handles in id order.
    let handles: BTreeMap<&StableId, String> = sel
        .render_ids
        .iter()
        .enumerate()
        .map(|(i, id)| (*id, format!("n{i}")))
        .collect();

    // Node kind per id (chooses the shape), including items the delta removed.
    let mut kind_of: BTreeMap<&StableId, NodeKind> =
        head.nodes.iter().map(|n| (&n.id, n.kind)).collect();
    for c in changes {
        if let Change::Removed(n) = c {
            kind_of.entry(&n.id).or_insert(n.kind);
        }
    }

    // Module delta class, so an entirely added/removed module box can be marked in its title.
    let modules = collect_nodes(head, changes, NodeKind::Module);
    let module_class = classify_nodes(&modules, changes);
    let module_class_by_id: BTreeMap<&str, Class> =
        module_class.iter().map(|(k, v)| (k.as_str(), *v)).collect();

    // Group rendered items into one subgraph per owning module (sorted). render_ids is already
    // sorted, so items within a subgraph stay in id order.
    let mut by_owner: BTreeMap<&str, Vec<&StableId>> = BTreeMap::new();
    for id in &sel.render_ids {
        by_owner
            .entry(call_owner(id.as_str()))
            .or_default()
            .push(id);
    }
    // Owner -> its subgraph handle, for linking Uses edges between subgraphs.
    let sg_handle: BTreeMap<&str, String> = by_owner
        .keys()
        .enumerate()
        .map(|(i, owner)| (*owner, format!("sg{i}")))
        .collect();

    for (owner, ids) in &by_owner {
        let suffix = match module_class_by_id.get(owner) {
            Some(Class::Added) => " (added)",
            Some(Class::Removed) => " (removed)",
            _ => "",
        };
        let _ = writeln!(
            out,
            "    subgraph {}[\"{}{}\"]",
            sg_handle[owner], owner, suffix
        );
        for id in ids {
            let is_stub = sel.stubs.contains(id);
            let class = if is_stub {
                Class::Context
            } else {
                node_class.get(id).copied().unwrap_or(Class::Context)
            };
            let name = call_name(id.as_str());
            let label = if is_stub {
                format!("{name} (external)")
            } else {
                name.to_string()
            };
            // Fns render as rounded boxes, types as rectangles, so the item kinds read apart.
            let (open, close) = match kind_of.get(id) {
                Some(NodeKind::Fn) => ("(", ")"),
                _ => ("[", "]"),
            };
            let _ = writeln!(
                out,
                "        {}{}\"{}\"{}:::{}",
                handles[id],
                open,
                label,
                close,
                class.css()
            );
        }
        out.push_str("    end\n");
    }

    // Item-level edges: all kinds together, disambiguated by a short label, delta-coloured with the
    // same arrow/marker convention as the module view.
    for (from, to, kind, class) in &kinded {
        if !handles.contains_key(from) || !handles.contains_key(to) {
            continue;
        }
        let tag = match kind {
            EdgeKind::Implements => "impl",
            EdgeKind::Associates => "assoc",
            _ => "calls",
        };
        let (arrow, marker) = match class {
            Class::Added => ("-->", " +"),
            Class::Removed => ("-.->", " -"),
            _ => ("-->", ""),
        };
        let _ = writeln!(
            out,
            "    {} {}|{}{}| {}",
            handles[from], arrow, tag, marker, handles[to]
        );
    }

    // Module-level Uses edges between the subgraphs that are rendered (both owners present).
    let uses = collect_edges(head, changes, &modules, EdgeKind::Uses);
    for (from, to, class) in &uses {
        let (Some(sf), Some(st)) = (sg_handle.get(from.as_str()), sg_handle.get(to.as_str()))
        else {
            continue;
        };
        let (arrow, marker) = match class {
            Class::Added => ("-->", " +"),
            Class::Removed => ("-.->", " -"),
            _ => ("-->", ""),
        };
        let _ = writeln!(out, "    {} {}|uses{}| {}", sf, arrow, marker, st);
    }

    // Honest dyn ceiling: a dashed edge to a `dyn` stub for each rendered fn with unresolved calls,
    // matching the call-graph view so the flattened overview never hides dynamic dispatch either.
    let unresolved = unresolved_calls(head);
    let mut dyn_idx = 0;
    for id in &sel.render_ids {
        if sel.stubs.contains(id) {
            continue;
        }
        if unresolved.contains_key(id.as_str()) {
            let _ = writeln!(out, "    dyn{dyn_idx}[\"dyn\"]:::context");
            let _ = writeln!(out, "    {} -.-> dyn{dyn_idx}", handles[id]);
            dyn_idx += 1;
        }
    }

    out
}

/// A view's selected graph, shared across the alternative output formats.
///
/// Bundles what an emitter needs after [`select`] has run: the per-node delta [`Class`], the drawn
/// edges tagged with their class, and the [`Selection`] (render ids in id order plus external
/// stubs). The Dot and Ascii emitters consume exactly this; only the syntax they write differs.
struct GraphView<'a> {
    node_class: BTreeMap<&'a StableId, Class>,
    edges: Vec<(&'a StableId, &'a StableId, Class)>,
    sel: Selection<'a>,
    /// Rendered non-stub fns carrying a dyn/generics fidelity ceiling, as `(fn id, count)` in id
    /// order (see [`unresolved_calls`]). Each alternative-format emitter attaches an honest "dyn"
    /// signal from this, so no format silently hides that dynamic calls exist. Empty for views with
    /// no such fns (every view but the call graph and the overview).
    unresolved: Vec<(&'a StableId, u32)>,
}

/// Build the shared selected graph for a graph-shaped `view`, reusing the same collection, pruning
/// and scoping machinery the Mermaid path uses. Returns `None` for [`View::Calls`], the one view
/// with no graph form (it is a per-entry sequence slice).
fn select_graph<'a>(
    head: &'a Graph,
    changes: &'a [Change],
    opts: &RenderOpts,
    view: View,
) -> Option<GraphView<'a>> {
    let files = file_index(head, changes);
    let patterns = compile_scope(&opts.scope);

    let (node_class, edges, sel) = match view {
        View::Modules => {
            let nodes = collect_nodes(head, changes, NodeKind::Module);
            let node_class = classify_nodes(&nodes, changes);
            let edges = collect_edges(head, changes, &nodes, EdgeKind::Uses);
            let sel = select(
                &nodes,
                &node_class,
                &edges,
                &files,
                &patterns,
                opts.full,
                CONTEXT_HOPS,
            );
            (node_class, edges, sel)
        }
        View::CallGraph => {
            let nodes = collect_nodes(head, changes, NodeKind::Fn);
            let node_class = classify_nodes(&nodes, changes);
            let edges = collect_edges(head, changes, &nodes, EdgeKind::Calls);
            let sel = select(
                &nodes,
                &node_class,
                &edges,
                &files,
                &patterns,
                opts.full,
                CONTEXT_HOPS,
            );
            (node_class, edges, sel)
        }
        View::States => {
            let mut nodes = collect_nodes(head, changes, NodeKind::Variant);
            let edges = collect_edges(head, changes, &nodes, EdgeKind::Transitions);
            // Same real-state-machine filter as the Mermaid state view: keep only variants of an
            // enum that has at least one transition.
            let machines: BTreeSet<String> = edges
                .iter()
                .flat_map(|(from, to, _)| [enum_of(from), enum_of(to)])
                .collect();
            nodes.retain(|id, _| machines.contains(&enum_of(id)));
            let node_class = classify_nodes(&nodes, changes);
            let sel = select(
                &nodes,
                &node_class,
                &edges,
                &files,
                &patterns,
                opts.full,
                CONTEXT_HOPS,
            );
            (node_class, edges, sel)
        }
        View::Types => {
            let mut nodes = collect_nodes(head, changes, NodeKind::Struct);
            nodes.extend(collect_nodes(head, changes, NodeKind::Enum));
            nodes.extend(collect_nodes(head, changes, NodeKind::Trait));
            let node_class = classify_nodes(&nodes, changes);
            let mut edges = collect_edges(head, changes, &nodes, EdgeKind::Implements);
            edges.extend(collect_edges(head, changes, &nodes, EdgeKind::Associates));
            edges.sort_by(|a, b| (a.0, a.1).cmp(&(b.0, b.1)));
            // One-hop prune, as in the Mermaid type view (a type graph fans out fast).
            let sel = select(&nodes, &node_class, &edges, &files, &patterns, opts.full, 1);
            (node_class, edges, sel)
        }
        View::Schema => {
            let nodes = collect_nodes(head, changes, NodeKind::Table);
            let node_class = classify_nodes(&nodes, changes);
            let edges = collect_edges(head, changes, &nodes, EdgeKind::ForeignKey);
            let sel = select(
                &nodes,
                &node_class,
                &edges,
                &files,
                &patterns,
                opts.full,
                CONTEXT_HOPS,
            );
            (node_class, edges, sel)
        }
        View::Overview => {
            // Union of the item kinds and all non-module edge kinds; the kind tags are only needed
            // by the Mermaid path, so the shared graph keeps the plain (from, to, class) edges.
            let mut nodes = collect_nodes(head, changes, NodeKind::Struct);
            nodes.extend(collect_nodes(head, changes, NodeKind::Enum));
            nodes.extend(collect_nodes(head, changes, NodeKind::Trait));
            nodes.extend(collect_nodes(head, changes, NodeKind::Fn));
            let node_class = classify_nodes(&nodes, changes);
            let mut edges = collect_edges(head, changes, &nodes, EdgeKind::Implements);
            edges.extend(collect_edges(head, changes, &nodes, EdgeKind::Associates));
            edges.extend(collect_edges(head, changes, &nodes, EdgeKind::Calls));
            edges.sort_by(|a, b| (a.0, a.1).cmp(&(b.0, b.1)));
            let sel = select(
                &nodes,
                &node_class,
                &edges,
                &files,
                &patterns,
                opts.full,
                CONTEXT_HOPS,
            );
            (node_class, edges, sel)
        }
        View::Calls => return None,
    };

    // Shared dyn/generics ceiling: rendered non-stub fns with unresolved calls, in id order (render
    // ids are already sorted). Every alternative-format emitter draws these so the ceiling is never
    // silently dropped. Only Fn nodes carry the attr, so this is empty for the non-call views.
    let umap = unresolved_calls(head);
    let unresolved: Vec<(&StableId, u32)> = sel
        .render_ids
        .iter()
        .filter(|id| !sel.stubs.contains(*id))
        .filter_map(|id| umap.get(id.as_str()).map(|c| (*id, *c)))
        .collect();

    Some(GraphView {
        node_class,
        edges,
        sel,
        unresolved,
    })
}

/// DOT node attributes for a delta class (SPEC.md section 4 colours as Graphviz attributes).
fn dot_node_attrs(class: Class) -> &'static str {
    match class {
        Class::Added => "color=\"#22c55e\",style=filled,fillcolor=\"#f0fdf4\"",
        Class::Removed => "color=\"#ef4444\",style=\"filled,dashed\",fillcolor=\"#fef2f2\"",
        Class::Changed => "color=\"#f59e0b\",style=filled,fillcolor=\"#fffbeb\"",
        Class::Context => "color=\"#d1d5db\"",
    }
}

/// Escape a DOT double-quoted string: backslash and quote only (labels never carry newlines here).
fn dot_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Emit a graph view as Graphviz DOT (`digraph { ... }`), JS-free and offline.
///
/// Nodes carry a `label` and the delta style ([`dot_node_attrs`]); edges added/removed/plain, with a
/// removed edge dashed. Node ids are deterministic (`n0`..) in id order. [`View::Calls`] is a
/// sequence slice with no DOT form, so it degrades to a one-line comment rather than crashing.
fn dot_view(view: View, head: &Graph, changes: &[Change], opts: &RenderOpts) -> String {
    let Some(gv) = select_graph(head, changes, opts, view) else {
        return String::from(
            "// dot format not supported for the sequence view; use --format mermaid\n",
        );
    };

    let handles: BTreeMap<&StableId, String> = gv
        .sel
        .render_ids
        .iter()
        .enumerate()
        .map(|(i, id)| (*id, format!("n{i}")))
        .collect();

    let mut out = String::from("digraph {\n");
    for id in &gv.sel.render_ids {
        let (label, class) = gv.sel.label_class(id, &gv.node_class);
        let _ = writeln!(
            out,
            "    {} [label=\"{}\",{}];",
            handles[id],
            dot_escape(&label),
            dot_node_attrs(class)
        );
    }
    for (from, to, class) in &gv.edges {
        if !handles.contains_key(from) || !handles.contains_key(to) {
            continue;
        }
        let attr = match class {
            Class::Added => " [color=\"#22c55e\"]",
            Class::Removed => " [style=dashed,color=\"#ef4444\"]",
            _ => "",
        };
        let _ = writeln!(out, "    {} -> {}{};", handles[from], handles[to], attr);
    }

    // Honest dyn ceiling: a dashed edge to a `dyn` context node for each rendered fn with unresolved
    // dynamic/generic callees, so the DOT output shows the ceiling instead of silently omitting it.
    for (i, (id, _count)) in gv.unresolved.iter().enumerate() {
        let _ = writeln!(
            out,
            "    dyn{i} [label=\"dyn\",{}];",
            dot_node_attrs(Class::Context)
        );
        let _ = writeln!(
            out,
            "    {} -> dyn{i} [style=dashed,color=\"#d1d5db\"];",
            handles[id]
        );
    }

    out.push('}');
    out.push('\n');
    out
}

/// The ASCII delta marker for a class: `+` added, `-` removed, `~` changed, space context.
fn ascii_marker(class: Class) -> char {
    match class {
        Class::Added => '+',
        Class::Removed => '-',
        Class::Changed => '~',
        Class::Context => ' ',
    }
}

/// Emit a DAG view as cargo-tree-style ASCII text, JS-free and offline.
///
/// Roots are nodes with no in-edge (else all nodes, sorted); each subtree is printed with
/// indentation and `->` branches, every node prefixed by its delta marker. A node already on the
/// current path is marked `(cycle)` and not re-expanded, so cycles terminate; a node expanded under
/// another root is not re-expanded either. A flat sorted `from -> to` edge list follows. Only the
/// Calls (sequence) view is not graph-shaped, so it alone degrades to a one-line note.
fn ascii_view(view: View, head: &Graph, changes: &[Change], opts: &RenderOpts) -> String {
    // select_graph is the single source of truth for view support: it returns None only for the
    // sequence (Calls) view, which is not graph-shaped. Every graph-shaped view renders here, so
    // the four generic formats can never disagree on which views they support.
    let Some(gv) = select_graph(head, changes, opts, view) else {
        return String::from(
            "%% ascii format not supported for the sequence view; use --format mermaid\n",
        );
    };

    if gv.sel.render_ids.is_empty() {
        return String::from("%% no structural changes in this view\n");
    }

    let render: BTreeSet<&StableId> = gv.sel.render_ids.iter().copied().collect();

    // Directed successors and in-degree over the kept edges (deduplicated, sorted for determinism).
    let mut succ: BTreeMap<&StableId, Vec<&StableId>> = BTreeMap::new();
    let mut indeg: BTreeMap<&StableId, usize> =
        gv.sel.render_ids.iter().map(|id| (*id, 0usize)).collect();
    for (from, to, _) in &gv.edges {
        if render.contains(from) && render.contains(to) {
            succ.entry(from).or_default().push(to);
            if let Some(d) = indeg.get_mut(to) {
                *d += 1;
            }
        }
    }
    for children in succ.values_mut() {
        children.sort();
        children.dedup();
    }

    // Roots: in-degree-zero nodes in id order, else every node (a fully cyclic graph).
    let mut roots: Vec<&StableId> = gv
        .sel
        .render_ids
        .iter()
        .copied()
        .filter(|id| indeg.get(id).copied() == Some(0))
        .collect();
    if roots.is_empty() {
        roots = gv.sel.render_ids.clone();
    }

    let mut out = String::new();
    let mut visited: BTreeSet<&StableId> = BTreeSet::new();
    let mut path: Vec<&StableId> = Vec::new();
    for root in roots {
        if visited.contains(root) {
            continue;
        }
        ascii_walk(root, 0, &succ, &gv, &mut visited, &mut path, &mut out);
    }

    // Flat sorted edge list (edges already in (from, to) order).
    for (from, to, _) in &gv.edges {
        if render.contains(from) && render.contains(to) {
            let _ = writeln!(out, "{} -> {}", from.as_str(), to.as_str());
        }
    }

    // Honest dyn ceiling: annotate each rendered fn that has dynamic/generic callees with no static
    // target, in id order, so the ASCII text states the calls exist instead of silently omitting them.
    for (id, count) in &gv.unresolved {
        let _ = writeln!(out, "{} : {count} dynamic call(s) not shown", id.as_str());
    }

    out
}

/// Print one node and its subtree (see [`ascii_view`]). A node on the current `path` is a cycle:
/// marked and not recursed. A node already `visited` under another root is printed but not
/// re-expanded, so the walk always terminates.
fn ascii_walk<'a>(
    id: &'a StableId,
    depth: usize,
    succ: &BTreeMap<&'a StableId, Vec<&'a StableId>>,
    gv: &GraphView<'a>,
    visited: &mut BTreeSet<&'a StableId>,
    path: &mut Vec<&'a StableId>,
    out: &mut String,
) {
    let (label, class) = gv.sel.label_class(id, &gv.node_class);
    let mut line = String::new();
    line.push(ascii_marker(class));
    line.push(' ');
    for _ in 0..depth {
        line.push_str("    ");
    }
    if depth > 0 {
        line.push_str("-> ");
    }
    line.push_str(&label);

    let is_cycle = path.contains(&id);
    if is_cycle {
        line.push_str(" (cycle)");
    }
    let _ = writeln!(out, "{line}");
    if is_cycle || !visited.insert(id) {
        return;
    }

    path.push(id);
    if let Some(children) = succ.get(id) {
        for child in children {
            ascii_walk(child, depth + 1, succ, gv, visited, path, out);
        }
    }
    path.pop();
}

/// Emit a DAG view as a native layered ASCII boxes-and-arrows diagram (see [`boxes`]).
///
/// Reuses the shared [`select_graph`] selection, then hands the delta-classed nodes and edges to the
/// pure [`boxes::layout`] layouter. Supported for the DAG views [`View::Modules`], [`View::Types`],
/// [`View::CallGraph`] and the flattened [`View::Overview`] (each item's label is prefixed with its
/// module). The spatial [`View::States`], sequence [`View::Calls`] and [`View::Schema`] views have no
/// boxes form and degrade to a one-line note. Node names are the short id (last `::segment`); a
/// scope stub is suffixed ` (external)`. Output is deterministic.
fn boxes_view(view: View, head: &Graph, changes: &[Change], opts: &RenderOpts) -> String {
    // One source of truth (select_graph's None) decides view support; only the sequence (Calls)
    // view is not graph-shaped. See ascii_view for the full rationale.
    let Some(gv) = select_graph(head, changes, opts, view) else {
        return String::from(
            "%% boxes format not supported for the sequence view; use --format mermaid\n",
        );
    };

    if gv.sel.render_ids.is_empty() {
        return String::from("%% no structural changes in this view\n");
    }

    let overview = matches!(view, View::Overview);

    // Render-id order is the node order; edges reference these indices.
    let index: BTreeMap<&StableId, usize> = gv
        .sel
        .render_ids
        .iter()
        .enumerate()
        .map(|(i, id)| (*id, i))
        .collect();

    let mut nodes: Vec<boxes::BoxNode> = Vec::with_capacity(gv.sel.render_ids.len());
    for id in &gv.sel.render_ids {
        let is_stub = gv.sel.stubs.contains(id);
        let class = if is_stub {
            Class::Context
        } else {
            gv.node_class.get(id).copied().unwrap_or(Class::Context)
        };
        // Overview flattens the nested module subgraphs, so each item carries its module as a prefix.
        let base = if overview {
            format!(
                "{}::{}",
                call_name(call_owner(id.as_str())),
                call_name(id.as_str())
            )
        } else {
            call_name(id.as_str()).to_string()
        };
        let name = if is_stub {
            format!("{base} (external)")
        } else {
            base
        };
        nodes.push(boxes::BoxNode {
            marker: ascii_marker(class),
            name,
        });
    }

    let mut edges: Vec<(usize, usize)> = Vec::new();
    for (from, to, _) in &gv.edges {
        if let (Some(&f), Some(&t)) = (index.get(from), index.get(to)) {
            edges.push((f, t));
        }
    }

    // Honest dyn ceiling: one `dyn` box per rendered fn with unresolved dynamic/generic callees, so
    // the layered diagram shows the dispatch that has no static target instead of dropping it.
    for (id, _count) in &gv.unresolved {
        if let Some(&f) = index.get(id) {
            let dyn_idx = nodes.len();
            nodes.push(boxes::BoxNode {
                marker: ' ',
                name: "dyn".to_string(),
            });
            edges.push((f, dyn_idx));
        }
    }

    boxes::layout(&nodes, &edges)
}

/// The (stroke, fill) `#rrggbb` colours for a delta class, matching the CLASS_DEFS convention.
fn class_svg_colours(class: Class) -> (&'static str, &'static str) {
    match class {
        Class::Added => ("#22c55e", "#f0fdf4"),
        Class::Removed => ("#ef4444", "#fef2f2"),
        Class::Changed => ("#f59e0b", "#fffbeb"),
        Class::Context => ("#d1d5db", "#ffffff"),
    }
}

/// Render a graph view as an SVG image via the pure-Rust layout crate (no external tools).
fn svg_view(view: View, head: &Graph, changes: &[Change], opts: &RenderOpts) -> String {
    // One source of truth (select_graph's None) decides view support; only the sequence (Calls)
    // view is not graph-shaped. See ascii_view for the full rationale.
    let Some(gv) = select_graph(head, changes, opts, view) else {
        return String::from(
            "<!-- svg not supported for the sequence view; use --format mermaid -->\n",
        );
    };
    if gv.sel.render_ids.is_empty() {
        return String::from("<!-- no structural changes in this view -->\n");
    }

    let overview = matches!(view, View::Overview);
    let index: BTreeMap<&StableId, usize> = gv
        .sel
        .render_ids
        .iter()
        .enumerate()
        .map(|(i, id)| (*id, i))
        .collect();

    let mut nodes: Vec<svg::SvgNode> = Vec::with_capacity(gv.sel.render_ids.len());
    for id in &gv.sel.render_ids {
        let is_stub = gv.sel.stubs.contains(id);
        let class = if is_stub {
            Class::Context
        } else {
            gv.node_class.get(id).copied().unwrap_or(Class::Context)
        };
        let base = if overview {
            format!(
                "{}::{}",
                call_name(call_owner(id.as_str())),
                call_name(id.as_str())
            )
        } else {
            call_name(id.as_str()).to_string()
        };
        let label = if is_stub {
            format!("{base} (external)")
        } else {
            base
        };
        let (stroke, fill) = class_svg_colours(class);
        nodes.push(svg::SvgNode {
            label,
            stroke: stroke.to_string(),
            fill: fill.to_string(),
        });
    }

    let mut edges: Vec<(usize, usize)> = Vec::new();
    for (from, to, _) in &gv.edges {
        if let (Some(&f), Some(&t)) = (index.get(from), index.get(to)) {
            edges.push((f, t));
        }
    }

    // Honest dyn ceiling: one `dyn` context box per rendered fn with unresolved dynamic/generic
    // callees, so the SVG shows the ceiling instead of silently omitting it.
    for (id, _count) in &gv.unresolved {
        if let Some(&f) = index.get(id) {
            let dyn_idx = nodes.len();
            let (stroke, fill) = class_svg_colours(Class::Context);
            nodes.push(svg::SvgNode {
                label: "dyn".to_string(),
                stroke: stroke.to_string(),
                fill: fill.to_string(),
            });
            edges.push((f, dyn_idx));
        }
    }

    svg::layout_svg(&nodes, &edges)
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
            member_types: BTreeMap::new(),
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
        assert!(out.contains("%% no state machines (no enum has transitions)"));
        // Nothing was changed, so no state lines are drawn.
        assert!(!out.contains(":::"));
        assert!(!out.contains("state \""));
    }

    #[test]
    fn state_view_excludes_enums_without_transitions() {
        // A data enum (no transition) alongside a real state machine (one transition).
        let head = Graph {
            nodes: vec![
                variant("m::Data::A"),
                variant("m::Data::B"),
                variant("m::Sm::Open"),
                variant("m::Sm::Closed"),
            ],
            edges: vec![transition("m::Sm::Open", "m::Sm::Closed")],
        };
        let changes = vec![
            Change::Added(variant("m::Data::A")),
            Change::Added(variant("m::Data::B")),
            Change::Added(variant("m::Sm::Open")),
            Change::Added(variant("m::Sm::Closed")),
            Change::EdgeAdded(transition("m::Sm::Open", "m::Sm::Closed")),
        ];
        let out = render_state_view(&head, &changes);
        assert!(
            out.contains("m::Sm::Open") && out.contains("m::Sm::Closed"),
            "the real state machine must render:\n{out}"
        );
        assert!(
            !out.contains("m::Data"),
            "a data enum with no transitions must be excluded:\n{out}"
        );
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

    /// A module node whose source path is `file`, for scope tests.
    fn module_at(id: &str, file: &str) -> Node {
        let mut n = module(id);
        n.span.file = file.into();
        n
    }

    #[test]
    fn full_renders_every_node_unchanged_as_context() {
        // a added; chain a->b->c->d. Default prunes d (3 hops); full draws every node.
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

        let full = render(
            View::Modules,
            &head,
            &changes,
            &RenderOpts {
                full: true,
                ..RenderOpts::default()
            },
        );
        assert!(full.contains("[\"crate::a\"]:::added"), "{full}");
        assert!(full.contains("[\"crate::b\"]:::context"), "{full}");
        assert!(full.contains("[\"crate::c\"]:::context"), "{full}");
        // full disables pruning, so the 3-hop node is kept as context.
        assert!(full.contains("[\"crate::d\"]:::context"), "{full}");

        // Same graph, default opts: the classic 2-hop prune still drops d.
        let pruned = render(View::Modules, &head, &changes, &RenderOpts::default());
        assert!(
            !pruned.contains("crate::d"),
            "default must prune d:\n{pruned}"
        );
    }

    #[test]
    fn scope_filters_nodes_stubs_crossing_edges_and_drops_outside() {
        // in1,in2 live in the scoped file; out1,out2 outside. in1->in2 stays, in1->out1 crosses
        // (out1 -> stub), out1->out2 is wholly outside (dropped).
        let head = Graph {
            nodes: vec![
                module_at("crate::in1", "src/focus.rs"),
                module_at("crate::in2", "src/focus.rs"),
                module_at("crate::out1", "src/other.rs"),
                module_at("crate::out2", "src/other.rs"),
            ],
            edges: vec![
                uses("crate::in1", "crate::in2"),
                uses("crate::in1", "crate::out1"),
                uses("crate::out1", "crate::out2"),
            ],
        };
        let out = render(
            View::Modules,
            &head,
            &[],
            &RenderOpts {
                full: true,
                scope: vec!["src/focus.rs".to_string()],
                ..RenderOpts::default()
            },
        );
        assert!(out.contains("[\"crate::in1\"]"), "{out}");
        assert!(out.contains("[\"crate::in2\"]"), "{out}");
        // The crossing endpoint renders as a collapsed external stub.
        assert!(
            out.contains("[\"crate::out1 (external)\"]:::context"),
            "crossing endpoint must be an external stub:\n{out}"
        );
        // The wholly-outside node is never drawn.
        assert!(
            !out.contains("crate::out2"),
            "outside node must drop:\n{out}"
        );
    }

    #[test]
    fn full_scope_golden() {
        // a (scoped, Modified) uses b (out of scope). Full+scope: a coloured changed, b a stub.
        let head = Graph {
            nodes: vec![
                module_at("crate::a", "src/focus.rs"),
                module_at("crate::b", "src/other.rs"),
            ],
            edges: vec![uses("crate::a", "crate::b")],
        };
        let changes = vec![Change::Modified {
            before: module_at("crate::a", "src/focus.rs"),
            after: module_at("crate::a", "src/focus.rs"),
        }];
        let out = render(
            View::Modules,
            &head,
            &changes,
            &RenderOpts {
                full: true,
                scope: vec!["src/focus.rs".to_string()],
                ..RenderOpts::default()
            },
        );
        let expected = concat!(
            "flowchart LR\n",
            "    classDef added fill:#f0fdf4,stroke:#22c55e,stroke-width:2px\n",
            "    classDef removed fill:#fef2f2,stroke:#ef4444,stroke-width:2px,stroke-dasharray:4 4\n",
            "    classDef changed fill:#fffbeb,stroke:#f59e0b,stroke-width:2px\n",
            "    classDef context fill:#ffffff,stroke:#d1d5db,color:#9ca3af\n",
            "    n0[\"crate::a\"]:::changed\n",
            "    n1[\"crate::b (external)\"]:::context\n",
            "    n0 --> n1\n",
        );
        assert_eq!(out, expected);
    }

    #[test]
    fn scope_composes_with_the_call_view() {
        // run (scoped) calls helper (out of scope): the out-of-scope callee is not expanded.
        let head = Graph {
            nodes: vec![
                {
                    let mut n = fn_node("crate::App::run");
                    n.span.file = "src/focus.rs".into();
                    n
                },
                {
                    let mut n = fn_node("crate::helper");
                    n.span.file = "src/other.rs".into();
                    n
                },
                {
                    let mut n = fn_node("crate::helper_deep");
                    n.span.file = "src/other.rs".into();
                    n
                },
            ],
            edges: vec![
                calls("crate::App::run", "crate::helper", 0),
                calls("crate::helper", "crate::helper_deep", 0),
            ],
        };
        let out = render(
            View::Calls,
            &head,
            &[],
            &RenderOpts {
                scope: vec!["src/focus.rs".to_string()],
                entry: Some(StableId::new("crate::App::run")),
                ..RenderOpts::default()
            },
        );
        // run's crossing call is shown, but the out-of-scope callee's own calls are not expanded.
        assert!(out.contains("p0 ->> p1 : helper"), "{out}");
        assert!(
            !out.contains("helper_deep"),
            "far side must not expand:\n{out}"
        );
    }

    fn table_node(id: &str, columns: &str) -> Node {
        let mut n = Node {
            id: StableId::new(id),
            kind: NodeKind::Table,
            span: span(),
            attrs: BTreeMap::new(),
            fingerprint: None,
        };
        n.attrs.insert("columns".into(), columns.into());
        n
    }

    fn foreign_key(from: &str, to: &str) -> Edge {
        Edge {
            from: StableId::new(from),
            to: StableId::new(to),
            kind: EdgeKind::ForeignKey,
            span: span(),
            ordinal: None,
        }
    }

    #[test]
    fn call_graph_two_modules_two_subgraphs_golden() {
        // f (crate::a) calls g (crate::b): two boxes in two subgraphs plus an arrow. `full` renders
        // both even without a delta.
        let head = Graph {
            nodes: vec![fn_node("crate::a::f"), fn_node("crate::b::g")],
            edges: vec![calls("crate::a::f", "crate::b::g", 0)],
        };
        let out = render(
            View::CallGraph,
            &head,
            &[],
            &RenderOpts {
                full: true,
                ..RenderOpts::default()
            },
        );
        let expected = concat!(
            "flowchart LR\n",
            "    classDef added fill:#f0fdf4,stroke:#22c55e,stroke-width:2px\n",
            "    classDef removed fill:#fef2f2,stroke:#ef4444,stroke-width:2px,stroke-dasharray:4 4\n",
            "    classDef changed fill:#fffbeb,stroke:#f59e0b,stroke-width:2px\n",
            "    classDef context fill:#ffffff,stroke:#d1d5db,color:#9ca3af\n",
            "    subgraph sg0[\"crate::a\"]\n",
            "        f0[\"f\"]:::context\n",
            "    end\n",
            "    subgraph sg1[\"crate::b\"]\n",
            "        f1[\"g\"]:::context\n",
            "    end\n",
            "    f0 --> f1\n",
        );
        assert_eq!(out, expected);
    }

    #[test]
    fn call_graph_added_call_is_coloured() {
        // A newly added call between two existing fns: the arrow carries the `+` marker and the
        // endpoints are pulled in by the prune (no `full` needed).
        let head = Graph {
            nodes: vec![fn_node("crate::a::f"), fn_node("crate::b::g")],
            edges: vec![calls("crate::a::f", "crate::b::g", 0)],
        };
        let changes = vec![Change::EdgeAdded(calls("crate::a::f", "crate::b::g", 0))];
        let out = render(View::CallGraph, &head, &changes, &RenderOpts::default());
        assert!(
            out.contains("    f0 -->|+| f1\n"),
            "added arrow missing:\n{out}"
        );
        assert!(out.contains("f0[\"f\"]:::context"), "{out}");
        assert!(out.contains("f1[\"g\"]:::context"), "{out}");
    }

    #[test]
    fn call_graph_flat_same_module_not_collapsed() {
        // Two free functions in the SAME module with a call between them. The sequence view would
        // collapse this to one lifeline (p0 ->> p0); the call graph keeps two distinct boxes.
        let head = Graph {
            nodes: vec![fn_node("crate::m::a"), fn_node("crate::m::b")],
            edges: vec![calls("crate::m::a", "crate::m::b", 0)],
        };
        let out = render(
            View::CallGraph,
            &head,
            &[],
            &RenderOpts {
                full: true,
                ..RenderOpts::default()
            },
        );
        // One subgraph, but two separate boxes and a real arrow between them.
        assert!(out.contains("    subgraph sg0[\"crate::m\"]\n"), "{out}");
        assert!(out.contains("        f0[\"a\"]:::context\n"), "{out}");
        assert!(out.contains("        f1[\"b\"]:::context\n"), "{out}");
        assert!(
            out.contains("    f0 --> f1\n"),
            "distinct boxes must connect:\n{out}"
        );
        // Not collapsed: no self-referential single-node shape.
        assert!(
            !out.contains("f0 --> f0"),
            "must not collapse to one node:\n{out}"
        );
    }

    #[test]
    fn call_graph_dyn_stub_for_unresolved_calls() {
        // A fn with unresolved (dynamic) callees gets a dashed edge to a `dyn` context stub.
        let head = Graph {
            nodes: vec![fn_node_dyn("crate::a::f", 2)],
            edges: vec![],
        };
        let out = render(
            View::CallGraph,
            &head,
            &[],
            &RenderOpts {
                full: true,
                ..RenderOpts::default()
            },
        );
        assert!(out.contains("    dyn0[\"dyn\"]:::context\n"), "{out}");
        assert!(
            out.contains("    f0 -.-> dyn0\n"),
            "dyn edge missing:\n{out}"
        );
    }

    /// A fn with two dynamic callees, drawn full so it always renders. Reused by the per-format
    /// dyn-ceiling honesty tests below.
    fn dyn_graph() -> Graph {
        Graph {
            nodes: vec![fn_node_dyn("crate::a::f", 2)],
            edges: vec![],
        }
    }

    fn render_full(view: View, format: Format, head: &Graph) -> String {
        render(
            view,
            head,
            &[],
            &RenderOpts {
                full: true,
                format,
                ..RenderOpts::default()
            },
        )
    }

    #[test]
    fn call_graph_dot_surfaces_dyn_ceiling() {
        let out = render_full(View::CallGraph, Format::Dot, &dyn_graph());
        assert!(
            out.contains("dyn0 [label=\"dyn\""),
            "dot dyn node missing:\n{out}"
        );
        assert!(
            out.contains("n0 -> dyn0 [style=dashed"),
            "dot dyn edge missing:\n{out}"
        );
    }

    #[test]
    fn call_graph_ascii_surfaces_dyn_ceiling() {
        let out = render_full(View::CallGraph, Format::Ascii, &dyn_graph());
        assert!(
            out.contains("crate::a::f : 2 dynamic call(s) not shown"),
            "ascii dyn note missing:\n{out}"
        );
    }

    #[test]
    fn call_graph_boxes_surfaces_dyn_ceiling() {
        let out = render_full(View::CallGraph, Format::Boxes, &dyn_graph());
        assert!(out.contains("dyn"), "boxes dyn box missing:\n{out}");
    }

    #[test]
    fn call_graph_svg_surfaces_dyn_ceiling() {
        let out = render_full(View::CallGraph, Format::Svg, &dyn_graph());
        assert!(out.contains("dyn"), "svg dyn box missing:\n{out}");
    }

    #[test]
    fn overview_mermaid_surfaces_dyn_ceiling() {
        let out = render_full(View::Overview, Format::Mermaid, &dyn_graph());
        assert!(
            out.contains("dyn0[\"dyn\"]:::context"),
            "overview mermaid dyn stub missing:\n{out}"
        );
        assert!(
            out.contains("-.-> dyn0"),
            "overview mermaid dyn edge missing:\n{out}"
        );
    }

    #[test]
    fn overview_dot_surfaces_dyn_ceiling() {
        let out = render_full(View::Overview, Format::Dot, &dyn_graph());
        assert!(
            out.contains("dyn0 [label=\"dyn\"") && out.contains("-> dyn0 [style=dashed"),
            "overview dot dyn stub missing:\n{out}"
        );
    }

    #[test]
    fn overview_boxes_surfaces_dyn_ceiling() {
        let out = render_full(View::Overview, Format::Boxes, &dyn_graph());
        assert!(
            out.contains("dyn"),
            "overview boxes dyn box missing:\n{out}"
        );
    }

    #[test]
    fn overview_svg_surfaces_dyn_ceiling() {
        let out = render_full(View::Overview, Format::Svg, &dyn_graph());
        assert!(out.contains("dyn"), "overview svg dyn box missing:\n{out}");
    }

    #[test]
    fn zero_unresolved_draws_no_dyn_signal() {
        // A fn with a resolved call and no unresolved attr: no format may invent a dyn signal.
        let head = Graph {
            nodes: vec![fn_node("crate::a::f"), fn_node("crate::b::g")],
            edges: vec![calls("crate::a::f", "crate::b::g", 0)],
        };
        for format in [Format::Dot, Format::Ascii, Format::Boxes, Format::Svg] {
            let out = render_full(View::CallGraph, format, &head);
            assert!(
                !out.contains("dyn"),
                "unexpected dyn signal for a resolved graph ({format:?}):\n{out}"
            );
        }
    }

    #[test]
    fn type_view_lists_added_and_removed_methods() {
        // Struct S (fields unchanged) gains method `foo` and loses method `bar`; a plain method
        // `baz` stays. Fields still render; methods carry +/-/() markers.
        let fp = fingerprint(&["id"], &[("u64", 1)]);
        let head = Graph {
            nodes: vec![
                type_node("crate::S", NodeKind::Struct, Some(fp.clone())),
                fn_node("crate::S::foo"),
                fn_node("crate::S::baz"),
            ],
            edges: vec![],
        };
        let changes = vec![
            Change::Modified {
                before: type_node("crate::S", NodeKind::Struct, Some(fp.clone())),
                after: type_node("crate::S", NodeKind::Struct, Some(fp)),
            },
            Change::Added(fn_node("crate::S::foo")),
            Change::Removed(fn_node("crate::S::bar")),
        ];
        let out = render_type_view(&head, &changes);
        let expected = concat!(
            "classDiagram\n",
            "    classDef added fill:#f0fdf4,stroke:#22c55e,stroke-width:2px\n",
            "    classDef removed fill:#fef2f2,stroke:#ef4444,stroke-width:2px,stroke-dasharray:4 4\n",
            "    classDef changed fill:#fffbeb,stroke:#f59e0b,stroke-width:2px\n",
            "    classDef context fill:#ffffff,stroke:#d1d5db,color:#9ca3af\n",
            "    class c0[\"crate::S\"] {\n",
            "        id\n",
            "        -bar()\n",
            "        baz()\n",
            "        +foo()\n",
            "    }\n",
            "    class c0:::changed\n",
        );
        assert_eq!(out, expected);
    }

    #[test]
    fn schema_two_tables_one_fk_golden() {
        // Two tables and a foreign key; `full` renders the whole schema with no delta markers.
        let head = Graph {
            nodes: vec![
                table_node("users", "id: Int4, name: Varchar"),
                table_node("posts", "id: Int4, user_id: Int4"),
            ],
            edges: vec![foreign_key("posts", "users")],
        };
        let out = render(
            View::Schema,
            &head,
            &[],
            &RenderOpts {
                full: true,
                ..RenderOpts::default()
            },
        );
        let expected = concat!(
            "erDiagram\n",
            "    posts {\n",
            "        Int4 id\n",
            "        Int4 user_id\n",
            "    }\n",
            "    users {\n",
            "        Int4 id\n",
            "        Varchar name\n",
            "    }\n",
            "    posts }o--|| users : \"references\"\n",
        );
        assert_eq!(out, expected);
    }

    #[test]
    fn schema_empty_renders_no_tables_comment() {
        let head = Graph {
            nodes: vec![],
            edges: vec![],
        };
        let out = render(View::Schema, &head, &[], &RenderOpts::default());
        assert_eq!(out, "erDiagram\n    %% no schema tables\n");
    }

    #[test]
    fn schema_added_table_and_fk_carry_text_markers() {
        // erDiagram cannot colour, so an added table gets a `csd_delta added` row and an added FK a
        // `(added)` label suffix.
        let head = Graph {
            nodes: vec![
                table_node("users", "id: Int4"),
                table_node("posts", "id: Int4, user_id: Int4"),
            ],
            edges: vec![foreign_key("posts", "users")],
        };
        let changes = vec![
            Change::Added(table_node("posts", "id: Int4, user_id: Int4")),
            Change::EdgeAdded(foreign_key("posts", "users")),
        ];
        let out = render(View::Schema, &head, &changes, &RenderOpts::default());
        assert!(
            out.contains("        csd_delta added\n"),
            "table marker missing:\n{out}"
        );
        assert!(
            out.contains("posts }o--|| users : \"references (added)\"\n"),
            "fk marker missing:\n{out}"
        );
    }

    #[test]
    fn dot_module_delta_golden() {
        // a added, an added a->b edge (green), and a removed c recovered from the delta (dashed).
        let head = Graph {
            nodes: vec![module("crate::a"), module("crate::b")],
            edges: vec![uses("crate::a", "crate::b")],
        };
        let changes = vec![
            Change::Added(module("crate::a")),
            Change::EdgeAdded(uses("crate::a", "crate::b")),
            Change::Removed(module("crate::c")),
        ];
        let out = render(
            View::Modules,
            &head,
            &changes,
            &RenderOpts {
                format: Format::Dot,
                ..RenderOpts::default()
            },
        );
        let expected = concat!(
            "digraph {\n",
            "    n0 [label=\"crate::a\",color=\"#22c55e\",style=filled,fillcolor=\"#f0fdf4\"];\n",
            "    n1 [label=\"crate::b\",color=\"#d1d5db\"];\n",
            "    n2 [label=\"crate::c\",color=\"#ef4444\",style=\"filled,dashed\",fillcolor=\"#fef2f2\"];\n",
            "    n0 -> n1 [color=\"#22c55e\"];\n",
            "}\n",
        );
        assert_eq!(out, expected);
    }

    #[test]
    fn dot_sequence_view_not_supported() {
        let head = Graph {
            nodes: vec![fn_node("crate::A::f")],
            edges: vec![],
        };
        let out = render(
            View::Calls,
            &head,
            &[],
            &RenderOpts {
                format: Format::Dot,
                entry: Some(StableId::new("crate::A::f")),
                ..RenderOpts::default()
            },
        );
        assert_eq!(
            out,
            "// dot format not supported for the sequence view; use --format mermaid\n"
        );
    }

    #[test]
    fn ascii_module_tree_with_cycle() {
        // a -> b -> a: a added, b removed. The back edge to the ancestor a is marked (cycle), so the
        // walk terminates; a flat sorted edge list follows.
        let head = Graph {
            nodes: vec![module("crate::a"), module("crate::b")],
            edges: vec![uses("crate::a", "crate::b"), uses("crate::b", "crate::a")],
        };
        let changes = vec![
            Change::Added(module("crate::a")),
            Change::Removed(module("crate::b")),
        ];
        let out = render(
            View::Modules,
            &head,
            &changes,
            &RenderOpts {
                full: true,
                format: Format::Ascii,
                ..RenderOpts::default()
            },
        );
        let expected = concat!(
            "+ crate::a\n",
            "-     -> crate::b\n",
            "+         -> crate::a (cycle)\n",
            "crate::a -> crate::b\n",
            "crate::b -> crate::a\n",
        );
        assert_eq!(out, expected);
    }

    /// A type node whose source path is `file`, for overview scope tests.
    fn type_node_at(id: &str, kind: NodeKind, file: &str) -> Node {
        let mut n = type_node(id, kind, None);
        n.span.file = file.into();
        n
    }

    #[test]
    fn overview_delta_places_items_in_module_and_colours() {
        // base: module m with struct A and fn f.
        // head: adds struct B, removes fn f, changes A, adds a Calls edge and an Implements edge.
        let before_a = fingerprint(&["id: u64"], &[("u64", 1)]);
        let after_a = fingerprint(&["id: u64", "extra: u8"], &[("u64", 1), ("u8", 1)]);
        let head = Graph {
            nodes: vec![
                type_node("m::A", NodeKind::Struct, Some(after_a.clone())),
                type_node("m::B", NodeKind::Struct, None),
                type_node("m::T", NodeKind::Trait, None),
                fn_node("m::g"),
                fn_node("m::h"),
            ],
            edges: vec![
                relation("m::A", "m::T", EdgeKind::Implements),
                calls("m::g", "m::h", 0),
            ],
        };
        let changes = vec![
            Change::Added(type_node("m::B", NodeKind::Struct, None)),
            Change::Removed(fn_node("m::f")),
            Change::Modified {
                before: type_node("m::A", NodeKind::Struct, Some(before_a)),
                after: type_node("m::A", NodeKind::Struct, Some(after_a)),
            },
            Change::EdgeAdded(relation("m::A", "m::T", EdgeKind::Implements)),
            Change::EdgeAdded(calls("m::g", "m::h", 0)),
        ];
        let out = render(View::Overview, &head, &changes, &RenderOpts::default());
        // Every item of module m lives inside the single `m` subgraph.
        assert!(out.contains("    subgraph sg0[\"m\"]\n"), "{out}");
        assert_eq!(
            out.matches("subgraph ").count(),
            1,
            "one module box:\n{out}"
        );
        // Delta colours match the other views: changed amber, added green, removed red.
        assert!(out.contains("[\"A\"]:::changed"), "A changed:\n{out}");
        assert!(out.contains("[\"B\"]:::added"), "B added:\n{out}");
        // f is a Fn (rounded box) recovered from the delta and coloured removed.
        assert!(out.contains("(\"f\"):::removed"), "f removed:\n{out}");
        // The added edges are shown, disambiguated by kind and carrying the `+` delta marker.
        assert!(out.contains("-->|impl +|"), "added impl edge:\n{out}");
        assert!(out.contains("-->|calls +|"), "added calls edge:\n{out}");
    }

    #[test]
    fn overview_full_groups_all_items_by_module() {
        // Empty delta, full=true: every item renders as context, grouped into per-module subgraphs.
        let head = Graph {
            nodes: vec![
                type_node("a::S", NodeKind::Struct, None),
                fn_node("a::f"),
                type_node("b::T", NodeKind::Trait, None),
            ],
            edges: vec![relation("a::S", "b::T", EdgeKind::Implements)],
        };
        let out = render(
            View::Overview,
            &head,
            &[],
            &RenderOpts {
                full: true,
                ..RenderOpts::default()
            },
        );
        let expected = concat!(
            "flowchart LR\n",
            "    classDef added fill:#f0fdf4,stroke:#22c55e,stroke-width:2px\n",
            "    classDef removed fill:#fef2f2,stroke:#ef4444,stroke-width:2px,stroke-dasharray:4 4\n",
            "    classDef changed fill:#fffbeb,stroke:#f59e0b,stroke-width:2px\n",
            "    classDef context fill:#ffffff,stroke:#d1d5db,color:#9ca3af\n",
            "    subgraph sg0[\"a\"]\n",
            "        n0[\"S\"]:::context\n",
            "        n1(\"f\"):::context\n",
            "    end\n",
            "    subgraph sg1[\"b\"]\n",
            "        n2[\"T\"]:::context\n",
            "    end\n",
            "    n0 -->|impl| n2\n",
        );
        assert_eq!(out, expected);
    }

    #[test]
    fn overview_scope_narrows_to_one_module_with_stub() {
        // Scope to module a's file: a::S renders in the `a` subgraph, and the out-of-scope b::T on
        // the crossing Implements edge collapses to an external stub in its own subgraph.
        let head = Graph {
            nodes: vec![
                type_node_at("a::S", NodeKind::Struct, "src/a.rs"),
                type_node_at("b::T", NodeKind::Trait, "src/b.rs"),
            ],
            edges: vec![relation("a::S", "b::T", EdgeKind::Implements)],
        };
        let out = render(
            View::Overview,
            &head,
            &[],
            &RenderOpts {
                full: true,
                scope: vec!["src/a.rs".to_string()],
                ..RenderOpts::default()
            },
        );
        assert!(out.contains("    subgraph sg0[\"a\"]\n"), "{out}");
        assert!(out.contains("[\"S\"]:::context"), "{out}");
        // The crossing endpoint is a collapsed external stub.
        assert!(
            out.contains("[\"T (external)\"]:::context"),
            "boundary stub missing:\n{out}"
        );
        // The crossing edge stays visible to the stub.
        assert!(
            out.contains("-->|impl| n1"),
            "crossing edge missing:\n{out}"
        );
    }

    #[test]
    fn overview_module_added_removed_marked_in_title() {
        // A wholly-added module box carries an ` (added)` title suffix (flowchart cannot colour the
        // box itself). The item inside is coloured added as usual.
        let head = Graph {
            nodes: vec![type_node("m::A", NodeKind::Struct, None), module("m")],
            edges: vec![],
        };
        let changes = vec![
            Change::Added(module("m")),
            Change::Added(type_node("m::A", NodeKind::Struct, None)),
        ];
        let out = render(View::Overview, &head, &changes, &RenderOpts::default());
        assert!(
            out.contains("    subgraph sg0[\"m (added)\"]\n"),
            "added module title missing:\n{out}"
        );
        assert!(out.contains("[\"A\"]:::added"), "{out}");
    }

    #[test]
    fn overview_empty_delta_renders_no_changes() {
        let head = Graph {
            nodes: vec![type_node("m::A", NodeKind::Struct, None)],
            edges: vec![],
        };
        let out = render(View::Overview, &head, &[], &RenderOpts::default());
        assert!(out.starts_with("flowchart LR\n"));
        assert!(out.contains("%% no structural changes in the overview"));
        assert!(!out.contains(":::"));
    }

    #[test]
    fn overview_full_empty_graph_renders_empty_note() {
        let head = Graph {
            nodes: vec![],
            edges: vec![],
        };
        let out = render(
            View::Overview,
            &head,
            &[],
            &RenderOpts {
                full: true,
                ..RenderOpts::default()
            },
        );
        assert!(out.contains("%% empty overview"), "{out}");
    }

    /// Boxes format over the module view, drawing the whole graph.
    fn boxes_opts() -> RenderOpts {
        RenderOpts {
            full: true,
            format: Format::Boxes,
            ..RenderOpts::default()
        }
    }

    #[test]
    fn boxes_chain_golden() {
        // a -> b -> c: three boxes in three columns, `--->` arrows between them.
        let head = Graph {
            nodes: vec![module("crate::a"), module("crate::b"), module("crate::c")],
            edges: vec![uses("crate::a", "crate::b"), uses("crate::b", "crate::c")],
        };
        let out = render(View::Modules, &head, &[], &boxes_opts());
        let expected = concat!(
            "+----+    +----+    +----+\n",
            "|  a |--->|  b |--->|  c |\n",
            "+----+    +----+    +----+\n",
        );
        assert_eq!(out, expected);
    }

    #[test]
    fn boxes_diamond_three_ranks_golden() {
        // a->b, a->c, b->d, c->d: a is rank 0, b/c rank 1, d rank 2. Adjacent edges are all arrows;
        // the row-crossing edges (a->c, c->d) jog through a vertical channel in the gutter.
        let head = Graph {
            nodes: vec![
                module("crate::a"),
                module("crate::b"),
                module("crate::c"),
                module("crate::d"),
            ],
            edges: vec![
                uses("crate::a", "crate::b"),
                uses("crate::a", "crate::c"),
                uses("crate::b", "crate::d"),
                uses("crate::c", "crate::d"),
            ],
        };
        let out = render(View::Modules, &head, &[], &boxes_opts());
        let expected = concat!(
            "+----+    +----+    +----+\n",
            "|  a |-+->|  b |-+->|  d |\n",
            "+----+ |  +----+ |  +----+\n",
            "       |         |\n",
            "       |  +----+ |\n",
            "       +->|  c |-+\n",
            "          +----+\n",
        );
        assert_eq!(out, expected);
    }

    #[test]
    fn boxes_cycle_back_edge_in_legend() {
        // a -> b -> a: the back-edge is excluded from ranking (so the layout terminates) and listed
        // in the legend. a is added, so its box carries the `+` marker.
        let head = Graph {
            nodes: vec![module("crate::a"), module("crate::b")],
            edges: vec![uses("crate::a", "crate::b"), uses("crate::b", "crate::a")],
        };
        let changes = vec![Change::Added(module("crate::a"))];
        let out = render(View::Modules, &head, &changes, &boxes_opts());
        let expected = concat!(
            "+----+    +----+\n",
            "| +a |--->|  b |\n",
            "+----+    +----+\n",
            "\n",
            "Legend:\n",
            "  b ---> a  (back)\n",
        );
        assert_eq!(out, expected);
    }

    #[test]
    fn boxes_added_node_shows_plus_marker() {
        // An added module renders `+name` inside its box.
        let head = Graph {
            nodes: vec![module("crate::widget")],
            edges: vec![],
        };
        let changes = vec![Change::Added(module("crate::widget"))];
        let out = render(View::Modules, &head, &changes, &boxes_opts());
        assert!(out.contains("| +widget |"), "added marker missing:\n{out}");
    }

    #[test]
    fn boxes_skip_edge_in_legend() {
        // a->b->c plus a->c: the two-rank-spanning a->c edge goes to the legend, not the grid.
        let head = Graph {
            nodes: vec![module("crate::a"), module("crate::b"), module("crate::c")],
            edges: vec![
                uses("crate::a", "crate::b"),
                uses("crate::b", "crate::c"),
                uses("crate::a", "crate::c"),
            ],
        };
        let out = render(View::Modules, &head, &[], &boxes_opts());
        assert!(
            out.contains("  a ---> c  (skip)\n"),
            "skip edge missing from legend:\n{out}"
        );
        // Adjacent edges are still drawn as arrows on the grid.
        assert!(out.contains("--->"), "{out}");
    }

    #[test]
    fn boxes_state_view_renders_the_graph() {
        // Boxes supports every graph-shaped view; a real state machine renders, it is not refused.
        let head = Graph {
            nodes: vec![variant("crate::S::a"), variant("crate::S::b")],
            edges: vec![transition("crate::S::a", "crate::S::b")],
        };
        let out = render(
            View::States,
            &head,
            &[],
            &RenderOpts {
                format: Format::Boxes,
                full: true,
                ..RenderOpts::default()
            },
        );
        assert!(
            !out.contains("not supported"),
            "state view should render in boxes, not be refused:\n{out}"
        );
    }

    #[test]
    fn ascii_state_view_renders_the_graph() {
        // The generic graph formats support every view except the Calls sequence view. A real state
        // machine (variants with a transition) renders as a DAG, it is not refused.
        let head = Graph {
            nodes: vec![variant("crate::S::a"), variant("crate::S::b")],
            edges: vec![transition("crate::S::a", "crate::S::b")],
        };
        let out = render(
            View::States,
            &head,
            &[],
            &RenderOpts {
                format: Format::Ascii,
                full: true,
                ..RenderOpts::default()
            },
        );
        assert!(
            !out.contains("not supported"),
            "state view should render in ascii, not be refused:\n{out}"
        );
        assert!(out.contains('a') && out.contains('b'), "{out}");
    }

    #[test]
    fn all_generic_formats_agree_on_overview() {
        // Regression: ascii once refused Overview while boxes and svg accepted it. Every generic
        // graph format must agree on which views are supported (Overview is supported by all).
        let head = Graph {
            nodes: vec![module("crate::a"), module("crate::b")],
            edges: vec![uses("crate::a", "crate::b")],
        };
        for format in [Format::Dot, Format::Ascii, Format::Boxes, Format::Svg] {
            let out = render(
                View::Overview,
                &head,
                &[],
                &RenderOpts {
                    format,
                    full: true,
                    ..RenderOpts::default()
                },
            );
            assert!(
                !out.contains("not supported"),
                "{format:?} refused Overview but must support it:\n{out}"
            );
        }
    }

    #[test]
    fn ascii_sequence_view_is_the_only_refusal() {
        // Calls is the one non-graph-shaped view; it alone degrades to the note.
        let out = render(
            View::Calls,
            &Graph {
                nodes: vec![],
                edges: vec![],
            },
            &[],
            &RenderOpts {
                format: Format::Ascii,
                ..RenderOpts::default()
            },
        );
        assert_eq!(
            out,
            "%% ascii format not supported for the sequence view; use --format mermaid\n"
        );
    }
}
