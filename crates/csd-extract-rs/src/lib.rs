//! Rust extraction: a crate root on disk to a [`csd_ir::Graph`].
//!
//! M0 scope (backlog area `extract`): the module tree from `mod` declarations and file layout
//! (`extract-modtree`), plus item nodes carrying member fingerprints (`extract-items`). Function
//! bodies, `impl`/trait realization edges, and `use`-path *resolution* are later backlog work.
//!
//! # What is emitted
//!
//! - [`NodeKind::Module`] for every module: the crate root, file modules (`foo.rs`, `foo/mod.rs`),
//!   inline `mod foo { .. }` blocks, and `#[path]`-redirected modules. Ids are crate-relative
//!   paths like `crate::foo::bar`.
//! - [`NodeKind::Struct`] / [`NodeKind::Enum`] / [`NodeKind::Trait`] / [`NodeKind::Fn`] for the
//!   corresponding items, each with a [`Fingerprint`] (see [`csd_ir::fingerprint`]).
//! - [`NodeKind::Variant`] for each enum variant (the state-machine view of SPEC 3.4).
//!
//! Inherent and trait `impl` methods are emitted as [`NodeKind::Fn`] nodes under their Self type
//! (`{Self-type}::method`), alongside free functions.
//!
//! # Call graph
//!
//! Fn bodies are walked for call and method-call expressions in source order (SPEC 3.3). Each is
//! turned into an [`EdgeKind::Calls`] edge from the caller Fn to the callee Fn, carrying a dense
//! per-caller `ordinal` so the sequence is preserved. Resolution is precision-first: free-fn calls
//! resolve through the same intra-crate rules as `use`, and `self.method()` / `Self::method()`
//! resolve to `{Self-type}::method`. Method calls on unknown receivers and `dyn`/generic calls have
//! no statically known callee and are never invented; each such unresolved call is counted on the
//! caller node's `unresolved_calls` attribute so the fidelity ceiling stays visible.
//!
//! # `use`-path resolution
//!
//! tree-sitter gives syntax, not name resolution (SPEC 3.1 amendment). The precision-first
//! mini-resolver (backlog `extract-resolver`, id `bb67bbb`) turns a `use` target into an
//! [`EdgeKind::Uses`] edge from the referencing module to the target module *only* when it is
//! certain: explicit intra-crate paths (`crate::`, `self::`, `super::`), intra-crate `pub use`
//! re-export chains, and (backlog `extract-crossrate`, id `422be96`) cross-crate paths whose first
//! segment names a sibling workspace crate emitted by this extraction, which resolve to the deepest
//! existing module along the path (else that crate's root) so the module view shows the workspace
//! dependency graph. Globs (`use foo::*`), preludes, and external crates (`std::`, third-party)
//! stay unresolved. A missed edge only costs coverage; a false edge costs a spurious red CI build,
//! which costs trust, so when unsure we do not emit. Targets that stay unresolved are recorded on
//! the module node in the `uses` attribute (comma-separated, sorted) so nothing is lost.
//!
//! # Fingerprint neighbours
//!
//! [`Fingerprint::neighbors`] is left empty. It is the edge neighbourhood, and edges are refined
//! by the resolver; populating it before edges exist would only add churn.
//!
//! # Excluded by default
//!
//! `#[cfg(test)]` items and `mod tests` are skipped (SPEC 3.4 / macro-cfg scope), so test
//! scaffolding does not pollute the structural graph.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::Result;
use csd_ir::{doc_hash, Edge, EdgeKind, Fingerprint, Graph, Node, NodeKind, SourceSpan, StableId};
use tree_sitter::{Node as TsNode, Parser};

/// Type constructors unwrapped to reach the associated type (SPEC 3.2). A field `Arc<Money>`
/// records `Money`, not `Arc`, so associations point at the real type.
const WRAPPERS: [&str; 5] = ["Arc", "Box", "Rc", "Vec", "Option"];

/// Extract a structural graph from a Rust crate or workspace rooted at `root`.
///
/// Discovers crate roots (`src/lib.rs`, `src/main.rs`) under `root`, skipping `target/`, then
/// follows `mod` declarations and file layout to build the module tree and item nodes. Output is
/// normalised so it is byte-stable.
///
/// Workspace note (backlog `extract-workspace-ids`, id `7344e8d`): a single crate is projected
/// under the `crate::` prefix. When more than one crate is discovered, each crate's ids are
/// namespaced by its crate name (`<crate>::module::Item`) so two crates no longer collide under a
/// shared `crate::` prefix. Single-crate output is byte-for-byte unchanged.
pub fn extract(root: &Path) -> Result<Graph> {
    let mut ex = Extractor::new(root);
    let roots = discover_crate_roots(root);
    // A crate is a directory holding `src/`; `lib.rs` + `main.rs` in one crate share a prefix.
    let crate_dirs: BTreeSet<PathBuf> = roots.iter().filter_map(|r| crate_dir_of(r)).collect();
    let multi = crate_dirs.len() > 1;
    for crate_root in roots {
        let prefix = if multi {
            crate_name_for(&crate_root)
        } else {
            "crate".to_string()
        };
        ex.process_file(&crate_root, &prefix);
    }
    ex.finish()
}

/// The crate directory (the parent of `src/`) for a `.../src/{lib,main}.rs` root.
fn crate_dir_of(crate_root: &Path) -> Option<PathBuf> {
    Some(crate_root.parent()?.parent()?.to_path_buf())
}

/// Namespace prefix for a crate: its `[package] name` from `Cargo.toml` if readable, else the
/// crate directory name. Hyphens map to underscores so the prefix reads as a path segment.
fn crate_name_for(crate_root: &Path) -> String {
    let dir = match crate_dir_of(crate_root) {
        Some(d) => d,
        None => return "crate".to_string(),
    };
    let from_toml = std::fs::read_to_string(dir.join("Cargo.toml"))
        .ok()
        .and_then(|t| package_name(&t));
    let name = from_toml
        .or_else(|| {
            dir.file_name()
                .and_then(|n| n.to_str())
                .map(|s| s.to_string())
        })
        .unwrap_or_else(|| "crate".to_string());
    name.replace('-', "_")
}

/// Parse `name = "..."` under the `[package]` table of a `Cargo.toml`. Only a quoted literal is
/// accepted, so `name.workspace = true` and other keys are ignored. No TOML dependency.
fn package_name(toml: &str) -> Option<String> {
    let mut in_package = false;
    for line in toml.lines() {
        let t = line.trim();
        if let Some(header) = t.strip_prefix('[') {
            in_package = header.starts_with("package]");
            continue;
        }
        if !in_package {
            continue;
        }
        if let Some(rest) = t.strip_prefix("name") {
            let rest = rest.trim_start();
            if let Some(rest) = rest.strip_prefix('=') {
                let v = rest.trim();
                if v.starts_with('"') {
                    let name = v.trim_matches('"');
                    if !name.is_empty() {
                        return Some(name.to_string());
                    }
                }
            }
        }
    }
    None
}

/// Recursively find `src/lib.rs` and `src/main.rs` files, skipping `target` and `.git`.
fn discover_crate_roots(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    walk(root, &mut |path| {
        if matches!(
            path.file_name().and_then(|n| n.to_str()),
            Some("lib.rs" | "main.rs")
        ) && path
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            == Some("src")
        {
            out.push(path.to_path_buf());
        }
    });
    out.sort();
    out
}

/// Depth-first file walk that skips `target` and `.git` directories.
fn walk(dir: &Path, visit: &mut impl FnMut(&Path)) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    paths.sort();
    for path in paths {
        if path.is_dir() {
            if matches!(
                path.file_name().and_then(|n| n.to_str()),
                Some("target" | ".git")
            ) {
                continue;
            }
            walk(&path, visit);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            visit(&path);
        }
    }
}

