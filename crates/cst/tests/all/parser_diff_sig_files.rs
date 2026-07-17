//! Differential test (`parser::parse_sig` vs FCS): signature-file (`.fsi`)
//! structure — phase 10.11. The file/segment skeleton (`SIG_FILE` root holding
//! `SynModuleOrNamespaceSig`s) with empty bodies; `val`/type/`open`/etc.
//! specifications arrive in 10.12–10.15.

use crate::common::{assert_sig_asts_match, assert_sig_asts_match_allow_errors};
use borzoi_cst::parser::parse_sig;

// ---- Phase 10.11: sig-file scaffolding + headers (empty bodies) ---------

/// An empty `.fsi` is a single empty `AnonModule` (mirrors the empty `.fs`).
#[test]
fn diff_sig_empty() {
    assert_sig_asts_match("");
}

/// A lone newline — still one empty `AnonModule`.
#[test]
fn diff_sig_blank() {
    assert_sig_asts_match("\n");
}

/// A whole-file `module M` header (no decls) → one `NamedModule`.
#[test]
fn diff_sig_module_header() {
    assert_sig_asts_match("module M\n");
}

/// A dotted whole-file `module A.B.C` header.
#[test]
fn diff_sig_module_header_dotted() {
    assert_sig_asts_match("module A.B.C\n");
}

/// `module rec M` — `isRecursive` set on the `NamedModule`.
#[test]
fn diff_sig_module_header_rec() {
    assert_sig_asts_match("module rec M\n");
}

/// A `namespace N` header → one empty `DeclaredNamespace`.
#[test]
fn diff_sig_namespace_header() {
    assert_sig_asts_match("namespace N\n");
}

/// A dotted `namespace A.B` header.
#[test]
fn diff_sig_namespace_header_dotted() {
    assert_sig_asts_match("namespace A.B\n");
}

/// `namespace rec N` — `isRecursive` set on the `DeclaredNamespace`.
#[test]
fn diff_sig_namespace_header_rec() {
    assert_sig_asts_match("namespace rec N\n");
}

/// Two adjacent `namespace` segments — the file loop emits one
/// `SynModuleOrNamespaceSig` per segment (file segmentation).
#[test]
fn diff_sig_two_namespaces() {
    assert_sig_asts_match("namespace A\nnamespace B\n");
}

/// A leading attributed whole-file header (`[<AutoOpen>]⏎module M`) — the attrs
/// land in `SynModuleOrNamespaceSig.attribs` (mirrors impl 10.7e). The leading
/// `[<` hides the swallowed `module` from the header detector, so the sig body
/// loop claims it.
#[test]
fn diff_sig_module_header_leading_attr() {
    assert_sig_asts_match("[<AutoOpen>]\nmodule M\n");
}

/// An after-keyword attributed whole-file header (`module [<AutoOpen>] M`,
/// mirrors impl 10.7k) — already handled by the reused header parser.
#[test]
fn diff_sig_module_header_after_kw_attr() {
    assert_sig_asts_match("module [<AutoOpen>] M\n");
}

// ---- Phase 10.13a: `open` / `open type` signature declarations -----------
//
// `SynModuleSigDecl.Open of SynOpenDeclTarget * range` — structurally identical
// to the impl-side `SynModuleDecl.Open`, so the `open` parser, target, and
// normaliser are reused. Warning directives (`#nowarn` / `#warnon`) are
// parsed-file trivia (FCS routes them to `WarnDirectives`, not module decls).

/// `open System` — a single-segment module/namespace open.
#[test]
fn diff_sig_open() {
    assert_sig_asts_match("open System\n");
}

/// `open System.Collections.Generic` — a dotted open path.
#[test]
fn diff_sig_open_dotted() {
    assert_sig_asts_match("open System.Collections.Generic\n");
}

/// `open type System.Math` — the `open type` form (`SynOpenDeclTarget.Type`); the
/// `type` keyword is swallowed and recovered as on the impl side.
#[test]
fn diff_sig_open_type() {
    assert_sig_asts_match("open type System.Math\n");
}

/// An `open` inside a `namespace` segment.
#[test]
fn diff_sig_open_in_namespace() {
    assert_sig_asts_match("namespace N\nopen System\n");
}

/// Two `open`s in one (anonymous) segment.
#[test]
fn diff_sig_two_opens() {
    assert_sig_asts_match("open System\nopen System.IO\n");
}

/// A `#nowarn` warning directive is parsed-file trivia, not a module
/// declaration — so the only projected decl is the following `open`. Pins that
/// we don't mis-parse the directive as a spec.
#[test]
fn diff_sig_hash_directive_is_trivia() {
    assert_sig_asts_match("#nowarn \"57\"\nopen System\n");
}

/// Ordinary `#` directives in a signature file are real
/// `SynModuleSigDecl.HashDirective`s, unlike `#nowarn` / `#warnon` trivia.
#[test]
fn diff_sig_hash_directive_decl() {
    assert_sig_asts_match("#I \"/tmp\"\n#load \"a.fsi\"\nopen System\n");
}

/// The light-syntax directive is lexer-consumed in `.fsi` too and should not
/// project as a `HashDirective`.
#[test]
fn diff_sig_hash_light_is_not_a_directive() {
    assert_sig_asts_match("#light\nopen System\n");
}

/// A trailing `;;` top separator after a sig decl is inert (FCS keeps one
/// `Open`, no error).
#[test]
fn diff_sig_open_then_semisemi() {
    assert_sig_asts_match("open System;;\n");
}

/// A `;`-separated pair of opens on one line (FCS's `opt_seps`).
#[test]
fn diff_sig_opens_semicolon_separated() {
    assert_sig_asts_match("open System; open System.IO\n");
}

/// Two opens on one line with *no* separator — FCS accepts this as two
/// `Open` decls (no separator is required between `moduleSpfn`s).
#[test]
fn diff_sig_opens_same_line_no_sep() {
    assert_sig_asts_match("open System open System.IO\n");
}

// ---- Phase 10.13b: module abbrev / nested-module signatures -------------
//
// `SynModuleSigDecl.ModuleAbbrev` (`module A = B.C`) and `.NestedModule`
// (`module M =`⏎`  <sig decls>`) — both reuse the impl-side nodes/projections,
// with a *signature* body. `module rec` is rejected in a `.fsi` (FCS drops it).

/// `module A = B.C` — a module abbreviation (dotted RHS).
#[test]
fn diff_sig_module_abbrev() {
    assert_sig_asts_match("module A = B.C\n");
}

/// `module A = B` — a single-segment abbreviation RHS.
#[test]
fn diff_sig_module_abbrev_single() {
    assert_sig_asts_match("module A = B\n");
}

/// Two module abbreviations in one (anonymous) segment.
#[test]
fn diff_sig_two_module_abbrevs() {
    assert_sig_asts_match("module A = B.C\nmodule D = E.F\n");
}

/// A nested module signature with an `open` body.
#[test]
fn diff_sig_nested_module_open_body() {
    assert_sig_asts_match("module M =\n  open System\n");
}

/// A nested module sig with two body decls.
#[test]
fn diff_sig_nested_module_two_body_decls() {
    assert_sig_asts_match("module M =\n  open System\n  open System.IO\n");
}

/// A doubly-nested module signature (`module M =`⏎`  module Inner =`⏎`    open …`).
#[test]
fn diff_sig_nested_module_doubly_nested() {
    assert_sig_asts_match("module M =\n  module Inner =\n    open System\n");
}

/// A nested module signature under a `namespace`.
#[test]
fn diff_sig_nested_module_in_namespace() {
    assert_sig_asts_match("namespace N\nmodule M =\n  open System\n");
}

/// An *attributed* nested module signature (`[<AutoOpen>]⏎module M =`⏎`  open …`)
/// — the attribute attaches to the nested `SynComponentInfo.attributes` (10.7d).
#[test]
fn diff_sig_nested_module_attr() {
    assert_sig_asts_match("[<AutoOpen>]\nmodule M =\n  open System\n");
}

/// `module rec M = …` in a `.fsi` is rejected (FCS "Invalid use of 'rec'
/// keyword") and the whole decl is dropped — both sides project no decl.
#[test]
fn diff_sig_module_rec_is_error() {
    assert_sig_asts_match_allow_errors("module rec M =\n  open System\n");
}

/// A *dotted* nested-module head (`module A.B =`⏎`  open System`) is rejected in
/// a `.fsi` (FCS "A module name must be a simple name, not a path") and the decl
/// is dropped to an empty `AnonModule` — both sides project no decl.
#[test]
fn diff_sig_dotted_nested_module_is_error() {
    assert_sig_asts_match_allow_errors("module A.B =\n  open System\n");
}

/// An `open` before the first `namespace` in a `.fsi` is the FS0222 illegal
/// prefix (same as the impl side): FCS drops the `open` and keeps only the
/// `DeclaredNamespace`. Our parser wraps the prefix in `ERROR` + flags it.
#[test]
fn diff_sig_open_before_namespace_is_error() {
    assert_sig_asts_match_allow_errors("open System\nnamespace N\n");
}

/// A `#`-directive before a `namespace` in a `.fsi` stays legal — one
/// `DeclaredNamespace`, no error.
#[test]
fn diff_sig_directive_before_namespace_is_ok() {
    assert_sig_asts_match("#nowarn \"57\"\nnamespace N\n");
}

/// Ordinary `#` compiler directives before the first `namespace` are legal and
/// omitted from the projected module list, not an FS0222 anonymous prefix.
#[test]
fn diff_sig_hash_directive_before_namespace_is_ok() {
    assert_sig_asts_match("#I \"/tmp\"\nnamespace N\n");
}

/// An *attribute-only* prefix before a `namespace` in a `.fsi` is illegal — the
/// prefix is dropped (one `DeclaredNamespace`), matching the impl side.
#[test]
fn diff_sig_attr_prefix_before_namespace_is_error() {
    assert_sig_asts_match_allow_errors("[<AutoOpen>]\nnamespace N\n");
}

/// File-form mixing in a `.fsi`: a whole-file `module M` header followed by a
/// `namespace` bails the whole file to one empty `AnonModule` (matching the impl
/// side) — both the module and the namespace are dropped.
#[test]
fn diff_sig_module_header_then_namespace_is_error() {
    assert_sig_asts_match_allow_errors("module M\nnamespace N\n");
}

/// File-form mixing with an *attributed* module header in a `.fsi`
/// (`[<AutoOpen>]⏎module M⏎namespace N`) — same empty-`AnonModule` recovery.
#[test]
fn diff_sig_attr_module_header_then_namespace_is_error() {
    assert_sig_asts_match_allow_errors("[<AutoOpen>]\nmodule M\nnamespace N\n");
}

// ---- Phase 10.12a: bare `val` signatures ---------------------------------
//
// `SynModuleSigDecl.Val of SynValSig * range`. The bare form `val x : <type>`
// (no named/optional params, no `when` constraints — those are later 10.12
// slices). The signature type reuses the phase-7 `parse_type`, so `int`, the
// arrow `int -> string`, curried, tupled, and generic types all diff-match.

/// `val x : int` — the simplest value signature (a non-function type).
#[test]
fn diff_sig_val_simple() {
    assert_sig_asts_match("module M\nval x : int\n");
}

/// `val f : int -> string` — a function type (`SynType.Fun`).
#[test]
fn diff_sig_val_fun() {
    assert_sig_asts_match("module M\nval f : int -> string\n");
}

/// `val mutable x : int` — `SynValSig.isMutable` (elided, but the `mutable`
/// keyword must be consumed for the shape to match).
#[test]
fn diff_sig_val_mutable() {
    assert_sig_asts_match("module M\nval mutable x : int\n");
}

/// `val inline f : int -> int` — `SynValSig.isInline` (elided). The bare
/// inline form has no typars/constraints, so it parses here.
#[test]
fn diff_sig_val_inline() {
    assert_sig_asts_match("module M\nval inline f : int -> int\n");
}

/// A curried function signature — `int -> int -> bool` (right-nested `Fun`).
#[test]
fn diff_sig_val_curried() {
    assert_sig_asts_match("module M\nval f : int -> int -> bool\n");
}

/// A tupled argument — `int * int -> bool` (`Tuple` arg, then `Fun`).
#[test]
fn diff_sig_val_tupled() {
    assert_sig_asts_match("module M\nval f : int * int -> bool\n");
}

/// A generic/applied type — `val xs : int list`.
#[test]
fn diff_sig_val_generic() {
    assert_sig_asts_match("module M\nval xs : int list\n");
}

/// A `val` under a `namespace` (the other header kind).
#[test]
fn diff_sig_val_in_namespace() {
    assert_sig_asts_match("namespace N\nval x : int\n");
}

/// Two `val` specifications in one module body.
#[test]
fn diff_sig_two_vals() {
    assert_sig_asts_match("module M\nval x : int\nval y : string\n");
}

/// `val` interleaved with `open` (the 10.13a decl) in one body.
#[test]
fn diff_sig_val_and_open() {
    assert_sig_asts_match("module M\nopen System\nval x : int\n");
}

/// A `val` inside a *nested* module signature body — closes the 10.13b
/// nested-body limitation (a `val` spec there previously errored).
#[test]
fn diff_sig_val_in_nested_module() {
    assert_sig_asts_match("module M\nmodule Inner =\n  val x : int\n");
}

/// `;`-separated `val`s on one line (FCS's `topSeparators`).
#[test]
fn diff_sig_val_semi_separated() {
    assert_sig_asts_match("module M\nval x : int; val y : int\n");
}

