//! Architectural lints over the module graph.
//!
//! Two rules for M1 (SPEC.md sections 3.1 and 7):
//! - `layering`: a `Uses` edge whose source and target layers form a forbidden pair.
//! - `cycles`: a strongly-connected component of the module `Uses` graph (a module cycle).
//!
//! Three rules for M2 over the transition graph (Variant nodes + Transitions edges, SPEC.md 3.4):
//! - `unreachable_state`: a Variant with no incoming and no outgoing transition (isolated).
//! - `terminal_state_without_exit`: a Variant with an incoming but no outgoing transition.
//! - `new_state_cycle`: a cycle among states (a Transitions SCC, or a self-transition).
//!
//! One rule for M4 over the call graph (Calls edges, SPEC.md 3.3):
//! - `io_in_hot_path`: a call from a `calls.hot` function into a `calls.io` module.
//!
//! All are ratchet-aware. Under [`RatchetMode::NewOnly`] only violations introduced by a newly
//! added edge fail, which is what lets a brownfield repo adopt the gate without a red day one.
//! A lint runs only when it appears in the config `deny` or `warn` list, and its findings are
//! tagged with the matching [`Severity`].

use std::collections::{BTreeMap, BTreeSet};

use csd_config::{Config, RatchetMode};
use csd_ir::{Change, EdgeKind, Graph, NodeKind, StableId};

/// Whether a finding fails the build or only reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// Listed in `lint.deny`: fails the build.
    Deny,
    /// Listed in `lint.warn`: reported only.
    Warn,
}

/// One lint violation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// The rule that fired (for example `layering`, `cycles`, `unreachable_state`).
    pub rule: String,
    /// Whether it denies or only warns.
    pub severity: Severity,
    /// Human-readable description.
    pub message: String,
}

/// Run the enabled lints over `head` given the base-to-head `changes` and `config`.
///
/// Findings are returned sorted for determinism. Use [`has_denials`] to derive an exit code.
pub fn lint(head: &Graph, changes: &[Change], config: &Config) -> Vec<Finding> {
    let deny: BTreeSet<&str> = config.lint.deny.iter().map(String::as_str).collect();
    let warn: BTreeSet<&str> = config.lint.warn.iter().map(String::as_str).collect();
    let severity_of = |rule: &str| -> Option<Severity> {
        if deny.contains(rule) {
            Some(Severity::Deny)
        } else if warn.contains(rule) {
            Some(Severity::Warn)
        } else {
            None
        }
    };

    let added = added_edges(changes);
    let added_transitions = added_transition_edges(changes);
    let touched_states = states_touched_by_change(changes);
    let paths = module_paths(head);
    let mut findings = Vec::new();

    if let Some(sev) = severity_of("layering") {
        findings.extend(layering(head, config, &paths, &added, sev));
    }
    if let Some(sev) = severity_of("cycles") {
        findings.extend(cycles(head, config, &added, sev));
    }
    if let Some(sev) = severity_of("unreachable_state") {
        findings.extend(unreachable_state(head, config, &touched_states, sev));
    }
    if let Some(sev) = severity_of("terminal_state_without_exit") {
        findings.extend(terminal_state_without_exit(
            head,
            config,
            &touched_states,
            sev,
        ));
    }
    if let Some(sev) = severity_of("new_state_cycle") {
        findings.extend(new_state_cycle(head, config, &added_transitions, sev));
    }
    if let Some(sev) = severity_of("io_in_hot_path") {
        let added_calls = added_call_edges(changes);
        findings.extend(io_in_hot_path(head, config, &added_calls, sev));
    }

    findings.sort_by(|a, b| (&a.rule, &a.message).cmp(&(&b.rule, &b.message)));
    findings
}

/// True if any finding denies the build.
pub fn has_denials(findings: &[Finding]) -> bool {
    findings.iter().any(|f| f.severity == Severity::Deny)
}

/// The `(from, to)` ids of `Uses` edges added between base and head.
fn added_edges(changes: &[Change]) -> BTreeSet<(&StableId, &StableId)> {
    changes
        .iter()
        .filter_map(|c| match c {
            Change::EdgeAdded(e) if e.kind == EdgeKind::Uses => Some((&e.from, &e.to)),
            _ => None,
        })
        .collect()
}

/// Map each module id to its source path (for layer lookup).
fn module_paths(head: &Graph) -> BTreeMap<&StableId, &str> {
    head.nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Module)
        .map(|n| (&n.id, n.span.file.as_str()))
        .collect()
}

/// Whether the config's ratchet mode restricts a rule to newly added edges.
fn new_only(config: &Config) -> bool {
    config.ratchet.mode == RatchetMode::NewOnly
}