/// File-scoped context threaded through the walk: the source bytes, the file's repo-relative path
/// (for spans), and its on-disk path (for `mod` file resolution). The module path is passed
/// separately since it changes as inline modules nest.
struct FileCtx<'a> {
    file_path: &'a Path,
    file: &'a str,
    src: &'a [u8],
}

/// Carries the graph under construction plus dedup state across the recursive module walk.
struct Extractor {
    root: PathBuf,
    parser: Parser,
    graph: Graph,
    seen_modules: BTreeSet<String>,
    seen_files: BTreeSet<PathBuf>,
    /// Raw `use` targets collected across the walk, resolved to edges in [`Extractor::finish`].
    uses: Vec<RawUse>,
    /// Candidate enum state transitions, resolved to edges in [`Extractor::finish`].
    transitions: Vec<RawTransition>,
    /// Candidate `impl Trait for Type` realizations, resolved in [`Extractor::finish`].
    impls: Vec<RawImplements>,
    /// Candidate type associations from a field type, resolved in [`Extractor::finish`].
    associates: Vec<RawAssociates>,
    /// Call sites collected from Fn bodies, resolved to [`EdgeKind::Calls`] edges in
    /// [`Extractor::finish`]. Push order within a caller is source order.
    calls: Vec<RawCall>,
}

/// One call site read from a caller Fn body. `callee` is the resolved candidate id, or `None` when
/// the call could not be resolved confidently (a method on a non-`self` receiver, a `dyn`/generic
/// call, or a path that is not certainly intra-crate). Membership in the emitted Fn set is
/// confirmed in [`Extractor::finish`]; a candidate that names no emitted Fn counts as unresolved.
struct RawCall {
    /// The calling Fn node's id.
    caller: String,
    /// The resolved callee id, or `None` when unresolved.
    callee: Option<String>,
    /// Where the call is written.
    span: SourceSpan,
}

/// One candidate realization: an `impl Trait for Type` block. Both the type and trait names are
/// resolved to local nodes in [`Extractor::finish`]; if either is external the candidate is dropped.
struct RawImplements {
    /// The `Self` type name/path exactly as written (bare or `crate::`/`self::`/`super::`).
    type_path: String,
    /// The implemented trait name/path exactly as written.
    trait_path: String,
    /// The module the impl block lives in, used to resolve bare names.
    module: String,
    /// Where the impl block is written.
    span: SourceSpan,
}

/// One candidate association: a field's owning type and the inner type it references. The inner
/// name is resolved to a local node in [`Extractor::finish`]; external/primitive/generic drop.
struct RawAssociates {
    /// The owning type node's id (already absolute).
    owner_id: String,
    /// The inner type name/path exactly as written, after unwrapping wrappers and references.
    inner_path: String,
    /// The owning type's module, used to resolve bare names.
    module: String,
    /// Where the field is written.
    span: SourceSpan,
}

/// One candidate state transition: two variant *names* of the same enum, resolved to actual
/// [`NodeKind::Variant`] nodes in [`Extractor::finish`]. Collection is liberal; the membership
/// check there is the precision guard, so a bare pattern that only binds is dropped.
struct RawTransition {
    /// The owning enum's id (`{module}::{Enum}`), assumed to be the impl's own module.
    enum_id: String,
    /// The from-variant name (unqualified).
    from: String,
    /// The to-variant name (unqualified).
    to: String,
    /// The arm the transition was read from.
    span: SourceSpan,
}

/// One expanded `use` target: a single path (nested `{..}` groups are flattened before storage).
#[derive(Clone)]
struct RawUse {
    /// The module that declared the `use`.
    module: String,
    /// The `::`-joined target path, exactly as written (e.g. `crate::money::Money`).
    path: String,
    /// Whether this is a glob (`use foo::*`). Globs are never resolved.
    glob: bool,
    /// Whether the `use` is `pub` (contributes a re-export binding).
    is_pub: bool,
    /// Alias in `use path as Name`, if any.
    alias: Option<String>,
    /// Where the `use` is written.
    span: SourceSpan,
}