// ---- Phase 10.12c: `when`-constrained `val` signatures -------------------
//
// FCS's `valSpfn` ends in `COLON topTypeWithTypeConstraints` (`pars.fsy:746`,
// `:6034`): the signature type may carry a trailing `when` clause, wrapping it
// in `SynType.WithGlobalConstraints(ty, constraints)`. The bare 10.12a forms
// stopped at the `when`; this slice routes the val-sig type through
// `parse_type_with_constraints` (the same wrapper a binding/member return type
// uses), so the trailing clause folds into a `CONSTRAINED_TYPE`. The free type
// variables (`'T`) are auto-generalised by FCS — no explicit `<'T>` decl is
// needed (those `SynValTyparDecls` stay a later slice, as do named/optional
// parameter signatures).

/// A single `comparison` constraint on a function-type `val` signature.
#[test]
fn diff_sig_val_when_single_constraint() {
    assert_sig_asts_match("module M\nval f : 'T -> 'T when 'T : comparison\n");
}

/// Two type parameters, one constraint each, joined by `and`.
#[test]
fn diff_sig_val_when_two_constraints() {
    assert_sig_asts_match("module M\nval f : 'T -> 'U when 'T : comparison and 'U : equality\n");
}

/// An `inline` `val` with a `when` constraint — the modifier run and the
/// trailing clause coexist.
#[test]
fn diff_sig_val_when_inline() {
    assert_sig_asts_match("module M\nval inline f : 'T -> 'T when 'T : comparison\n");
}

/// A subtype constraint (`'T :> System.IComparable`).
#[test]
fn diff_sig_val_when_subtype() {
    assert_sig_asts_match("module M\nval f : 'T -> 'T when 'T :> System.IComparable\n");
}

/// A `when` over a non-arrow base type (`'T list`) — the wrapper sits around an
/// applied type, not only a `Fun`.
#[test]
fn diff_sig_val_when_over_app_type() {
    assert_sig_asts_match("module M\nval xs : 'T list when 'T : comparison\n");
}

// ---- Phase 10.12 (typars): explicit value type parameters ----------------
//
// FCS's `valSpfn` has `nameop opt_explicitValTyparDecls COLON …`
// (`pars.fsy:746`): the value name may carry postfix `<'T, …>` type parameters
// (`SynValSig.explicitTypeParams`, a `SynValTyparDecls`), optionally with an
// inline `when` constraint clause. We reuse the phase-9.3 postfix typar-decls
// parser (`parse_typar_decls_postfix`, the `TYPAR_DECLS` node) — the same one a
// `type T<'a>` header uses — and project the typars (and inside-`<>`
// constraints) the way a type-definition header does.

/// A single explicit value typar — `val f<'T> : 'T -> 'T`.
#[test]
fn diff_sig_val_explicit_typar() {
    assert_sig_asts_match("module M\nval f<'T> : 'T -> 'T\n");
}

/// Two explicit value typars — `val f<'T, 'U> : 'T -> 'U`.
#[test]
fn diff_sig_val_explicit_typars_two() {
    assert_sig_asts_match("module M\nval f<'T, 'U> : 'T -> 'U\n");
}

/// `inline` modifier plus explicit typars — the modifier run precedes the name.
#[test]
fn diff_sig_val_inline_explicit_typar() {
    assert_sig_asts_match("module M\nval inline f<'T> : 'T -> 'T\n");
}

/// Explicit typars with an inline `<… when …>` constraint clause —
/// `val f<'T when 'T : comparison> : 'T -> 'T`.
#[test]
fn diff_sig_val_explicit_typar_constrained() {
    assert_sig_asts_match("module M\nval f<'T when 'T : comparison> : 'T -> 'T\n");
}

/// An attribute on a `type`-header typar in a signature — `type T<[<Measure>]
/// 'a>` (the `FSharp.Core` `Map.fsi`/`Set.fsi` header shape). The same
/// `SynTyparDecl(attributes, …)` carrier the implementation side uses.
#[test]
fn diff_sig_type_attributed_typar() {
    assert_sig_asts_match("module M\ntype T<[<Measure>] 'a>\n");
}

/// A *spaced* empty explicit typar list — `val f< > : int`. FCS's `valSpfn`
/// permits an empty `explicitValTyparDeclsCore` (only a non-adjacent-typars
/// *warning*, which does not set `ParseHadErrors`), so this is valid and projects
/// to an empty `TyparDecls`. (`val f<>` — adjacent — instead lexes `<>` as the
/// not-equal operator and errors; see `sig_val_adjacent_empty_typar_is_rejected`.)
#[test]
fn diff_sig_val_empty_typar_list_spaced() {
    assert_sig_asts_match("module M\nval f< > : int\n");
}

// ---- Phase 10.12 (literal): `= <literal>` value on a `val` sig ------------
//
// FCS's `valSpfn` ends in `optLiteralValueSpfn = EQUALS declExpr` (`pars.fsy:765`)
// — a `[<Literal>]` value's right-hand side, stored in `SynValSig.synExpr` (field
// 9, a `SynExpr option`). After `=`, LexFilter frames the RHS as `OBLOCKBEGIN
// declExpr OBLOCKEND`, the same shape a `let` binding RHS uses, so it reuses
// `parse_let_equals_rhs`. The RHS is a full `SynExpr` (usually a `Const`, but
// `1 + 2` / `E.A` also parse), projected via the shared expression normaliser.

/// An integer literal value — `val x : int = 1`.
#[test]
fn diff_sig_val_literal_int() {
    assert_sig_asts_match("module M\nval x : int = 1\n");
}

/// A float literal value — `val pi : float = 3.14`.
#[test]
fn diff_sig_val_literal_float() {
    assert_sig_asts_match("module M\nval pi : float = 3.14\n");
}

/// A negative (sign-folded) integer literal — `val x : int = -1`.
#[test]
fn diff_sig_val_literal_signed() {
    assert_sig_asts_match("module M\nval x : int = -1\n");
}

/// A string literal value — `val s : string = "hi"`.
#[test]
fn diff_sig_val_literal_string() {
    assert_sig_asts_match("module M\nval s : string = \"hi\"\n");
}

/// A bool literal value — `val b : bool = true`.
#[test]
fn diff_sig_val_literal_bool() {
    assert_sig_asts_match("module M\nval b : bool = true\n");
}

/// An attributed literal value — `[<Literal>] val x : int = 1` (the realistic
/// form; the attribute attaches to the `VAL_DECL`).
#[test]
fn diff_sig_val_literal_attributed() {
    assert_sig_asts_match("module M\n[<Literal>]\nval x : int = 1\n");
}

/// A dotted-long-ident RHS — `val x : E = E.A` (a `SynExpr.LongIdent`, not a
/// `Const`) — exercises the full-expr projection.
#[test]
fn diff_sig_val_literal_long_ident() {
    assert_sig_asts_match("module M\nval x : E = E.A\n");
}

/// A non-`Const` expression RHS — `val x : int = 1 + 2` (a `SynExpr.App`) —
/// confirms the RHS is projected as a full expression, not just a literal const.
#[test]
fn diff_sig_val_literal_app_expr() {
    assert_sig_asts_match("module M\nval x : int = 1 + 2\n");
}

/// An offside RHS — the literal on the next, indented line.
#[test]
fn diff_sig_val_literal_offside() {
    assert_sig_asts_match("module M\nval x : int =\n  1\n");
}

/// A literal value followed by a sibling `val` — the RHS block close must drain
/// so the sibling spec is reached, not swallowed.
#[test]
fn diff_sig_val_literal_then_sibling() {
    assert_sig_asts_match("module M\nval x : int = 1\nval y : int\n");
}

// A `;`-separated sibling *after* a literal-value `val` (`val x : int = 1; …`)
// is **not** a valid `topSeparators` for FCS — `optLiteralValueSpfn = EQUALS
// declExpr` makes the RHS a full expression, and a `;` there opens a sequence
// the following `val`/`open` keyword can't continue, so FCS errors (FS0010
// "Unexpected symbol ';' in value signature"). We accept it leniently instead
// (the trailing `;` is absorbed by the shared `parse_seq_block_body` gatherer's
// existing repeated-separator tolerance), so there is no diff test here — the
// `sig_val_literal_then_semi_sibling_is_lenient_lossless` structure test pins
// the lossless acceptance.

// ---- Phase 10.12b: named / optional parameter signatures -----------------
//
// FCS's `topType` / `topAppType` (`pars.fsy:6055`, `:6125`) — used for value /
// member / delegate signature types — admits a labelled argument
// `[?]ident : <appType>`, lowered to `SynType.SignatureParameter(attrs,
// isOptional, Some id, usedType, range)` at each arrow / tuple-element position.
// `SIGNATURE_PARAMETER_TYPE` wraps `[QMARK_TOK] IDENT_TOK COLON_TOK <appType>`.
// This `topType` layer is *distinct* from the general `typ`: a named param is
// valid only at the top level of a sig type (and its arrow/tuple structure),
// not inside parens or generic arguments (which reset to `typ`). Attributed
// params (`[<A>] x: int`) land their attribute lists in the
// `SignatureParameter`'s `attributes` field (see the section below); explicit
// value typars (`val f<'T> : …`, now supported) and a `= <literal>` value are
// covered by their own slices.

/// A single named parameter — `val f : x: int -> int`.
#[test]
fn diff_sig_val_named_param() {
    assert_sig_asts_match("module M\nval f : x: int -> int\n");
}

/// Curried named parameters — each arrow argument is a `SignatureParameter`.
#[test]
fn diff_sig_val_named_params_curried() {
    assert_sig_asts_match("module M\nval f : x: int -> y: string -> bool\n");
}

/// An optional parameter — `val f : ?x: int -> int` (`isOptional = true`).
#[test]
fn diff_sig_val_optional_param() {
    assert_sig_asts_match("module M\nval f : ?x: int -> int\n");
}

/// Two optional parameters.
#[test]
fn diff_sig_val_optional_params_two() {
    assert_sig_asts_match("module M\nval f : ?x: int -> ?y: string -> bool\n");
}

/// A named parameter mixed with an unnamed (bare-type) one — only the labelled
/// argument is a `SignatureParameter`.
#[test]
fn diff_sig_val_named_param_mixed() {
    assert_sig_asts_match("module M\nval f : int -> y: string -> bool\n");
}

/// A named parameter whose value type is an applied type (`int list`).
#[test]
fn diff_sig_val_named_param_app_type() {
    assert_sig_asts_match("module M\nval f : xs: int list -> int\n");
}

/// Tupled named parameters — `x: int * y: int -> int`; each tuple element is a
/// `SignatureParameter`.
#[test]
fn diff_sig_val_named_params_tupled() {
    assert_sig_asts_match("module M\nval f : x: int * y: int -> int\n");
}

// ---- Attributes on signature parameters (`[<A>] x: int`) -------------------
//
// FCS's `topAppTypeElement` admits an attribute run before a (labelled,
// unlabelled, or optional) element, stored in `SynType.SignatureParameter`'s
// `attributes` field (field 0). Our `SIGNATURE_PARAMETER_TYPE` now carries the
// leading `ATTRIBUTE_LIST` children, projected through the shared attribute
// normaliser (the FCS side already read them). Motivated by the FSharp.Core
// builder sigs (`Cancellable.fsi` / `tasks.fsi`), dense with
// `[<InlineIfLambda>] k: …`.

/// A leading attributed parameter — `[<E>] x: int -> int`.
#[test]
fn diff_sig_val_attr_param_leading() {
    assert_sig_asts_match("module M\nval f : [<System.Obsolete>] x: int -> int\n");
}

/// An attributed parameter after a `*` separator — the `Cancellable.fsi` shape
/// (`comp: _ * [<InlineIfLambda>] k: _`). Guards the tuple-separator gate.
#[test]
fn diff_sig_val_attr_param_after_star() {
    assert_sig_asts_match("module M\nval f : x: int * [<System.Obsolete>] y: int -> int\n");
}

/// The realistic member-sig shape — `member inline Bind : comp: int *
/// [<InlineIfLambda>] k: (int -> int) -> int`.
#[test]
fn diff_sig_member_attr_param() {
    assert_sig_asts_match(
        "module M\ntype T =\n  member inline Bind : comp: int * [<System.Obsolete>] k: (int -> int) -> int\n",
    );
}

/// An attributed *optional* parameter — `[<E>] ?y: int`.
#[test]
fn diff_sig_val_attr_param_optional() {
    assert_sig_asts_match("module M\nval f : x: int * [<System.Obsolete>] ?y: int -> int\n");
}

/// An attributed *unnamed* parameter (`[<E>] int`) — FCS lowers it to a
/// `SignatureParameter` whose `id` is `None`.
#[test]
fn diff_sig_val_attr_param_unnamed() {
    assert_sig_asts_match("module M\nval f : int * [<System.Obsolete>] int -> int\n");
}

/// Several attribute lists on one parameter (`[<A>] [<B>] x: int`).
#[test]
fn diff_sig_val_attr_param_multi_list() {
    assert_sig_asts_match(
        "module M\nval f : [<System.Obsolete>] [<System.Diagnostics.Conditional(\"X\")>] x: int -> int\n",
    );
}

/// Several attributes in one list (`[<A; B>] x: int`).
#[test]
fn diff_sig_val_attr_param_multi_attr() {
    assert_sig_asts_match(
        "module M\nval f : [<System.Obsolete; System.Diagnostics.Conditional(\"X\")>] x: int -> int\n",
    );
}

