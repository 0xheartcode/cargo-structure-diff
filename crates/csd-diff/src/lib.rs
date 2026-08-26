//! Diffing between two [`csd_ir::Graph`]s.
//!
//! Two layers, tracked in the backlog (area `diff`):
//! 1. a flat set diff over nodes and edges, and
//! 2. a rename/move matcher that rewrites `Added` + `Removed` pairs into `Moved`/`Modified` when
//!    structural fingerprints are similar enough.
//!
//! Only the type surface exists at v0.0.0; the bodies are backlog work.

use csd_ir::{Change, Graph};

/// Options controlling rename/move detection.
#[derive(Debug, Clone, Copy)]
pub struct DiffOptions {
    /// Similarity in `[0.0, 1.0]` at or above which a candidate pair is accepted as a move.
    /// `None` disables rename detection (pure set diff).
    pub rename_threshold: Option<f32>,
}

impl Default for DiffOptions {
    fn default() -> Self {
        Self {
            rename_threshold: Some(0.7),
        }
    }
}

/// Compute the delta from `base` to `head`.
///
/// Backlog `diff`: not yet implemented. Returns an empty delta so the workspace builds.
pub fn diff(_base: &Graph, _head: &Graph, _opts: DiffOptions) -> Vec<Change> {
    Vec::new()
}
