#!/usr/bin/env bash
# Regenerate BACKLOG.md, a working-tree mirror of the local git-native-issue backlog.
# Issues themselves live under refs/issues/ (run `git issue ls`); this file is a convenience
# index so the backlog is reviewable in a normal diff and on the web.
set -euo pipefail

cd "$(dirname "$0")/.."

out="BACKLOG.md"

{
    echo "# Backlog"
    echo
    echo "Generated from the local [git-native-issue](https://github.com/remenoscodes/git-native-issue)"
    echo "backlog under \`refs/issues/\`. Do not edit by hand; run \`just backlog\` (or"
    echo "\`scripts/gen-backlog.sh\`) to regenerate. Source of truth is \`git issue ls\`."
    echo
    echo "Dependencies are not a native git-issue field, so each issue body carries a"
    echo "\`Blocked by:\` line and every issue is labelled \`blocking\` or \`independent\`."
    echo
    echo "## Open issues"
    echo
    echo '```'
    git issue ls --state all -f full --sort priority || true
    echo '```'
    echo
    echo "## Dependency graph"
    echo
    echo "Slugs match the \`Slug:\` line in each issue body. An arrow means \"blocks\"."
    echo
    echo '```mermaid'
    echo 'flowchart TD'
    echo '    ir[ir: id + fingerprints + similarity]'
    echo '    ir --> extract_modtree[extract: module tree + use paths]'
    echo '    ir --> extract_items[extract: item fingerprints]'
    echo '    ir --> diff_setdiff[diff: set diff]'
    echo '    ir --> test_golden[test: golden fixtures]'
    echo '    extract_modtree --> extract_resolver[extract: use-path resolver (M1)]'
    echo '    extract_modtree --> harness_worktree[harness: worktree + IR cache]'
    echo '    ir --> harness_worktree'
    echo '    extract_items --> diff_rename[diff: rename/move matcher]'
    echo '    diff_setdiff --> diff_rename'
    echo '    diff_setdiff --> diff_gitmove[diff: git file-rename seed]'
    echo '    diff_rename --> harness_metrics[harness: threshold sweep + metrics]'
    echo '    harness_worktree --> harness_metrics'
    echo '    harness_metrics --> harness_report[harness: M0-REPORT.md]'
    echo '    harness_metrics --> spike_rustdoc[spike: rustdoc-json vs tree-sitter (M3)]'
    echo '    harness_corpus[harness: corpus + pair walker]:::indep'
    echo '    infra_ci[infra: CI gate]:::indep'
    echo '    infra_gate[infra: quality gate]:::indep'
    echo '    classDef indep stroke-dasharray:4 4;'
    echo '```'
    echo
    echo "Dashed nodes have no blockers and can start immediately."
} > "$out"

echo "wrote $out"
