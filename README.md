# cargo-structure-diff (`csd`)

> Extract structural graphs from a codebase, diff them across two git refs, render the delta as
> one annotated diagram, and fail CI when a change violates a declared architectural constraint.

**Status:** v0.0.0, pre-alpha and moving fast. The pipeline is real: extraction, diff with
rename and move detection, six views, five output formats, architectural lints, a ratchet gate,
and a PR-commenting CI action all work. Interfaces are not yet stable.

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

## Usage

```sh
# Gate: diff the working tree against a base ref, render the delta, run the lints.
# Exits non-zero on a denied architectural finding (the diagram is the error message).
csd diff --base main
csd diff --base main --show --format boxes   # also print the diagram, ASCII boxes
csd diff --base main --list                  # one machine-readable line per change

# Changelog: the grouped, human-facing companion to the diagram, for a PR body or release note.
csd changelog --base main

# Snapshot: pin the current structure as JSON, then gate later work against it without the old ref.
csd snapshot -o structure.json
csd diff --baseline structure.json

# Doc: a whole-codebase Markdown structure report (one diagram per view, no delta).
csd doc -o STRUCTURE.md

# Files: a per-file index of what each source file defines, with type members and fn signatures.
csd files

# Init: scaffold a .csd.toml pre-filled with this repo's real crate globs.
csd init

# Walk: flip through one frame per commit to watch the architecture evolve (great with ascii).
csd walk --base HEAD~20 --format ascii --view modules

# Drift: a compact count of how far the structure has moved since a ref or a pinned baseline.
csd drift --base main
csd drift --baseline structure.json

# Version: prints the build's commit, for example csd 0.0.0 (a678f58 2026-08-28T12:19:01Z).
csd --version
```

Views are selected in `.csd.toml` (`[views] enabled`); formats are `mermaid`, `dot`, `ascii`,
`boxes`, and `svg`. The ratchet defaults to `new-only`, so CI fails on violations introduced by the
change under review, not on pre-existing ones. See [`.csd.toml`](.csd.toml) and [`SPEC.md`](SPEC.md).

## CI gate

The composite action diffs a pull request against its base, posts the structural changelog as a
single sticky comment, and fails the check on lint denials:

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
