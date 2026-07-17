//! Differential matrix: **module-vs-type qualifier precedence** against FCS.
//!
//! The broader guard the `String.Equals` fix asked for
//! (`resolve_string_qualifier_repro.rs`): when a same-named module and type are
//! both in scope, the qualifier of `Name.member` follows FCS's
//! `ResolveExprLongIdentPrim` (NameResolution.fs) — the module search runs
//! first, but a module reading whose in-module member lookup fails does **not**
//! own the path (`AtMostOneResultQuery` lets the type search re-root it), while
//! a member the module *does* supply — a val, a union case, an occupied name —
//! keeps the path on the module.
//!
//! Two collision arenas:
//!
//! - the **real** FSharp.Core `String` module vs `System.String` (the shipped
//!   DLLs both sides read), covering the module-val / type-static /
//!   `Object`-member-name arms over a collision users actually hit;
//! - the purpose-built `SemaQualifierFixture` (`tests/fixtures/qualifier_env`),
//!   whose `QP.ModHalf.Collide` module / `QP.TypeHalf.Collide` type collision
//!   adds the arms FSharp.Core cannot: a name on **both** halves (`Shared` —
//!   FCS binds the module in *either* open order: modules-before-types), union
//!   cases colliding with `Object` member names (`Equals`), and a **generic
//!   child of every type kind** (`SHAPES`) — the exhaustive guard for the
//!   child-type occupancy rule, where FCS keeps the module for every kind except
//!   a record/union.
//!
//! Every cell asserts **both** sides explicitly: the FCS answer is pinned
//! literally (so an oracle drift is caught, not silently absorbed), and our
//! resolution is asserted per the certain-implies-exact doctrine — where we
//! commit, we must name FCS's entity; where the answer is beyond the model (an
//! overload set, a union-case or module-kept leaf), we must make *no claim*,
//! never a wrong one.
//!
//! The one `#[ignore]`d cell pins the single FCS rule the tier walk does not yet
//! implement — **modules are searched before types across all opens**, not
//! interleaved by open recency — in the `resolve_string_qualifier_repro` mould:
//! deterministic asserts of the FCS-correct answer, red on purpose until the
//! walk models it.

use std::path::Path;

use crate::common::{
    NormalisedUse, ensure_fsharp_core_dll, ensure_qualifier_fixture_built,
    ensure_system_runtime_dll, invoke_fcs_dump_with_refs, parse_fcs_uses, temp_fs_file,
};

use borzoi_assembly::{Ecma335Assembly, Member};
use borzoi_cst::parser::parse;
use borzoi_cst::syntax::{AstNode, ImplFile};
use borzoi_sema::{
    AssemblyEnv, EntityHandle, ProjectItems, Resolution, ResolvedFile, resolve_file,
};
use rowan::TextRange;

/// An [`AssemblyEnv`] over `dlls`, via the authoritative `from_views`
/// projection (source names and auto-opens applied). `System.Runtime` rides
/// along in every cell so the *type* half's `Object` base chain resolves — the
/// class-receiver occupancy arm must run on a complete chain, exactly as at
/// runtime.
fn env_of(dlls: &[&Path]) -> AssemblyEnv {
    let bytes: Vec<Vec<u8>> = dlls
        .iter()
        .map(|d| std::fs::read(d).unwrap_or_else(|e| panic!("read {d:?}: {e}")))
        .collect();
    let views: Vec<Ecma335Assembly> = bytes
        .iter()
        .map(|b| Ecma335Assembly::parse(b).expect("parse fixture/BCL dll"))
        .collect();
    AssemblyEnv::from_views(&views).expect("build AssemblyEnv")
}

fn resolve_src(src: &str, env: &AssemblyEnv) -> ResolvedFile {
    let parsed = parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors in {src:?}: {:?}",
        parsed.errors
    );
    let file = ImplFile::cast(parsed.root).expect("impl file");
    resolve_file(&file, &ProjectItems::default(), env)
}

