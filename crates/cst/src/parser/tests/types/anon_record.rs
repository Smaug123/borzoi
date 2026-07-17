//! Anonymous record types (`{| F : int |}`).
//!
//! Extracted verbatim from the former flat `types.rs` (one submodule per
//! `parse_type` grammar form).

use super::super::super::*; // parser internals under test
use super::super::*; // shared tree-rendering test helpers (see `tests/mod.rs`)

/// Phase 7.9 — `(x : {| F : int |})`: the reference anon-record
/// type with a single field. Pins the shape
/// `ANON_RECD_TYPE > [LBRACE_BAR_TOK,
/// ANON_RECD_TYPE_FIELD > [IDENT(F), COLON_TOK, LongIdent(int)],
/// BAR_RBRACE_TOK]` — no leading STRUCT_TOK, no SEMI_TOK separator,
/// projects to `AnonRecdType::is_struct() = false`.
#[test]
fn anon_recd_type_single_field() {
    use crate::syntax::{AnonRecdType, AstNode, Type};
    let source = "(x : {| F : int |})\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "got errors: {:?}", parse.errors);
    let anon_node = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::ANON_RECD_TYPE)
        .expect("ANON_RECD_TYPE present for `{| F : int |}`");
    let anon = AnonRecdType::cast(anon_node).expect("ANON_RECD_TYPE casts");
    assert!(!anon.is_struct(), "reference variant: is_struct = false");
    let fields: Vec<_> = anon.fields().collect();
    assert_eq!(
        fields.len(),
        1,
        "expected one field; got tree:\n{}",
        debug_tree(&parse.root),
    );
    let f = &fields[0];
    assert_eq!(
        f.ident().expect("field ident").text(),
        "F",
        "field name; got tree:\n{}",
        debug_tree(&parse.root),
    );
    assert!(matches!(f.ty().expect("field type"), Type::LongIdent(_)));
    assert_lossless(source, &parse);
}

/// Phase 7.9 — `(x : {| F : int; G : string |})`: two
/// `;`-separated fields. The trailing `;` is *not* present here;
/// `parse_anon_recd_type`'s `Semi` loop reads zero-or-more
/// continuations and the close `|}` exits the loop.
#[test]
fn anon_recd_type_multiple_fields() {
    use crate::syntax::{AnonRecdType, AstNode};
    let source = "(x : {| F : int; G : string |})\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "got errors: {:?}", parse.errors);
    let anon_node = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::ANON_RECD_TYPE)
        .expect("ANON_RECD_TYPE present");
    let anon = AnonRecdType::cast(anon_node).expect("ANON_RECD_TYPE casts");
    let names: Vec<_> = anon
        .fields()
        .map(|f| f.ident().expect("field ident").text().to_string())
        .collect();
    assert_eq!(
        names,
        vec!["F".to_string(), "G".to_string()],
        "expected fields F then G; got tree:\n{}",
        debug_tree(&parse.root),
    );
    // A SEMI_TOK must sit between the two ANON_RECD_TYPE_FIELDs
    // (sibling of the outer ANON_RECD_TYPE node), not inside one
    // of them.
    let outer_kids: Vec<_> = anon
        .syntax()
        .children_with_tokens()
        .filter(|el| !el.kind().is_trivia())
        .map(|el| el.kind())
        .collect();
    assert!(
        outer_kids.contains(&SyntaxKind::SEMI_TOK),
        "expected a SEMI_TOK as a direct child of ANON_RECD_TYPE; got kids {outer_kids:?}\ntree:\n{}",
        debug_tree(&parse.root),
    );
    assert_lossless(source, &parse);
}

