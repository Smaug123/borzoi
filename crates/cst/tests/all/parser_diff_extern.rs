//! Differential test (`parser::parse` vs FCS): `extern` DllImport declarations
//! (FCS's `cPrototype`, `pars.fsy:3186`). An `extern cRetType opt_access ident
//! ( externArgs )` lowers to a `SynModuleDecl.Let([binding])` whose binding has
//! `SynLeadingKeyword.Extern`, head pattern `LongIdent(name, Pats [Tuple
//! [Typed(Wild|Named, cType) …]])`, the C return type in `returnInfo` (elided by
//! the normaliser), and a synthetic `failwith "…"` RHS. The normaliser compares
//! the leading keyword, the attributes, the name+typed-arg pattern, and the
//! synthetic RHS.
//!
//! PR 1 covered the core grammar with **plain-path** argument types (`int`,
//! `string`, `System.IntPtr`, a bare `byref` path, …), `void`/typed returns,
//! `opt_access`, and leading + argument attributes. PR 2 adds the C-type
//! modifiers (`T&`, `T*`, `T[]`, `void*`).

use crate::common::assert_asts_match;
use borzoi_cst::parser::parse;
use borzoi_cst::syntax::{AstNode, ExternCTypeBase, ExternCTypeSuffix, ExternDecl};

fn extern_return_info(src: &str) -> borzoi_cst::syntax::ExternRet {
    parse(src)
        .root
        .descendants()
        .find_map(ExternDecl::cast)
        .expect("fixture must contain an extern declaration")
        .return_info()
        .expect("extern declaration must contain return info")
}

// ---- core forms -----------------------------------------------------------

/// The canonical no-arg `void` extern (`SynLeadingKeyword.Extern`, `unit`
/// return, empty arg tuple).
#[test]
fn diff_ast_extern_void_no_args() {
    assert_asts_match("extern void Meh()\n");
}

#[test]
fn extern_return_is_void_is_bare_void_only() {
    let bare_void = extern_return_info("extern void Meh()\n");
    assert!(bare_void.is_void());
    assert!(bare_void.ty().is_none());
    assert!(
        matches!(bare_void.c_type_base(), Some(ExternCTypeBase::Void(tok)) if tok.text() == "void")
    );
    assert_eq!(bare_void.c_type_suffixes().count(), 0);

    let void_ptr = extern_return_info("extern void* Read()\n");
    assert!(!void_ptr.is_void());
    assert!(void_ptr.ty().is_none());
    assert!(
        matches!(void_ptr.c_type_base(), Some(ExternCTypeBase::Void(tok)) if tok.text() == "void")
    );
    let suffixes: Vec<_> = void_ptr.c_type_suffixes().collect();
    assert!(
        matches!(suffixes.as_slice(), [ExternCTypeSuffix::Pointer(star)] if star.text() == "*")
    );
}

/// A typed return with no args (`extern int a()`).
#[test]
fn diff_ast_extern_typed_return_no_args() {
    assert_asts_match("extern int a()\n");
}

/// A single named path-typed argument (`extern int A(int a)`).
#[test]
fn diff_ast_extern_one_arg() {
    assert_asts_match("extern int A(int a)\n");
}

/// A `string` argument (the `SanityCheck01`/`puts` shape).
#[test]
fn diff_ast_extern_string_arg() {
    assert_asts_match("extern int puts(string c)\n");
}

/// Two path-typed arguments (`extern bool Beep(int frequency, int duration)`).
#[test]
fn diff_ast_extern_two_args() {
    assert_asts_match("extern bool Beep(int frequency, int duration)\n");
}

/// A dotted-path argument type and a bare `byref` *path* (not the `&` modifier)
/// — the `SyntaxTree/Extern/Extern 01.fs` shape (return type `ReturnCode`).
#[test]
fn diff_ast_extern_dotted_and_byref_path() {
    assert_asts_match("extern ReturnCode GetParent(System.IntPtr inRef, byref outParentRef)\n");
}

/// An *unnamed* argument (`externArg = opt_attributes cType` → `Typed(Wild, …)`).
#[test]
fn diff_ast_extern_unnamed_arg() {
    assert_asts_match("extern bool Contains(ExplicitPoint)\n");
}

// ---- opt_access -----------------------------------------------------------

/// `private` between the return type and the name (`extern int private c()`).
#[test]
fn diff_ast_extern_access_private() {
    assert_asts_match("extern int private c()\n");
}

/// `public` accessibility (`extern int public b()`).
#[test]
fn diff_ast_extern_access_public() {
    assert_asts_match("extern int public b()\n");
}

// ---- attributes -----------------------------------------------------------

/// The headline use: a leading `[<DllImport(...)>]` attribute attaches to the
/// binding (FCS's `SynBinding.attributes`).
#[test]
fn diff_ast_extern_dllimport_attr() {
    assert_asts_match("[<DllImport(\"msvcrt.dll\")>]\nextern int puts(string c)\n");
}

