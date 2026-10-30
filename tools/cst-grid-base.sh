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

grids=$(grep -rhoE 'check_no_worse_than_base\("[a-z_]+"' "$tests/all" |
  sed -E 's/.*\("([a-z_]+)"/\1/' | sort -u)
if [ -z "$grids" ]; then
  echo "no grid calls check_no_worse_than_base" >&2
  exit 1
fi

git -C "$repo" worktree add --detach --quiet "$scratch/base" "$rev"
rm -rf "$scratch/base/crates/cst/tests"
cp -R "$tests" "$scratch/base/crates/cst/tests"
mkdir "$scratch/out"

# Other tests in the group check this checkout's fixes and fail over REV's
# parser, so the run's exit status says nothing; each grid's base file must be
# written instead. Rewriting the grids' own manifests in the throwaway worktree
# keeps that check from stopping a grid before it writes its base.
(cd "$scratch/base" &&
  BORZOI_UPDATE_MANIFESTS=1 BORZOI_GRID_BASE_OUT="$scratch/out" \
    CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$repo/target/grid-base}" \
    nix develop -c cargo test -p borzoi-cst --test all parser_diff_recovery:: \
    >"$scratch/run.log" 2>&1) || true

for grid in $grids; do
  if [ ! -f "$scratch/out/$grid.base.txt" ]; then
    echo "grid $grid wrote no base file over $rev; the run's log:" >&2
    cat "$scratch/run.log" >&2
    exit 1
  fi
  {
    echo "# Base revision: $(git -C "$repo" rev-parse "$rev")"
    cat "$scratch/out/$grid.base.txt"
  } >"$manifests/$grid.base.txt"
  echo "wrote $manifests/$grid.base.txt"
done
