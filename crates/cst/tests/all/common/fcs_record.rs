//! Reading an `fcs-dump ast` record's tree, at any depth fcs-dump emits.
//!
//! The record is AdjacentTag JSON: a DU case is `{"Case": …, "Fields": [ … ]}`,
//! so each level of FCS's tree costs about three levels of JSON. `serde_json`
//! refuses input nested deeper than 128 levels by default, which is only some
//! forty AST nodes: a function body of forty statements (a right-nested
//! `Sequential` chain) is already past it. Every FCS-side reader goes through
//! [`read_fcs_record`], which lifts that limit.
//!
//! The limit guards the stack. Reading a `Value`, projecting it and dropping
//! it all recurse once per JSON level, and so does our own side of each
//! comparison once the file is read. What bounds the depth instead is
//! fcs-dump: it serialises with `MaxDepth = 4096` and answers a deeper tree
//! with an `Error` record rather than a `ParseTree`, so no record it emits is
//! deeper than [`FCS_DUMP_MAX_DEPTH`]. A thread from [`on_deep_stack`] or
//! [`spawn_deep`] has [`DEEP_STACK_BYTES`], which
//! `fcs_record_stack_bound` holds every reader to (with a margin of two) on
//! the deepest record fcs-dump emits for each repetition form of the grammar.
//!
//! A record no deeper than [`SHALLOW_DEPTH`] reads on any thread, as it always
//! has; a deeper one read anywhere but a deep-stack thread panics, naming the
//! fix, rather than risk an overflow, which would abort the test binary.

use std::cell::Cell;

use serde::Deserialize;
use serde_json::Value;

/// The deepest JSON nesting, counted by [`json_depth`], of a record fcs-dump
/// emits: its serialiser's `MaxDepth` (`tools/fcs-dump/Program.fs`).
/// `System.Text.Json` holds both its writer's open containers and its
/// serialiser's nesting of values to that, and the second counts a primitive
/// leaf too, so a record whose deepest container holds a value stops one
/// short. `fcs_record_stack_bound` holds fcs-dump to the bound, and checks
/// that every tree it refused would have passed it.
pub const FCS_DUMP_MAX_DEPTH: usize = 4096;

/// `serde_json`'s default recursion limit. A record this shallow is read on
/// whatever thread asks.
pub const SHALLOW_DEPTH: usize = 128;

/// The stack of a thread that reads records of any depth fcs-dump emits, and
/// runs both sides of a comparison over them.
pub const DEEP_STACK_BYTES: usize = 256 << 20;

thread_local! {
    /// Whether this thread was started with [`DEEP_STACK_BYTES`].
    static ON_DEEP_STACK: Cell<bool> = const { Cell::new(false) };
}

/// The deepest nesting of `[`/`{` in `json`, the outermost counting as 1.
/// Iterative, so measuring a deep record cannot overflow.
pub fn json_depth(json: &str) -> usize {
    let (mut depth, mut deepest) = (0usize, 0usize);
    let (mut in_string, mut escaped) = (false, false);
    for b in json.bytes() {
        if in_string {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_string = false;
            }
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'[' | b'{' => {
                depth += 1;
                deepest = deepest.max(depth);
            }
            b']' | b'}' => depth -= 1,
            _ => {}
        }
    }
    deepest
}

/// The whole `fcs-dump` record `json`, read with no recursion limit.
///
/// # Panics
///
/// Panics if `json` is not JSON, if it is deeper than fcs-dump emits, or if
/// it is deeper than [`SHALLOW_DEPTH`] and this thread is not one from
/// [`on_deep_stack`] or [`spawn_deep`].
pub fn read_fcs_record(json: &str) -> Value {
    let depth = json_depth(json);
    assert!(
        depth <= FCS_DUMP_MAX_DEPTH,
        "fcs-dump record nested {depth} deep, past the {FCS_DUMP_MAX_DEPTH} fcs-dump emits"
    );
    assert!(
        depth <= SHALLOW_DEPTH || ON_DEEP_STACK.with(Cell::get),
        "fcs-dump record nested {depth} deep, read on a thread without the deep \
         stack: run the caller in `common::fcs_record::on_deep_stack` (a sweep's \
         `par_map` workers already are)"
    );
    let mut de = serde_json::Deserializer::from_str(json);
    de.disable_recursion_limit();
    let value = Value::deserialize(&mut de).expect("fcs-dump record is JSON");
    de.end().expect("fcs-dump record is one JSON value");
    value
}

/// Spawn `f` on a [`DEEP_STACK_BYTES`] thread of `scope`.
pub fn spawn_deep<'scope, 'env, T: Send + 'scope>(
    scope: &'scope std::thread::Scope<'scope, 'env>,
    f: impl FnOnce() -> T + Send + 'scope,
) -> std::thread::ScopedJoinHandle<'scope, T> {
    spawn_marked_deep(scope, DEEP_STACK_BYTES, f)
}

fn spawn_marked_deep<'scope, 'env, T: Send + 'scope>(
    scope: &'scope std::thread::Scope<'scope, 'env>,
    stack_bytes: usize,
    f: impl FnOnce() -> T + Send + 'scope,
) -> std::thread::ScopedJoinHandle<'scope, T> {
    std::thread::Builder::new()
        .name("deep stack".into())
        .stack_size(stack_bytes)
        .spawn_scoped(scope, move || {
            ON_DEEP_STACK.with(|d| d.set(true));
            f()
        })
        .expect("spawn a deep-stack thread")
}

/// Run `f` on a fresh [`DEEP_STACK_BYTES`] thread, returning its result or
/// re-raising its panic.
pub fn on_deep_stack<T: Send>(f: impl FnOnce() -> T + Send) -> T {
    on_stack_marked_deep(DEEP_STACK_BYTES, f)
}

/// [`on_deep_stack`] with `stack_bytes` in place of [`DEEP_STACK_BYTES`]:
/// for `fcs_record_stack_bound`, which holds the readers to a fraction of it.
pub fn on_stack_marked_deep<T: Send>(stack_bytes: usize, f: impl FnOnce() -> T + Send) -> T {
    std::thread::scope(|scope| {
        spawn_marked_deep(scope, stack_bytes, f)
            .join()
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
    })
}

#[test]
fn json_depth_counts_brackets_outside_strings() {
    assert_eq!(json_depth("1"), 0);
    assert_eq!(json_depth("{}"), 1);
    assert_eq!(json_depth(r#"{"a":[[1],{"b":2}]}"#), 3);
    assert_eq!(json_depth(r#"{"a":"[[[{\"]]"}"#), 1);
    assert_eq!(json_depth(r#"["\\",[1]]"#), 2);
}

#[test]
fn a_deep_record_reads_only_on_a_deep_stack() {
    let deep = format!(
        "{}{}",
        "[".repeat(SHALLOW_DEPTH + 1),
        "]".repeat(SHALLOW_DEPTH + 1)
    );
    let shallow = format!("{}{}", "[".repeat(SHALLOW_DEPTH), "]".repeat(SHALLOW_DEPTH));
    read_fcs_record(&shallow);
    assert!(super::catch_unwind_silent(|| read_fcs_record(&deep)).is_err());
    on_deep_stack(|| read_fcs_record(&deep));
}
