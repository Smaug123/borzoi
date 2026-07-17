# Bounded, serialised child processes

Everything in this repo that shells out — the LSP spawning its C# sidecar, the
differential tests driving `fcs-dump`, `msbuild-condition-oracle` and
`nuget-oracle`, the fixture `dotnet build`s — is a child process we write to and
read from. Two hazards live there, and both are *process-global*, which is why
they belong in a crate rather than in a helper per caller. `crates/spawn`
(`borzoi-spawn`) holds the lock and `BoundedCommand`; `crates/oracle-harness`
holds `BatchChild`; `crates/lsp/src/spawn.rs` re-exports the spawn crate rather
than reimplementing it. (Descriptor leak diagnosed in #910.)

## The two hazards

**The descriptor leak.** On macOS there is no `pipe2`, so `std::process` creates
each stdio pipe with `pipe()` and flips `FD_CLOEXEC` only afterwards. A spawn on
another thread inside that window inherits the raw descriptors. A long-lived
inheritor — the sidecar, a resident oracle, an MSBuild node-reuse worker — then
holds a duplicate of a short-lived child's stdout write end for its own lifetime,
and the `read()` draining that child never sees EOF: the call hangs forever *even
though the child exited*. The trap is that the child is not wedged — it has
already exited — so bounded waits alone make the hang *sayable* but never fix it;
only one shared critical section around every spawn does.

**The unbounded wait.** A child that genuinely never answers (a wedged FCS, a
`dotnet` on a broken toolchain — both observed) turns a suite or an LSP request
into a hang. That is strictly worse than a failure: a run that stops is
diagnosable, one that hangs is not. Bounding does not make a wedged oracle
succeed; it makes the failure *sayable*.

## Why a crate

The exclusion the spawn lock provides is only sound if **every** launch in the
process shares **one** critical section. Two locks are no lock. So the lock is
defined once, in `crates/spawn`, and everything spawns through it: the LSP library
(whose `spawn.rs` is a re-export), the test harnesses, the oracle drivers.
`clippy.toml` bans direct `Command::{spawn,status,output}` to enforce it
mechanically.

## The primitives

`crates/spawn` — **`BoundedCommand`** (with the `output_bounded` `io::Result` face
over it). A one-shot child, spawned under the lock, run to a deadline, with both
output pipes drained on threads and stdin streamed from a third. The streamed
stdin matters: writing stdin synchronously with the output pipes undrained
deadlocks once the child fills its stdout buffer and stops reading — a bug that
only surfaces on large input (fine at 1k paths, hangs at 200k). `BoundedCommand`
also surfaces *undelivered* input: a child that stops reading and exits 0 has
answered only a *prefix* of the question, so returning that `Output` would be a
silent truncation, and is instead a failure.

`crates/oracle-harness` — **`BatchChild`**. Only what is specific to a *resident*
oracle: the lock-step request/response loop (the oracles cost ~150–300 ms of .NET
+ FCS startup and the per-case tests make thousands of calls, so they are kept
resident), and recovery when one wedges mid-conversation — kill, reap, respawn,
retry, then panic. Both the read *and* the write are bounded: a request larger
than the pipe buffer, sent to an oracle that has stopped reading, blocks in the
write itself, upstream of the response deadline where no retry logic can see it.

## The deadline must cover the whole wait

The invariant to check in any similar code: **a bound that doesn't cover the whole
wait isn't a bound.** The deadline must cover not just the child's exit but the
collection of its output — a child can exit while a descendant holds the pipes
(`dotnet build` leaves MSBuild workers behind), so a naive join afterwards blocks
forever — and it must cover the batch *write* as well as the batch *read*. It must
also be *scaled to the work*: a budget sized for one snippet that kills a *healthy*
whole-project `uses-project` run is as bad as no bound, because the caller cannot
tell it from a real timeout, so whole-project runs get a project-scale budget.

## What this does not do

It does not make a wedged oracle succeed. `BatchChild` retries once on a fresh
child because that empirically clears it; otherwise the run fails, loudly.

It does not bound `crates/lsp/build.rs`. Its two `dotnet` calls carry a deliberate
`#[allow(clippy::disallowed_methods)]`: a build script is provably single-threaded,
so the descriptor race cannot arise there, and an unbounded wait hangs `cargo
build` rather than a test suite. Left as-is, on purpose.