/// Layering: flag a `Uses` edge from layer A to layer B when `{from: A, to: B}` is forbidden.
fn layering(
    head: &Graph,
    config: &Config,
    paths: &BTreeMap<&StableId, &str>,
    added: &BTreeSet<(&StableId, &StableId)>,
    severity: Severity,
) -> Vec<Finding> {
    let restrict = new_only(config);
    let mut findings = Vec::new();
    for e in &head.edges {
        if e.kind != EdgeKind::Uses {
            continue;
        }
        if restrict && !added.contains(&(&e.from, &e.to)) {
            continue;
        }
        let (Some(from_path), Some(to_path)) = (paths.get(&e.from), paths.get(&e.to)) else {
            continue;
        };
        let (Some(from_layer), Some(to_layer)) =
            (config.layer_of(from_path), config.layer_of(to_path))
        else {
            continue;
        };
        let forbidden = config
            .layers
            .forbid
            .iter()
            .any(|r| r.from == from_layer && r.to == to_layer);
        if forbidden {
            findings.push(Finding {
                rule: "layering".to_string(),
                severity,
                message: format!(
                    "{} ({}) must not depend on {} ({})",
                    e.from.as_str(),
                    from_layer,
                    e.to.as_str(),
                    to_layer
                ),
            });
        }
    }
    findings
}

/// Cycles: report each non-trivial strongly-connected component of the module `Uses` graph. Under
/// new-only, a component is reported only when a newly added edge lies inside it.
fn cycles(
    head: &Graph,
    config: &Config,
    added: &BTreeSet<(&StableId, &StableId)>,
    severity: Severity,
) -> Vec<Finding> {
    let restrict = new_only(config);
    let module_ids: BTreeSet<&StableId> = head
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Module)
        .map(|n| &n.id)
        .collect();

    let mut adj: BTreeMap<&StableId, Vec<&StableId>> = BTreeMap::new();
    for e in &head.edges {
        if e.kind == EdgeKind::Uses && module_ids.contains(&e.from) && module_ids.contains(&e.to) {
            adj.entry(&e.from).or_default().push(&e.to);
        }
    }

    let mut findings = Vec::new();
    for scc in strongly_connected(&module_ids, &adj) {
        // A cycle is an SCC of two or more nodes, or a single node with a self-loop.
        let is_cycle = scc.len() > 1
            || scc
                .first()
                .is_some_and(|only| adj.get(only).is_some_and(|ns| ns.contains(only)));
        if !is_cycle {
            continue;
        }
        let members: BTreeSet<&StableId> = scc.iter().copied().collect();
        if restrict {
            let has_new = added
                .iter()
                .any(|(f, t)| members.contains(f) && members.contains(t));
            if !has_new {
                continue;
            }
        }
        let mut names: Vec<&str> = members.iter().map(|id| id.as_str()).collect();
        names.sort_unstable();
        findings.push(Finding {
            rule: "cycles".to_string(),
            severity,
            message: format!("module cycle: {}", names.join(" -> ")),
        });
    }
    findings
}

/// The `(from, to)` ids of `Transitions` edges added between base and head.
fn added_transition_edges(changes: &[Change]) -> BTreeSet<(&StableId, &StableId)> {
    changes
        .iter()
        .filter_map(|c| match c {
            Change::EdgeAdded(e) if e.kind == EdgeKind::Transitions => Some((&e.from, &e.to)),
            _ => None,
        })
        .collect()
}

/// State ids touched by a `Transitions` edge added or removed between base and head. Used to
/// ratchet the structural state rules: under new-only, a state is only flagged when the change
/// added or removed a transition on it, so a pre-existing dead-end does not fail a brownfield repo.
fn states_touched_by_change(changes: &[Change]) -> BTreeSet<&StableId> {
    let mut touched = BTreeSet::new();
    for c in changes {
        let e = match c {
            Change::EdgeAdded(e) | Change::EdgeRemoved(e) if e.kind == EdgeKind::Transitions => e,
            _ => continue,
        };
        touched.insert(&e.from);
        touched.insert(&e.to);
    }
    touched
}

/// The Variant node ids of `head`.
fn variant_ids(head: &Graph) -> BTreeSet<&StableId> {
    head.nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Variant)
        .map(|n| &n.id)
        .collect()
}

