//! Architectural lints over the module graph.
//!
//! Two rules for M1 (SPEC.md sections 3.1 and 7):
//! - `layering`: a `Uses` edge whose source and target layers form a forbidden pair.
//! - `cycles`: a strongly-connected component of the module `Uses` graph (a module cycle).
//!
//! Both are ratchet-aware. Under [`RatchetMode::NewOnly`] only violations introduced by a newly
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
    /// The rule that fired (`layering` or `cycles`).
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
    let paths = module_paths(head);
    let mut findings = Vec::new();

    if let Some(sev) = severity_of("layering") {
        findings.extend(layering(head, config, &paths, &added, sev));
    }
    if let Some(sev) = severity_of("cycles") {
        findings.extend(cycles(head, config, &added, sev));
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
