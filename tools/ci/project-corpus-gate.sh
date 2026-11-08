#!/usr/bin/env bash
# The whole-project gate: every check `ci.yml`'s `corpus-diff` job runs over
# the pinned project corpus (`nix/project-corpus.json`). This file is the one
# list of them. CI runs each as `project-corpus-gate.sh STEP`, one Actions step
# per gate, and `project-corpus-gate.test.sh` fails if the job's steps and
# `--list` differ.
#
#   project-corpus-gate.sh            every step, in order
#   project-corpus-gate.sh STEP...    just those
#   project-corpus-gate.sh --list     the step names, one per line
#
# Run with every step, the script keeps going after a failure and names the
# failed steps at the end, so one run reports (or, under
# BORZOI_UPDATE_MANIFESTS=1, regenerates) every manifest the steps check:
#
#   BORZOI_UPDATE_MANIFESTS=1 bash tools/ci/project-corpus-gate.sh
#
# In CI, `tools/ci/project-corpus.sh` has already materialised the corpus and
# exported BORZOI_PROJECT_LIST. Run locally without it, this materialises the
# corpus itself first, under
# ${BORZOI_PROJECT_CORPUS_CACHE:-~/.cache/borzoi/project-corpus}/v2, outside the
# repository, so no `Directory.Build.props` of ours can reach the corpus's
# restores.
#
# Several local runs may share that cache at once (agents in sibling
# worktrees). The checkouts and the packages folder are shared; each run's plan
# and env files are its own. A lock serialises the phase that writes the
# corpus, and every run is registered as a reader of the corpus from the
# moment its materialisation is settled until it exits, so no run checks out
# or restores under another run's steps. A run whose plan, SDK and
# materialisation script match the cache's `materialised` stamp skips the
# writing phase and reads straight away. A run killed outright (SIGKILL)
# leaves its lock or its reader registration behind, and the next run that
# needs either gone fails naming it rather than guessing.
#
# Run it outside `nix develop`: it enters the devshell itself.
set -euo pipefail

repo="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$repo"

steps=(resolution corpus-diff-tests assembly-projection restore-graph restore-cone xml-doc-source)

