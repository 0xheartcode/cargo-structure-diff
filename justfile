# cargo-structure-diff developer tasks
# run `just` to list recipes

set shell := ["bash", "-uc"]

_default:
    @just --list

# build the whole workspace
build:
    cargo build --workspace

# run the full test suite
test:
    cargo test --workspace

# format check (no writes)
fmt-check:
    cargo fmt --all -- --check

# apply formatting
fmt:
    cargo fmt --all

# clippy with warnings denied (matches CI)
clippy:
    cargo clippy --workspace --all-targets -- -D warnings

# fast type-check
check:
    cargo check --workspace --all-targets

# local gate: everything CI runs, in order
gate: fmt-check clippy test
    @echo "gate passed"

# bob-pass quality gate: automatable subset (fmt-check, clippy, test)
bob: fmt-check clippy test
    @echo "note: full bob-pass clean-code review (CRAP hotspots, interface depth, lying-tests) is run locally via Claude Code before merge"

# backlog helpers (git-native-issue)
issues:
    git issue ls

issue id:
    git issue show {{id}}

# regenerate the working-tree backlog index from refs/issues
backlog:
    scripts/gen-backlog.sh
