#!/usr/bin/env bash
# Git merge driver for the exact manifests (`borzoi_oracle_harness::manifest`):
# merges them as keyed line sets, so two branches that moved different items
# merge without regenerating, and fails when both moved the same item
# differently. `.gitattributes` names the keyed manifests it serves; enable it
# once per clone with
#
#   git config merge.borzoi-manifest.driver 'bash tools/manifest-merge.sh %O %A %B %P'
#
# Git runs it from the top of the worktree being merged into, so it builds the
# driver that worktree checks out. CI's gates stay the judge of the merged
# manifest: a clean merge here says only that no item moved two ways.
set -euo pipefail

cmd=(cargo run --quiet --release -p borzoi-oracle-harness --bin manifest-merge -- "$@")
if command -v cargo >/dev/null 2>&1; then
    exec "${cmd[@]}"
else
    exec nix develop --command "${cmd[@]}"
fi
