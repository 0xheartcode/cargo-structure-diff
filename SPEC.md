# cargo-structure-diff (`csd`) - specification

This is the design of record. It is the original project thesis with the review amendments from
2026-08-26 folded in. Changed or added decisions are tagged **[AMEND]**.

## 1. Thesis

Prior "UML for Rust" crates died because a diagram generator has no daily-use loop. `csd`
inverts the framing: the primary artifact is a **CI assertion**; the diagram is the error
message attached to a failed assertion, rendered only when a human needs to see why the build
went red. That gets it run on every PR whether anyone opens the picture or not.

Two supporting insights:

- **Do not diff rendered images.** Graph layout is unstable; one node added re-ranks the whole
  drawing. Extract a semantic graph at `base` and at `head`, diff the models, render one picture
  with the delta colour-encoded. Layout instability stops mattering because there is one drawing.
- **The wedge is the diff.** Others generate a diagram of a change; nobody diffs two structural
  graphs and links the result back to the diff hunks.

## 2. Scope of views

Four views plus one optional, all legitimate UML types under native names.

| `csd` view | UML name | Derivable | Diff quality |
|---|---|---|---|
| Module graph | Package diagram | Yes | Excellent |
| Type & trait structure | Class diagram | Yes | Good |
| Call graph / sequence slice | Sequence diagram | Partial (dyn dispatch) | Fair |
| State machine | State machine diagram | Yes | Excellent |
| Schema (optional) | ER | Yes, if Diesel/SeaORM | Excellent |

Out of scope: activity, use case, timing, interaction overview, composite structure, profile,
object diagrams. The information is not in the source, or its granularity makes every diff noise.

## 3. Views in detail

### 3.1 Module graph (build first)
- Extract: `mod` tree plus resolved `use` paths. Nodes are modules; edges are "references an
  item from".
- Render: Mermaid `flowchart LR`.
- Diff: flat set diff over nodes and edges.
- Lints: `layering`, `cycles`, `fan_in` / `fan_out`.

**[AMEND] Resolver honesty.** Module edges are *resolved* `use` references, and tree-sitter
gives syntax, not resolution (`pub use` chains, globs, preludes, `#[path]`, macro-generated mods
are invisible to it). M1 needs a precision-first mini-resolver: emit only edges we are certain of
(explicit paths, resolved intra-crate re-export chains), record globs as unresolved rather than
guessing. For a CI gate a missed edge only costs coverage; a false edge costs a spurious red
build, which costs trust. M0 must therefore measure edge precision/recall, not only rename false
positives.

### 3.2 Type & trait structure
- Extract: item declarations. struct/enum to nodes; trait to interface node; `impl Trait for T`
  to realization edge; field types to association edges. Unwrap `Arc`, `Box`, `Rc`, `Vec`,
  `Option` to reach the inner type.
- Render: Mermaid `classDiagram`, or Graphviz HTML-like labels if per-member colour is needed
  (Mermaid `classDiagram` cannot colour individual members).
- Diff: three-level - node add/remove, member matching within surviving nodes, signature
  comparison.
- Lints: none owned here. **[AMEND] Drop `public_api_break`** - cargo-semver-checks owns that
  slot and is mature; competing there dilutes effort.

### 3.3 Call graph, sliced into sequences
- Extract: call expressions resolved to definitions. Graph is the artifact; a sequence diagram
  is a slice from an entry point.
- Render: Mermaid `sequenceDiagram`, changed regions in `rect rgb(...)`.
- Diff: tree edit distance (GumTree / Zhang-Shasha), not set diff - call order is significant.
- Fidelity ceiling: `Arc<dyn Trait>` and generics have no statically known callee. Render
  honestly (an `alt` over impls, or a dashed `dyn` edge). Hybrid escape hatch: run the test suite
  under a `crustrace-mermaid` tracing layer in CI and diff observed traces. (`crustrace-mermaid`
  confirmed to exist on crates.io, v0.1.6.)
- Lints: `io_in_hot_path`.

