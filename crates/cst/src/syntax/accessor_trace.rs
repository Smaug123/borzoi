//! Which typed-AST accessors a computation reads (the `accessor-trace`
//! feature).
//!
//! Every public accessor in [`crate::syntax`] opens with `accessor!("Type::name")`.
//! Without the feature that expands to nothing. With it, the accessor enters a
//! per-thread [`Guard`]; while a [`record`] call is running on the thread, each
//! *outermost* accessor call is logged (an accessor that another accessor calls
//! is part of the outer one's answer, not a separate read).
//!
//! It exists for one test: the parser differential's normalised AST must read
//! every accessor that `borzoi-sema` and the LSP read, or a field a consumer
//! depends on is compared with FCS nowhere. The differential records what its
//! projection reads; a separate scan finds what the consumers call
//! (`crates/cst/tests/all/accessor_coverage.rs`).

use std::cell::RefCell;
use std::collections::BTreeSet;

#[derive(Default)]
struct State {
    /// How many accessor calls are open on this thread.
    depth: u32,
    /// The accessors read at depth 0 while a [`record`] runs.
    log: Option<BTreeSet<&'static str>>,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

/// An open accessor call. Dropping it (on return or unwind) closes it.
pub struct Guard(());

/// Open the accessor `name` (`"Type::method"`), logging it when it is the
/// outermost open call and a [`record`] is running.
pub fn enter(name: &'static str) -> Guard {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        if s.depth == 0
            && let Some(log) = s.log.as_mut()
        {
            log.insert(name);
        }
        s.depth += 1;
    });
    Guard(())
}

impl Drop for Guard {
    fn drop(&mut self) {
        STATE.with(|s| s.borrow_mut().depth -= 1);
    }
}

/// Restores the enclosing log when a [`record`] ends, including by unwinding.
struct Recording {
    outer: Option<BTreeSet<&'static str>>,
}

impl Drop for Recording {
    fn drop(&mut self) {
        let outer = self.outer.take();
        STATE.with(|s| s.borrow_mut().log = outer);
    }
}

/// Run `f` and return, with its result, every accessor it read directly on
/// this thread. Nested calls each see only their own reads.
pub fn record<R>(f: impl FnOnce() -> R) -> (R, BTreeSet<&'static str>) {
    let outer = STATE.with(|s| s.borrow_mut().log.replace(BTreeSet::new()));
    let guard = Recording { outer };
    let result = f();
    let read = STATE.with(|s| s.borrow_mut().log.take().unwrap_or_default());
    drop(guard);
    (result, read)
}