# Each step is a single command, so its status is the step's.
run_step() { # name
  case "$1" in
    resolution)
      # Two gates, in order. Zero divergences in every direction is a hard
      # assertion: both sides read one reference set, so neither can resolve a
      # name in an assembly the other cannot see. Then the run must reproduce
      # `crates/corpus-diff/manifests/project_corpus.txt` exactly — every
      # project's verdict, every file's match counts, every graded record that
      # is not a match — so a change that makes the LSP answer less, or moves
      # a project to "skipped", fails here even though it diverges nowhere.
      # The exit code *is* the gate, which is the whole difference from
      # `stats.yml`'s job over the same corpus: that one records the numbers
      # into the series and deliberately ignores this status.
      #
      # Keys are relative to the checkouts' root, so the manifest names the
      # same project and file wherever the corpus was materialised.
      nix develop --command env \
        BORZOI_PROJECT_LIST="$BORZOI_PROJECT_LIST" \
        BORZOI_PROJECT_MANIFEST="$repo/crates/corpus-diff/manifests/project_corpus.txt" \
        BORZOI_PROJECT_MANIFEST_ROOT="$RUNNER_TEMP/project-corpus" \
        cargo run --locked --profile gate -p borzoi-corpus-diff
      ;;
    corpus-diff-tests)
      # No other job tests this crate. Its FCS-driven tests are the runner's
      # own fixtures: small generated projects shaped to break its grading
      # rather than the server (sibling projects sharing a file, a byte-order
      # mark on line 1, the attribute and qualifier matrices). They are
      # `#[ignore]`d only because they drive FCS, so every ignored test runs
      # except the two that take a project from the environment:
      # `project_corpus_resolution_diff` is the `resolution` step in a debug
      # build (and would read BORZOI_PROJECT_LIST), and
      # `explain_token_at_position` is a hand-run trace.
      nix develop --command cargo test -p borzoi-corpus-diff -- \
        --include-ignored \
        --skip project_corpus_resolution_diff \
        --skip explain_token_at_position
      ;;
    assembly-projection)
      # This sweep wants a populated package cache rather than a fixture, and
      # the corpus's restores have just filled exactly one.
      #
      # A whole-assembly skip is the grade of failure that invalidates
      # readings into *other* assemblies too (the missing one could declare a
      # colliding type or an assembly-level `[<AutoOpen>]`), so any loss
      # without a named exemption fails. Losses are identified by the file's
      # bytes, never its path, so the cache layout cannot perturb a verdict.
      #
      # Measured 2026-07-31 against CI's restore: 1124 assemblies found, 1118
      # projected, 6 exempt (native PE), 0 dropped types. The sweep's own
      # `MIN_PROJECTED = 1_000` anti-vacuity floor exists to catch a mistyped
      # root, and this population clears it by 118 — thin enough that a corpus
      # bump which drops a few dependencies could fail this step for a reason
      # that has nothing to do with projection. If it goes red with
      # "population too small" after a `nix/project-corpus.json` change, that
      # is what happened; re-measure rather than assuming a projection defect.
      # A local packages folder may hold more than CI's, from earlier pins.
      nix develop --command env \
        BORZOI_DLL_SWEEP_ROOT="$RUNNER_TEMP/project-corpus-packages" \
        cargo test -p borzoi-assembly --test all projection_skip_sweep:: \
        -- --ignored --nocapture
      ;;
    restore-graph)
      # Stages 2 to 4 of docs/nuget-dgspec-boundary-plan.md over the same
      # projects, restored offline against the packages folder their
      # materialising restores filled: the dependency-graph spec generated
      # without restoring must be exactly the one restore itself reads, NuGet
      # handed that spec must compute exactly the assets file restore wrote,
      # B₁ must decline or compute that file's package closures, and the
      # outcome (B₁'s declines by field among it) is pinned in
      # `restore_graph_projects.txt`.
      nix develop --command env \
        BORZOI_PROJECT_LIST="$BORZOI_PROJECT_LIST" \
        BORZOI_PROJECT_PACKAGES="$RUNNER_TEMP/project-corpus-packages" \
        cargo test -p borzoi --test all restore_graph::pinned_projects \
        -- --ignored --nocapture
      ;;
    restore-cone)
      # The restore-cone census of docs/nuget-dgspec-boundary-plan.md (stage
      # 8) over the same projects: every target and task
      # `GenerateRestoreGraphFile` executes under restore's globals, under
      # every SDK the devshell installs, pinned in `restore_cone_projects.txt`
      # with each target file's hash and each task assembly's identity.
      nix develop --command env \
        BORZOI_PROJECT_LIST="$BORZOI_PROJECT_LIST" \
        cargo test -p borzoi-msbuild --test restore_cone_census pinned_projects \
        -- --ignored --nocapture
      ;;
    xml-doc-source)
      # Hover's project-local `///` docs over the same restored projects,
      # loaded through the LSP's runtime path (Compile order, reference
      # closure, signature-aware fold) with FCS reading exactly the same
      # references: where hover would show a doc, FCS attaches exactly it,
      # and the outcome is pinned in `xml_doc_source_projects.txt`.
      nix develop --command env \
        BORZOI_PROJECT_LIST="$BORZOI_PROJECT_LIST" \
        cargo test -p borzoi --test all xml_doc_source_diff::project::pinned \
        -- --ignored --nocapture
      ;;
    *)
      echo "project-corpus-gate: run_step has no case for listed step $1" >&2
      return 1
      ;;
  esac
}

if [ "${1:-}" = --list ]; then
  [ $# -eq 1 ] || { echo "usage: $0 --list" >&2; exit 2; }
  printf '%s\n' "${steps[@]}"
  exit 0
fi

selected=("$@")
[ $# -gt 0 ] || selected=("${steps[@]}")
for want in "${selected[@]}"; do
  case " ${steps[*]} " in
    *" $want "*) ;;
    *)
      echo "project-corpus-gate: unknown step '$want'; the steps are: ${steps[*]}" >&2
      exit 1
      ;;
  esac