/// An attributed parameter on a `delegate of …` signature (the third `topType`
/// consumer alongside `val` and `member`).
#[test]
fn diff_sig_delegate_attr_param() {
    assert_sig_asts_match("namespace N\ntype T = delegate of [<System.Obsolete>] x: int -> int\n");
}

/// An attribute on its *own line*, with the parameter on the next line — FCS's
/// `attributes opt_OBLOCKSEP` leaves a `Virtual::BlockSep` between them, which
/// `parse_signature_parameter` drains before the label.
#[test]
fn diff_sig_attr_param_own_line() {
    assert_sig_asts_match(
        "module M\nval f : x: int *\n        [<System.Obsolete>]\n        y: int -> int\n",
    );
}

// ---- Operator-name and active-pattern-name `val` signatures --------------
//
// FCS's `valSpfn` names its value through `opName` (`pars.fsy`), which — beyond
// a plain identifier — admits a parenthesised operator-value (`(+)`, `( * )`)
// and an active-pattern name (`(|Foo|_|)`). Each reuses the binding-head
// machinery: the operator emits `[LPAREN_TOK, IDENT_TOK(op), RPAREN_TOK]` into
// the `VAL_SIG` (FCS mangles to `op_*` + `OriginalNotationWithParen`, which the
// FCS-side normaliser unwraps to the same bare operator we store under
// `IDENT_TOK`); the active-pattern name emits an `ACTIVE_PAT_NAME` node whose
// case tokens rebuild FCS's single `idText` (`"|Foo|_|"`).

/// `val (&) : bool -> bool` — a symbolic operator-value name (`Token::Amp`).
#[test]
fn diff_sig_val_operator_amp() {
    assert_sig_asts_match("module M\nval (&) : bool -> bool\n");
}

/// A multi-character `Token::Op` operator name — `val (+->) : …`.
#[test]
fn diff_sig_val_operator_multichar() {
    assert_sig_asts_match("module M\nval (+->) : int -> int -> int\n");
}

/// The spaced multiply operator-value `( * )` — admitted as a name here (a
/// `val` name has no whole-dimension-wildcard ambiguity), like a binding head.
#[test]
fn diff_sig_val_operator_spaced_star() {
    assert_sig_asts_match("module M\nval ( * ) : int -> int -> int\n");
}

/// The glued multiply operator-value `(*)` (the lexer's `LParenStarRParen`).
#[test]
fn diff_sig_val_operator_glued_star() {
    assert_sig_asts_match("module M\nval (*) : int -> int -> int\n");
}

/// An operator name carrying labelled parameters, as in the corpus
/// (`IntrinsicOperators`'s `val (&)`): the `topType` labelled-arg layer applies
/// to an operator-named `val` exactly as to an identifier-named one.
#[test]
fn diff_sig_val_operator_labelled_params() {
    assert_sig_asts_match("module M\nval (&) : e1: bool -> e2: bool -> bool\n");
}

/// A partial active-pattern name — `val (|Foo|_|) : int -> int option`.
#[test]
fn diff_sig_val_active_pattern_partial() {
    assert_sig_asts_match("module M\nval (|Foo|_|) : int -> int option\n");
}

/// A multi-case total active-pattern name — `val (|Foo|Bar|) : …`.
#[test]
fn diff_sig_val_active_pattern_multi_case() {
    assert_sig_asts_match("module M\nval (|Foo|Bar|) : int -> Choice<int, int>\n");
}

/// `inline` before an active-pattern name — the modifier run precedes the name
/// slot (corpus: `CheckRecordSyntaxHelpers`'s `val inline (|IsSimpleOrBoundExpr|_|)`).
#[test]
fn diff_sig_val_inline_active_pattern() {
    assert_sig_asts_match("module M\nval inline (|Foo|_|) : int -> int option\n");
}

/// `internal` before an active-pattern name (corpus:
/// `ServiceParsedInputOps`'s `val internal (|Sequentials|_|)`).
#[test]
fn diff_sig_val_internal_active_pattern() {
    assert_sig_asts_match("module M\nval internal (|Foo|_|) : int -> int option\n");
}

/// Named parameters on a member signature's return type (`topTypeWithType‑
/// Constraints`, the same `topType` layer).
#[test]
fn diff_sig_member_named_params() {
    assert_sig_asts_match("namespace N\ntype T =\n  abstract M : x: int -> y: int -> int\n");
}

/// A named parameter on a `delegate of …` signature (`DELEGATE OF topType`).
#[test]
fn diff_sig_delegate_named_param() {
    assert_sig_asts_match("namespace N\ntype T = delegate of x: int -> int\n");
}

// (Scope boundary: a parenthesised `(x: int)` resets to the general `typ`
// grammar, where a labelled argument is **not** a `SignatureParameter` — FCS
// rejects it. The error-recovery *shape* is not byte-faithful to FCS, so this is
// pinned by the `sig_val_paren_param_not_signature_parameter` structure test
// (must-error + lossless) rather than a diff test.)

// ---- Phase 10.14 (first slice): type-abbreviation signatures -------------
//
// `SynModuleSigDecl.Types of SynTypeDefnSig list * range`, restricted to the
// abbreviation repr `type T = <ty>` (FCS's `SynTypeDefnSigRepr.Simple` wrapping
// `SynTypeDefnSimpleRepr.TypeAbbrev`). The `SynTypeDefnSig` reuses the impl-side
// `TYPE_DEFNS`/`TYPE_DEFN`/`TYPE_ABBREV` nodes and the `NormalisedTypeDefn`
// projection; record/union/enum/object-model reprs, `and`-chains, opaque
// (bodyless) types, and exception sigs are later slices.

/// `type Alias = int` — the simplest abbreviation under a namespace.
#[test]
fn diff_sig_type_abbrev() {
    assert_sig_asts_match("namespace N\ntype Alias = int\n");
}

/// A tuple abbreviation — `type Pair = int * string`.
#[test]
fn diff_sig_type_abbrev_tuple() {
    assert_sig_asts_match("namespace N\ntype Pair = int * string\n");
}

/// A function-type abbreviation — `type Op = int -> int`.
#[test]
fn diff_sig_type_abbrev_fun() {
    assert_sig_asts_match("namespace N\ntype Op = int -> int\n");
}

/// A generic/applied abbreviation RHS — `type Ints = int list`.
#[test]
fn diff_sig_type_abbrev_applied() {
    assert_sig_asts_match("namespace N\ntype Ints = int list\n");
}

/// A postfix-generic abbreviation — `type Box<'a> = 'a list`
/// (`SynComponentInfo.typeParams`, reused from phase 9.3).
#[test]
fn diff_sig_type_abbrev_generic_postfix() {
    assert_sig_asts_match("namespace N\ntype Box<'a> = 'a list\n");
}

/// A prefix-single-typar abbreviation — `type 'a Box = 'a list`.
#[test]
fn diff_sig_type_abbrev_generic_prefix() {
    assert_sig_asts_match("namespace N\ntype 'a Box = 'a list\n");
}

/// An abbreviation under a whole-file `module` header (the other placement).
#[test]
fn diff_sig_type_abbrev_in_module() {
    assert_sig_asts_match("module M\ntype Alias = int\n");
}

/// Two abbreviations as *separate* (non-`and`) decls — each its own
/// `SynModuleSigDecl.Types` singleton.
#[test]
fn diff_sig_two_type_abbrevs() {
    assert_sig_asts_match("namespace N\ntype A = int\ntype B = string\n");
}

/// An abbreviation interleaved with a `val` in one body.
#[test]
fn diff_sig_type_abbrev_and_val() {
    assert_sig_asts_match("module M\ntype Alias = int\nval x : Alias\n");
}

/// An abbreviation inside a *nested* module signature body.
#[test]
fn diff_sig_type_abbrev_in_nested_module() {
    assert_sig_asts_match("module M\nmodule Inner =\n  type Alias = int\n");
}

// ---- Phase 10.14 (slice 2a): opaque / bodyless type signatures ------------
//
// A bodyless type definition `type T` (no `=`, no `with`) — FCS lowers this to
// `SynModuleSigDecl.Types` holding one `SynTypeDefnSig` whose repr is
// `SynTypeDefnSigRepr.Simple(SynTypeDefnSimpleRepr.None)`. The opaque type is
// the defining `.fsi` construct (a type whose representation is hidden). The
// normaliser already reads an absent repr as `NormalisedTypeRepr::None`, so this
// reuses the abbreviation slice's `SigDecl::Types` projection unchanged. Member
// sigs (`type T with member …`) and `and`-chains are later slices.

/// `type T` — the simplest opaque type, under a namespace.
#[test]
fn diff_sig_type_opaque() {
    assert_sig_asts_match("namespace N\ntype T\n");
}

/// An opaque type under a whole-file `module` header.
#[test]
fn diff_sig_type_opaque_in_module() {
    assert_sig_asts_match("module M\ntype Handle\n");
}

/// Two opaque types as separate decls — each its own `Types` singleton.
#[test]
fn diff_sig_two_opaque_types() {
    assert_sig_asts_match("namespace N\ntype T\ntype U\n");
}

/// A postfix-generic opaque type — `type Box<'a>`
/// (`SynComponentInfo.typeParams`, reused from phase 9.3).
#[test]
fn diff_sig_type_opaque_generic_postfix() {
    assert_sig_asts_match("namespace N\ntype Box<'a>\n");
}

/// A prefix-single-typar opaque type — `type 'a Box`.
#[test]
fn diff_sig_type_opaque_generic_prefix() {
    assert_sig_asts_match("namespace N\ntype 'a Box\n");
}

/// An opaque type interleaved with a `val` and an abbreviation in one body.
#[test]
fn diff_sig_type_opaque_and_val() {
    assert_sig_asts_match("module M\ntype T\nval x : T\ntype Alias = int\n");
}

/// An opaque type inside a *nested* module signature body.
#[test]
fn diff_sig_type_opaque_in_nested_module() {
    assert_sig_asts_match("module M\nmodule Inner =\n  type T\n");
}

// (An *attributed* bodyless type — `[<Measure>] type m` — is deferred: a leading
// attribute on any type signature specification is a separate later phase-10
// slice, independent of opaque-type support. See the attribute-dispatch arm in
// `parse_sig_module_decls`.)

// ---- Phase 10.14 (slice 2b): record / union / enum signature reprs --------
//
// The structural `SynTypeDefnSimpleRepr` forms — `type R = { … }` (Record),
// `type U = A | B of int` (Union), `type E = X = 0 | …` (Enum) — in a `.fsi`.
// FCS's `tyconDefnOrSpfnSimpleRepr` grammar is shared between impl and sig, so a
// `SynTypeDefnSigRepr.Simple` wraps the same `SynTypeDefnSimpleRepr` as the impl
// side: the existing `parse_record_repr` / `parse_union_or_enum_repr` parsers,
// the `RECORD_REPR`/`UNION_REPR`/`ENUM_REPR` nodes, and the normaliser
// (`fcs_simple_repr`) are all reused unchanged. Trailing member sigs
// (`type R = { … } with member …`) are deferred to the member-sig slice.

/// `type R = { X : int }` — a single-field record signature.
#[test]
fn diff_sig_type_record() {
    assert_sig_asts_match("namespace N\ntype R = { X : int }\n");
}

/// A multi-field record with a `mutable` field.
#[test]
fn diff_sig_type_record_multi() {
    assert_sig_asts_match("namespace N\ntype R = { X : int; mutable Y : string }\n");
}

/// An offside (multi-line) record body.
#[test]
fn diff_sig_type_record_offside() {
    assert_sig_asts_match("namespace N\ntype R =\n  { X : int\n    Y : string }\n");
}

/// A generic record — `type Box<'a> = { Value : 'a }`.
#[test]
fn diff_sig_type_record_generic() {
    assert_sig_asts_match("namespace N\ntype Box<'a> = { Value : 'a }\n");
}

/// `type U = A | B` — a nullary discriminated union.
#[test]
fn diff_sig_type_union_nullary() {
    assert_sig_asts_match("namespace N\ntype U = A | B\n");
}

/// A union with `of`-typed cases — `type U = A of int | B of string`.
#[test]
fn diff_sig_type_union_typed() {
    assert_sig_asts_match("namespace N\ntype U = A of int | B of string\n");
}

/// A union with a leading bar, offside — `type U =`⏎`  | A`⏎`  | B of int`.
#[test]
fn diff_sig_type_union_offside_leading_bar() {
    assert_sig_asts_match("namespace N\ntype U =\n  | A\n  | B of int\n");
}

/// A generic union — `type Tree<'a> = Leaf | Node of 'a`.
#[test]
fn diff_sig_type_union_generic() {
    assert_sig_asts_match("namespace N\ntype Tree<'a> = Leaf | Node of 'a\n");
}

/// `type E = X = 0 | Y = 1` — an enum signature.
#[test]
fn diff_sig_type_enum() {
    assert_sig_asts_match("namespace N\ntype E = X = 0 | Y = 1\n");
}

/// An offside enum — `type E =`⏎`  | X = 0`⏎`  | Y = 1`.
#[test]
fn diff_sig_type_enum_offside() {
    assert_sig_asts_match("namespace N\ntype E =\n  | X = 0\n  | Y = 1\n");
}

/// A record under a whole-file `module` header, interleaved with a `val`.
#[test]
fn diff_sig_type_record_and_val() {
    assert_sig_asts_match("module M\ntype R = { X : int }\nval mk : int -> R\n");
}

/// A union inside a *nested* module signature body.
#[test]
fn diff_sig_type_union_in_nested_module() {
    assert_sig_asts_match("module M\nmodule Inner =\n  type U = A | B\n");
}

