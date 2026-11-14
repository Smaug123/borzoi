#!/usr/bin/env bash
#
# The published facades' freeze (`docs/ast-versioning-plan.md` D6): diff every
# published facade lock, and every frozen consumer fixture, against the base
# commit, and allow only what no consumer of a published facade can observe as
# a break.
#
# A lock under crates/cst/facades/ is published when it carries a
# `# freeze-epoch: N` stamp (astgen writes it when it publishes the facade).
# A lock published at the base must, at HEAD:
#   - still exist, published, with the same stamp;
#   - keep every record it had, an accessor's delegate aside (it names the
#     union accessor re-cast, which a caller never sees, and is how a
#     published accessor survives a union rename). A removed or rewritten
#     record breaks callers; that includes an `alias-of` record dropped or
#     retargeted, since an alias's types are its head's and that identity is
#     part of the contract;
#   - gain only `accessor`, `node` and `derive` records: a new accessor, a new
#     kind for a node (or a new node type), a new trait on a value enum. An
#     added `enum` or `value` record is a new variant, which breaks every
#     exhaustive match on its enum, and an added `alias-of` turns a head into
#     an alias.
# A consumer fixture under crates/cst/tests/all/facade_consumers/ (other than
# its generated mod.rs) present at the base must be byte-identical at HEAD.
# A lock first stamped, or a fixture first added, since the base is a
# publication, and passes.
#
# All of it is waived for a change that raises FREEZE_EPOCH
# (tools/astgen/src/facade.rs), the one re-baseline route; lowering it fails.
#
# Usage: facade-lock-check.sh [BASE]
#   Compares the commit HEAD with BASE. Without BASE: in CI, the event's base
#   commit (GITHUB_EVENT_NAME pull_request -> GITHUB_PR_BASE_SHA, merge_group
#   -> GITHUB_MERGE_GROUP_BASE_SHA); otherwise `git merge-base HEAD
#   origin/main`. Unlike change detection this fails closed: a base it cannot
#   resolve is an error (exit 2), never a pass.
#
# Exit status: 0 when the freeze holds, 1 when it is broken, 2 when the check
# cannot run.

set -euo pipefail
export LC_ALL=C

locks_dir=crates/cst/facades
fixtures_dir=crates/cst/tests/all/facade_consumers
epoch_file=tools/astgen/src/facade.rs

die() {
  echo "facade-lock-check: $*" >&2
  exit 2
}

cd "$(git rev-parse --show-toplevel)" || die "not in a git repository"

resolve_base() {
  if [[ $# -ge 1 ]]; then
    printf '%s\n' "$1"
    return
  fi
  case "${GITHUB_EVENT_NAME:-}" in
    pull_request)
      [[ -n "${GITHUB_PR_BASE_SHA:-}" ]] || die "a pull_request event without GITHUB_PR_BASE_SHA"
      printf '%s\n' "$GITHUB_PR_BASE_SHA"
      ;;
    merge_group)
      [[ -n "${GITHUB_MERGE_GROUP_BASE_SHA:-}" ]] || die "a merge_group event without GITHUB_MERGE_GROUP_BASE_SHA"
      printf '%s\n' "$GITHUB_MERGE_GROUP_BASE_SHA"
      ;;
    "")
      git merge-base HEAD origin/main 2>/dev/null || die "no merge base of HEAD and origin/main; pass BASE"
      ;;
    *)
      die "no base commit for a ${GITHUB_EVENT_NAME} event; pass BASE"
      ;;
  esac
}

base="$(resolve_base "$@")"
git cat-file -e "$base^{commit}" 2>/dev/null ||
  die "the base $base is not a commit in this clone (a shallow checkout needs fetch-depth: 0)"

# The text of PATH at REV; fails when REV has no PATH.
show() { git show "$1:$2" 2>/dev/null; }

freeze_epoch_at() {
  show "$1" "$epoch_file" | sed -n 's/^pub const FREEZE_EPOCH: u32 = \([0-9][0-9]*\);$/\1/p'
}

