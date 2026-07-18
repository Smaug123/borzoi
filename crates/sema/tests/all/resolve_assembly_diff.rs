//! Differential test: our fully-qualified assembly resolution against FCS.
//!
//! The fixture assembly (`tests/fixtures/assembly_env`) is fed to *both* sides
//! — to FCS as a `-r:` reference (via `BORZOI_FCS_EXTRA_REFS`) and to us as
//! an [`AssemblyEnv`] — so a snippet's qualified references (`Demo.Calc.Zero`)
//! resolve into it on both. A referenced-assembly symbol has no usable
//! declaration range (FCS reports a synthetic/in-file one), so the oracle
//! currency here is the symbol's **assembly simple name + full name**.
//!
//! The D7 property: for every FCS use that resolves into the fixture assembly,
//! our resolution is an `Entity`/`Member` with the same `(assembly, full name)`
//! **or** honestly `Deferred`/unrecorded (namespace-qualifier uses, which we do
//! not model, fall here) — never `Unresolved`, a local/item, or a wrong entity.
//! A per-snippet count of expected resolutions keeps it from passing vacuously.

use crate::common::{
    ensure_assembly_fixture_built, invoke_fcs_dump_with_refs, parse_fcs_uses, temp_fs_file,
};
use borzoi_assembly::{Ecma335Assembly, EcmaView, Member};
use borzoi_cst::parser::parse;
use borzoi_cst::syntax::{AstNode, ImplFile};
use borzoi_sema::{AssemblyEnv, ProjectItems, Resolution, resolve_file};
use rowan::TextRange;

/// The fixture's assembly simple name (its `<AssemblyName>`), which FCS reports
/// as the declaring assembly of the fixture's symbols.
const FIXTURE_ASM: &str = "SemaAssemblyEnvFixture";

fn span(start: usize, end: usize) -> TextRange {
    TextRange::new(
        u32::try_from(start).unwrap().into(),
        u32::try_from(end).unwrap().into(),
    )
}

fn member_name(m: &Member) -> &str {
    match m {
        Member::Method(x) => &x.name,
        Member::Field(x) => &x.name,
        Member::Property(x) => &x.name,
        Member::Event(x) => &x.name,
    }
}

/// The `(assembly, full name)` our resolution names, for comparison with FCS.
fn our_assembly_full(env: &AssemblyEnv, res: Resolution) -> (String, String) {
    fn full(ns: &[String], name: &str) -> String {
        if ns.is_empty() {
            name.to_string()
        } else {
            format!("{}.{}", ns.join("."), name)
        }
    }
    match res {
        Resolution::Entity(h) => {
            let e = env.entity(h);
            (e.assembly.name.clone(), full(&e.namespace, &e.name))
        }
        Resolution::Member { parent, idx } => {
            let e = env.entity(parent);
            (
                e.assembly.name.clone(),
                format!(
                    "{}.{}",
                    full(&e.namespace, &e.name),
                    member_name(env.member_at(parent, idx))
                ),
            )
        }
        _ => unreachable!("only Entity/Member reach here"),
    }
}