/// A structural repr alongside an abbreviation and an opaque type — three
/// separate `Types` decls in one body.
#[test]
fn diff_sig_type_structural_mixed() {
    assert_sig_asts_match("namespace N\ntype R = { X : int }\ntype Alias = int\ntype Opaque\n");
}

// ---- Phase 10.14 (slice 3a): member/abstract member signatures ------------
//
// `SynMemberSig.Member(SynValSig, flags, …)` in a lightweight object-model body
// `type T =`⏎`  member X : int` / `  abstract Y : int`. The repr is
// `SynTypeDefnSigRepr.ObjectModel(Unspecified, memberSigs, …)`. A member sig is
// a `SynValSig` carrier (name + `: <type>`, no body) plus a leading keyword
// (`member` / `abstract` / `static member`) — projecting like an abstract slot.
// inherit / interface / val-field sigs, explicit class/struct/interface-end
// bodies, the outer-slot (augmentation) members, and property get/set are later
// slices.

/// A single `abstract` member signature.
#[test]
fn diff_sig_member_abstract_simple() {
    assert_sig_asts_match("namespace N\ntype I =\n  abstract Name : string\n");
}

/// An abstract member with a function type.
#[test]
fn diff_sig_member_abstract_fun() {
    assert_sig_asts_match("namespace N\ntype I =\n  abstract Add : int -> int -> int\n");
}

/// `abstract member` (the `member`-keyword form → `AbstractMember`).
#[test]
fn diff_sig_member_abstract_member_kw() {
    assert_sig_asts_match("namespace N\ntype I =\n  abstract member M : int\n");
}

/// A concrete `member` signature.
#[test]
fn diff_sig_member_concrete() {
    assert_sig_asts_match("namespace N\ntype T =\n  member Compute : int\n");
}

/// A `static member` signature.
#[test]
fn diff_sig_member_static() {
    assert_sig_asts_match("namespace N\ntype T =\n  static member Make : int -> T\n");
}

/// Several member sigs in one body (abstract + concrete + static).
#[test]
fn diff_sig_member_multi() {
    assert_sig_asts_match(
        "namespace N\ntype I =\n  abstract Name : string\n  abstract Add : int -> int\n  member Compute : int\n",
    );
}

/// A member sig alongside a `val` spec in the same module (the member is inside
/// the type, the `val` is a sibling module decl).
#[test]
fn diff_sig_member_and_module_val() {
    assert_sig_asts_match("module M\ntype I =\n  abstract M : int\nval mk : unit -> I\n");
}

/// A member sig in a type under a nested module.
#[test]
fn diff_sig_member_in_nested_module() {
    assert_sig_asts_match("module M\nmodule Inner =\n  type I =\n    abstract M : int\n");
}

/// `static abstract member` — an F# static-abstract interface member sig
/// (`StaticAbstractMember` leading keyword).
#[test]
fn diff_sig_member_static_abstract_member() {
    assert_sig_asts_match("namespace N\ntype I =\n  static abstract member M : int\n");
}

/// `static abstract` (no `member` keyword) — `StaticAbstract` leading keyword.
#[test]
fn diff_sig_member_static_abstract() {
    assert_sig_asts_match("namespace N\ntype I =\n  static abstract M : int\n");
}

// ---- `override` / `default` member signatures ------------------------------
//
// `override M : ty` / `default M : ty` — the same `SynMemberSig.Member`
// (`SynValSig`) carrier as `member`/`abstract`, distinguished by the leading
// keyword. FCS carries it in `SynValSig.trivia.LeadingKeyword`
// (`SynLeadingKeyword.Override` / `.Default`); the two share identical
// `SynMemberFlags` (`IsOverrideOrExplicitImpl = true`), so the keyword is the
// only distinguisher. Common in `.fsi` abstract-slot overrides
// (`AbstractSlot01.fsi`, `HasSignatureWithMissingOverride.fsi`).

/// A single `override` member signature.
#[test]
fn diff_sig_member_override_simple() {
    assert_sig_asts_match("namespace N\ntype T =\n  override ToString : unit -> string\n");
}

/// A single `default` member signature.
#[test]
fn diff_sig_member_default_simple() {
    assert_sig_asts_match("namespace N\ntype T =\n  default M : int\n");
}

/// `override` with a value (property) type — no arrow.
#[test]
fn diff_sig_member_override_property() {
    assert_sig_asts_match("namespace N\ntype T =\n  override Count : int\n");
}

/// The `AbstractSlot01.fsi` shape: `abstract`/`default` in one type, `inherit`
/// + `override` in a derived one.
#[test]
fn diff_sig_member_abstract_default_override_mix() {
    assert_sig_asts_match(
        "module M\ntype Foo =\n  abstract M : unit -> int\n  default M : unit -> int\ntype Bar =\n  inherit Foo\n  override M : unit -> int\n",
    );
}

/// All six member-sig keyword forms together
/// (`SynMemberSigMemberHasCorrectKeywords.fsi`).
#[test]
fn diff_sig_member_all_keyword_forms() {
    assert_sig_asts_match(
        "namespace X\ntype Y =\n  abstract A : int\n  abstract member B : double\n  static member C : string\n  member D : int\n  override E : int\n  default F : int\n",
    );
}

// ---- `inline` member signatures (`member inline M : …`) --------------------
//
// FCS's `classMemberSpfn` carries `opt_inline` between the member keyword(s)
// and the name (`[static] member [inline] opt_access nameop`), setting
// `SynValSig.isInline`. The flag is elided by the normaliser, so consuming the
// `inline` keyword for the shape to match is all that is needed. Motivated by
// FSharp.Core's builder sigs (`async.fsi` / `tasks.fsi` / `illib.fsi` /
// `Cancellable.fsi`), which are dense with `member inline`.

/// A concrete `member inline` signature — the FSharp.Core builder shape.
#[test]
fn diff_sig_member_inline() {
    assert_sig_asts_match("namespace N\ntype T =\n  member inline Bind : int -> int\n");
}

/// A curried `member inline` signature (`member inline MergeSources : …`).
#[test]
fn diff_sig_member_inline_curried() {
    assert_sig_asts_match(
        "namespace N\ntype T =\n  member inline MergeSources : int -> int -> int\n",
    );
}

/// A `static member inline` signature.
#[test]
fn diff_sig_member_static_inline() {
    assert_sig_asts_match("namespace N\ntype T =\n  static member inline FromFunc : int -> T\n");
}

/// `inline` followed by a name-position access modifier — FCS's order is
/// `member inline private` (`inline` *before* the modifier).
#[test]
fn diff_sig_member_inline_private() {
    assert_sig_asts_match("namespace N\ntype T =\n  member inline private Run : int -> int\n");
}

/// `inline` composes with an operator name (`member inline (+) : …`).
#[test]
fn diff_sig_member_inline_operator() {
    assert_sig_asts_match("namespace N\ntype T =\n  static member inline (+) : T * T -> T\n");
}

// ---- Explicit type parameters on member signatures (`member M<'U> : …`) -----
//
// FCS's `classMemberSpfn` carries `opt_explicitValTyparDecls` between the name
// and the `:` (`[static] member [inline] nameop <'U …> : …`), homed in
// `SynValSig.explicitTypeParams` — the same `SynValTyparDecls` a `val f<'T>` /
// `abstract M<'U>` sig carries. The typars (and any inside-`<>` `when`
// constraints) are elided by the `AbstractSlot` projection, so consuming the
// postfix `<…>` is all that is needed. An after-type `when` clause (plain or an
// SRTP `(member …)` support constraint) folds into the signature type via the
// existing `topType` wrapper. Motivated by `tasks.fsi`, whose builder sigs are
// dense with generic `member inline`s.

/// A concrete generic member sig — `member M<'U> : 'U -> 'U`.
#[test]
fn diff_sig_member_generic() {
    assert_sig_asts_match("namespace N\ntype T =\n  member M<'U> : 'U -> 'U\n");
}

/// A generic `abstract` member sig — the `.fsi` abstract slot also routes
/// through `parse_member_sig`, so it needs the same postfix-typar parse.
#[test]
fn diff_sig_member_abstract_generic() {
    assert_sig_asts_match("namespace N\ntype T =\n  abstract M<'U> : 'U -> 'U\n");
}

/// A generic `member inline` sig with several typars — the `tasks.fsi` shape.
#[test]
fn diff_sig_member_inline_generic() {
    assert_sig_asts_match("namespace N\ntype T =\n  member inline Bind< ^A, ^Aw, 'T> : ^A -> 'T\n");
}

/// An inside-`<>` `when` constraint on the typars — the `Using` shape
/// (`member inline Using<'R, 'T when 'R :> System.IDisposable> : …`). The
/// constraint folds into the `TYPAR_DECLS` postfix list (elided), like a
/// `val f<'T when …>`.
#[test]
fn diff_sig_member_generic_inside_constraint() {
    assert_sig_asts_match(
        "namespace N\ntype T =\n  member inline Using<'R, 'T when 'R :> System.IDisposable> : 'R -> 'T\n",
    );
}

/// An after-type plain `when` constraint on a generic member sig — the
/// constraint wraps the signature type in `SynType.WithGlobalConstraints`, not
/// the typar decls.
#[test]
fn diff_sig_member_generic_after_when() {
    assert_sig_asts_match(
        "namespace N\ntype T =\n  member M<'U> : 'U -> 'U when 'U : comparison\n",
    );
}

/// An after-type *SRTP-member* `when` constraint with a chained `and` — the
/// `Bind` shape from `tasks.fsi`. The `(member …)` support in the `when` clause
/// composes with the member-sig parse (it reaches the same
/// `member_sig_body_is_supported` gate the constraint already drives).
#[test]
fn diff_sig_member_generic_after_srtp_when() {
    assert_sig_asts_match(
        "namespace N\ntype T =\n  member inline Bind< ^A, ^Aw> : ^A -> int \
         when ^A : (member GetAwaiter : unit -> ^Aw) and ^Aw :> System.IComparable\n",
    );
}

/// A spaced empty typar list `< >` — FCS's `explicitValTyparDeclsCore` admits an
/// empty core (`permit_empty = true`, matching the `val f< >` sibling).
#[test]
fn diff_sig_member_empty_typar_list() {
    assert_sig_asts_match("namespace N\ntype T =\n  member M< > : int -> int\n");
}

// ---- Phase 10.12 (member literal): `= <literal>` on a member sig ------------
//
// FCS's `classMemberSpfn` member arm ends in the same `optLiteralValueSpfn =
// EQUALS declExpr` (`pars.fsy`) as the module-level `valSpfn` — a `[<Literal>]`
// member value's RHS, stored in the member's `SynValSig.synExpr` (field 9). All
// three concrete carriers accept it (fcs-dump-verified `ParseHadErrors: false`):
// `member a : int = 10`, `abstract a : int = 10`, `static member a : int = 10`.
// (An *impl*-side `abstract M : int = 1` is rejected by FCS — FS0010 — so the
// shared `AbstractSlot`'s literal is only ever populated on the sig side.) The
// RHS is a full `SynExpr` (usually a `Const`, but `E.A` / `1 + 2` parse too),
// projected via the shared expression normaliser into the new
// `NormalisedMember::AbstractSlot.literal`.

/// A concrete `member` literal — the canonical `[<Literal>]` member value.
#[test]
fn diff_sig_member_literal_concrete() {
    assert_sig_asts_match("namespace N\ntype X =\n  member a : int = 10\n");
}

/// An `abstract` member with a literal RHS (FCS accepts it syntactically).
#[test]
fn diff_sig_member_literal_abstract() {
    assert_sig_asts_match("namespace N\ntype X =\n  abstract a : int = 10\n");
}

/// A `static member` literal.
#[test]
fn diff_sig_member_literal_static() {
    assert_sig_asts_match("namespace N\ntype X =\n  static member a : int = 10\n");
}

/// A string-literal member value (a non-int `Const`).
#[test]
fn diff_sig_member_literal_string() {
    assert_sig_asts_match("namespace N\ntype X =\n  member s : string = \"hi\"\n");
}

/// A non-`Const` RHS — a dotted path `E.A` — proving the full-expression
/// projection (not just constants).
#[test]
fn diff_sig_member_literal_long_ident() {
    assert_sig_asts_match("namespace N\ntype X =\n  member a : int = E.A\n");
}

/// A member literal followed by a sibling member sig — the RHS block close must
/// drain so the sibling is reached, not swallowed.
#[test]
fn diff_sig_member_literal_then_sibling() {
    assert_sig_asts_match("namespace N\ntype X =\n  member a : int = 1\n  member b : int\n");
}

/// An `[<Literal>]`-attributed member literal (attrs + RHS together).
#[test]
fn diff_sig_member_literal_attributed() {
    assert_sig_asts_match("namespace N\ntype X =\n  [<Literal>] member a : int = 10\n");
}

// ---- Operator-named member signatures (`member (+) : …`) --------------------
//
// FCS reads the member name via `opName` and mangles the operator to its
// compiled spelling (`op_Addition`) with an `IdentTrivia.OriginalNotation`
// source-spelling slot. Our parser routes the name through the binding-head
// operator machinery (`peek_operator_head` / `consume_paren_op_value`), emitting
// `[LPAREN_TOK, IDENT_TOK(op), RPAREN_TOK]` as the `SynValSig` name.

/// A concrete operator member sig — `member (+) : T -> T`.
#[test]
fn diff_sig_member_operator_plus() {
    assert_sig_asts_match("namespace N\ntype T =\n  member (+) : T -> T\n");
}

