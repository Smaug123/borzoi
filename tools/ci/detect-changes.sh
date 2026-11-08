#!/usr/bin/env bash
#
# CI change detection: decide which workspace areas a change touches, so the
# downstream jobs in `.github/workflows/ci.yml` can gate on the result. This
# replaces the dorny/paths-filter action with plain git — no external action,
# no un-retried network fetch, and an explicit failure mode we control.
#
# Emits `name=true|false` for every filter to `$GITHUB_OUTPUT` (or to stdout
# when that is unset). Change detection is an optimization, so it FAILS OPEN:
# when there is no diff base — an event other than the two below, a base
# variable left empty, or a fetch that left the base commit absent — every
# filter is reported `true` and the full suite runs. Wasted CI time is
# acceptable; silently skipping a job that should have run is not.
#
# Environment:
#   GITHUB_EVENT_NAME            "pull_request" or "merge_group"; any other
#                                value (or none) fails open
#   GITHUB_PR_BASE_SHA           base commit, for pull_request events
#   GITHUB_MERGE_GROUP_BASE_SHA  the commit the merge-queue entry is built on
#                                (main, or the entry ahead of it), for
#                                merge_group events
#   GITHUB_OUTPUT                file to append `name=value` lines to; stdout
#                                if unset
#
# Subcommand (for tests):
#   classify   read a newline-separated changed-file list on stdin and emit the
#              filter results, skipping git/base resolution entirely.

set -euo pipefail

# Each filter maps to one or more path prefixes. A changed file belongs to a
# filter when it equals a prefix or lies under it (prefix + "/"). These mirror
# the globs the dorny config used; keep them in lockstep with the per-job gates
# in ci.yml.
filter_prefixes() {
  case "$1" in
    cst)       echo "crates/cst" ;;
    # The condition/property oracle exists solely for crates/msbuild's
    # differential tests, so both live under one filter (mirrors nuget).
    msbuild)   echo "crates/msbuild tools/msbuild-condition-oracle" ;;
    assembly)  echo "crates/assembly" ;;
    # The inheritdoc oracle exists solely for crates/lsp's differential
    # tests, so both live under one filter (mirrors nuget). crates/lsp's
    # restore-graph fixtures grade crates/nuget's dgspec resolver against the
    # nuget oracle, so both of those are in this filter as well as nuget's.
    lsp)       echo "crates/lsp tools/inheritdoc-oracle crates/nuget tools/nuget-oracle" ;;
    sema)      echo "crates/sema" ;;
    # The oracle tool exists solely for crates/nuget's differential tests,
    # so both live under one filter.
    nuget)     echo "crates/nuget tools/nuget-oracle" ;;
    astgen)    echo "tools/astgen" ;;
    stats)     echo "tools/stats" ;;
    fcs)       echo "tools/fcs-dump" ;;
    sidecar)   echo "tools/csharp-sidecar tools/csharp-sidecar.tests" ;;
    # crates/spawn (every test harness shells out through it) and
    # crates/oracle-harness (drives every resident oracle) underpin the whole
    # suite, so — like the build-config entries — a change to either runs
    # everything. Each still has its own test job, gated on `workspace`.
    workspace) echo "Cargo.toml Cargo.lock flake.nix flake.lock nix rust-toolchain.toml .github/workflows tools/ci crates/spawn crates/oracle-harness" ;;
    *)         echo "filter_prefixes: unknown filter '$1'" >&2; exit 2 ;;
  esac
}

FILTERS="cst msbuild assembly lsp sema nuget astgen stats fcs sidecar workspace"

# Read a changed-file list on stdin; print "name=true|false" per filter.
classify() {
  local changed name prefix line matched
  changed="$(cat)"
  for name in $FILTERS; do
    matched=false
    while IFS= read -r line; do
      [[ -n "$line" ]] || continue
      for prefix in $(filter_prefixes "$name"); do
        if [[ "$line" == "$prefix" || "$line" == "$prefix"/* ]]; then
          matched=true
          break
        fi
      done
      if [[ "$matched" == true ]]; then break; fi
    done <<<"$changed"
    printf '%s=%s\n' "$name" "$matched"
  done
}

all_true() {
  local name
  for name in $FILTERS; do printf '%s=true\n' "$name"; done
}

# Print the changed-file list for this event on stdout, or return non-zero to
# signal "fail open" (base unresolved). A two-dot `git diff <base> HEAD`
# compares the two tips directly: correct for PR merge refs and merge-queue
# commits, and safe should the base ever not be an ancestor of HEAD, where it
# can only over-report, never miss a changed path. A three-dot diff would
# instead start from the merge base and silently drop files that differ
# between the two tips. `--no-renames` so a file moved between crates flags
# both its source and destination areas.
changed_files() {
  local base
  case "${GITHUB_EVENT_NAME:-}" in
    pull_request) base="${GITHUB_PR_BASE_SHA:-}" ;;
    merge_group)  base="${GITHUB_MERGE_GROUP_BASE_SHA:-}" ;;
    *)
      echo "detect-changes: no diff base for event '${GITHUB_EVENT_NAME:-}'; failing open." >&2
      return 1
      ;;
  esac
  if [[ -z "$base" || "$base" =~ ^0+$ ]] || ! git cat-file -e "${base}^{commit}" 2>/dev/null; then
    echo "detect-changes: diff base unavailable ('${base}'); failing open." >&2
    return 1
  fi
  git diff --name-only --no-renames "$base" HEAD
}

write_results() {
  if [[ -n "${GITHUB_OUTPUT:-}" ]]; then
    cat >> "$GITHUB_OUTPUT"
  else
    cat
  fi
}

main() {
  local changed status
  set +e
  changed="$(changed_files)"
  status=$?
  set -e
  if [[ $status -eq 0 ]]; then
    echo "detect-changes: changed files since base:" >&2
    printf '%s\n' "${changed:-  (none)}" >&2
    classify <<<"$changed"
  else
    all_true
  fi
}

case "${1:-}" in
  classify) classify ;;
  "")       main | write_results ;;
  *)        echo "usage: $0 [classify]" >&2; exit 2 ;;
esac