/// Phase 7.9 — `(x : struct {| F : int |})`: the struct variant.
/// FCS's `anonRecdType: STRUCT braceBarFieldDeclListCore`
/// (`pars.fsy:2510-2513`). Pins (a) leading STRUCT_TOK as a direct
/// child of ANON_RECD_TYPE, (b) `is_struct() = true`.
#[test]
fn anon_recd_type_struct_variant() {
    use crate::syntax::{AnonRecdType, AstNode};
    let source = "(x : struct {| F : int |})\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "got errors: {:?}", parse.errors);
    let anon_node = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::ANON_RECD_TYPE)
        .expect("ANON_RECD_TYPE present for struct variant");
    let anon = AnonRecdType::cast(anon_node).expect("ANON_RECD_TYPE casts");
    assert!(anon.is_struct(), "struct variant: is_struct = true");
    // STRUCT_TOK must precede LBRACE_BAR_TOK among the direct
    // token children.
    let toks: Vec<_> = anon
        .syntax()
        .children_with_tokens()
        .filter_map(|el| el.into_token())
        .filter(|t| !t.kind().is_trivia())
        .map(|t| t.kind())
        .collect();
    let struct_idx = toks.iter().position(|k| *k == SyntaxKind::STRUCT_TOK);
    let lbb_idx = toks.iter().position(|k| *k == SyntaxKind::LBRACE_BAR_TOK);
    assert!(
        struct_idx.is_some() && lbb_idx.is_some() && struct_idx < lbb_idx,
        "expected STRUCT_TOK before LBRACE_BAR_TOK; got toks {toks:?}\ntree:\n{}",
        debug_tree(&parse.root),
    );
    assert_lossless(source, &parse);
}

/// Phase 7.9 — `(x : {| F : int -> int |})`: the inner field type
/// is a full `typ` (FCS uses unrestricted `typ` in `recdFieldDecl`),
/// so a `Fun` type must project. Pins that
/// `parse_anon_recd_type_field` calls `parse_type`, not
/// `parse_atomic_type` / `parse_app_type`.
#[test]
fn anon_recd_type_inner_function_type() {
    use crate::syntax::{AnonRecdType, AstNode, Type};
    let source = "(x : {| F : int -> int |})\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "got errors: {:?}", parse.errors);
    let anon_node = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::ANON_RECD_TYPE)
        .expect("ANON_RECD_TYPE present");
    let anon = AnonRecdType::cast(anon_node).expect("ANON_RECD_TYPE casts");
    let field = anon.fields().next().expect("first field");
    match field.ty().expect("field ty") {
        Type::Fun(_) => {}
        other => panic!(
            "field type must be Fun(_); got {other:?}\ntree:\n{}",
            debug_tree(&parse.root)
        ),
    }
    assert_lossless(source, &parse);
}

/// Phase 7.9 — `(x : {| F : int |} list)`: anon-recd under a
/// postfix application. The dispatch in `parse_app_type` happens
/// after the shared checkpoint `cp`, so the postfix loop wraps the
/// anon-recd as the sole arg of `App(list, …, postfix)`. Same
/// layering FCS uses (`appType: atomTypeOrAnonRecdType (postfix)+`,
/// `pars.fsy:6378`).
#[test]
fn anon_recd_type_under_postfix_app() {
    use crate::syntax::{AppType, AstNode, Type};
    let source = "(x : {| F : int |} list)\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "got errors: {:?}", parse.errors);
    let app_node = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::APP_TYPE)
        .expect("APP_TYPE present for `{| … |} list`");
    let app = AppType::cast(app_node).expect("APP_TYPE casts");
    assert!(app.is_postfix(), "outer App must be postfix `T list`");
    let args = app.type_args();
    assert_eq!(args.len(), 1);
    assert!(
        matches!(args[0], Type::AnonRecd(_)),
        "App's sole arg must be AnonRecd(_); got tree:\n{}",
        debug_tree(&parse.root),
    );
    assert_lossless(source, &parse);
}

/// Phase 7.9 — `(x : {| F : int |}[])`: anon-recd under an array
/// suffix. Same `parse_app_type` checkpoint mechanic as the
/// postfix-app case. Green shape:
/// `Array(rank=1, AnonRecd { fields = [(F, int)] })`.
#[test]
fn anon_recd_type_under_array() {
    use crate::syntax::{ArrayType, AstNode, Type};
    let source = "(x : {| F : int |}[])\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "got errors: {:?}", parse.errors);
    let array_node = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::ARRAY_TYPE)
        .expect("ARRAY_TYPE present for `{| … |}[]`");
    let array = ArrayType::cast(array_node).expect("ARRAY_TYPE casts");
    assert_eq!(array.rank(), 1);
    let element = array.element_type().expect("element type");
    assert!(
        matches!(element, Type::AnonRecd(_)),
        "Array element must be AnonRecd(_); got tree:\n{}",
        debug_tree(&parse.root),
    );
    assert_lossless(source, &parse);
}

