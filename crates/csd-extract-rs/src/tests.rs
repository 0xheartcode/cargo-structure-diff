//! Fixture-crate tests for the extractor. A tiny crate is laid out under a temp dir and extracted;
//! assertions are on the resulting node set and fingerprints, all order-independent.

use std::fs;
use std::path::Path;

use csd_ir::{EdgeKind, Graph, Node, NodeKind};

use super::extract;

/// Write a file, creating parent dirs.
fn write(root: &Path, rel: &str, body: &str) {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, body).unwrap();
}

/// Lay out the fixture crate and extract it.
fn fixture() -> Graph {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    write(
        root,
        "src/lib.rs",
        r#"//! crate root
use std::sync::Arc;
use crate::money::Money;

pub mod money;
pub mod foo;

#[path = "special/loc.rs"]
pub mod special;

/// An order.
pub struct Order {
    pub id: u64,
    total: Arc<Money>,
    tags: Vec<String>,
    maybe: Option<Box<Money>>,
}

pub enum Status { Active, Closed(Money), Pending { since: u64 } }

pub trait Repo {
    type Id;
    fn get(&self, id: u64) -> Option<Money>;
}

pub fn make(a: u64, b: &Money) -> Order {
    Order { id: a, total: todo!(), tags: vec![], maybe: None }
}

#[cfg(test)]
mod tests {
    struct ShouldNotAppear;
    fn should_not_appear() {}
}
"#,
    );
    write(
        root,
        "src/money.rs",
        "pub struct Money { pub cents: u64 }\n",
    );
    write(root, "src/foo/mod.rs", "pub mod bar;\npub fn foo_fn() {}\n");
    write(root, "src/foo/bar.rs", "pub struct Bar { x: u8 }\n");
    write(root, "src/special/loc.rs", "pub struct Special;\n");
    // `target/` must be skipped, not walked as a crate.
    write(root, "target/debug/junk.rs", "pub struct Junk;\n");

    extract(root).unwrap()
}

fn node<'a>(g: &'a Graph, id: &str) -> &'a Node {
    g.nodes
        .iter()
        .find(|n| n.id.as_str() == id)
        .unwrap_or_else(|| panic!("missing node {id}"))
}

fn ids_of_kind(g: &Graph, kind: NodeKind) -> Vec<String> {
    let mut v: Vec<String> = g
        .nodes
        .iter()
        .filter(|n| n.kind == kind)
        .map(|n| n.id.as_str().to_string())
        .collect();
    v.sort();
    v
}

#[test]
fn module_set_from_layout_and_path_attr() {
    let g = fixture();
    assert_eq!(
        ids_of_kind(&g, NodeKind::Module),
        vec![
            "crate",
            "crate::foo",
            "crate::foo::bar",
            "crate::money",
            "crate::special",
        ]
    );
}

#[test]
fn cfg_test_items_and_mod_tests_excluded() {
    let g = fixture();
    assert!(!g
        .nodes
        .iter()
        .any(|n| n.id.as_str().contains("ShouldNotAppear")));
    assert!(!g
        .nodes
        .iter()
        .any(|n| n.id.as_str().contains("should_not_appear")));
    assert!(!g.nodes.iter().any(|n| n.id.as_str() == "crate::tests"));
}

#[test]
fn target_dir_is_not_extracted() {
    let g = fixture();
    assert!(!g.nodes.iter().any(|n| n.id.as_str().contains("Junk")));
}

#[test]
fn item_kinds_are_correct() {
    let g = fixture();
    assert_eq!(node(&g, "crate::Order").kind, NodeKind::Struct);
    assert_eq!(node(&g, "crate::Status").kind, NodeKind::Enum);
    assert_eq!(node(&g, "crate::Repo").kind, NodeKind::Trait);
    assert_eq!(node(&g, "crate::make").kind, NodeKind::Fn);
    assert_eq!(node(&g, "crate::money::Money").kind, NodeKind::Struct);
    assert_eq!(node(&g, "crate::foo::bar::Bar").kind, NodeKind::Struct);
    assert_eq!(node(&g, "crate::special::Special").kind, NodeKind::Struct);
    assert_eq!(node(&g, "crate::foo::foo_fn").kind, NodeKind::Fn);
}