/// `abstract member (+) : …` — the operator name with the `member`-keyword form.
#[test]
fn diff_sig_member_abstract_operator() {
    assert_sig_asts_match("namespace N\ntype T =\n  abstract member (+) : T -> T -> T\n");
}

/// `static member (+) : …` — the standard operator-overload signature shape.
#[test]
fn diff_sig_member_static_operator() {
    assert_sig_asts_match("namespace N\ntype T =\n  static member (+) : T * T -> T\n");
}

/// The glued multiply operator `(*)` (the dedicated `LParenStarRParen` token).
#[test]
fn diff_sig_member_operator_star() {
    assert_sig_asts_match("namespace N\ntype T =\n  static member (*) : T * T -> T\n");
}

/// A prefix/unary operator name `(~-)`.
#[test]
fn diff_sig_member_operator_unary_minus() {
    assert_sig_asts_match("namespace N\ntype T =\n  static member (~-) : T -> T\n");
}

/// A spaced operator name `( + )` — the same `SynValSig` name as the unspaced
/// `(+)` (guards the spaced `at_paren_op_value_pat` path).
#[test]
fn diff_sig_member_operator_spaced() {
    assert_sig_asts_match("namespace N\ntype T =\n  member ( + ) : T -> T\n");
}

// Active-pattern-named member signatures — the active-pattern analogue of the
// operator member sigs above. FCS's `opName` member name covers the
// active-pattern productions, folding the whole name into the single `idText`
// of `SynValSig.ident` (`"|Foo|_|"`); our parser emits an `ACTIVE_PAT_NAME`
// node into the `VAL_SIG` (the same node the `val`-sig / pattern positions use),
// and the normaliser rebuilds the `idText` from its case tokens. (FCS's parser
// accepts these even though active patterns aren't valid members semantically.)

/// A partial active-pattern member sig — `member (|Foo|_|) : int -> int option`.
#[test]
fn diff_sig_member_active_pattern_partial() {
    assert_sig_asts_match("namespace N\ntype T =\n  member (|Foo|_|) : int -> int option\n");
}

/// A multi-case active-pattern member sig with the `abstract member` form.
#[test]
fn diff_sig_member_abstract_active_pattern() {
    assert_sig_asts_match(
        "namespace N\ntype T =\n  abstract member (|Foo|Bar|) : int -> Choice<int, int>\n",
    );
}

/// A `static member` active-pattern sig.
#[test]
fn diff_sig_member_static_active_pattern() {
    assert_sig_asts_match(
        "namespace N\ntype T =\n  static member (|Foo|Bar|) : int -> Choice<int, int>\n",
    );
}

/// `;`-separated member sigs on one line (FCS's `opt_seps`).
#[test]
fn diff_sig_member_semi_separated() {
    assert_sig_asts_match("namespace N\ntype I =\n  abstract A : int; abstract B : int\n");
}

// ---- Phase 10.14 (slice 3a, cont.): `when`-constrained member sigs ---------
//
// FCS's `classMemberSpfn` member arm (`pars.fsy:969`) ends in
// `COLON topTypeWithTypeConstraints`, so a member signature's type may carry a
// trailing `when` clause — `SynType.WithGlobalConstraints(ty, constraints)`. The
// member sig now routes its type through `parse_type_with_constraints` (the same
// `CONSTRAINED_TYPE` wrapper a binding/member return type uses, phase 9.3b, and
// the `val`-sig 10.12c / abstract-slot paths), so the clause folds in. Free type
// variables in a member signature are auto-generalised to the member's own type
// parameters, so no explicit `<'T>` decl is needed.

/// A `when` constraint on an `abstract` member signature.
#[test]
fn diff_sig_member_abstract_when() {
    assert_sig_asts_match("namespace N\ntype I =\n  abstract M : 'T -> 'T when 'T : comparison\n");
}

/// A `when` constraint on a *concrete* `member` signature.
#[test]
fn diff_sig_member_concrete_when() {
    assert_sig_asts_match(
        "namespace N\ntype T =\n  member Compute : 'T -> 'T when 'T : comparison\n",
    );
}

/// A `when` constraint on a `static member` signature.
#[test]
fn diff_sig_member_static_when() {
    assert_sig_asts_match(
        "namespace N\ntype T =\n  static member Make : 'T -> T when 'T : comparison\n",
    );
}

/// Two type parameters, one constraint each, joined by `and`.
#[test]
fn diff_sig_member_when_two_constraints() {
    assert_sig_asts_match(
        "namespace N\ntype I =\n  abstract M : 'T -> 'U when 'T : comparison and 'U : equality\n",
    );
}

/// A `when`-constrained member sig followed by a sibling member — the clause must
/// terminate cleanly so the member block continues.
#[test]
fn diff_sig_member_when_then_sibling() {
    assert_sig_asts_match(
        "namespace N\ntype I =\n  abstract M : 'T -> 'T when 'T : comparison\n  abstract N : int\n",
    );
}

// (The *blockless* column-0 after-keyword-attribute regime with a member body —
// `type [<A>]`⏎`C =`⏎`  member M : int` — is invalid F# (FCS drops the whole
// file to empty decls), so it is not a differential case. Our recovery there
// (parse the member, keep a dedented sibling, no leak) is pinned our-side by the
// `sig_val_in_unsupported_type_body_does_not_leak` structural test instead.)

// ---- Phase 10.14 (slice 3b): inherit / interface / val-field member sigs ---
//
// The remaining `SynMemberSig` variants in a signature object-model body:
// `inherit T` (`Inherit`), `interface I` (`Interface`), and `val x : T`
// (`ValField`). Each reuses the impl-side member node (`INHERIT_MEMBER` /
// `INTERFACE_IMPL` / `VAL_FIELD`) and projects to the matching `NormalisedMember`
// variant (the sig FCS union differs from the impl one, but the normalised form
// is shared). The body still routes through `OBJECT_MODEL_REPR`.

/// `inherit Base` — a base-class clause in a signature.
#[test]
fn diff_sig_member_inherit() {
    assert_sig_asts_match("namespace N\ntype T =\n  inherit Base\n  abstract M : int\n");
}

/// A dotted/generic inherited type — `inherit Base<int>`.
#[test]
fn diff_sig_member_inherit_generic() {
    assert_sig_asts_match("namespace N\ntype T =\n  inherit Base<int>\n  abstract M : int\n");
}

/// A postfix-*application* inherited type — `inherit int list`. FCS's sig
/// `inherit` is `appTypeWithoutNull` (an app type), so the whole `int list` is
/// the base type (the impl-side `inherit` parses only an `atomType`).
#[test]
fn diff_sig_member_inherit_app_type() {
    assert_sig_asts_match("namespace N\ntype T =\n  inherit int list\n  abstract M : int\n");
}

/// `interface IFoo` — an interface clause in a signature.
#[test]
fn diff_sig_member_interface() {
    assert_sig_asts_match("namespace N\ntype T =\n  interface IFoo\n  abstract M : int\n");
}

/// A dotted interface type — `interface System.IDisposable`.
#[test]
fn diff_sig_member_interface_dotted() {
    assert_sig_asts_match(
        "namespace N\ntype T =\n  interface System.IDisposable\n  abstract M : int\n",
    );
}

/// `val x : int` — a field signature.
#[test]
fn diff_sig_member_val_field() {
    assert_sig_asts_match("namespace N\ntype T =\n  val x : int\n");
}

/// `val mutable count : int` — a mutable field signature.
#[test]
fn diff_sig_member_val_field_mutable() {
    assert_sig_asts_match("namespace N\ntype T =\n  val mutable count : int\n");
}

/// A generic field type — `val items : int list`.
#[test]
fn diff_sig_member_val_field_generic() {
    assert_sig_asts_match("namespace N\ntype T =\n  val items : int list\n");
}

/// A mixed object-model body — inherit, abstract member, and a val field.
#[test]
fn diff_sig_member_mixed() {
    assert_sig_asts_match(
        "namespace N\ntype T =\n  inherit Base\n  abstract M : int -> int\n  val x : int\n",
    );
}

/// An `inherit`/`val` body inside a nested module sig.
#[test]
fn diff_sig_member_3b_in_nested_module() {
    assert_sig_asts_match(
        "module M\nmodule Inner =\n  type T =\n    inherit Base\n    val x : int\n",
    );
}

// ---------------------------------------------------------------------------
// Attributes on top-level `val` signatures (FCS's `SynValSig.attributes`).
// Previously deferred ("a later phase-10 slice"); the attribute lists become
// leading `ATTRIBUTE_LIST` children of the `VAL_DECL`.
// ---------------------------------------------------------------------------

/// A single attribute on a `val` sig (`[<Literal>] val x : int`).
#[test]
fn diff_sig_val_one_attribute() {
    assert_sig_asts_match("module M\n[<Literal>]\nval x : int\n");
}

/// Multiple attribute lists stacked on a `val` sig.
#[test]
fn diff_sig_val_two_attribute_lists() {
    assert_sig_asts_match("module M\n[<Literal>]\n[<System.Obsolete>]\nval x : int\n");
}

/// Two attributes inside one `[< ; >]` list on a `val` sig.
#[test]
fn diff_sig_val_attribute_list_two() {
    assert_sig_asts_match("module M\n[<Literal; System.Obsolete>]\nval x : int\n");
}

/// An attribute carrying an argument (`[<CompiledName(\"X\")>]`).
#[test]
fn diff_sig_val_attribute_with_arg() {
    assert_sig_asts_match("module M\n[<CompiledName(\"X\")>]\nval x : int\n");
}

/// Same-line attribute then `val` (`[<Literal>] val x : int`).
#[test]
fn diff_sig_val_attribute_same_line() {
    assert_sig_asts_match("module M\n[<Literal>] val x : int\n");
}

/// An attributed `val` followed by a plain `val` — the attribute binds only the
/// first (no leak into the sibling).
#[test]
fn diff_sig_val_attribute_then_plain() {
    assert_sig_asts_match("module M\n[<Literal>]\nval x : int\nval y : string\n");
}

// ---------------------------------------------------------------------------
// Attributes on top-level `type` signatures (FCS's `SynComponentInfo.attributes`
// on a `SynTypeDefnSig`). The attribute lists become leading `ATTRIBUTE_LIST`
// children of the first `TYPE_DEFN`, exactly like the impl-side `[<A>] type T`.
// ---------------------------------------------------------------------------

/// A single attribute on a type-abbreviation sig (`[<Measure>] type m = int`).
#[test]
fn diff_sig_type_one_attribute() {
    assert_sig_asts_match("module M\n[<Measure>]\ntype m = int\n");
}

/// A single attribute on an opaque (bodyless) type sig (`[<Sealed>] type T`).
#[test]
fn diff_sig_type_attribute_opaque() {
    assert_sig_asts_match("module M\n[<Sealed>]\ntype T\n");
}

/// Two stacked attribute lists on a type sig.
#[test]
fn diff_sig_type_two_attribute_lists() {
    assert_sig_asts_match("module M\n[<Sealed>]\n[<System.Obsolete>]\ntype T = int\n");
}

/// An attribute carrying an argument on a type sig.
#[test]
fn diff_sig_type_attribute_with_arg() {
    assert_sig_asts_match("module M\n[<CompiledName(\"X\")>]\ntype T = int\n");
}

/// Same-line attribute then `type` (`[<Sealed>] type T = int`).
#[test]
fn diff_sig_type_attribute_same_line() {
    assert_sig_asts_match("module M\n[<Sealed>] type T = int\n");
}

/// An attributed `type` followed by a plain `val` — the attribute binds only
/// the type (no leak into the sibling decl).
#[test]
fn diff_sig_type_attribute_then_val() {
    assert_sig_asts_match("module M\n[<Sealed>]\ntype T = int\nval y : string\n");
}

// ---- Phase 10.14 (slice 3c): explicit class/struct/interface-end bodies -----
//
// `type T = class … end` / `struct … end` / `interface … end` in a signature —
// FCS's `SynTypeDefnSigRepr.ObjectModel(Class|Struct|Interface, SynMemberSig
// list, _)`. The explicit-kind wrapper around the slice-3a/3b member sigs; the
// `kind` discriminant + member projection are already in the normaliser, so the
// member sigs reuse the 3a/3b loop. The body is delimited by `end`.

/// `type T = class … end` with member/abstract sigs.
#[test]
fn diff_sig_kind_class() {
    assert_sig_asts_match(
        "namespace N\ntype T = class\n  abstract M : int\n  member P : int\nend\n",
    );
}

/// `type T = struct … end` with `val` fields (`struct` opens no inner block).
#[test]
fn diff_sig_kind_struct() {
    assert_sig_asts_match("namespace N\ntype T = struct\n  val x : int\n  val y : string\nend\n");
}

/// `type T = interface … end` with abstract members.
#[test]
fn diff_sig_kind_interface() {
    assert_sig_asts_match("namespace N\ntype T = interface\n  abstract M : int -> int\nend\n");
}

/// A `class … end` mixing inherit, member, and val-field sigs.
#[test]
fn diff_sig_kind_class_mixed() {
    assert_sig_asts_match(
        "namespace N\ntype T = class\n  inherit Base\n  abstract M : int\n  val x : int\nend\n",
    );
}

/// A generic explicit-kind type — `type Box<'a> = class … end`.
#[test]
fn diff_sig_kind_class_generic() {
    assert_sig_asts_match("namespace N\ntype Box<'a> = class\n  abstract Get : 'a\nend\n");
}