/// For the Variant subgraph, the sets of states with at least one incoming and at least one
/// outgoing `Transitions` edge (endpoints restricted to Variant nodes). A self-transition counts
/// as both an incoming and an outgoing edge on its state.
fn transition_endpoints<'a>(
    head: &'a Graph,
    variants: &BTreeSet<&'a StableId>,
) -> (BTreeSet<&'a StableId>, BTreeSet<&'a StableId>) {
    let mut has_incoming = BTreeSet::new();
    let mut has_outgoing = BTreeSet::new();
    for e in &head.edges {
        if e.kind != EdgeKind::Transitions {
            continue;
        }
        if !variants.contains(&e.from) || !variants.contains(&e.to) {
            continue;
        }
        has_outgoing.insert(&e.from);
        has_incoming.insert(&e.to);
    }
    (has_incoming, has_outgoing)
}

/// Unreachable state: a Variant with no incoming and no outgoing `Transitions` edge (an isolated
/// state). Entry is defined pragmatically to keep the rule precision-first: a state with outgoing
/// but no incoming transitions is a plausible entry and is NOT flagged; only a state with neither
/// incoming nor outgoing is unreachable. Under new-only, report one only when the change added or
/// removed a transition touching that state; under all, report every isolated state.
fn unreachable_state(
    head: &Graph,
    config: &Config,
    touched: &BTreeSet<&StableId>,
    severity: Severity,
) -> Vec<Finding> {
    let restrict = new_only(config);
    let variants = variant_ids(head);
    let (has_incoming, has_outgoing) = transition_endpoints(head, &variants);
    let mut findings = Vec::new();
    for id in &variants {
        if has_incoming.contains(id) || has_outgoing.contains(id) {
            continue;
        }
        if restrict && !touched.contains(id) {
            continue;
        }
        findings.push(Finding {
            rule: "unreachable_state".to_string(),
            severity,
            message: format!("unreachable state: {}", id.as_str()),
        });
    }
    findings
}

/// Terminal state without exit: a Variant with at least one incoming `Transitions` edge but no
/// outgoing edge (a dead-end). Under new-only, report one only when the change added or removed a
/// transition touching that state; under all, report every dead-end.
fn terminal_state_without_exit(
    head: &Graph,
    config: &Config,
    touched: &BTreeSet<&StableId>,
    severity: Severity,
) -> Vec<Finding> {
    let restrict = new_only(config);
    let variants = variant_ids(head);
    let (has_incoming, has_outgoing) = transition_endpoints(head, &variants);
    let mut findings = Vec::new();
    for id in &variants {
        if !has_incoming.contains(id) || has_outgoing.contains(id) {
            continue;
        }
        if restrict && !touched.contains(id) {
            continue;
        }
        findings.push(Finding {
            rule: "terminal_state_without_exit".to_string(),
            severity,
            message: format!("terminal state without exit: {}", id.as_str()),
        });
    }
    findings
}

/// State cycles: report each non-trivial strongly-connected component of the `Transitions` graph,
/// or a single state with a self-transition. Under new-only, a component is reported only when a
/// newly added `Transitions` edge lies inside it (mirrors the module `cycles` rule).
fn new_state_cycle(
    head: &Graph,
    config: &Config,
    added: &BTreeSet<(&StableId, &StableId)>,
    severity: Severity,
) -> Vec<Finding> {
    let restrict = new_only(config);
    let variants = variant_ids(head);

    let mut adj: BTreeMap<&StableId, Vec<&StableId>> = BTreeMap::new();
    for e in &head.edges {
        if e.kind == EdgeKind::Transitions && variants.contains(&e.from) && variants.contains(&e.to)
        {
            adj.entry(&e.from).or_default().push(&e.to);
        }
    }

    let mut findings = Vec::new();
    for scc in strongly_connected(&variants, &adj) {
        // A cycle is an SCC of two or more nodes, or a single node with a self-loop.
        let is_cycle = scc.len() > 1
            || scc
                .first()
                .is_some_and(|only| adj.get(only).is_some_and(|ns| ns.contains(only)));
        if !is_cycle {
            continue;
        }
        let members: BTreeSet<&StableId> = scc.iter().copied().collect();
        if restrict {
            let has_new = added
                .iter()
                .any(|(f, t)| members.contains(f) && members.contains(t));
            if !has_new {
                continue;
            }
        }
        let mut names: Vec<&str> = members.iter().map(|id| id.as_str()).collect();
        names.sort_unstable();
        findings.push(Finding {
            rule: "new_state_cycle".to_string(),
            severity,
            message: format!("state cycle: {}", names.join(" -> ")),
        });
    }
    findings
}

/// The `(from, to)` ids of `Calls` edges added between base and head.
fn added_call_edges(changes: &[Change]) -> BTreeSet<(&StableId, &StableId)> {
    changes
        .iter()
        .filter_map(|c| match c {
            Change::EdgeAdded(e) if e.kind == EdgeKind::Calls => Some((&e.from, &e.to)),
            _ => None,
        })
        .collect()
}

