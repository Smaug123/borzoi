//! [`par_map`]: a corpus sweep's per-file work on every core, in file order.
//!
//! The corpus sweeps grade thousands of independent files, and a serial loop
//! leaves all but one core idle (CI measured `silent_region_sweep` and
//! `parser_corpus_sweep` at one busy core of four). Each sweep keeps its own
//! merge, which stays serial and sees results in the order the loop would
//! have produced them, so a manifest, a count or a printed sample is the
//! serial one.

use std::sync::atomic::{AtomicUsize, Ordering};

/// `f` applied to each of `0..len`, on up to one worker per core, returned in
/// index order.
///
/// `f` runs on worker threads, so what it returns must be `Send`; a sweep
/// hands it an index and reads and parses the file there. The workers have
/// the default stack: a sweep that resolves or infers runs that on a
/// `WALK_STACK_BYTES` thread of its own (`stack_probe::on_stack`). Per-thread state
/// (a [`silence_panics_here`](crate::panic_silence::silence_panics_here)
/// guard, an accessor trace) must be set up inside `f`, not around the call.
/// A panic in `f` is re-raised here with its own payload.
pub fn par_map<T: Send>(len: usize, f: impl Fn(usize) -> T + Sync) -> Vec<T> {
    let workers = std::thread::available_parallelism()
        .map_or(4, |n| n.get())
        .min(len)
        .max(1);
    let next = AtomicUsize::new(0);
    let mut out: Vec<(usize, T)> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..workers)
            .map(|_| {
                scope.spawn(|| {
                    let mut mine = Vec::new();
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        if i >= len {
                            return mine;
                        }
                        mine.push((i, f(i)));
                    }
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().unwrap_or_else(|p| std::panic::resume_unwind(p)))
            .collect()
    });
    out.sort_by_key(|(i, _)| *i);
    assert_eq!(out.len(), len, "every index has a result");
    out.into_iter().map(|(_, t)| t).collect()
}

#[cfg(test)]
mod tests {
    use super::par_map;

    #[test]
    fn agrees_with_the_serial_map_at_every_length() {
        for len in 0..200 {
            let f = |i: usize| i.wrapping_mul(2_654_435_761) % 1_000;
            let serial: Vec<usize> = (0..len).map(f).collect();
            assert_eq!(par_map(len, f), serial, "len {len}");
        }
    }

    #[test]
    fn a_panic_in_a_worker_reaches_the_caller_with_its_payload() {
        let caught = std::panic::catch_unwind(|| {
            par_map(64, |i| {
                if i == 37 {
                    panic!("item 37 failed");
                }
                i
            })
        })
        .expect_err("the panic propagates");
        assert_eq!(caught.downcast_ref::<&str>(), Some(&"item 37 failed"));
    }
}
