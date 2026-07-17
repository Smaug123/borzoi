//! Skip-count ratchet over the SDK's `Microsoft.NETCore.App.Ref` reference
//! pack — the ~170 reference assemblies every .NET build resolves against,
//! and so the exact real-world surface the LSP's assembly reader must hold.
//!
//! The projector's "bound uncertainty" posture drops-and-records what it
//! cannot model (`Entity::skipped_members`, the assembly-level dropped-type
//! list) instead of failing, which is right for the LSP — but it means coverage
//! can regress *silently*: a change that suddenly drops a thousand members
//! still enumerates `Ok`. The fixtures pin individual constructs; nothing
//! pinned the aggregate. This sweep is that pin: every pack DLL must parse and
//! enumerate, the kept-type total must not collapse, and the drop totals must
//! stay within a budget.
//!
//! The budgets carry ~25% headroom over the observed totals (10.0.8 pack:
//! 3,926 types kept, 162 member drops, 52 type drops) so routine SDK patch
//! drift doesn't trip them, while anything pathological — the class of bug
//! where one undecodable construct zeroes a whole assembly — blows straight
//! through. When genuine reader improvements lower the observed numbers,
//! ratchet the budgets down; when a new SDK legitimately raises them, the
//! failure message prints the per-DLL offenders to justify raising.
//!
//! Requires the .NET 10 SDK on PATH — the Nix devShell provides it.

use borzoi_assembly::{Ecma335Assembly, EcmaView, Entity};

use crate::common::sdk_ref_pack_dir;

/// Ratchet budgets and floors. See the module doc for the observed baseline
/// these wrap.
const MIN_DLLS: usize = 150;
/// Measured 3981 (2026-07-12), up from the 3.5k this floor was set at: the
/// generic-math / parsing interfaces (`INumber`, `IParsable`,
/// `IComparisonOperators`, …) stopped being dropped once constraint-row
/// `[Nullable]` attributes were classified rather than refused (EX-2).
const MIN_TOTAL_TYPES: usize = 3_900;
const MAX_MEMBER_DROPS: usize = 180;
/// **Zero.** Every type in the .NET 10 reference pack now projects. The previous
/// budget of 65 was almost entirely one cause: a `GenericParamConstraint` row
/// carrying `[Nullable]` (which the BCL emits on `where TSelf : IParsable<TSelf>`
/// and friends) was refused as "hand-authored metadata we cannot represent" — a
/// claim that was true when written and false on .NET 9+. Dropping those types
/// also made every namespace holding one *unknowable* to the overload engine's
/// extension gate, which is how a stale metadata assumption ended up costing
/// overload coverage in every file that says `open System`.
const MAX_TYPE_DROPS: usize = 0;

/// Count kept types, recursing into nested types.
fn count_types(entities: &[Entity]) -> usize {
    entities
        .iter()
        .map(|e| 1 + count_types(&e.nested_types))
        .sum()
}

/// Count recorded member drops, recursing into nested types.
fn count_member_drops(entities: &[Entity]) -> usize {
    entities
        .iter()
        .map(|e| e.skipped_members.len() + count_member_drops(&e.nested_types))
        .sum()
}

#[test]
fn ref_pack_projects_within_the_skip_budget() {
    let dir = sdk_ref_pack_dir();
    let mut paths: Vec<_> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read ref pack dir {dir:?}: {e}"))
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "dll"))
        .collect();
    paths.sort();

    let mut total_types = 0usize;
    let mut total_member_drops = 0usize;
    let mut total_type_drops = 0usize;
    // Per-DLL drop tallies, kept for the failure message: a budget breach is
    // only actionable if it names where the drops live.
    let mut offenders: Vec<(String, usize, usize)> = Vec::new();

    for path in &paths {
        let name = path
            .file_name()
            .expect("dll path has a file name")
            .to_string_lossy()
            .to_string();
        let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("read ref-pack DLL {name}: {e}"));
        // Reference assemblies are ordinary managed images; a parse or
        // enumeration *error* on one is not a budget question but a plain
        // regression — every one of these previously projected `Ok`.
        let view = Ecma335Assembly::parse(&bytes)
            .unwrap_or_else(|e| panic!("ref-pack DLL {name} failed to parse: {e}"));
        let (entities, skips) = view
            .enumerate_type_defs_with_skips()
            .unwrap_or_else(|e| panic!("ref-pack DLL {name} failed to enumerate: {e}"));

        let member_drops = count_member_drops(&entities);
        let type_drops = skips.dropped_types.len();
        total_types += count_types(&entities);
        total_member_drops += member_drops;
        total_type_drops += type_drops;
        if member_drops > 0 || type_drops > 0 {
            offenders.push((name, member_drops, type_drops));
        }
    }

    offenders.sort_by_key(|&(_, m, t)| std::cmp::Reverse(m + t));
    let summary = format!(
        "{} DLLs, {total_types} types kept, {total_member_drops} member drops, \
         {total_type_drops} type drops; offenders (dll, member drops, type drops): \
         {offenders:#?}",
        paths.len(),
    );

    assert!(
        paths.len() >= MIN_DLLS,
        "expected a full reference pack (≥ {MIN_DLLS} DLLs) at {dir:?}; {summary}"
    );
    assert!(
        total_types >= MIN_TOTAL_TYPES,
        "kept-type total collapsed below {MIN_TOTAL_TYPES}; {summary}"
    );
    assert!(
        total_member_drops <= MAX_MEMBER_DROPS,
        "member-drop total exceeded the {MAX_MEMBER_DROPS} budget; {summary}"
    );
    // Equality, not `<=`: `MAX_TYPE_DROPS` is 0, and `<= 0` on a `usize` trips
    // `clippy::absurd_extreme_comparisons` under `-D warnings`.
    assert!(
        total_type_drops == MAX_TYPE_DROPS,
        "type-drop total is {total_type_drops}, expected {MAX_TYPE_DROPS}; {summary}"
    );

    // Not an assertion, but make the healthy numbers visible in --nocapture
    // runs so ratcheting the budgets down stays easy.
    eprintln!("[bcl_ref_pack_sweep] {summary}");
}