### 3.4 State machine (the differentiator)
- Extract: an enum plus the functions consuming and producing its variants. A transition is
  `(from_variant, to_variant, fn_name)`.
- **[AMEND] Idiom coverage.** Recover transitions from more than match-arm-returns-variant:
  handle `*self = State::B`, `self.state = State::B`, and `fn step(self) -> Result<State>`
  return-variant forms. Missing these whiffs on most real machines.
- Render: Mermaid `stateDiagram-v2`.
- Diff: flat set diff over transition triples.
- Lints: `unreachable_state`, `terminal_state_without_exit`, `new_state_cycle`.

### 3.5 Schema (optional)
Diesel/SeaORM `schema.rs` is generated and machine-readable. Tables, columns, FKs fall out.
Lint: `destructive_migration`.

## 4. Architecture

One graph IR; every view is a projection over it. Do not build four extractors, four differs,
four renderers.

```
   git base ---> extract ---> IR(base) ---\
                                           >--> diff ---> Delta ---> render ---> view
   git head ---> extract ---> IR(head) ---/                          \--> lint ---> exit code
```

The IR contract lives in `crates/csd-ir` (see that crate's `lib.rs`): `Node`, `Edge`, `Graph`,
`Change`, `StableId`, `SourceSpan`, `NodeKind`, `EdgeKind`. Determinism is a requirement, not a
nicety: sorted collections everywhere so golden output is byte-stable.

Both refs are materialised via `git worktree add` into temp dirs, extracted independently, then
diffed. No stateful index, no daemon.

**[AMEND] Extraction backends.** Three candidates, decided per view with M0 data:
- tree-sitter: fast, no build, multi-language, but shallow (no name resolution). Default for
  modules/states/calls.
- rustdoc JSON (`cargo rustdoc --output-format json`, nightly): fully resolved items, paths,
  impls, field types with zero resolver work; requires both refs to compile; no function bodies.
  Best candidate for the types view (3.2); revisit at M3.
- `ra_ap_*` (rust-analyzer as a library): true semantic resolution, but republished from HEAD
  with no semver stability. Take on only if measured false-positive rates prove the others
  insufficient.

### Rendering convention (fix once, reuse across views)
Green added, red-dashed removed, amber modified, gray unchanged context. Prune to changed nodes
plus <= 2 hops; never render the whole system.

## 5. The hard problem: stable identity

If node IDs are fully-qualified paths, moving `OrderService` from `billing::` to `payments::` is
a delete plus an add, and the diagram lights up for a pure refactor. That single failure mode
destroys trust. Rename/move detection is a similarity heuristic over structural fingerprints
(method set, field type multiset, edge neighbourhood, doc-comment hash); score candidate pairs,
accept above a threshold, emit `Moved` instead of `Added` + `Removed`.

**[AMEND] Use git's own rename signal as an input, not just a benchmark.** Module moves are
usually file moves, and `git diff -M` detects those cheaply and precisely. Seed module-level
`Moved` from that signal, leaving structural fingerprints to handle intra-file type moves. This
materially shrinks the risk in this section.

Adding a fifth view is a weekend. Making rename detection not lie is months. Budget accordingly.

## 6. Validation harness (build before features)

Replay several hundred merged PRs from `tokio`, `ripgrep`, `bat` and measure how often a pure
refactor produces a spurious add/delete pair. That number decides the project.

**[AMEND] Measure both failure directions:**
1. Spurious-split rate - git says a file was renamed, csd said `Added` + `Removed`.
2. False-merge rate - csd emitted `Moved` for two genuinely different items, silently swallowing
   a real change. Worse than a split; audit the lowest-scoring accepted `Moved` pairs by hand.
3. Edge precision/recall - compare module-edge sets against a `cargo-modules` (ra_ap) oracle on
   pinned SHAs.

Ground truth is seeded from `git diff -M50 --name-status` plus a commit-subject regex
(`rename|move|refactor`), then hand-audited on a sample.

## 7. Config: `.csd.toml`

See `.csd.toml` at the repo root for the working example. Layers are declared as an ordered list
plus a **[AMEND] path-glob `map`** from module path to layer (the original spec declared layers
but never said how a module lands in one). Workspaces get **[AMEND] crate-level layering** in
addition to module-level. Default CI behaviour is **[AMEND] ratchet mode**: fail only on
violations new since the base ref - the brownfield adoption unlock, and a natural consequence of
being diff-native rather than a bolted-on feature.

## 8. Distribution

`csd` is the primary binary; the cargo shim is one distribution surface, not the architecture
(a cargo subcommand is Rust-only by construction). Both ship from the one
`cargo-structure-diff` crate via two `[[bin]]` targets.

**[AMEND] Naming reality.** `csd` (crate and binary) is taken on crates.io by an unrelated
search-and-replace tool; `csdiff` collides with Red Hat csutils. We publish
`cargo-structure-diff`, ship the `csd` binary, and reserve `strc` as the fallback rename if the
`csd` binary collides on a user's PATH.

**[AMEND] PR comments without the hosted app.** A thin GitHub Action using `GITHUB_TOKEN` can
post the delta as a PR comment without the hosted layer. This erodes the commercial moat while
boosting adoption; treat it as a conscious decision, deferred to M5, not a default.

Keep the core language-agnostic: the IR contains no Rust; extraction sits behind an `Extractor`
trait so other languages are additive.

## 9. Milestones

- **M0** Harness: IR + `git worktree` extraction + PR replay measuring rename false positives
  and edge precision. No renderer.
- **M1** Module graph: extract, set diff, Mermaid render, `layering` + `cycles` lints, non-zero
  exit, ratchet mode. Ship and publish here.
- **M2** State machines.
- **M3** Type & trait structure; resolve Mermaid-vs-DOT and tree-sitter-vs-rustdoc-JSON.
- **M4** Call graph; `crustrace` traced mode as a separate subcommand.
- **M5** GitHub App / Action: PR comments, hunk-to-node linking, check-run annotations.

## 10. Non-goals

Do not rebuild the code diff view. No round-tripping (diagram to code). No GUI editor. Do not
market as "a UML tool"; the views are UML types and can be labelled as such only for audiences
with a documentation mandate.

## Prior art (**[AMEND]**, informs positioning)

- dependency-cruiser (JS), ArchUnit (Java), import-linter (Python): established layering linters.
  They validate the market and the skew toward large Java/TypeScript codebases. None of them
  diff two graphs or render a delta - that is `csd`'s novel layer.
- cargo-modules: the extraction quality benchmark (ra_ap based).
- cargo-semver-checks: owns public-API break detection; integrate or stay out of its lane.

## Backend strategy (decision, 2026-08-26)

Extraction backends are chosen per view, not globally, and ranked by how much
instability they force on the build:

1. **tree-sitter is the default and always-available base** for every view. It is fast,
   multi-language, and - critically for a diff tool run on two refs mid-refactor - it
   parses code that does not compile. It is shallow (no name resolution); that is the
   accepted trade.
2. **rustdoc JSON is the opt-in backend for the type/trait view** (see
   `docs/spikes/rustdoc-json-vs-tree-sitter.md`). It resolves field types, generics, and
   impls that tree-sitter cannot, at a bounded cost (nightly, must-compile, a pinned
   `rustdoc-types` version). Confining it to one opt-in view means a non-compiling ref or
   a missing nightly degrades only that view.
3. **rust-analyzer (`ra_ap_*`) is the last resort, feature-flag gated, for the call view
   only.** Method-receiver resolution (`x.method()`) needs real type inference, which
   neither tree-sitter nor rustdoc (no bodies) can provide. `ra_ap_*` has no semver and is
   republished from HEAD, so it must never be load-bearing for the default path; quarantine
   it behind a Cargo feature so its breakage cannot sink `csd`. Prefer the crustrace traced
   mode (observed call traces, true by construction) before reaching for it.

Net: tree-sitter default -> rustdoc-JSON opt-in for types -> `ra_ap_*` last, flag-gated.
