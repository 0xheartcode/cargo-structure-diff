# Backlog

Generated from the local [git-native-issue](https://github.com/remenoscodes/git-native-issue)
backlog under `refs/issues/`. Do not edit by hand; run `just backlog` (or
`scripts/gen-backlog.sh`) to regenerate. Source of truth is `git issue ls`.

Dependencies are not a native git-issue field, so each issue body carries a
`Blocked by:` line and every issue is labelled `blocking` or `independent`.

## Open issues

```
14c3416 [closed] ir: implement StableId, structural fingerprints and similarity scoring
        labels:area:ir, blocking, type:feat priority:critical milestone:m0
422be96 [closed] extract: resolve cross-crate use paths to workspace dependency edges
        labels:area:extract, blocking, type:feat priority:critical milestone:m6
775ae67 [closed] diff: rename/move matcher with fingerprint similarity and threshold
        labels:area:diff, blocking, type:feat priority:critical milestone:m0
e462549 [closed] extract/render: states view only for real state machines
        labels:area:render, blocking, type:feat priority:critical milestone:m6
0121366 [closed] config: parse .csd.toml (layers, glob map, views, lint, ratchet)
        labels:area:config, independent, type:feat priority:high milestone:m1
0591220 [closed] render-types: classDiagram with member-level delta
        labels:area:render, blocking, type:feat priority:high milestone:m3
07ab983 [closed] extract-impls: Implements and Associates edges
        labels:area:extract, blocking, type:feat priority:high milestone:m3
08ff882 [closed] render/cli: --full whole-graph mode
        labels:area:render, blocking, type:feat priority:high milestone:m6
09e7534 [closed] cli: csd and cargo-structure-diff binaries wiring the pipeline
        labels:area:cli, blocking, type:feat priority:high milestone:m1
0a83140 [closed] extract-rs: module tree and use-path collection via tree-sitter
        labels:area:extract, blocking, type:feat priority:high milestone:m0
1ca8a08 [closed] extract-calls: Calls edges with source-order ordinals
        labels:area:extract, blocking, type:feat priority:high milestone:m4
22cc17f [closed] harness: select refactor-heavy pairs for the sweep
        labels:area:harness, independent, type:feat priority:high milestone:m0
2a3e316 [closed] render-states: stateDiagram-v2 with delta colouring
        labels:area:render, independent, type:feat priority:high milestone:m2
311ff60 [closed] lint-states: reachability and cycle rules
        labels:area:lint, independent, type:feat priority:high milestone:m2
4894e6a [closed] lint: layering and cycles over the module graph with ratchet
        labels:area:lint, blocking, type:feat priority:high milestone:m1
53b3162 [closed] render: combined overview view (the code schema) - modules containing types + fns with all edges
        labels:area:render, independent, type:feat priority:high milestone:m6
563525b [closed] cli/render: --scope focus view (files/folders) with boundary stubs
        labels:area:cli, area:render, independent, type:feat priority:high milestone:m6
5b13237 [closed] diff-calls: tree edit distance over call sequences
        labels:area:diff, independent, type:feat priority:high milestone:m4
5d04481 [closed] extract-states: enum transitions to Transitions edges
        labels:area:extract, blocking, type:feat priority:high milestone:m2
613c5ab [closed] cli: wire the calls view into csd diff
        labels:area:cli, blocking, type:feat priority:high milestone:m4
681e0a7 [closed] render: module graph to Mermaid flowchart with delta colouring
        labels:area:render, blocking, type:feat priority:high milestone:m1
7344e8d [closed] extract-rs: namespace node ids per crate for workspaces
        labels:area:extract, independent, type:feat priority:high milestone:m1
743edea [closed] cli: --list textual per-node change summary
        labels:area:cli, independent, type:feat priority:high milestone:m6
7916cfb [closed] cli: csd doc - browsable whole-codebase structure report (SchemaSpy-style)
        labels:area:cli, area:render, independent, type:feat priority:high milestone:m6
87f08db [closed] harness: threshold sweep and metrics
        labels:area:harness, blocking, type:feat priority:high milestone:m0
8b1bc21 [closed] render: global call-graph flowchart (functions + flow), not just the sequence slice
        labels:area:render, independent, type:feat priority:high milestone:m6
9ee7683 [closed] render-calls: sequenceDiagram slice with changed regions
        labels:area:render, blocking, type:feat priority:high milestone:m4
a04d762 [closed] cli: wire the types view into csd diff
        labels:area:cli, blocking, type:feat priority:high milestone:m3
b59eafb [closed] render: native ASCII boxes-and-arrows layout for graph views
        labels:area:render, independent, type:feat priority:high milestone:m6
b951fc4 [closed] harness: git worktree materialization and IR cache by SHA
        labels:area:harness, blocking, type:feat priority:high milestone:m0
bb67bbb [closed] extract-rs: precision-first use-path mini-resolver
        labels:area:extract, blocking, type:feat priority:high milestone:m1
c1b3a2b [closed] diff: flat set diff over nodes and edges
        labels:area:diff, blocking, type:feat priority:high milestone:m0
e84ac0e [closed] cli: wire the states view into csd diff
        labels:area:cli, blocking, type:feat priority:high milestone:m2
ebf7c6d [closed] ir/diff: per-member types for precise member-diff
        labels:area:ir, blocking, type:feat priority:high milestone:m6
ff3a772 [closed] extract-rs: item nodes with member fingerprints
        labels:area:extract, independent, type:feat priority:high milestone:m0
25768fb [closed] harness: generate M0-REPORT.md go/no-go artifact
        labels:area:harness, blocking, type:docs priority:medium milestone:m0
3407789 [closed] diff-members: member-level matching within surviving types
        labels:area:diff, independent, type:feat priority:medium milestone:m3
369ee6b [open] cli: csd walk - render each commit's graph frame by frame
        labels:area:cli, independent, type:feat priority:medium milestone:m6
3aefc9b [closed] extract-db/render: schema (ER) view for Diesel and SeaORM (SPEC 3.5)
        labels:area:extract, independent, type:feat priority:medium milestone:m6
4a971c7 [closed] diff: seed module-level Moved from git file-rename signal
        labels:area:diff, independent, type:feat priority:medium milestone:m0
71d1b0d [closed] perf: persistent SHA-keyed IR cache (with serde on csd-ir)
        labels:area:ir, independent, type:feat priority:medium milestone:m6
ad3822a [closed] harness: corpus clone and first-parent pair walker
        labels:area:harness, independent, type:feat priority:medium milestone:m0
d42fcd9 [closed] render/cli: --format ascii - in-terminal text rendering of the DAG views
        labels:area:render, area:cli, independent, type:feat priority:medium milestone:m6
db76929 [closed] render: list methods as class members in the type view
        labels:area:render, independent, type:feat priority:medium milestone:m6
ec31139 [closed] test: golden fixtures for IR and Change classification
        labels:area:test, independent, type:test priority:medium milestone:m0
eeb3211 [closed] lint: io_in_hot_path over the call graph
        labels:area:lint, independent, type:feat priority:medium milestone:m4
f7c40ef [closed] infra: CI workflow (fmt, clippy -D warnings, test on stable)
        labels:area:infra, independent, type:infra priority:medium milestone:m0
fdaac35 [closed] render: --format dot output (offline, JS-free) for the graph views
        labels:area:render, independent, type:feat priority:medium milestone:m6
0444f3e [closed] harness/cli: crustrace traced mode as a separate subcommand
        labels:area:cli, independent, type:feat priority:low milestone:m4
0e76290 [closed] spike: rustdoc-json vs tree-sitter for the types view
        labels:area:extract, independent, type:docs priority:low milestone:m3
29f3ab0 [closed] extract-states: broaden idiom test coverage
        labels:area:extract, independent, type:test priority:low milestone:m2
2a73d33 [closed] perf: parallel extraction with rayon
        labels:area:extract, independent, type:feat priority:low milestone:m6
60f22da [closed] render: --format svg via the pure-Rust layout crate (native image, no Node)
        labels:area:render, independent, type:feat priority:low milestone:m6
db21bac [closed] infra: wire the quality gate into justfile and CI
        labels:area:infra, independent, type:infra priority:low milestone:m0
```

## Dependency graph

Slugs match the `Slug:` line in each issue body. An arrow means "blocks".

```mermaid
flowchart TD
    ir[ir: id + fingerprints + similarity]
    ir --> extract_modtree[extract: module tree + use paths]
    ir --> extract_items[extract: item fingerprints]
    ir --> diff_setdiff[diff: set diff]
    ir --> test_golden[test: golden fixtures]
    extract_modtree --> extract_resolver[extract: use-path resolver (M1)]
    extract_modtree --> harness_worktree[harness: worktree + IR cache]
    ir --> harness_worktree
    extract_items --> diff_rename[diff: rename/move matcher]
    diff_setdiff --> diff_rename
    diff_setdiff --> diff_gitmove[diff: git file-rename seed]
    diff_rename --> harness_metrics[harness: threshold sweep + metrics]
    harness_worktree --> harness_metrics
    harness_metrics --> harness_report[harness: M0-REPORT.md]
    harness_metrics --> spike_rustdoc[spike: rustdoc-json vs tree-sitter (M3)]
    harness_corpus[harness: corpus + pair walker]:::indep
    infra_ci[infra: CI gate]:::indep
    infra_gate[infra: quality gate]:::indep
    classDef indep stroke-dasharray:4 4;
```

Dashed nodes have no blockers and can start immediately.
