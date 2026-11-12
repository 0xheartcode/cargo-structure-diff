#!/usr/bin/env bash
# Regenerate the demo gallery under docs/gallery/.
#
# Each example is the EXACT sticky comment the csd gate would post, captured by running the real gate
# script (scripts/csd-structure-gate.sh) against a materialized commit. Real examples replay a merged
# PR from the validation corpus (clones under ~/.cache/csd-harness, see crates/csd-harness); two
# synthetic examples build a tiny repo on the fly to show the gate's teeth (a layering violation) and
# its anti-noise guarantee (a file move shown as a move, not delete + add).
#
# Usage: scripts/gen-gallery.sh
#   CSD_BIN   csd binary to run (default: target/debug/csd)
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
csd="${CSD_BIN:-$root/target/debug/csd}"
gate="$root/scripts/csd-structure-gate.sh"
cache="${CSD_HARNESS_CACHE:-$HOME/.cache/csd-harness}"
out="$root/docs/gallery"
mkdir -p "$out"

[ -x "$csd" ] || { echo "csd binary not found at $csd; run 'cargo build' first." >&2; exit 1; }

# Render the gate comment for a commit in a corpus clone, diffing it against its parent.
#   $1 slug  $2 repo  $3 head-sha  $4 parent-sha
render_corpus() {
    local slug="$1" repo="$2" head="$3" parent="$4"
    local clone="$cache/$repo"
    [ -d "$clone/.git" ] || { echo "skip $slug: no clone at $clone" >&2; return; }
    local wt; wt="$(mktemp -d)"
    git -C "$clone" worktree add -q --detach "$wt" "$head"
    ( cd "$wt" && CSD_BIN="$csd" CSD_COMMENT=false bash "$gate" "$parent" ) >"$out/$slug.md" 2>/dev/null || true
    git -C "$clone" worktree remove --force "$wt" >/dev/null 2>&1 || true
    echo "wrote $out/$slug.md ($repo)"
}

# Render the gate comment for the head commit of a freshly built synthetic repo. The caller passes a
# function that populates $dir and echoes the base SHA on its last line.
render_synthetic() {
    local slug="$1" builder="$2"
    local dir; dir="$(mktemp -d)"
    local base; base="$($builder "$dir")"
    ( cd "$dir" && CSD_BIN="$csd" CSD_COMMENT=false bash "$gate" "$base" ) >"$out/$slug.md" 2>/dev/null || true
    rm -rf "$dir"
    echo "wrote $out/$slug.md (synthetic)"
}

git_quiet() { git -C "$1" -c user.email=demo@csd.invalid -c user.name=csd "${@:2}"; }

# Synthetic 1: a clean-architecture layering violation (the gate's teeth). A `domain` layer must not
# depend on `infra`; the head commit adds exactly that import, so the gate denies.
build_layering() {
    local d="$1"; mkdir -p "$d/src/domain" "$d/src/infra"
    git -C "$d" init -q
    printf 'pub mod domain;\npub mod infra;\n' >"$d/src/lib.rs"
    printf 'pub mod order;\n' >"$d/src/domain/mod.rs"
    printf 'pub struct Order { pub id: u64 }\n' >"$d/src/domain/order.rs"
    printf 'pub mod db;\n' >"$d/src/infra/mod.rs"
    printf 'pub struct Db;\n' >"$d/src/infra/db.rs"
    printf '[layers]\norder = ["domain", "infra"]\nmap = [\n  { layer = "domain", glob = "src/domain/**" },\n  { layer = "infra", glob = "src/infra/**" },\n]\nforbid = [{ from = "domain", to = "infra" }]\n[views]\nenabled = ["modules"]\n[lint]\ndeny = ["layering"]\n[ratchet]\nmode = "new-only"\n' >"$d/.csd.toml"
    git_quiet "$d" add -A; git_quiet "$d" commit -qm "scaffold domain and infra layers"
    local base; base="$(git -C "$d" rev-parse HEAD)"
    # The offending change: domain now reaches into infra.
    printf 'use crate::infra::db::Db;\npub struct Order { pub id: u64, db: Db }\n' >"$d/src/domain/order.rs"
    git_quiet "$d" add -A; git_quiet "$d" commit -qm "order: cache a Db handle from infra"
    echo "$base"
}

# Synthetic 2: a pure file move. csd must report the module as moved/modified, never as a deletion
# plus an addition - the trust-defining anti-noise guarantee.
build_move() {
    local d="$1"; mkdir -p "$d/src"
    git -C "$d" init -q
    printf 'pub mod billing;\n' >"$d/src/lib.rs"
    printf 'pub struct Invoice { pub cents: u64 }\npub fn total(i: &Invoice) -> u64 { i.cents }\n' >"$d/src/billing.rs"
    git_quiet "$d" add -A; git_quiet "$d" commit -qm "add the billing module"
    local base; base="$(git -C "$d" rev-parse HEAD)"
    git_quiet "$d" mv src/billing.rs src/payments.rs
    printf 'pub mod payments;\n' >"$d/src/lib.rs"
    git_quiet "$d" add -A; git_quiet "$d" commit -qm "rename billing to payments"
    echo "$base"
}

render_corpus "01-axum-new-module-cycle"     axum    bc8d49124352 7c3c67879dc5
render_corpus "02-ripgrep-new-module-cycle"  ripgrep b621e65b4fc8 c55818fd32d6
render_corpus "03-ripgrep-large-pr-clean"    ripgrep 8372866810a1 d99ac34406f8
render_corpus "04-ripgrep-non-structural"    ripgrep 3fce3b5bb023 5055264ae021
render_synthetic "05-layering-violation" build_layering
render_synthetic "06-rename-shown-as-move" build_move

echo "gallery regenerated in $out"
