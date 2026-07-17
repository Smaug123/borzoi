//! `panic_silence` must swallow only the *silenced thread's* panics.
//!
//! The corpus sweeps in `borzoi-cst`, `borzoi-sema` and `borzoi`
//! parse or resolve thousands of real files, expect some to panic, and silence
//! those panics' messages. Each used to do it by swapping the *process-global*
//! panic hook — sound only while every sweep was its own process. In the unified
//! test binaries libtest runs them on concurrent threads, where the swaps race:
//! two sweeps interleaving `take_hook` / silent `set_hook` / restore can leave the
//! silent hook installed for good, and every later panic message in the process —
//! including a genuine failure's — vanishes. A test failure with no diagnostics is
//! a bad enough outcome to be worth testing against.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Barrier};

use borzoi_oracle_harness::panic_silence::{
    catch_unwind_silent, panic_is_silenced_here, panics_printed_here, silence_panics_here,
    take_silenced_panic,
};

/// A silent region on one thread must not silence any *other* thread, however the
/// regions interleave. This is the property the old global-hook swap could not
/// offer at any interleaving.
#[test]
fn silence_does_not_leak_across_threads() {
    const THREADS: usize = 8;
    const ROUNDS: usize = 200;

    // The silencers and the observer start together, to overlap the silent
    // regions. Only they are parties: a `Barrier` is reusable, so having the main
    // thread wait too would leave it blocked for a second cohort that never comes.
    let barrier = Arc::new(Barrier::new(THREADS + 1));
    // Set if the observer ever finds itself silenced by someone else's region.
    let leaked = Arc::new(AtomicBool::new(false));

    let silencers: Vec<_> = (0..THREADS)
        .map(|_| {
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                for _ in 0..ROUNDS {
                    let caught = catch_unwind_silent(|| panic!("expected, and silenced"));
                    assert!(caught.is_err(), "the panic should still be *caught*");
                    assert!(
                        !panic_is_silenced_here(),
                        "the silent region must not outlive the call that opened it"
                    );
                }
            })
        })
        .collect();

    // …while this thread — which never opens a silent region — checks that its own
    // panics would still print, throughout.
    let observer = {
        let barrier = Arc::clone(&barrier);
        let leaked = Arc::clone(&leaked);
        std::thread::spawn(move || {
            barrier.wait();
            for _ in 0..ROUNDS * THREADS {
                if panic_is_silenced_here() {
                    leaked.store(true, Ordering::Relaxed);
                }
                std::hint::spin_loop();
            }
        })
    };

    for t in silencers {
        t.join().expect("silencer thread panicked");
    }
    observer.join().expect("observer thread panicked");

    assert!(
        !leaked.load(Ordering::Relaxed),
        "a concurrent silent region silenced an unrelated thread — a real test \
         failure's panic message would have been swallowed"
    );
}

/// The region closes even when the guarded code panics — otherwise the first
/// expected panic in a sweep would silence that thread for the rest of the run.
#[test]
fn silence_is_released_after_a_panic() {
    assert!(!panic_is_silenced_here());
    let caught = catch_unwind_silent(|| panic!("boom"));
    assert!(caught.is_err());
    assert!(
        !panic_is_silenced_here(),
        "a panic inside the silent region left the region open"
    );
}

/// Regions nest: a sweep holds one across its whole loop while `catch_unwind_silent`
/// opens another per file. The inner one closing must not un-silence the outer —
/// which is why the thread-local is a depth counter and not a flag.
#[test]
fn regions_nest() {
    let outer = silence_panics_here();
    assert!(panic_is_silenced_here());

    let caught = catch_unwind_silent(|| panic!("inner"));
    assert!(caught.is_err());
    assert!(
        panic_is_silenced_here(),
        "the inner region closing un-silenced the outer one — a flag, not a depth"
    );

    drop(outer);
    assert!(!panic_is_silenced_here());
}

/// A silenced panic is *captured*, so a caller that wants to report it in its own
/// format (as `assembly`'s `fail_loud` does — a compact one-liner instead of a
/// backtrace) can, without installing a hook of its own.
#[test]
fn a_silenced_panic_is_captured_for_the_caller() {
    // Nothing was silenced on this thread yet.
    assert!(take_silenced_panic().is_none());

    let caught = catch_unwind_silent(|| panic!("the parser fell over"));
    assert!(caught.is_err());

    let p = take_silenced_panic().expect("the silenced panic should have been captured");
    assert_eq!(p.message, "the parser fell over");
    assert!(
        p.location.contains("panic_silence.rs"),
        "expected the panic site, got {:?}",
        p.location
    );

    // Taking it clears it, so the next caller can't misattribute a stale panic.
    assert!(take_silenced_panic().is_none());
}

/// The property with teeth: a panic *outside* a silent region still reaches the
/// printing hook, while panics inside one do not. This is what the sweeps put at
/// risk — a swallowed panic is a test failure with no message.
///
/// The counter is per-thread, so each half runs on its own scoped thread and reads
/// it *there* — immune to whatever the rest of the binary panics about
/// concurrently. (Panicking on a spawned thread also lets libtest's stderr capture
/// eat the expected messages, keeping the run's output clean.)
#[test]
fn an_unsilenced_panic_still_reaches_the_printing_hook() {
    // A panic inside a silent region must not reach the printing hook…
    let printed = std::thread::scope(|s| {
        s.spawn(|| {
            let before = panics_printed_here();
            let caught = catch_unwind_silent(|| panic!("silenced"));
            assert!(caught.is_err(), "the panic should still be caught");
            panics_printed_here() - before
        })
        .join()
        .expect("silencing thread panicked")
    });
    assert_eq!(
        printed, 0,
        "a panic inside a silent region was printed anyway"
    );

    // …and a panic outside one must.
    let printed = std::thread::scope(|s| {
        s.spawn(|| {
            let before = panics_printed_here();
            let caught = std::panic::catch_unwind(|| panic!("printed"));
            assert!(caught.is_err());
            panics_printed_here() - before
        })
        .join()
        .expect("printing thread panicked")
    });
    assert_eq!(
        printed, 1,
        "a panic outside any silent region never reached the printing hook — a \
         genuine failure's message would have been swallowed"
    );
}