/// FCS symbol uses for `src`, with `refs` resolvable (`BORZOI_FCS_EXTRA_REFS`).
fn fcs_uses(src: &str, refs: &[&Path]) -> Vec<NormalisedUse> {
    let path = temp_fs_file("qualifier_diff", src);
    let json = invoke_fcs_dump_with_refs("uses", &path, refs);
    let _ = std::fs::remove_file(&path);
    parse_fcs_uses(&json, src)
}

/// Byte range of `needle`'s first occurrence in `src`.
fn at(src: &str, needle: &str) -> TextRange {
    let i = src
        .find(needle)
        .unwrap_or_else(|| panic!("{needle:?} not in {src:?}"));
    TextRange::new(
        u32::try_from(i).unwrap().into(),
        u32::try_from(i + needle.len()).unwrap().into(),
    )
}

/// The qualifier segment's range: the first `.`-terminated prefix of `path`.
fn qualifier_of(src: &str, path: &str) -> TextRange {
    let whole = at(src, path);
    let head = path.split('.').next().expect("dotted path");
    TextRange::new(whole.start(), whole.start() + rowan::TextSize::of(head))
}

/// Our identity currency for an entity: `(assembly simple name, dotted full
/// path)` with the **source** leaf name (`StringModule` renders as its F#
/// spelling `String`). Top-level entities only — a nested entity's
/// `Entity::namespace` is empty, so its path would be truncated; no cell needs
/// one (union-case leaves are no-claim).
fn our_entity(env: &AssemblyEnv, h: EntityHandle) -> (String, String) {
    let e = env.entity(h);
    assert!(
        !e.namespace.is_empty(),
        "cells only ever commit top-level entities; got nested {:?}",
        e.name
    );
    let leaf = e.source_name.as_deref().unwrap_or(&e.name);
    (
        e.assembly.name.clone(),
        format!("{}.{}", e.namespace.join("."), leaf),
    )
}

fn member_source_name(m: &Member) -> &str {
    match m {
        Member::Method(x) => x.source_name.as_deref().unwrap_or(&x.name),
        Member::Field(x) => &x.name,
        Member::Property(x) => &x.name,
        Member::Event(x) => &x.name,
    }
}

/// Assert FCS reports a use at exactly `range` with the pinned
/// `(full name, assembly)` — the oracle guard, so a cell cannot rot into
/// comparing us against a drifted FCS answer.
fn assert_fcs_pin(uses: &[NormalisedUse], src: &str, range: TextRange, full: &str, asm: &str) {
    let hit = uses
        .iter()
        .find(|u| {
            u32::from(range.start()) as usize == u.start && u32::from(range.end()) as usize == u.end
        })
        .unwrap_or_else(|| panic!("no FCS use at {range:?} in {src:?}"));
    assert_eq!(
        (hit.full_name.as_deref(), hit.assembly.as_deref()),
        (Some(full), Some(asm)),
        "FCS pin drifted at {range:?} in {src:?}"
    );
}

/// Assert our resolution at `range` is an [`Resolution::Entity`] naming exactly
/// `(asm, full)` (per [`our_entity`]).
fn assert_our_entity(
    rf: &ResolvedFile,
    env: &AssemblyEnv,
    range: TextRange,
    asm: &str,
    full: &str,
) {
    let res = rf
        .resolution_at(range)
        .unwrap_or_else(|| panic!("no resolution at {range:?}"));
    let Resolution::Entity(h) = res else {
        panic!("expected an Entity at {range:?}, got {res:?}");
    };
    assert_eq!(our_entity(env, h), (asm.to_string(), full.to_string()));
}

