# cargo-structure-diff (`csd`)

> Extract a structural graph from a Rust codebase, diff it across two git refs, render the delta as
> one annotated diagram, and fail CI when a change crosses an architectural boundary you declared.

**Status:** v0.1.0, pre-1.0 and moving fast. The pipeline is real end to end; interfaces are not yet
stable. Rust only for now.

## The one number that matters

A gate that cries wolf gets uninstalled. So csd was measured before it was tagged: **250 merged pull
requests replayed across ripgrep, bat, tokio, axum and nushell produced zero false red builds, zero
non-deterministic results, and zero renames mis-reported as delete + add.** The gate turns a build
red only when a change violates an architectural rule you declared. See
[`docs/gallery/`](docs/gallery/) for the exact comments it posts, and run the harness yourself with
`cargo run -p csd-harness -- replay`.

## 30-second quickstart

```sh
# Install the binary (published as cargo-structure-diff; the binary is csd).
cargo install cargo-structure-diff

# See what a pull request changes structurally, against its base branch.
csd diff --base main --show

# Generate a starter config. On a workspace it infers layers from your crate graph.
csd init

# Gate the change: exit non-zero when a declared architectural rule is violated.
csd diff --base main
```

With no `.csd.toml`, `csd diff` is informational: it summarises the delta and warns on new module
cycles, but never fails the build. It gains teeth once you declare layers (see the CI gate below).

## What it is

Every prior "UML for Rust" tool died because a diagram generator has no daily-use loop. `csd` inverts
that: the primary artifact is a **CI assertion**, and the diagram is the error message attached to a
failed assertion. It runs on every PR whether or not anyone opens the picture.

It does not diff rendered images (graph layout is unstable). It extracts a semantic graph at `base`
and at `head`, diffs the models, and renders one picture with the delta colour-encoded. The hard part
is stable identity: a type moved from one module to another is reported as a **move**, not a deletion
plus an addition, so a pure refactor does not light up the diagram. That guarantee is what the replay
above verifies.

See [`SPEC.md`](SPEC.md) for the full design.

## What csd is NOT

csd is **not a code reviewer**. It finds no logic bugs, no security issues, no style problems. It
answers one question deterministically: *did this change cross an architectural boundary?* It
complements an LLM reviewer (CodeRabbit, Claude, and the like) rather than competing with it — the
LLM reads intent probabilistically; csd enforces structure by computation, the same way every time.

## Views and formats

Six views over one IR — modules, types, call graph, state machines, schema (Diesel/SeaORM), and a
combined overview — rendered as `mermaid`, `dot`, `ascii`, `boxes`, or `svg`. The module view is the
gate's error message; the others are available via `csd doc` or by listing them in `.csd.toml`.

```sh
csd changelog --base main          # grouped, human-facing delta for a PR body or release note
csd diff --base main --view types  # render a specific view
csd doc -o STRUCTURE.md            # whole-codebase structure report, one diagram per view
csd files                          # per-file index of what each source file defines
csd snapshot -o structure.json     # pin the current structure; gate later work against it
csd walk --base HEAD~20 --format ascii   # one frame per commit, to watch the architecture evolve
csd drift --base main              # a compact count of how far the structure has moved
```

## CI gate

The composite action diffs a pull request against its base, posts the structural changelog plus any
violations as one sticky comment (with a collapsible Mermaid diagram), and fails the check on a lint
denial. It handles shallow clones, oversized comments, and fork pull requests (where it falls back to
the job summary).

```yaml
# .github/workflows/structure.yml in a consumer repo
on: pull_request
permissions: { contents: read, pull-requests: write }
jobs:
  structure:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
        with: { fetch-depth: 0 }
      - uses: dtolnay/rust-toolchain@stable
      - uses: 0xheartcode/cargo-structure-diff/.github/actions/csd-gate@main
```

This repo dogfoods the same gate on its own pull requests via
[`.github/workflows/structure.yml`](.github/workflows/structure.yml).

To make the gate fail on a real architectural regression, declare layers. `csd init` infers them from
your crate graph as warnings you promote to `deny`:

```toml
[layers]
order = ["app", "core"]              # top (may depend downward) to bottom
map = [
  { layer = "app",  glob = "crates/app/**" },
  { layer = "core", glob = "crates/core/**" },
]
forbid = [{ from = "core", to = "app" }]   # core must not depend on app
[lint]
deny = ["layering"]
```

The ratchet defaults to `new-only`, so CI fails on violations introduced by the change under review,
not on pre-existing ones — the brownfield adoption unlock.

## Known limitations

- **Rust only.** The IR contains no Rust and extraction sits behind an `Extractor` trait, so other
  languages are additive — but the only extractor today is for Rust.
- **Dynamic-dispatch ceiling.** `Arc<dyn Trait>` and generic call sites have no statically known
  callee; the call view renders this honestly (a dashed `dyn` edge) rather than guessing an edge.

## Binaries and naming

Ships from one crate as two `[[bin]]` targets: `csd` (primary) and `cargo-structure-diff` (the cargo
subcommand shim, `cargo structure-diff ...`). The `csd` name is taken on crates.io by an unrelated
tool, so the crate is published as `cargo-structure-diff`; if the `csd` binary collides on your PATH,
the reserved fallback is `strc`.

## Development

Requires stable Rust (see `rust-toolchain.toml`) and [`just`](https://github.com/casey/just).

```sh
just          # list recipes
just gate     # fmt-check + clippy -D warnings + test (what CI runs)
just issues   # show the local backlog (git-native-issue)
```

See [`CHANGELOG.md`](CHANGELOG.md) for release notes and the MSRV.

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option.
