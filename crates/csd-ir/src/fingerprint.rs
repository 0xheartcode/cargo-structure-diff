//! Structural fingerprints and similarity scoring.
//!
//! A [`Fingerprint`] is a name-independent summary of a node's structure. Two nodes that differ
//! only in name (a pure rename) produce identical fingerprints, which is what lets the differ
//! emit `Moved`/`Modified` instead of `Added` + `Removed`. [`similarity`] scores two fingerprints
//! in `[0.0, 1.0]`; the differ accepts a candidate pair as a move when the score clears a
//! configured threshold.
//!
//! Everything here is pure and deterministic: sets and multisets are `BTreeSet`/`BTreeMap`, and
//! [`doc_hash`] uses a fixed-seed hasher so the same doc text always hashes the same across runs.

use std::collections::{BTreeMap, BTreeSet};
use std::hash::{Hash, Hasher};

/// A name-independent structural summary of a node.
///
/// Deliberately excludes the node's own name so a rename does not change it. It is built by the
/// extractor (backlog `extract`) and consumed by the differ (backlog `diff`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Fingerprint {
    /// Member and method names declared on the node (fields, variants, trait items).
    pub members: BTreeSet<String>,
    /// Multiset of the types referenced by the node's fields or parameters: type name to count.
    pub field_types: BTreeMap<String, u32>,
    /// Ids of adjacent nodes (the edge neighbourhood), giving structural context beyond members.
    pub neighbors: BTreeSet<String>,
    /// Fixed-seed hash of the node's doc comment, or `0` when it has none.
    pub doc_hash: u64,
}

/// Relative weights of the four fingerprint components. They are normalised at scoring time, so
/// they need not sum to one.
#[derive(Debug, Clone, Copy)]
pub struct SimilarityWeights {
    /// Weight of the member-name Jaccard overlap.
    pub members: f32,
    /// Weight of the field-type multiset overlap.
    pub field_types: f32,
    /// Weight of the neighbourhood Jaccard overlap.
    pub neighbors: f32,
    /// Weight of the doc-hash match.
    pub doc: f32,
}

impl Default for SimilarityWeights {
    fn default() -> Self {
        // Members and field types carry the signal; neighbourhood and docs are tie-breakers.
        Self {
            members: 0.4,
            field_types: 0.4,
            neighbors: 0.1,
            doc: 0.1,
        }
    }
}

/// Hash doc-comment text with a fixed-seed hasher so results are stable across processes.
///
/// Returns `0` for empty text so "no docs on either side" reads as a match, not a hash collision.
pub fn doc_hash(doc: &str) -> u64 {
    if doc.is_empty() {
        return 0;
    }
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    doc.hash(&mut hasher);
    hasher.finish()
}

/// Score how structurally similar two fingerprints are, in `[0.0, 1.0]`.
///
/// Uses [`SimilarityWeights::default`]. See [`similarity_weighted`] to supply custom weights.
pub fn similarity(a: &Fingerprint, b: &Fingerprint) -> f32 {
    similarity_weighted(a, b, SimilarityWeights::default())
}

/// Score two fingerprints with explicit component weights, in `[0.0, 1.0]`.
pub fn similarity_weighted(a: &Fingerprint, b: &Fingerprint, w: SimilarityWeights) -> f32 {
    let total = w.members + w.field_types + w.neighbors + w.doc;
    if total <= 0.0 {
        return 0.0;
    }

    let member_score = jaccard(&a.members, &b.members);
    let field_score = multiset_jaccard(&a.field_types, &b.field_types);
    let neighbor_score = jaccard(&a.neighbors, &b.neighbors);
    let doc_score = if a.doc_hash == b.doc_hash { 1.0 } else { 0.0 };

    let weighted = w.members * member_score
        + w.field_types * field_score
        + w.neighbors * neighbor_score
        + w.doc * doc_score;

    weighted / total
}

/// Jaccard overlap of two sets. Two empty sets score `1.0` (no evidence they differ).
fn jaccard(a: &BTreeSet<String>, b: &BTreeSet<String>) -> f32 {
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    let intersection = a.intersection(b).count();
    let union = a.len() + b.len() - intersection;
    if union == 0 {
        return 1.0;
    }
    intersection as f32 / union as f32
}

/// Jaccard overlap of two multisets (sum of per-key minima over sum of per-key maxima). Two empty
/// multisets score `1.0`.
fn multiset_jaccard(a: &BTreeMap<String, u32>, b: &BTreeMap<String, u32>) -> f32 {
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    let mut min_sum: u32 = 0;
    let mut max_sum: u32 = 0;
    for key in a.keys().chain(b.keys()).collect::<BTreeSet<_>>() {
        let ca = a.get(key).copied().unwrap_or(0);
        let cb = b.get(key).copied().unwrap_or(0);
        min_sum += ca.min(cb);
        max_sum += ca.max(cb);
    }
    if max_sum == 0 {
        return 1.0;
    }
    min_sum as f32 / max_sum as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fp(members: &[&str], fields: &[(&str, u32)], doc: &str) -> Fingerprint {
        Fingerprint {
            members: members.iter().map(|s| s.to_string()).collect(),
            field_types: fields.iter().map(|(t, c)| (t.to_string(), *c)).collect(),
            neighbors: BTreeSet::new(),
            doc_hash: doc_hash(doc),
        }
    }

    #[test]
    fn identical_fingerprints_score_one() {
        let a = fp(&["id", "total"], &[("u64", 1), ("Money", 1)], "an order");
        let b = a.clone();
        assert_eq!(similarity(&a, &b), 1.0);
    }

    #[test]
    fn rename_only_still_scores_one() {
        // Same structure, and the fingerprint never encodes the node's own name, so a pure rename
        // is indistinguishable from identity here. That is the point.
        let a = fp(&["id", "total"], &[("u64", 1), ("Money", 1)], "docs");
        let b = fp(&["id", "total"], &[("u64", 1), ("Money", 1)], "docs");
        assert_eq!(similarity(&a, &b), 1.0);
    }

    #[test]
    fn unrelated_fingerprints_score_low() {
        let a = fp(
            &["id", "total", "customer"],
            &[("u64", 1), ("Money", 1)],
            "an order",
        );
        let b = fp(&["kind", "payload"], &[("String", 1)], "a message");
        assert!(
            similarity(&a, &b) < 0.2,
            "expected low score, got {}",
            similarity(&a, &b)
        );
    }

    #[test]
    fn partial_overlap_scores_between() {
        let a = fp(&["id", "total"], &[("u64", 1)], "");
        let b = fp(&["id", "amount"], &[("u64", 1)], "");
        let s = similarity(&a, &b);
        assert!(s > 0.3 && s < 1.0, "expected mid score, got {s}");
    }

    #[test]
    fn doc_hash_is_stable_and_empty_is_zero() {
        assert_eq!(doc_hash(""), 0);
        assert_eq!(doc_hash("hello"), doc_hash("hello"));
        assert_ne!(doc_hash("hello"), doc_hash("world"));
    }

    #[test]
    fn empty_fingerprints_match() {
        let a = Fingerprint::default();
        let b = Fingerprint::default();
        assert_eq!(similarity(&a, &b), 1.0);
    }
}