#[test]
fn enum_variants_emitted_as_variant_nodes() {
    let g = fixture();
    assert_eq!(
        ids_of_kind(&g, NodeKind::Variant),
        vec![
            "crate::Status::Active",
            "crate::Status::Closed",
            "crate::Status::Pending",
        ]
    );
}

#[test]
fn struct_fingerprint_members_and_unwrapped_field_types() {
    let g = fixture();
    let fp = node(&g, "crate::Order").fingerprint.as_ref().unwrap();

    let members: Vec<&str> = fp.members.iter().map(String::as_str).collect();
    assert_eq!(members, vec!["id", "maybe", "tags", "total"]);

    // Arc<Money> and Option<Box<Money>> both unwrap to Money, so the multiset counts it twice.
    assert_eq!(fp.field_types.get("Money"), Some(&2));
    assert_eq!(fp.field_types.get("u64"), Some(&1));
    assert_eq!(fp.field_types.get("String"), Some(&1));
    // Wrappers must never surface as association targets.
    for wrapper in ["Arc", "Box", "Rc", "Vec", "Option"] {
        assert!(
            !fp.field_types.contains_key(wrapper),
            "wrapper {wrapper} leaked"
        );
    }
    assert_ne!(fp.doc_hash, 0, "doc comment should hash non-zero");
}

#[test]
fn enum_and_trait_and_fn_fingerprints() {
    let g = fixture();

    let status = node(&g, "crate::Status").fingerprint.as_ref().unwrap();
    let variants: Vec<&str> = status.members.iter().map(String::as_str).collect();
    assert_eq!(variants, vec!["Active", "Closed", "Pending"]);
    assert_eq!(status.field_types.get("Money"), Some(&1));
    assert_eq!(status.field_types.get("u64"), Some(&1));

    let repo = node(&g, "crate::Repo").fingerprint.as_ref().unwrap();
    let items: Vec<&str> = repo.members.iter().map(String::as_str).collect();
    assert_eq!(items, vec!["Id", "get"]);

    let make = node(&g, "crate::make").fingerprint.as_ref().unwrap();
    let params: Vec<&str> = make.members.iter().map(String::as_str).collect();
    assert_eq!(params, vec!["a", "b"]);
    // params `u64` + `&Money`, return `Order`.
    assert_eq!(make.field_types.get("u64"), Some(&1));
    assert_eq!(make.field_types.get("Money"), Some(&1));
    assert_eq!(make.field_types.get("Order"), Some(&1));
}

#[test]
fn intra_crate_use_resolves_external_use_stays_unresolved() {
    let g = fixture();
    // `use crate::money::Money` resolves to a Uses edge to the item's owning module.
    assert!(g.edges.iter().any(|e| e.kind == EdgeKind::Uses
        && e.from.as_str() == "crate"
        && e.to.as_str() == "crate::money"));
    // `use std::sync::Arc` is external, so no edge points into std or any external crate.
    assert!(!g.edges.iter().any(|e| e.to.as_str().starts_with("std")));
    // The unresolved external target is still recorded; the resolved one is not.
    let uses = node(&g, "crate").attrs.get("uses").unwrap();
    assert!(uses.contains("std::sync::Arc"));
    assert!(!uses.contains("crate::money::Money"));
}