/// Resolve `src` and assert the assembly-resolution differential, expecting
/// exactly `expected` uses to agree with FCS as `Entity`/`Member`.
fn assert_matches_fcs(src: &str, expected: usize) {
    let fixture = ensure_assembly_fixture_built();

    // Our resolution.
    let bytes = std::fs::read(fixture).expect("read fixture dll");
    let view = Ecma335Assembly::parse(&bytes).expect("parse fixture dll");
    let env = AssemblyEnv::from_views(std::slice::from_ref(&view)).expect("build AssemblyEnv");
    let parsed = parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors in {src:?}: {:?}",
        parsed.errors
    );
    let file = ImplFile::cast(parsed.root).expect("impl file");
    let rf = resolve_file(&file, &ProjectItems::default(), &env);

    // FCS oracle, with the fixture as a reference.
    let path = temp_fs_file("asm_diff", src);
    let json = invoke_fcs_dump_with_refs("uses", &path, &[fixture]);
    let _ = std::fs::remove_file(&path);
    let uses = parse_fcs_uses(&json, src);

    let mut agreed = 0usize;
    for u in &uses {
        if u.start == u.end {
            continue;
        }
        // Only uses FCS resolves *into the fixture assembly* are in scope here.
        if u.assembly.as_deref() != Some(FIXTURE_ASM) {
            continue;
        }
        match rf.resolution_at(span(u.start, u.end)) {
            // Honest "say nothing" — e.g. the namespace qualifier we don't model.
            None | Some(Resolution::Deferred(_)) => {}
            Some(res @ (Resolution::Entity(_) | Resolution::Member { .. })) => {
                let (asm, full) = our_assembly_full(&env, res);
                assert_eq!(
                    Some(asm.as_str()),
                    u.assembly.as_deref(),
                    "use {:?}: assembly mismatch",
                    u.name
                );
                assert_eq!(
                    Some(full.as_str()),
                    u.full_name.as_deref(),
                    "use {:?}: full-name mismatch",
                    u.name
                );
                agreed += 1;
            }
            other => panic!(
                "use {:?} at {}..{} resolves into the fixture assembly, but we gave {other:?}",
                u.name, u.start, u.end
            ),
        }
    }

    assert_eq!(agreed, expected, "assembly resolutions agreed for {src:?}");
}

/// The D7 **soundness** check, both directions, without a fixed count:
/// - **FCS → ours**: every use FCS resolves into the fixture must be matched *or*
///   honestly `Deferred`/unrecorded — never a wrong entity.
/// - **ours → FCS**: every resolution *we* make into the fixture must be confirmed
///   by FCS — some FCS use *covering* our range must also resolve into the fixture
///   (lenient on the segment-vs-whole-long-id range convention: we record the
///   `Entity` at a segment, FCS spans the whole path). If FCS resolves our span to
///   a **project** entity instead (no covering fixture use), we have wrong-targeted
///   the assembly — exactly the nested-`module Sub`/`Demo.Sub.Calc` corner.
///
/// Returns `(agreed, fixture_uses)` so the sweep can surface pure under-resolution
/// without failing on it.
fn sweep_sound(src: &str) -> (usize, usize) {
    let fixture = ensure_assembly_fixture_built();
    let bytes = std::fs::read(fixture).expect("read fixture dll");
    let view = Ecma335Assembly::parse(&bytes).expect("parse fixture dll");
    let env = AssemblyEnv::from_views(std::slice::from_ref(&view)).expect("build AssemblyEnv");
    let parsed = parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors in {src:?}: {:?}",
        parsed.errors
    );
    let file = ImplFile::cast(parsed.root).expect("impl file");
    let rf = resolve_file(&file, &ProjectItems::default(), &env);

    let path = temp_fs_file("asm_sweep", src);
    let json = invoke_fcs_dump_with_refs("uses", &path, &[fixture]);
    let _ = std::fs::remove_file(&path);
    let uses = parse_fcs_uses(&json, src);

    let (mut agreed, mut fixture_uses) = (0usize, 0usize);
    for u in &uses {
        if u.start == u.end || u.assembly.as_deref() != Some(FIXTURE_ASM) {
            continue;
        }
        fixture_uses += 1;
        match rf.resolution_at(span(u.start, u.end)) {
            None | Some(Resolution::Deferred(_)) => {}
            Some(res @ (Resolution::Entity(_) | Resolution::Member { .. })) => {
                let (asm, full) = our_assembly_full(&env, res);
                assert_eq!(
                    Some(asm.as_str()),
                    u.assembly.as_deref(),
                    "{src:?} use {:?}: assembly",
                    u.name
                );
                assert_eq!(
                    Some(full.as_str()),
                    u.full_name.as_deref(),
                    "{src:?} use {:?}: full name",
                    u.name
                );
                agreed += 1;
            }
            other => panic!(
                "{src:?} use {:?} at {}..{} resolves into the fixture, but we gave {other:?}",
                u.name, u.start, u.end
            ),
        }
    }

    // ours → FCS: a fixture resolution we made that FCS resolves *elsewhere* (a
    // project entity, no covering fixture use) is a wrong target.
    for (range, res) in rf.resolutions() {
        if !matches!(res, Resolution::Entity(_) | Resolution::Member { .. }) {
            continue;
        }
        let (start, end) = (
            u32::from(range.start()) as usize,
            u32::from(range.end()) as usize,
        );
        let covering = uses
            .iter()
            .filter(|u| u.start != u.end && u.start <= start && end <= u.end);
        let (mut any_covering, mut any_fixture) = (false, false);
        for u in covering {
            any_covering = true;
            if u.assembly.as_deref() == Some(FIXTURE_ASM) {
                any_fixture = true;
            }
        }
        assert!(
            !any_covering || any_fixture,
            "{src:?}: we resolved {start}..{end} into the fixture ({:?}), but FCS resolves \
             that span elsewhere (no covering fixture use) — a wrong target",
            our_assembly_full(&env, *res),
        );
    }
    (agreed, fixture_uses)
}

