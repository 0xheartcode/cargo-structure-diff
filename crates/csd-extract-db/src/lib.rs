//! Diesel `schema.rs` extraction: `table!` / `joinable!` macros to schema-view IR.
//!
//! Scope (backlog `schema-er-view`, id 3aefc9b): parse Diesel-generated `schema.rs` into
//! [`NodeKind::Table`] nodes carrying their columns, and [`EdgeKind::ForeignKey`] edges. The Mermaid
//! `erDiagram` render and the `destructive_migration` lint are separate, later work and are not built
//! here. SeaORM entity models and raw SQL are out of scope for this issue; they are future backends.
//!
//! # What is parsed
//!
//! Extraction is precision-first and driven off `macro_invocation` nodes whose trailing macro-path
//! segment is `table` or `joinable` (bare or `diesel::`-qualified). The token tree inside each such
//! macro is parsed by text, so grammar tokenization details of the macro body do not matter.
//!
//! `table! { users (id) { id -> Int4, name -> Varchar } }` becomes a [`NodeKind::Table`] node whose
//! id is the table name (`users`). Its columns are recorded on the node's `columns` attribute as a
//! `name: Type` list sorted by column name so output is byte-stable. The primary-key group `(id)` is
//! not modelled in v1.
//!
//! `joinable!(posts -> users (user_id))` becomes an [`EdgeKind::ForeignKey`] edge from the child
//! table (`posts`) to the parent table (`users`). The edge is emitted only when both endpoints name
//! known [`NodeKind::Table`] nodes, so a join to an unknown table is dropped rather than invented.
//! [`Edge`] has no attribute map, so the foreign-key column (`user_id`) is not retained in v1.
//!
//! `allow_tables_to_appear_in_same_query!` and any macro shape not confidently understood are
//! ignored.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::Result;
use csd_ir::{Edge, EdgeKind, Graph, Node, NodeKind, SourceSpan, StableId};
use tree_sitter::{Node as TsNode, Parser};

/// Extract a schema-view graph from the Diesel `schema.rs` files under `root`.
///
/// Finds every `schema.rs` (skipping `target/` and `.git/`), parses each with tree-sitter-rust, and
/// emits a [`NodeKind::Table`] node per `table!` macro and an [`EdgeKind::ForeignKey`] edge per
/// `joinable!` whose endpoints are both known tables. Output is normalised so it is byte-stable.
pub fn extract_schema(root: &Path) -> Result<Graph> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_rust::LANGUAGE.into())
        .expect("tree-sitter-rust grammar loads");

    let mut tables: Vec<RawTable> = Vec::new();
    let mut joins: Vec<RawJoin> = Vec::new();
    for path in discover_schema_files(root) {
        let Ok(src) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Some(tree) = parser.parse(src.as_bytes(), None) else {
            continue;
        };
        let file = rel(root, &path);
        let bytes = src.as_bytes();
        let mut macros = Vec::new();
        collect_macros(tree.root_node(), &mut macros);
        for m in macros {
            match macro_name(m, bytes).as_deref() {
                Some("table") => {
                    if let Some(t) = parse_table(m, bytes, &file) {
                        tables.push(t);
                    }
                }
                Some("joinable") => {
                    if let Some(j) = parse_joinable(m, bytes, &file) {
                        joins.push(j);
                    }
                }
                _ => {}
            }
        }
    }

    let mut graph = Graph::new();
    // First `table!` for a given name wins; a duplicate definition is ignored.
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for t in tables {
        if !seen.insert(t.name.clone()) {
            continue;
        }
        let mut attrs = BTreeMap::new();
        if !t.columns.is_empty() {
            attrs.insert("columns".to_string(), t.columns);
        }
        graph.nodes.push(Node {
            id: StableId::new(t.name),
            kind: NodeKind::Table,
            span: t.span,
            attrs,
            fingerprint: None,
        });
    }

    // Foreign keys only between known tables; deduped by (from, to), first span kept. A self-key
    // (child == parent, e.g. `manager_id`) is legitimate and is kept.
    let mut edges: BTreeMap<(String, String), SourceSpan> = BTreeMap::new();
    for j in joins {
        if seen.contains(&j.child) && seen.contains(&j.parent) {
            edges.entry((j.child, j.parent)).or_insert(j.span);
        }
    }
    for ((from, to), span) in edges {
        graph.edges.push(Edge {
            from: StableId::new(from),
            to: StableId::new(to),
            kind: EdgeKind::ForeignKey,
            span,
            ordinal: None,
        });
    }

    graph.normalize();
    Ok(graph)
}

