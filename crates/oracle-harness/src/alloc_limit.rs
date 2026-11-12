//! A global allocator that counts the bytes a process holds, and refuses to
//! hold more than a limit.
//!
//! A test that runs a case which may exhaust memory needs the case stopped at
//! a bound, not left to swap the machine: `RLIMIT_AS` and `RLIMIT_DATA` are
//! not enforced on macOS, so the bound is drawn here, in the allocator. A
//! test binary installs [`LimitedAlloc`] as its global allocator; it holds no
//! limit until one is set, and costs a few relaxed atomic operations per
//! allocation. It also counts the allocations made ([`allocations`]): unlike
//! time, how many allocations a walk makes is the same on every run, and unlike
//! the bytes it holds at its peak, it sees work whose memory is freed as soon
//! as it is made. A child copy of the binary
//! ([`stack_probe`](crate::stack_probe)) sets one with [`limit_live_bytes`]
//! before it runs its case; an allocation past it fails, which aborts the
//! child with Rust's `memory allocation of … bytes failed`, and the parent
//! reads that as [`Limited::OutOfMemory`](crate::stack_probe::Limited).
//!
//! ```ignore
//! #[global_allocator]
//! static ALLOC: borzoi_oracle_harness::alloc_limit::LimitedAlloc =
//!     borzoi_oracle_harness::alloc_limit::LimitedAlloc;
//! ```

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

/// The system allocator, counting what it hands out against
/// [`limit_live_bytes`].
pub struct LimitedAlloc;

/// Bytes allocated and not yet freed.
static LIVE: AtomicUsize = AtomicUsize::new(0);
/// The most [`LIVE`] has been since [`reset_peak`].
static PEAK: AtomicUsize = AtomicUsize::new(0);
/// The most [`LIVE`] may be.
static LIMIT: AtomicUsize = AtomicUsize::new(usize::MAX);
/// How many times `alloc` or `alloc_zeroed` has been called.
static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

/// Count `bytes` more as live, or refuse if that passes the limit.
fn reserve(bytes: usize) -> bool {
    let live = LIVE.fetch_add(bytes, Ordering::Relaxed) + bytes;
    if live > LIMIT.load(Ordering::Relaxed) {
        LIVE.fetch_sub(bytes, Ordering::Relaxed);
        return false;
    }
    PEAK.fetch_max(live, Ordering::Relaxed);
    true
}

fn release(bytes: usize) {
    LIVE.fetch_sub(bytes, Ordering::Relaxed);
}

// SAFETY: every method forwards to `System` with the caller's arguments, and
// only adds bookkeeping; a refusal returns null, which `GlobalAlloc` permits.
unsafe impl GlobalAlloc for LimitedAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        if !reserve(layout.size()) {
            return std::ptr::null_mut();
        }
        // SAFETY: forwarded unchanged from our caller.
        let ptr = unsafe { System.alloc(layout) };
        if ptr.is_null() {
            release(layout.size());
        }
        ptr
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        if !reserve(layout.size()) {
            return std::ptr::null_mut();
        }
        // SAFETY: forwarded unchanged from our caller.
        let ptr = unsafe { System.alloc_zeroed(layout) };
        if ptr.is_null() {
            release(layout.size());
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: forwarded unchanged from our caller.
        unsafe { System.dealloc(ptr, layout) };
        release(layout.size());
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let old = layout.size();
        if new_size > old && !reserve(new_size - old) {
            return std::ptr::null_mut();
        }
        // SAFETY: forwarded unchanged from our caller.
        let moved = unsafe { System.realloc(ptr, layout, new_size) };
        match (moved.is_null(), new_size > old) {
            // The block is unchanged: undo the growth reserved above.
            (true, true) => release(new_size - old),
            (true, false) => {}
            (false, true) => {}
            (false, false) => release(old - new_size),
        }
        moved
    }
}

/// Refuse any allocation that would take the bytes held past `bytes`. Takes
/// effect only where [`LimitedAlloc`] is the global allocator.
pub fn limit_live_bytes(bytes: usize) {
    LIMIT.store(bytes, Ordering::Relaxed);
}

/// The bytes held now, as [`LimitedAlloc`] counts them.
pub fn live_bytes() -> usize {
    LIVE.load(Ordering::Relaxed)
}

/// The most bytes held since the last [`reset_peak`] (or the start).
pub fn peak_bytes() -> usize {
    PEAK.load(Ordering::Relaxed)
}

/// Start measuring [`peak_bytes`] from what is held now.
pub fn reset_peak() {
    PEAK.store(LIVE.load(Ordering::Relaxed), Ordering::Relaxed);
}

/// How many blocks have been allocated since the process began, as
/// [`LimitedAlloc`] counts them: every `alloc`, none of the `realloc`s that
/// grow a block in place or move it.
pub fn allocations() -> usize {
    ALLOCATIONS.load(Ordering::Relaxed)
}