/// Broad soundness sweep across the value/type-path tier surface — the precedence
/// corners the assembly-namespace walker must get right (opens vs enclosing vs
/// root; complete vs partial; ambiguity). Every case is asserted **sound** (no
/// wrong target) against FCS; the per-case completeness (`agreed`/`fixture_uses`)
/// is printed (run with `--nocapture`) to surface deferrals for follow-up without
/// failing the build on a *sound* under-resolution.
///
/// The printed `agreed` count *understates* type-position completeness: FCS spans
/// the rightmost type over the **whole** long-ident (`Sub.Thing`), while we record
/// the `Entity` at its **segment** (`Thing`), so the exact-range match here misses
/// it (a sound deferral at the whole range). The containment-based
/// [`assert_type_use_complete`] is the real type-position completeness oracle;
/// this sweep is purely the no-wrong-target guard.
#[test]
fn assembly_resolution_is_sound_across_the_tier_surface() {
    let cases: &[&str] = &[
        // Fully-qualified value/member paths.
        "module M\nlet x = Demo.Calc.Zero()\n",
        "module M\nlet x = Demo.Calc.Answer\n",
        "module M\nlet x = Demo.Calc.Nope\n", // partial: type, bad member
        "module M\nlet x = Demo.Calc.Twice\n", // overloaded static → defer
        "module M\nlet x = Demo.Sub.Calc.Zero()\n",
        "module M\nlet x = Sub.Calc.Zero()\n",
        "module M\nlet x = Calc.Nope\n", // the global-namespace `Calc`, rooted
        // Single open (root / relative / nested).
        "open Demo\nlet x = Calc.Zero()\n",
        "open Demo\nlet x = Calc.Answer\n",
        "open Demo\nlet x = Calc.Nope\n", // partial open reading vs complete root `::Calc.Nope`
        "open Demo.Sub\nlet x = Calc.Zero()\n",
        "open Sub\nlet x = Calc.Zero()\n",
        // Two opens: completeness disambiguates vs genuine ambiguity.
        "open Demo\nopen Sub\nlet x = Calc.Answer\n", // unique complete → Demo.Calc.Answer
        "open Demo\nopen Demo.Sub\nlet x = Calc.Zero()\n", // both complete → defer
        "open Demo\nopen Sub\nlet x = Calc.Zero()\n", // both complete → defer
        // Enclosing namespace (tier 2), bare and relative-qualified.
        "namespace Demo\n\nmodule M =\n    let x = Calc.Zero()\n",
        "namespace Demo\n\nmodule M =\n    let x = Sub.Calc.Zero()\n",
        "namespace Demo.Sub\n\nmodule M =\n    let x = Calc.Zero()\n",
        "namespace Demo.Sub\n\nmodule M =\n    let x = Sub.Calc.Zero()\n", // FS0039 root, not ancestor
        // Enclosing namespace + open: partial open vs complete enclosing.
        "namespace Demo\n\nmodule M =\n    open Sub\n    let x = Calc.Answer\n",
        "namespace Demo\n\nmodule M =\n    open Sub\n    let x = Calc.Zero()\n",
        // Type position (annotations) across the same tiers.
        "module M\nlet f (x : Demo.Thing) = x\n",
        "open Demo\nlet f (x : Thing) = x\n",
        "open Demo\nlet f (x : Pair) = x\n",
        "open Demo\nlet f (x : Pair<int>) = x\n",
        "namespace Demo\n\nmodule M =\n    let f (x : Thing) = x\n",
        "namespace Demo\n\nmodule M =\n    let f (x : Sub.Thing) = x\n",
        "namespace Demo\n\nmodule M =\n    open Sub\n    let f (x : Thing) = x\n",
        "open Demo\nopen Sub\nlet f (x : Thing) = x\n",
        // Nested `module Sub` (collides with assembly `Demo.Sub`): a type through
        // it must defer, never wrong-target `Demo.Sub.Calc` (the ours→FCS guard).
        "module M\nopen Demo\nmodule Sub =\n    type Calc = int\nlet f (x : Sub.Calc) = x\n",
        "module M\nopen Demo\nmodule Sub =\n    let placeholder = 1\nlet f (x : Sub.Calc) = x\n",
        // Expression-position constructor fallback edge cases (the ours→FCS guard
        // catches a wrong-target): a same-file `type Thing` shadows the opened
        // `Demo.Thing` (FCS binds the project type), and an explicit type
        // application must not name the arity-0 sibling.
        "module M\nopen Demo\ntype Thing() = class end\nlet y = Thing ()\n",
        "open Demo\nlet x = Pair<int> ()\n",
        "open Demo\nlet x = Calc ()\n", // static class → no bare constructor
    ];
    for src in cases {
        let (agreed, total) = sweep_sound(src);
        eprintln!("[sweep] {agreed}/{total} {src:?}");
    }
}