done

# --- local materialisation ---------------------------------------------------

# A process, as `pid start-time`: the start time tells a live owner from an
# unrelated process that has since been given the same pid. Rendered in a
# fixed zone and locale, so every run spells one process the same way. Fails
# for a process `ps` cannot report, so no identity without a start time is
# ever recorded: one would match a dead process forever.
identity() { # pid
  local start
  start="$(TZ=UTC0 LC_ALL=C ps -o lstart= -p "$1" 2>/dev/null)" || return 1
  [ -n "$start" ] || return 1
  printf '%s %s' "$1" "$start"
}
alive() { # identity
  local now
  now="$(identity "${1%% *}")" || return 1
  [ "$now" = "$1" ]
}

# Whether FILE (a lock's owner, or a reader registration) records a process
# that has exited; if so, prints that identity. Every run removes its own file
# before it exits, so an owner seen dead may have released the file (and,
# for the lock, another run taken it) since it was read: abandoned means the
# file still records that same dead owner afterwards.
abandoned() { # file
  local owner
  owner="$(cat "$1" 2>/dev/null || true)"
  [ -n "$owner" ] || return 1
  ! alive "$owner" || return 1
  [ "$(cat "$1" 2>/dev/null || true)" = "$owner" ] || return 1
  printf '%s' "$owner"
}

lock_held=
reader=
run_env=
plan=
wanted=
cleanup() {
  # Forget the lock before removing it: a signal between the two then leaks a
  # lock (which the next run reports) rather than removing one another run
  # has since taken.
  if [ -n "$lock_held" ]; then
    lock_held=
    rm -f "$lock"
  fi
  local f
  for f in "$reader" "$run_env" "$plan" "$wanted"; do
    [ -z "$f" ] || rm -f "$f"
  done
}

# The lock is a file created with O_EXCL (bash's noclobber `>`), so the kernel
# admits one creator whichever coreutils the host has. A `mkdir` lock would
# rest on `mkdir` failing for an existing directory, and uutils' `mkdir` before
# 0.11 (Ubuntu 26.04's coreutils) succeeds there instead. The owner goes in
# with the create, in one write: a lock seen empty is one being taken.
take_lock() { (set -C && printf '%s\n' "$me" >"$lock") 2>/dev/null; }

acquire_lock() {
  local owner said=
  until take_lock; do
    # No owner yet is a run between its create and recording itself; for
    # longer than a minute, it is one that died there.
    if owner="$(abandoned "$lock")" ||
      { [ ! -s "$lock" ] && [ -n "$(find "$lock" -maxdepth 0 -mmin +1 2>/dev/null)" ]; }; then
      echo "project-corpus-gate: $lock is held by a run that has exited" \
        "(${owner:-no owner recorded}). If no gate run is in progress, remove it:" >&2
      echo "  rm -r '$lock'" >&2
      exit 1
    fi
    owner="$(cat "$lock" 2>/dev/null || true)"
    if [ -z "$said" ]; then
      echo "project-corpus-gate: waiting for $lock (held by pid ${owner%% *})" >&2
      said=1
    fi
    sleep 1
  done
  lock_held=1
}

release_lock() {
  lock_held=
  rm -f "$lock"
}

