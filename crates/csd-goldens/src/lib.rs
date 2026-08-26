//! Dev-only golden fixtures for the extract -> diff pipeline. Tests live under `#[cfg(test)]`.

#[cfg(test)]
mod tests {
    use std::fs;

    use csd_diff::{diff, DiffOptions};
    use csd_extract_rs::extract;
    use csd_ir::Change;

    use tempfile::TempDir;

    /// Write `src/lib.rs` under a fresh temp dir and return the dir (kept alive by the caller).
    fn crate_dir(lib_rs: &str) -> TempDir {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("src/lib.rs");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, lib_rs).unwrap();
        dir
    }

    /// Extract a crate laid out from a single `src/lib.rs` body.
    fn graph_of(lib_rs: &str) -> csd_ir::Graph {
        let dir = crate_dir(lib_rs);
        extract(dir.path()).unwrap()
    }

    /// Span-independent projection of a `Change`. Byte spans shift with any edit above a node, so
    /// the golden pins the classification and the ids, not the offsets (which the differ already
    /// ignores for identity, see `csd-diff`).
    #[derive(Debug, PartialEq, Eq)]
    enum Delta {
        Added(String),
        Removed(String),
        /// `fp_changed` records whether the fingerprint actually differs, so a rename (unchanged
        /// structure) is distinguishable from a real content edit.
        Modified {
            before: String,
            after: String,
            fp_changed: bool,
        },
        Moved {
            node: String,
            from: String,
            to: String,
        },
        EdgeAdded(String, String),
        EdgeRemoved(String, String),
    }

    fn project(c: &Change) -> Delta {
        match c {
            Change::Added(n) => Delta::Added(n.id.as_str().to_string()),
            Change::Removed(n) => Delta::Removed(n.id.as_str().to_string()),
            Change::Modified { before, after } => Delta::Modified {
                before: before.id.as_str().to_string(),
                after: after.id.as_str().to_string(),
                fp_changed: before.fingerprint != after.fingerprint,
            },
            Change::Moved { node, from, to } => Delta::Moved {
                node: node.as_str().to_string(),
                from: from.as_str().to_string(),
                to: to.as_str().to_string(),
            },
            Change::EdgeAdded(e) => {
                Delta::EdgeAdded(e.from.as_str().to_string(), e.to.as_str().to_string())
            }
            Change::EdgeRemoved(e) => {
                Delta::EdgeRemoved(e.from.as_str().to_string(), e.to.as_str().to_string())
            }
        }
    }

    /// Full pipeline: extract both sides, diff with default options, project the result.
    fn delta(before: &str, after: &str) -> Vec<Delta> {
        let base = graph_of(before);
        let head = graph_of(after);
        diff(&base, &head, DiffOptions::default())
            .iter()
            .map(project)
            .collect()
    }

    // Scenario 1: type rename inside the same module. Identical fields keep the fingerprint, so the
    // matcher reconciles Added + Removed into ONE Modified (parents are equal), never Added+Removed.
    #[test]
    fn type_rename_same_module_is_one_modified() {
        let before = "pub struct Order {\n    pub id: u64,\n    pub total: u64,\n}\n";
        let after = "pub struct Invoice {\n    pub id: u64,\n    pub total: u64,\n}\n";
        assert_eq!(
            delta(before, after),
            vec![Delta::Modified {
                before: "crate::Order".to_string(),
                after: "crate::Invoice".to_string(),
                // note: fingerprint is unchanged; this is a pure rename, matched by structure.
                fp_changed: false,
            }]
        );
    }

    // Scenario 2: identical struct moved between inline modules (`billing` -> `payments`) within the
    // one lib.rs. The move is NOT file-driven (both modules share src/lib.rs), so there is no
    // FileRename to pass; the struct is reconciled by fingerprint similarity into a `Moved`. The
    // module nodes themselves carry no fingerprint, so they cannot be reconciled and honestly
    // surface as Added(new module) + Removed(old module).
    #[test]
    fn identical_struct_moved_between_modules_is_moved() {
        let before = "pub mod billing {\n    pub struct Order {\n        pub id: u64,\n        pub total: u64,\n    }\n}\n";
        let after = "pub mod payments {\n    pub struct Order {\n        pub id: u64,\n        pub total: u64,\n    }\n}\n";
        assert_eq!(
            delta(before, after),
            vec![
                Delta::Added("crate::payments".to_string()),
                Delta::Removed("crate::billing".to_string()),
                Delta::Moved {
                    node: "crate::payments::Order".to_string(),
                    from: "crate::billing".to_string(),
                    to: "crate::payments".to_string(),
                },
            ]
        );
    }

    // Scenario 3: a field type changes (`id: u32` -> `id: u64`) while the struct keeps its name. Id
    // matches directly, the fingerprint differs, so exactly ONE Modified with a changed fingerprint.
    #[test]
    fn member_type_change_is_one_modified_with_changed_fingerprint() {
        let before = "pub struct Config {\n    pub id: u32,\n    pub name: String,\n}\n";
        let after = "pub struct Config {\n    pub id: u64,\n    pub name: String,\n}\n";
        assert_eq!(
            delta(before, after),
            vec![Delta::Modified {
                before: "crate::Config".to_string(),
                after: "crate::Config".to_string(),
                fp_changed: true,
            }]
        );
    }

    // Scenario 4: formatting-only change (blank lines, spacing, brace placement) with identical
    // items. The anti-noise guarantee: an EMPTY delta.
    #[test]
    fn formatting_only_change_is_empty_delta() {
        let before =
            "pub struct Config {\n    pub id: u32,\n    pub name: String,\n}\n\npub fn run(x: u32) -> u32 {\n    x\n}\n";
        let after =
            "pub struct Config { pub id: u32, pub name: String }\n\n\n\npub fn run(x: u32)   ->   u32 {\n\n    x\n\n}\n";
        assert_eq!(delta(before, after), Vec::<Delta>::new());
    }
}
