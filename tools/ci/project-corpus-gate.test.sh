#!/usr/bin/env bash
#
# Tests for project-corpus-gate.sh: CI's `corpus-diff` job runs exactly the
# script's steps, and concurrent local runs share the corpus cache safely.
#
# The concurrency cases run the real scripts against real Git, with `nix`,
# `cargo`, `dotnet` and `borzoi-stats` replaced by fakes: the "restore" writes
# its assets file in two halves a second apart, and each "gate step" keeps
# re-reading the checkout and that file while it runs, so a run that rewrites
# the corpus under another run's step is seen doing so.
#
# `bash tools/ci/project-corpus-gate.test.sh [ci-steps|concurrency]` runs one
# section; no argument runs both.

set -uo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
repo="$(cd "$here/../.." && pwd)"
fails=0

assert_eq() { # name expected actual
  if [[ "$2" == "$3" ]]; then
    printf 'ok   - %s\n' "$1"
  else
    printf 'FAIL - %s\n    expected: [%s]\n    actual:   [%s]\n' "$1" "$2" "$3"
    fails=$((fails + 1))
  fi
}

# --- the job's steps are the script's steps ---------------------------------

# Every `run:` of CI's `corpus-diff` job, one per line: the inline command, or
# for a `run: |` block its first line.
corpus_diff_job_runs() {
  awk '
    /^  corpus-diff:$/ { on = 1; next }
    on && /^  [A-Za-z0-9_-]+:$/ { exit }
    !on { next }
    block { sub(/^ +/, ""); print; block = 0; next }
    /^ +run: \|$/ { block = 1; next }
    /^ +run: / { sub(/^ +run: /, ""); print }
  ' "$repo/.github/workflows/ci.yml"
}

test_ci_steps() {
  local expected step
  expected="$(printf '%s\n' 'df -h / /mnt || true' 'bash tools/ci/project-corpus.sh')"
  while IFS= read -r step; do
    expected+=$'\n'"bash tools/ci/project-corpus-gate.sh $step"
  done < <(bash "$here/project-corpus-gate.sh" --list)
  # Exact, not a subset: a gate added to the job as its own `run:` would be
  # one the local script never runs, which is the drift this guards against.
  assert_eq "corpus-diff job runs the setup, then every gate step in order" \
    "$expected" "$(corpus_diff_job_runs)"
  assert_eq "the step list is not empty" "" \
    "$(bash "$here/project-corpus-gate.sh" --list | grep -q . || echo empty)"
  # Before any materialisation or step: the variables only keep a broken
  # check from reaching for a real corpus.
  assert_eq "an unknown step is refused by name" 1 \
    "$(BORZOI_PROJECT_LIST=/nonexistent RUNNER_TEMP=/nonexistent \
      bash "$here/project-corpus-gate.sh" no-such-step 2>&1 >/dev/null |
      grep -c "unknown step 'no-such-step'")"
}

# --- concurrent local runs ---------------------------------------------------

tmp=
setup_fixture() {
  tmp="$(mktemp -d)"
  local fake="$tmp/repo" bin="$tmp/bin"
  mkdir -p "$fake/tools/ci" "$fake/target/debug" "$bin" "$tmp/nops" "$tmp/slowps" "$tmp/remote/owner"
  cp "$here/project-corpus-gate.sh" "$here/project-corpus.sh" "$fake/tools/ci/"

  # One upstream repository with two pins, each holding the project.
  git init --quiet "$tmp/src"
  mkdir -p "$tmp/src/proj"
  for n in 1 2; do
    echo "<Project>$n</Project>" >"$tmp/src/proj/P.fsproj"
    git -C "$tmp/src" add -A
    git -C "$tmp/src" -c user.name=t -c user.email=t@t commit --quiet -m "pin $n"
  done
  rev2="$(git -C "$tmp/src" rev-parse HEAD)"
  rev1="$(git -C "$tmp/src" rev-parse HEAD~1)"
  git clone --quiet --bare "$tmp/src" "$tmp/remote/owner/repo"
  if ! git -C "$tmp/remote/owner/repo" cat-file -e "$rev1^{commit}" ||
    ! git -C "$tmp/remote/owner/repo" cat-file -e "$rev2^{commit}"; then
    echo "FAIL - the fixture's upstream repository has both pins" >&2
    exit 1
  fi
  printf 'owner/repo\t%s\tproj/P.fsproj\n' "$rev1" >"$tmp/plan1"
  printf 'owner/repo\t%s\tproj/P.fsproj\n' "$rev2" >"$tmp/plan2"

  cat >"$fake/target/debug/borzoi-stats" <<'EOF'
#!/usr/bin/env bash
cat "$FAKE_PLAN"
EOF
  cat >"$bin/nix" <<'EOF'
#!/usr/bin/env bash
[ "$1 $2" = "develop --command" ] || { echo "fake nix: unexpected $*" >&2; exit 99; }
shift 2
exec "$@"
EOF
  cat >"$bin/dotnet" <<'EOF'
#!/usr/bin/env bash
case "$1" in
  --version) echo 10.0.100-fake ;;
  restore)
    echo "restore $2" >>"$FAKE_LOG"
    if [ -n "${FAKE_RESTORE_FAILS:-}" ]; then
      # Holding the lock until the waiter is looking at it, if one is meant to.
      if [ -n "${FAKE_RESTORE_FAILS_AFTER:-}" ]; then
        for _ in $(seq 100); do [ -e "$FAKE_RESTORE_FAILS_AFTER" ] && break; sleep 0.1; done
      fi
      exit 1
    fi
    obj="$(dirname "$2")/obj"
    mkdir -p "$obj"
    printf 'partial' >"$obj/project.assets.json"
    sleep 1
    printf 'complete %s' "$(git -C "$(dirname "$2")" rev-parse HEAD)" >"$obj/project.assets.json"
    ;;
  *) echo "fake dotnet: unexpected $*" >&2; exit 99 ;;