/// An attribute on an argument (`extern int A([<myAttrib>] int a)` — the
/// `InExternDecl.fs` regression shape).
#[test]
fn diff_ast_extern_arg_attr() {
    assert_asts_match("extern int A([<myAttrib>] int a)\n");
}

// ---- C type modifiers -----------------------------------------------------

/// A postfix managed-byref argument (`T&`) with no argument name.
#[test]
fn diff_ast_extern_postfix_byref_unnamed_arg() {
    assert_asts_match("extern bool PointInRect(ExplicitRect&, ExplicitPoint)\n");
}

/// A postfix managed-byref argument (`T&`) with an argument name.
#[test]
fn diff_ast_extern_postfix_byref_named_arg() {
    assert_asts_match("extern bool PointInRect(ExplicitRect& rect, ExplicitPoint pt)\n");
}

/// FCS accepts whitespace before the postfix `&`.
#[test]
fn diff_ast_extern_postfix_byref_spaced_named_arg() {
    assert_asts_match("extern bool CopyFile(char [] src, char [] dst, bool & overwrite)\n");
}

/// Array suffixes are part of the C type in extern prototypes.
#[test]
fn diff_ast_extern_array_args() {
    assert_asts_match("extern bool CopyFile(char[] src, char [] dst)\n");
}

/// Native-pointer suffixes are part of the C type in extern prototypes.
#[test]
fn diff_ast_extern_pointer_arg() {
    assert_asts_match("extern int Read(byte* buffer)\n");
}

/// C-type suffixes recurse: `byte*[]` is an array of native pointers.
#[test]
fn diff_ast_extern_pointer_array_arg() {
    assert_asts_match("extern int Read(byte*[] buffer)\n");
}

/// C-type suffixes recurse: `byte* *` is a native pointer to native pointer.
#[test]
fn diff_ast_extern_pointer_pointer_arg() {
    assert_asts_match("extern int Read(byte* * buffer)\n");
}

/// Argument attributes compose with the C-type modifier forms.
#[test]
fn diff_ast_extern_arg_attr_with_byref() {
    assert_asts_match(
        "extern bool CopyFile([<SomeAttrib>] char [] src, [<SomeAttrib()>] bool & overwrite)\n",
    );
}

/// The C `void*` spelling is a native pointer over void, not the return-only
/// `void` special case.
#[test]
fn diff_ast_extern_void_pointer_arg() {
    assert_asts_match("extern int Read(void* buffer)\n");
}

/// `void*` is also legal as an extern return `cType`.
#[test]
fn diff_ast_extern_void_pointer_return() {
    assert_asts_match("extern void* Read()\n");
}

/// The special extern return `void` case is bare only; `void*` comes through
/// `cType`, but non-pointer void suffixes remain parse errors.
#[test]
fn extern_void_return_rejects_non_pointer_suffixes() {
    use borzoi_cst::parser::parse;

    for src in ["extern void& F()\n", "extern void[] F()\n"] {
        let p = parse(src);
        assert!(
            !p.errors.is_empty(),
            "extern void return suffix must error like FCS does: {src:?}",
        );
        assert_eq!(p.root.text().to_string(), src, "lossless: {src:?}");
    }
}

/// An extern array suffix is only valid when the closing `]` is present.
#[test]
fn extern_array_suffix_requires_closing_bracket() {
    use borzoi_cst::parser::parse;

    for src in ["extern int F(char[ x)\n", "extern int F(char[)\n"] {
        let p = parse(src);
        assert!(
            !p.errors.is_empty(),
            "unterminated extern array suffix must error like FCS does: {src:?}",
        );
        assert_eq!(p.root.text().to_string(), src, "lossless: {src:?}");
    }
}

// ---- placement / separators ----------------------------------------------

/// An extern inside a module body, followed by a sibling `let`.
#[test]
fn diff_ast_extern_in_module_then_let() {
    assert_asts_match("module M\nextern void Meh()\nlet y = 1\n");
}

/// A trailing `;` top-level separator (the `SanityCheck01` shape).
#[test]
fn diff_ast_extern_trailing_semicolon() {
    assert_asts_match("extern int puts(string c);\n");
}

// ---- name grammar: `ident`, not a path ------------------------------------

/// The prototype name is FCS's `ident` — a *single* identifier. A dotted name
/// (`extern int A.B()`) or `global` (`extern int global()`) is an FCS parse
/// error, so we must reject them too rather than claiming a `LONG_IDENT` path and
/// producing a valid-looking prototype. Pins that both forms error and stay
/// lossless.
#[test]
fn extern_non_ident_name_rejects() {
    use borzoi_cst::parser::parse;
    for src in ["extern int A.B()\n", "extern int global()\n"] {
        let p = parse(src);
        assert!(
            !p.errors.is_empty(),
            "extern with a non-ident name must error (FCS does): {src:?}"
        );
        assert_eq!(p.root.text().to_string(), src, "lossless: {src:?}");
    }
}