impl Extractor {
    fn new(root: &Path) -> Self {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_rust::LANGUAGE.into())
            .expect("tree-sitter-rust grammar loads");
        Self {
            root: root.to_path_buf(),
            parser,
            graph: Graph::new(),
            seen_modules: BTreeSet::new(),
            seen_files: BTreeSet::new(),
            uses: Vec::new(),
            transitions: Vec::new(),
            impls: Vec::new(),
            associates: Vec::new(),
            calls: Vec::new(),
        }
    }

    /// Resolve collected `use` targets into edges, park the unresolved ones on their module node,
    /// normalise, and return the graph.
    fn finish(mut self) -> Result<Graph> {
        let module_ids: BTreeSet<String> = self
            .graph
            .nodes
            .iter()
            .filter(|n| n.kind == NodeKind::Module)
            .map(|n| n.id.as_str().to_string())
            .collect();
        let reexports = self.build_reexports();
        // Known workspace crate names: the single-segment module ids are the per-crate top-level
        // module nodes (`csd_ir`, `csd_diff`, ..), i.e. the crate-name prefixes this extraction
        // emitted. A `use` whose first segment names one is a sibling-crate reference.
        let crate_names: BTreeSet<String> = module_ids
            .iter()
            .filter(|id| !id.contains("::"))
            .cloned()
            .collect();

        // Resolved edges keyed by (from, to) so repeated uses collapse to one edge (first span).
        let mut edges: BTreeMap<(String, String), SourceSpan> = BTreeMap::new();
        // Unresolved raw targets parked per module.
        let mut unresolved: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();

        for u in &self.uses {
            match resolve_use(u, &module_ids, &reexports, &crate_names) {
                Some(target) if target != u.module => {
                    edges
                        .entry((u.module.clone(), target))
                        .or_insert_with(|| u.span.clone());
                }
                // Resolved to the declaring module itself: a self-reference, not a cross edge.
                Some(_) => {}
                None => {
                    unresolved
                        .entry(u.module.clone())
                        .or_default()
                        .insert(u.path.clone());
                }
            }
        }

        for ((from, to), span) in edges {
            self.graph.edges.push(Edge {
                from: StableId::new(from),
                to: StableId::new(to),
                kind: EdgeKind::Uses,
                span,
                ordinal: None,
            });
        }

        for node in &mut self.graph.nodes {
            if node.kind == NodeKind::Module {
                if let Some(targets) = unresolved.get(node.id.as_str()) {
                    if !targets.is_empty() {
                        let joined = targets.iter().cloned().collect::<Vec<_>>().join(", ");
                        node.attrs.insert("uses".to_string(), joined);
                    }
                }
            }
        }

        // Resolve candidate transitions: emit an edge only when both endpoints name a variant node
        // actually emitted for that enum, and the arm changes variant. Deduped by (from, to).
        let variant_ids: BTreeSet<String> = self
            .graph
            .nodes
            .iter()
            .filter(|n| n.kind == NodeKind::Variant)
            .map(|n| n.id.as_str().to_string())
            .collect();
        let mut transitions: BTreeMap<(String, String), SourceSpan> = BTreeMap::new();
        for t in &self.transitions {
            let from = format!("{}::{}", t.enum_id, t.from);
            let to = format!("{}::{}", t.enum_id, t.to);
            if from == to || !variant_ids.contains(&from) || !variant_ids.contains(&to) {
                continue;
            }
            transitions
                .entry((from, to))
                .or_insert_with(|| t.span.clone());
        }
        for ((from, to), span) in transitions {
            self.graph.edges.push(Edge {
                from: StableId::new(from),
                to: StableId::new(to),
                kind: EdgeKind::Transitions,
                span,
                ordinal: None,
            });
        }

        // Type view (SPEC 3.2). Both endpoints must resolve to nodes this extractor emitted, so
        // external traits/types (`Display`, `String`) and generic params emit nothing.
        let type_ids: BTreeSet<String> = self
            .graph
            .nodes
            .iter()
            .filter(|n| matches!(n.kind, NodeKind::Struct | NodeKind::Enum))
            .map(|n| n.id.as_str().to_string())
            .collect();
        let trait_ids: BTreeSet<String> = self
            .graph
            .nodes
            .iter()
            .filter(|n| n.kind == NodeKind::Trait)
            .map(|n| n.id.as_str().to_string())
            .collect();

        // Implements: from the local type node to the local trait node. Deduped by (from, to).
        let mut implements: BTreeMap<(String, String), SourceSpan> = BTreeMap::new();
        for i in &self.impls {
            let Some(from) =
                resolve_local(&i.type_path, &i.module).filter(|id| type_ids.contains(id))
            else {
                continue;
            };
            let Some(to) =
                resolve_local(&i.trait_path, &i.module).filter(|id| trait_ids.contains(id))
            else {
                continue;
            };
            implements
                .entry((from, to))
                .or_insert_with(|| i.span.clone());
        }
        for ((from, to), span) in implements {
            self.graph.edges.push(Edge {
                from: StableId::new(from),
                to: StableId::new(to),
                kind: EdgeKind::Implements,
                span,
                ordinal: None,
            });
        }

        // Associates: from the owning type node to the field's inner local type/trait node.
        let mut associates: BTreeMap<(String, String), SourceSpan> = BTreeMap::new();
        for a in &self.associates {
            let Some(to) = resolve_local(&a.inner_path, &a.module)
                .filter(|id| type_ids.contains(id) || trait_ids.contains(id))
            else {
                continue;
            };
            associates
                .entry((a.owner_id.clone(), to))
                .or_insert_with(|| a.span.clone());
        }
        for ((from, to), span) in associates {
            self.graph.edges.push(Edge {
                from: StableId::new(from),
                to: StableId::new(to),
                kind: EdgeKind::Associates,
                span,
                ordinal: None,
            });
        }

        // Call graph (SPEC 3.3). Emit a Calls edge only when the callee names a Fn node this
        // extractor emitted; the ordinal is dense per caller in source order. Every call that does
        // not resolve to an emitted Fn bumps the caller's `unresolved_calls` count so the fidelity
        // ceiling stays visible. Deduped by (from, to, ordinal).
        let fn_ids: BTreeSet<String> = self
            .graph
            .nodes
            .iter()
            .filter(|n| n.kind == NodeKind::Fn)
            .map(|n| n.id.as_str().to_string())
            .collect();
        let mut next_ordinal: BTreeMap<String, u32> = BTreeMap::new();
        let mut unresolved_calls: BTreeMap<String, u32> = BTreeMap::new();
        let mut seen: BTreeSet<(String, String, u32)> = BTreeSet::new();
        let mut call_edges: Vec<Edge> = Vec::new();
        for c in &self.calls {
            match &c.callee {
                Some(id) if fn_ids.contains(id) => {
                    let slot = next_ordinal.entry(c.caller.clone()).or_insert(0);
                    let ordinal = *slot;
                    if seen.insert((c.caller.clone(), id.clone(), ordinal)) {
                        call_edges.push(Edge {
                            from: StableId::new(c.caller.clone()),
                            to: StableId::new(id.clone()),
                            kind: EdgeKind::Calls,
                            span: c.span.clone(),
                            ordinal: Some(ordinal),
                        });
                        *slot += 1;
                    }
                }
                _ => {
                    *unresolved_calls.entry(c.caller.clone()).or_insert(0) += 1;
                }
            }
        }
        self.graph.edges.extend(call_edges);
        for node in &mut self.graph.nodes {
            if node.kind == NodeKind::Fn {
                if let Some(n) = unresolved_calls.get(node.id.as_str()) {
                    if *n > 0 {
                        node.attrs
                            .insert("unresolved_calls".to_string(), n.to_string());
                    }
                }
            }
        }

        self.graph.normalize();
        Ok(self.graph)
    }

    /// Build the intra-crate `pub use` re-export bindings: `(module, local_name) -> absolute path`.
    /// Only non-glob `pub use` whose target normalises to an absolute path is recorded.
    fn build_reexports(&self) -> BTreeMap<(String, String), String> {
        let mut map = BTreeMap::new();
        for u in &self.uses {
            if !u.is_pub || u.glob {
                continue;
            }
            let Some(abs) = normalize_path(&u.path, &u.module) else {
                continue;
            };
            let name = u
                .alias
                .clone()
                .or_else(|| abs.rsplit("::").next().map(|s| s.to_string()));
            if let Some(name) = name {
                map.insert((u.module.clone(), name), abs);
            }
        }
        map
    }

    /// Path relative to `root`, forward-slashed, for a [`SourceSpan`].
    fn rel(&self, path: &Path) -> String {
        path.strip_prefix(&self.root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/")
    }

    /// Parse one source file and emit its module node plus everything declared in it.
    fn process_file(&mut self, path: &Path, mod_path: &str) {
        let path = path.to_path_buf();
        if !self.seen_files.insert(path.clone()) {
            return;
        }
        let Ok(src) = std::fs::read_to_string(&path) else {
            return;
        };
        let Some(tree) = self.parser.parse(src.as_bytes(), None) else {
            return;
        };
        let file = self.rel(&path);
        let ctx = FileCtx {
            file_path: &path,
            file: &file,
            src: src.as_bytes(),
        };
        let root_node = tree.root_node();
        self.emit_module(mod_path, span_of(root_node, &file));
        self.process_body(root_node, &ctx, mod_path);
    }

    /// Walk the children of a `source_file` or a module's `declaration_list`.
    fn process_body(&mut self, body: TsNode, ctx: &FileCtx, mod_path: &str) {
        let src = ctx.src;
        let mut docs: Vec<String> = Vec::new();
        let mut cfg_test = false;
        let mut path_attr: Option<String> = None;
        let mut cursor = body.walk();
        for child in body.named_children(&mut cursor) {
            match child.kind() {
                "line_comment" => {
                    if let Some(text) = outer_doc_text(child, src) {
                        docs.push(text);
                    }
                    continue;
                }
                "attribute_item" => {
                    match attribute_kind(child, src) {
                        AttrKind::CfgTest => cfg_test = true,
                        AttrKind::Path(p) => path_attr = Some(p),
                        AttrKind::Other => {}
                    }
                    continue;
                }
                "block_comment" => continue,
                _ => {}
            }

            let doc = docs.join("");
            if !cfg_test {
                self.emit_item(child, ctx, mod_path, &doc, path_attr.as_deref());
            }
            docs.clear();
            cfg_test = false;
            path_attr = None;
        }
    }

    /// Emit the node(s) for one item child, recursing into modules.
    fn emit_item(
        &mut self,
        node: TsNode,
        ctx: &FileCtx,
        mod_path: &str,
        doc: &str,
        path_attr: Option<&str>,
    ) {
        let (src, file) = (ctx.src, ctx.file);
        match node.kind() {
            "struct_item" => {
                if let Some(name) = child_name(node, src) {
                    let id = format!("{mod_path}::{name}");
                    let fp = struct_fingerprint(node, src, doc);
                    self.push(id.clone(), NodeKind::Struct, span_of(node, file), Some(fp));
                    self.collect_struct_associates(node, &id, mod_path, file, src);
                }
            }
            "enum_item" => {
                if let Some(name) = child_name(node, src) {
                    let id = format!("{mod_path}::{name}");
                    let fp = enum_fingerprint(node, src, doc);
                    self.push(id.clone(), NodeKind::Enum, span_of(node, file), Some(fp));
                    self.emit_variants(node, &id, file, src);
                    self.collect_enum_associates(node, &id, mod_path, file, src);
                }
            }
            "trait_item" => {
                if let Some(name) = child_name(node, src) {
                    let id = format!("{mod_path}::{name}");
                    let fp = trait_fingerprint(node, src, doc);
                    self.push(id, NodeKind::Trait, span_of(node, file), Some(fp));
                }
            }
            "function_item" => {
                if let Some(name) = child_name(node, src) {
                    let id = format!("{mod_path}::{name}");
                    let fp = fn_fingerprint(node, src, doc);
                    self.push(id.clone(), NodeKind::Fn, span_of(node, file), Some(fp));
                    if let Some(body) = node.child_by_field_name("body") {
                        self.collect_calls(body, &id, mod_path, None, ctx);
                    }
                }
            }
            "use_declaration" => {
                if let Some(arg) = node.child_by_field_name("argument") {
                    let is_pub = child_kind(node, "visibility_modifier").is_some();
                    let span = span_of(node, file);
                    collect_uses(arg, "", mod_path, is_pub, &span, src, &mut self.uses);
                }
            }
            "impl_item" => self.analyze_impl(node, ctx, mod_path),
            "mod_item" => self.emit_mod(node, ctx, mod_path, path_attr),
            _ => {}
        }
    }

    /// Scan an `impl` block for enum state transitions (SPEC 3.4). No nodes are emitted; candidate
    /// transitions are collected and resolved against the emitted variant set in [`Self::finish`].
    /// The impl's `Self` type is assumed to live in the impl's own module (`mod_path`).
    fn analyze_impl(&mut self, node: TsNode, ctx: &FileCtx, mod_path: &str) {
        let (src, file) = (ctx.src, ctx.file);
        let Some(ty) = node.child_by_field_name("type") else {
            return;
        };
        // `impl Trait for Type`: collect a realization candidate. Inherent impls have no `trait`
        // field and emit nothing. Resolution to local nodes happens in `finish`.
        if let Some(tr) = node.child_by_field_name("trait") {
            let type_path = type_path_text(ty, src);
            let trait_path = type_path_text(tr, src);
            if !type_path.is_empty() && !trait_path.is_empty() {
                self.impls.push(RawImplements {
                    type_path,
                    trait_path,
                    module: mod_path.to_string(),
                    span: span_of(node, file),
                });
            }
        }
        let enum_name = last_ident(ty, src);
        if enum_name.is_empty() {
            return;
        }
        let enum_id = format!("{mod_path}::{enum_name}");
        // Self-type id for method node ids and `self`/`Self` call resolution. Precision-first: a
        // bare or intra-crate type resolves; anything else parks methods under the impl's module.
        let self_type =
            resolve_local(&type_path_text(ty, src), mod_path).unwrap_or_else(|| enum_id.clone());
        let Some(body) = child_kind(node, "declaration_list") else {
            return;
        };
        let mut docs: Vec<String> = Vec::new();
        let mut cfg_test = false;
        let mut cursor = body.walk();
        for item in body.named_children(&mut cursor) {
            match item.kind() {
                "line_comment" => {
                    if let Some(text) = outer_doc_text(item, src) {
                        docs.push(text);
                    }
                    continue;
                }
                "attribute_item" => {
                    if matches!(attribute_kind(item, src), AttrKind::CfgTest) {
                        cfg_test = true;
                    }
                    continue;
                }
                "block_comment" => continue,
                _ => {}
            }
            if item.kind() == "function_item" && !cfg_test {
                if let Some(name) = child_name(item, src) {
                    let doc = docs.join("");
                    let id = format!("{self_type}::{name}");
                    let fp = fn_fingerprint(item, src, &doc);
                    self.push(id.clone(), NodeKind::Fn, span_of(item, file), Some(fp));
                    if let Some(mbody) = item.child_by_field_name("body") {
                        self.collect_calls(mbody, &id, mod_path, Some(&self_type), ctx);
                    }
                    self.analyze_method(item, &enum_id, &enum_name, ctx);
                }
            }
            docs.clear();
            cfg_test = false;
        }
    }

    /// Walk a Fn body for call and method-call expressions in source order, resolving each to a
    /// candidate callee id (or `None`). Nested `function_item`s own their own calls and are skipped.
    fn collect_calls(
        &mut self,
        body: TsNode,
        caller: &str,
        module: &str,
        self_type: Option<&str>,
        ctx: &FileCtx,
    ) {
        let mut out = Vec::new();
        walk_calls(body, caller, module, self_type, ctx.src, ctx.file, &mut out);
        self.calls.extend(out);
    }

    /// Read transitions out of one method whose receiver (or a parameter) is the enum type.
    fn analyze_method(&mut self, node: TsNode, enum_id: &str, enum_name: &str, ctx: &FileCtx) {
        let (src, file) = (ctx.src, ctx.file);
        let Some(body) = node.child_by_field_name("body") else {
            return;
        };
        let mut has_self = false;
        let mut enum_params: BTreeSet<String> = BTreeSet::new();
        if let Some(params) = child_kind(node, "parameters") {
            let mut cursor = params.walk();
            for param in params.named_children(&mut cursor) {
                match param.kind() {
                    "self_parameter" => has_self = true,
                    "parameter" => {
                        let is_enum = param
                            .child_by_field_name("type")
                            .map(|t| last_ident(t, src) == enum_name)
                            .unwrap_or(false);
                        if is_enum {
                            if let Some(pat) = param.child_by_field_name("pattern") {
                                if pat.kind() == "identifier" {
                                    enum_params.insert(text(pat, src));
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }

        let mut matches = Vec::new();
        collect_match_exprs(body, &mut matches);
        for m in matches {
            let Some(value) = m.child_by_field_name("value") else {
                continue;
            };
            if !is_enum_scrutinee(value, has_self, &enum_params, src) {
                continue;
            }
            let Some(block) = m.child_by_field_name("body") else {
                continue;
            };
            let mut cursor = block.walk();
            for arm in block.named_children(&mut cursor) {
                if arm.kind() != "match_arm" {
                    continue;
                }
                let Some(from) = arm
                    .child_by_field_name("pattern")
                    .and_then(|p| variant_from_pattern(p, enum_name, src))
                else {
                    continue;
                };
                let Some(value) = arm.child_by_field_name("value") else {
                    continue;
                };
                for to in arm_to_variants(value, enum_name, src) {
                    self.transitions.push(RawTransition {
                        enum_id: enum_id.to_string(),
                        from: from.clone(),
                        to,
                        span: span_of(arm, file),
                    });
                }
            }
        }
    }

    /// Emit an inline or file-backed submodule and recurse into it.
    fn emit_mod(&mut self, node: TsNode, ctx: &FileCtx, mod_path: &str, path_attr: Option<&str>) {
        let (src, file) = (ctx.src, ctx.file);
        let Some(name) = child_name(node, src) else {
            return;
        };
        // `mod tests` is test scaffolding; excluded by default (SPEC 3.4).
        if name == "tests" {
            return;
        }
        let child_path = format!("{mod_path}::{name}");
        if let Some(body) = child_kind(node, "declaration_list") {
            self.emit_module(&child_path, span_of(node, file));
            self.process_body(body, ctx, &child_path);
        } else if let Some(target) = resolve_mod_file(ctx.file_path, &name, path_attr) {
            self.process_file(&target, &child_path);
        } else {
            // Declared but the backing file was not found; record the module anyway.
            self.emit_module(&child_path, span_of(node, file));
        }
    }

    /// Emit a `Variant` node per enum variant (SPEC 3.4 state view).
    fn emit_variants(&mut self, enum_node: TsNode, enum_id: &str, file: &str, src: &[u8]) {
        let Some(list) = child_kind(enum_node, "enum_variant_list") else {
            return;
        };
        let mut cursor = list.walk();
        for variant in list.named_children(&mut cursor) {
            if variant.kind() != "enum_variant" {
                continue;
            }
            if let Some(name) = child_name(variant, src) {
                let id = format!("{enum_id}::{name}");
                self.push(id, NodeKind::Variant, span_of(variant, file), None);
            }
        }
    }

    /// Collect association candidates from a struct's fields (named or tuple).
    fn collect_struct_associates(
        &mut self,
        node: TsNode,
        owner_id: &str,
        mod_path: &str,
        file: &str,
        src: &[u8],
    ) {
        if let Some(list) = child_kind(node, "field_declaration_list") {
            let mut cursor = list.walk();
            for field in list.named_children(&mut cursor) {
                if field.kind() != "field_declaration" {
                    continue;
                }
                if let Some(ty) = field.child_by_field_name("type") {
                    self.push_associates(ty, owner_id, mod_path, file, src);
                }
            }
        } else if let Some(list) = child_kind(node, "ordered_field_declaration_list") {
            let mut cursor = list.walk();
            for child in list.named_children(&mut cursor) {
                if is_type(child.kind()) {
                    self.push_associates(child, owner_id, mod_path, file, src);
                }
            }
        }
    }

    /// Collect association candidates from an enum's variant payloads. The owner is the enum type.
    fn collect_enum_associates(
        &mut self,
        node: TsNode,
        owner_id: &str,
        mod_path: &str,
        file: &str,
        src: &[u8],
    ) {
        let Some(list) = child_kind(node, "enum_variant_list") else {
            return;
        };
        let mut cursor = list.walk();
        for variant in list.named_children(&mut cursor) {
            if variant.kind() != "enum_variant" {
                continue;
            }
            if let Some(payload) = child_kind(variant, "ordered_field_declaration_list") {
                let mut vc = payload.walk();
                for child in payload.named_children(&mut vc) {
                    if is_type(child.kind()) {
                        self.push_associates(child, owner_id, mod_path, file, src);
                    }
                }
            } else if let Some(payload) = child_kind(variant, "field_declaration_list") {
                let mut vc = payload.walk();
                for field in payload.named_children(&mut vc) {
                    if let Some(ty) = field.child_by_field_name("type") {
                        self.push_associates(ty, owner_id, mod_path, file, src);
                    }
                }
            }
        }
    }

    /// Unwrap a field type and record one association candidate per reachable inner type path.
    fn push_associates(
        &mut self,
        ty: TsNode,
        owner_id: &str,
        mod_path: &str,
        file: &str,
        src: &[u8],
    ) {
        let mut paths = Vec::new();
        collect_assoc_paths(ty, src, &mut paths);
        let span = span_of(ty, file);
        for inner_path in paths {
            self.associates.push(RawAssociates {
                owner_id: owner_id.to_string(),
                inner_path,
                module: mod_path.to_string(),
                span: span.clone(),
            });
        }
    }

    /// Emit a module node once per path.
    fn emit_module(&mut self, path: &str, span: SourceSpan) {
        if self.seen_modules.insert(path.to_string()) {
            self.push(path.to_string(), NodeKind::Module, span, None);
        }
    }

    fn push(
        &mut self,
        id: String,
        kind: NodeKind,
        span: SourceSpan,
        fingerprint: Option<Fingerprint>,
    ) {
        self.graph.nodes.push(Node {
            id: StableId::new(id),
            kind,
            span,
            attrs: BTreeMap::new(),
            fingerprint,
        });
    }
}

/// Resolve a `mod name;` declaration to a file, honouring `#[path]`.
fn resolve_mod_file(declaring_file: &Path, name: &str, path_attr: Option<&str>) -> Option<PathBuf> {
    let dir = declaring_file.parent()?;
    if let Some(rel) = path_attr {
        let candidate = dir.join(rel);
        return candidate.exists().then_some(candidate);
    }
    // Submodules of `mod.rs`/`lib.rs`/`main.rs` live beside the file; of `foo.rs`, under `foo/`.
    let sub_dir = match declaring_file.file_stem().and_then(|s| s.to_str()) {
        Some("mod" | "lib" | "main") => dir.to_path_buf(),
        Some(stem) => dir.join(stem),
        None => dir.to_path_buf(),
    };
    let flat = sub_dir.join(format!("{name}.rs"));
    if flat.exists() {
        return Some(flat);
    }
    let nested = sub_dir.join(name).join("mod.rs");
    nested.exists().then_some(nested)
}

/// Flatten a `use` argument node into one [`RawUse`] per target path, expanding `{..}` groups.
fn collect_uses(
    node: TsNode,
    prefix: &str,
    module: &str,
    is_pub: bool,
    span: &SourceSpan,
    src: &[u8],
    out: &mut Vec<RawUse>,
) {
    match node.kind() {
        "identifier" | "scoped_identifier" | "crate" | "self" | "super" | "metavariable" => {
            out.push(RawUse {
                module: module.to_string(),
                path: join_path(prefix, &text(node, src)),
                glob: false,
                is_pub,
                alias: None,
                span: span.clone(),
            });
        }
        "use_wildcard" => {
            let base = match node.named_child(0) {
                Some(inner) => join_path(prefix, &text(inner, src)),
                None => prefix.to_string(),
            };
            out.push(RawUse {
                module: module.to_string(),
                path: base,
                glob: true,
                is_pub,
                alias: None,
                span: span.clone(),
            });
        }
        "use_as_clause" => {
            let path = node
                .child_by_field_name("path")
                .map(|n| text(n, src))
                .unwrap_or_default();
            let alias = node.child_by_field_name("alias").map(|n| text(n, src));
            out.push(RawUse {
                module: module.to_string(),
                path: join_path(prefix, &path),
                glob: false,
                is_pub,
                alias,
                span: span.clone(),
            });
        }
        "scoped_use_list" => {
            let new_prefix = match node.child_by_field_name("path") {
                Some(p) => join_path(prefix, &text(p, src)),
                None => prefix.to_string(),
            };
            if let Some(list) = node.child_by_field_name("list") {
                let mut cursor = list.walk();
                for item in list.named_children(&mut cursor) {
                    collect_uses(item, &new_prefix, module, is_pub, span, src, out);
                }
            }
        }
        "use_list" => {
            let mut cursor = node.walk();
            for item in node.named_children(&mut cursor) {
                collect_uses(item, prefix, module, is_pub, span, src, out);
            }
        }
        _ => {}
    }
}

/// Join a path prefix and a segment with `::` (either side may be empty).
fn join_path(prefix: &str, seg: &str) -> String {
    match (prefix.is_empty(), seg.is_empty()) {
        (true, _) => seg.to_string(),
        (_, true) => prefix.to_string(),
        _ => format!("{prefix}::{seg}"),
    }
}

/// Rewrite an intra-crate `use` path (`crate::`/`self::`/`super::`) into an absolute id under the
/// declaring module's crate prefix. Returns `None` for anything not certainly intra-crate
/// (external crates, `std`, bare names): those are left unresolved on purpose.
fn normalize_path(path: &str, module: &str) -> Option<String> {
    let mut segs = path.split("::").filter(|s| !s.is_empty());
    let first = segs.next()?;
    let rest: Vec<&str> = segs.collect();
    let mod_segs: Vec<&str> = module.split("::").collect();
    let crate_prefix = mod_segs.first().copied().unwrap_or("crate");
    match first {
        "crate" => {
            let mut out = vec![crate_prefix.to_string()];
            out.extend(rest.iter().map(|s| s.to_string()));
            Some(out.join("::"))
        }
        "self" => {
            let mut out: Vec<String> = mod_segs.iter().map(|s| s.to_string()).collect();
            out.extend(rest.iter().map(|s| s.to_string()));
            Some(out.join("::"))
        }
        "super" => {
            // Pop one module segment for the leading `super`, then for each further `super`.
            let mut base: Vec<&str> = mod_segs.clone();
            if base.is_empty() {
                return None;
            }
            base.pop();
            let mut tail = &rest[..];
            while tail.first() == Some(&"super") {
                if base.is_empty() {
                    return None;
                }
                base.pop();
                tail = &tail[1..];
            }
            let mut out: Vec<String> = base.iter().map(|s| s.to_string()).collect();
            out.extend(tail.iter().map(|s| s.to_string()));
            Some(out.join("::"))
        }
        _ => None,
    }
}

/// Resolve one `use` target to the module it references, or `None` when uncertain. Globs are never
/// resolved. Explicit intra-crate paths and `pub use` re-export chains are followed to the owning
/// module of the final item.
///
/// Cross-crate (backlog `extract-crossrate`, id `422be96`): when the path is not intra-crate but its
/// first segment names a sibling workspace crate (a crate root emitted by this extraction), it
/// resolves to the deepest existing module node along the path, else that crate's root. Non-workspace
/// first segments (`std`, `tokio`, `serde`, ..) still stay unresolved so no external edge is invented.
fn resolve_use(
    u: &RawUse,
    module_ids: &BTreeSet<String>,
    reexports: &BTreeMap<(String, String), String>,
    crate_names: &BTreeSet<String>,
) -> Option<String> {
    if u.glob {
        return None;
    }
    if let Some(abs) = normalize_path(&u.path, &u.module) {
        let resolved = follow_reexports(abs, reexports);
        return owning_module(&resolved, module_ids);
    }
    // Not intra-crate: resolve only if the first segment names a known workspace crate. The own
    // crate name resolves like intra-crate; a self-loop (target == declaring module) is dropped by
    // the caller's `target != u.module` guard, so no crate->crate self-edge is emitted.
    let first = u.path.split("::").find(|s| !s.is_empty())?;
    if !crate_names.contains(first) {
        return None;
    }
    let resolved = follow_reexports(u.path.clone(), reexports);
    deepest_module(&resolved, module_ids)
}

/// The deepest prefix of `path` that names a known module node, walking segments off the tail. For a
/// sibling-crate path this is the item's owning module, else a deeper module, else the crate root.
fn deepest_module(path: &str, module_ids: &BTreeSet<String>) -> Option<String> {
    let mut cur = path;
    loop {
        if module_ids.contains(cur) {
            return Some(cur.to_string());
        }
        match cur.rsplit_once("::") {
            Some((parent, _)) => cur = parent,
            None => return None,
        }
    }
}

/// Follow `pub use` re-export bindings until the path no longer names a re-export. Bounded to
/// guard against pathological cycles.
fn follow_reexports(mut path: String, reexports: &BTreeMap<(String, String), String>) -> String {
    for _ in 0..16 {
        let Some((parent, name)) = path.rsplit_once("::") else {
            break;
        };
        match reexports.get(&(parent.to_string(), name.to_string())) {
            Some(next) if next != &path => path = next.clone(),
            _ => break,
        }
    }
    path
}

/// The module that owns `path`: `path` itself if it is a module, else its parent if that is a
/// module. `None` when neither is a known module (so we never invent an edge).
fn owning_module(path: &str, module_ids: &BTreeSet<String>) -> Option<String> {
    if module_ids.contains(path) {
        return Some(path.to_string());
    }
    let (parent, _) = path.rsplit_once("::")?;
    module_ids.contains(parent).then(|| parent.to_string())
}

/// Kinds of attribute we care about on an item.
enum AttrKind {
    CfgTest,
    Path(String),
    Other,
}

/// Classify an `attribute_item`: `#[cfg(test)]`, `#[path = "..."]`, or anything else.
fn attribute_kind(attr_item: TsNode, src: &[u8]) -> AttrKind {
    let Some(attr) = child_kind(attr_item, "attribute") else {
        return AttrKind::Other;
    };
    let name = attr
        .named_child(0)
        .filter(|n| n.kind() == "identifier")
        .map(|n| text(n, src));
    match name.as_deref() {
        Some("cfg") => {
            if let Some(tt) = child_kind(attr, "token_tree") {
                if text(tt, src).contains("test") {
                    return AttrKind::CfgTest;
                }
            }
            AttrKind::Other
        }
        Some("path") => {
            match child_kind(attr, "string_literal").and_then(|s| child_kind(s, "string_content")) {
                Some(content) => AttrKind::Path(text(content, src)),
                None => AttrKind::Other,
            }
        }
        _ => AttrKind::Other,
    }
}

/// Fingerprint a `struct_item`: field names and unwrapped field types.
fn struct_fingerprint(node: TsNode, src: &[u8], doc: &str) -> Fingerprint {
    let mut fp = base_fingerprint(doc);
    if let Some(list) = child_kind(node, "field_declaration_list") {
        let mut cursor = list.walk();
        for field in list.named_children(&mut cursor) {
            if field.kind() != "field_declaration" {
                continue;
            }
            if let Some(name) = field.child_by_field_name("name") {
                fp.members.insert(text(name, src));
            }
            if let Some(ty) = field.child_by_field_name("type") {
                add_types(ty, src, &mut fp.field_types);
            }
        }
    } else if let Some(list) = child_kind(node, "ordered_field_declaration_list") {
        add_ordered_types(list, src, &mut fp.field_types);
    }
    fp
}

/// Fingerprint an `enum_item`: variant names and the types in variant payloads.
fn enum_fingerprint(node: TsNode, src: &[u8], doc: &str) -> Fingerprint {
    let mut fp = base_fingerprint(doc);
    if let Some(list) = child_kind(node, "enum_variant_list") {
        let mut cursor = list.walk();
        for variant in list.named_children(&mut cursor) {
            if variant.kind() != "enum_variant" {
                continue;
            }
            if let Some(name) = child_name(variant, src) {
                fp.members.insert(name);
            }
            if let Some(payload) = child_kind(variant, "ordered_field_declaration_list") {
                add_ordered_types(payload, src, &mut fp.field_types);
            } else if let Some(payload) = child_kind(variant, "field_declaration_list") {
                let mut vc = payload.walk();
                for field in payload.named_children(&mut vc) {
                    if let Some(ty) = field.child_by_field_name("type") {
                        add_types(ty, src, &mut fp.field_types);
                    }
                }
            }
        }
    }
    fp
}

/// Fingerprint a `trait_item`: member names only. `field_types` stays empty (trait method
/// signatures are refined with the types view, backlog M3).
fn trait_fingerprint(node: TsNode, src: &[u8], doc: &str) -> Fingerprint {
    let mut fp = base_fingerprint(doc);
    if let Some(list) = child_kind(node, "declaration_list") {
        let mut cursor = list.walk();
        for item in list.named_children(&mut cursor) {
            match item.kind() {
                "function_item" | "function_signature_item" | "associated_type" | "const_item" => {
                    if let Some(name) = child_name(item, src) {
                        fp.members.insert(name);
                    }
                }
                _ => {}
            }
        }
    }
    fp
}

/// Fingerprint a `function_item`: parameter names and the parameter/return types.
fn fn_fingerprint(node: TsNode, src: &[u8], doc: &str) -> Fingerprint {
    let mut fp = base_fingerprint(doc);
    if let Some(params) = child_kind(node, "parameters") {
        let mut cursor = params.walk();
        for param in params.named_children(&mut cursor) {
            if param.kind() != "parameter" {
                continue; // skip `self_parameter`, variadics
            }
            if let Some(pat) = param.child_by_field_name("pattern") {
                if pat.kind() == "identifier" {
                    fp.members.insert(text(pat, src));
                }
            }
            if let Some(ty) = param.child_by_field_name("type") {
                add_types(ty, src, &mut fp.field_types);
            }
        }
    }
    if let Some(ret) = node.child_by_field_name("return_type") {
        add_types(ret, src, &mut fp.field_types);
    }
    fp
}

/// A fingerprint with only the doc hash set.
fn base_fingerprint(doc: &str) -> Fingerprint {
    Fingerprint {
        doc_hash: doc_hash(doc.trim()),
        ..Default::default()
    }
}

/// Add each type in an `ordered_field_declaration_list` (tuple struct / tuple variant).
fn add_ordered_types(list: TsNode, src: &[u8], out: &mut BTreeMap<String, u32>) {
    let mut cursor = list.walk();
    for child in list.named_children(&mut cursor) {
        if is_type(child.kind()) {
            add_types(child, src, out);
        }
    }
}

/// Collect type names from a type node into the multiset, unwrapping [`WRAPPERS`].
fn add_types(node: TsNode, src: &[u8], out: &mut BTreeMap<String, u32>) {
    for name in type_names(node, src) {
        *out.entry(name).or_insert(0) += 1;
    }
}

/// The associated type names of a type node, after unwrapping `Arc`/`Box`/`Rc`/`Vec`/`Option`.
fn type_names(node: TsNode, src: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    collect_type_names(node, src, &mut out);
    out
}

fn collect_type_names(node: TsNode, src: &[u8], out: &mut Vec<String>) {
    match node.kind() {
        "type_identifier" | "primitive_type" => out.push(text(node, src)),
        "scoped_type_identifier" => {
            if let Some(name) = node.child_by_field_name("name") {
                out.push(text(name, src));
            }
        }
        "generic_type" => {
            let base = node.named_child(0);
            let base_name = base.map(|b| ident_of(b, src)).unwrap_or_default();
            let args = child_kind(node, "type_arguments");
            if WRAPPERS.contains(&base_name.as_str()) {
                if let Some(args) = args {
                    let mut cursor = args.walk();
                    for arg in args.named_children(&mut cursor) {
                        if is_type(arg.kind()) {
                            collect_type_names(arg, src, out);
                        }
                    }
                }
            } else {
                out.push(base_name);
            }
        }
        "reference_type" => {
            if let Some(inner) = node.child_by_field_name("type") {
                collect_type_names(inner, src, out);
            }
        }
        "tuple_type" => {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                if is_type(child.kind()) {
                    collect_type_names(child, src, out);
                }
            }
        }
        "array_type" | "slice_type" => {
            if let Some(inner) = node.child_by_field_name("element") {
                collect_type_names(inner, src, out);
            }
        }
        "dynamic_type" => {
            if let Some(inner) = node.named_child(0) {
                collect_type_names(inner, src, out);
            }
        }
        _ => {}
    }
}

/// Collect the *path* of each reachable inner type of a field type, unwrapping [`WRAPPERS`] and
/// references (mirrors [`collect_type_names`] but keeps the full path so `crate::`-qualified names
/// stay resolvable). Primitives are skipped since they are never local.
fn collect_assoc_paths(node: TsNode, src: &[u8], out: &mut Vec<String>) {
    match node.kind() {
        "type_identifier" | "scoped_type_identifier" => out.push(text(node, src)),
        "generic_type" => {
            let base = node.named_child(0);
            let base_name = base.map(|b| ident_of(b, src)).unwrap_or_default();
            if WRAPPERS.contains(&base_name.as_str()) {
                if let Some(args) = child_kind(node, "type_arguments") {
                    let mut cursor = args.walk();
                    for arg in args.named_children(&mut cursor) {
                        if is_type(arg.kind()) {
                            collect_assoc_paths(arg, src, out);
                        }
                    }
                }
            } else if let Some(base) = base {
                out.push(text(base, src));
            }
        }
        "reference_type" => {
            if let Some(inner) = node.child_by_field_name("type") {
                collect_assoc_paths(inner, src, out);
            }
        }
        "tuple_type" => {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                if is_type(child.kind()) {
                    collect_assoc_paths(child, src, out);
                }
            }
        }
        "array_type" | "slice_type" => {
            if let Some(inner) = node.child_by_field_name("element") {
                collect_assoc_paths(inner, src, out);
            }
        }
        "dynamic_type" => {
            if let Some(inner) = node.named_child(0) {
                collect_assoc_paths(inner, src, out);
            }
        }
        _ => {}
    }
}

/// The type path text for an impl's `Self` type or trait: the full path for a plain or scoped
/// name, or the base path of a generic (`Foo<T>` -> `Foo`, `a::b::Foo<T>` -> `a::b::Foo`).
fn type_path_text(node: TsNode, src: &[u8]) -> String {
    match node.kind() {
        "type_identifier" | "scoped_type_identifier" => text(node, src),
        "generic_type" => node
            .named_child(0)
            .map(|b| text(b, src))
            .unwrap_or_default(),
        _ => String::new(),
    }
}

/// Resolve a type/trait name written in `module` to an absolute node id, precision-first. A
/// `crate::`/`self::`/`super::` path goes through [`normalize_path`]; a bare name binds to the
/// declaring module (`{module}::{name}`); any other scoped path is treated as external (`None`).
fn resolve_local(path: &str, module: &str) -> Option<String> {
    let first = path.split("::").next().unwrap_or("");
    if matches!(first, "crate" | "self" | "super") {
        normalize_path(path, module)
    } else if path.is_empty() || path.contains("::") {
        None
    } else {
        Some(format!("{module}::{path}"))
    }
}

/// Walk `node` in source order, recording each `call_expression` as a [`RawCall`]. Method calls are
/// `call_expression`s whose function is a `field_expression` (`self.other()`). Recursion is
/// pre-order so an outer call precedes calls in its arguments. Nested `function_item`s are skipped
/// so their calls are not attributed to the enclosing caller.
fn walk_calls(
    node: TsNode,
    caller: &str,
    module: &str,
    self_type: Option<&str>,
    src: &[u8],
    file: &str,
    out: &mut Vec<RawCall>,
) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() == "function_item" {
            continue;
        }
        if child.kind() == "call_expression" {
            out.push(RawCall {
                caller: caller.to_string(),
                callee: resolve_call(child, module, self_type, src),
                span: span_of(child, file),
            });
        }
        walk_calls(child, caller, module, self_type, src, file, out);
    }
}

/// Resolve a `call_expression`'s callee, precision-first. A bare `foo()` binds to the caller's
/// module; `crate::`/`self::`/`super::` paths normalise; `Self::assoc()` and `self.method()` bind
/// to the Self type. Any other scoped path or receiver stays unresolved (`None`) since the callee
/// is not known statically (SPEC 3.3 ceiling).
fn resolve_call(node: TsNode, module: &str, self_type: Option<&str>, src: &[u8]) -> Option<String> {
    let func = node.child_by_field_name("function")?;
    match func.kind() {
        "identifier" => resolve_local(&text(func, src), module),
        "scoped_identifier" => {
            let full = text(func, src);
            if full.split("::").next() == Some("Self") {
                let name = full.rsplit("::").next()?;
                self_type.map(|st| format!("{st}::{name}"))
            } else {
                resolve_local(&full, module)
            }
        }
        "field_expression" => {
            let value = func.child_by_field_name("value")?;
            if value.kind() != "self" {
                return None;
            }
            let field = func.child_by_field_name("field")?;
            self_type.map(|st| format!("{st}::{}", text(field, src)))
        }
        _ => None,
    }
}

/// The trailing identifier of a possibly-scoped type name (`a::b::T` -> `T`).
fn ident_of(node: TsNode, src: &[u8]) -> String {
    match node.kind() {
        "scoped_type_identifier" => node
            .child_by_field_name("name")
            .map(|n| text(n, src))
            .unwrap_or_default(),
        _ => text(node, src),
    }
}

/// Whether a node kind denotes a type (used to skip lifetimes and punctuation).
fn is_type(kind: &str) -> bool {
    kind.ends_with("_type")
        || matches!(
            kind,
            "type_identifier" | "scoped_type_identifier" | "primitive_type"
        )
}

/// The `name`-field identifier of an item, as text.
fn child_name(node: TsNode, src: &[u8]) -> Option<String> {
    node.child_by_field_name("name").map(|n| text(n, src))
}

/// First direct child of the given kind.
fn child_kind<'a>(node: TsNode<'a>, kind: &str) -> Option<TsNode<'a>> {
    let mut cursor = node.walk();
    let found = node.children(&mut cursor).find(|c| c.kind() == kind);
    found
}