#[test]
fn pub_use_reexport_chain_resolves_to_owning_module() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "src/lib.rs",
        "pub mod inner;\npub mod facade;\npub mod consumer;\n",
    );
    write(root, "src/inner.rs", "pub struct Widget;\n");
    write(root, "src/facade.rs", "pub use crate::inner::Widget;\n");
    write(root, "src/consumer.rs", "use crate::facade::Widget;\n");
    let g = extract(root).unwrap();
    // consumer imports through facade's re-export; the edge targets the owning module `inner`.
    assert!(g.edges.iter().any(|e| e.kind == EdgeKind::Uses
        && e.from.as_str() == "crate::consumer"
        && e.to.as_str() == "crate::inner"));
    // No edge lands on the re-exporting facade module.
    assert!(!g
        .edges
        .iter()
        .any(|e| e.from.as_str() == "crate::consumer" && e.to.as_str() == "crate::facade"));
}

#[test]
fn multi_crate_ids_are_namespaced_per_crate() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "alpha/src/lib.rs",
        "pub mod util;\npub struct Thing;\n",
    );
    write(root, "alpha/src/util.rs", "pub struct Helper;\n");
    write(
        root,
        "beta/src/lib.rs",
        "pub mod util;\npub struct Thing;\n",
    );
    write(root, "beta/src/util.rs", "pub struct Helper;\n");
    let g = extract(root).unwrap();
    let ids: Vec<&str> = g.nodes.iter().map(|n| n.id.as_str()).collect();
    // Each crate is namespaced by its directory name, so identical paths no longer collide.
    for id in [
        "alpha",
        "alpha::Thing",
        "alpha::util",
        "alpha::util::Helper",
        "beta",
        "beta::Thing",
        "beta::util",
        "beta::util::Helper",
    ] {
        assert!(ids.contains(&id), "missing id {id}");
    }
    // Nothing falls back to the shared `crate::` prefix.
    assert!(!ids
        .iter()
        .any(|id| *id == "crate" || id.starts_with("crate::")));
}

#[test]
fn cross_crate_use_resolves_to_sibling_crate_module() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    // `alpha` depends on `beta`; a `use beta::Thing` should become an edge alpha -> beta.
    write(
        root,
        "alpha/src/lib.rs",
        "use beta::Thing;\nuse std::sync::Arc;\npub fn run(t: Thing) -> Arc<u8> { todo!() }\n",
    );
    write(root, "beta/src/lib.rs", "pub struct Thing;\n");
    let g = extract(root).unwrap();
    // The cross-crate use resolves to beta's crate-root module node.
    assert!(
        g.edges.iter().any(|e| e.kind == EdgeKind::Uses
            && e.from.as_str() == "alpha"
            && e.to.as_str() == "beta"),
        "expected a Uses edge alpha -> beta"
    );
    // The external `use std::sync::Arc` produces no edge.
    assert!(!g.edges.iter().any(|e| e.to.as_str().starts_with("std")));
    // The unresolved external target is still parked; the resolved sibling-crate one is not.
    let uses = node(&g, "alpha").attrs.get("uses").unwrap();
    assert!(uses.contains("std::sync::Arc"));
    assert!(!uses.contains("beta::Thing"));
}

#[test]
fn cross_crate_use_targets_deepest_existing_module() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    // `alpha` imports an item nested in beta's `inner` module: the edge targets `beta::inner`.
    write(root, "alpha/src/lib.rs", "use beta::inner::Widget;\n");
    write(root, "beta/src/lib.rs", "pub mod inner;\n");
    write(root, "beta/src/inner.rs", "pub struct Widget;\n");
    let g = extract(root).unwrap();
    assert!(
        g.edges.iter().any(|e| e.kind == EdgeKind::Uses
            && e.from.as_str() == "alpha"
            && e.to.as_str() == "beta::inner"),
        "expected a Uses edge alpha -> beta::inner"
    );
}

