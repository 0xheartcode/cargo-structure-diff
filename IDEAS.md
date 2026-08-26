# Ideas parking lot

Local scratch notes, intentionally NOT committed to git. For future direction only.

## Ship csd as a GitHub integration (the CodeRabbit shape) - eventual M5

### How CodeRabbit actually works
It is a GitHub App (not a plain Action), installed on a repo with permission to read PRs and
write comments/checks. Flow:

1. Webhook trigger: GitHub sends CodeRabbit a webhook on PR opened/synchronized. Their hosted
   backend receives it; nothing runs in the user's CI.
2. Fetch diff + context via the GitHub API using an installation token (the App's per-repo
   credential), plus surrounding files for context.
3. LLM pass over the hunks (their own orchestration + prompts): review, summary, and a sequence
   diagram of the change.
4. Write back through the API: a PR summary comment, inline review comments on lines, and a
   check run (the green/red status).
5. Conversational loop: it watches comment-reply webhooks so you can @-mention it.

Key point: the compute is theirs, hosted. The GitHub App is just identity + permissions +
webhook surface. OSS/free tier earns trust; the hosted layer does the integration and is where
the money is (SPEC.md section 10).

### Two ways csd could ship it
- Option A - plain GitHub Action, no hosted backend. A workflow runs `csd diff --base
  $GITHUB_BASE_REF` in the user's own CI runner. On a lint failure it exits non-zero (PR check
  goes red) and a thin step posts the Mermaid delta as a PR comment using the built-in
  `GITHUB_TOKEN`. GitHub renders Mermaid in comments natively, so the diagram shows inline. No
  server, no App, no hosted compute. Cheap, honest first step; boosts adoption but erodes the
  commercial moat (SPEC.md section 8 amendment).
- Option B - hosted GitHub App (the CodeRabbit shape, M5). Same webhook -> installation-token ->
  API-writeback loop, plus hunk-to-node linking via SourceSpan, check-run annotations pinned to
  exact lines, and cross-PR state.

### Why csd has an edge over the CodeRabbit model
CodeRabbit's output is LLM-generated: a model call per PR, non-deterministic. csd's core finding
("this PR adds a forbidden domain -> infra edge", "Rejected is now reachable from Approved") is a
deterministic graph computation. So Option A (pure Action, no hosted compute) is genuinely viable
for the core value, and the hosted layer becomes integration polish (inline linking, annotations,
comment threading) rather than load-bearing for correctness. The diagram is the error message
attached to a deterministic failed assertion - cheaper and cleaner to host than a per-PR LLM
review.

### How it maps to the backlog
- The `cli-diff` issue (M1) already produces the exit code + the Mermaid diagram: that is the
  engine both options wrap.
- Near-term win: a small Action wrapper (Option A).
- Eventual product surface: the hosted GitHub App (M5).