/// The expression-position **constructor** fallback, swept generatively over
/// *every* public type the fixture declares in `Demo`. For each, `open Demo; let
/// x = <Name> ()` is checked certain-implies-exact against FCS ([`sweep_sound`]):
/// where we commit an `Entity`, FCS must name the same one; otherwise we must
/// honestly defer. Enumerating from the assembly (not a hand-list) means a new
/// fixture type is probed automatically — the guard against the
/// [`AssemblyEnv::bare_expr_constructible`] predicate silently drifting from
/// FCS's actual constructor surface (a static class, union, record, generic, or
/// abbreviation that must defer, vs a plain class that must resolve).
///
/// This is the systematic backstop for the constructor fallback: the type-kind
/// edge cases (static `Calc`/`Exts` → nothing, generic `Pair<'T>` → the arity
/// sibling, struct unions → nothing) are checked by the machine rather than
/// reasoned about one at a time.
#[test]
fn bare_constructor_fallback_is_sound_over_every_demo_type() {
    let fixture = ensure_assembly_fixture_built();
    let bytes = std::fs::read(fixture).expect("read fixture dll");
    let view = Ecma335Assembly::parse(&bytes).expect("parse fixture dll");
    let entities = view.enumerate_type_defs().expect("enumerate type defs");

    // Distinct public simple names declared directly in `Demo` (dedup the
    // generic-arity siblings `Pair` / `Pair`1` / `Pair`2`, which share a name).
    let mut names: Vec<String> = Vec::new();
    for e in &entities {
        if e.namespace == ["Demo"] && !names.contains(&e.name) {
            names.push(e.name.clone());
        }
    }
    assert!(names.len() > 5, "fixture should declare many Demo types");

    let mut total_agreed = 0usize;
    for name in &names {
        let src = format!("open Demo\nlet x = {name} ()\n");
        let (agreed, _total) = sweep_sound(&src);
        eprintln!("[ctor-sweep] agreed={agreed} {src:?}");
        total_agreed += agreed;
    }
    // Non-vacuity: the fallback must actually resolve at least the plain classes
    // (`Thing`, `Other`, `Widget`, `Gizmo`, `Pair`), or the sweep proves nothing.
    assert!(
        total_agreed >= 4,
        "the constructor fallback resolved nothing — the sweep is vacuous"
    );
}

