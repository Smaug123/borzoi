//! A type's documentation comment ID against Roslyn's, over generated
//! metadata.
//!
//! A type's ID is its row's name, except where the name's `` `n `` suffix does
//! not count the type parameters the type introduces: Roslyn then appends the
//! count, and fsc does not
//! ([`borzoi_assembly::doc_id::type_segment`]). No C# compiler writes such a
//! name, so no doc file shipped beside a C# assembly carries one, and the C#
//! differentials never see the rule at work. This one has the oracle emit
//! assemblies whose rows are named from a small alphabet built to collide
//! (two stems; no suffix, or a canonical, non-canonical, empty or non-numeric
//! one), with zero to two parameters of their own, nested or not, repeating
//! their encloser's parameters as metadata does or (malformed) not, and asks
//! Roslyn for `GetDocumentationCommentId` of every type it imports. Each type
//! is paired with ours by its metadata identity — namespace and row names —
//! and must key exactly as Roslyn keys it, in [`DocIdDialect::Metadata`]; the
//! projection must drop none of them, and Roslyn must import every one.
//!
//! fsc's side of the rule is graded against fsc's own doc file by
//! `borzoi-assembly`'s `doc_id_fsharp_diff` (the `DocIdsFs` fixture).

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use borzoi_assembly::doc_id::{DocIdDialect, TypeDocName, type_doc_name};
use borzoi_assembly::{Ecma335Assembly, EcmaView, Entity};
use proptest::prelude::*;
use proptest::test_runner::{Config, FileFailurePersistence, TestRunner};
use serde_json::{Value, json};
use tempfile::TempDir;

use crate::common::inheritdoc_oracle::oracle;

/// Cases in the normal suite; `BORZOI_INHERITDOC_CASES` overrides it.
const DEFAULT_CASES: u32 = 32;

/// Cases in the deep run.
const DEEP_CASES: u32 = 384;

