//! Suppressing *expected* panic messages, per-thread, without a hook race.
//!
//! The corpus sweeps parse or resolve thousands of real files and expect some to
//! panic on constructs we don't model; they count the outcomes themselves and
//! don't want thousands of backtraces on stderr. Each used to silence them by
//! swapping the **process-global** panic hook around the sweep — `take_hook`, a
//! silent `set_hook`, restore afterwards.
//!
//! That is sound only while every sweep is its own *process*. Once a crate's test
//! cases share one binary (as they now do — see the `tests/all/` targets), libtest
//! runs them on concurrent threads and the swaps interleave: sweep A takes the
//! default hook, sweep B takes A's *silent* hook believing it to be the default,
//! and when B restores, the silent hook is installed for good. Every later panic
//! message in the process vanishes — including a genuine failure's. A test that
//! fails with no diagnostics is worse than one that fails loudly.
//!
//! There is also no need for the hook to change at all. Install **one** hook for
//! the process and have it consult a thread-local depth counter: a panic on a
//! thread inside a silent region prints nothing, and every other panic — on any
//! other test thread, or on this one outside the region — prints as usual. The
//! hook is then immutable, so there is nothing left to race on.

use std::cell::{Cell, RefCell};
use std::marker::PhantomData;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::OnceLock;

thread_local! {
    /// How many [`PanicSilence`] guards this thread currently holds. A counter
    /// rather than a flag so regions nest: a sweep may hold one across its whole
    /// loop while [`catch_unwind_silent`] opens another per file, and the inner
    /// one closing must not un-silence the outer.
    static SILENT_DEPTH: Cell<usize> = const { Cell::new(0) };

    /// Panics this thread has let through to the printing hook — see
    /// [`panics_printed_here`].
    static PRINTED_HERE: Cell<usize> = const { Cell::new(0) };

    /// The most recent panic silenced on this thread — see [`take_silenced_panic`].
    static LAST_SILENCED: RefCell<Option<SilencedPanic>> = const { RefCell::new(None) };
}

/// What a silenced panic said, for callers that want to report it *themselves*
/// rather than let it print.
///
/// Some harnesses (`assembly`'s `fail_loud`) expect panics from a parser on
/// corrupt input and want a compact one-liner per panic instead of a backtrace.
/// Installing a custom hook to do that is exactly the process-global mutation
/// this module exists to avoid — a hook installed by one case group formats and
/// de-backtraces failures from *every other* group in the binary. So: silence
/// the panic, capture what it said, and let the caller print in its own format.
#[derive(Debug, Clone)]
pub struct SilencedPanic {
    /// `file:line` of the panic site, or `<unknown>`.
    pub location: String,
    /// The panic payload, as far as it downcasts to a string.
    pub message: String,
}

/// Take the most recent panic silenced on this thread, clearing it.
///
/// Only panics raised inside a silent region are recorded, so this returns
/// `None` unless a [`catch_unwind_silent`] on this thread has just caught one.
pub fn take_silenced_panic() -> Option<SilencedPanic> {
    LAST_SILENCED.with(|s| s.borrow_mut().take())
}

/// Install the process-wide hook, once. Delegates to whatever hook was in place
/// (libtest's, normally) for every panic not raised inside a silent region.
fn install_hook() {
    static INSTALLED: OnceLock<()> = OnceLock::new();
    INSTALLED.get_or_init(|| {
        let default = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if SILENT_DEPTH.with(Cell::get) > 0 {
                let payload = info.payload();
                let message = payload
                    .downcast_ref::<&str>()
                    .map(|s| (*s).to_owned())
                    .or_else(|| payload.downcast_ref::<String>().cloned())
                    .unwrap_or_default();
                let location = info.location().map_or_else(
                    || "<unknown>".to_owned(),
                    |l| format!("{}:{}", l.file(), l.line()),
                );
                LAST_SILENCED.with(|s| {
                    *s.borrow_mut() = Some(SilencedPanic { location, message });
                });
                return;
            }
            PRINTED_HERE.with(|n| n.set(n.get() + 1));
            default(info);
        }));
    });
}