/// A crate whose enum carries a state machine, exercising the transition idioms.
fn state_fixture() -> Graph {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "src/lib.rs",
        r#"
pub enum Light { Red, Green, Yellow, Off }

impl Light {
    /// match-arm-returns-variant, mixing `Light::` and `Self::` and a bare name.
    pub fn next(self) -> Light {
        match self {
            Light::Red => Light::Green,
            Self::Green => Yellow,
            Light::Yellow => Light::Red,
            Light::Off => Light::Off, // self-loop: from == to, no edge
        }
    }

    /// `*self = Variant` inside a block arm, and a direct assignment arm.
    pub fn turn_off(&mut self) {
        match self {
            Light::Red => { *self = Light::Off; }
            Self::Green => *self = Self::Off,
            other => { let _ = other; } // binding, not a variant: no edge
        }
    }

    /// An unrelated match on an integer must yield no transitions.
    pub fn describe(&self) -> u8 {
        match 3u8 {
            0 => 1,
            _ => 2,
        }
    }

    /// Assigning a *different* enum's variant must not be read as a transition.
    pub fn touch(&mut self) {
        match self {
            Light::Red => self.foreign = Mode::On,
            _ => {}
        }
    }
}

pub enum Mode { On, Off }
"#,
    );
    extract(root).unwrap()
}

fn transitions(g: &Graph) -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = g
        .edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Transitions)
        .map(|e| (e.from.as_str().to_string(), e.to.as_str().to_string()))
        .collect();
    v.sort();
    v
}

#[test]
fn enum_transitions_are_detected() {
    let g = state_fixture();
    let got = transitions(&g);
    let want: Vec<(String, String)> = [
        ("crate::Light::Green", "crate::Light::Off"),
        ("crate::Light::Green", "crate::Light::Yellow"),
        ("crate::Light::Red", "crate::Light::Green"),
        ("crate::Light::Red", "crate::Light::Off"),
        ("crate::Light::Yellow", "crate::Light::Red"),
    ]
    .iter()
    .map(|(f, t)| (f.to_string(), t.to_string()))
    .collect();
    assert_eq!(got, want);
}

#[test]
fn no_spurious_transitions() {
    let g = state_fixture();
    let ts = transitions(&g);
    // Self-loops are never emitted.
    assert!(!ts.iter().any(|(f, t)| f == t));
    // The unrelated integer match and the foreign-enum assignment produce nothing extra.
    assert!(!ts
        .iter()
        .any(|(_, t)| t.contains("Mode") || t.starts_with("crate::Mode")));
    // No transition ever lands on a non-Light node.
    assert!(ts
        .iter()
        .all(|(f, t)| f.starts_with("crate::Light::") && t.starts_with("crate::Light::")));
}

#[test]
fn transitions_are_deterministic() {
    assert_eq!(transitions(&state_fixture()), transitions(&state_fixture()));
}

/// A crate whose `step(self) -> Self` builds the to-state with every constructor form: an explicit
/// `return`, a tuple `Variant(..)`, a struct `Variant { .. }`, and a bare/`Self::` name.
fn constructor_fixture() -> Graph {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "src/lib.rs",
        r#"
pub enum Door {
    Open,
    Closed,
    Locked(u32),
    Ajar { gap: u32 },
}

impl Door {
    /// `-> Self` return, exercising every to-expr constructor form.
    pub fn step(self) -> Self {
        match self {
            Door::Open => return Door::Closed,       // explicit return, qualified
            Door::Closed => Door::Locked(1),         // tuple `Variant(..)` constructor
            Door::Locked(_) => Self::Ajar { gap: 2 }, // struct `Self::Variant { .. }` constructor
            Door::Ajar { .. } => Self::Open,          // struct-pattern from, `Self::` to
        }
    }
}
"#,
    );
    extract(root).unwrap()
}

#[test]
fn to_expr_constructor_forms_are_detected() {
    let g = constructor_fixture();
    // return-position variant, tuple constructor, struct constructor, and Self:: name all resolve.
    let want: Vec<(String, String)> = [
        ("crate::Door::Ajar", "crate::Door::Open"),
        ("crate::Door::Closed", "crate::Door::Locked"),
        ("crate::Door::Locked", "crate::Door::Ajar"),
        ("crate::Door::Open", "crate::Door::Closed"),
    ]
    .iter()
    .map(|(f, t)| (f.to_string(), t.to_string()))
    .collect();
    assert_eq!(transitions(&g), want);
}

