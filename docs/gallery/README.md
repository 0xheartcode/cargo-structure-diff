# csd gate gallery

Each file here is the **exact sticky comment** the `csd` gate would post on a pull request. They are
generated, not hand-written: `scripts/gen-gallery.sh` runs the real gate script against a
materialized commit and captures its output verbatim. Regenerate with:

```sh
cargo build && scripts/gen-gallery.sh
```

The real examples replay a merged PR from the validation corpus (see `crates/csd-harness`); the
synthetic ones build a tiny repo on the fly to show one behaviour cleanly.

| Example | Source | What it shows |
|---|---|---|
| [01 axum: new module cycle](01-axum-new-module-cycle.md) | axum `#3856` | A PR adds a module to a crate whose modules already form a cycle. The gate **passes** and surfaces the cycle as a **warning** — signal, never a red build, because Rust module graphs are naturally cyclic. |
| [02 ripgrep: new module cycle](02-ripgrep-new-module-cycle.md) | ripgrep `ignore: incremental` | The same, in a different codebase: a warning on a passing build. |
| [03 ripgrep: large PR, clean](03-ripgrep-large-pr-clean.md) | ripgrep indexing scaffolding | A ~1200-change PR summarised into a grouped changelog and one colour-coded diagram, with a green verdict. |
| [04 ripgrep: non-structural PR](04-ripgrep-non-structural.md) | ripgrep release bump | A PR that changes no structure at all: the gate is correctly **silent** ("No structural changes"). Half of real PRs look like this. |
| [05 layering violation](05-layering-violation.md) | synthetic | The gate's teeth: a `domain` module reaches into `infra`, a declared-forbidden dependency, so the gate **FAILS** with a `[deny] layering` finding naming both sides. This is the value — enforcing architecture you declared. |
| [06 rename shown as a move](06-rename-shown-as-move.md) | synthetic | The trust-defining guarantee: a file renamed `billing.rs` → `payments.rs` is reported as **moved**, never as a deletion plus an addition. A tool that lit up on every refactor would be uninstalled by lunchtime. |

The gate never turns a build red unless a declared architectural rule is violated (example 05). New
module cycles (01, 02) are warnings. This is deliberate: across 250 replayed merged PRs the gate
produced **zero** false red builds. See the Phase-1 report in `docs/phase1-report.md`.
