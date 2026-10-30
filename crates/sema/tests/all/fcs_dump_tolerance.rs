//! The `types` and `binder-types` oracles answer for every file the inference
//! corpus sweep sends them, and say exactly what they could not report.
//!
//! # Unconverted declarations
//!
//! FCS converts its typed tree to the public expression view lazily, one
//! binding body at a time, re-solving each call's witnesses as it goes. A call
//! whose type arguments violate a constraint throws there, so before fcs-dump
//! caught it per declaration a single `g = g` sank the whole file. These pin
//! the three shapes that did: a function compared directly, a function compared
//! through an in-file generic, and a `[<NoEquality>]` record compared. Each
//! must come back with FCS's FS0001, the offending declaration listed as
//! unconverted, and every other declaration still reported.
//!
//! # Measure abbreviations
//!
//! FCS reports a unit-of-measure abbreviation (`[<Measure>] type sqrm = m^2`)
//! as an abbreviation whose `AbbreviatedType` is itself, so a renderer that
//! chases abbreviations to a fixpoint never returns on one. A file that holds a
//! `float<sqrm>` must still be answered.

use crate::common::{
    FcsTypedDump, FcsUnconverted, invoke_fcs_dump, parse_fcs_binder_types_dump,
    parse_fcs_types_dump, temp_fs_file,
};

/// Each case's source, in which `bad` violates a constraint and `ok` is clean.
const CASES: [(&str, &str); 3] = [
    (
        "function_compared",
        "module M\nlet g (x: int) = x + 1\nlet bad = g = g\nlet ok = 1\n",
    ),
    (
        "function_through_generic",
        "module M\nlet eq x y = x = y\nlet g (x: int) = x\nlet bad = eq g g\nlet ok = 1\n",
    ),
    (
        "no_equality_record",
        "module M\n[<NoEquality; NoComparison>]\ntype R = { X: int }\nlet r = { X = 1 }\nlet bad = r = r\nlet ok = 1\n",
    ),
];

fn dumps(label: &str, src: &str) -> (FcsTypedDump, FcsTypedDump) {
    let path = temp_fs_file(label, src);
    let types = parse_fcs_types_dump(&invoke_fcs_dump("types", &path), src);
    let binders = parse_fcs_binder_types_dump(&invoke_fcs_dump("binder-types", &path), src);
    let _ = std::fs::remove_file(&path);
    (types, binders)
}

#[test]
fn a_constraint_violation_leaves_one_declaration_unconverted() {
    for (label, src) in CASES {
        let bad_line = src
            .lines()
            .position(|l| l.starts_with("let bad"))
            .expect("a `bad` line") as u32
            + 1;
        let bad = src.find("let bad").expect("bad") + 4;
        let bad_end = src.find("\nlet ok").expect("ok");
        let ok = bad_end + 5;
        let (types, binders) = dumps(label, src);
        for (op, dump) in [("types", &types), ("binder-types", &binders)] {
            assert!(
                dump.errors
                    .iter()
                    .any(|e| e.code == 1 && e.line == bad_line),
                "{label} {op}: FS0001 on line {bad_line}: {:#?}",
                dump.errors
            );
            match dump.unconverted.as_slice() {
                [
                    FcsUnconverted::At {
                        start, end, name, ..
                    },
                ] => {
                    assert_eq!(name, "bad", "{label} {op}");
                    assert!(
                        *start <= bad && bad + 3 <= *end && *end <= bad_end,
                        "{label} {op}: the range covers `bad` and no further: {start}..{end}"
                    );
                }
                other => panic!("{label} {op}: one located `bad`, got {other:#?}"),
            }
            // Nothing at all from inside `bad`.
            let inside: Vec<_> = dump
                .types
                .keys()
                .filter(|(s, e)| *s >= bad - 4 && *e <= bad_end)
                .collect();
            assert!(inside.is_empty(), "{label} {op}: {inside:?}");
        }
        assert_eq!(
            types.unconverted, binders.unconverted,
            "{label}: the ops agree"
        );
        // The rest of the file is still reported: `ok`'s binder and its `1`.
        assert_eq!(
            binders.types.get(&(ok, ok + 2)).map(String::as_str),
            Some("System.Int32"),
            "{label}"
        );
        assert_eq!(
            types.types.get(&(ok + 5, ok + 6)).map(String::as_str),
            Some("System.Int32"),
            "{label}"
        );
    }
}

/// A caller that reads a dump as complete refuses one with an unconverted
/// declaration rather than take its missing nodes as FCS's answer.
#[test]
#[should_panic(expected = "could not convert 1 declaration")]
fn a_complete_reading_refuses_an_unconverted_declaration() {
    let (label, src) = CASES[0];
    let path = temp_fs_file(label, src);
    let json = invoke_fcs_dump("types", &path);
    let _ = std::fs::remove_file(&path);
    let _ = crate::common::parse_fcs_types(&json, src);
}

/// A value typed through a unit-of-measure abbreviation is answered, not hung
/// on, by both ops.
#[test]
fn a_measure_abbreviation_is_rendered() {
    let src = "module M\n[<Measure>] type m\n[<Measure>] type sqrm = m^2\nlet a = 2.0<sqrm>\nlet b = [2.0<sqrm>]\nlet ok = 1\n";
    let (types, binders) = dumps("measure_abbreviation", src);
    let ok = src.find("let ok").expect("ok") + 4;
    assert!(
        types.errors.is_empty() && types.unconverted.is_empty(),
        "{types:#?}"
    );
    for name in ["a", "b", "ok"] {
        let at = src.find(&format!("let {name} ")).expect("binder") + 4;
        assert!(
            binders.types.contains_key(&(at, at + name.len())),
            "`{name}` is reported: {binders:#?}"
        );
    }
    assert_eq!(
        types.types.get(&(ok + 5, ok + 6)).map(String::as_str),
        Some("System.Int32")
    );
}