/// Assert our resolution at `range` is a [`Resolution::Member`] naming exactly
/// `(asm, parent-full.member-source-name)`.
fn assert_our_member(
    rf: &ResolvedFile,
    env: &AssemblyEnv,
    range: TextRange,
    asm: &str,
    full: &str,
) {
    let res = rf
        .resolution_at(range)
        .unwrap_or_else(|| panic!("no resolution at {range:?}"));
    let Resolution::Member { parent, idx } = res else {
        panic!("expected a Member at {range:?}, got {res:?}");
    };
    let (pasm, pfull) = our_entity(env, parent);
    let got = format!("{pfull}.{}", member_source_name(env.member_at(parent, idx)));
    assert_eq!((pasm, got), (asm.to_string(), full.to_string()));
}

/// Assert we make **no claim** at `range` (unrecorded or an honest deferral) —
/// the sound outcome for a leaf beyond the model (an overload set, a union
/// case).
fn assert_no_claim(rf: &ResolvedFile, range: TextRange) {
    match rf.resolution_at(range) {
        None | Some(Resolution::Deferred(_)) => {}
        Some(other) => panic!("expected no claim at {range:?}, got {other:?}"),
    }
}

// ============================================================================
// Arena 1 — the real FSharp.Core `String` module vs `System.String`.
// ============================================================================

fn fsharp_core_env() -> AssemblyEnv {
    env_of(&[&ensure_fsharp_core_dll(), &ensure_system_runtime_dll()])
}

/// One `String.<member>` snippet under `opens`, all four member arms, our side
/// and FCS's compared per cell. `Equals`/`Compare` are `System.String` statics
/// (overload sets — the qualifier commits, the member honestly defers);
/// `length`/`concat` are module vals (unique — the whole path commits).
fn string_cells(opens: &str) {
    let src = format!(
        "module M\n{opens}let a = String.Equals (\"a\", \"b\")\nlet b = String.length \"ab\"\n\
         let c = String.Compare (\"a\", \"b\")\nlet d = String.concat \", \" [\"a\"]\n"
    );
    let src = src.as_str();
    let env = fsharp_core_env();
    let rf = resolve_src(src, &env);
    let uses = fcs_uses(src, &[]);

    // `Equals` exists only on the TYPE: the module reading must not own the
    // path (`Object.Equals` is unreachable through a module qualifier), so the
    // `open System` reading wins whatever the open order — the repro's rule,
    // now FCS-live.
    assert_fcs_pin(
        &uses,
        src,
        qualifier_of(src, "String.Equals"),
        "System.String",
        "System.Runtime",
    );
    assert_fcs_pin(
        &uses,
        src,
        at(src, "String.Equals"),
        "System.String.Equals",
        "System.Runtime",
    );
    assert_our_entity(
        &rf,
        &env,
        qualifier_of(src, "String.Equals"),
        "System.Runtime",
        "System.String",
    );
    assert_no_claim(&rf, at(src, "String.Equals")); // overload set — defer, never guess

    // `length` exists only on the MODULE (a `[<CompiledName>]`-renamed val):
    // the module reading owns and resolves.
    assert_fcs_pin(
        &uses,
        src,
        qualifier_of(src, "String.length"),
        "String",
        "FSharp.Core",
    );
    assert_fcs_pin(
        &uses,
        src,
        at(src, "String.length"),
        "Microsoft.FSharp.Core.String.length",
        "FSharp.Core",
    );
    assert_our_entity(
        &rf,
        &env,
        qualifier_of(src, "String.length"),
        "FSharp.Core",
        "Microsoft.FSharp.Core.String",
    );
    assert_our_member(
        &rf,
        &env,
        at(src, "String.length"),
        "FSharp.Core",
        "Microsoft.FSharp.Core.String.length",
    );

    // `Compare` — the type-static arm again, with no `Object` collision.
    assert_fcs_pin(
        &uses,
        src,
        at(src, "String.Compare"),
        "System.String.Compare",
        "System.Runtime",
    );
    assert_our_entity(
        &rf,
        &env,
        qualifier_of(src, "String.Compare"),
        "System.Runtime",
        "System.String",
    );
    assert_no_claim(&rf, at(src, "String.Compare"));

    // `concat` — the module-val arm with no rename.
    assert_fcs_pin(
        &uses,
        src,
        at(src, "String.concat"),
        "Microsoft.FSharp.Core.String.concat",
        "FSharp.Core",
    );
    assert_our_member(
        &rf,
        &env,
        at(src, "String.concat"),
        "FSharp.Core",
        "Microsoft.FSharp.Core.String.concat",
    );
}