/// The owning module id of a function id: the id minus its last `::segment`. Returns `None` when
/// the id has no `::` (no enclosing module to attribute the call to).
fn owning_module(fn_id: &str) -> Option<&str> {
    fn_id.rsplit_once("::").map(|(module, _)| module)
}

/// io_in_hot_path: flag a `Calls` edge whose caller matches a `calls.hot` glob and whose callee's
/// owning module matches a `calls.io` glob. Under new-only, only newly added calls are flagged;
/// under all, any matching call is flagged.
fn io_in_hot_path(
    head: &Graph,
    config: &Config,
    added: &BTreeSet<(&StableId, &StableId)>,
    severity: Severity,
) -> Vec<Finding> {
    let restrict = new_only(config);
    let mut findings = Vec::new();
    for e in &head.edges {
        if e.kind != EdgeKind::Calls {
            continue;
        }
        if restrict && !added.contains(&(&e.from, &e.to)) {
            continue;
        }
        let Some(callee_owner) = owning_module(e.to.as_str()) else {
            continue;
        };
        if config.calls.is_hot(e.from.as_str()) && config.calls.is_io_module(callee_owner) {
            findings.push(Finding {
                rule: "io_in_hot_path".to_string(),
                severity,
                message: format!("{} (hot) calls into {} (io)", e.from.as_str(), callee_owner),
            });
        }
    }
    findings
}

/// Tarjan strongly-connected components over the module subgraph. Returns one Vec per component.
fn strongly_connected<'a>(
    nodes: &BTreeSet<&'a StableId>,
    adj: &BTreeMap<&'a StableId, Vec<&'a StableId>>,
) -> Vec<Vec<&'a StableId>> {
    struct State<'a> {
        index: BTreeMap<&'a StableId, usize>,
        low: BTreeMap<&'a StableId, usize>,
        on_stack: BTreeSet<&'a StableId>,
        stack: Vec<&'a StableId>,
        next: usize,
        out: Vec<Vec<&'a StableId>>,
    }

    fn visit<'a>(
        v: &'a StableId,
        adj: &BTreeMap<&'a StableId, Vec<&'a StableId>>,
        st: &mut State<'a>,
    ) {
        st.index.insert(v, st.next);
        st.low.insert(v, st.next);
        st.next += 1;
        st.stack.push(v);
        st.on_stack.insert(v);

        if let Some(neighbours) = adj.get(v) {
            for &w in neighbours {
                if !st.index.contains_key(w) {
                    visit(w, adj, st);
                    let lw = st.low[w];
                    let lv = st.low[v];
                    st.low.insert(v, lv.min(lw));
                } else if st.on_stack.contains(w) {
                    let iw = st.index[w];
                    let lv = st.low[v];
                    st.low.insert(v, lv.min(iw));
                }
            }
        }

        if st.low[v] == st.index[v] {
            let mut component = Vec::new();
            while let Some(w) = st.stack.pop() {
                st.on_stack.remove(w);
                component.push(w);
                if w == v {
                    break;
                }
            }
            st.out.push(component);
        }
    }

    let mut st = State {
        index: BTreeMap::new(),
        low: BTreeMap::new(),
        on_stack: BTreeSet::new(),
        stack: Vec::new(),
        next: 0,
        out: Vec::new(),
    };
    for &v in nodes {
        if !st.index.contains_key(v) {
            visit(v, adj, &mut st);
        }
    }
    st.out
}

#[cfg(test)]
mod tests {
    use super::*;
    use csd_ir::{Edge, SourceSpan};

    fn cfg(toml: &str) -> Config {
        Config::parse(toml).unwrap()
    }