#[test]
fn assembly_resolution_agrees_with_fcs() {
    // `Demo.Calc.Zero` → the `Calc` type (Entity) + the `Zero` method (Member).
    assert_matches_fcs("module M\nlet x = Demo.Calc.Zero()\n", 2);
    // `Demo.Calc.Answer` → the `Calc` type (Entity) + the `Answer` property.
    assert_matches_fcs("module M\nlet x = Demo.Calc.Answer\n", 2);
    // Two references in one file: each contributes its type + member (the `Calc`
    // entity is reported per-occurrence).
    assert_matches_fcs(
        "module M\nlet a = Demo.Calc.Zero()\nlet b = Demo.Calc.Answer\n",
        4,
    );

    // Stage E: `open Demo` shortens the path — `Calc.Zero` resolves as
    // `Demo.Calc.Zero` (Entity `Demo.Calc` + Member `Zero`), the implicit `Demo`
    // namespace prefix coming from the open, not the source.
    assert_matches_fcs("open Demo\nlet x = Calc.Zero()\n", 2);
    assert_matches_fcs("open Demo\nlet y = Calc.Answer\n", 2);

    // A bare *constructor* call uses the type name as an expression head: `open
    // Demo` then `Thing ()` resolves the bare `Thing` to the `Demo.Thing` type
    // (Entity). FCS reports the type symbol at the occurrence, so this is the
    // expression-position twin of the type-position `(x : Thing)` below — only
    // the type use counts (the `Demo` open-clause qualifier we leave unresolved).
    assert_matches_fcs("open Demo\nlet x = Thing ()\n", 1);

    // An inaccessible `open Demo.Hidden` (internal type) must not suppress the
    // valid `open Demo`: `Calc.Zero` still resolves to `Demo.Calc.Zero`.
    assert_matches_fcs("open Demo\nopen Demo.Hidden\nlet x = Calc.Zero()\n", 2);

    // Stage E long tail: `open type Demo.Calc` brings the type's *static members*
    // into unqualified scope — bare `Zero` / `Answer` resolve to the `Zero`
    // method / `Answer` property of `Demo.Calc` (Member). The open-clause path
    // (`Demo` namespace + `Calc` type) we leave unresolved, which the differential
    // allows; only the bare member use is counted.
    assert_matches_fcs("open type Demo.Calc\nlet x = Zero()\n", 1);
    assert_matches_fcs("open type Demo.Calc\nlet y = Answer\n", 1);

    // The `open type` *target* itself is resolved through the active name
    // environment: `open Demo` shortens `open type Calc` to `Demo.Calc`, so bare
    // `Zero` still resolves to `Demo.Calc.Zero`. Only the bare member use counts
    // (the open clauses, as above, we leave unresolved — allowed by the
    // differential).
    assert_matches_fcs("open Demo\nopen type Calc\nlet x = Zero()\n", 1);

    // An explicit `open` outranks the enclosing namespace when resolving the
    // `open type` target: in `namespace Demo` with `open Demo.Sub`, `open type
    // Calc` binds to `Demo.Sub.Calc` (the open), not `Demo.Calc` (the namespace),
    // so bare `Zero` is `Demo.Sub.Calc.Zero`.
    assert_matches_fcs(
        "namespace Demo\nmodule M =\n    open Demo.Sub\n    open type Calc\n    let x = Zero()\n",
        1,
    );
    // With no shadowing open, the `open type` target falls back to the enclosing
    // namespace: `Calc` binds to `Demo.Calc`, so bare `Zero` is `Demo.Calc.Zero`.
    assert_matches_fcs(
        "namespace Demo\nmodule M =\n    open type Calc\n    let x = Zero()\n",
        1,
    );

    // Two `open type`s sharing a static name: opens form one source-ordered,
    // latest-wins frame, so the *later* open wins — bare `Zero` is
    // `Demo.Sub.Calc.Zero`, not an ambiguity. (Only the bare member use counts.)
    assert_matches_fcs(
        "open type Demo.Calc\nopen type Demo.Sub.Calc\nlet x = Zero()\n",
        1,
    );

    // A later `open type` shadows an earlier in-project local of the same name:
    // `let Answer = 9` then `open type Demo.Calc` makes bare `Answer` the opened
    // static `Demo.Calc.Answer` (the local binding is in-project, so only the
    // shadowing member use resolves into the fixture).
    assert_matches_fcs("let Answer = 9\nopen type Demo.Calc\nlet y = Answer\n", 1);

    // Relative-open canonicalisation (expression path): inside `namespace Demo`,
    // `open Sub` is the *relative* `Demo.Sub` (the fixture has both root `Sub` and
    // `Demo.Sub`), so `Calc.Zero` is `Demo.Sub.Calc.Zero` — the `Calc` entity and
    // the `Zero` member, both agreeing with FCS, never the root `Sub.Calc`.
    assert_matches_fcs(
        "namespace Demo\n\nmodule M =\n    open Sub\n    let z = Calc.Zero()\n",
        2,
    );

    // Completeness disambiguates *between opens*: `open Demo` (whose `Calc` has
    // `Answer`) and `open Sub` (root `Sub.Calc`, which does not) both match the
    // `Calc` prefix, but only `Demo.Calc.Answer` resolves the member — FCS binds
    // it (the `Calc` entity + the `Answer` member = 2), not an ambiguity. Contrast
    // `ambiguous_opens_defer`, where both opens fully resolve `Calc.Zero`.
    assert_matches_fcs("open Demo\nopen Sub\nlet x = Calc.Answer\n", 2);

    // An `open type` target resolved through the **merged root** of a
    // project-namespace open: `namespace Demo; open Sub` opens the merged root
    // `Sub`, so `open type RootOnly` (a type only in root `Sub`) finds its target
    // and is *not* opaque — a later `open type Demo.Calc` then makes bare `Zero`
    // the `Demo.Calc.Zero` member. (Were `RootOnly` missed, the open would go
    // opaque and suppress `Zero`.) Only the bare member use counts; the open-type
    // clauses defer.
    assert_matches_fcs(
        "namespace Demo\nmodule M =\n    open Sub\n    open type RootOnly\n    open type Demo.Calc\n    let x = Zero()\n",
        1,
    );
}

