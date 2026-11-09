#!/usr/bin/env bash
# Render the per-test timings `timed-test-runner.sh` recorded as a Markdown
# table of the slowest tests, then each test binary run's wall time and the
# machine's average busy cores over it, for `$GITHUB_STEP_SUMMARY`.
#
#   slowest-tests.sh [TIMES_TSV] [N]
#
# Wall-clock per test, measured inside a binary whose tests run concurrently,
# so a test's time includes contention with its neighbours: rank by it, but do
# not sum it.
set -euo pipefail

times=${1:-${BORZOI_TEST_TIMES:-${RUNNER_TEMP:-/tmp}/test-times.tsv}}
n=${2:-30}

if [[ ! -s "$times" ]]; then
  echo "::warning::no per-test timings were recorded at $times"
  echo "No per-test timings were recorded."
  exit 0
fi

total=$(wc -l <"$times" | tr -d ' ')
echo "### Slowest ${n} of ${total} tests"
echo
echo "| seconds | binary | test | outcome |"
echo "| ---: | --- | --- | --- |"
# `awk` stops printing rather than `head` closing the pipe, which would make
# `sort` die of SIGPIPE and `pipefail` fail the step.
sort -t$'\t' -k1,1 -g -r "$times" |
  awk -F'\t' -v n="$n" 'NR <= n { gsub(/\|/, "\\|", $3); printf "| %s | %s | `%s` | %s |\n", $1, $2, $3, $4 }'

binaries=${times%.tsv}-binaries.tsv
if [[ -s "$binaries" ]]; then
  echo
  echo "### Test binary runs: wall time and average busy cores"
  echo
  echo "| seconds | binary | busy cores | of | libtest args |"
  echo "| ---: | --- | ---: | ---: | --- |"
  awk -F'\t' '$1 >= 5 { gsub(/\|/, "\\|", $5); printf "| %s | %s | %s | %s | `%s` |\n", $1, $2, $3, $4, $5 }' "$binaries"
fi
