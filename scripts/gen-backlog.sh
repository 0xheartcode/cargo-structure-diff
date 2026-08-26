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
} > "$out"

echo "wrote $out"
