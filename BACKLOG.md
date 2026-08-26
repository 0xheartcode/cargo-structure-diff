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
775ae67 [closed] diff: rename/move matcher with fingerprint similarity and threshold
        labels:area:diff, blocking, type:feat priority:critical milestone:m0
0a83140 [closed] extract-rs: module tree and use-path collection via tree-sitter
        labels:area:extract, blocking, type:feat priority:high milestone:m0
22cc17f [open] harness: select refactor-heavy pairs for the sweep
        labels:area:harness, independent, type:feat priority:high milestone:m0
7344e8d [open] extract-rs: namespace node ids per crate for workspaces
        labels:area:extract, independent, type:feat priority:high milestone:m1
87f08db [closed] harness: threshold sweep and metrics
        labels:area:harness, blocking, type:feat priority:high milestone:m0
b951fc4 [closed] harness: git worktree materialization and IR cache by SHA
        labels:area:harness, blocking, type:feat priority:high milestone:m0
bb67bbb [open] extract-rs: precision-first use-path mini-resolver
        labels:area:extract, blocking, type:feat priority:high milestone:m1
c1b3a2b [closed] diff: flat set diff over nodes and edges
        labels:area:diff, blocking, type:feat priority:high milestone:m0
ff3a772 [closed] extract-rs: item nodes with member fingerprints
        labels:area:extract, independent, type:feat priority:high milestone:m0
25768fb [closed] harness: generate M0-REPORT.md go/no-go artifact
        labels:area:harness, blocking, type:docs priority:medium milestone:m0
4a971c7 [closed] diff: seed module-level Moved from git file-rename signal
        labels:area:diff, independent, type:feat priority:medium milestone:m0
ad3822a [closed] harness: corpus clone and first-parent pair walker
        labels:area:harness, independent, type:feat priority:medium milestone:m0
ec31139 [closed] test: golden fixtures for IR and Change classification
        labels:area:test, independent, type:test priority:medium milestone:m0
f7c40ef [closed] infra: CI workflow (fmt, clippy -D warnings, test on stable)
        labels:area:infra, independent, type:infra priority:medium milestone:m0
0e76290 [open] spike: rustdoc-json vs tree-sitter for the types view
        labels:area:extract, independent, type:docs priority:low milestone:m3
db21bac [closed] infra: wire bob-pass quality gate into justfile and CI
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
    infra_gate[infra: bob-pass gate]:::indep
    classDef indep stroke-dasharray:4 4;
```

Dashed nodes have no blockers and can start immediately.