/// Phase 7.9 regression — a `struct` at a type position that opens
/// *neither* an anon record (`struct {|`) *nor* a struct tuple
/// (`struct (`) must recover (push an "expected type" error) rather
/// than panic. The earlier free predicate `raw_starts_anon_recd_type`
/// accepted every `struct` token unconditionally, which made the
/// three recovery gates (in `parse_type`, `parse_tuple_type` after
/// `*`, `parse_atomic_type`'s LPAREN body) admit a bare `struct`,
/// then the dispatch in `parse_app_type` failed to find a matching
/// arm and tripped `parse_atomic_type`'s `unreachable!`. The fix is
/// to gate on the parser-level `peek_starts_type_or_anon_recd`, which
/// performs the two-token `struct → {|` / `struct → (` lookahead.
/// Pins both that the parse does not panic and that the bare `struct`
/// (here followed by an ident, or end-of-annotation) does not
/// silently get absorbed into the type tree as a real type starter.
/// (`struct (T * U)` *is* a valid struct-tuple type — see
/// `function_tuple::struct_tuple_type_shape`.)
#[test]
fn anon_recd_type_bare_struct_does_not_panic() {
    for source in ["(x : struct foo)\n", "let f (x : struct) = x\n"] {
        let parse = parse(source);
        // The recovery error fires at the `struct` position. Every
        // input must produce *some* parse error (the bare `struct`
        // is a rejected type-position surface), and the
        // `ANON_RECD_TYPE` node must not be present (no `{|`
        // follows the `struct`).
        assert!(
            !parse.errors.is_empty(),
            "expected at least one parse error for `{source}` — bare `struct` is not a type starter; got tree:\n{}",
            debug_tree(&parse.root),
        );
        assert!(
            !parse
                .root
                .descendants()
                .any(|n| n.kind() == SyntaxKind::ANON_RECD_TYPE),
            "no ANON_RECD_TYPE node should appear for `{source}` (no `{{|` after `struct`); got tree:\n{}",
            debug_tree(&parse.root),
        );
        assert_lossless(source, &parse);
    }
}

/// Phase 7.9 regression — `let f (x : {| F :\n              G : string |}) = x`:
/// missing field type immediately before a layout break. LexFilter
/// emits a `Virtual::BlockSep` between F's colon and `G`; without
/// rejecting that virtual at the recovery gate,
/// `parse_app_type` → `parse_atomic_type` would dispatch (because
/// the raw lookahead past the virtual sees `G` as a valid type
/// starter) and `parse_atomic_type` would then panic at its
/// `unreachable!` arm with the virtual still at the filtered
/// cursor. The fix is to short-circuit `peek_starts_type_or_anon_recd`
/// when the filtered peek is a layout virtual, so the gate fires
/// "expected type" and recovery picks up cleanly with G as the
/// next field.
#[test]
fn anon_recd_type_missing_field_type_before_block_sep_does_not_panic() {
    let source = "let f (x : {| F :\n              G : string |}) = x\n";
    let parse = parse(source);
    // The bug was a panic; the requirement is that the parser
    // produces a recoverable "expected type" error and continues
    // parsing G as a second field.
    assert!(
        parse
            .errors
            .iter()
            .any(|e| e.message.contains("expected type")),
        "expected an `expected type` error for missing field type before BlockSep; got errors {:?}\ntree:\n{}",
        parse.errors,
        debug_tree(&parse.root),
    );
    // Two `ANON_RECD_TYPE_FIELD` siblings must be present (F's
    // incomplete one + G's full one).
    let anon_node = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::ANON_RECD_TYPE)
        .expect("ANON_RECD_TYPE present");
    let field_count = anon_node
        .children()
        .filter(|c| c.kind() == SyntaxKind::ANON_RECD_TYPE_FIELD)
        .count();
    assert_eq!(
        field_count,
        2,
        "expected two ANON_RECD_TYPE_FIELD siblings (incomplete F + complete G); got tree:\n{}",
        debug_tree(&parse.root),
    );
    assert_lossless(source, &parse);
}

