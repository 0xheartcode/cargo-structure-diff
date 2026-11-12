# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project aims to follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html) from 0.1.0 onward. Interfaces are not
yet stable before 1.0.

Minimum supported Rust version (MSRV): **1.82**.

## [0.1.0] - Unreleased

First tagged release. The pipeline is real end to end: extraction, diff with rename and move
detection, six views, five output formats, architectural lints, a ratchet gate, and a PR-commenting
CI action. Validated by replaying 250 merged pull requests across ripgrep, bat, tokio, axum and
nushell: **zero false red builds, zero non-deterministic results, zero renames mis-reported as
delete + add**.

### Added
- `csd-harness replay`: a PR-replay harness that runs the exact gate verdict path over a corpus of
  merged pull requests and records verdicts, findings, a determinism hash, runtime and rename-split
  noise. The evidence behind the numbers above.
- `csd init` infers a layered architecture on a multi-crate workspace: one layer per crate, tiered
  by dependency depth, with `forbid` rules banning upward dependencies. The rules ship as a
  `layering` warning with a commented `deny` to promote, so adopting the tool never turns a build
  red by surprise.
- Zero-config defaults: with no `.csd.toml`, `csd diff` runs the module view, warns on new module
  cycles (never denies), and points at `csd init`.
- `docs/gallery/`: generated examples of the exact comment the gate posts, including a layering
  violation and a rename shown as a move. Regenerate with `scripts/gen-gallery.sh`.

### Changed
- `csd diff --view <name>` now overrides the views enabled in `.csd.toml` instead of being silently
  ignored, and validates the name. `call-graph` is accepted as an alias for `callgraph`.
- `csd init` generates a config that denies nothing by default. Module cycles are a warning, not a
  deny, because Rust module graphs are naturally cyclic; the unimplemented `fan_in`/`fan_out` lints
  are no longer emitted.
- `Report::print` surfaces lint findings whenever any exist, not only on a denial, so warnings are
  visible in the gate comment and in interactive use.
- The `csd-gate` GitHub Action hardened for real pull requests: a defensive base-ref fetch with a
  clear error, a single sticky comment carrying findings plus a collapsible changelog and Mermaid
  diagram, a compact fallback when the comment exceeds GitHub's size limit, and a job-summary
  fallback when a fork pull request's token cannot post a comment.
- CI runs the test suite on Linux, macOS and Windows; a `.gitattributes` normalizes line endings so
  golden tests are deterministic across platforms.

### Known limitations
- Rust only. The IR is language-agnostic by design, but the only extractor today is for Rust.
- The call graph has a dynamic-dispatch ceiling: `Arc<dyn Trait>` and generic call sites have no
  statically known callee and are rendered honestly rather than guessed.

[0.1.0]: https://github.com/0xheartcode/cargo-structure-diff/releases/tag/v0.1.0
