#!/usr/bin/env bash
# Structure gate for pull requests.
#
# Diffs the checked-out working tree against a base ref with csd, posts the grouped changelog as a
# single sticky PR comment, and exits non-zero when an architectural lint denies the change (so the
# check turns the build red). Portable: needs only csd, git, and, to comment, the gh CLI.
#
# Usage: csd-structure-gate.sh [<base-ref>]
#   base-ref   Ref to diff against. Falls back to $CSD_BASE, then origin/main.
#
# Environment:
#   CSD_BIN            csd binary to run (default: csd)
#   GITHUB_TOKEN       token for gh; when set with PR_NUMBER, the comment is posted
#   PR_NUMBER          pull-request number to comment on
#   GITHUB_REPOSITORY  owner/repo, used to find and update the sticky comment
#   CSD_COMMENT        set to "false" to skip commenting and only print the body
set -euo pipefail

base="${1:-${CSD_BASE:-origin/main}}"
csd="${CSD_BIN:-csd}"
marker="<!-- csd-structure-gate -->"

body_file="$(mktemp)"
comment_file="$(mktemp)"
trap 'rm -f "$body_file" "$comment_file"' EXIT

# The comment body is always the grouped changelog.
"$csd" changelog --base "$base" >"$body_file"

# The gate is the diff exit code: 1 denies, 0 is clean. Capture it without tripping set -e.
code=0
"$csd" diff --base "$base" || code=$?

verdict="passed"
if [ "$code" -ne 0 ]; then
    verdict="FAILED"
fi

{
    echo "$marker"
    echo "## Structure gate: $verdict"
    echo
    cat "$body_file"
} >"$comment_file"

# Comment only inside a PR with a token, unless disabled. Otherwise print the body for local runs.
if [ "${CSD_COMMENT:-true}" = "true" ] && [ -n "${GITHUB_TOKEN:-}" ] && [ -n "${PR_NUMBER:-}" ]; then
    repo="${GITHUB_REPOSITORY:?GITHUB_REPOSITORY is required to comment}"
    # Sticky: update our previous comment when one exists, else create a new one.
    existing="$(gh api "repos/$repo/issues/$PR_NUMBER/comments" \
        --jq ".[] | select(.body | startswith(\"$marker\")) | .id" | head -n1 || true)"
    if [ -n "$existing" ]; then
        gh api -X PATCH "repos/$repo/issues/comments/$existing" \
            -f body="$(cat "$comment_file")" >/dev/null
    else
        gh pr comment "$PR_NUMBER" --body-file "$comment_file"
    fi
else
    cat "$comment_file"
fi

exit "$code"