/// One parsed `table!`: the table name, its rendered `columns` attribute, and the macro's span.
struct RawTable {
    name: String,
    columns: String,
    span: SourceSpan,
}

/// One parsed `joinable!(child -> parent (fk))`. The fk column is not modelled in v1.
struct RawJoin {
    child: String,
    parent: String,
    span: SourceSpan,
}

/// Recursively find `schema.rs` files under `root`, sorted, skipping `target` and `.git`.
fn discover_schema_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    walk(root, &mut |path| {
        if path.file_name().and_then(|n| n.to_str()) == Some("schema.rs") {
            out.push(path.to_path_buf());
        }
    });
    out.sort();
    out
}

/// Depth-first `.rs` file walk that skips `target` and `.git` directories.
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

/// Collect every `macro_invocation` reachable under `node` (Diesel puts them at file or module top
/// level; the walk is recursive so nesting does not matter).
fn collect_macros<'a>(node: TsNode<'a>, out: &mut Vec<TsNode<'a>>) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() == "macro_invocation" {
            out.push(child);
        }
        collect_macros(child, out);
    }
}

/// The trailing segment of a macro invocation's path: `table` for `table!` and `diesel::table!`.
fn macro_name(node: TsNode, src: &[u8]) -> Option<String> {
    let m = node.child_by_field_name("macro")?;
    match m.kind() {
        "identifier" => Some(text(m, src)),
        "scoped_identifier" => m.child_by_field_name("name").map(|n| text(n, src)),
        _ => None,
    }
}

/// Parse a `table! { name (pk) { col -> Type, .. } }` invocation. The macro token tree is read by
/// text: the first identifier is the table name, and the `{ .. }` group holds the columns.
fn parse_table(node: TsNode, src: &[u8], file: &str) -> Option<RawTable> {
    let tt = child_kind(node, "token_tree")?;
    let tt_text = text(tt, src);
    let inner = strip_delims(&tt_text);
    let name = leading_ident(inner)?;
    let columns = braced_block(inner).map(parse_columns).unwrap_or_default();
    Some(RawTable {
        name,
        columns,
        span: span_of(node, file),
    })
}

/// Parse a `joinable!(child -> parent (fk))` invocation from the macro token-tree text.
fn parse_joinable(node: TsNode, src: &[u8], file: &str) -> Option<RawJoin> {
    let tt = child_kind(node, "token_tree")?;
    let tt_text = text(tt, src);
    let inner = strip_delims(&tt_text);
    let (child_part, parent_part) = inner.split_once("->")?;
    let child = leading_ident(child_part)?;
    let parent = leading_ident(parent_part)?;
    Some(RawJoin {
        child,
        parent,
        span: span_of(node, file),
    })
}