/// `(kind, IL name, is_static, has explicit-interface entries)` for every
/// member of `e` that can carry them.
fn member_explicit_flags(e: &Entity) -> Vec<(&'static str, &str, bool, bool)> {
    e.members
        .iter()
        .filter_map(|m| match m {
            borzoi_assembly::Member::Method(m) => Some((
                "method",
                m.name.as_str(),
                m.is_static,
                !m.implements.is_empty(),
            )),
            borzoi_assembly::Member::Property(p) => Some((
                "property",
                p.name.as_str(),
                p.is_static,
                !p.implements.is_empty(),
            )),
            borzoi_assembly::Member::Event(ev) => Some((
                "event",
                ev.name.as_str(),
                ev.is_static,
                !ev.implements.is_empty(),
            )),
            borzoi_assembly::Member::Field(_) => None,
        })
        .collect()
}

#[test]
fn explicit_impl_classification_agrees_with_roslyn_convention_across_the_pack() {
    // Corpus-wide differential between two independent notions of "implements
    // an interface member":
    //
    // - ours, classified from the `MethodImpl` declaration target (interface
    //   vs base class) — see `reader/members.rs::apply_method_impls`;
    // - Roslyn's, readable off the member name: Roslyn name-mangles exactly
    //   the *explicit* interface implementations (`IFace<…>.Member`, hence a
    //   `.`) and nothing else it emits into a reference assembly.
    //
    // Over ~170 real assemblies:
    //
    // - every dotted member must be flagged (a miss is a wrongly-skipped row
    //   or a regressed interface decode) — both instance and static;
    // - a flagged *instance* member must be dotted (a plain-named one means a
    //   base-class override was misclassified as an interface impl: instance
    //   `MethodImpl` rows exist only for explicit impls, which Roslyn always
    //   mangles);
    // - a flagged plain-named *static* member is correct and expected: static
    //   interface members have no vtable slot, so even *implicit* impls (C#11
    //   generic math — `NFloat` satisfying `INumberBase<NFloat>.Parse`) are
    //   wired through `MethodImpl`. The 10.0 pack carries ~1,100 of them.
    //
    // The two oracles share no code — the name is never consulted by the
    // classifier — so agreement here is evidence, not tautology. (The shapes
    // where the notions *deliberately* diverge are compiler-unreachable;
    // `methodimpl_classification.rs` pins them from fabricated IL.)
    let dir = sdk_ref_pack_dir();
    let mut paths: Vec<_> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read ref pack dir {dir:?}: {e}"))
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "dll"))
        .collect();
    paths.sort();

    struct Totals {
        flagged: usize,
        implicit_static: usize,
        violations: Vec<String>,
    }
    fn walk(dll: &str, e: &Entity, t: &mut Totals) {
        for (kind, name, is_static, flagged) in member_explicit_flags(e) {
            let is_ctor = name == ".ctor" || name == ".cctor";
            let dotted = name.contains('.') && !is_ctor;
            if flagged {
                t.flagged += 1;
            }
            let violation = if flagged && !dotted && is_static {
                t.implicit_static += 1; // implicit static impl — expected
                false
            } else {
                flagged != dotted
            };
            if violation {
                t.violations.push(format!(
                    "{dll}: {kind} `{}::{name}` — dotted={dotted}, static={is_static}, \
                     flagged={flagged}",
                    e.name
                ));
            }
        }
        for n in &e.nested_types {
            walk(dll, n, t);
        }
    }
    let mut totals = Totals {
        flagged: 0,
        implicit_static: 0,
        violations: Vec::new(),
    };

    for path in &paths {
        let name = path
            .file_name()
            .expect("dll path has a file name")
            .to_string_lossy()
            .to_string();
        let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("read ref-pack DLL {name}: {e}"));
        let view = Ecma335Assembly::parse(&bytes)
            .unwrap_or_else(|e| panic!("ref-pack DLL {name} failed to parse: {e}"));
        let (entities, _skips) = view
            .enumerate_type_defs_with_skips()
            .unwrap_or_else(|e| panic!("ref-pack DLL {name} failed to enumerate: {e}"));
        for e in &entities {
            walk(&name, e, &mut totals);
        }
    }
    let (flagged_total, implicit_static_total) = (totals.flagged, totals.implicit_static);

    assert!(
        totals.violations.is_empty(),
        "{} classification/convention disagreements across the pack:\n{}",
        totals.violations.len(),
        totals.violations.join("\n"),
    );
    // Anti-vacuity floors: the pack carries thousands of interface-member
    // impls (and specifically ~1,100 implicit static ones); a classifier that
    // silently dropped a whole category would satisfy the agreement check in
    // the worst way.
    assert!(
        flagged_total >= 1_000,
        "only {flagged_total} interface-member impls recognised across the whole \
         pack — the classifier is dropping real ones",
    );
    assert!(
        implicit_static_total >= 500,
        "only {implicit_static_total} implicit static interface impls recognised \
         across the whole pack — the static-body arm regressed",
    );
    eprintln!(
        "[bcl_ref_pack_sweep] {flagged_total} interface-member impls recognised \
         across the pack ({implicit_static_total} implicit static)"
    );
}