fn cases(default: u32) -> u32 {
    std::env::var("BORZOI_INHERITDOC_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// One generated row.
#[derive(Debug, Clone)]
struct Row {
    /// `stem` + `suffix`: the row's name.
    name: String,
    /// The type parameters it introduces.
    own: usize,
    /// The earlier row it is nested in, by index.
    nested_in: Option<usize>,
    /// Whether its parameter list repeats its encloser's first, as metadata
    /// requires of a nested generic type; when not, the row lists only its
    /// own.
    repeats_encloser: bool,
}

fn row() -> impl Strategy<Value = (String, usize, Option<prop::sample::Index>, bool)> {
    (
        (
            prop::sample::select(vec!["U", "V"]),
            prop::sample::select(vec!["", "`0", "`1", "`2", "`01", "`", "`x"]),
        )
            .prop_map(|(stem, suffix)| format!("{stem}{suffix}")),
        0usize..3,
        prop::option::weighted(0.4, any::<prop::sample::Index>()),
        prop::bool::weighted(0.85),
    )
}

/// A world: rows in order, each nested only in an earlier one, and no two
/// siblings (or two top-level rows) sharing a name.
fn world() -> impl Strategy<Value = Vec<Row>> {
    prop::collection::vec(row(), 1..10).prop_map(|raw| {
        let mut rows: Vec<Row> = Vec::new();
        let mut taken: BTreeSet<(Option<usize>, String)> = BTreeSet::new();
        for (name, own, nest, repeats_encloser) in raw {
            let nested_in = match nest {
                Some(ix) if !rows.is_empty() => Some(ix.index(rows.len())),
                _ => None,
            };
            if taken.insert((nested_in, name.clone())) {
                rows.push(Row {
                    name,
                    own,
                    nested_in,
                    repeats_encloser,
                });
            }
        }
        rows
    })
}

/// Each row's full generic-parameter list, as emitted.
fn parameters(rows: &[Row]) -> Vec<Vec<String>> {
    let mut lists: Vec<Vec<String>> = Vec::new();
    for (i, r) in rows.iter().enumerate() {
        let mut list = match r.nested_in {
            Some(outer) if r.repeats_encloser => lists[outer].clone(),
            _ => Vec::new(),
        };
        list.extend((0..r.own).map(|g| format!("T{i}_{g}")));
        lists.push(list);
    }
    lists
}

/// Our ID of every type in the projection, by metadata identity, with any row
/// the projection dropped.
fn ours(dll: &Path) -> (BTreeMap<String, String>, usize) {
    fn walk(
        e: &Entity,
        enclosing: Option<(&TypeDocName, &str)>,
        out: &mut BTreeMap<String, String>,
    ) {
        let row = e.metadata_name.as_deref().expect("a row has its name");
        let path = match enclosing {
            Some((_, outer)) => format!("{outer}/{row}"),
            None => format!("{}|{row}", e.namespace.join(".")),
        };
        let decl = type_doc_name(e, enclosing.map(|(d, _)| d), DocIdDialect::Metadata)
            .expect("a row has a Roslyn ID");
        for nested in &e.nested_types {
            walk(nested, Some((&decl, &path)), out);
        }
        assert!(
            out.insert(path.clone(), decl.type_id()).is_none(),
            "two projected rows at {path}"
        );
    }
    let bytes = std::fs::read(dll).expect("read the emitted assembly");
    let view = Ecma335Assembly::parse(&bytes).expect("parse the emitted assembly");
    let (roots, skips) = view
        .enumerate_type_defs_with_skips()
        .expect("project the emitted assembly");
    let mut out = BTreeMap::new();
    for root in &roots {
        walk(root, None, &mut out);
    }
    (out, skips.dropped_types.len())
}

/// Emit `rows`, and require our ID of each to be Roslyn's; Roslyn's IDs.
fn run(rows: &[Row]) -> BTreeMap<String, String> {
    let dir = TempDir::new().unwrap();
    let dll = dir.path().join("TypeIds.dll");
    let lists = parameters(rows);
    let types: Vec<Value> = rows
        .iter()
        .zip(&lists)
        .map(|(r, generic)| {
            let mut t = json!({
                "ns": if r.nested_in.is_some() { "" } else { "N" },
                "name": r.name,
                "kind": "class",
                "base": null,
                "interfaces": [],
                "methods": [],
                "properties": [],
                "generic": generic,
            });
            if let Some(outer) = r.nested_in {
                t["nestedIn"] = json!(outer);
            }
            t
        })
        .collect();
    oracle().lock().unwrap().emit(json!({
        "path": dll,
        "assembly": {"name": "TypeIds", "version": "1.0.0.0", "culture": null},
        "refs": [],
        "types": types,
    }));
    let mut roslyn = oracle()
        .lock()
        .unwrap()
        .type_ids(std::slice::from_ref(&dll), &dll);
    // Roslyn imports the module's `<Module>` row as a type; the projection
    // does not, and no documentation is keyed by it.
    assert_eq!(
        roslyn.remove("|<Module>").as_deref(),
        Some("T:<Module>"),
        "Roslyn imports `<Module>`"
    );
    let (ours, dropped) = ours(&dll);
    assert_eq!(dropped, 0, "{rows:#?}: the projection dropped a row");
    assert_eq!(
        roslyn.len(),
        rows.len(),
        "{rows:#?}: Roslyn imported {roslyn:#?}"
    );
    assert_eq!(ours, roslyn, "{rows:#?}");
    roslyn
}

fn check(cases: u32) {
    let config = Config {
        cases,
        failure_persistence: Some(Box::new(FileFailurePersistence::SourceParallel(
            "proptest-regressions",
        ))),
        ..Config::default()
    };
    let mut runner = TestRunner::new(config);
    if let Err(e) = runner.run(&world(), |rows| {
        run(&rows);
        Ok(())
    }) {
        panic!("{e}");
    }
}

#[test]
fn type_ids_key_as_roslyn_keys_them() {
    check(cases(DEFAULT_CASES));
}

#[test]
#[ignore = "deep run: CI's <inheritdoc> sweep step"]
fn deep_type_ids_key_as_roslyn_keys_them() {
    check(cases(DEEP_CASES));
}

/// The shapes the rule turns on, each pinned to Roslyn's own answer, so a
/// generator that stopped reaching them would not leave them ungraded.
#[test]
fn the_suffix_cases_key_as_roslyn_keys_them() {
    let row = |name: &str, own, nested_in| Row {
        name: name.to_string(),
        own,
        nested_in,
        repeats_encloser: true,
    };
    let rows = vec![
        row("U`2", 1, None),
        row("U", 1, None),
        row("U`1", 1, None),
        row("V`3", 0, None),
        row("V`0", 0, None),
        row("V`01", 1, None),
        row("U`1", 1, Some(0)),
        row("V", 0, Some(1)),
        // Malformed: `X` lists none of `W`1`'s parameter, and `Y` one, which
        // Roslyn reads as `W`1`'s inherited through `X`.
        row("W`1", 1, None),
        Row {
            repeats_encloser: false,
            ..row("X", 0, Some(8))
        },
        row("Y", 1, Some(9)),
    ];
    let roslyn = run(&rows);
    let expected = [
        // The suffix counts the wrong number: kept, and the count appended.
        ("N|U`2", "T:N.U`2`1"),
        // No suffix on a generic type: the count appended.
        ("N|U", "T:N.U`1"),
        // So Roslyn keys these two alike; a consumer counts keys for that.
        ("N|U`1", "T:N.U`1"),
        // No parameters: the name verbatim, whatever its suffix.
        ("N|V`3", "T:N.V`3"),
        ("N|V`0", "T:N.V`0"),
        // A non-canonical suffix counts nothing.
        ("N|V`01", "T:N.V`01`1"),
        // Nested: the encloser's ID, then the type's own parameters.
        ("N|U`2/U`1", "T:N.U`2`1.U`1"),
        ("N|U/V", "T:N.U`1.V"),
        // A nested type inherits at least its encloser's count, however few
        // parameters its own row lists.
        ("N|W`1/X", "T:N.W`1.X"),
        ("N|W`1/X/Y", "T:N.W`1.X.Y"),
    ];
    for (path, id) in expected {
        assert_eq!(roslyn.get(path).map(String::as_str), Some(id), "{path}");
    }
}
