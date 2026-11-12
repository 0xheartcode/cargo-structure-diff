#!/usr/bin/env bash
# Structure gate for pull requests.
#
# Diffs the checked-out working tree against a base ref with csd, posts the grouped changelog plus
# any architectural violations as a single sticky PR comment, and exits non-zero when a lint denies
# the change (so the check turns the build red). Portable: needs only csd, git, and, to comment, the
# gh CLI.
#
# The comment carries, in order: the verdict, the denials and warnings as a list, the grouped
# changelog in a collapsible block, and the Mermaid delta in a second collapsible block that GitHub
# renders inline. If the assembled body would exceed GitHub's comment size limit it is replaced by a
# compact summary. On a fork pull request the token is read-only, so a failed comment falls back to
# the job's step summary instead of failing the gate.
#
# Usage: csd-structure-gate.sh [<base-ref>]
#   base-ref   Ref to diff against. Falls back to $CSD_BASE, then origin/main.
#
# Environment:
#   CSD_BIN            csd binary to run (default: csd)
#   GITHUB_TOKEN       token for gh; when set with PR_NUMBER, the comment is posted
#   PR_NUMBER          pull-request number to comment on
#   GITHUB_REPOSITORY  owner/repo, used to find and update the sticky comment
#   GITHUB_STEP_SUMMARY  file the body is written to when a PR comment cannot be posted
#   CSD_COMMENT        set to "false" to skip commenting and only print the body
set -euo pipefail

base="${1:-${CSD_BASE:-origin/main}}"
csd="${CSD_BIN:-csd}"
marker="<!-- csd-structure-gate -->"
# GitHub rejects issue comments over 65536 characters; leave headroom for the wrapper and a notice.
max_comment=60000

changelog_file="$(mktemp)"
diff_file="$(mktemp)"
body_file="$(mktemp)"
comment_file="$(mktemp)"
trap 'rm -f "$changelog_file" "$diff_file" "$body_file" "$comment_file"' EXIT

# The grouped changelog is the human-facing summary of the delta.
if ! "$csd" changelog --base "$base" >"$changelog_file" 2>/dev/null; then
    echo "csd-structure-gate: 'csd changelog --base $base' failed; is the base ref '$base' fetched?" >&2
    exit 2
fi

# One diff run produces the verdict (exit code), the findings (on a denial), and the Mermaid delta
# (via --show). Capture stdout and stderr together, since findings and the diagram go to stderr on a
# denial but stdout on a clean --show run. Do not let a non-zero exit trip set -e here.
code=0
"$csd" diff --base "$base" --show --format mermaid >"$diff_file" 2>&1 || code=$?

verdict="passed"
[ "$code" -ne 0 ] && verdict="FAILED"

# Extract the findings lines: `  [deny] rule: message` / `  [warn] rule: message`.
findings="$(grep -E '^\s*\[(deny|warn)\]' "$diff_file" || true)"

# Extract the Mermaid diagram: everything from the first `<name> view:` line to the end, with each
# view header turned into a fenced ```mermaid block GitHub renders.
diagram="$(awk '
    /^[A-Za-z0-9_-]+ view:$/ {
        if (inblock) { print "```" }
        sub(/ view:$/, "")
        print "**" $0 " view**"
        print "```mermaid"
        inblock = 1
        next
    }
    inblock { print }
    END { if (inblock) print "```" }
' "$diff_file")"

# Assemble the full comment body.
{
    echo "$marker"
    echo "## Structure gate: $verdict"
    echo
    if [ -n "$findings" ]; then
        echo "### Findings"
        echo
        # Render each finding as a list item, keeping the [deny]/[warn] tag.
        echo "$findings" | sed -E 's/^[[:space:]]*/- /'
        echo
    fi
    echo "<details><summary>Changelog</summary>"
    echo
    cat "$changelog_file"
    echo
    echo "</details>"
    echo
    if [ -n "$diagram" ]; then
        echo "<details><summary>Structure diagram</summary>"
        echo
        echo "$diagram"
        echo
        echo "</details>"
    fi
} >"$body_file"

# If the body is too large for a GitHub comment, replace it with a compact summary. The gate verdict
# is unchanged; only the comment is trimmed.
if [ "$(wc -c <"$body_file")" -gt "$max_comment" ]; then
    summary_line="$(grep -E '^cargo-structure-diff:' "$diff_file" | head -n1 || true)"
    {
        echo "$marker"
        echo "## Structure gate: $verdict"
        echo
        [ -n "$summary_line" ] && { echo "$summary_line"; echo; }
        if [ -n "$findings" ]; then
            echo "### Findings"
            echo
            echo "$findings" | sed -E 's/^[[:space:]]*/- /'
            echo
        fi
        echo "_The full changelog and diagram exceeded GitHub's comment size limit. Run \`csd diff --base $base --show\` locally to see the delta._"
    } >"$comment_file"
else
    cp "$body_file" "$comment_file"
fi

# Post the sticky comment inside a PR with a token, unless disabled. On any failure (a fork PR's
# read-only token returns 403), fall back to the job step summary so the result is never lost and the
# gate still reports its real verdict. With no PR context, print the body for local runs.
posted=false
if [ "${CSD_COMMENT:-true}" = "true" ] && [ -n "${GITHUB_TOKEN:-}" ] && [ -n "${PR_NUMBER:-}" ]; then
    repo="${GITHUB_REPOSITORY:?GITHUB_REPOSITORY is required to comment}"
    existing="$(gh api "repos/$repo/issues/$PR_NUMBER/comments" \
        --jq ".[] | select(.body | startswith(\"$marker\")) | .id" 2>/dev/null | head -n1 || true)"
    if [ -n "$existing" ]; then
        if gh api -X PATCH "repos/$repo/issues/comments/$existing" \
            -f body="$(cat "$comment_file")" >/dev/null 2>&1; then
            posted=true
        fi
    elif gh pr comment "$PR_NUMBER" --body-file "$comment_file" >/dev/null 2>&1; then
        posted=true
    fi

    if [ "$posted" = false ]; then
        echo "csd-structure-gate: could not post the PR comment (a fork PR's token is read-only); writing to the job summary instead." >&2
        if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
            cat "$comment_file" >>"$GITHUB_STEP_SUMMARY"
        else
            cat "$comment_file"
        fi
    fi
elif [ -n "${GITHUB_STEP_SUMMARY:-}" ] && [ "${CSD_COMMENT:-true}" != "false" ]; then
    cat "$comment_file" >>"$GITHUB_STEP_SUMMARY"
else
    cat "$comment_file"
fi

exit "$code"
