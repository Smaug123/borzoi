//! Stage A smoke test: prove the project-aware FCS oracle harness round-trips a
//! known *cross-file* resolution before any resolver relies on it (the analogue
//! of `uses_smoke.rs` for the single-file harness).
//!
//! No `sema` code is exercised — this is harness-only infrastructure built
//! ahead of its consumer (Stage B, the Compile-order fold). The files use named
//! modules + `open`, which our parser does not fully model yet but FCS resolves
//! fine; the harness is pure FCS, so that is exactly what it must handle.

use crate::common::{invoke_fcs_dump_project, parse_fcs_uses_project, temp_fs_file};

/// A two-file project where `file2` references a `let` declared in `file1`. The
/// harness must report, in `file2`, a non-definition use of that name whose
/// declaration site is the binder back in `file1`.
#[test]
fn cross_file_use_reports_its_declaration_in_the_earlier_file() {
    // file1 declares `foo`; file2 opens file1's module and references `foo`.
    let file1_src = "module Shared\nlet foo = 1\n";
    let file2_src = "module Other\nopen Shared\nlet bar = foo\n";
    let file1 = temp_fs_file("uses_project_1", file1_src);
    let file2 = temp_fs_file("uses_project_2", file2_src);

    // Compile order is load-bearing: file1 before file2.
    let json = invoke_fcs_dump_project(&[&file1, &file2]);

    let sources = vec![
        (file1.clone(), file1_src.to_string()),
        (file2.clone(), file2_src.to_string()),
    ];
    let files = parse_fcs_uses_project(&json, &sources);

    let _ = std::fs::remove_file(&file1);
    let _ = std::fs::remove_file(&file2);

    // The cross-file fact: in file2, the use of `foo` resolves to its binder in
    // file1.
    let f2 = files
        .iter()
        .find(|f| f.path.file_name() == file2.file_name())
        .expect("uses reported for file2");
    let foo_use = f2
        .uses
        .iter()
        .find(|u| u.name == "foo" && !u.is_from_definition)
        .expect("a non-definition use of `foo` in file2");

    let decl = foo_use
        .decl
        .as_ref()
        .expect("`foo`'s declaration is in-project (file1), not None");
    assert_eq!(
        decl.file.file_name(),
        file1.file_name(),
        "the declaration of `foo` is in file1"
    );
    let foo_binder = file1_src.find("foo").expect("foo binder in file1");
    assert_eq!(
        (decl.start, decl.end),
        (foo_binder, foo_binder + "foo".len()),
        "declaration range is `foo`'s binder in file1"
    );

    // And the use itself is the `foo` on file2's `let bar = foo` line.
    let foo_at = file2_src.rfind("foo").expect("foo use in file2");
    assert_eq!(
        (foo_use.start, foo_use.end),
        (foo_at, foo_at + "foo".len()),
        "use range is `foo` in file2"
    );

    // Sanity: file1's own definition of `foo` reports an in-file declaration —
    // proving the same-file case still projects correctly under the project
    // oracle.
    let f1 = files
        .iter()
        .find(|f| f.path.file_name() == file1.file_name())
        .expect("uses reported for file1");
    let foo_def = f1
        .uses
        .iter()
        .find(|u| u.name == "foo" && u.is_from_definition)
        .expect("the defining occurrence of `foo` in file1");
    let def_decl = foo_def.decl.as_ref().expect("definition has a decl site");
    assert_eq!(def_decl.file.file_name(), file1.file_name());
    assert_eq!((def_decl.start, def_decl.end), (foo_binder, foo_binder + 3));
}
