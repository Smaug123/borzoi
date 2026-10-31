#!/usr/bin/env bash
# Regenerate the base files of the cst recovery grids
# (crates/cst/tests/manifests/<grid>.base.txt): the cells where the parser at
# REV (default origin/main) diverges from FCS. Each grid asserts that the
# parser under test diverges only on those cells
# (`corpus_manifest::check_no_worse_than_base`).
#
# The grids run from this checkout's test code over REV's parser, so a grid
# added or widened here is graded on both parsers alike.
#
#   bash tools/cst-grid-base.sh [REV]
set -euo pipefail

rev="${1:-origin/main}"
repo="$(git rev-parse --show-toplevel)"
tests="$repo/crates/cst/tests"
manifests="$tests/manifests"
scratch="$(mktemp -d)"
trap 'git -C "$repo" worktree remove --force "$scratch/base" >/dev/null 2>&1 || true; rm -rf "$scratch"' EXIT

git -C "$repo" worktree add --detach --quiet "$scratch/base" "$rev"
rm -rf "$scratch/base/crates/cst/tests"
cp -R "$tests" "$scratch/base/crates/cst/tests"
mkdir "$scratch/out"

# Other tests in the group check this checkout's fixes and fail over REV's
# parser, so the run's exit status says nothing. Each grid that ran writes its
# base file; every grid with a checked-in base must be among them, so a grid
# that stopped before writing one fails here rather than keeping a stale base.
# Rewriting the grids' own manifests in the throwaway worktree keeps that
# check from stopping a grid before it writes its base.
(cd "$scratch/base" &&
  BORZOI_UPDATE_MANIFESTS=1 BORZOI_GRID_BASE_OUT="$scratch/out" \
    CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$repo/target/grid-base}" \
    nix develop -c cargo test -p borzoi-cst --test all parser_diff_recovery:: \
    >"$scratch/run.log" 2>&1) || true

for checked_in in "$manifests"/*.base.txt; do
  [ -e "$checked_in" ] || continue
  if [ ! -f "$scratch/out/$(basename "$checked_in")" ]; then
    echo "$(basename "$checked_in" .base.txt) wrote no base file over $rev; the run's log:" >&2
    cat "$scratch/run.log" >&2
    exit 1
  fi
done
written=0
for out in "$scratch/out"/*.base.txt; do
  [ -e "$out" ] || continue
  {
    echo "# Base revision: $(git -C "$repo" rev-parse "$rev")"
    cat "$out"
  } >"$manifests/$(basename "$out")"
  echo "wrote $manifests/$(basename "$out")"
  written=$((written + 1))
done
if [ "$written" -eq 0 ]; then
  echo "no grid wrote a base file over $rev; the run's log:" >&2
  cat "$scratch/run.log" >&2
  exit 1
fi
