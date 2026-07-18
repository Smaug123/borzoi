//! The **pickled definition range** on module members: the host CCU's
//! signature pickle records each val's `DefinitionRange` (FCS's
//! `TypedTreePickle.fs` `p_ValData` pickles `(val_range, DefinitionRange)`
//! via `p_ranges`), and the member-list cutover carries it onto the claimed
//! [`MethodLike`] as [`MethodLike::definition_range`].
//!
//! Why it exists: an F# module **value** (`let createNull = …`) compiles to a
//! static property whose getter merely reads a backing field — the getter
//! MethodDef carries *no* PDB sequence point (the initialiser lives in the
//! module's `.cctor`), so token-based go-to-definition finds nothing. The
//! pickled range is the only source location for such a member, and it is
//! exactly the one FCS itself navigates to (`Val.DefinitionRange`, "used by
//! Visual Studio"). Verified empirically: all 747 of FSharp.Core's module
//! values lack sequence points.
//!
//! Conventions (pinned by the self-referential fixture oracle below):
//! **1-based lines, 0-based columns**, spanning exactly the binder
//! identifier. The file is the compile-time path — for a deterministic
//! (SourceLink) build it matches the PDB's document names byte-for-byte.
//!
//! Requires the .NET 10 SDK on PATH — the Nix devShell provides it.

use borzoi_assembly::{Ecma335Assembly, EcmaView, Entity, Member, MethodLike};

use crate::common::{ensure_minilib_built, ensure_minilib_fs_built};

fn load_fs() -> Vec<Entity> {
    let dll = ensure_minilib_fs_built();
    let bytes = std::fs::read(dll).expect("read MiniLibFs.dll");
    let view = Ecma335Assembly::parse(&bytes).expect("Ecma335Assembly::parse MiniLibFs");
    view.enumerate_type_defs()
        .expect("enumerate MiniLibFs types")
}

fn hello_method(entities: &[Entity], il_name: &str) -> MethodLike {
    let hello = entities
        .iter()
        .find(|e| e.name == "Hello")
        .expect("MiniLibFs has a Hello module");
    hello
        .members
        .iter()
        .find_map(|m| match m {
            Member::Method(m) if m.name == il_name => Some(m.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("method {il_name:?} not found on Hello"))
}

/// The repo copy of the fixture source — identical content to the tempdir
/// copy the fixture actually builds from, so it can serve as the oracle for
/// the pickled line/column values without hardcoding them.
fn fixture_source() -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/assembly/MiniLibFs/Library.fs");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path:?}: {e}"))
}

/// `(1-based line, 0-based column)` of the first occurrence of `ident`
/// immediately preceded by `let ` (possibly with `mutable`/attribute text in
/// between handled by the caller passing the right `prefix`).
fn binder_position(src: &str, prefix: &str, ident: &str) -> (u32, u32) {
    for (i, line) in src.lines().enumerate() {
        if let Some(p) = line.find(prefix) {
            let col = p + prefix.len();
            // The binder must be followed by a non-identifier char, so
            // `inc` doesn't match a line declaring `incremented`.
            let rest = &line[col..];
            if rest.starts_with(ident)
                && !rest[ident.len()..]
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_alphanumeric() || c == '_')
            {
                return (i as u32 + 1, col as u32);
            }
        }
    }
    panic!("no `{prefix}{ident}` binder in the fixture source");
}

#[test]
fn module_value_carries_the_pickled_definition_range() {
    // `let answer = 42` — a plain module value: its property getter has no
    // sequence point, so this range is go-to-definition's only source.
    let entities = load_fs();
    let m = hello_method(&entities, "answer");
    assert!(m.module_value.is_some(), "answer is a value binding");
    let range = m
        .definition_range
        .expect("a pickled module value carries its definition range");
    assert!(
        range.file.ends_with("Library.fs"),
        "range names the fixture source; got {}",
        range.file
    );

    let (line, col) = binder_position(&fixture_source(), "let ", "answer");
    assert_eq!(
        (range.start_line, range.start_column),
        (line, col),
        "1-based line, 0-based column of the binder identifier"
    );
    assert_eq!(
        (range.end_line, range.end_column),
        (line, col + "answer".len() as u32),
        "the range spans exactly the identifier"
    );
}

#[test]
fn mutable_module_value_carries_the_pickled_definition_range() {
    let entities = load_fs();
    let m = hello_method(&entities, "counter");
    assert!(m.module_value.is_some(), "counter is a (mutable) value");
    let range = m.definition_range.expect("counter carries a range");
    let (line, col) = binder_position(&fixture_source(), "let mutable ", "counter");
    assert_eq!((range.start_line, range.start_column), (line, col));
}

#[test]
fn module_function_carries_the_pickled_definition_range() {
    // Functions have PDB sequence points, so the range is a consistency
    // bonus rather than a necessity — but the pickle records it identically
    // and the claim stamps it uniformly.
    let entities = load_fs();
    let m = hello_method(&entities, "inc");
    assert!(m.module_value.is_none(), "inc is a function");
    let range = m.definition_range.expect("inc carries a range");
    let (line, col) = binder_position(&fixture_source(), "let ", "inc");
    assert_eq!((range.start_line, range.start_column), (line, col));
    assert_eq!(range.end_column, col + "inc".len() as u32);
}

#[test]
fn renamed_function_range_covers_the_source_identifier() {
    // `[<CompiledName("RenamedAtIl")>] let renamed x = …`: the range is the
    // *source* binder `renamed`, not the IL name.
    let entities = load_fs();
    let m = hello_method(&entities, "RenamedAtIl");
    let range = m.definition_range.expect("renamed carries a range");
    let (line, col) = binder_position(&fixture_source(), "let ", "renamed");
    assert_eq!((range.start_line, range.start_column), (line, col));
    assert_eq!(range.end_column, col + "renamed".len() as u32);
}

#[test]
fn csharp_assembly_methods_have_no_definition_range() {
    // A C# assembly has no signature pickle: nothing may invent a range.
    let dll = ensure_minilib_built();
    let bytes = std::fs::read(dll).expect("read MiniLib.dll");
    let view = Ecma335Assembly::parse(&bytes).expect("parse MiniLib");
    let entities = view.enumerate_type_defs().expect("enumerate MiniLib");
    fn sweep(entities: &[Entity]) {
        for e in entities {
            for m in &e.members {
                if let Member::Method(m) = m {
                    assert_eq!(
                        m.definition_range, None,
                        "C# method {}.{} must have no pickled range",
                        e.name, m.name
                    );
                }
            }
            sweep(&e.nested_types);
        }
    }
    sweep(&entities);
}
