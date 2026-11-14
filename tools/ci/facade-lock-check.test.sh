#!/usr/bin/env bash
#
# Tests for facade-lock-check.sh: each case commits a base holding a published
# lock, an alias's published lock, an unpublished lock and a consumer fixture
# to a throwaway repository, commits one change on top, and runs the real
# script against the base. And CI runs the script, with the history it needs.

set -uo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
repo="$(cd "$here/../.." && pwd)"
script="$here/facade-lock-check.sh"
fails=0

assert_eq() { # name expected actual
  if [[ "$2" == "$3" ]]; then
    printf 'ok   - %s\n' "$1"
  else
    printf 'FAIL - %s\n    expected: [%s]\n    actual:   [%s]\n' "$1" "$2" "$3"
    fails=$((fails + 1))
  fi
}

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

T=$'\t'
LOCKS=crates/cst/facades
FIXTURES=crates/cst/tests/all/facade_consumers
EPOCH_FILE=tools/astgen/src/facade.rs

published_lock() { # module epoch records...
  local module="$1" epoch="$2"
  shift 2
  printf '# The `%s` typed facade'"'"'s surface. PUBLISHED.\n' "$module"
  printf '# freeze-epoch: %s\n' "$epoch"
  printf '%s\n' "$@"
}

base_tree() { # dir
  local d="$1"
  mkdir -p "$d/$LOCKS" "$d/$FIXTURES" "$d/$(dirname "$EPOCH_FILE")"
  printf 'pub const FREEZE_EPOCH: u32 = 1;\n' >"$d/$EPOCH_FILE"
  published_lock v8 1 \
    "node${T}AppExpr${T}APP_EXPR" \
    "node${T}ParenExpr${T}PAREN_EXPR" \
    "enum${T}Expr${T}App${T}AppExpr" \
    "enum${T}Expr${T}Paren${T}ParenExpr" \
    "value${T}Form${T}Postfix${T}unit" \
    "derive${T}Form${T}Clone" \
    "accessor${T}ParenExpr${T}inner${T}option enum Expr${T}union.inner" \
    >"$d/$LOCKS/v8.lock"
  published_lock v10 1 "alias-of${T}v8" >"$d/$LOCKS/v10.lock"
  printf '# The `v11` facade, unpublished.\nalias-of\tv8\n' >"$d/$LOCKS/v11.lock"
  printf '//! freeze-epoch: 1\nfn enum_expr() {}\n' >"$d/$FIXTURES/v8.rs"
  printf 'mod v8;\n' >"$d/$FIXTURES/mod.rs"
}

commit() { # dir message
  git -C "$1" add -A
  git -C "$1" -c user.name=t -c user.email=t@t -c commit.gpgsign=false commit --quiet --allow-empty -m "$2"
}

n=0
# Run the script on CHANGE (a function editing the tree in its argument)
# committed over the base; print its exit status and its output.
run_case() { # change
  n=$((n + 1))
  local d="$tmp/case$n"
  git init --quiet "$d"
  base_tree "$d"
  commit "$d" base
  local base
  base="$(git -C "$d" rev-parse HEAD)"
  "$1" "$d"
  commit "$d" change
  local out status
  out="$(cd "$d" && env -u GITHUB_EVENT_NAME bash "$script" "$base" 2>&1)"
  status=$?
  printf '%s\n%s' "$status" "$out"
}

status_of() { head -n1 <<<"$1"; }
mentions() { # output needle
  if grep -qF -- "$2" <<<"$1"; then echo yes; else echo no; fi
}