# The freeze-epoch stamp of the lock text on stdin, or nothing.
stamp() { sed -n 's/^# freeze-epoch: \([0-9][0-9]*\)$/\1/p'; }

# The records of the lock text on stdin, sorted for `comm`. An accessor's
# delegate (its fifth field, `union.<method>`) is dropped: it names the union
# accessor the published one re-casts, not anything a caller sees, and
# pointing it at a renamed union accessor is how a published facade survives
# the rename.
records() {
  grep -v '^#' | grep -v '^$' |
    awk -F'\t' 'BEGIN { OFS = "\t" } $1 == "accessor" { print $1, $2, $3, $4; next } { print }' |
    sort || true
}

head_epoch="$(freeze_epoch_at HEAD)"
[[ -n "$head_epoch" ]] || die "HEAD's $epoch_file declares no FREEZE_EPOCH"
base_epoch="$(freeze_epoch_at "$base")"
if [[ -n "$base_epoch" && "$head_epoch" != "$base_epoch" ]]; then
  if ((head_epoch < base_epoch)); then
    echo "FAIL - FREEZE_EPOCH is lowered from $base_epoch to $head_epoch; it only rises" >&2
    exit 1
  fi
  echo "FREEZE_EPOCH rises from $base_epoch to $head_epoch: the published locks and fixtures are re-baselined, unchecked."
  exit 0
fi

failures=0
fail() {
  echo "FAIL - $*" >&2
  failures=$((failures + 1))
}

published=0
while IFS= read -r path; do
  [[ "$path" == *.lock ]] || continue
  base_text="$(show "$base" "$path")"
  base_stamp="$(stamp <<<"$base_text")"
  [[ -n "$base_stamp" ]] || continue
  published=$((published + 1))
  if ! head_text="$(show HEAD "$path")"; then
    fail "$path is published, and deleted"
    continue
  fi
  head_stamp="$(stamp <<<"$head_text")"
  if [[ "$head_stamp" != "$base_stamp" ]]; then
    fail "$path is stamped freeze-epoch $base_stamp at the base and ${head_stamp:-not at all} at HEAD, but FREEZE_EPOCH is unchanged"
    continue
  fi
  base_records="$(records <<<"$base_text")"
  head_records="$(records <<<"$head_text")"
  removed="$(comm -23 <(printf '%s\n' "$base_records") <(printf '%s\n' "$head_records"))"
  added="$(comm -13 <(printf '%s\n' "$base_records") <(printf '%s\n' "$head_records"))"
  if [[ -n "$removed" ]]; then
    fail "$path removes or rewrites published records:"
    sed 's/^/    - /' <<<"$removed" >&2
  fi
  forbidden="$(grep -Ev $'^(accessor|node|derive)\t' <<<"$added" || true)"
  if [[ -n "$forbidden" ]]; then
    fail "$path adds records a published facade cannot take (only accessor, node and derive records may be added):"
    sed 's/^/    + /' <<<"$forbidden" >&2
  fi
done < <(git ls-tree --name-only "$base" "$locks_dir/")

while IFS= read -r path; do
  [[ "$path" == *.rs && "$(basename "$path")" != mod.rs ]] || continue
  if ! git cat-file -e "HEAD:$path" 2>/dev/null; then
    fail "$path is a frozen consumer fixture, and deleted"
  elif [[ "$(git rev-parse "$base:$path")" != "$(git rev-parse "HEAD:$path")" ]]; then
    fail "$path is a frozen consumer fixture, and edited"
  fi
done < <(git ls-tree --name-only "$base" "$fixtures_dir/")

if ((failures > 0)); then
  cat >&2 <<EOF

The published facades' freeze is broken ($failures finding(s) against $base).
A published lock is astgen's input, not its output: it changes only by added
accessor, node and derive records, and its consumer fixture never changes. To
re-baseline every published facade (allowed only until the first external
release), raise FREEZE_EPOCH in $epoch_file and regenerate; see
"Publishing a facade" in docs/ast-versioning-plan.md.
EOF
  exit 1
fi
echo "ok - $published published lock(s) and their fixtures hold against $base"
