//! Serialised, bounded child-process launching.
//!
//! The implementation lives in [`borzoi_spawn`], because the exclusion it
//! provides is only sound if **every** launch in the process shares one critical
//! section — and this process is not the only one that spawns. The LSP library
//! spawns its C# sidecar; the differential-test harnesses that link it spawn
//! `fcs-dump` and the MSBuild/NuGet oracles. A lock per crate would be no lock at
//! all, so the lock is defined once, in that crate, and everything goes through
//! it. See its module docs for the descriptor-leak hazard this closes.
//!
//! This module is the LSP-facing name for it: re-exported rather than reimplemented
//! so existing call sites (and `clippy.toml`'s ban on direct
//! `Command::{spawn,status,output}`) keep working unchanged.

pub use borzoi_spawn::{
    BoundedCommand, ChildFailure, default_timeout, in_thread, output_bounded, output_serialised,
    spawn_serialised, status_serialised,
};
