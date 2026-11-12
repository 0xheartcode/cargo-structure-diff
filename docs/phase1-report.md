# Phase 1 report: prove the gate does not cry wolf, then ship

Goal: get `csd` into real users' CI as a gate they trust. A gate lives or dies on two numbers — how
many red builds were not real problems (noise, must be zero) and how many told a reviewer something
true (signal, must be greater than zero). This report records what the PR-replay harness measured,
every false result found and its root cause, the runtimes, and what comes next.

## Method

`csd-harness replay` walks the last 50 first-parent commits (one per merged PR) of five pinned
corpus repos of different shapes — ripgrep, bat, tokio, axum, nushell — and runs the exact gate
verdict path (`diff` at the gate's 0.7 rename threshold, then `lint`) under two configs: **zero**
(no `.csd.toml`) and **init** (the real `csd init` output, parsed back). It records, per pair, the
verdict, every finding, change counts, a portable determinism hash it re-checks, wall time, and
whether any git `.rs` rename came out as delete + add. 250 pairs total.

## Noise and signal, before and after the fixes

| metric | before | after |
|---|---|---|
| Pairs replayed | 250 | 250 |
| Non-deterministic pairs | 0 | 0 |
| Renames mis-reported as delete + add | 0 (of 0 `.rs` renames) | 0 |
| Zero-config red builds | 0 | 0 |
| `csd init` red builds | **2** (both false) | **0** |
| Real findings surfaced | 0 (findings were hidden) | 2 module-cycle warnings |

Before: the only red builds the gate produced out of the box were two false denials. After: zero
red builds across 250 real PRs, with the two genuine module cycles now surfaced as warnings. Noise
is zero; signal out of the box is advisory (warnings), and becomes a build failure only once a team
declares layering (verified end to end — see the gallery's layering example).

Half of all replayed PRs changed no structure at all (median changes per pair: 0); the gate is
correctly silent on those.

## Every false violation, and its root cause

Both false denials were the `cycles` lint, under the config `csd init` generated:

- **axum** `bc8d4912`, "feat: add InnerPath extractor (#3856)" — flagged a 28-module cycle.
- **ripgrep** `b621e65b`, "ignore: add incremental checking" — flagged a 9-module cycle.

**Root cause: rule-false** (a correct graph, a bad default rule). The module graphs are right — axum
has a 28-module strongly-connected component, ripgrep a 9-module one — but those cycles are ordinary
Rust: 591 of axum's 683 inter-module edges are cross-module `use` references, and crate roots
re-export their children with `pub use`. A new module joining that natural web is not a defect, so
`deny = ["cycles"]` fails builds on healthy code. The new-only ratchet is the only reason just 2 of
250 fired (the SCCs pre-exist; a red needs a newly added edge inside one).

**Fix:** `csd init` now emits `cycles` as a *warning*, never a deny, and drops the `fan_in`/`fan_out`
names it used to emit (the linter never implemented them — dead config). The gate's deny power now
comes only from layering the user declares. Regression tests lock this in. No extractor-false
results were found; rename/move detection did not mis-split anything.

## Runtime distribution

Per pair, cold (two worktree extractions included; adjacent pairs share one cached SHA):

| repo | median | p90 | max |
|---|---|---|---|
| bat | 106 ms | 123 ms | 214 ms |
| tokio | 118 ms | 637 ms | 677 ms |
| ripgrep | 151 ms | 195 ms | 276 ms |
| axum | 688 ms | 821 ms | 1.4 s |
| nushell | 2.5 s | 3.3 s | 4.5 s |

Runtime is extraction-bound and scales with repo size. nushell (a very large workspace) is the only
one above ~1.5 s, and still well within a CI step's budget. No speed work is warranted yet.

## What I would do next, and why

1. **Refine the `cycles` lint to exclude `pub use` re-export edges.** Re-exports inflate the SCCs
   and are not logic dependencies; excluding them would make an opted-in `cycles = deny` meaningful
   instead of noisy. This is the highest-value correctness follow-up.
2. **Findings with source spans, so the gate can link a violation to its diff hunk.** Today the gate
   lists violations by module id; emitting `file:line` would let the PR comment link each one.
3. **Reject flags a subcommand ignores.** `parse_args` is flag-first, so e.g. `csd doc --view x` is
   silently dropped — the same class of bug as the `diff --view` one fixed in Phase 1.
4. **A versioned `--format json` / SARIF output**, so findings flow into code-scanning dashboards.
5. **Validate Windows CI.** The extractor forward-slashes paths and `.gitattributes` normalizes line
   endings, so the tri-OS matrix should pass, but the first real Windows run is the proof.

None of these are blocking. The headline stands: across 250 merged PRs, zero false red builds, zero
non-determinism, zero rename noise. The gate is safe to ship.

## Reproduce

```sh
cargo run -p csd-harness -- replay 50 --out /tmp/replay   # writes replay.json + a markdown summary
```
