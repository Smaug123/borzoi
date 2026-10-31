#!/usr/bin/env bash
# Materialise the pinned project corpus and emit the `BORZOI_PROJECT_LIST` for
# it. Shared by the CI gate (`ci.yml`), the continuous measurement
# (`stats.yml`) and the local gate (`project-corpus-gate.sh`) so they cannot
# come to measure different corpora: a drifted copy of these steps would be
# invisible, because both would still pass.
#
#   project-corpus.sh                    all three phases below, in order
#   project-corpus.sh plan PLAN          write the plan to PLAN: one
#                                        `repository<TAB>revision<TAB>project`
#                                        per line
#   project-corpus.sh materialise PLAN   check out and restore every project
#   project-corpus.sh env PLAN           check the checkouts are at their pins,
#                                        and append the variables below
#
# The phases are separate so the local gate can lock around the one that
# writes the shared corpus (`materialise`) and skip it when the corpus is
# already what PLAN asks for; CI, on a fresh runner, runs them all.
#
# Writes to $RUNNER_TEMP:
#   project-corpus/           the checkouts, detached at their pins
#   project-corpus-packages/  the NuGet packages folder for their restores
#
# Appends to $GITHUB_ENV:
#   BORZOI_PROJECT_LIST       colon-separated absolute .fsproj paths
#   BORZOI_PROJECT_PINNED     how many projects the pin file names
set -euo pipefail

: "${RUNNER_TEMP:?must be set}"
: "${GITHUB_WORKSPACE:?must be set}"
: "${GITHUB_ENV:?must be set}"

plan() { # out
  nix develop --command cargo build --locked -p borzoi-stats
  "$GITHUB_WORKSPACE/target/debug/borzoi-stats" corpus \
    --pins nix/project-corpus.json --emit plan >"$1"
  cat "$1"
}

materialise() { # plan
  while IFS=$'\t' read -r repository revision project; do
    dir="$RUNNER_TEMP/project-corpus/$repository"
    # The plan has one line per *project*, and a repository may legitimately
    # contribute several, so clone at most once per repository. `borzoi-stats
    # corpus` has already rejected a corpus that pins one repository at two
    # revisions, so a repeat visit wants the working tree that is already here.
    if [ ! -e "$dir" ]; then
      mkdir -p "$(dirname "$dir")"
      git clone --quiet --no-checkout "https://github.com/$repository" "$dir"
    elif ! git -C "$dir" cat-file -e "$revision^{commit}" 2>/dev/null; then
      # A checkout reused from an earlier run (a local cache, see
      # `project-corpus-gate.sh`) may predate the pin.
      git -C "$dir" fetch --quiet origin
    fi
    # Detached at the exact pin, every time: a branch would let the corpus drift
    # under a series that claims its points are comparable, and a reused checkout
    # may sit at an older pin.
    git -C "$dir" checkout --quiet --detach "$revision"
    test "$(git -C "$dir" rev-parse HEAD)" = "$revision"
    test -f "$dir/$project"
  done <"$1"

  # These restores must reach nuget.org. The devShell pins NuGet at the repo's own
  # offline closure, which by design holds only what Borzoi's own builds need, not
  # what these unrelated projects reference. The packages folder is separate from
  # that closure so a corpus package cannot shadow one the workspace's own tests
  # restore.
  while IFS=$'\t' read -r repository _revision project; do
    nix develop --command env \
      RestoreSources=https://api.nuget.org/v3/index.json \
      NUGET_PACKAGES="$RUNNER_TEMP/project-corpus-packages" \
      dotnet restore "$RUNNER_TEMP/project-corpus/$repository/$project"
  done <"$1"
}

emit_env() { # plan
  # The corpus is the pin file itself, passed as an explicit list, so no sampling
  # happens: `BORZOI_PROJECT_STRIDE` and `_LIMIT` select only when walking a
  # directory. Every pinned project is visited, which is what the callers'
  # comparable-count assertions rely on.
  list=
  pinned=0
  while IFS=$'\t' read -r repository revision project; do
    dir="$RUNNER_TEMP/project-corpus/$repository"
    # Read-only, so it is safe beside another run's gate steps: a corpus this
    # phase is asked to describe must be the one PLAN pins.
    test "$(git -C "$dir" rev-parse HEAD)" = "$revision"
    test -f "$dir/$project"
    list="${list:+$list:}$dir/$project"
    pinned=$((pinned + 1))
  done <"$1"

  {
    echo "BORZOI_PROJECT_LIST=$list"
    echo "BORZOI_PROJECT_PINNED=$pinned"
  } >>"$GITHUB_ENV"
}

case "${1:-}" in
  "")
    [ $# -eq 0 ] || { echo "usage: $0 [plan|materialise|env PLAN]" >&2; exit 2; }
    planfile="$(mktemp "$RUNNER_TEMP/project-corpus-plan.XXXXXX")"
    trap 'rm -f "$planfile"' EXIT
    plan "$planfile"
    materialise "$planfile"
    emit_env "$planfile"
    ;;
  plan | materialise | env)
    [ $# -eq 2 ] || { echo "usage: $0 $1 PLAN" >&2; exit 2; }
    case "$1" in
      plan) plan "$2" ;;
      materialise) materialise "$2" ;;
      env) emit_env "$2" ;;
    esac
    ;;
  *) echo "usage: $0 [plan|materialise|env PLAN]" >&2; exit 2 ;;
esac