# Called holding the lock, which is the only time a reader registers, so an
# empty registration is one whose run died writing it, before any step began.
# A run that died after registering is another matter: its steps are its
# children, and a run killed outright leaves them reading the corpus (and,
# under BORZOI_UPDATE_MANIFESTS=1, writing manifests from it). Nothing here
# can tell whether they have finished, so a writer stops rather than guess.
wait_for_readers() {
  local r owner live said=
  while :; do
    live=
    for r in "$readers"/*; do
      [ -e "$r" ] || continue
      owner="$(cat "$r" 2>/dev/null || true)"
      if [ -z "$owner" ]; then
        rm -f "$r"
      elif owner="$(abandoned "$r")"; then
        echo "project-corpus-gate: $r registers a run that has exited ($owner)," \
          "and its steps may still be reading the corpus. Once none is, remove it:" >&2
        echo "  rm '$r'" >&2
        exit 1
      else
        # Alive, or it has just finished: either way, look again.
        live+=" ${owner%% *}"
      fi
    done
    [ -n "$live" ] || return 0
    if [ -z "$said" ]; then
      echo "project-corpus-gate: waiting for the runs reading the corpus to finish (pids$live)" >&2
      said=1
    fi
    sleep 1
  done
}

if [ -z "${BORZOI_PROJECT_LIST:-}" ]; then
  # Keyed by the lock protocol: runs that take the lock differently each
  # misread the other's lock, so they must not share a cache. This file lock's
  # cache is `v2`; a gate script that locks with `mkdir` uses the root itself,
  # which can be deleted once no worktree has such a script.
  cache="${BORZOI_PROJECT_CORPUS_CACHE:-${XDG_CACHE_HOME:-$HOME/.cache}/borzoi/project-corpus}/v2"
  lock="$cache/lock"
  readers="$cache/readers"
  stamp="$cache/materialised"
  if ! me="$(identity $$)"; then
    echo "project-corpus-gate: \`ps -o lstart= -p $$\` reports no start time, so" \
      "this run cannot register in $cache in a form other runs can tell is dead" >&2
    exit 1
  fi
  mkdir -p "$readers"
  trap cleanup EXIT
  trap 'exit 129' HUP
  trap 'exit 130' INT
  trap 'exit 143' TERM
  run_env="$(mktemp "$cache/corpus-env.XXXXXX")"
  plan="$(mktemp "$cache/project-corpus-plan.XXXXXX")"
  wanted="$(mktemp "$cache/materialised-wanted.XXXXXX")"

  RUNNER_TEMP="$cache"
  GITHUB_WORKSPACE="$repo"
  GITHUB_ENV="$run_env"
  export RUNNER_TEMP GITHUB_WORKSPACE GITHUB_ENV

  bash tools/ci/project-corpus.sh plan "$plan"
  # What a materialisation is a function of, beyond the pins: the SDK that
  # restores, and how this branch's script checks out and restores.
  {
    cat "$plan"
    echo "sdk $(nix develop --command dotnet --version)"
    cat tools/ci/project-corpus.sh
  } >"$wanted"

  acquire_lock
  if ! cmp -s "$wanted" "$stamp"; then
    wait_for_readers
    # Gone before the first write, so a materialisation that fails part-way
    # leaves no stamp and the next run redoes it.
    rm -f "$stamp"
    bash tools/ci/project-corpus.sh materialise "$plan"
    cp "$wanted" "$stamp"
  fi
  reader="$(mktemp "$readers/XXXXXX")"
  printf '%s\n' "$me" >"$reader"
  release_lock

  bash tools/ci/project-corpus.sh env "$plan"
  # Read as data, not sourced: the values are paths, and a cache directory
  # with a space in its name must not turn into a command.
  seen=" "
  while IFS='=' read -r key value; do
    case "$key" in
      BORZOI_PROJECT_LIST | BORZOI_PROJECT_PINNED)
        case "$seen" in
          *" $key "*)
            echo "project-corpus-gate: $run_env sets $key twice" >&2
            exit 1
            ;;
        esac
        seen+="$key "
        export "$key=$value"
        ;;
    esac
  done <"$run_env"
fi
: "${BORZOI_PROJECT_LIST:?project-corpus.sh emitted no BORZOI_PROJECT_LIST}"
: "${RUNNER_TEMP:?must be set alongside BORZOI_PROJECT_LIST}"

# --- the steps ---------------------------------------------------------------

failed=()
for step in "${selected[@]}"; do
  echo "==> project-corpus-gate: $step" >&2
  run_step "$step" || failed+=("$step")
done
if [ ${#failed[@]} -gt 0 ]; then
  echo "project-corpus-gate: failed: ${failed[*]}" >&2
  exit 1
fi