/// The kind keyword on the line *after* `=` (`type T =`⏎`  class … end`) — FCS's
/// explicit-end form leaves an extra `OBLOCKSEP` after `end` (before the outer
/// body block's `OBLOCKEND`), which must be skipped so this valid body is not
/// mistaken for an unsupported one.
#[test]
fn diff_sig_kind_class_offside_keyword() {
    assert_sig_asts_match("namespace N\ntype T =\n  class\n    abstract M : int\n  end\n");
}

/// An empty `class … end` (no members). FCS emits the recovery AST but marks
/// this signature form as erroneous — the col-0 `end` is offside of the `class`
/// body block, an FS0058. Since the §A offside emission landed we report the
/// matching offside error while recovering the identical class-body shape, so
/// both sides now error and their trees agree.
#[test]
fn diff_sig_kind_class_empty() {
    assert_sig_asts_match_allow_errors("namespace N\ntype T = class\nend\n");
}

/// An explicit-kind body under a whole-file `module` header, with a sibling.
#[test]
fn diff_sig_kind_struct_then_val() {
    assert_sig_asts_match("module M\ntype T = struct\n  val x : int\nend\nval mk : unit -> T\n");
}

/// An explicit-kind body inside a nested module sig.
#[test]
fn diff_sig_kind_in_nested_module() {
    assert_sig_asts_match(
        "module M\nmodule Inner =\n  type T = interface\n    abstract M : int\n  end\n",
    );
}

// ---- Phase 10.14 (slice 3e): property get/set + new-ctor member sigs --------
//
// The remaining `SynMemberSig.Member` sub-forms inside a type body: a property
// signature with `with get[, set]` accessors, and a `new : … -> T` constructor
// sig. Both project to the shared `AbstractSlot` (the property kind is in FCS's
// `flags`, elided; the ctor's leading keyword is `New`, name "new"), so the
// normaliser is largely reused — property get/set needs no FCS-side change, and
// the ctor reuses `fcs_member_sig`'s `Member` arm.

/// An abstract property with a `get` accessor.
#[test]
fn diff_sig_member_property_get() {
    assert_sig_asts_match("namespace N\ntype I =\n  abstract P : int with get\n");
}

/// An abstract property with `get, set`.
#[test]
fn diff_sig_member_property_get_set() {
    assert_sig_asts_match("namespace N\ntype I =\n  abstract P : int with get, set\n");
}

/// A concrete member property with `get, set`.
#[test]
fn diff_sig_member_property_member_get_set() {
    assert_sig_asts_match("namespace N\ntype T =\n  member P : int with get, set\n");
}

/// A property `set`-only accessor.
#[test]
fn diff_sig_member_property_set() {
    assert_sig_asts_match("namespace N\ntype I =\n  abstract P : int with set\n");
}

/// A property sig alongside a plain member, then a sibling — the get/set clause
/// must not leak into the next member or the module.
#[test]
fn diff_sig_member_property_then_member() {
    assert_sig_asts_match(
        "module M\ntype I =\n  abstract P : int with get, set\n  abstract M : int\nval mk : unit -> I\n",
    );
}

/// A `new : unit -> T` constructor sig in a `class … end` body.
#[test]
fn diff_sig_member_new_ctor() {
    assert_sig_asts_match("namespace N\ntype T = class\n  new : unit -> T\nend\n");
}

/// A `new` ctor with a tupled argument type.
#[test]
fn diff_sig_member_new_ctor_tupled() {
    assert_sig_asts_match("namespace N\ntype T = class\n  new : int * string -> T\nend\n");
}

/// A `new` ctor alongside an abstract member in a class body.
#[test]
fn diff_sig_member_new_ctor_mixed() {
    assert_sig_asts_match(
        "namespace N\ntype T = class\n  new : unit -> T\n  abstract M : int\nend\n",
    );
}

// ---- Phase 10.14 (slice V1): name-position accessibility on member sigs -------
//
// FCS's `classMemberSpfn` (`pars.fsy:969`) takes an `opt_access` modifier right
// before the member name (its `$5`): `member private M`, `static member internal
// M`. It is valid only on *concrete* members — abstract members reject all
// accessibility (`parsAccessibilityModsIllegalForAbstract`, see the allow-errors
// cases below). Accessibility is elided by the normaliser, so a valid form
// projects identically to its plain counterpart; we just consume the modifier as
// an `ACCESS_TOK` (the precedent is the top-level `val` sig's `opt_access`).

/// `member private M : int` — a `private` member.
#[test]
fn diff_sig_member_private() {
    assert_sig_asts_match("namespace N\ntype T =\n  member private M : int\n");
}

/// `member internal M : int` — an `internal` member.
#[test]
fn diff_sig_member_internal() {
    assert_sig_asts_match("namespace N\ntype T =\n  member internal M : int\n");
}

/// `member public M : int` — a `public` member.
#[test]
fn diff_sig_member_public() {
    assert_sig_asts_match("namespace N\ntype T =\n  member public M : int\n");
}

/// `static member private M : int -> I` — accessibility on a static member.
#[test]
fn diff_sig_member_static_private() {
    assert_sig_asts_match("namespace N\ntype I =\n  static member private M : int -> I\n");
}

/// Accessibility combined with a property accessor clause — `member private P :
/// int with get, set`.
#[test]
fn diff_sig_member_private_property() {
    assert_sig_asts_match("namespace N\ntype T =\n  member private P : int with get, set\n");
}

/// A `private` member alongside a plain abstract sibling and a module-level
/// sibling — the access modifier must not leak into the next member or the module.
#[test]
fn diff_sig_member_private_then_siblings() {
    assert_sig_asts_match(
        "module M\ntype T =\n  member private M : int\n  abstract N : int\nval z : int\n",
    );
}

/// `abstract member private M` — FCS rejects accessibility on an abstract member
/// (FS561 `parsAccessibilityModsIllegalForAbstract`) but recovers, building the
/// abstract slot with the modifier dropped. We flag it likewise; both sides
/// project the same `member`-keyworded abstract slot (accessibility elided).
#[test]
fn diff_sig_member_abstract_member_access_rejected() {
    assert_sig_asts_match_allow_errors(
        "namespace N\ntype T =\n  abstract member private M : int\n",
    );
}

/// `abstract private M` — the `abstract`-only abstract slot likewise rejects
/// accessibility (FS561) and recovers.
#[test]
fn diff_sig_member_abstract_access_rejected() {
    assert_sig_asts_match_allow_errors("namespace N\ntype T =\n  abstract private M : int\n");
}

// ---- Phase 10.14 (slice V2): leading accessibility on `new` + accessor access -
//
// FCS's `classMemberSpfn` rejects a *leading* `opt_access` (before the keyword) on
// every member-spec form EXCEPT the `new` ctor (`pars.fsy:1040`,
// `opt_access NEW COLON …`), where it is the ctor's visibility. Accessor-level
// access (`opt_access` before `get`/`set`, `pars.fsy:1072/1081`) is valid on a
// concrete property. All elided by the normaliser, so each diff-matches its plain
// counterpart once the modifier is consumed as an `ACCESS_TOK`.

/// `private new : unit -> T` — leading accessibility on a constructor sig.
#[test]
fn diff_sig_member_new_ctor_private() {
    assert_sig_asts_match("namespace N\ntype T = class\n  private new : unit -> T\nend\n");
}

/// `internal new : unit -> T`.
#[test]
fn diff_sig_member_new_ctor_internal() {
    assert_sig_asts_match("namespace N\ntype T = class\n  internal new : unit -> T\nend\n");
}

/// `public new : unit -> T`.
#[test]
fn diff_sig_member_new_ctor_public() {
    assert_sig_asts_match("namespace N\ntype T = class\n  public new : unit -> T\nend\n");
}

/// A plain `new` ctor followed by a `private new` ctor — the leading modifier on
/// the *second* member must be recognised across the inter-member separator (the
/// `sig_member_item_follows_block_sep` look-past gate).
#[test]
fn diff_sig_member_new_ctor_private_after_plain() {
    assert_sig_asts_match(
        "namespace N\ntype T = class\n  new : unit -> T\n  private new : int -> T\nend\n",
    );
}

/// A `private new` alongside an abstract member — the leading access on the ctor
/// must not bleed into the sibling member.
#[test]
fn diff_sig_member_new_ctor_private_mixed() {
    assert_sig_asts_match(
        "namespace N\ntype T = class\n  private new : unit -> T\n  abstract M : int\nend\n",
    );
}

/// Accessor-level accessibility — `member P : int with get, private set` (a
/// `private` setter). Already consumed by `parse_member_sig_get_set_clause`; this
/// locks in the FCS parity.
#[test]
fn diff_sig_member_accessor_private_set() {
    assert_sig_asts_match("namespace N\ntype T =\n  member P : int with get, private set\n");
}

/// Accessor-level accessibility on a lone getter — `with private get`.
#[test]
fn diff_sig_member_accessor_private_get() {
    assert_sig_asts_match("namespace N\ntype T =\n  member P : int with private get\n");
}

// ---- Phase 10.14 (slice 4): `with`-augmentation type sigs -------------------
//
// FCS's `tyconSpfn` second alternative `typeNameInfo opt_classSpfn`
// (`pars.fsy:820`) lowers a *bodyless* type carrying a `with`-augmentation —
// `type T with member M : int` — to `SynTypeDefnSig(info, repr =
// Simple(SynTypeDefnSimpleRepr.None), members, …)`: the repr stays **None**
// (an opaque type), and the augmentation's member *sigs* (`SynMemberSig list`)
// land in the *outer* `SynTypeDefnSig.members` slot — unlike the impl side,
// where `type T with member …` is an `ObjectModel(Augmentation)` repr. The
// filtered stream after `with` is identical to the 10.15 exception
// augmentation, so the shared `parse_with_augmentation_members(sig = true)`
// primitive parses the member sigs into the outer slot; the `with` is a plain
// `WITH_TOK` direct child of `TYPE_DEFN` (no `OBJECT_MODEL_REPR` marker), so
// `repr()` stays absent → `None`, matching FCS. The members project via the
// existing `normalise_member` `MemberSig` arm, and the FCS side reads field 2
// (`members`) through `fcs_member_sig`.

/// A single-member-sig augmentation on a bodyless type — the canonical opaque
/// type with one member signature.
#[test]
fn diff_sig_type_augment_member() {
    assert_sig_asts_match("namespace N\ntype T with member M : int\n");
}

/// An augmentation whose member sig is a function type.
#[test]
fn diff_sig_type_augment_member_fun() {
    assert_sig_asts_match("namespace N\ntype T with member Add : int -> int -> int\n");
}

/// A two-member augmentation in the offside block form (exercises the
/// inter-member `OBLOCKSEP` continuation in the outer slot).
#[test]
fn diff_sig_type_augment_two_members() {
    assert_sig_asts_match("namespace N\ntype T with\n  member M : int\n  member N : string\n");
}

/// A `static member` sig in a type augmentation.
#[test]
fn diff_sig_type_augment_static_member() {
    assert_sig_asts_match("namespace N\ntype T with\n  static member Make : int -> T\n");
}

/// An `abstract` member sig in a type augmentation.
#[test]
fn diff_sig_type_augment_abstract_member() {
    assert_sig_asts_match("namespace N\ntype T with\n  abstract M : int\n");
}

/// A generic bodyless type carrying an augmentation — `type Box<'a> with …`.
#[test]
fn diff_sig_type_augment_generic() {
    assert_sig_asts_match("namespace N\ntype Box<'a> with member Get : 'a\n");
}

/// An augmentation followed by a sibling `val` — the augment's close virtuals
/// must drain so the sibling spec is reached (not swallowed).
#[test]
fn diff_sig_type_augment_then_sibling() {
    assert_sig_asts_match("module M\ntype T with member M : int\nval f : int\n");
}

/// An augmentation in a nested module body, with a following sibling.
#[test]
fn diff_sig_type_augment_in_nested_module() {
    assert_sig_asts_match(
        "module M\nmodule Inner =\n  type T with member M : int\n  val f : int\n",
    );
}

/// An augmentation under a whole-file `module` header (the other placement).
#[test]
fn diff_sig_type_augment_in_module() {
    assert_sig_asts_match("module M\ntype T with member M : int\n");
}

/// A bodyless-type augmentation closed by an explicit `end`
/// (`type T with member M : int end`) — the corpus `.fsi` shape. The `end` lands
/// as an inert `END_TOK` child of `TYPE_DEFN`; the projection matches the
/// offside-closed form.
#[test]
fn diff_sig_type_augment_member_end() {
    assert_sig_asts_match("namespace N\ntype T with\n  member M : int\n  end\n");
}

/// An *empty* explicit-`end` bodyless-type augmentation (`type T with end`) —
/// FCS-valid, repr stays `None`.
#[test]
fn diff_sig_type_augment_empty_end() {
    assert_sig_asts_match("namespace N\ntype T with end\n");
}

// ---- Phase 10.14 (slice 5): `and`-chained type signatures -------------------
//
// FCS's `tyconSpfn` chains via `AND tyconSpfn` (`pars.fsy:557`), so
// `type A = … and B = …` is **one** `SynModuleSigDecl.Types(types, _)` holding
// several `SynTypeDefnSig`s (verified against `fcs-dump`: one `Types` group, one
// definition per `and`). Each continuation is its own `TYPE_DEFN` leading with an
// `AND_TOK`; the `TYPE_DEFNS` group node and the `Types`-list projection already
// hold/iterate multiple definitions, so this is parser-only (it mirrors the impl
// `and`-chain loop in `parse_type_defn_at`). Before this slice the head parsed and
// the `and` was skipped as ERROR via `skip_unsupported_type_continuation`.

