#!/usr/bin/env bash
# Materialise the pinned project corpus and emit the `BORZOI_PROJECT_LIST` for
# it. Shared by the CI gate (`ci.yml`) and the continuous measurement
# (`stats.yml`) so the two cannot come to measure different corpora: a drifted
# copy of these steps would be invisible, because both would still pass.
#
# Writes to $RUNNER_TEMP:
#   project-corpus-plan.tsv   one `repository<TAB>revision<TAB>project` per line
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

plan="$RUNNER_TEMP/project-corpus-plan.tsv"

nix develop --command cargo build --locked -p borzoi-stats
"$GITHUB_WORKSPACE/target/debug/borzoi-stats" corpus \
  --pins nix/project-corpus.json --emit plan >"$plan"
cat "$plan"

while IFS=$'\t' read -r repository revision project; do
  dir="$RUNNER_TEMP/project-corpus/$repository"
  # The plan has one line per *project*, and a repository may legitimately
  # contribute several, so clone at most once per repository. `borzoi-stats
  # corpus` has already rejected a corpus that pins one repository at two
  # revisions, so a repeat visit wants the working tree that is already here.
  if [ ! -e "$dir" ]; then
    mkdir -p "$(dirname "$dir")"
    git clone --quiet --no-checkout "https://github.com/$repository" "$dir"
    # Detached at the exact pin: a branch would let the corpus drift under a
    # series that claims its points are comparable.
    git -C "$dir" checkout --quiet --detach "$revision"
  fi
  test -f "$dir/$project"
done <"$plan"

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
done <"$plan"

# The corpus is the pin file itself, passed as an explicit list, so no sampling
# happens: `BORZOI_PROJECT_STRIDE` and `_LIMIT` select only when walking a
# directory. Every pinned project is visited, which is what the callers'
# comparable-count assertions rely on.
list=
pinned=0
while IFS=$'\t' read -r repository _revision project; do
  list="${list:+$list:}$RUNNER_TEMP/project-corpus/$repository/$project"
  pinned=$((pinned + 1))
done <"$plan"

{
  echo "BORZOI_PROJECT_LIST=$list"
  echo "BORZOI_PROJECT_PINNED=$pinned"
} >>"$GITHUB_ENV"
