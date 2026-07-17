//! The `fcs-dump` child budget must actually bound the children.
//!
//! Each child is a resident FCS/.NET process holding hundreds of MB, so
//! `BORZOI_FCS_CHILDREN` exists to let a memory-constrained runner (or a dev
//! box already running sibling worktrees) hold the suite to fewer of them. A
//! budget that silently overruns is worse than no budget: the operator believes
//! they have capped memory, and finds out otherwise when something is OOM-killed.
//!
//! The arithmetic that divides the budget among the pools is integer division and
//! is easy to get subtly wrong — the original `(budget / POOLS).max(1)` handed one
//! child to *each* pool when the budget was 1 or 2, holding three children while
//! claiming to honour a budget of one. So rather than reason about it, search the
//! whole input range.

use crate::common::{effective_fcs_children, fcs_slots_per_pool};

/// The number of pools the budget is divided among. Kept in step with `common`'s
/// `FCS_POOLS` by [`the_pool_count_matches_the_harness`] below.
const POOLS: usize = 3;

/// **The invariant**: however the budget is set, the children a full complement of
/// pools can hold must not exceed it. Exhaustive over every setting that could
/// plausibly be typed, plus the boundaries — this is cheap to search and the
/// failure is invisible, so search it.
#[test]
fn no_setting_lets_the_pools_overrun_the_budget() {
    for requested in 1..=1024 {
        let budget = effective_fcs_children(requested);
        let held = POOLS * fcs_slots_per_pool(requested);

        assert!(
            held <= budget,
            "requested {requested} → each of the {POOLS} pools holds \
             {} child(ren) = {held} resident, over the {budget} in force",
            fcs_slots_per_pool(requested),
        );
        assert!(
            fcs_slots_per_pool(requested) >= 1,
            "requested {requested} → a pool with no children cannot serve a \
             request at all"
        );
    }
}

/// The budget is only ever clamped *up*, and only to the floor that makes the
/// pools workable — never silently down, which would under-serve a runner that
/// deliberately asked for more.
#[test]
fn the_budget_is_honoured_or_raised_to_the_workable_floor() {
    assert_eq!(
        effective_fcs_children(1),
        POOLS,
        "a budget below one-child-per-pool is not satisfiable, so it must be \
         raised to the floor rather than quietly overrun"
    );
    for requested in POOLS..=1024 {
        assert_eq!(
            effective_fcs_children(requested),
            requested,
            "a satisfiable budget must be honoured exactly"
        );
    }
}

/// `oracle_pool.rs` hardcodes the pool count to check the arithmetic against; if
/// a fourth pool is added to `common`, this catches the drift. (`common`'s own
/// `BatchPool::new` asserts the same thing from the other side, at run time.)
#[test]
fn the_pool_count_matches_the_harness() {
    // With a budget of exactly one-per-pool, each pool holds exactly one — which
    // is only true if `common` divides by the same POOLS this file assumes.
    assert_eq!(fcs_slots_per_pool(POOLS), 1);
    assert_eq!(effective_fcs_children(POOLS), POOLS);
}