    fn module(id: &str, file: &str) -> csd_ir::Node {
        csd_ir::Node {
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

    fn uses(from: &str, to: &str) -> Edge {
        Edge {
            from: StableId::new(from),
            to: StableId::new(to),
            kind: EdgeKind::Uses,
            span: SourceSpan {
                file: "x".into(),
                start: 0,
                end: 1,
            },
            ordinal: None,
        }
    }

    fn variant(id: &str) -> csd_ir::Node {
        csd_ir::Node {
            id: StableId::new(id),
            kind: NodeKind::Variant,
            span: SourceSpan {
                file: "src/state.rs".into(),
                start: 0,
                end: 1,
            },
            attrs: BTreeMap::new(),
            fingerprint: None,
        }
    }

    fn transition(from: &str, to: &str) -> Edge {
        Edge {
            from: StableId::new(from),
            to: StableId::new(to),
            kind: EdgeKind::Transitions,
            span: SourceSpan {
                file: "src/state.rs".into(),
                start: 0,
                end: 1,
            },
            ordinal: None,
        }
    }

    const STATES: &str = r#"
[lint]
deny = ["unreachable_state", "terminal_state_without_exit", "new_state_cycle"]
"#;

    const STATES_ALL: &str = r#"
[lint]
deny = ["unreachable_state", "terminal_state_without_exit", "new_state_cycle"]

[ratchet]
mode = "all"
"#;

    const LAYERS: &str = r#"
[layers]
order = ["app", "infra"]
map = [
    { layer = "app",   glob = "src/app/**" },
    { layer = "infra", glob = "src/infra/**" },
]
forbid = [{ from = "app", to = "infra" }]

[lint]
deny = ["layering", "cycles"]
"#;

    #[test]
    fn forbidden_edge_trips_layering() {
        let head = Graph {
            nodes: vec![
                module("crate::app", "src/app/mod.rs"),
                module("crate::infra", "src/infra/mod.rs"),
            ],
            edges: vec![uses("crate::app", "crate::infra")],
        };
        // Edge is newly added, so it fails even under the default new-only ratchet.
        let changes = vec![Change::EdgeAdded(uses("crate::app", "crate::infra"))];
        let f = lint(&head, &changes, &cfg(LAYERS));
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].rule, "layering");
        assert_eq!(f[0].severity, Severity::Deny);
        assert!(f[0].message.contains("must not depend on"));
    }

    #[test]
    fn preexisting_violation_passes_under_ratchet() {
        let head = Graph {
            nodes: vec![
                module("crate::app", "src/app/mod.rs"),
                module("crate::infra", "src/infra/mod.rs"),
            ],
            edges: vec![uses("crate::app", "crate::infra")],
        };
        // No EdgeAdded: the violation is pre-existing, so new-only does not fail it.
        let f = lint(&head, &[], &cfg(LAYERS));
        assert!(
            f.is_empty(),
            "pre-existing violation must not fail under ratchet: {f:?}"
        );
    }

    #[test]
    fn preexisting_violation_fails_under_all_mode() {
        let toml = format!("{LAYERS}\n[ratchet]\nmode = \"all\"\n");
        let head = Graph {
            nodes: vec![
                module("crate::app", "src/app/mod.rs"),
                module("crate::infra", "src/infra/mod.rs"),
            ],
            edges: vec![uses("crate::app", "crate::infra")],
        };
        let f = lint(&head, &[], &cfg(&toml));
        assert_eq!(f.len(), 1, "all mode flags pre-existing violations");
    }

    #[test]
    fn allowed_edge_does_not_trip_layering() {
        // infra -> app is not forbidden (only app -> infra is).
        let head = Graph {
            nodes: vec![
                module("crate::app", "src/app/mod.rs"),
                module("crate::infra", "src/infra/mod.rs"),
            ],
            edges: vec![uses("crate::infra", "crate::app")],
        };
        let changes = vec![Change::EdgeAdded(uses("crate::infra", "crate::app"))];
        assert!(lint(&head, &changes, &cfg(LAYERS)).is_empty());
    }

    #[test]
    fn new_cycle_trips_cycles() {
        let head = Graph {
            nodes: vec![
                module("crate::a", "src/a.rs"),
                module("crate::b", "src/b.rs"),
            ],
            edges: vec![uses("crate::a", "crate::b"), uses("crate::b", "crate::a")],
        };
        // The back-edge b -> a is what closed the cycle.
        let changes = vec![Change::EdgeAdded(uses("crate::b", "crate::a"))];
        let f = lint(&head, &changes, &cfg(LAYERS));
        let cyc: Vec<_> = f.iter().filter(|f| f.rule == "cycles").collect();
        assert_eq!(cyc.len(), 1, "expected one cycle finding: {f:?}");
        assert!(cyc[0].message.contains("crate::a"));
        assert!(cyc[0].message.contains("crate::b"));
    }

    #[test]
    fn preexisting_cycle_passes_under_ratchet() {
        let head = Graph {
            nodes: vec![
                module("crate::a", "src/a.rs"),
                module("crate::b", "src/b.rs"),
            ],
            edges: vec![uses("crate::a", "crate::b"), uses("crate::b", "crate::a")],
        };
        // No added edges: the cycle predates the change, so new-only stays silent.
        let f = lint(&head, &[], &cfg(LAYERS));
        assert!(f.iter().all(|f| f.rule != "cycles"), "{f:?}");
    }