/// Collect every `match_expression` reachable under `node` (including nested ones).
fn collect_match_exprs<'a>(node: TsNode<'a>, out: &mut Vec<TsNode<'a>>) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() == "match_expression" {
            out.push(child);
        }
        collect_match_exprs(child, out);
    }
}

/// Whether a match scrutinee is the enum: `self`/`*self`/`&self` (in an impl of the enum) or a
/// parameter typed as the enum.
fn is_enum_scrutinee(
    value: TsNode,
    has_self: bool,
    enum_params: &BTreeSet<String>,
    src: &[u8],
) -> bool {
    match value.kind() {
        "self" => has_self,
        "identifier" => enum_params.contains(&text(value, src)),
        "unary_expression" | "reference_expression" | "parenthesized_expression" => {
            has_self && has_self_descendant(value)
        }
        _ => false,
    }
}

/// Whether `node` contains a `self` token (used for `*self` / `&self` scrutinees and targets).
fn has_self_descendant(node: TsNode) -> bool {
    let mut cursor = node.walk();
    let found = node
        .named_children(&mut cursor)
        .any(|c| c.kind() == "self" || has_self_descendant(c));
    found
}

/// The from-variant named by a `match_pattern`, if it resolves to this enum. The pattern is the
/// first child; a trailing `if` guard is ignored.
fn variant_from_pattern(match_pattern: TsNode, enum_name: &str, src: &[u8]) -> Option<String> {
    let pat = match_pattern.named_child(0)?;
    match pat.kind() {
        "scoped_identifier" => scoped_variant(pat, enum_name, src),
        "identifier" => Some(text(pat, src)),
        "tuple_struct_pattern" | "struct_pattern" => pat
            .child_by_field_name("type")
            .and_then(|t| variant_from_type_path(t, enum_name, src)),
        _ => None,
    }
}

