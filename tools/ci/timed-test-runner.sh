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
set -uo pipefail

bin=$1
shift

if [[ "$(basename "$(dirname "$bin")")" != deps ]]; then
  exec "$bin" "$@"
fi

out=${BORZOI_TEST_TIMES:-${RUNNER_TEMP:-/tmp}/test-times.tsv}
log=$(mktemp)

RUSTC_BOOTSTRAP=1 "$bin" "$@" -Zunstable-options --report-time | tee "$log"
status=${PIPESTATUS[0]}

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
exit "$status"
