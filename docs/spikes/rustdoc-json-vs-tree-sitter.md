# Spike: rustdoc JSON vs tree-sitter for the types view

Backlog id `0e76290` (`spike-rustdoc`), milestone M3, area `extract`. Time-boxed
evaluation, no production code. Scope: the type and trait view (SPEC.md 3.2) only. The
extraction-backend decision is per view (SPEC.md 4 [AMEND]).

## Question

SPEC.md 3.2 wants the types view to emit, per crate: struct/enum nodes, trait interface
nodes, `impl Trait for T` realization edges, and field-type association edges (unwrapping
`Arc`/`Box`/`Rc`/`Vec`/`Option` to the inner type). Which backend produces that with
acceptable cost: the current tree-sitter extractor, `cargo rustdoc --output-format json`,
or `ra_ap_*`?

## Summary

| backend       | resolution        | build needed | speed          | stability            | bodies |
| ---           | ---               | ---          | ---            | ---                  | ---    |
| tree-sitter   | none (syntax)     | no           | fast, ~ms      | grammar is stable    | yes    |
| rustdoc JSON  | full, by item id  | yes, nightly | compile-bound  | unstable FORMAT_VER  | no     |
| `ra_ap_*`     | full (semantic)   | no (indexes) | index-bound    | none (HEAD republish)| yes    |

Resolution is the axis that matters for 3.2. Realization and association edges are
name-resolution problems, and tree-sitter cannot solve them; that is the whole reason the
row exists.

## What tree-sitter cannot do (current backend)

`crates/csd-extract-rs/src/lib.rs` builds nodes and fingerprints from the syntax tree. It
is deliberately shallow and its own module doc says so:

- It emits `NodeKind::Struct`/`Enum`/`Trait`/`Fn`/`Module`/`Variant` and, for edges, only
  `EdgeKind::Uses`. The IR already defines `Implements` and `Associates`
  (`crates/csd-ir/src/lib.rs`), but the extractor emits **neither**. `impl` blocks are not
  walked at all (no `impl_item` arm in `emit_item`), so there are zero realization edges.
- Field types are recorded as syntactic names in the fingerprint (`struct_fingerprint` ->
  `add_types` -> `collect_type_names`). Unwrapping is a hard-coded five-name list
  (`WRAPPERS = [Arc, Box, Rc, Vec, Option]`). A field typed `BTreeMap<String, String>`
  records the bare name `BTreeMap` and drops the inner `String, String`; anything outside
  the five wrappers is not unwrapped. The name is text, not a link to the defining item, so
  two distinct `Error` types in different modules are indistinguishable.
- No name resolution. Re-exports (`pub use`), glob imports, prelude names, and external
  crate paths are not resolved into the item they name. The mini-resolver
  (`extract-resolver`, `bb67bbb`) only resolves `use` paths to *module* edges, and only
  when certain; it does not resolve a field type or an `impl` target to a type node.

Net: with tree-sitter, the types view can list types and approximate their member
fingerprints, but it cannot draw the realization graph or a trustworthy association graph.

### Tie-back to the M0 finding

M0 (`M0-REPORT.md`) proved the rename/move matcher does not lie, but it also confirms the
graph the matcher runs on carries **no resolved cross-item edges** beyond module `use`
edges: fingerprints are "members, parameter names, field types and a doc hash, not the
function body," and edge kinds `Implements`/`Associates`/`Calls` are unpopulated. For the
modules/states view that is fine (M0 verdict: GO, correctness proven). For the types view
it is the gap: the interesting 3.2 signal (which trait a type gains or loses, which
association appears) does not exist in the current IR, and tree-sitter cannot add it
without reimplementing name resolution, which is exactly what the `ra_ap_*`/rustdoc rows
avoid.

## rustdoc JSON: hands-on findings

Nightly was **not** installed at the start (`rustup toolchain list` showed only
`stable-x86_64-unknown-linux-gnu`). `cargo +nightly rustdoc` auto-provisioned it on first
use. This spike therefore ran the real command against a workspace crate:

```
cargo +nightly rustdoc -p csd-ir -- --output-format json -Z unstable-options
# nightly 1.100.0-nightly (787af2b8c 2026-08-25), exit 0, ~0.16s after compile
# output: target/doc/csd_ir.json, 309,893 bytes for a ~7-struct crate
```

Observed shape of `csd_ir.json`:

- Top-level keys: `root`, `crate_version`, `includes_private`, `index`, `paths`,
  `external_crates`, `target`, `format_version`. **`format_version` is `61`.**