/// The repro's order — the FSharp.Core module re-introduced by the *later*
/// open, so its (member-absent) reading is the tier tried first.
#[test]
fn string_cells_with_the_module_open_last() {
    string_cells("open System\nopen Microsoft.FSharp.Core\n");
}

/// The reverse order: `open System` last. FCS's answers are identical (its
/// rule is member-existence-driven, not open-recency-driven) — pinned so.
#[test]
fn string_cells_with_the_type_open_last() {
    string_cells("open Microsoft.FSharp.Core\nopen System\n");
}

/// No explicit FSharp.Core open at all: only the implicit auto-open supplies
/// the module. The pre-fix resolver got this order right — keep it that way.
#[test]
fn string_cells_with_only_open_system() {
    string_cells("open System\n");
}

// ============================================================================
// Arena 2 — the purpose-built `Collide` module/type collision fixture.
// ============================================================================

const FIXTURE_ASM: &str = "SemaQualifierFixture";

/// The module qualifier's full dotted path (our rendering).
const MOD: &str = "QP.ModHalf.Collide";
/// The type qualifier's full dotted path (our rendering).
const TYP: &str = "QP.TypeHalf.Collide";

/// Every non-abbreviation generic `Gen*` child kind, paired with whether FCS
/// keeps the **module** qualifier on `Collide.<name>` (probed exhaustively, both
/// open orders). The systematic guard for `child_type_keeps_module_qualifier`:
/// every type kind keeps the module EXCEPT a record/union, and the table is
/// diffed against FCS so a mis-modelled kind reddens rather than passing
/// silently. Abbreviations are *not* here — FCS chases their (unmodelled) target
/// so they defer, covered by [`abbreviation_children_defer_the_qualifier`].
const SHAPES: &[(&str, bool)] = &[
    ("GenCls", true),        // class, public ctor
    ("GenClsPriv", true),    // class, only a private ctor — still owned by the module
    ("GenStructCtor", true), // struct, explicit ctor
    ("GenStructDef", true),  // struct, only the implicit default ctor
    ("GenIface", true),      // interface — not constructible, yet owned by the module
    ("GenDel", true),        // delegate
    ("GenRec", false),       // record — bare name is not an expression, falls through
    ("GenUni", false),       // union — likewise
];

/// The generic type-**abbreviation** children, with the kind FCS's target-chase
/// lands on (`true` = the qualifier FCS keeps is the module, `false` = FCS falls
/// through to the type). We model neither — both defer — so this pairing is only
/// the FCS-side pin the defer test documents against.
const ABBREVS: &[(&str, bool)] = &[
    ("GenAbbrCls", true),  // abbreviation -> class: FCS keeps the module
    ("GenAbbrRec", false), // abbreviation -> record: FCS falls through to the type
];

fn fixture_env() -> AssemblyEnv {
    env_of(&[
        ensure_qualifier_fixture_built(),
        &ensure_system_runtime_dll(),
    ])
}

/// A snippet under `opens` that references `Collide.<name>` once for every name
/// the cells assert on — the fixed cells, every [`SHAPES`] entry, and every
/// [`ABBREVS`] entry — so `at(src, "Collide.<name>")` finds each exactly once.
fn fixture_src(opens: &str) -> String {
    let mut src = format!(
        "module Snippet\n{opens}let a = Collide.fromModule ()\nlet b = Collide.TypeOnly ()\n\
         let c = Collide.Shared ()\nlet d = Collide.Equals\nlet e = Collide.CaseOnly\n"
    );
    for (i, (name, _)) in SHAPES.iter().chain(ABBREVS.iter()).enumerate() {
        src.push_str(&format!("let s{i} = Collide.{name} ()\n"));
    }
    src
}