esac
EOF
  # A gate step: it reads the corpus for a while, and fails if what it reads
  # is not the pin its run materialised.
  cat >"$bin/cargo" <<'EOF'
#!/usr/bin/env bash
case "$1" in
  build) exit 0 ;;
  run | test) ;;
  *) echo "fake cargo: unexpected $*" >&2; exit 99 ;;
esac
fail() { echo "step of run $FAKE_RUN: $*" >&2; exit 1; }
touch "$FAKE_DIR/started.$FAKE_RUN"
[ -n "${BORZOI_PROJECT_LIST:-}" ] || fail "no BORZOI_PROJECT_LIST"
lines="$(grep -c '^BORZOI_PROJECT_LIST=' "$GITHUB_ENV")"
[ "$lines" = 1 ] || fail "$GITHUB_ENV has $lines BORZOI_PROJECT_LIST lines"
for _ in 1 2 3 4 5 6; do
  IFS=: read -ra projects <<<"$BORZOI_PROJECT_LIST"
  for p in "${projects[@]}"; do
    head="$(git -C "$(dirname "$p")" rev-parse HEAD)"
    [ "$head" = "$FAKE_EXPECT_REV" ] || fail "checkout at $head"
    assets="$(cat "$(dirname "$p")/obj/project.assets.json")"
    [ "$assets" = "complete $FAKE_EXPECT_REV" ] || fail "assets read '$assets'"
  done
  sleep 0.5
done
echo "step $FAKE_RUN" >>"$FAKE_LOG"
EOF
  # A `ps` that reports nothing, for the run that cannot record its identity.
  printf '#!/usr/bin/env bash\nexit 1\n' >"$tmp/nops/ps"
  # A `ps` that, asked about FAKE_PS_SLOW_PID, says so and answers only once
  # that process has exited: the waiter reads the lock's owner, the owner
  # releases and exits, and only then does the waiter learn it is dead.
  cat >"$tmp/slowps/ps" <<'EOF'
#!/usr/bin/env bash
if [ "${*: -1}" = "$FAKE_PS_SLOW_PID" ]; then
  touch "$FAKE_PS_ENTERED"
  for _ in $(seq 100); do kill -0 "$FAKE_PS_SLOW_PID" 2>/dev/null || break; sleep 0.1; done