/// The to-variant(s) produced by a match arm: a direct variant expression, a `*self`/`self.field`
/// assignment, or a returned variant. Bounded to the arm value and its direct statements.
fn arm_to_variants(value: TsNode, enum_name: &str, src: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    collect_tos(value, enum_name, src, &mut out);
    out
}

fn collect_tos(node: TsNode, enum_name: &str, src: &[u8], out: &mut Vec<String>) {
    match node.kind() {
        "block" => {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                collect_tos(child, enum_name, src, out);
            }
        }
        "expression_statement" => {
            if let Some(inner) = node.named_child(0) {
                collect_tos(inner, enum_name, src, out);
            }
        }
        "return_expression" => {
            if let Some(inner) = node.named_child(0) {
                if let Some(v) = variant_of_expr(inner, enum_name, src) {
                    out.push(v);
                }
            }
        }
        "assignment_expression" => {
            let self_target = node
                .child_by_field_name("left")
                .map(is_self_target)
                .unwrap_or(false);
            if self_target {
                if let Some(v) = node
                    .child_by_field_name("right")
                    .and_then(|r| variant_of_expr(r, enum_name, src))
                {
                    out.push(v);
                }
            }
        }
        _ => {
            if let Some(v) = variant_of_expr(node, enum_name, src) {
                out.push(v);
            }
        }
    }
}

