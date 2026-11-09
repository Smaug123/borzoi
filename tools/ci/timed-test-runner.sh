#!/usr/bin/env bash
# Cargo `runner` for CI's test binaries: runs each with libtest's per-test
# timing (`--report-time`) and appends one TSV row per finished test to
# `$BORZOI_TEST_TIMES` (default `$RUNNER_TEMP/test-times.tsv`), which
# `slowest-tests.sh` turns into the job summary.
#
# `ci.yml` installs it for every `cargo test` in the workflow through
# `CARGO_TARGET_<triple>_RUNNER`, so no step has to remember a flag. Cargo
# invokes it as `timed-test-runner.sh <binary> <args>…`; it is never used for
# build scripts or doc-tests.
#
# Cargo uses a runner for `cargo run` too (`project-corpus-gate.sh` runs the
# corpus-diff binary), whose arguments are not libtest's. Cargo leaves a test
# executable in `target/<profile>/deps/` and uplifts a `cargo run` binary to
# `target/<profile>/`, so anything outside `deps/` runs untouched.
#
# `--report-time` is an unstable libtest option, which a stable libtest
# accepts only when `RUSTC_BOOTSTRAP` is set *in the test process*. It is set
# here, at run time, rather than on `cargo`: build scripts may read it, and a
# changed build-script input would rebuild the cached `target/`.
#
# Row format: `<seconds>\t<package>/<binary>\t<test>\t<outcome>`.
#
# Per binary run it also appends a row to `<times>-binaries.tsv`: its wall
# seconds and the machine's average busy cores over that time, from
# `/proc/stat`. A suite whose binary keeps every core busy gains nothing from
# running its tests more concurrently, and per-test wall times alone cannot
# say whether it does. Row format:
# `<seconds>\t<package>/<binary>\t<busy cores>\t<cores>\t<libtest args>`.
set -uo pipefail

bin=$1
shift

if [[ "$(basename "$(dirname "$bin")")" != deps ]]; then
  exec "$bin" "$@"
fi

out=${BORZOI_TEST_TIMES:-${RUNNER_TEMP:-/tmp}/test-times.tsv}
log=$(mktemp)

# Busy and total jiffies summed over every core; nothing where there is no
# `/proc/stat` (macOS), which leaves the binaries table without that run.
cpu_jiffies() {
  awk '/^cpu / { busy = $2 + $3 + $4 + $7 + $8 + $9; print busy, busy + $5 + $6; exit }' \
    /proc/stat 2>/dev/null || true
}

cpu_before=$(cpu_jiffies)
started=$(date +%s)
RUSTC_BOOTSTRAP=1 "$bin" "$@" -Zunstable-options --report-time | tee "$log"
status=${PIPESTATUS[0]}
cpu_after=$(cpu_jiffies)
elapsed=$(($(date +%s) - started))

target=$(basename "$bin")
target=${target%-*} # strip cargo's `-<hash>` suffix
# libtest prints `test <name> ... <outcome> <secs>` in one line when tests run
# concurrently. Run serially (one test selected, or `--test-threads=1`) it
# prints `test <name> ... ` first and the outcome after the test's own output,
# which under `--nocapture` may share the outcome's line; the pending name
# covers that.
awk -v pkg="${CARGO_PKG_NAME:-?}" -v target="$target" '
  match($0, /^test .* \.\.\. /) {
    pending = substr($0, 6, RLENGTH - 10)
  }
  match($0, /(ok|FAILED) <[0-9.]+s>$/) && pending != "" {
    tail = substr($0, RSTART)
    split(tail, parts, " ")
    secs = parts[2]
    gsub(/[<>s]/, "", secs)
    printf "%s\t%s/%s\t%s\t%s\n", secs, pkg, target, pending, parts[1]
    pending = ""
  }
' "$log" >>"$out" || true
rm -f "$log"

if [[ -n "$cpu_before" && -n "$cpu_after" ]]; then
  cores=$(nproc 2>/dev/null || echo 1)
  read -r busy0 total0 <<<"$cpu_before"
  read -r busy1 total1 <<<"$cpu_after"
  awk -v b="$((busy1 - busy0))" -v t="$((total1 - total0))" -v cores="$cores" \
    -v secs="$elapsed" -v name="${CARGO_PKG_NAME:-?}/$target" -v args="$*" \
    'BEGIN { busy = t > 0 ? cores * b / t : 0; printf "%d\t%s\t%.2f\t%d\t%s\n", secs, name, busy, cores, args }' \
    >>"${out%.tsv}-binaries.tsv" || true
fi
exit "$status"