/// Strengthened (completeness) differential: a type-position use that FCS
/// resolves into the fixture assembly **must** resolve on our side too — not be
/// left `Deferred`/unrecorded. Unlike [`assert_matches_fcs`] (sound: deferral is
/// allowed), this *fails* on incompleteness, so the assembly-only envelope's
/// completeness is measured rather than hidden.
///
/// We record the type's [`Resolution::Entity`] at its *segment* (`Thing` in
/// `Demo.Thing`), as the expression path does for an intermediate type; FCS
/// instead spans the rightmost type over the *whole* long-ident (`Demo.Thing`).
/// So the oracle match is by **containment**: FCS must have a fixture use whose
/// range covers our segment with the same (arity-stripped) full name.
/// `needle` is the type-name segment; `expected_full` its entity's full name.
fn assert_type_use_complete(src: &str, needle: &str, expected_full: &str) {
    assert_use_complete(src, needle, expected_full, false);
}

/// Value/member-position counterpart of [`assert_type_use_complete`]: a use FCS
/// resolves into the fixture as an `Entity` **or** `Member` must resolve on our
/// side too (the value path's rooting type is an `Entity` at its segment; the
/// whole path a `Member`). Matched by containment + (arity-stripped) full name.
fn assert_value_use_complete(src: &str, needle: &str, expected_full: &str) {
    assert_use_complete(src, needle, expected_full, true);
}