/// FCS keeps the module qualifier on `Collide.<name>` — its full name renders as
/// the bare `"Collide"` for a module. (We do not pin FCS's *leaf*: a keeper's
/// leaf varies — the type itself, an empty private-ctor result, or an
/// abbreviation target — and our side makes no claim on it anyway.)
fn assert_fcs_qualifier_is_module(uses: &[NormalisedUse], src: &str, name: &str) {
    let path = format!("Collide.{name}");
    assert_fcs_pin(uses, src, qualifier_of(src, &path), "Collide", FIXTURE_ASM);
}

/// FCS re-roots `Collide.<name>` to the **type** static (qualifier + leaf).
fn assert_fcs_qualifier_is_type(uses: &[NormalisedUse], src: &str, name: &str) {
    let path = format!("Collide.{name}");
    assert_fcs_pin(uses, src, qualifier_of(src, &path), TYP, FIXTURE_ASM);
    assert_fcs_pin(
        uses,
        src,
        at(src, &path),
        &format!("{TYP}.{name}"),
        FIXTURE_ASM,
    );
}

/// The cells that agree in **both** open orders (FCS is order-independent
/// throughout; a cell lands here when *our* answer is order-independent too —
/// i.e. the winning reading owns its tier or is fallen through to regardless of
/// which open is latest).
fn fixture_order_independent_cells(src: &str) {
    let env = fixture_env();
    let rf = resolve_src(src, &env);
    let uses = fcs_uses(src, &[ensure_qualifier_fixture_built()]);

    // Module-only val: both searches agree on the module.
    assert_fcs_pin(
        &uses,
        src,
        at(src, "Collide.fromModule"),
        "QP.ModHalf.Collide.fromModule",
        FIXTURE_ASM,
    );
    assert_fcs_qualifier_is_module(&uses, src, "fromModule");
    assert_our_entity(
        &rf,
        &env,
        qualifier_of(src, "Collide.fromModule"),
        FIXTURE_ASM,
        MOD,
    );
    assert_our_member(
        &rf,
        &env,
        at(src, "Collide.fromModule"),
        FIXTURE_ASM,
        "QP.ModHalf.Collide.fromModule",
    );

    // Type-only static: the module reading must fall through (the fixture's
    // `String.Equals` shape — the very bug).
    assert_fcs_qualifier_is_type(&uses, src, "TypeOnly");
    assert_our_entity(
        &rf,
        &env,
        qualifier_of(src, "Collide.TypeOnly"),
        FIXTURE_ASM,
        TYP,
    );
    assert_our_member(
        &rf,
        &env,
        at(src, "Collide.TypeOnly"),
        FIXTURE_ASM,
        "QP.TypeHalf.Collide.TypeOnly",
    );

    // Union case only in the module: occupied there (`TryFindTypeWithUnionCase`),
    // so the qualifier is the module and the case leaf is beyond the model — no
    // claim, never the type half.
    assert_fcs_pin(
        &uses,
        src,
        at(src, "Collide.CaseOnly"),
        "QP.ModHalf.Collide.U.CaseOnly",
        FIXTURE_ASM,
    );
    assert_fcs_qualifier_is_module(&uses, src, "CaseOnly");
    assert_our_entity(
        &rf,
        &env,
        qualifier_of(src, "Collide.CaseOnly"),
        FIXTURE_ASM,
        MOD,
    );
    assert_no_claim(&rf, at(src, "Collide.CaseOnly"));

    // Every record/union shape: FCS re-roots to the type static in *both* orders,
    // and so do we (the module child does not occupy the name, so a lower type
    // reading owns the whole path whichever tier it sits in). This is the crux of
    // the occupancy fix — a kind-blind occupancy would retain the module here.
    for &(name, keeps_module) in SHAPES {
        if keeps_module {
            continue;
        }
        let path = format!("Collide.{name}");
        assert_fcs_qualifier_is_type(&uses, src, name);
        assert_our_entity(&rf, &env, qualifier_of(src, &path), FIXTURE_ASM, TYP);
        assert_our_member(
            &rf,
            &env,
            at(src, &path),
            FIXTURE_ASM,
            &format!("{TYP}.{name}"),
        );
    }
}