/// Two abbreviations joined by `and` — the canonical mutually-referential pair.
#[test]
fn diff_sig_type_and_chain_abbrev() {
    assert_sig_asts_match("namespace N\ntype A = int\nand B = string\n");
}

/// A bodyless `and`-chain — `type A`⏎`and B` (both `Simple(None)`); FCS keeps both
/// in one group even with absent bodies.
#[test]
fn diff_sig_type_and_chain_bodyless() {
    assert_sig_asts_match("namespace N\ntype A\nand B\n");
}

/// A structural-repr continuation — an abbreviation head, a record/union `and`
/// continuation (each repr closes its own offside block before the `and`).
#[test]
fn diff_sig_type_and_chain_structural() {
    assert_sig_asts_match("namespace N\ntype R = { X : int }\nand U = A | B of int\n");
}

/// Three definitions in one `and`-chain.
#[test]
fn diff_sig_type_and_chain_three() {
    assert_sig_asts_match("namespace N\ntype A = int\nand B = string\nand C = bool\n");
}

/// Accessibility on a continuation — `and internal U = string`
/// (`SynComponentInfo` access on the second definition).
#[test]
fn diff_sig_type_and_chain_access() {
    assert_sig_asts_match("namespace N\ntype T = int\nand internal U = string\n");
}

/// An after-keyword attribute on a continuation — `and [<…>] U = string` (the
/// attribute attaches to the continuation's `SynComponentInfo.attributes`).
#[test]
fn diff_sig_type_and_chain_attr_continuation() {
    assert_sig_asts_match("namespace N\ntype T = int\nand [<System.Obsolete>] U = string\n");
}

/// A generic head with an inside-`<>` `when` clause plus a continuation —
/// exercises the header typar/constraint parse before the `and`-loop.
#[test]
fn diff_sig_type_and_chain_generic_when() {
    assert_sig_asts_match("namespace N\ntype A<'T when 'T : comparison> = 'T list\nand B = int\n");
}

/// An `and`-chain under a whole-file `module` header (the other placement).
#[test]
fn diff_sig_type_and_chain_in_module() {
    assert_sig_asts_match("module M\ntype A = int\nand B = string\n");
}

/// An `and`-chain inside a *nested* module signature body.
#[test]
fn diff_sig_type_and_chain_in_nested_module() {
    assert_sig_asts_match("module M\nmodule Inner =\n  type A = int\n  and B = string\n");
}

/// An `and`-chain interleaved with a sibling `val` after the group closes — the
/// last definition's body close must drain so the `val` is reached, not swallowed.
#[test]
fn diff_sig_type_and_chain_then_sibling_val() {
    assert_sig_asts_match("module M\ntype A = int\nand B = string\nval f : int\n");
}

// (A *single-line* chain (`type A = int and B = string`) is invalid F# — FCS
// rejects it and drops the chain. The recovery shape is not byte-faithful to
// FCS, so it is pinned by the `inline_sig_type_and_chain_is_rejected` structure
// test (lossless + rejected) rather than a diff test, mirroring the impl-side
// `inline_type_and_chain_is_rejected`.)

// ---- Phase 10.14 (slice 6): trailing member sigs on a structural/abbrev repr -
//
// FCS's `tyconSpfnRhs` is `tyconDefnOrSpfnSimpleRepr opt_classSpfn` and the
// `#light` `tyconSpfnRhsBlock` admits a bare `classSpfnMembers` run after the
// repr (`pars.fsy:838`), so a structural / abbreviation repr can carry trailing
// member *sigs* — `type R = { … } with member …`, the bare offside form
// `type R =`⏎`  { … }`⏎`  member …`, and the same on unions/enums/abbreviations.
// The repr stays `Simple(Record|Union|Enum|TypeAbbrev)` and the member sigs land
// in the **outer** `SynTypeDefnSig.members` slot (verified against `fcs-dump`),
// exactly like the slice-4 bodyless `with`-augmentation but over a non-`None`
// repr. They reuse `parse_with_augmentation_members(sig = true)` (the `with`
// form) and `parse_sig_member_block_items` (the bare form), emitted as direct
// `MEMBER_SIG` children of the `TYPE_DEFN` after the repr node; the normaliser
// already projects the outer `members()` slot.

/// A record repr with a trailing `with member …` sig.
#[test]
fn diff_sig_type_record_with_member() {
    assert_sig_asts_match("namespace N\ntype R = { x : int } with member M : int\n");
}

/// A union repr with a trailing `with member …` sig.
#[test]
fn diff_sig_type_union_with_member() {
    assert_sig_asts_match("namespace N\ntype U = A | B with member M : int\n");
}

/// An enum repr with a trailing `with member …` sig (FCS accepts it in a `.fsi`).
#[test]
fn diff_sig_type_enum_with_member() {
    assert_sig_asts_match("namespace N\ntype E = A = 0 | B = 1 with member M : int\n");
}

/// An abbreviation repr with a trailing `with member …` sig.
#[test]
fn diff_sig_type_abbrev_with_member() {
    assert_sig_asts_match("namespace N\ntype T = int with member M : int\n");
}

/// A `with` augmentation in the offside block form, two members (one `static`).
#[test]
fn diff_sig_type_record_with_offside_members() {
    assert_sig_asts_match(
        "namespace N\ntype R = { x : int } with\n  member M : int\n  static member Make : int -> R\n",
    );
}

/// A *bare* (no `with`) offside trailing member on a record — the `#light` form
/// where the member sits at the repr's offside column.
#[test]
fn diff_sig_type_record_bare_trailing_member() {
    assert_sig_asts_match("namespace N\ntype R =\n  { x : int }\n  member M : int\n");
}

/// Two bare offside trailing members on a record.
#[test]
fn diff_sig_type_record_bare_trailing_two_members() {
    assert_sig_asts_match(
        "namespace N\ntype R =\n  { x : int }\n  member M : int\n  member N : string\n",
    );
}

/// A bare offside trailing member on a union.
#[test]
fn diff_sig_type_union_bare_trailing_member() {
    assert_sig_asts_match("namespace N\ntype U =\n  | A\n  | B\n  member M : int\n");
}

/// A bare offside `val`-field member at the record's member column — FCS routes it
/// to the type's outer members (a `ValField` member sig), *not* a top-level export.
#[test]
fn diff_sig_type_record_bare_trailing_val_field() {
    assert_sig_asts_match("module M\ntype R =\n  { x : int }\n  val y : int\n");
}

/// A *dedented* `val` after a record repr is a module-level sibling, not a member
/// — the column discrimination LexFilter draws between an `OBLOCKSEP` (member) and
/// the body-close `OBLOCKEND` (sibling). Pins that the bare-trailing-member gate
/// does not swallow a sibling `val` as an outer member.
#[test]
fn diff_sig_type_record_then_dedented_sibling_val() {
    assert_sig_asts_match("module M\ntype R = { x : int }\nval y : int\n");
}

/// An inline bare trailing member — `type R = { x : int } member M : int` (the
/// `opt_OBLOCKSEP` form, valid F#).
#[test]
fn diff_sig_type_record_inline_bare_member() {
    assert_sig_asts_match("namespace N\ntype R = { x : int } member M : int\n");
}

/// A trailing `with` member followed by a sibling `val` — the augment's close
/// virtuals must drain so the `val` is reached, not swallowed.
#[test]
fn diff_sig_type_record_with_member_then_sibling() {
    assert_sig_asts_match("module M\ntype R = { x : int } with member M : int\nval f : int\n");
}

/// A trailing `with` member then an `and`-chained continuation — the member
/// lands in `R`'s outer slot and the chain still folds `U` into the group.
#[test]
fn diff_sig_type_record_with_member_then_and() {
    assert_sig_asts_match("namespace N\ntype R = { x : int } with member M : int\nand U = int\n");
}

/// An *offside* record body with the `with` on its own (indented) line — the
/// record's block closes (its `OBLOCKEND` is consumed) before the `with`, so the
/// augmentation member still lands in the outer slot. Covers the layout where the
/// `with` is reached only after the body-close handling.
#[test]
fn diff_sig_type_record_offside_then_indented_with() {
    assert_sig_asts_match("namespace N\ntype R =\n  { x : int }\n  with member M : int\n");
}

/// An offside record body with a column-0 (undented) `with` — LexFilter's `with`
/// undentation grace; the member still lands in the outer slot.
#[test]
fn diff_sig_type_record_offside_then_undented_with() {
    assert_sig_asts_match("namespace N\ntype R =\n  { x : int }\nwith member M : int\n");
}

/// An offside record body with the `with` indented *deeper* than the repr — still
/// valid F# (the repr's block closes before the `with` regardless of its
/// indentation), so the member lands in the outer slot.
#[test]
fn diff_sig_type_record_offside_then_deeper_with() {
    assert_sig_asts_match("namespace N\ntype R =\n  { x : int }\n    with member M : int\n");
}

/// An abbreviation with the `with` on a deeper-indented following line.
#[test]
fn diff_sig_type_abbrev_then_deeper_with() {
    assert_sig_asts_match("namespace N\ntype T = int\n    with member M : int\n");
}

// ---- Phase 10.14 (slice 7): delegate type signatures ------------------------
//
// `type T = delegate of <topType>` — FCS's `tyconSpfnRhs`'s `DELEGATE OF topType`
// alternative (`pars.fsy`), lowered to `SynTypeDefnSigRepr.ObjectModel(
// SynTypeDefnKind.Delegate(ty, arity), [Invoke], _)`. We keep the surface
// `DELEGATE_REPR` node, reusing the impl-side `parse_delegate_repr` and the
// `NormalisedTypeRepr::Delegate` projection (which keeps only the signature `ty`;
// the `arity` and the synthetic `Invoke` slot are both derived from it). FCS
// forbids an augmentation on a delegate (`parsAugmentationsIllegalOnDelegateType`).

/// The minimal delegate signature — `delegate of int -> int`.
#[test]
fn diff_sig_type_delegate() {
    assert_sig_asts_match("namespace N\ntype T = delegate of int -> int\n");
}

/// A tupled-argument delegate — `delegate of int * int -> int`.
#[test]
fn diff_sig_type_delegate_tupled() {
    assert_sig_asts_match("namespace N\ntype T = delegate of int * int -> int\n");
}

/// A curried-argument delegate — `delegate of int -> int -> int`.
#[test]
fn diff_sig_type_delegate_curried() {
    assert_sig_asts_match("namespace N\ntype T = delegate of int -> int -> int\n");
}

/// A unit-argument delegate — `delegate of unit -> int`.
#[test]
fn diff_sig_type_delegate_unit() {
    assert_sig_asts_match("namespace N\ntype T = delegate of unit -> int\n");
}

/// A generic delegate — `type T<'a> = delegate of 'a -> int` (the header typar
/// reused from phase 9.3).
#[test]
fn diff_sig_type_delegate_generic() {
    assert_sig_asts_match("namespace N\ntype T<'a> = delegate of 'a -> int\n");
}

/// A delegate signature inside a nested module body.
#[test]
fn diff_sig_type_delegate_in_nested_module() {
    assert_sig_asts_match("module M\nmodule Inner =\n  type T = delegate of int -> int\n");
}

/// A delegate signature followed by a sibling `val` — the body close must drain
/// so the `val` is reached, not swallowed.
#[test]
fn diff_sig_type_delegate_then_sibling_val() {
    assert_sig_asts_match("module M\ntype T = delegate of int -> int\nval f : T\n");
}

// ---- Phase 10.14 (slice 8): attributed member signatures --------------------
//
// `[<…>] member …` / `[<…>] abstract …` / `[<…>] val …` inside an object-model
// (or augmentation / bare-trailing) signature body. FCS attaches the attribute
// lists to the member sig's `SynValSig.attributes` (member/abstract/static
// member) or `SynField.attributes` (val-field) — the repr stays `ObjectModel` /
// `Simple`. The member-block loop ([`parse_sig_member_block_items`]) parses a
// leading `[<…>]` run under a checkpoint and threads it into the member node
// (mirroring the impl-side `parse_member_block_items`), so the attributes become
// leading children that `MemberSig::attributes()` / the `val`-field facade read.
// (Attributed `inherit`/`interface` member sigs, and the same-line `OWITH`
// attributed-augment form `type T with [<A>] member …`, stay deferred.)

/// A single attribute on an `abstract` member sig.
#[test]
fn diff_sig_member_attr_abstract() {
    assert_sig_asts_match("namespace N\ntype T =\n  [<CLIEvent>] abstract M : int\n");
}

/// A single attribute on a concrete `member` sig.
#[test]
fn diff_sig_member_attr_member() {
    assert_sig_asts_match("namespace N\ntype T =\n  [<System.Obsolete>] member M : int\n");
}

/// A single attribute on a `static member` sig.
#[test]
fn diff_sig_member_attr_static_member() {
    assert_sig_asts_match(
        "namespace N\ntype T =\n  [<System.Obsolete>] static member Make : int -> T\n",
    );
}

/// An attribute on a `val`-field member sig (`SynField.attributes`).
#[test]
fn diff_sig_member_attr_val_field() {
    assert_sig_asts_match("namespace N\ntype T =\n  [<DefaultValue>] val mutable x : int\n");
}