/// A crate whose match uses bare patterns and bare to-exprs (as under `use Enum::*`).
fn bare_name_fixture() -> Graph {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "src/lib.rs",
        r#"
pub enum Flag { Up, Down }

impl Flag {
    pub fn flip(self) -> Self {
        match self {
            Up => Down,
            Down => Up,
        }
    }
}
"#,
    );
    extract(root).unwrap()
}

#[test]
fn bare_patterns_and_exprs_are_detected() {
    let g = bare_name_fixture();
    let want: Vec<(String, String)> = [
        ("crate::Flag::Down", "crate::Flag::Up"),
        ("crate::Flag::Up", "crate::Flag::Down"),
    ]
    .iter()
    .map(|(f, t)| (f.to_string(), t.to_string()))
    .collect();
    assert_eq!(transitions(&g), want);
}

/// A crate assigning a variant through a `self.field = Variant` target inside a match arm.
fn field_assign_fixture() -> Graph {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "src/lib.rs",
        r#"
pub enum Sig { A, B, C }

impl Sig {
    pub fn poke(&mut self) {
        match self {
            Sig::A => self.inner = Sig::B,
            _ => {}
        }
    }
}
"#,
    );
    extract(root).unwrap()
}

#[test]
fn self_field_assignment_reads_rhs_variant() {
    let g = field_assign_fixture();
    // `is_self_target` accepts a `self.field` LHS, so the RHS variant is read as the to-state. The
    // from-variant is the arm pattern (`A`). This is the real behavior of the field-assign branch.
    let want: Vec<(String, String)> = [("crate::Sig::A", "crate::Sig::B")]
        .iter()
        .map(|(f, t)| (f.to_string(), t.to_string()))
        .collect();
    assert_eq!(transitions(&g), want);
}

/// A crate exercising the type-view edges: a local trait realized by a local type, an external
/// trait impl, an inherent impl, and struct fields of local, external, and wrapped-local types.
fn type_view_fixture() -> Graph {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "src/lib.rs",
        r#"
use std::fmt;
use std::sync::Arc;

pub trait Repo {
    fn get(&self);
}

pub struct Widget;
pub struct Gadget;

pub struct Store {
    inner: Widget,
    boxed: Arc<Gadget>,
    name: String,
    count: u64,
}

impl Repo for Store {
    fn get(&self) {}
}

impl std::fmt::Display for Store {
    fn fmt(&self, _f: &mut fmt::Formatter) -> fmt::Result {
        Ok(())
    }
}

impl Store {}
"#,
    );
    extract(root).unwrap()
}

fn edges_of_kind(g: &Graph, kind: EdgeKind) -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = g
        .edges
        .iter()
        .filter(|e| e.kind == kind)
        .map(|e| (e.from.as_str().to_string(), e.to.as_str().to_string()))
        .collect();
    v.sort();
    v
}

#[test]
fn implements_edge_only_for_local_trait_and_type() {
    let g = type_view_fixture();
    // Local `impl Repo for Store` yields exactly one Implements edge from the type to the trait.
    // The external `impl Display for Store` and the inherent `impl Store {}` yield nothing.
    assert_eq!(
        edges_of_kind(&g, EdgeKind::Implements),
        vec![("crate::Store".to_string(), "crate::Repo".to_string())]
    );
}

#[test]
fn associates_edges_only_for_local_field_types() {
    let g = type_view_fixture();
    // `inner: Widget` and `boxed: Arc<Gadget>` associate to local nodes (Arc unwrapped). The
    // `name: String` and `count: u64` fields are external/primitive and emit nothing.
    assert_eq!(
        edges_of_kind(&g, EdgeKind::Associates),
        vec![
            ("crate::Store".to_string(), "crate::Gadget".to_string()),
            ("crate::Store".to_string(), "crate::Widget".to_string()),
        ]
    );
}

