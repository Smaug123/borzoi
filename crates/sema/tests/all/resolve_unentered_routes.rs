//! Regressions for the declarations that bind a bare name without a scope
//! entry (`resolve/surface.rs`), on shapes `auto_open_member_sweep` cannot
//! build: its probes are the bare name `Nn`, which is never a real type and
//! never a dotted head.
//!
//! Each case declines a *wrong* answer in the sweep and must leave the
//! ordinary code around it resolving exactly as before.

use borzoi_cst::parser::parse;
use borzoi_cst::syntax::{AstNode, ImplFile};
use borzoi_sema::{ProjectItems, Resolution, ResolvedFile, SyntaxRecovery, resolve_file};
use rowan::TextRange;

fn resolve(src: &str) -> ResolvedFile {
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "parse errors in {src:?}");
    let recovery = SyntaxRecovery::of(&parsed);
    let file = ImplFile::cast(parsed.root).expect("impl file");
    resolve_file(
        &file,
        &ProjectItems::default(),
        crate::common::fsharp_core_env(),
        &recovery,
    )
}

/// The range of the last occurrence of `needle`.
fn last(src: &str, needle: &str) -> TextRange {
    let i = src.rfind(needle).expect("needle in source");
    TextRange::new(
        u32::try_from(i).expect("fits").into(),
        u32::try_from(i + needle.len()).expect("fits").into(),
    )
}

/// A `rec` block declines the bare names its declarations bind from its start,
/// but its constructible types push no entry: they enter the slot at the
/// block's start, in the slot account that dotted heads read too. An entry
/// under the type's name would blind the dotted head that reads the type's own
/// static member, which is ordinary code in a `module rec`.
#[test]
fn a_rec_block_type_still_qualifies_its_own_static_member() {
    for src in [
        "module rec M\ntype Color() =\n    static member Red = 1\nlet x = Color.Red\n",
        "module M\nmodule rec R =\n    type Color() =\n        static member Red = 1\n    let x = Color.Red\n",
        "namespace rec N\ntype Color() =\n    static member Red = 1\n    static member Use = Color.Red\n",
        // An outer value of the name loses the slot to the block's type from the
        // block's start, so the head is still the type.
        "module Root\nlet Color = 42\nmodule rec R =\n    type Color() =\n        static member Red = 1\n    let probe = Color.Red\n",
    ] {
        let rf = resolve(src);
        assert!(
            matches!(
                rf.resolution_at(last(src, "Color.Red")),
                Some(Resolution::Local(_))
            ),
            "`Color.Red` names the static member in {src:?}, got {:?}",
            rf.resolution_at(last(src, "Color.Red"))
        );
    }
}

/// A bar-less `type Name = string` is an abbreviation, because FSharp.Core's
/// `string` is a type in scope — even in an env without the BCL, where the
/// abbreviation's target cannot be chased. So it declares no case, and a later
/// `string 5` is still FSharp.Core's function. Inside a `rec` block too, where
/// the right-hand side defers as a possible forward type.
#[test]
fn an_abbreviation_of_a_primitive_declares_no_case() {
    for src in [
        "module M\ntype Name = string\nlet x = string 5\n",
        "module M\ntype Name = int\nlet x = int \"5\"\n",
        "module rec M\ntype Name = string\nlet x = string 5\n",
        "module M\nmodule rec R =\n    type Name = string\n    let x = string 5\n",
    ] {
        let rf = resolve(src);
        let name = if src.contains("int \"") {
            "int"
        } else {
            "string"
        };
        assert!(
            matches!(
                rf.resolution_at(last(src, name)),
                Some(Resolution::Member { .. })
            ),
            "`{name}` is FSharp.Core's function in {src:?}, got {:?}",
            rf.resolution_at(last(src, name))
        );
    }
}

/// …while one whose right-hand side names no type is a union, and its case
/// takes the name from what was in scope: the use declines rather than binding
/// the earlier `let`.
#[test]
fn a_barless_case_takes_the_name_from_an_earlier_binder() {
    for src in [
        "module M\nlet Nn = 1\ntype T = Nn\nlet x = Nn\n",
        "module M\nlet Nn = 1\nmodule rec R =\n    let x = Nn\n    type T = Nn\n",
    ] {
        let rf = resolve(src);
        let use_of = {
            let x = last(src, "x = Nn");
            TextRange::new(x.end() - rowan::TextSize::from(2), x.end())
        };
        assert!(
            matches!(
                rf.resolution_at(use_of),
                None | Some(Resolution::Deferred(_))
            ),
            "the use of `Nn` declines in {src:?}, got {:?}",
            rf.resolution_at(use_of)
        );
    }
}