/// A *repeated* field separator inside an anon-record type is invalid. FCS's
/// `seps` is a single separator group, so `(x : {| F : int; ; G : string |})`
/// is a parse error (`ParseHadErrors: true`, verified against `fcs-dump ast`).
/// The parser consumes exactly one group per gap, so the stray second `;` trips
/// the field parser's recovery — pinning that we do *not* silently accept the
/// malformed run. Single-`;` and column-aligned offside (`OBLOCKSEP`) forms stay
/// valid (covered by the other tests here).
#[test]
fn anon_recd_type_repeated_separator_errors() {
    let source = "let f (x : {| F : int; ; G : string |}) = x\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(
        !parse.errors.is_empty(),
        "a repeated anon-record-type separator must record a parse error",
    );
}

/// Phase 7.9 regression — `let f (x : {| F : int\n              ; G : string |}) = x`:
/// the `OBLOCKSEP SEMICOLON` separator order — the `;` is written
/// at the start of the next field line. FCS accepts this surface
/// with no errors (verified via `fcs-dump`), even though the
/// formal `seps` rule (`pars.fsy:2522`) only lists
/// `SEMICOLON OBLOCKSEP`. The fix is to greedily consume any run
/// of `;` / `BlockSep` tokens as a single separator chunk; the
/// earlier loop only handled `;`, `BlockSep`, and `; BlockSep`
/// and would mis-parse the swapped order as an empty field name
/// before `;`.
#[test]
fn anon_recd_type_block_sep_then_semi_parses_cleanly() {
    use crate::syntax::{AnonRecdType, AstNode};
    let source = "let f (x : {| F : int\n              ; G : string |}) = x\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "got errors: {:?}", parse.errors);
    let anon = AnonRecdType::cast(
        parse
            .root
            .descendants()
            .find(|n| n.kind() == SyntaxKind::ANON_RECD_TYPE)
            .expect("ANON_RECD_TYPE present"),
    )
    .expect("ANON_RECD_TYPE casts");
    let names: Vec<_> = anon
        .fields()
        .map(|f| f.ident().expect("field ident").text().to_string())
        .collect();
    assert_eq!(
        names,
        vec!["F".to_string(), "G".to_string()],
        "expected fields F then G across OBLOCKSEP SEMICOLON; got tree:\n{}",
        debug_tree(&parse.root),
    );
    assert_lossless(source, &parse);
}

/// Phase 7.9 regression — `let f (x : {| F : int\n              G : string |}) = x`:
/// the multi-line anon-recd surface with two fields at the same
/// offside as F, inside a `let`-binding's typed paren-pat (the
/// LexFilter context that actually emits `Virtual::BlockSep`
/// between same-indent fields). The field-continuation loop in
/// `parse_anon_recd_type` must accept that virtual as a separator
/// — FCS's `seps: SEMICOLON | OBLOCKSEP | SEMICOLON OBLOCKSEP`
/// (`pars.fsy:2522`). Without it, the inner `parse_app_type`
/// postfix loop saw the next ident `G` past the virtual and
/// panicked at `parse_app_type_con_power`'s unreachable arm with
/// the virtual still parked at the cursor; with only the loop-
/// break fix (P2a) in place the field type would terminate at
/// `int` and the field loop would then push an `expected |}`
/// error on `G`. Pins (a) two `ANON_RECD_TYPE_FIELD`s, (b) no
/// parse errors. (The top-level `(x : {| … |})` form — no `let`
/// — does *not* emit a `BlockSep` here, so it isn't a useful
/// regression for the `BlockSep`-as-sep path.)
#[test]
fn anon_recd_type_multi_line_block_sep() {
    use crate::syntax::{AnonRecdType, AstNode};
    let source = "let f (x : {| F : int\n              G : string |}) = x\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "got errors: {:?}", parse.errors);
    let anon_node = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::ANON_RECD_TYPE)
        .expect("ANON_RECD_TYPE present for multi-line anon-recd");
    let anon = AnonRecdType::cast(anon_node).expect("ANON_RECD_TYPE casts");
    let names: Vec<_> = anon
        .fields()
        .map(|f| f.ident().expect("field ident").text().to_string())
        .collect();
    assert_eq!(
        names,
        vec!["F".to_string(), "G".to_string()],
        "expected fields F then G across the line break; got tree:\n{}",
        debug_tree(&parse.root),
    );
    assert_lossless(source, &parse);
}
