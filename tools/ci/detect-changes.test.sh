#!/usr/bin/env bash
#
# Tests for detect-changes.sh: the pure path-matching (`classify`) and the
# base-resolution / fail-open behaviour (against throwaway git repos).

set -uo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
script="$here/detect-changes.sh"
fails=0

assert_eq() { # name expected actual
  if [[ "$2" == "$3" ]]; then
    printf 'ok   - %s\n' "$1"
  else
    printf 'FAIL - %s\n    expected: [%s]\n    actual:   [%s]\n' "$1" "$2" "$3"
    fails=$((fails + 1))
  fi
}

# Expected result string for a given set of true filters, in the same
# sorted/space-joined shape the runners below produce.
expect() { # true-filters...
  local f out=""
  for f in assembly astgen cst fcs lsp msbuild nuget sema sidecar workspace; do
    if [[ " $* " == *" $f "* ]]; then out+="$f=true "; else out+="$f=false "; fi
  done
  printf '%s' "$out"
}

# Run `classify` over a newline-separated file list; normalise to sorted+joined.
classify() { printf '%s\n' "$1" | bash "$script" classify | sort | tr '\n' ' '; }

# --- classify: path matching -------------------------------------------------

assert_eq "single crate file"        "$(expect cst)"           "$(classify 'crates/cst/src/lexer/mod.rs')"
assert_eq "multiple crates + ignored" "$(expect cst lsp)"      "$(classify $'crates/cst/a.rs\ncrates/lsp/b.rs\nREADME.md')"
assert_eq "exact workspace file"     "$(expect workspace)"     "$(classify 'Cargo.toml')"
assert_eq "nix dir"                  "$(expect workspace)"     "$(classify 'nix/deps.json')"
assert_eq "ci workflow file"         "$(expect workspace)"     "$(classify '.github/workflows/ci.yml')"
assert_eq "this script (tools/ci)"   "$(expect workspace)"     "$(classify 'tools/ci/detect-changes.sh')"
assert_eq "astgen tool"              "$(expect astgen)"        "$(classify 'tools/astgen/src/lib.rs')"
assert_eq "fcs-dump tool"            "$(expect fcs)"           "$(classify 'tools/fcs-dump/Program.fs')"
assert_eq "sidecar prod"             "$(expect sidecar)"       "$(classify 'tools/csharp-sidecar/Foo.cs')"
assert_eq "sidecar tests"            "$(expect sidecar)"       "$(classify 'tools/csharp-sidecar.tests/Bar.cs')"
assert_eq "nuget crate"              "$(expect nuget)"         "$(classify 'crates/nuget/src/version.rs')"
assert_eq "nuget oracle tool"        "$(expect nuget)"         "$(classify 'tools/nuget-oracle/Program.fs')"
# The generated facade lives under crates/cst, so a hand-edit there is a `cst`
# change; the staleness gate (test-astgen) runs on `cst` too, catching it.
assert_eq "generated facade is cst"  "$(expect cst)"           "$(classify 'crates/cst/src/syntax/generated/union_types.rs')"
assert_eq "nothing changed"          "$(expect)"               "$(classify '')"
# Prefix-boundary guards: a sibling dir that shares a name prefix must NOT match.
assert_eq "crates/cstfoo is not cst" "$(expect)"               "$(classify 'crates/cstfoo/x.rs')"
assert_eq "tools/astgenfoo not astgen" "$(expect)"             "$(classify 'tools/astgenfoo/x.rs')"
assert_eq "csharp-sidecar-extra"     "$(expect)"               "$(classify 'tools/csharp-sidecar-extra/x.cs')"
assert_eq "nuget-oracle-extra"       "$(expect)"               "$(classify 'tools/nuget-oracle-extra/x.fs')"
# Crates/tools routed after the initial gate set.
assert_eq "sema crate"               "$(expect sema)"          "$(classify 'crates/sema/src/infer.rs')"
assert_eq "spawn is workspace"       "$(expect workspace)"     "$(classify 'crates/spawn/src/lib.rs')"
assert_eq "oracle-harness is workspace" "$(expect workspace)"  "$(classify 'crates/oracle-harness/src/lib.rs')"
assert_eq "msbuild-condition-oracle" "$(expect msbuild)"       "$(classify 'tools/msbuild-condition-oracle/Program.fs')"
assert_eq "crates/semafoo is not sema" "$(expect)"             "$(classify 'crates/semafoo/x.rs')"
assert_eq "spawnfoo is not spawn"    "$(expect)"               "$(classify 'crates/spawnfoo/x.rs')"
assert_eq "msbuild-cond-oracle-extra" "$(expect)"              "$(classify 'tools/msbuild-condition-oracle-extra/x.fs')"

# --- base resolution / fail-open (real git repos) ----------------------------

repo="$(mktemp -d)"
trap 'rm -rf "$repo" "${fp:-}"' EXIT
git init -q "$repo"
git -C "$repo" config user.email ci@test
git -C "$repo" config user.name ci
git -C "$repo" config commit.gpgsign false

mkdir -p "$repo/crates/cst"
echo a > "$repo/crates/cst/a.rs"
git -C "$repo" add -A
git -C "$repo" commit -q -m c1
base="$(git -C "$repo" rev-parse HEAD)"

mkdir -p "$repo/crates/lsp"
echo b > "$repo/crates/lsp/b.rs"
git -C "$repo" add -A
git -C "$repo" commit -q -m c2