#[test]
fn fixture_cells_with_the_module_open_last() {
    let src = fixture_src("open QP.TypeHalf\nopen QP.ModHalf\n");
    fixture_order_independent_cells(&src);

    // With the module's open latest, the module tier is tried first, so the
    // module-owned cells agree without needing the cross-tier rule.
    let env = fixture_env();
    let rf = resolve_src(&src, &env);
    let uses = fcs_uses(&src, &[ensure_qualifier_fixture_built()]);

    // A name on BOTH halves: FCS searches modules before types, so the module
    // val wins.
    assert_fcs_pin(
        &uses,
        &src,
        at(&src, "Collide.Shared"),
        "QP.ModHalf.Collide.Shared",
        FIXTURE_ASM,
    );
    assert_our_member(
        &rf,
        &env,
        at(&src, "Collide.Shared"),
        FIXTURE_ASM,
        "QP.ModHalf.Collide.Shared",
    );

    // A union case named `Equals`: found by the in-module case search — never
    // `Object.Equals` (a module qualifier has no base chain), never the type.
    assert_fcs_pin(
        &uses,
        &src,
        at(&src, "Collide.Equals"),
        "QP.ModHalf.Collide.U.Equals",
        FIXTURE_ASM,
    );
    assert_fcs_qualifier_is_module(&uses, &src, "Equals");
    assert_our_entity(
        &rf,
        &env,
        qualifier_of(&src, "Collide.Equals"),
        FIXTURE_ASM,
        MOD,
    );
    assert_no_claim(&rf, at(&src, "Collide.Equals"));

    // Every module-keeping generic kind: FCS keeps the module, and with the
    // module tier tried first we agree on the qualifier and make no claim on the
    // leaf. The record/union arm is the order-independent block above; this is
    // the whole rest of the kind table (class/struct/interface/delegate/abbrev),
    // the exhaustive guard that the kind rule is "keep unless record/union".
    for &(name, keeps_module) in SHAPES {
        if !keeps_module {
            continue;
        }
        let path = format!("Collide.{name}");
        assert_fcs_qualifier_is_module(&uses, &src, name);
        assert_our_entity(&rf, &env, qualifier_of(&src, &path), FIXTURE_ASM, MOD);
        assert_no_claim(&rf, at(&src, &path));
    }
}

#[test]
fn fixture_cells_with_the_type_open_last() {
    fixture_order_independent_cells(&fixture_src("open QP.ModHalf\nopen QP.TypeHalf\n"));
}

