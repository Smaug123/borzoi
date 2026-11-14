//! The CPU time the calling thread has used: the clock for a test that holds
//! a walk's cost on a machine it shares.
//!
//! A wall clock counts every moment the thread waits for a core, so on a
//! loaded machine a run's time is mostly how long the scheduler kept it off
//! one: a walk of a few milliseconds reads as a hundred, and any bound on a
//! ratio of two runs fails at random. The thread's CPU time counts only what
//! it ran. Load still slows each instruction a little (shared caches, a
//! sibling hyperthread), but that is a factor on both runs, not time added to
//! one.

use std::time::Duration;

/// The CPU time the calling thread has used since it started.
///
/// # Panics
///
/// Panics if the platform has no per-thread CPU clock
/// (`CLOCK_THREAD_CPUTIME_ID`), which every target the tests run on has.
pub fn thread_cpu_time() -> Duration {
    let mut now = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `now` is a valid, writable `timespec` for the call's duration,
    // and `clock_gettime` writes nothing else.
    let status = unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut now) };
    assert_eq!(
        status,
        0,
        "clock_gettime(CLOCK_THREAD_CPUTIME_ID): {}",
        std::io::Error::last_os_error()
    );
    let secs = u64::try_from(now.tv_sec).expect("a thread's CPU time is not negative");
    let nanos = u32::try_from(now.tv_nsec).expect("tv_nsec is below a second");
    Duration::new(secs, nanos)
}

/// Run `f` and return its result with the CPU time the calling thread spent
/// in it.
pub fn cpu_timed<T>(f: impl FnOnce() -> T) -> (T, Duration) {
    let start = thread_cpu_time();
    let value = f();
    (value, thread_cpu_time().saturating_sub(start))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    /// Burn the calling thread's CPU until `amount` of it has gone, or panic
    /// after a minute of wall time, however loaded the machine.
    fn spin_for(amount: Duration) {
        let start = thread_cpu_time();
        let deadline = Instant::now() + Duration::from_secs(60);
        let mut x = 0u64;
        while thread_cpu_time().saturating_sub(start) < amount {
            for i in 0..10_000u64 {
                x = std::hint::black_box(x.wrapping_mul(31).wrapping_add(i));
            }
            assert!(
                Instant::now() < deadline,
                "a minute of wall time ran {:?} of thread CPU time, short of {amount:?}",
                thread_cpu_time().saturating_sub(start)
            );
        }
        std::hint::black_box(x);
    }

    #[test]
    fn time_spent_off_the_cpu_is_not_counted() {
        let ((), spent) = cpu_timed(|| std::thread::sleep(Duration::from_millis(300)));
        assert!(
            spent < Duration::from_millis(50),
            "300 ms asleep counted as {spent:?} of CPU time"
        );
    }

    #[test]
    fn another_threads_work_is_not_counted() {
        let ((), spent) = cpu_timed(|| {
            std::thread::spawn(|| spin_for(Duration::from_millis(200)))
                .join()
                .expect("the spinning thread");
        });
        assert!(
            spent < Duration::from_millis(50),
            "waiting on a thread that ran 200 ms counted as {spent:?} of CPU time"
        );
    }

    #[test]
    fn the_threads_own_work_is_counted_and_never_exceeds_the_wall_clock() {
        let wall = Instant::now();
        let ((), spent) = cpu_timed(|| spin_for(Duration::from_millis(100)));
        let wall = wall.elapsed();
        assert!(spent >= Duration::from_millis(100), "{spent:?}");
        // The two readings come from different clocks: the thread's CPU time
        // from the scheduler's accounting, the wall time from the monotonic
        // clocksource. Neither is ordered against the other to the
        // microsecond (a VM's least of all), so the CPU time can read a few
        // microseconds past the wall time it ran in. A millisecond of slack
        // absorbs that skew and still fails a clock that counts the whole
        // process's time while other threads run, which reads milliseconds
        // high over a 100 ms spin.
        let skew = Duration::from_millis(1);
        assert!(
            spent <= wall + skew,
            "{spent:?} of CPU time in {wall:?} of wall time, beyond {skew:?} of skew between the clocks"
        );
    }
}
