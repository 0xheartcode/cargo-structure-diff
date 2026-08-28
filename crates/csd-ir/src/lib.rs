//! The structural graph IR shared by every `csd` view.
//!
//! This crate is the contract the extractor, differ, renderer, and linter all build against. It
//! contains **no language-specific concepts** (see [`NodeKind`]); Rust, and any language added
//! later, projects into these same types.
//!
//! # Determinism
//!
//! Output must be byte-stable so golden tests and rendered diagrams do not churn. Every
//! collection here is either a [`BTreeMap`] or a `Vec` that callers are required to keep sorted
//! via [`Graph::normalize`]. Do not introduce `HashMap` into serialized state.
//!
//! # What is implemented here
//!
//! The data model (this file) is the lead-owned seam and is stable. Structural fingerprints and
//! similarity scoring live in [`fingerprint`]. Stable-id *derivation* (assigning a [`StableId`]
//! that survives a rename, rather than wrapping a caller-provided string) is still deferred to the
//! differ and tracked in the backlog (area `ir`/`diff`).

pub mod fingerprint;

pub use fingerprint::{doc_hash, similarity, similarity_weighted, Fingerprint, SimilarityWeights};

use std::collections::BTreeMap;

/// A stable identity for a node, designed to survive renames and moves.
///
/// Concrete derivation is deferred (backlog `ir`): it will combine a structural fingerprint with
/// a path so that a moved-but-unchanged item keeps its id. For now it is an opaque string so the
/// surrounding types can be built and tested.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct StableId(pub String);

impl StableId {
    /// Wrap a raw identity string.
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// The underlying string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The kind of a node. Language-agnostic on purpose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum NodeKind {
    /// A module / namespace / package.
    Module,
    /// A product type (`struct`, record, class).
    Struct,
    /// A sum type (`enum`).
    Enum,
    /// An interface (`trait`).
    Trait,
    /// A function or method.
    Fn,
    /// An enum variant, used as a state in the state-machine view.
    Variant,
    /// A database table, used by the schema view.
    Table,
}

/// The kind of an edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum EdgeKind {
    /// Module references an item from another module.
    Uses,
    /// `impl Trait for T` realization.
    Implements,
    /// A field or parameter associates one type with another.
    Associates,
    /// A call from one function to another.
    Calls,
    /// A state transition between two enum variants.
    Transitions,
    /// A database foreign key.
    ForeignKey,
}

/// A location in source, used to link nodes and edges back to diff hunks.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SourceSpan {
    /// Repo-relative path.
    pub file: String,
    /// Inclusive start byte offset.
    pub start: u32,
    /// Exclusive end byte offset.
    pub end: u32,
}

/// A node in the structural graph.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Node {
    /// Identity that should survive rename/move.
    pub id: StableId,
    /// What kind of thing this is.
    pub kind: NodeKind,
    /// Where it lives in source.
    pub span: SourceSpan,
    /// Free-form, view-specific attributes (kept sorted by key via `BTreeMap`).
    pub attrs: BTreeMap<String, String>,
    /// Name-independent structural summary used by the differ's rename/move matcher. Set by the
    /// extractor for kinds that have structure worth matching (types, traits, functions); `None`
    /// for kinds where it does not apply (for example modules).
    pub fingerprint: Option<Fingerprint>,
}

/// An edge in the structural graph.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Edge {
    /// Source node id.
    pub from: StableId,
    /// Target node id.
    pub to: StableId,
    /// Relationship kind.
    pub kind: EdgeKind,
    /// Where the relationship is expressed in source.
    pub span: SourceSpan,
    /// Position within an ordered view (call sequences). `None` for unordered views.
    pub ordinal: Option<u32>,
}

/// A whole extracted graph for one git ref.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Graph {
    /// Nodes, kept sorted by id after [`Graph::normalize`].
    pub nodes: Vec<Node>,
    /// Edges, kept sorted after [`Graph::normalize`].
    pub edges: Vec<Edge>,
}

impl Graph {
    /// An empty graph.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sort nodes and edges into a canonical order so downstream diffing and rendering are
    /// deterministic. Idempotent.
    pub fn normalize(&mut self) {
        self.nodes.sort_by(|a, b| a.id.cmp(&b.id));
        self.edges.sort_by(|a, b| {
            (&a.from, &a.to, a.kind, a.ordinal, &a.span)
                .cmp(&(&b.from, &b.to, b.kind, b.ordinal, &b.span))
        });
    }
}

/// One element of a computed delta between two graphs.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Change {
    /// A node present only in the head graph.
    Added(Node),
    /// A node present only in the base graph.
    Removed(Node),
    /// A node whose identity was matched but whose contents changed.
    Modified {
        /// The base-side node.
        before: Node,
        /// The head-side node.
        after: Node,
    },
    /// A node re-parented (for example, a type moved to a different module).
    Moved {
        /// The moved node's id.
        node: StableId,
        /// Previous parent.
        from: StableId,
        /// New parent.
        to: StableId,
    },
    /// An edge present only in the head graph.
    EdgeAdded(Edge),
    /// An edge present only in the base graph.
    EdgeRemoved(Edge),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span() -> SourceSpan {
        SourceSpan {
            file: "src/lib.rs".into(),
            start: 0,
            end: 1,
        }
    }

    fn node(id: &str) -> Node {
        Node {
            id: StableId::new(id),
            kind: NodeKind::Module,
            span: span(),
            attrs: BTreeMap::new(),
            fingerprint: None,
        }
    }

    #[test]
    fn normalize_sorts_nodes_by_id() {
        let mut g = Graph::new();
        g.nodes = vec![node("c"), node("a"), node("b")];
        g.normalize();
        let ids: Vec<_> = g.nodes.iter().map(|n| n.id.as_str()).collect();
        assert_eq!(ids, ["a", "b", "c"]);
    }

    #[test]
    fn normalize_is_idempotent() {
        let mut g = Graph::new();
        g.nodes = vec![node("b"), node("a")];
        g.normalize();
        let once = g.clone();
        g.normalize();
        assert_eq!(g, once);
    }

    #[cfg(feature = "serde")]
    #[test]
    fn graph_survives_a_json_round_trip() {
        let mut g = Graph::new();
        let mut typed = node("a");
        typed.kind = NodeKind::Struct;
        typed.attrs.insert("vis".into(), "pub".into());
        typed.fingerprint = Some(Fingerprint {
            members: ["id", "total"].iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        });
        g.nodes = vec![typed, node("b")];
        g.edges = vec![Edge {
            from: StableId::new("a"),
            to: StableId::new("b"),
            kind: EdgeKind::Uses,
            span: span(),
            ordinal: None,
        }];
        g.normalize();

        let json = serde_json::to_string(&g).expect("serialize");
        let back: Graph = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(
            g, back,
            "the graph must survive a JSON round trip byte-for-byte"
        );
    }
}