/// A generic type **abbreviation** child (`ABBREVS`): FCS chases the target
/// before deciding ownership — a class target keeps the module, a record target
/// falls through to the type — so FCS's answer is *target-sensitive*. Our
/// projection does not model the abbreviation target, so we **defer** the whole
/// path (`has_public_abbreviation_child` → the tier-local `AbbreviationOpaque`),
/// making no claim on the qualifier in *either* direction. That is sound both
/// ways: committing the module would be wrong for a record target, committing
/// the type wrong for a class target (codex review). Asserted with the module
/// open last, where our module tier is tried first and the defer actually fires
/// (with the type open last the type tier resolves its own static first — the
/// modules-before-types gap, not this concern).
#[test]
fn abbreviation_children_defer_the_qualifier() {
    let src = fixture_src("open QP.TypeHalf\nopen QP.ModHalf\n");
    let env = fixture_env();
    let rf = resolve_src(&src, &env);
    let uses = fcs_uses(&src, &[ensure_qualifier_fixture_built()]);

    for &(name, fcs_keeps_module) in ABBREVS {
        let path = format!("Collide.{name}");
        // Pin FCS's target-sensitive answer both ways, so the divergence we are
        // deliberately avoiding is documented, not assumed.
        if fcs_keeps_module {
            assert_fcs_qualifier_is_module(&uses, &src, name);
        } else {
            assert_fcs_qualifier_is_type(&uses, &src, name);
        }
        // We defer: no claim on the qualifier or the leaf, whatever FCS did.
        assert_no_claim(&rf, qualifier_of(&src, &path));
        assert_no_claim(&rf, at(&src, &path));
    }
}

/// A module legally declaring **both** `let X` and `type X<'a>`: FCS's in-module
/// search checks vals before types, so `M.X` resolves to the **val**. The
/// abbreviation defer must therefore run *after* the own-value lookup — inside
/// `static_lookup`'s `Uncertain` arm, not before it — or it would suppress a
/// resolvable member (codex review 4). Here `Collide.ValAndAbbr` is a val and a
/// generic abbreviation both; module open last, so our module tier is tried
/// first.
#[test]
fn a_module_value_wins_over_its_own_same_named_abbreviation() {
    let src = "module Snippet\nopen QP.TypeHalf\nopen QP.ModHalf\nlet a = Collide.ValAndAbbr ()\n";
    let env = fixture_env();
    let rf = resolve_src(src, &env);
    let uses = fcs_uses(src, &[ensure_qualifier_fixture_built()]);

    assert_fcs_qualifier_is_module(&uses, src, "ValAndAbbr");
    assert_fcs_pin(
        &uses,
        src,
        at(src, "Collide.ValAndAbbr"),
        "QP.ModHalf.Collide.ValAndAbbr",
        FIXTURE_ASM,
    );
    assert_our_entity(
        &rf,
        &env,
        qualifier_of(src, "Collide.ValAndAbbr"),
        FIXTURE_ASM,
        MOD,
    );
    assert_our_member(
        &rf,
        &env,
        at(src, "Collide.ValAndAbbr"),
        FIXTURE_ASM,
        "QP.ModHalf.Collide.ValAndAbbr",
    );
}

/// An as-written-**root** module whose child is a generic abbreviation must NOT
/// preempt a higher-priority `open` that resolves the whole path. The global
/// `RootHolder.Aliased` is an abbreviation; `open QP.HighNs` brings a real
/// `QP.HighNs.RootHolder.Aliased` val at higher priority, and FCS binds it. Our
/// root abbreviation defers *tier-locally* (`AbbreviationOpaque`, not the
/// preemptive `ProjectShadowed`), so the open is tried first and wins (codex
/// review 4).
#[test]
fn a_higher_open_outranks_a_root_abbreviation() {
    let src = "module Snippet\nopen QP.HighNs\nlet a = RootHolder.Aliased ()\n";
    let env = fixture_env();
    let rf = resolve_src(src, &env);
    let uses = fcs_uses(src, &[ensure_qualifier_fixture_built()]);

    assert_fcs_pin(
        &uses,
        src,
        at(src, "RootHolder.Aliased"),
        "QP.HighNs.RootHolder.Aliased",
        FIXTURE_ASM,
    );
    assert_our_member(
        &rf,
        &env,
        at(src, "RootHolder.Aliased"),
        FIXTURE_ASM,
        "QP.HighNs.RootHolder.Aliased",
    );
}