/// Render a `{ col -> Type, .. }` body's inner text into a `name: Type` list sorted by column name.
/// Each entry is `col -> Type`; the column name is the last token before `->` (so leading attributes
/// like `#[sql_name = ".."]` are tolerated) and the type is the whitespace-collapsed remainder.
fn parse_columns(body: &str) -> String {
    let mut cols: BTreeMap<String, String> = BTreeMap::new();
    for entry in split_top_level(body) {
        let Some((left, right)) = entry.split_once("->") else {
            continue;
        };
        let Some(name) = left.split_whitespace().next_back() else {
            continue;
        };
        let ty = right.split_whitespace().collect::<Vec<_>>().join(" ");
        if name.is_empty() || ty.is_empty() {
            continue;
        }
        cols.insert(name.to_string(), ty);
    }
    cols.into_iter()
        .map(|(name, ty)| format!("{name}: {ty}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Trim whitespace, then drop one layer of surrounding `{}`, `()`, or `[]` delimiters if present.
fn strip_delims(s: &str) -> &str {
    let s = s.trim();
    let bytes = s.as_bytes();
    if bytes.len() >= 2 {
        let open = bytes[0];
        let close = bytes[bytes.len() - 1];
        if matches!((open, close), (b'{', b'}') | (b'(', b')') | (b'[', b']')) {
            return s[1..s.len() - 1].trim();
        }
    }
    s
}

/// The leading identifier (`[A-Za-z0-9_]+`) of `s` after trimming, or `None` if there is none.
fn leading_ident(s: &str) -> Option<String> {
    let ident: String = s
        .trim_start()
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    (!ident.is_empty()).then_some(ident)
}

/// The text inside the first balanced `{ .. }` group of `s` (exclusive of the braces), or `None`.
fn braced_block(s: &str) -> Option<&str> {
    let start = s.find('{')?;
    let mut depth = 0u32;
    for (i, c) in s[start..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&s[start + 1..start + i]);
                }
            }
            _ => {}
        }
    }
    None
}

/// Split `s` on top-level commas, ignoring commas nested in `<>`, `()`, `[]`, or `{}`. The `->`
/// arrow token is skipped so its `>` is not read as a closing angle bracket.
fn split_top_level(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth: i32 = 0;
    let mut start = 0usize;
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'-' && i + 1 < bytes.len() && bytes[i + 1] == b'>' {
            i += 2;
            continue;
        }
        match c {
            b'<' | b'(' | b'[' | b'{' => depth += 1,
            b'>' | b')' | b']' | b'}' => depth -= 1,
            b',' if depth == 0 => {
                out.push(s[start..i].to_string());
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    out.push(s[start..].to_string());
    out
}

/// First direct child of the given kind.
fn child_kind<'a>(node: TsNode<'a>, kind: &str) -> Option<TsNode<'a>> {
    let mut cursor = node.walk();
    let found = node.children(&mut cursor).find(|c| c.kind() == kind);
    found
}

/// UTF-8 text of a node.
fn text(node: TsNode, src: &[u8]) -> String {
    node.utf8_text(src).unwrap_or_default().to_string()
}

/// Path relative to `root`, forward-slashed, for a [`SourceSpan`].
fn rel(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
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
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    const SCHEMA: &str = r#"
// Bare `table!` and `diesel::table!` are both handled.
table! {
    users (id) {
        id -> Int4,
        name -> Varchar,
    }
}

diesel::table! {
    posts (id) {
        id -> Int4,
        user_id -> Int4,
        title -> Text,
    }
}

joinable!(posts -> users (user_id));
// A join to a table that has no `table!` node must not emit an edge.
diesel::joinable!(posts -> ghosts (ghost_id));

diesel::allow_tables_to_appear_in_same_query!(users, posts);
"#;

    fn extract_fixture() -> Graph {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src/schema.rs"), SCHEMA).unwrap();
        extract_schema(dir.path()).unwrap()
    }

    #[test]
    fn tables_and_columns() {
        let g = extract_fixture();
        let tables: Vec<(&str, &str)> = g
            .nodes
            .iter()
            .map(|n| {
                assert_eq!(n.kind, NodeKind::Table);
                (n.id.as_str(), n.attrs["columns"].as_str())
            })
            .collect();
        // Sorted by id; columns sorted by column name.
        assert_eq!(
            tables,
            vec![
                ("posts", "id: Int4, title: Text, user_id: Int4"),
                ("users", "id: Int4, name: Varchar"),
            ]
        );
    }

    #[test]
    fn foreign_keys_only_between_known_tables() {
        let g = extract_fixture();
        let fks: Vec<(&str, &str)> = g
            .edges
            .iter()
            .map(|e| {
                assert_eq!(e.kind, EdgeKind::ForeignKey);
                (e.from.as_str(), e.to.as_str())
            })
            .collect();
        // `posts -> ghosts` is dropped: `ghosts` has no table node.
        assert_eq!(fks, vec![("posts", "users")]);
    }

    #[test]
    fn deterministic() {
        assert_eq!(extract_fixture(), extract_fixture());
    }
}
