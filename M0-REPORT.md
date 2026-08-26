# M0 report: does rename detection lie?

Date: 2026-08-26. Tool version: v0.0.0.

M0 exists to answer one question before any renderer is built (SPEC.md section 6): **how often
does a pure refactor produce a spurious add/delete pair?** That number decides the project.

## Verdict: conditional GO

- **Correctness is proven.** The set diff, the fingerprint rename/move matcher, the git
  file-rename module seed, and the anti-noise guarantee are verified by 52 tests, including
  end-to-end golden fixtures (`crates/csd-goldens`) that assert exact classifications: a type
  rename yields one `Modified`, a move yields `Moved`, a member-type change yields a
  fingerprint-changed `Modified`, and a formatting-only edit yields an empty delta. The
  false-merge guard holds in the adversarial unit test (a genuinely different pair stays
  `Added` + `Removed`).
- **The real-corpus false-positive RATE is not yet measurable** on a blind recent-history
  sample, for two reasons below. The harness pipeline itself (clone, first-parent pairs, worktree
  materialization, per-SHA IR cache, threshold sweep) runs end to end on real repos.

So: proceed to M1 (the module graph is independent of this open question), but do **not** publish
a real-corpus rename false-positive rate until the two follow-ups below are done.

## What was run

`csd-harness sweep <repo> <pairs>` on the fixed corpus, rename detection off vs on across the
threshold sweep `[0.5, 0.6, 0.7, 0.8, 0.9]`.

| run | added | removed | moved | modified | git .rs renames | spurious splits | false merges |
| --- | --- | --- | --- | --- | --- | --- | --- |
| ripgrep, 20 pairs (off) | 37 | 0 | 0 | 6 | 0 | n/a | n/a |
| ripgrep, 20 pairs (0.7) | 37 | 0 | 0 | 6 | 0 | 0 | 0 |
| ripgrep, 80 pairs (off) | 43 | 1 | 0 | 13 | 0 | n/a | n/a |
| ripgrep, 80 pairs (0.5-0.9) | 42 | 0 | 0 | 14 | 0 | 0 | 0 |
| bat, 50 pairs (all) | 19 | 0 | 0 | 5 | 0 | 0 | 0 |

The one informative event: in ripgrep at 80 pairs the single `Removed` node (rename off) was
reconciled into a `Modified` once detection was on (removed 1 to 0, modified 13 to 14), with **0
spurious splits and 0 false merges flagged**. The matcher fired, and it fired correctly. But n=1
is not a rate.

## Why the sample is thin (two confounds)

1. **Structural churn in a blind recent sample is low, by design.** A fingerprint is members,
   parameter names, field types and a doc hash, not the function body. Most commits in a mature
   repo change bodies, docs, tests, CI or dependencies, none of which move the structural graph.
   The tool is correctly silent on them. Rename and move events are genuinely sparse in a random
   recent slice, so a blind sample cannot exercise the failure mode M0 targets. The fix is to
   **select refactor commits on purpose** (commit-subject regex `rename|move|refactor` plus a
   `git diff -M` rename filter), concentrating the events instead of hoping to stumble on them.
2. **Workspaces are flattened.** The extractor projects every crate root under a single `crate::`
   prefix (a documented v0.0.0 limitation). ripgrep and tokio are multi-crate workspaces, so
   module paths from different crates collide and the diff signal is confounded. bat is closer to
   single-crate. **Per-crate id namespacing** is needed before workspace numbers mean anything.

Neither confound is a correctness bug; both are measurement gaps.

## Follow-ups filed

- `harness-refactor-sampling`: select refactor-heavy pairs (subject regex + `git diff -M`) so the
  sweep concentrates real rename/move events. This is what turns M0 from "inconclusive" to a
  measured rate.
- `extract-workspace-ids`: namespace node ids per crate so multi-crate workspaces are not
  flattened under `crate::`.

## What this de-risks

The engineering risk the spec flagged as "months" (SPEC.md section 5) is rename detection that
lies. On every controlled input we can construct, it does not: pure renames become `Modified`,
moves become `Moved`, unrelated pairs stay split, and formatting is invisible. The remaining work
is measurement fidelity on real corpora, not correctness of the matcher.