    #[test]
    fn acyclic_graph_has_no_cycle_finding() {
        let head = Graph {
            nodes: vec![
                module("crate::a", "src/a.rs"),
                module("crate::b", "src/b.rs"),
            ],
            edges: vec![uses("crate::a", "crate::b")],
        };
        let changes = vec![Change::EdgeAdded(uses("crate::a", "crate::b"))];
        assert!(lint(&head, &changes, &cfg(LAYERS))
            .iter()
            .all(|f| f.rule != "cycles"));
    }

    #[test]
    fn isolated_state_trips_unreachable_under_all() {
        // Open and Done are connected; Orphan has no transitions at all.
        let head = Graph {
            nodes: vec![variant("S::Open"), variant("S::Done"), variant("S::Orphan")],
            edges: vec![transition("S::Open", "S::Done")],
        };
        let f = lint(&head, &[], &cfg(STATES_ALL));
        let u: Vec<_> = f.iter().filter(|f| f.rule == "unreachable_state").collect();
        assert_eq!(u.len(), 1, "expected one unreachable finding: {f:?}");
        assert!(u[0].message.contains("S::Orphan"));
    }

    #[test]
    fn entry_state_is_not_unreachable() {
        // Open has outgoing but no incoming: a plausible entry, must not be flagged.
        let head = Graph {
            nodes: vec![variant("S::Open"), variant("S::Done")],
            edges: vec![transition("S::Open", "S::Done")],
        };
        let f = lint(&head, &[], &cfg(STATES_ALL));
        assert!(
            f.iter().all(|f| f.rule != "unreachable_state"),
            "entry state must not be unreachable: {f:?}"
        );
    }

    #[test]
    fn preexisting_isolated_state_passes_under_ratchet() {
        let head = Graph {
            nodes: vec![variant("S::Open"), variant("S::Done"), variant("S::Orphan")],
            edges: vec![transition("S::Open", "S::Done")],
        };
        // No change touches Orphan, so new-only stays silent on it.
        let f = lint(&head, &[], &cfg(STATES));
        assert!(f.iter().all(|f| f.rule != "unreachable_state"), "{f:?}");
    }

    #[test]
    fn isolated_state_trips_unreachable_when_change_touches_it() {
        // A transition on Orphan was removed, leaving it isolated: new-only flags it.
        let head = Graph {
            nodes: vec![variant("S::Open"), variant("S::Done"), variant("S::Orphan")],
            edges: vec![transition("S::Open", "S::Done")],
        };
        let changes = vec![Change::EdgeRemoved(transition("S::Open", "S::Orphan"))];
        let f = lint(&head, &changes, &cfg(STATES));
        let u: Vec<_> = f.iter().filter(|f| f.rule == "unreachable_state").collect();
        assert_eq!(u.len(), 1, "expected one unreachable finding: {f:?}");
        assert!(u[0].message.contains("S::Orphan"));
    }

    #[test]
    fn dead_end_trips_terminal_under_all() {
        // Done has an incoming transition but no outgoing: a dead-end.
        let head = Graph {
            nodes: vec![variant("S::Open"), variant("S::Done")],
            edges: vec![transition("S::Open", "S::Done")],
        };
        let f = lint(&head, &[], &cfg(STATES_ALL));
        let t: Vec<_> = f
            .iter()
            .filter(|f| f.rule == "terminal_state_without_exit")
            .collect();
        assert_eq!(t.len(), 1, "expected one terminal finding: {f:?}");
        assert!(t[0].message.contains("S::Done"));
    }

    #[test]
    fn preexisting_dead_end_passes_under_ratchet() {
        let head = Graph {
            nodes: vec![variant("S::Open"), variant("S::Done")],
            edges: vec![transition("S::Open", "S::Done")],
        };
        // No change touches Done, so new-only does not fail the pre-existing dead-end.
        let f = lint(&head, &[], &cfg(STATES));
        assert!(
            f.iter().all(|f| f.rule != "terminal_state_without_exit"),
            "{f:?}"
        );
    }

    #[test]
    fn dead_end_trips_terminal_when_new_transition_reaches_it() {
        // The transition into Done is new, so new-only flags Done as a dead-end.
        let head = Graph {
            nodes: vec![variant("S::Open"), variant("S::Done")],
            edges: vec![transition("S::Open", "S::Done")],
        };
        let changes = vec![Change::EdgeAdded(transition("S::Open", "S::Done"))];
        let f = lint(&head, &changes, &cfg(STATES));
        let t: Vec<_> = f
            .iter()
            .filter(|f| f.rule == "terminal_state_without_exit")
            .collect();
        assert_eq!(t.len(), 1, "expected one terminal finding: {f:?}");
        assert!(t[0].message.contains("S::Done"));
    }