/// Whether `left` is a `*self` or `self.field` assignment target.
fn is_self_target(left: TsNode) -> bool {
    match left.kind() {
        "unary_expression" => has_self_descendant(left),
        "field_expression" => left
            .child_by_field_name("value")
            .map(|v| v.kind() == "self")
            .unwrap_or(false),
        _ => false,
    }
}

/// The variant named by an expression that constructs one: `Enum::V`, `Self::V`, bare `V`,
/// `Enum::V(..)`, or `Self::V { .. }`.
fn variant_of_expr(node: TsNode, enum_name: &str, src: &[u8]) -> Option<String> {
    match node.kind() {
        "scoped_identifier" => scoped_variant(node, enum_name, src),
        "identifier" | "type_identifier" => Some(text(node, src)),
        "call_expression" => node
            .child_by_field_name("function")
            .and_then(|f| variant_of_expr(f, enum_name, src)),
        "struct_expression" => node
            .child_by_field_name("name")
            .and_then(|t| variant_from_type_path(t, enum_name, src)),
        _ => None,
    }
}

/// The variant name from a `scoped_identifier` whose path is the enum or `Self`.
fn scoped_variant(node: TsNode, enum_name: &str, src: &[u8]) -> Option<String> {
    let path = node.child_by_field_name("path")?;
    if !path_is_enum(path, enum_name, src) {
        return None;
    }
    node.child_by_field_name("name").map(|n| text(n, src))
}