/// A silent region on **this thread**: panics raised while it is alive print
/// nothing. Dropping it (including while unwinding) ends the region.
///
/// Deliberately not [`Send`]: the depth it decrements is its creating thread's,
/// so moving a guard to another thread would silence one thread and un-silence
/// another. The compiler enforces that rather than a comment asking nicely.
#[must_use = "dropping the guard immediately ends the silent region"]
pub struct PanicSilence {
    _not_send: PhantomData<*const ()>,
}

impl Drop for PanicSilence {
    fn drop(&mut self) {
        SILENT_DEPTH.with(|d| d.set(d.get() - 1));
    }
}

/// Open a silent region on this thread; it lasts until the guard is dropped.
///
/// Use this for a *sweep*: hold one guard across the loop. For a single call,
/// [`catch_unwind_silent`] is the closure-shaped equivalent.
pub fn silence_panics_here() -> PanicSilence {
    install_hook();
    SILENT_DEPTH.with(|d| d.set(d.get() + 1));
    PanicSilence {
        _not_send: PhantomData,
    }
}

/// Run `f`, catching a panic without printing it — the closure-shaped
/// [`silence_panics_here`].
///
/// Starts from a clean capture, so a later [`take_silenced_panic`] reflects *this*
/// call: it yields `f`'s panic iff `f` panicked, never a stale one from an earlier
/// catch whose panic the caller never took. (The hook only ever *overwrites* the
/// capture, so without this reset a non-panicking call would leave the previous
/// one standing — the contract [`take_silenced_panic`] documents would not hold.)
/// The sweep form [`silence_panics_here`] does not reset: it is the
/// accumulate-then-take-when-ready primitive, and its callers rely on last-wins.
pub fn catch_unwind_silent<F, T>(f: F) -> std::thread::Result<T>
where
    F: FnOnce() -> T,
{
    take_silenced_panic();
    let _silence = silence_panics_here();
    catch_unwind(AssertUnwindSafe(f))
}

/// Whether a panic raised *on this thread, right now* would be swallowed.
pub fn panic_is_silenced_here() -> bool {
    SILENT_DEPTH.with(Cell::get) > 0
}

/// How many panics *this thread* has passed through to the printing hook.
///
/// Instrumentation for the tests: the property that matters is not "the depth
/// reads zero" but "a panic outside a silent region still *reaches the printing
/// hook*" — i.e. a real failure's message is not swallowed — and with libtest
/// owning stderr, counting is the only way to observe that in-process.
///
/// Per-thread rather than a global counter: the whole point is that threads do
/// not interfere, and any `#[should_panic]` case running concurrently would bump
/// a global one and make the assertions flaky.
pub fn panics_printed_here() -> usize {
    PRINTED_HERE.with(Cell::get)
}

#[cfg(test)]
mod tests {
    use super::{catch_unwind_silent, take_silenced_panic};

    // The captured panic is thread-local, and each `#[test]` runs on its own
    // thread, so these do not interfere despite the process-global hook.

    #[test]
    fn a_caught_panic_is_captured_with_its_message() {
        let caught = catch_unwind_silent(|| panic!("boom-{}", 42));
        assert!(caught.is_err());
        let p = take_silenced_panic().expect("the panic should have been captured");
        assert!(p.message.contains("boom-42"), "message was {:?}", p.message);
    }

    #[test]
    fn take_after_a_non_panicking_catch_is_none() {
        // A caught panic left untaken…
        let _ = catch_unwind_silent(|| panic!("earlier"));
        // …must not survive a later, non-panicking catch: that call resets the
        // capture, so `take` reflects it (nothing), not the stale panic. Without
        // the reset this would wrongly return `Some("earlier")` and the next
        // caller would report an unrelated failure as its own.
        let ok = catch_unwind_silent(|| 7);
        assert_eq!(ok.expect("no panic"), 7);
        assert!(
            take_silenced_panic().is_none(),
            "a non-panicking catch must clear the previous capture"
        );
    }

    #[test]
    fn take_reflects_the_most_recent_catch() {
        let _ = catch_unwind_silent(|| panic!("first"));
        let _ = catch_unwind_silent(|| panic!("second"));
        let p = take_silenced_panic().expect("captured");
        assert!(p.message.contains("second"), "message was {:?}", p.message);
    }
}