    #[test]
    fn state_with_exit_is_not_terminal() {
        // Open has an outgoing transition, so it is never terminal.
        let head = Graph {
            nodes: vec![variant("S::Open"), variant("S::Done")],
            edges: vec![transition("S::Open", "S::Done")],
        };
        let changes = vec![Change::EdgeAdded(transition("S::Open", "S::Done"))];
        let f = lint(&head, &changes, &cfg(STATES));
        assert!(f
            .iter()
            .all(|f| !(f.rule == "terminal_state_without_exit" && f.message.contains("S::Open"))));
    }

    #[test]
    fn new_transition_closes_state_cycle() {
        let head = Graph {
            nodes: vec![variant("S::A"), variant("S::B")],
            edges: vec![transition("S::A", "S::B"), transition("S::B", "S::A")],
        };
        let changes = vec![Change::EdgeAdded(transition("S::B", "S::A"))];
        let f = lint(&head, &changes, &cfg(STATES));
        let c: Vec<_> = f.iter().filter(|f| f.rule == "new_state_cycle").collect();
        assert_eq!(c.len(), 1, "expected one state cycle finding: {f:?}");
        assert!(c[0].message.contains("S::A"));
        assert!(c[0].message.contains("S::B"));
    }

    #[test]
    fn self_transition_is_a_state_cycle() {
        let head = Graph {
            nodes: vec![variant("S::Loop")],
            edges: vec![transition("S::Loop", "S::Loop")],
        };
        let changes = vec![Change::EdgeAdded(transition("S::Loop", "S::Loop"))];
        let f = lint(&head, &changes, &cfg(STATES));
        let c: Vec<_> = f.iter().filter(|f| f.rule == "new_state_cycle").collect();
        assert_eq!(c.len(), 1, "expected one state cycle finding: {f:?}");
        assert!(c[0].message.contains("S::Loop"));
    }

    #[test]
    fn preexisting_state_cycle_passes_under_ratchet() {
        let head = Graph {
            nodes: vec![variant("S::A"), variant("S::B")],
            edges: vec![transition("S::A", "S::B"), transition("S::B", "S::A")],
        };
        // No added transition: the cycle predates the change, so new-only stays silent.
        let f = lint(&head, &[], &cfg(STATES));
        assert!(f.iter().all(|f| f.rule != "new_state_cycle"), "{f:?}");
    }

    #[test]
    fn preexisting_state_cycle_fails_under_all_mode() {
        let head = Graph {
            nodes: vec![variant("S::A"), variant("S::B")],
            edges: vec![transition("S::A", "S::B"), transition("S::B", "S::A")],
        };
        let f = lint(&head, &[], &cfg(STATES_ALL));
        let c: Vec<_> = f.iter().filter(|f| f.rule == "new_state_cycle").collect();
        assert_eq!(
            c.len(),
            1,
            "all mode flags pre-existing state cycles: {f:?}"
        );
    }

    #[test]
    fn acyclic_states_have_no_cycle_finding() {
        let head = Graph {
            nodes: vec![variant("S::A"), variant("S::B")],
            edges: vec![transition("S::A", "S::B")],
        };
        let f = lint(&head, &[], &cfg(STATES_ALL));
        assert!(f.iter().all(|f| f.rule != "new_state_cycle"), "{f:?}");
    }

    #[test]
    fn disabled_state_lints_do_not_run() {
        // States present but no state lint enabled: nothing fires.
        let head = Graph {
            nodes: vec![variant("S::Open"), variant("S::Done"), variant("S::Orphan")],
            edges: vec![transition("S::Open", "S::Done")],
        };
        assert!(lint(&head, &[], &cfg("")).is_empty());
    }

    fn calls(from: &str, to: &str) -> Edge {
        Edge {
            from: StableId::new(from),
            to: StableId::new(to),
            kind: EdgeKind::Calls,
            span: SourceSpan {
                file: "src/lib.rs".into(),
                start: 0,
                end: 1,
            },
            ordinal: None,
        }
    }

    fn fn_node(id: &str) -> csd_ir::Node {
        csd_ir::Node {
            id: StableId::new(id),
            kind: NodeKind::Fn,
            span: SourceSpan {
                file: "src/lib.rs".into(),
                start: 0,
                end: 1,
            },
            attrs: BTreeMap::new(),
            fingerprint: None,
        }
    }

    const CALLS: &str = r#"
[calls]
hot = ["crate::server::*"]
io  = ["crate::db", "crate::net::*"]

[lint]
deny = ["io_in_hot_path"]
"#;