/// A lexical binder outranks every type in scope, whatever their source order:
/// FCS adds a member's parameter to the environment when it checks the body,
/// after the whole `and` group's types. So the slot account a bare name asks
/// does not apply to it — an `and`-group type written after the member is in
/// scope there, and still loses (codex review). (Lowercase, because an
/// uppercase parameter is a provisional binder sema declines regardless.)
#[test]
fn a_parameter_outranks_a_later_and_group_type() {
    let src = "module M\ntype Host() =\n    member _.Probe (n: int) = n\nand n() = class end\n";
    let rf = resolve(src);
    let use_of = last(src, "= n");
    let use_of = TextRange::new(use_of.end() - rowan::TextSize::from(1), use_of.end());
    let param = last(src, "(n: int)");
    let param = TextRange::new(
        param.start() + rowan::TextSize::from(1),
        param.start() + rowan::TextSize::from(2),
    );
    let res = rf.resolution_at(use_of);
    assert_eq!(
        res.and_then(|r| rf.resolved_def(r)).map(|d| d.range),
        Some(param),
        "`n` is the parameter in {src:?}, got {res:?}"
    );
}

/// An inaccessible type does not make `type T = Nn` an abbreviation: FCS's
/// lookup sees only accessible types, so `Hidden`'s `private` `Nn` leaves `T` a
/// union whose case takes the name from the folded `A.Nn` (codex review). The
/// grid cannot build this — it needs an earlier binder, an opened private type
/// and the bar-less union together.
#[test]
fn an_inaccessible_type_leaves_a_barless_union_a_case() {
    let src = "module M\n[<AutoOpen>]\nmodule A =\n    let Nn = 1\nmodule Hidden =\n    type private Nn = int\nopen Hidden\ntype T = Nn\nlet result = Nn\n";
    let rf = resolve(src);
    let use_of = last(src, "Nn");
    assert!(
        matches!(
            rf.resolution_at(use_of),
            None | Some(Resolution::Deferred(_))
        ),
        "the use declines rather than binding `A.Nn` in {src:?}, got {:?}",
        rf.resolution_at(use_of)
    );
}

/// A referenced F# **module** is not a type, so a bar-less `type T = M` naming
/// one is still a union whose case takes the name (codex review): FSharp.Core's
/// `ExtraTopLevelOperators` module does not make `T` an abbreviation.
#[test]
fn a_module_of_the_name_leaves_a_barless_union_a_case() {
    let src = "module M\n[<AutoOpen>]\nmodule A =\n    let ExtraTopLevelOperators = 1\ntype T = ExtraTopLevelOperators\nlet probe = ExtraTopLevelOperators\n";
    let rf = resolve(src);
    let use_of = last(src, "ExtraTopLevelOperators");
    assert!(
        matches!(
            rf.resolution_at(use_of),
            None | Some(Resolution::Deferred(_))
        ),
        "the use declines rather than binding `A.ExtraTopLevelOperators` in {src:?}, got {:?}",
        rf.resolution_at(use_of)
    );
}

/// An abbreviation takes FCS's unqualified slot exactly when its target does,
/// so `type Diff = Diff'<string>` over a union leaves the union's case `Diff`
/// standing for a later bare use, as in WoofWare.Expect's `Diff.fs` (the
/// whole-project gate). Read off the syntax alone, an abbreviation is
/// undecidable and the use declined.
#[test]
fn an_abbreviation_of_a_union_leaves_its_case_standing() {
    let src = "namespace N\ntype U<'a> = private | Diff of 'a list\ntype Diff = U<string>\nmodule M =\n    let f x = x |> Diff\n";
    let rf = resolve(src);
    let res = rf.resolution_at(last(src, "Diff"));
    let case = last(src, "| Diff");
    let case = TextRange::new(case.start() + rowan::TextSize::from(2), case.end());
    assert_eq!(
        res.and_then(|r| rf.resolved_def(r)).map(|d| d.range),
        Some(case),
        "`Diff` is the union case in {src:?}, got {res:?}"
    );
}