/// An attribute on a `static val`-field member sig — `static` is a two-token
/// introducer prefix, so the attribute-aware lookahead must consume it and find
/// the `val` head in the same scope.
#[test]
fn diff_sig_member_attr_static_val_field() {
    assert_sig_asts_match("namespace N\ntype T =\n  [<DefaultValue>] static val x : int\n");
}

/// Two attribute lists on one member sig.
#[test]
fn diff_sig_member_attr_two_lists() {
    assert_sig_asts_match("namespace N\ntype T =\n  [<A>] [<B>] abstract M : int\n");
}

/// A plain member sig followed by an attributed one — the attribute attaches only
/// to the second.
#[test]
fn diff_sig_member_attr_plain_then_attributed() {
    assert_sig_asts_match(
        "namespace N\ntype T =\n  abstract A : int\n  [<CLIEvent>] abstract M : int\n",
    );
}

/// An attributed member sig as the *first* item — the entry gate must see through
/// the leading `[<…>]` to recognise the object-model body.
#[test]
fn diff_sig_member_attr_first_item() {
    assert_sig_asts_match(
        "namespace N\ntype T =\n  [<CLIEvent>] abstract M : int\n  abstract N : int\n",
    );
}

/// An attributed member sig inside an explicit `class … end` body (slice-3c
/// carrier).
#[test]
fn diff_sig_member_attr_in_class_end() {
    assert_sig_asts_match("namespace N\ntype T = class\n  [<CLIEvent>] abstract M : int\nend\n");
}

/// An attributed member sig in an offside `with`-augmentation (slice-4 carrier) —
/// it lands in the outer `SynTypeDefnSig.members` slot.
#[test]
fn diff_sig_member_attr_in_with_augment() {
    assert_sig_asts_match(
        "namespace N\ntype T = { x : int } with\n  [<CLIEvent>] abstract M : int\n",
    );
}

/// An attributed bare trailing member sig on an offside record (slice-6 carrier).
#[test]
fn diff_sig_member_attr_bare_trailing() {
    assert_sig_asts_match(
        "namespace N\ntype R =\n  { x : int }\n  [<CLIEvent>] abstract M : int\n",
    );
}

/// An attributed access-modified constructor as the *first* member — FCS's
/// `opt_attributes opt_access NEW`. The entry gate must look past both the
/// attribute run and the leading `private` to the `new`.
#[test]
fn diff_sig_member_attr_private_new_first_item() {
    assert_sig_asts_match("namespace N\ntype T =\n  [<System.Obsolete>] private new : unit -> T\n");
}

// ---- Phase 10.15 (first slice): exception signatures (no `with` members) ----
//
// `SynModuleSigDecl.Exception of SynExceptionSig * range`, where
// `SynExceptionSig(exnRepr, withKeyword, members, range)` shares the
// `SynExceptionDefnRepr` (and its field layout) with the impl-side
// `SynExceptionDefn` (phase 9.15a). So the `exconCore` forms — bare / `of` /
// abbreviation, with optional accessibility and attributes — reuse the impl
// `EXCEPTION_DEFN` node, its facade, and the `NormalisedExnDefn` projection
// (members empty here). The `with member …` augmentation (`opt_classSpfn`, whose
// members are member *sigs*) is a later slice.

/// The bare form `exception E` (a nullary exception sig).
#[test]
fn diff_sig_exception_bare() {
    assert_sig_asts_match("module M\nexception E\n");
}

/// A single anonymous payload field — `exception E of int`.
#[test]
fn diff_sig_exception_of_int() {
    assert_sig_asts_match("module M\nexception E of int\n");
}

/// A tupled payload — `exception E of int * string`.
#[test]
fn diff_sig_exception_of_tuple() {
    assert_sig_asts_match("module M\nexception E of int * string\n");
}

/// A named payload field — `exception E of x: int * string`.
#[test]
fn diff_sig_exception_of_named() {
    assert_sig_asts_match("module M\nexception E of x: int * string\n");
}

/// The abbreviation form `exception E = SomeExn` (the `=` is the abbreviation
/// target, not an enum value).
#[test]
fn diff_sig_exception_abbrev() {
    assert_sig_asts_match("module M\nexception E = SomeExn\n");
}

/// A dotted abbreviation target — `exception E = System.Exception`.
#[test]
fn diff_sig_exception_abbrev_dotted() {
    assert_sig_asts_match("module M\nexception E = System.Exception\n");
}

/// An accessibility modifier — `exception internal E` (FCS's `exconCore`
/// `opt_access`, valid on an exception).
#[test]
fn diff_sig_exception_access() {
    assert_sig_asts_match("module M\nexception internal E\n");
}

/// An exception sig under a `namespace` (the other header kind).
#[test]
fn diff_sig_exception_in_namespace() {
    assert_sig_asts_match("namespace N\nexception E of string\n");
}

/// An exception sig alongside a sibling `val` — two separate module sig decls.
#[test]
fn diff_sig_exception_then_val() {
    assert_sig_asts_match("module M\nexception E of int\nval f : unit -> int\n");
}

/// A leading attribute on an exception sig (`[<System.Obsolete>] exception E`,
/// FCS's `SynExceptionDefnRepr.attributes`).
#[test]
fn diff_sig_exception_attribute() {
    assert_sig_asts_match("module M\n[<System.Obsolete>]\nexception E of int\n");
}

/// An exception sig in a nested module body.
#[test]
fn diff_sig_exception_in_nested_module() {
    assert_sig_asts_match("module M\nmodule Inner =\n  exception E of int\n");
}

/// `;`-separated exception sigs on one line (FCS's `opt_seps`).
#[test]
fn diff_sig_exception_semi_separated() {
    assert_sig_asts_match("module M\nexception A of int; exception B of string\n");
}

// ---- Phase 10.15 (second slice): exception `with member …` augmentation -----
//
// FCS's `exconSpfn` is `exconCore opt_classSpfn`, where `opt_classSpfn = WITH
// classSpfnBlock declEnd` — the `with` augmentation whose members are member
// *sigs* (`SynMemberSig list`), landing in the outer `SynExceptionSig.members`
// slot. The filtered stream after `with` mirrors the impl 9.15b augmentation
// (`WITH OBLOCKBEGIN … OBLOCKEND ODECLEND`), so the framing is shared; only the
// member items differ — `parse_sig_member_block_items` (member sigs) instead of
// the impl's `parse_member_block_items` (member bodies). The members are direct
// `MEMBER_SIG` children of `EXCEPTION_DEFN` after the `WITH_TOK`, projected via
// the existing `normalise_member` `MemberSig` arm.

/// A single-member-sig augmentation on a bare exception.
#[test]
fn diff_sig_exception_augment_member() {
    assert_sig_asts_match("module M\nexception E with member M : int\n");
}

/// An augmentation on an `of`-fields exception — the `of` case data precedes the
/// `with`, and the member sig still lands in the outer slot.
#[test]
fn diff_sig_exception_augment_of_fields() {
    assert_sig_asts_match("module M\nexception E of int with member M : int -> int\n");
}

/// A two-member augmentation in the offside block form (exercises the
/// inter-member `OBLOCKSEP` continuation in the outer slot).
#[test]
fn diff_sig_exception_augment_two_members() {
    assert_sig_asts_match("module M\nexception E with\n  member M : int\n  member N : string\n");
}

/// A `static member` sig in an augmentation.
#[test]
fn diff_sig_exception_augment_static_member() {
    assert_sig_asts_match("module M\nexception E with\n  static member Make : int -> exn\n");
}

/// An augmentation followed by a sibling `val` — the augment's close virtuals
/// must drain so the sibling spec is reached (not swallowed).
#[test]
fn diff_sig_exception_augment_then_sibling() {
    assert_sig_asts_match("module M\nexception E with member M : int\nval f : int\n");
}

/// An augmentation in a nested module body, with a following sibling.
#[test]
fn diff_sig_exception_augment_in_nested_module() {
    assert_sig_asts_match(
        "module M\nmodule Inner =\n  exception E with member M : int\n  val f : int\n",
    );
}

/// An *attributed* `override`/`default` member sig (`[<A>] override M : int`) —
/// the attributed-member lookahead (`attributed_member_sig_follows_from`) must
/// admit the `override`/`default` introducer, like `member`/`abstract`.
#[test]
fn diff_sig_member_attributed_override_default() {
    assert_sig_asts_match(
        "namespace N\ntype T =\n  [<System.Obsolete>] override M : int\n  [<System.Obsolete>] default N : int\n",
    );
}

/// `abstract override` / `abstract default` are *not* legal introducer runs —
/// FCS's `classMemberSpfn` picks either `abstractMemberFlags` or `memberFlags`,
/// never both. `override`/`default` are standalone, so these combinations must
/// stay on the error path (FCS reports FS0010), not silently parse as an
/// `Override`/`Default` sig. Asserted directly rather than differentially: the
/// recovery tree has a nameless member sig the shared normaliser cannot project,
/// so check for a parse error + lossless round-trip instead.
#[test]
fn abstract_override_default_sig_is_rejected() {
    for src in [
        "namespace N\ntype T =\n  abstract override M : int\n",
        "namespace N\ntype T =\n  abstract default M : int\n",
    ] {
        let parse = parse_sig(src);
        assert!(
            !parse.errors.is_empty(),
            "{src:?}: `abstract override`/`default` must be a parse error",
        );
        assert_eq!(
            parse.root.text().to_string(),
            src,
            "{src:?}: recovery must stay lossless",
        );
    }
}

// ---- Opaque type followed by an abutting `val` spec -------------------------
//
// A bodyless `type Name` (no `=`) is an opaque `Simple(None)` type. When the
// following `val` sig is *indented* under it, the lex-filter emits no
// declaration separator (the `val` abuts the type header), yet FCS still closes
// the opaque type and parses the `val` as a *module-level* `SynModuleSigDecl.Val`
// — the val is promoted out of the type, not made a member of it. Only `val`
// (plain / `[<A>]`-attributed / `mutable`) is accepted this way; an abutting
// `member` is rejected (see `opaque_type_then_indented_member_is_rejected`).
// Seen in `tests/service/data/TestTP/ProvidedTypes.fsi` (`type Shape` + `val`s).

/// `type Shape`⏎`  val X : int` — the indented `val` promotes to a module-level
/// `Val` sibling of the opaque type.
#[test]
fn diff_sig_opaque_type_then_indented_val() {
    assert_sig_asts_match("module M\ntype Shape\n    val X : int\n    val Y : int\n");
}

/// The same, with an active-pattern `val (|Foo|Bar|)` name (as ProvidedTypes.fsi).
#[test]
fn diff_sig_opaque_type_then_active_pattern_val() {
    assert_sig_asts_match(
        "module M\ntype Shape\n    val (|Foo|Bar|) : int -> int\n    val Rebuild : int -> int\n",
    );
}

/// A `val mutable` promotes the same way.
#[test]
fn diff_sig_opaque_type_then_indented_val_mutable() {
    assert_sig_asts_match("module M\ntype Shape\n    val mutable X : int\n");
}

/// An *attributed* `val` (`[<A>] val X`) promotes too — FCS makes it a
/// module-level attributed `Val`. The boundary check looks past the attribute
/// run for the `val`.
#[test]
fn diff_sig_opaque_type_then_attributed_val() {
    assert_sig_asts_match(
        "module M\ntype Shape\n    [<System.Obsolete>] val X : int\n    val Y : int\n",
    );
}

/// But an *attributed non-`val`* (`[<A>] type …`) after an opaque type is not a
/// valid module decl — FCS rejects it, so we must keep rejecting (no promotion).
#[test]
fn opaque_type_then_attributed_non_val_is_rejected() {
    let src = "module M\ntype Shape\n    [<System.Obsolete>] type Y = int\n";
    let parse = parse_sig(src);
    assert!(
        !parse.errors.is_empty(),
        "{src:?}: an attributed non-`val` after an opaque type must stay a parse error",
    );
    assert_eq!(
        parse.root.text().to_string(),
        src,
        "{src:?}: recovery must stay lossless",
    );
}

/// An abutting `member` after an opaque type is *not* a valid module decl — FCS
/// rejects it, so we must keep rejecting (no phantom promotion).
#[test]
fn opaque_type_then_indented_member_is_rejected() {
    let src = "module M\ntype Shape\n    member A : int\n";
    let parse = parse_sig(src);
    assert!(
        !parse.errors.is_empty(),
        "{src:?}: an abutting `member` after an opaque type must stay a parse error",
    );
    assert_eq!(
        parse.root.text().to_string(),
        src,
        "{src:?}: recovery must stay lossless",
    );
}

/// The nameless-`namespace` leniency is **impl-only**: FCS recovers a bare
/// `namespace` / `namespace rec` in a *signature* file as an `AnonModule`
/// (dropping `rec`), not an empty `DeclaredNamespace`. Rather than mis-project a
/// `DeclaredNamespace` where FCS produced an `AnonModule` (a both-accept-but-
/// different divergence), the sig side keeps its pre-existing parse error; the
/// sig-specific `AnonModule` recovery is a separate deferred item. (This asserts
/// only *that* we reject — a `we_reject_fcs_accepts` residual, not an AST match.)
#[test]
fn nameless_namespace_in_sig_stays_a_parse_error() {
    for src in ["namespace\n", "namespace rec\n"] {
        let parse = parse_sig(src);
        assert!(
            !parse.errors.is_empty(),
            "{src:?}: a nameless namespace in a .fsi must stay a parse error \
             (the impl-only leniency must not leak to signatures)",
        );
        assert_eq!(
            parse.root.text().to_string(),
            src,
            "{src:?}: recovery must stay lossless",
        );
    }
}
