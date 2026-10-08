#!/usr/bin/env bash
# The whole-project resolution gate: run `borzoi-corpus-diff` over the pinned
# project corpus (`nix/project-corpus.json`) and check the run against the
# exact manifest checked in at crates/corpus-diff/manifests/project_corpus.txt.
# The runner's zero-divergence gate runs first, so the manifest cannot bless a
# wrong answer.
#
# CI's `corpus-diff` job runs this after `tools/ci/project-corpus.sh` has
# materialised the corpus, which exported BORZOI_PROJECT_LIST. Run locally
# without it, this materialises the corpus itself first — under
# ${BORZOI_PROJECT_CORPUS_CACHE:-~/.cache/borzoi/project-corpus}, outside the
# repository, so no `Directory.Build.props` of ours can reach the corpus's
# restores. Regenerate the manifest with
#
#   BORZOI_UPDATE_MANIFESTS=1 bash tools/ci/project-corpus-gate.sh
#
# Run it outside `nix develop`: it enters the devshell itself.
set -euo pipefail

repo="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$repo"

if [ -z "${BORZOI_PROJECT_LIST:-}" ]; then
  RUNNER_TEMP="${BORZOI_PROJECT_CORPUS_CACHE:-${XDG_CACHE_HOME:-$HOME/.cache}/borzoi/project-corpus}"
  export RUNNER_TEMP
  mkdir -p "$RUNNER_TEMP"
  export GITHUB_WORKSPACE="$repo"
  export GITHUB_ENV="$RUNNER_TEMP/corpus-env"
  : >"$GITHUB_ENV"
  bash tools/ci/project-corpus.sh
  set -a
  # shellcheck disable=SC1090
  . "$GITHUB_ENV"
  set +a
fi
: "${RUNNER_TEMP:?must be set alongside BORZOI_PROJECT_LIST}"

# Keys are relative to the checkouts' root, so the manifest names the same
# project and file wherever the corpus was materialised.
nix develop --command env \
  BORZOI_PROJECT_LIST="$BORZOI_PROJECT_LIST" \
  BORZOI_PROJECT_MANIFEST="$repo/crates/corpus-diff/manifests/project_corpus.txt" \
  BORZOI_PROJECT_MANIFEST_ROOT="$RUNNER_TEMP/project-corpus" \
  cargo run --locked -p borzoi-corpus-diff