check() { # name change expected-status [needle]
  local out
  out="$(run_case "$2")"
  assert_eq "$1: exit status" "$3" "$(status_of "$out")"
  if [[ $# -ge 4 ]]; then
    assert_eq "$1: says '$4'" yes "$(mentions "$out" "$4")"
  fi
}

edit_lock() { # dir lock sed-expression
  sed "$3" "$1/$LOCKS/$2" >"$1/edited" && mv "$1/edited" "$1/$LOCKS/$2"
}

# --- what passes --------------------------------------------------------------

nothing() { :; }
add_accessor() { echo "accessor${T}AppExpr${T}func${T}option enum Expr${T}union.func" >>"$1/$LOCKS/v8.lock"; }
add_kind() { echo "node${T}AppExpr${T}INFIX_APP_EXPR" >>"$1/$LOCKS/v8.lock"; }
add_node() { echo "node${T}FreshNode${T}FRESH_NODE" >>"$1/$LOCKS/v8.lock"; }
change_delegate() { edit_lock "$1" v8.lock 's/union\.inner/union.inner_expr/'; }
add_compat() { echo "accessor${T}ParenExpr${T}old_inner${T}option enum Expr${T}union.inner" >>"$1/$LOCKS/v8.lock"; }
add_derive() { echo "derive${T}Form${T}Copy" >>"$1/$LOCKS/v8.lock"; }
edit_comment() { edit_lock "$1" v8.lock 's/^# The .*/# Reworded./'; }
edit_unpublished() { printf 'node\tAny\tANY\n' >"$1/$LOCKS/v11.lock"; }
publish_v11() { published_lock v11 1 "node${T}Any${T}ANY" >"$1/$LOCKS/v11.lock"; }
new_fixture() { printf '//! freeze-epoch: 1\n' >"$1/$FIXTURES/v11.rs"; }
edit_fixtures_mod() { printf 'mod v8;\nmod v11;\n' >"$1/$FIXTURES/mod.rs"; }
bump() {
  printf 'pub const FREEZE_EPOCH: u32 = 2;\n' >"$1/$EPOCH_FILE"
  published_lock v8 2 "node${T}Other${T}OTHER" >"$1/$LOCKS/v8.lock"
  printf '//! freeze-epoch: 2\n' >"$1/$FIXTURES/v8.rs"
}

check "an unchanged freeze" nothing 0 "ok - 2 published lock(s)"
check "an added accessor" add_accessor 0
check "an added kind for a node" add_kind 0
check "an added node type" add_node 0
check "an added derive" add_derive 0
check "a reworded comment" edit_comment 0
check "an unpublished lock changes freely" edit_unpublished 0
check "a first publication" publish_v11 0
check "a new fixture" new_fixture 0
check "the fixtures' generated mod.rs" edit_fixtures_mod 0
check "a FREEZE_EPOCH bump re-baselines" bump 0 "re-baselined"
check "a delegate pointed at a renamed union accessor" change_delegate 0
check "a compat accessor beside it" add_compat 0

# --- what fails ---------------------------------------------------------------

remove_accessor() { edit_lock "$1" v8.lock '/^accessor/d'; }
change_shape() { edit_lock "$1" v8.lock 's/option enum Expr/iter enum Expr/'; }

remove_kind() { edit_lock "$1" v8.lock '/PAREN_EXPR/d'; }
remove_derive() { edit_lock "$1" v8.lock '/^derive/d'; }
add_variant() {
  echo "enum${T}Expr${T}Fresh${T}AppExpr" >>"$1/$LOCKS/v8.lock"
}
add_value() { echo "value${T}Form${T}Prefix${T}unit" >>"$1/$LOCKS/v8.lock"; }
retarget_alias() { edit_lock "$1" v10.lock "s/alias-of${T}v8/alias-of${T}v9/"; }
alias_to_head() {
  published_lock v10 1 "node${T}AppExpr${T}APP_EXPR" >"$1/$LOCKS/v10.lock"
}
head_to_alias() { published_lock v8 1 "alias-of${T}v4_6" >"$1/$LOCKS/v8.lock"; }
delete_lock() { rm "$1/$LOCKS/v8.lock"; }
unstamp() { edit_lock "$1" v8.lock '/^# freeze-epoch/d'; }
restamp() { edit_lock "$1" v8.lock 's/^# freeze-epoch: 1$/# freeze-epoch: 2/'; }
edit_fixture() { printf '//! freeze-epoch: 1\nfn enum_expr() { todo!() }\n' >"$1/$FIXTURES/v8.rs"; }
delete_fixture() { rm "$1/$FIXTURES/v8.rs"; }
lower() { printf 'pub const FREEZE_EPOCH: u32 = 0;\n' >"$1/$EPOCH_FILE"; }

check "a removed accessor" remove_accessor 1 "- accessor${T}ParenExpr${T}inner"
check "a changed accessor shape" change_shape 1 "removes or rewrites"
check "a removed kind" remove_kind 1 "- node${T}ParenExpr${T}PAREN_EXPR"
check "a removed derive" remove_derive 1 "removes or rewrites"
check "an added enum variant" add_variant 1 "+ enum${T}Expr${T}Fresh"
check "an added value variant" add_value 1 "adds records"
check "a retargeted alias" retarget_alias 1 "v10.lock removes or rewrites"
check "an alias become a head" alias_to_head 1 "- alias-of${T}v8"
check "a head become an alias" head_to_alias 1 "+ alias-of${T}v4_6"
check "a deleted published lock" delete_lock 1 "published, and deleted"
check "an unpublished lock" unstamp 1 "not at all at HEAD"
check "a restamped lock without a bump" restamp 1 "FREEZE_EPOCH is unchanged"
check "an edited fixture" edit_fixture 1 "frozen consumer fixture, and edited"
check "a deleted fixture" delete_fixture 1 "frozen consumer fixture, and deleted"
check "a lowered FREEZE_EPOCH" lower 1 "only rises"

# --- it fails closed ------------------------------------------------------------

d="$tmp/closed"
git init --quiet "$d"
base_tree "$d"
commit "$d" base
assert_eq "an unknown base is an error" 2 \
  "$(cd "$d" && bash "$script" 0123456789012345678901234567890123456789 >/dev/null 2>&1; echo $?)"
assert_eq "a pull_request event without its base is an error" 2 \
  "$(cd "$d" && GITHUB_EVENT_NAME=pull_request GITHUB_PR_BASE_SHA= bash "$script" >/dev/null 2>&1; echo $?)"
assert_eq "a pull_request event's base is its base" 0 \
  "$(cd "$d" && GITHUB_EVENT_NAME=pull_request GITHUB_PR_BASE_SHA="$(git rev-parse HEAD)" \
    bash "$script" >/dev/null 2>&1; echo $?)"
assert_eq "a merge_group event's base is its base" 0 \
  "$(cd "$d" && GITHUB_EVENT_NAME=merge_group GITHUB_MERGE_GROUP_BASE_SHA="$(git rev-parse HEAD)" \
    bash "$script" >/dev/null 2>&1; echo $?)"
assert_eq "another event has no base" 2 \
  "$(cd "$d" && GITHUB_EVENT_NAME=push bash "$script" >/dev/null 2>&1; echo $?)"

# --- CI runs it -----------------------------------------------------------------

# The `facade-locks` job: full history, the event's base, and the script.
job="$(awk '
  /^  facade-locks:$/ { on = 1; print; next }
  on && /^  [A-Za-z0-9_-]+:$/ { exit }
  on { print }
' "$repo/.github/workflows/ci.yml")"
assert_eq "ci.yml has a facade-locks job running the script" yes \
  "$(mentions "$job" "run: bash tools/ci/facade-lock-check.sh")"
assert_eq "the facade-locks job fetches the base" yes "$(mentions "$job" "fetch-depth: 0")"
assert_eq "the facade-locks job is required" yes \
  "$(awk '/^  all-required-checks-complete:$/ { on = 1 } on' "$repo/.github/workflows/ci.yml" |
    grep -qx '      - facade-locks' && echo yes || echo no)"
assert_eq "the lint job runs this test" yes \
  "$(mentions "$(cat "$repo/.github/workflows/ci.yml")" "run: bash tools/ci/facade-lock-check.test.sh")"
# The repository's own published locks hold against its own HEAD.
assert_eq "the checkout's freeze holds against itself" 0 \
  "$(cd "$repo" && bash "$script" HEAD >/dev/null 2>&1; echo $?)"

if ((fails > 0)); then
  printf '\n%d failure(s)\n' "$fails"
  exit 1
fi
printf '\nall passed\n'