/// KNOWN GAP (pinned red, `resolve_string_qualifier_repro` mould): FCS searches
/// **all module candidates before any type candidate** (`moduleSearch +++
/// tyconSearch`, order-independent — the FCS pins in
/// [`fixture_cells_with_the_module_open_last`] hold in this open order too), but
/// our tier walk interleaves readings by open recency. With `open QP.TypeHalf`
/// last the type tier is tried first and owns every module-kept name — `Shared`
/// and `Equals` via the type's own member, and each module-keeping generic kind
/// via the type's static. So EVERY name FCS keeps on the module binds the type
/// half for us in this order. Modelling modules-before-types across tiers is a
/// separate walk restructure; until then this cell documents the whole divergent
/// set deterministically.
#[test]
#[ignore = "known gap: modules are searched before types across all opens, but the \
            tier walk interleaves by open recency — every module-kept `Collide.*` \
            binds the type half when the type's open is later; run with --ignored"]
fn module_kept_names_bind_the_module_even_when_the_type_open_is_later() {
    let src = fixture_src("open QP.ModHalf\nopen QP.TypeHalf\n");
    let env = fixture_env();
    let rf = resolve_src(&src, &env);

    // The whole modules-before-types set, in one cell: `Shared`, the union case
    // `Equals`, and every module-keeping generic kind.
    assert_our_member(
        &rf,
        &env,
        at(&src, "Collide.Shared"),
        FIXTURE_ASM,
        "QP.ModHalf.Collide.Shared",
    );
    assert_our_entity(
        &rf,
        &env,
        qualifier_of(&src, "Collide.Equals"),
        FIXTURE_ASM,
        MOD,
    );
    for &(name, keeps_module) in SHAPES {
        if !keeps_module {
            continue;
        }
        assert_our_entity(
            &rf,
            &env,
            qualifier_of(&src, &format!("Collide.{name}")),
            FIXTURE_ASM,
            MOD,
        );
    }
}

// ============================================================================
// Deterministic clause pins (no FCS): the module-occupancy predicate, clause
// by clause, over the fixture module — `static_lookup`'s module branch
// (`module_qualified_occupied`). The FSharp.Core `Object`-name pins live in
// `assembly_env.rs` (`static_lookup_on_a_module_ignores_object_members`).
// ============================================================================

#[test]
fn module_occupancy_follows_the_in_module_search_domain() {
    use borzoi_sema::StaticLookup;
    let env = fixture_env();
    let ns: Vec<String> = ["QP", "ModHalf"].iter().map(|s| s.to_string()).collect();
    let module = env
        .lookup_type(&ns, "Collide", 0)
        .expect("QP.ModHalf.Collide in env");

    // Own vals resolve.
    assert!(matches!(
        env.static_lookup(module, "fromModule"),
        StaticLookup::Resolved(_)
    ));
    assert!(matches!(
        env.static_lookup(module, "Shared"),
        StaticLookup::Resolved(_)
    ));
    // A union case occupies (in-module `TryFindTypeWithUnionCase`) but names no
    // static — deferred, and the path stays on the module.
    assert_eq!(env.static_lookup(module, "Equals"), StaticLookup::Uncertain);
    assert_eq!(
        env.static_lookup(module, "CaseOnly"),
        StaticLookup::Uncertain
    );
    // Every generic child kind: a module-keeping kind is `Uncertain` (occupied,
    // path stays on the module), a record/union is `Absent` (a lower reading may
    // own the path). The exhaustive kind split, mirroring the FCS matrix.
    for &(name, keeps_module) in SHAPES {
        let got = env.static_lookup(module, name);
        if keeps_module {
            assert_eq!(
                got,
                StaticLookup::Uncertain,
                "{name} should keep the module qualifier"
            );
        } else {
            assert_eq!(got, StaticLookup::Absent, "{name} should fall through");
        }
    }
    // The type half's static is NOT in this module: genuinely absent, so a
    // lower-priority reading may own the path.
    assert_eq!(env.static_lookup(module, "TypeOnly"), StaticLookup::Absent);
}