#[test]
fn type_view_edges_are_deterministic() {
    let a = type_view_fixture();
    let b = type_view_fixture();
    assert_eq!(
        edges_of_kind(&a, EdgeKind::Implements),
        edges_of_kind(&b, EdgeKind::Implements)
    );
    assert_eq!(
        edges_of_kind(&a, EdgeKind::Associates),
        edges_of_kind(&b, EdgeKind::Associates)
    );
}

/// A crate exercising the call graph: free fn to free fn, self/Self methods, and an unresolvable
/// external call plus a method on an unknown receiver.
fn calls_fixture() -> Graph {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "src/lib.rs",
        r#"
pub fn helper(x: u64) -> u64 {
    x + 1
}

pub fn entry(x: u64) -> u64 {
    let a = helper(x);
    external_thing();
    helper(a)
}

pub struct Widget;

impl Widget {
    pub fn assoc() -> u64 {
        0
    }

    pub fn other(&self) -> u64 {
        7
    }

    pub fn run(&self) -> u64 {
        let a = self.other();
        let b = Self::assoc();
        let c = unknown.compute();
        a + b + c
    }
}
"#,
    );
    extract(root).unwrap()
}

fn calls(g: &Graph) -> Vec<(String, String, u32)> {
    let mut v: Vec<(String, String, u32)> = g
        .edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Calls)
        .map(|e| {
            (
                e.from.as_str().to_string(),
                e.to.as_str().to_string(),
                e.ordinal.expect("Calls edges carry an ordinal"),
            )
        })
        .collect();
    v.sort();
    v
}

#[test]
fn impl_methods_are_emitted_as_fn_nodes() {
    let g = calls_fixture();
    for id in [
        "crate::Widget::assoc",
        "crate::Widget::other",
        "crate::Widget::run",
    ] {
        assert_eq!(node(&g, id).kind, NodeKind::Fn, "missing method fn {id}");
    }
}

#[test]
fn calls_edges_are_ordered_and_resolved() {
    let g = calls_fixture();
    // Free fn to free fn (twice, dense ordinals), and self/Self method calls. The external
    // `external_thing()` and the `unknown.compute()` method call resolve to nothing.
    assert_eq!(
        calls(&g),
        vec![
            (
                "crate::Widget::run".to_string(),
                "crate::Widget::assoc".to_string(),
                1
            ),
            (
                "crate::Widget::run".to_string(),
                "crate::Widget::other".to_string(),
                0
            ),
            ("crate::entry".to_string(), "crate::helper".to_string(), 0),
            ("crate::entry".to_string(), "crate::helper".to_string(), 1),
        ]
    );
}

#[test]
fn unresolved_calls_are_counted_on_the_caller() {
    let g = calls_fixture();
    // `entry` has one external call; `run` has one method call on an unknown receiver.
    assert_eq!(
        node(&g, "crate::entry").attrs.get("unresolved_calls"),
        Some(&"1".to_string())
    );
    assert_eq!(
        node(&g, "crate::Widget::run").attrs.get("unresolved_calls"),
        Some(&"1".to_string())
    );
    // Callees with only resolved (or no) calls carry no ceiling attr.
    for id in [
        "crate::helper",
        "crate::Widget::other",
        "crate::Widget::assoc",
    ] {
        assert!(
            !node(&g, id).attrs.contains_key("unresolved_calls"),
            "{id} should have no unresolved_calls attr"
        );
    }
}

#[test]
fn calls_are_deterministic() {
    assert_eq!(calls(&calls_fixture()), calls(&calls_fixture()));
}

#[test]
fn output_is_deterministic() {
    let a = fixture();
    let b = fixture();
    // Same layout, same graph: node ids and kinds are stable across runs.
    let ids_a: Vec<_> = a.nodes.iter().map(|n| (n.id.as_str(), n.kind)).collect();
    let ids_b: Vec<_> = b.nodes.iter().map(|n| (n.id.as_str(), n.kind)).collect();
    assert_eq!(ids_a, ids_b);
}