fi
PATH="${PATH#*:}" exec ps "$@"
EOF
  chmod +x "$fake/target/debug/borzoi-stats" "$bin"/* "$tmp/nops/ps" "$tmp/slowps/ps"
}

# run_gate NAME PLAN REV [STEP...]: one local gate run, in the background of
# nothing; its output goes to $tmp/NAME.out and its status to $tmp/NAME.status.
run_gate() {
  local name="$1" plan="$2" rev="$3"
  shift 3
  (
    env -u BORZOI_PROJECT_LIST -u RUNNER_TEMP -u GITHUB_ENV \
      PATH="${EXTRA_PATH:+$EXTRA_PATH:}$tmp/bin:$PATH" \
      TZ="${RUN_TZ:-UTC}" LC_ALL="${RUN_LC_ALL:-C}" \
      BORZOI_PROJECT_CORPUS_CACHE="$tmp/cache" \
      GIT_CONFIG_COUNT=1 \
      GIT_CONFIG_KEY_0="url.$tmp/remote/.insteadOf" \
      GIT_CONFIG_VALUE_0="https://github.com/" \
      FAKE_PLAN="$plan" FAKE_EXPECT_REV="$rev" FAKE_RUN="$name" \
      FAKE_LOG="$tmp/log" FAKE_RESTORE_FAILS="${FAKE_RESTORE_FAILS:-}" \
      FAKE_RESTORE_FAILS_AFTER="${FAKE_RESTORE_FAILS_AFTER:-}" \
      FAKE_PS_SLOW_PID="${FAKE_PS_SLOW_PID:-}" FAKE_PS_ENTERED="$tmp/ps-entered" \
      FAKE_DIR="$tmp" \
      bash "$tmp/repo/tools/ci/project-corpus-gate.sh" "$@" >"$tmp/$name.out" 2>&1
    echo $? >"$tmp/$name.status"
  ) 2>/dev/null
}

show_failure() { # name
  if [ "$(cat "$tmp/$1.status")" != 0 ]; then
    sed 's/^/    | /' "$tmp/$1.out"
  fi
}

count_log() { grep -c "^$1" "$tmp/log" 2>/dev/null || true; }

test_concurrency() {
  local step
  # The fixture's own commits and the runs' clones alike: no signing, no
  # rewriting of github.com URLs but the fixture's own.
  export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1
  step="$(bash "$here/project-corpus-gate.sh" --list | head -n1)"

  setup_fixture
  run_gate a "$tmp/plan1" "$rev1" "$step" &
  run_gate b "$tmp/plan1" "$rev1" "$step" &
  wait
  assert_eq "two concurrent runs of one pin: first succeeds" 0 "$(cat "$tmp/a.status")"
  show_failure a
  assert_eq "two concurrent runs of one pin: second succeeds" 0 "$(cat "$tmp/b.status")"
  show_failure b
  assert_eq "both runs' steps ran" 2 "$(count_log step)"
  assert_eq "no run left an env or plan file in the cache" "" \
    "$(cd "$tmp/cache" && ls -d corpus-env* project-corpus-plan* 2>/dev/null)"

  : >"$tmp/log"
  run_gate c "$tmp/plan1" "$rev1" "$step"
  assert_eq "a rerun of a materialised pin succeeds" 0 "$(cat "$tmp/c.status")"
  show_failure c
  assert_eq "a rerun of a materialised pin restores nothing" 0 "$(count_log restore)"

  # The two runs disagree on time zone, so neither may judge the other's
  # liveness by a time it renders in its own.
  : >"$tmp/log"
  RUN_TZ=Pacific/Kiritimati run_gate d "$tmp/plan1" "$rev1" "$step" &
  # Until d is reading: e must arrive at a corpus in use, not race d to it.
  until [ -n "$(ls "$tmp/cache/readers")" ] || [ -e "$tmp/d.status" ]; do sleep 0.1; done
  RUN_TZ=America/Adak run_gate e "$tmp/plan2" "$rev2" "$step" &
  wait
  assert_eq "a run of another pin beside a reader: reader succeeds" 0 "$(cat "$tmp/d.status")"
  show_failure d
  assert_eq "a run of another pin beside a reader: it succeeds too" 0 "$(cat "$tmp/e.status")"
  show_failure e
  assert_eq "the other pin was restored" 1 "$(count_log restore)"

  : >"$tmp/log"
  FAKE_RESTORE_FAILS=1 run_gate f "$tmp/plan1" "$rev1" "$step"
  assert_eq "a failed restore fails the run" 1 "$(cat "$tmp/f.status")"
  run_gate g "$tmp/plan1" "$rev1" "$step"
  assert_eq "the run after a failed restore restores again" 2 "$(count_log restore)"
  assert_eq "the run after a failed restore succeeds" 0 "$(cat "$tmp/g.status")"
  show_failure g

  # A reader registration left by a process that no longer exists. Its steps
  # may outlive it, so a run that would rewrite the corpus stops.
  local dead
  sh -c 'exit 0' &
  dead=$!
  wait "$dead"
  echo "$dead gone" >"$tmp/cache/readers/stale"
  run_gate h "$tmp/plan2" "$rev2" "$step"
  assert_eq "a dead reader stops a run that would rewrite the corpus" 1 "$(cat "$tmp/h.status")"
  assert_eq "the failure names the registration to remove" 1 \
    "$(grep -c "rm '$tmp/cache/readers/stale'" "$tmp/h.out")"
  rm "$tmp/cache/readers/stale"

  # An empty registration is a run that died registering, before any step.
  : >"$tmp/cache/readers/empty"
  run_gate h2 "$tmp/plan2" "$rev2" "$step"
  assert_eq "an empty registration does not stop materialisation" 0 "$(cat "$tmp/h2.status")"
  show_failure h2

  mkdir "$tmp/cache/lock"
  echo "$dead gone" >"$tmp/cache/lock/owner"
  run_gate i "$tmp/plan2" "$rev2" "$step"
  assert_eq "a lock held by a dead run fails fast" 1 "$(cat "$tmp/i.status")"
  assert_eq "the failure names the lock to remove" 1 \
    "$(grep -c "rm -r.*$tmp/cache/lock" "$tmp/i.out")"
  rm -r "$tmp/cache/lock"

  # A lock whose run died between taking it and recording itself.
  mkdir "$tmp/cache/lock"
  touch -t 202001010000 "$tmp/cache/lock"
  run_gate i2 "$tmp/plan2" "$rev2" "$step"
  assert_eq "a lock with no owner for over a minute fails fast" 1 "$(cat "$tmp/i2.status")"
  assert_eq "naming the lock to remove" 1 \
    "$(grep -c "no owner recorded" "$tmp/i2.out")"
  rm -r "$tmp/cache/lock"

  # Or that died with its owner file created but not yet written.
  mkdir "$tmp/cache/lock"
  : >"$tmp/cache/lock/owner"
  touch -t 202001010000 "$tmp/cache/lock"
  run_gate i3 "$tmp/plan2" "$rev2" "$step"
  assert_eq "a lock with an empty owner for over a minute fails fast" 1 "$(cat "$tmp/i3.status")"
  rm -r "$tmp/cache/lock"

  EXTRA_PATH="$tmp/nops" run_gate j "$tmp/plan2" "$rev2" "$step"
  assert_eq "a run that cannot read its own start time fails" 1 "$(cat "$tmp/j.status")"
  assert_eq "and registers nowhere" "" \
    "$(ls "$tmp/cache/readers"; ls -d "$tmp/cache/lock" 2>/dev/null)"

  # A waiter that finds the lock's owner dead only after the owner has
  # released it: the lock it read is gone, not abandoned.
  local holder
  : >"$tmp/log"
  FAKE_RESTORE_FAILS=1 FAKE_RESTORE_FAILS_AFTER="$tmp/ps-entered" \
    run_gate k "$tmp/plan1" "$rev1" "$step" &
  for _ in $(seq 100); do [ -s "$tmp/cache/lock/owner" ] && break; sleep 0.1; done
  holder="$(cut -d' ' -f1 "$tmp/cache/lock/owner")"
  EXTRA_PATH="$tmp/slowps" FAKE_PS_SLOW_PID="$holder" run_gate l "$tmp/plan1" "$rev1" "$step"
  wait
  assert_eq "the waiter did look at the holder as it exited" yes \
    "$([ -e "$tmp/ps-entered" ] && echo yes)"
  assert_eq "the run whose restore failed fails" 1 "$(cat "$tmp/k.status")"
  assert_eq "the run that waited on it takes the lock and succeeds" 0 "$(cat "$tmp/l.status")"
  show_failure l

  # A writer that finds a reader dead only after the reader has finished
  # normally: the registration it read is gone, not abandoned.
  local reader
  rm -f "$tmp/ps-entered"
  : >"$tmp/log"
  run_gate o "$tmp/plan2" "$rev2" "$step" &
  for _ in $(seq 100); do [ -e "$tmp/started.o" ] && break; sleep 0.1; done
  reader="$(cut -d' ' -f1 "$tmp/cache/readers"/*)"
  EXTRA_PATH="$tmp/slowps" FAKE_PS_SLOW_PID="$reader" run_gate p "$tmp/plan1" "$rev1" "$step"
  wait
  assert_eq "the writer did look at the reader as it exited" yes \
    "$([ -e "$tmp/ps-entered" ] && echo yes)"
  assert_eq "the reader that finished succeeds" 0 "$(cat "$tmp/o.status")"
  show_failure o
  assert_eq "the writer that waited on it succeeds" 0 "$(cat "$tmp/p.status")"
  show_failure p

  # A run killed outright while its step still reads: the step outlives the
  # registration's owner, and a run of another pin must not restore under it.
  : >"$tmp/log"
  run_gate m "$tmp/plan1" "$rev1" "$step" &
  for _ in $(seq 100); do [ -e "$tmp/started.m" ] && break; sleep 0.1; done
  kill -9 "$(cut -d' ' -f1 "$tmp/cache/readers"/*)"
  run_gate n "$tmp/plan2" "$rev2" "$step"
  for _ in $(seq 100); do [ "$(count_log step)" = 0 ] || break; sleep 0.1; done
  assert_eq "a killed run's surviving step reads its own pin to the end" 1 "$(count_log "step m")"
  assert_eq "the run of another pin beside it stops" 1 "$(cat "$tmp/n.status")"

  rm -rf "$tmp"
}

case "${1:-}" in
  "") test_ci_steps; test_concurrency ;;
  ci-steps) test_ci_steps ;;
  concurrency) test_concurrency ;;
  *) echo "unknown section: $1" >&2; exit 2 ;;
esac

if [[ $fails -gt 0 ]]; then
  printf '\n%d test(s) failed\n' "$fails"
  exit 1
fi
printf '\nall tests passed\n'