fn assert_use_complete(src: &str, needle: &str, expected_full: &str, allow_member: bool) {
    let fixture = ensure_assembly_fixture_built();

    let bytes = std::fs::read(fixture).expect("read fixture dll");
    let view = Ecma335Assembly::parse(&bytes).expect("parse fixture dll");
    let env = AssemblyEnv::from_views(std::slice::from_ref(&view)).expect("build AssemblyEnv");
    let parsed = parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors in {src:?}: {:?}",
        parsed.errors
    );
    let file = ImplFile::cast(parsed.root).expect("impl file");
    let rf = resolve_file(&file, &ProjectItems::default(), &env);

    // FCS oracle: pick the occurrence of `needle` that FCS resolves *into the
    // fixture* as `expected_full`. Choosing by the oracle (not the first textual
    // match) lets the same name appear elsewhere — e.g. a `module Calc` header —
    // without the test mistaking that occurrence for the use under test.
    let path = temp_fs_file("asm_use_diff", src);
    let json = invoke_fcs_dump_with_refs("uses", &path, &[fixture]);
    let _ = std::fs::remove_file(&path);
    let uses = parse_fcs_uses(&json, src);
    let start = src
        .match_indices(needle)
        .map(|(i, _)| i)
        .find(|&start| {
            let end = start + needle.len();
            uses.iter()
                .filter(|u| u.start <= start && end <= u.end)
                .filter(|u| u.assembly.as_deref() == Some(FIXTURE_ASM))
                .any(|u| {
                    u.full_name
                        .as_deref()
                        .map(|f| f.split('`').next().unwrap_or(f))
                        == Some(expected_full)
                })
        })
        .unwrap_or_else(|| {
            panic!("oracle: FCS does not resolve any {needle:?} → {expected_full} in {src:?}")
        });
    let end = start + needle.len();

    // Completeness + soundness: we must resolve the segment to the same entity
    // (or, in value/member position, the same member).
    match rf.resolution_at(span(start, end)) {
        Some(res @ Resolution::Entity(_)) => {
            let (asm, full) = our_assembly_full(&env, res);
            assert_eq!(asm, FIXTURE_ASM, "{needle:?}: assembly");
            assert_eq!(full, expected_full, "{needle:?}: full name");
        }
        Some(res @ Resolution::Member { .. }) if allow_member => {
            let (asm, full) = our_assembly_full(&env, res);
            assert_eq!(asm, FIXTURE_ASM, "{needle:?}: assembly");
            assert_eq!(full, expected_full, "{needle:?}: full name");
        }
        other => {
            panic!("incomplete: FCS resolves {needle:?} → {expected_full}, but we gave {other:?}")
        }
    }
}

#[test]
fn value_position_resolution_is_complete_in_the_assembly_envelope() {
    // Fully-qualified and namespace-opened value/member paths must resolve.
    assert_value_use_complete("module M\nlet x = Demo.Calc.Zero()\n", "Calc", "Demo.Calc");
    assert_value_use_complete("open Demo\nlet x = Calc.Zero()\n", "Calc", "Demo.Calc");
    // Enclosing-namespace precedence (tier 2, the stage-2 completeness win): the
    // rooting `Calc` qualifier resolves through the enclosing `Demo` to
    // `Demo.Sub.Calc`, and the whole `Sub.Calc.Zero` to its `Zero` member.
    assert_value_use_complete(
        "namespace Demo\n\nmodule M =\n    let z = Sub.Calc.Zero()\n",
        "Calc",
        "Demo.Sub.Calc",
    );
    assert_value_use_complete(
        "namespace Demo\n\nmodule M =\n    let z = Sub.Calc.Zero()\n",
        "Sub.Calc.Zero",
        "Demo.Sub.Calc.Zero",
    );
    // Top-level blocks are isolated (value position): `namespace A`'s nested
    // `module Sub` is invisible from the distinct `namespace B` block, so it
    // must not shadow the root `Sub.Calc.Zero` there — FCS resolves the
    // fixture. (PR #667 review round 2: the nested-module shadow set
    // accumulated across the whole file. The `open Demo` sibling of this case
    // — `Demo.Sub.Calc.Zero` through the open — stays deferred for an
    // unrelated pre-existing reason: a 3-segment path under an explicit open
    // whose assembly target owns the middle segment is conservatively deferred
    // by the same-file case classifier, even with no project candidate.)
    assert_value_use_complete(
        "namespace A\n\nmodule Sub =\n    module Calc =\n        let Zero () = 9\n\n\
         namespace B\n\nmodule M =\n    let z = Sub.Calc.Zero()\n",
        "Calc",
        "Sub.Calc",
    );
}

