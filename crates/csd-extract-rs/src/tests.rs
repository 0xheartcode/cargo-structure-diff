//! Fixture-crate tests for the extractor. A tiny crate is laid out under a temp dir and extracted;
//! assertions are on the resulting node set and fingerprints, all order-independent.

use std::fs;
use std::path::Path;

use csd_ir::{Graph, Node, NodeKind};

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
fn unresolved_use_paths_recorded_not_edged() {
    let g = fixture();
    // No edges are emitted: resolution is deferred to the resolver (bb67bbb).
    assert!(g.edges.is_empty());
    // Raw use targets are parked on the module node instead.
    let uses = node(&g, "crate").attrs.get("uses").unwrap();
    assert!(uses.contains("crate::money::Money"));
    assert!(uses.contains("std::sync::Arc"));
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