    const CALLS_ALL: &str = r#"
[calls]
hot = ["crate::server::*"]
io  = ["crate::db", "crate::net::*"]

[lint]
deny = ["io_in_hot_path"]

[ratchet]
mode = "all"
"#;

    #[test]
    fn new_io_call_from_hot_fn_trips() {
        let head = Graph {
            nodes: vec![
                fn_node("crate::server::handle"),
                fn_node("crate::db::query"),
            ],
            edges: vec![calls("crate::server::handle", "crate::db::query")],
        };
        let changes = vec![Change::EdgeAdded(calls(
            "crate::server::handle",
            "crate::db::query",
        ))];
        let f = lint(&head, &changes, &cfg(CALLS));
        let io: Vec<_> = f.iter().filter(|f| f.rule == "io_in_hot_path").collect();
        assert_eq!(io.len(), 1, "expected one io finding: {f:?}");
        assert_eq!(io[0].severity, Severity::Deny);
        assert_eq!(
            io[0].message,
            "crate::server::handle (hot) calls into crate::db (io)"
        );
    }

    #[test]
    fn preexisting_io_call_passes_under_ratchet() {
        let head = Graph {
            nodes: vec![
                fn_node("crate::server::handle"),
                fn_node("crate::db::query"),
            ],
            edges: vec![calls("crate::server::handle", "crate::db::query")],
        };
        // No EdgeAdded: the call predates the change, so new-only stays silent.
        let f = lint(&head, &[], &cfg(CALLS));
        assert!(f.iter().all(|f| f.rule != "io_in_hot_path"), "{f:?}");
    }

    #[test]
    fn preexisting_io_call_trips_under_all_mode() {
        let head = Graph {
            nodes: vec![
                fn_node("crate::server::handle"),
                fn_node("crate::db::query"),
            ],
            edges: vec![calls("crate::server::handle", "crate::db::query")],
        };
        let f = lint(&head, &[], &cfg(CALLS_ALL));
        let io: Vec<_> = f.iter().filter(|f| f.rule == "io_in_hot_path").collect();
        assert_eq!(io.len(), 1, "all mode flags pre-existing calls: {f:?}");
    }

    #[test]
    fn hot_call_into_non_io_does_not_trip() {
        let head = Graph {
            nodes: vec![
                fn_node("crate::server::handle"),
                fn_node("crate::domain::compute"),
            ],
            edges: vec![calls("crate::server::handle", "crate::domain::compute")],
        };
        let changes = vec![Change::EdgeAdded(calls(
            "crate::server::handle",
            "crate::domain::compute",
        ))];
        assert!(lint(&head, &changes, &cfg(CALLS_ALL))
            .iter()
            .all(|f| f.rule != "io_in_hot_path"));
    }

    #[test]
    fn non_hot_call_into_io_does_not_trip() {
        let head = Graph {
            nodes: vec![fn_node("crate::worker::run"), fn_node("crate::db::query")],
            edges: vec![calls("crate::worker::run", "crate::db::query")],
        };
        let changes = vec![Change::EdgeAdded(calls(
            "crate::worker::run",
            "crate::db::query",
        ))];
        assert!(lint(&head, &changes, &cfg(CALLS_ALL))
            .iter()
            .all(|f| f.rule != "io_in_hot_path"));
    }

    #[test]
    fn io_in_hot_path_silent_when_not_selected() {
        let head = Graph {
            nodes: vec![
                fn_node("crate::server::handle"),
                fn_node("crate::db::query"),
            ],
            edges: vec![calls("crate::server::handle", "crate::db::query")],
        };
        let toml = "[calls]\nhot = [\"crate::server::*\"]\nio = [\"crate::db\"]\n";
        let changes = vec![Change::EdgeAdded(calls(
            "crate::server::handle",
            "crate::db::query",
        ))];
        assert!(lint(&head, &changes, &cfg(toml)).is_empty());
    }

    #[test]
    fn disabled_lint_does_not_run() {
        // Config enables neither lint.
        let head = Graph {
            nodes: vec![
                module("crate::app", "src/app/mod.rs"),
                module("crate::infra", "src/infra/mod.rs"),
            ],
            edges: vec![uses("crate::app", "crate::infra")],
        };
        let toml =
            "[layers]\norder = [\"app\",\"infra\"]\nforbid = [{from=\"app\",to=\"infra\"}]\n";
        let changes = vec![Change::EdgeAdded(uses("crate::app", "crate::infra"))];
        assert!(lint(&head, &changes, &cfg(toml)).is_empty());
    }
}