- `index`: 360 entries (this crate's items, keyed by numeric id). `paths`: 934 entries
  (fully-qualified path + kind for every id referenced, local and external). Every type
  reference is an id into one of these two tables, so a field type is a link, not a string.

Field types are fully resolved. For `struct SourceSpan`:

```
file : {"resolved_path":{"path":"String","id":2}}
start: {"primitive":"u32"}
end  : {"primitive":"u32"}
```

For `struct Node`, generic arguments survive intact:

```
attrs      : resolved_path BTreeMap, args angle_bracketed [String, String]
fingerprint: resolved_path Option,   args angle_bracketed [Fingerprint (id 7)]
```

This is the direct contrast with tree-sitter: `attrs` keeps `<String, String>` (tree-sitter
would keep only `BTreeMap`), and `fingerprint`'s inner `Fingerprint` is an id pointing at
the defining struct in `index`, not the text `Fingerprint`.

Realization edges are present and resolved:

```
227 impl blocks in index; 225 carry a `trait`; all 225 resolve their `for` type
to an id in THIS crate's index (Fingerprint, Node, Edge, Graph, ...).
```

Caveat, and it is a real one: the overwhelming majority of those 225 are **synthetic and
blanket impls** (`Send`, `Sync`, `Freeze`, `Unpin`, `UnwindSafe`, `RefUnwindSafe`,
`Borrow`, `From<T> for T`, etc.), not user-written trait realizations. A types view built on
rustdoc JSON must filter impls (drop auto-traits and blanket impls, keep user
`impl Trait for T`) or the class diagram drowns in noise. rustdoc marks these
(`is_synthetic`, blanket-impl provenance), so the filter is mechanical, but it is required
work, not free.

Field name change in this version worth noting for any future reader: the impl's trait
field is `trait` (bare), not `trait_`; auto-trait impls sit under `synthetic`. Field names
move between format versions (see stability below).

## Costs of rustdoc JSON (do not oversell it)

- **Nightly required.** `--output-format json` is `-Z unstable-options`, nightly-only, with
  no stabilization date. CI must pin and install a nightly toolchain.
- **Must compile.** rustdoc runs the front end, so **both** git refs (base and head) must
  build. A ref that does not compile yields no types view for that side, and the diff
  silently loses a whole view exactly when the code is mid-refactor, which is when a
  structure diff is most wanted. tree-sitter has no such failure mode (it parses broken
  code). This alone argues against making rustdoc the default backend.
- **Format churn.** `FORMAT_VERSION` is an integer that bumps on any schema change (we
  observed `61`; the `trait_` -> `trait` and `synthetic` field shapes confirm churn is
  real). A consumer must gate on the exact version and carry a shim per supported version.
  Prior art absorbs this the same way: the `public-api` crate and `cargo-semver-checks`
  both pin `rustdoc-types` to a known `FORMAT_VERSION` and update on bump rather than
  parsing free-form JSON. We would depend on `rustdoc-types` and accept the same update
  treadmill.
- **No bodies.** rustdoc JSON has signatures and items but no function bodies, so it can
  never feed the call-graph view (3.3). It is a types/trait/signature backend only. This is
  fine because the decision is per view; it just means rustdoc cannot be the one backend.
- **Workspace shape.** rustdoc JSON is per crate (one `<crate>.json` each). That aligns
  with the `extract-workspace-ids` direction (per-crate id namespacing) but means the types
  view must invoke rustdoc per crate and stitch, versus tree-sitter's single filesystem
  walk.

## `ra_ap_*` (brief)

rust-analyzer as a library gives true semantic resolution (types, traits, method
resolution, macro expansion) and, unlike rustdoc, does not require a clean compile or
bodies to be absent. The disqualifier is stability: the `ra_ap_*` crates are republished
from rust-analyzer HEAD with no semver guarantee, so the dependency breaks on their
schedule, not ours. SPEC.md 4 [AMEND] already scopes it correctly: take it on only if
measured false-positive rates prove tree-sitter and rustdoc both insufficient. Nothing in
this spike changes that; it stays the escape hatch, not the M3 choice.

## Recommendation

Adopt **rustdoc JSON as an opt-in backend for the types view (3.2) only**, keeping
tree-sitter as the default for the modules, states, and calls views. Rationale:

1. The types view's defining edges (`Implements`, `Associates`) are name-resolution
   results. tree-sitter provably cannot produce them (empty `impl` handling, five-name
   unwrap, string-not-id types), and M0 confirms the current IR carries none. rustdoc JSON
   produces all of them, resolved, with zero resolver code on our side (verified: 225
   resolved realizations and full generic field types on `csd-ir`).
2. The costs are real but bounded and view-local: nightly + must-compile + `FORMAT_VERSION`
   churn are exactly what `public-api` and `cargo-semver-checks` already live with via
   `rustdoc-types`. Confining rustdoc to one opt-in view means a non-compiling ref or a
   missing nightly degrades only the types view, and the modules/states/calls views (the
   M0-proven core) keep working from tree-sitter.
3. Do **not** make it the default and do **not** make it the sole backend: it needs
   nightly, it needs both refs to build, and it has no bodies (so it cannot serve 3.3).

Concretely for M3: add a `--backend=rustdoc` (or per-view config) path that shells
`cargo +nightly rustdoc --output-format json` per crate, parses via a pinned
`rustdoc-types` version, filters impls to user-written `impl Trait for T`, and maps items
onto the existing `Struct`/`Enum`/`Trait` nodes and the already-defined `Implements`/
`Associates` edges. When nightly or a clean build is unavailable, fall back to the
tree-sitter types view (nodes and member fingerprints, no realization graph) rather than
failing the run.

## Reproduction

```
cargo +nightly rustdoc -p csd-ir -- --output-format json -Z unstable-options
# then inspect target/doc/csd_ir.json: format_version, index, paths, per-field
# struct_field types, and impl.trait / impl.for ids.
```