#[test]
fn type_position_resolution_is_complete_in_the_assembly_envelope() {
    // Fully-qualified and namespace-opened type names, arity 0/1/2 — every one
    // FCS resolves into the fixture, so we must too.
    assert_type_use_complete(
        "module M\nlet f (x : Demo.Thing) = x\n",
        "Thing",
        "Demo.Thing",
    );
    assert_type_use_complete("open Demo\nlet f (x : Thing) = x\n", "Thing", "Demo.Thing");
    assert_type_use_complete("open Demo\nlet f (x : Pair) = x\n", "Pair", "Demo.Pair");
    assert_type_use_complete(
        "open Demo\nlet f (x : Pair<int>) = x\n",
        "Pair",
        "Demo.Pair",
    );
    assert_type_use_complete(
        "module M\nlet f (x : Demo.Pair<int, string>) = x\n",
        "Pair",
        "Demo.Pair",
    );
    // Enclosing-namespace precedence (tier 2): a bare type and a qualified
    // namespace-relative type, both resolved through the enclosing `Demo`.
    assert_type_use_complete(
        "namespace Demo\n\nmodule M =\n    let f (x : Thing) = x\n",
        "Thing",
        "Demo.Thing",
    );
    assert_type_use_complete(
        "namespace Demo\n\nmodule M =\n    let f (x : Sub.Thing) = x\n",
        "Thing",
        "Demo.Sub.Thing",
    );
    // Chained open: `open Demo; open Sub` shortens the second open through the
    // first (→ `Demo.Sub`), so `Deep` (only in `Demo.Sub`) is `Demo.Sub.Deep`.
    assert_type_use_complete(
        "open Demo\nopen Sub\nlet f (x : Deep) = x\n",
        "Deep",
        "Demo.Sub.Deep",
    );
    // Ancestor exclusion (FS0039): in `namespace Demo.Sub`, `Sub.Calc` resolves
    // through the root (`Sub.Calc`), not the ancestor `Demo` (`Demo.Sub.Calc`).
    assert_type_use_complete(
        "namespace Demo.Sub\n\nmodule M =\n    let f (x : Sub.Calc) = x\n",
        "Calc",
        "Sub.Calc",
    );
    // Accessibility: `Demo.Hush` holds only an internal type, so `open Hush` in
    // `namespace Demo` falls back to the public root `Hush` — `Visible` is
    // `Hush.Visible`, agreeing with FCS.
    assert_type_use_complete(
        "namespace Demo\n\nmodule M =\n    open Hush\n    let f (x : Visible) = x\n",
        "Visible",
        "Hush.Visible",
    );
    // A **module is not a type**: the file's *own* top-level `module Calc` does
    // not shadow the assembly type `Demo.Calc` in type position — `open Demo`
    // resolves `(x : Calc)` to the assembly type (FCS). The shared tier walker
    // must try the opens tier *before* the as-written module shadow vetoes (which
    // gates only the value path). The module-header `Calc` use is skipped by the
    // oracle (FCS resolves it to the project module, not the fixture).
    assert_type_use_complete(
        "module Calc\nopen Demo\nlet f (x : Calc) = x\n",
        "Calc",
        "Demo.Calc",
    );
    // Top-level blocks are isolated: a nested `module Sub` in `namespace A` is
    // NOT visible from a distinct `namespace B` block later in the same file, so
    // it must not veto `Sub.Calc` there — FCS resolves the opened assembly
    // `Demo.Sub.Calc`. (PR #667 review round 2: the nested-module shadow set
    // accumulated across the whole file instead of per top-level block.)
    assert_type_use_complete(
        "namespace A\n\nmodule Sub =\n    let x = 1\n\n\
         namespace B\n\nmodule M =\n    open Demo\n    let f (y : Sub.Calc) = y\n",
        "Calc",
        "Demo.Sub.Calc",
    );
    // (Nested-type resolution — `Demo.Thing.Inner` — is pinned by handle in the
    // FCS-free `resolve_assembly.rs`; the differential's `our_assembly_full`
    // doesn't reconstruct a nested type's enclosing-chain full name, so it's not
    // re-checked here.)
}
