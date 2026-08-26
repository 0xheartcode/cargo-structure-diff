# cargo-structure-diff (`csd`)

> Extract structural graphs from a codebase, diff them across two git refs, render the delta as
> one annotated diagram, and fail CI when a change violates a declared architectural constraint.

**Status:** v0.0.0, pre-alpha. Scaffolding and backlog only. Nothing here does real work yet.

## What it is

Every prior "UML for Rust" tool died because a diagram generator has no daily-use loop. `csd`
inverts that: the primary artifact is a **CI assertion**, and the diagram is the error message
attached to a failed assertion. It runs on every PR whether or not anyone opens the picture.

It does not diff rendered images (graph layout is unstable). It extracts a semantic graph at
`base` and at `head`, diffs the models, and renders one picture with the delta colour-encoded.

See [`SPEC.md`](SPEC.md) for the full design.

## Binaries

Ships from one crate as two `[[bin]]` targets:

- `csd` (primary, language-agnostic aim)
- `cargo-structure-diff` (cargo subcommand shim, `cargo structure-diff ...`)

> Note: the `csd` crate name and binary name already exist on crates.io (an unrelated
> search-and-replace tool). We publish `cargo-structure-diff`; if the `csd` binary collides on a
> user's PATH the reserved fallback is `strc`.

## Development

Requires stable Rust (see `rust-toolchain.toml`) and [`just`](https://github.com/casey/just).

```sh
just          # list recipes
just build
just gate     # fmt-check + clippy -D warnings + test (what CI runs)
just issues   # show the local backlog (git-native-issue)
```

## Backlog

Work is tracked as local [git-native-issue](https://github.com/remenoscodes/git-native-issue)
issues under `refs/issues/`, not GitHub. A working-tree mirror lives in
[`BACKLOG.md`](BACKLOG.md); regenerate it with `just backlog`.

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option.