# Run the default (CI) path inside the repo with a given environment; capture
# only the name=value lines (logs go to stderr), normalised to sorted+joined.
run() { # env-assignments...
  ( cd "$repo" && env -u GITHUB_OUTPUT "$@" bash "$script" ) 2>/dev/null | sort | tr '\n' ' '
}

assert_eq "push diff (before=c1)" "$(expect lsp)" \
  "$(run GITHUB_EVENT_NAME=push GITHUB_EVENT_BEFORE="$base")"
assert_eq "pull_request diff (base=c1)" "$(expect lsp)" \
  "$(run GITHUB_EVENT_NAME=pull_request GITHUB_PR_BASE_SHA="$base")"
assert_eq "fail-open: empty before" "$(expect cst msbuild assembly lsp sema nuget astgen fcs sidecar workspace)" \
  "$(run GITHUB_EVENT_NAME=push GITHUB_EVENT_BEFORE=)"
assert_eq "fail-open: all-zeros before" "$(expect cst msbuild assembly lsp sema nuget astgen fcs sidecar workspace)" \
  "$(run GITHUB_EVENT_NAME=push GITHUB_EVENT_BEFORE=0000000000000000000000000000000000000000)"
assert_eq "fail-open: unknown base sha" "$(expect cst msbuild assembly lsp sema nuget astgen fcs sidecar workspace)" \
  "$(run GITHUB_EVENT_NAME=push GITHUB_EVENT_BEFORE=deadbeefdeadbeefdeadbeefdeadbeefdeadbeef)"

# --- non-ancestor base (force-push) ------------------------------------------
# A force-push leaves `before` pointing at a tip that is not an ancestor of
# HEAD. The diff must compare the two tips directly (two-dot), not from their
# merge base (three-dot) — otherwise files that differ between the old and new
# tips are missed and their gated jobs wrongly skipped.
fp="$(mktemp -d)"
git init -q "$fp"
git -C "$fp" config user.email ci@test
git -C "$fp" config user.name ci
git -C "$fp" config commit.gpgsign false
mkdir -p "$fp/crates/lsp"
echo k > "$fp/crates/lsp/keep.rs"
git -C "$fp" add -A
git -C "$fp" commit -q -m mergebase
mergebase="$(git -C "$fp" rev-parse HEAD)"
mkdir -p "$fp/crates/cst"
echo o > "$fp/crates/cst/old.rs"
git -C "$fp" add -A
git -C "$fp" commit -q -m oldtip
oldtip="$(git -C "$fp" rev-parse HEAD)"
# Diverge from the merge base: HEAD becomes a tip that does NOT descend from
# oldtip (oldtip's crates/cst/old.rs is absent here; a crates/msbuild file is new).
git -C "$fp" checkout -q "$mergebase"
mkdir -p "$fp/crates/msbuild"
echo n > "$fp/crates/msbuild/new.rs"
git -C "$fp" add -A
git -C "$fp" commit -q -m newtip

fp_run() { ( cd "$fp" && env -u GITHUB_OUTPUT "$@" bash "$script" ) 2>/dev/null | sort | tr '\n' ' '; }
assert_eq "force-push: old tip..HEAD (cst removed + msbuild added)" \
  "$(expect cst msbuild)" \
  "$(fp_run GITHUB_EVENT_NAME=push GITHUB_EVENT_BEFORE="$oldtip")"

# --- $GITHUB_OUTPUT sink -----------------------------------------------------

out="$repo/gh_output"
: > "$out"
( cd "$repo" && env GITHUB_OUTPUT="$out" GITHUB_EVENT_NAME=push GITHUB_EVENT_BEFORE="$base" bash "$script" ) >/dev/null 2>&1
assert_eq "writes to GITHUB_OUTPUT" "$(expect lsp)" "$(sort "$out" | tr '\n' ' ')"

# --- coverage guard: every workspace crate must route to a filter ------------
# A crate reachable by no filter silently skips its tests on a crate-only change
# — the bug that had left sema/spawn/oracle-harness untested. Enumerate the real
# crates/* dirs and assert each maps to at least one filter, with an explicit
# exemption for crates whose tests are intentionally not run in CI.
repo_root="$(cd "$here/../.." && pwd)"
# corpus-diff: its whole-project name-resolution diffs need restored real
# projects (the manual resolve_real_project_diff flow); the crate is
# compile-checked by the lint job's `clippy --all-targets`, but has no test job.
crate_is_ci_exempt() { case "$1" in corpus-diff) return 0 ;; *) return 1 ;; esac; }
for cdir in "$repo_root"/crates/*/; do
  cname="$(basename "$cdir")"
  if [[ "$(classify "crates/$cname/__probe__.rs")" == *"=true"* ]]; then
    assert_eq "crate '$cname' routes to a filter" routed routed
  elif crate_is_ci_exempt "$cname"; then
    assert_eq "crate '$cname' is intentionally CI-exempt" exempt exempt
  else
    assert_eq "crate '$cname' routes to a filter" routed \
      "UNROUTED — add a filter prefix in detect-changes.sh (or exempt it here)"
  fi
done

echo
if [[ $fails -eq 0 ]]; then
  echo "all detect-changes tests passed"
else
  echo "$fails detect-changes test(s) failed"
  exit 1
fi
