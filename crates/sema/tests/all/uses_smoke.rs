//! End-to-end smoke test for the `fcs-dump uses` oracle harness.
//!
//! Proves the harness round-trips a known case before the Stage C name
//! resolver relies on it: for `let x = 1` followed by a bare `x`, FCS must
//! report exactly two uses of `x` — the defining occurrence at the binder and
//! a non-defining reference — both with declaration range pointing back at the
//! binder. (FCS also reports the implicit module symbol derived from the file
//! name; that is noise for this test, so we filter to the symbol under test.)

use std::io::Write;

use crate::common::{NormalisedUse, invoke_fcs_dump, parse_fcs_uses};

/// Write `source` to a uniquely-named temp `.fs` file so parallel test
/// binaries don't collide, returning the path. The file is intentionally left
/// on disk for the duration of the `fcs-dump` child process.
fn temp_fs_file(source: &str) -> std::path::PathBuf {
    let path =
        std::env::temp_dir().join(format!("borzoi_sema_uses_smoke_{}.fs", std::process::id()));
    let mut f = std::fs::File::create(&path).expect("create temp .fs");
    f.write_all(source.as_bytes()).expect("write temp .fs");
    path
}

#[test]
fn uses_round_trips_a_let_binding_and_its_reference() {
    let source = "let x = 1\nx\n";
    let path = temp_fs_file(source);

    let json = invoke_fcs_dump("uses", &path);
    let _ = std::fs::remove_file(&path);

    let uses = parse_fcs_uses(&json, source);

    // Restrict to the symbol under test; FCS also emits the implicit module.
    let x_uses: Vec<&NormalisedUse> = uses.iter().filter(|u| u.name == "x").collect();

    // Every range (and in-file declaration) must slice back to the name `x`.
    for u in &x_uses {
        assert_eq!(&source[u.start..u.end], "x", "use range slices to its name");
        if let Some((ds, de)) = u.decl {
            assert_eq!(&source[ds..de], "x", "decl range slices to the binder name");
        }
    }

    let defs: Vec<&&NormalisedUse> = x_uses.iter().filter(|u| u.is_from_definition).collect();
    let refs: Vec<&&NormalisedUse> = x_uses.iter().filter(|u| !u.is_from_definition).collect();

    assert_eq!(defs.len(), 1, "exactly one defining use of x: {x_uses:?}");
    assert_eq!(
        refs.len(),
        1,
        "exactly one referencing use of x: {x_uses:?}"
    );

    // The defining use is the binder `x` at byte 4..5; its declaration points
    // at itself.
    let def = defs[0];
    assert_eq!((def.start, def.end), (4, 5));
    assert_eq!(def.decl, Some((4, 5)));

    // The reference is the bare `x` on line 2 at byte 10..11; its declaration
    // points back at the binder.
    let r = refs[0];
    assert_eq!((r.start, r.end), (10, 11));
    assert_eq!(r.decl, Some((4, 5)));
}