/// The variant name from a pattern/struct type path (`Enum::V`, `Self::V`, bare `V`).
fn variant_from_type_path(node: TsNode, enum_name: &str, src: &[u8]) -> Option<String> {
    match node.kind() {
        "identifier" | "type_identifier" => Some(text(node, src)),
        "scoped_identifier" | "scoped_type_identifier" => {
            let path = node.child_by_field_name("path")?;
            if !path_is_enum(path, enum_name, src) {
                return None;
            }
            node.child_by_field_name("name").map(|n| text(n, src))
        }
        _ => None,
    }
}

/// Whether a scope path's trailing segment is the enum name or `Self`.
fn path_is_enum(path: TsNode, enum_name: &str, src: &[u8]) -> bool {
    let last = last_ident(path, src);
    last == enum_name || last == "Self"
}

/// The trailing identifier of a possibly-scoped path or type (`a::b::T` -> `T`).
fn last_ident(node: TsNode, src: &[u8]) -> String {
    match node.kind() {
        "scoped_identifier" | "scoped_type_identifier" => node
            .child_by_field_name("name")
            .map(|n| text(n, src))
            .unwrap_or_default(),
        "generic_type" => node
            .child_by_field_name("type")
            .or_else(|| node.named_child(0))
            .map(|t| last_ident(t, src))
            .unwrap_or_default(),
        _ => text(node, src),
    }
}

/// The outer-doc text (`///`) of a `line_comment`, or `None` for a regular comment.
fn outer_doc_text(line_comment: TsNode, src: &[u8]) -> Option<String> {
    let is_outer = child_kind(line_comment, "outer_doc_comment_marker").is_some();
    if !is_outer {
        return None;
    }
    child_kind(line_comment, "doc_comment").map(|c| text(c, src))
}

/// UTF-8 text of a node.
fn text(node: TsNode, src: &[u8]) -> String {
    node.utf8_text(src).unwrap_or_default().to_string()
}

/// A [`SourceSpan`] covering a node.
fn span_of(node: TsNode, file: &str) -> SourceSpan {
    SourceSpan {
        file: file.to_string(),
        start: node.start_byte() as u32,
        end: node.end_byte() as u32,
    }
}

#[cfg(test)]
mod tests;
