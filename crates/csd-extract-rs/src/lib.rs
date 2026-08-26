//! Rust extraction: a crate root on disk to a [`csd_ir::Graph`].
//!
//! M0 scope (backlog area `extract`): the module tree from `mod` declarations and file layout,
//! `use`-path collection with a precision-first mini-resolver, and item nodes carrying member
//! fingerprints. Bodies are backlog work; only the entry-point signature exists at v0.0.0.

use std::path::Path;

use anyhow::Result;
use csd_ir::Graph;

/// Extract a structural graph from a Rust crate or workspace rooted at `root`.
///
/// Backlog `extract`: not yet implemented. Returns an empty graph so the workspace builds.
pub fn extract(_root: &Path) -> Result<Graph> {
    Ok(Graph::new())
}
