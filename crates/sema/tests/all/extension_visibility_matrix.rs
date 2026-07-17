//! The extension-visibility **matrix**: every declaration shape an extension
//! member can take, crossed with every access channel, diffed against FCS.
//!
//! ## Why a matrix, and why this property
//!
//! `resolve_assembly_diff.rs` quantifies over *FCS's* uses: for every use FCS
//! resolves into the fixture assembly, ours agrees or honestly defers. That
//! property is one-directional, and it is blind in exactly the place the
//! extension bugs lived — when FCS reports FS0039 it emits **no use at all**, so a
//! name we wrongly resolve is a name the oracle is never asked about. Every defect
//! in PR #916 (and both findings of its review) sat in that blind spot:
//!
//! - bare `Force` / `Select` resolved where FCS says FS0039 (we spoke; FCS was silent);
//! - a `let` sharing its name with an augmentation stopped resolving (FCS spoke; we were silent).
//!
//! So this harness makes **absence a value on both sides** and asserts an exact
//! bijection per probe: FCS resolves the probe to `X` ⟺ we resolve it to `X`, and
//! FCS resolves nothing ⟺ we resolve nothing (no member, no entity). A deferral is
//! *not* accepted where FCS resolves — that would re-open the second blind spot —
//! so the fixture is built to keep every probe uniquely selectable (no overload
//! sets, whose deferral is legitimate and is covered elsewhere).
//!
//! ## The grid
//!
//! Declaration shapes (`tests/fixtures/autoopen_env/Fixture.fs`, `Demo.ExtMatrix`):
//! instance / static / `[<CompiledName>]`-renamed / generic augmentation, an
//! augmentation colliding with a plain `let`, a plain `let`, an `[<Extension>]`
//! module `let`, a C#-style `[<Extension>]` static (F#-declared), and a plain static
//! beside it; plus the C# fixture's `Demo.Exts` (a real Roslyn-emitted extension
//! method) and `Demo.Calc` (plain statics).
//!
//! Access channels: bare after `open`, bare after auto-open, module-qualified, bare
//! after `open type`, type-qualified.
//!
//! The cells are *generated*, not hand-picked, and the expectation is whatever FCS
//! says — nothing here hardcodes a rule. When a new declaration shape or channel is
//! added, its whole row/column is checked for free.

use std::path::Path;

use crate::common::{
    ensure_assembly_fixture_built, invoke_fcs_dump_with_refs, parse_fcs_uses, temp_fs_file,
};
use borzoi_assembly::{Ecma335Assembly, Member};
use borzoi_cst::parser::parse;
use borzoi_cst::syntax::{AstNode, ImplFile};
use borzoi_sema::{AssemblyEnv, ProjectItems, Resolution, resolve_file};
use rowan::TextRange;

/// Build the F# auto-open fixture (which carries the `Demo.ExtMatrix` shapes) once.
///
/// Delegates to [`crate::common::ensure_autoopen_fixture_built`], which builds it
/// behind the binary-wide `BUILD_LOCK`; two groups building this same fixture
/// concurrently would otherwise race its `obj/`/`bin/`.
fn ensure_autoopen_fixture_built() -> &'static Path {
    crate::common::ensure_autoopen_fixture_built()
}

/// One cell of the matrix: a snippet that references `probe` through one channel.
struct Cell {
    /// What the row/column is, for the failure message.
    label: &'static str,
    /// Lines that bring the name into scope (`open …`), if any.
    opens: &'static [&'static str],
    /// The exact source text of the reference — the probe span.
    probe: &'static str,
    /// The rest of the call, so the snippet is well-formed (`" 1"`, `" \"s\""`, …).
    args: &'static str,
}

const CELLS: &[Cell] = &[
    // ---- module `open`: augmentations are invisible; plain `let`s are not ----
    Cell {
        label: "open / instance augmentation",
        opens: &["open Demo.ExtMatrix.Aug"],
        probe: "InstAug",
        args: " \"s\"",
    },
    Cell {
        label: "open / static augmentation",
        opens: &["open Demo.ExtMatrix.Aug"],
        probe: "StatAug",
        args: " \"s\"",
    },
    Cell {
        label: "open / renamed augmentation",
        opens: &["open Demo.ExtMatrix.Aug"],
        probe: "RenamedAug",
        args: " \"s\"",
    },
    Cell {
        label: "open / generic augmentation",
        opens: &["open Demo.ExtMatrix.Aug"],
        probe: "GenericAug",
        args: " \"s\"",
    },
    Cell {
        label: "open / plain let",
        opens: &["open Demo.ExtMatrix.Aug"],
        probe: "plainLet",
        args: " 1",
    },
    Cell {
        label: "open / let colliding with an augmentation",
        opens: &["open Demo.ExtMatrix.Aug"],
        probe: "Clash",
        args: " 1",
    },
    // ---- auto-open: same rules, reached through the implicit fold ----
    Cell {
        label: "auto-open / instance augmentation",
        opens: &["open Demo.ExtMatrix"],
        probe: "AutoInstAug",
        args: " \"s\"",
    },
    Cell {
        label: "auto-open / static augmentation",
        opens: &["open Demo.ExtMatrix"],
        probe: "AutoStatAug",
        args: " \"s\"",
    },
    Cell {
        label: "auto-open / plain let",
        opens: &["open Demo.ExtMatrix"],
        probe: "autoPlainLet",
        args: " 1",
    },
    // ---- `[<Extension>]` module: the `let`s are values, so they ARE in scope ----
    Cell {
        label: "open / [<Extension>] module let",
        opens: &["open Demo.ExtMatrix.ExtAttrLets"],
        probe: "ExtAttrLet",
        args: " 1",
    },
    // ---- module-qualified: augmentations are unreachable here too ----
    Cell {
        label: "module-qualified / instance augmentation",
        opens: &[],
        probe: "Demo.ExtMatrix.Aug.InstAug",
        args: " \"s\"",
    },
    Cell {
        label: "module-qualified / static augmentation",
        opens: &[],
        probe: "Demo.ExtMatrix.Aug.StatAug",
        args: " \"s\"",
    },
    Cell {
        label: "module-qualified / plain let",
        opens: &[],
        probe: "Demo.ExtMatrix.Aug.plainLet",
        args: " 1",
    },
    Cell {
        label: "module-qualified / let colliding with an augmentation",
        opens: &[],
        probe: "Demo.ExtMatrix.Aug.Clash",
        args: " 1",
    },
    // ---- `open type` on an F#-declared C#-style extension type ----
    Cell {
        label: "open type / C#-style extension static",
        opens: &["open type Demo.ExtMatrix.ExtType"],
        probe: "CsStyle",
        args: " 1",
    },
    Cell {
        label: "open type / plain static beside it",
        opens: &["open type Demo.ExtMatrix.ExtType"],
        probe: "PlainStatic",
        args: " 1",
    },
    Cell {
        label: "open type / curried C#-style extension static",
        opens: &["open type Demo.ExtMatrix.ExtType"],
        probe: "CurriedExt",
        args: " 1 2",
    },
    // ---- type-qualified: a C#-style extension static IS reachable ----
    Cell {
        label: "type-qualified / C#-style extension static",
        opens: &[],
        probe: "Demo.ExtMatrix.ExtType.CsStyle",
        args: " 1",
    },
    Cell {
        label: "type-qualified / plain static",
        opens: &[],
        probe: "Demo.ExtMatrix.ExtType.PlainStatic",
        args: " 1",
    },
    // ---- the C# assembly: a real Roslyn-emitted extension method ----
    Cell {
        label: "open type / Roslyn extension method",
        opens: &["open type Demo.Exts"],
        probe: "Doubled",
        args: " 1",
    },
    Cell {
        label: "open type / plain static in the extension class",
        opens: &["open type Demo.Exts"],
        probe: "Origin",
        args: " ()",
    },
    Cell {
        label: "type-qualified / Roslyn extension method",
        opens: &[],
        probe: "Demo.Exts.Doubled",
        args: " 1",
    },
    Cell {
        label: "open type / plain C# static class",
        opens: &["open type Demo.Calc"],
        probe: "Zero",
        args: " ()",
    },
    // ---- TIERED: a lower `open` holding an ordinary member of the same name ----
    //
    // The ownership dimension. In a single tier, "we own the path and defer" and
    // "the name is absent here" are the same observation (nothing resolves), which
    // is why the matrix was blind to both of PR #916's round-3 and round-4 bugs.
    // With a lower tier present, FCS *names the owner*, so each failure mode shows
    // up as a different full name rather than a shared silence:
    //   - wrongly owning  ⇒ we resolve nothing where FCS resolves `Demo.TierLow.…`
    //   - wrongly absent  ⇒ we resolve `Demo.TierLow.…` where FCS resolves TierHigh
    //
    // Augmentations are unreachable qualified, so these must fall THROUGH to the
    // lower tier (this is exactly the round-4 bug, and it fails without that fix).
    Cell {
        label: "tiered qualified / instance augmentation",
        opens: &["open Demo.TierLow", "open Demo.TierHigh"],
        probe: "M.InstAug",
        args: " \"s\"",
    },
    Cell {
        label: "tiered qualified / static augmentation",
        opens: &["open Demo.TierLow", "open Demo.TierHigh"],
        probe: "M.StatAug",
        args: " \"s\"",
    },
    Cell {
        label: "tiered qualified / renamed augmentation",
        opens: &["open Demo.TierLow", "open Demo.TierHigh"],
        probe: "M.RenamedAug",
        args: " \"s\"",
    },
    // The converse guard: an ordinary `let` in the higher tier must NOT fall
    // through. A fix that made every extension-ish member non-owning would pass the
    // three cells above and fail this one.
    Cell {
        label: "tiered qualified / plain let in the higher tier",
        opens: &["open Demo.TierLow", "open Demo.TierHigh"],
        probe: "M.TierPlain",
        args: " 1",
    },
    // A C#-style extension static IS reachable qualified, so this one must NOT fall
    // through either — the mirror image of the augmentation cells above.
    Cell {
        label: "tiered type-qualified / C#-style extension static",
        opens: &["open Demo.TierLow", "open Demo.TierHigh"],
        probe: "TierType.CsStyle",
        args: " 1",
    },
    Cell {
        label: "tiered type-qualified / plain static",
        opens: &["open Demo.TierLow", "open Demo.TierHigh"],
        probe: "TierType.PlainStatic",
        args: " 1",
    },
    // …and the same C#-style static in the BARE channel, where it *is* hidden: it
    // must fall through to the lower tier's ordinary static. One shape, opposite
    // answers in the two channels — the ownership rule cannot be keyed on the
    // member alone, only on (member, channel).
    Cell {
        label: "tiered open type / C#-style extension static hidden by the higher tier",
        opens: &[
            "open type Demo.TierLow.TierType",
            "open type Demo.TierHigh.TierType",
        ],
        probe: "CsStyle",
        args: " 1",
    },
    Cell {
        label: "tiered open type / plain static in the higher tier",
        opens: &[
            "open type Demo.TierLow.TierType",
            "open type Demo.TierHigh.TierType",
        ],
        probe: "PlainStatic",
        args: " 1",
    },
    // The same bare-channel fall-through against the **C# assembly**, where there is
    // no uncertainty to hide behind: a Roslyn extension method always has exactly one
    // argument group, so `Doubled` is decidably hidden and the lower tier's ordinary
    // static must win. This is the cell that actually pins the property — its
    // F#-assembly twin above can only ever be a deferral (see KNOWN_GAPS).
    Cell {
        label: "tiered open type / Roslyn extension method hidden by the higher tier",
        opens: &["open type Demo.TierLow.TierCs", "open type Demo.Exts"],
        probe: "Doubled",
        args: " 1",
    },
    // Converse guard: the plain static beside it is visible, so the higher tier keeps
    // the name — a fix that made the whole extension *class* fall through fails here.
    Cell {
        label: "tiered open type / plain static in the Roslyn extension class",
        opens: &["open type Demo.TierLow.TierCs", "open type Demo.Exts"],
        probe: "Origin",
        args: " ()",
    },
];

/// Cells where we knowingly resolve **nothing** and FCS resolves something — a
/// deferral, never a wrong target. Each is a real gap with a reason; the test pins
/// both halves (we still defer, FCS still resolves), so closing a gap *fails* the
/// test until its entry is deleted. Nothing may be added here for a cell where we
/// name the wrong target: that is a bug, not a gap.
const KNOWN_GAPS: &[(&str, &str)] = &[
    // (The three `open <assembly module>` cells this matrix found on its first run
    // are CLOSED — Slice A of `docs/assembly-module-open-plan.md`. Their entries are
    // deleted, which is what the ratchet below demands: a fixed gap fails the test
    // until it is removed from this list.)
    // FCS's C#-style predicate matches only a method with exactly ONE argument
    // group, so a curried `[<Extension>] static member M x y` stays in scope. In an
    // **F# assembly** the flattened IL signature cannot say whether the source was
    // curried (kept) or tupled (hidden) — `arg_group_count` is `None` — and both
    // guesses are wrong resolutions, so we defer. (A Roslyn extension method always
    // has one group, so the C#-fixture cells below are exact.) Closing this needs the
    // pickle's `tcaug` member vals — a type-member projection this migration
    // deliberately scoped out (`docs/completed/fsharp-pickle-member-projection-plan.md` §2).
    (
        "open type / curried C#-style extension static",
        "curried-vs-tupled is invisible in an F# assembly's IL signature",
    ),
    // The *tiered* face of that same gap, and the reason the tier dimension was
    // worth adding: in one tier, "FCS hides this member" and "we cannot tell, so we
    // defer" are the same observation (nothing resolves) — the single-tier cell
    // above passes by coincidence. With a lower tier holding an ordinary `CsStyle`,
    // FCS *names* it (the higher tier's member is hidden, so it falls through) while
    // we still defer: the name might be a curried extension, which WOULD shadow, and
    // the two readings name different targets. Deferring is the honest answer, but
    // it is a lost resolution, and it is only visible here.
    //
    // Not a gap in the C# fixture's twin cell (`Doubled`), where the one-argument-
    // group rule decides it — that cell must always pass. Closing this one needs the
    // pickle's `tcaug` member vals, exactly as the entry above.
    (
        "tiered open type / C#-style extension static hidden by the higher tier",
        "curried-vs-tupled is invisible in an F# assembly's IL signature",
    ),
];

/// The snippet for one cell, and the byte span of its probe within it.
fn cell_source(cell: &Cell) -> (String, TextRange) {
    let mut src = String::new();
    for open in cell.opens {
        src.push_str(open);
        src.push('\n');
    }
    src.push_str("let probeResult = ");
    let start = src.len();
    src.push_str(cell.probe);
    let end = src.len();
    src.push_str(cell.args);
    src.push('\n');
    let span = TextRange::new(
        u32::try_from(start).unwrap().into(),
        u32::try_from(end).unwrap().into(),
    );
    (src, span)
}

/// What a side resolved the probe to: the symbol's full name, or `None` for
/// "nothing at all" (FCS: FS0039 / no symbol use; us: unrecorded or deferred).
type Resolved = Option<String>;

/// FCS's reading of the probe: the use whose span *covers* the probe (FCS reports a
/// member of a qualified path as spanning the whole path) and which lands in one of
/// our fixture assemblies. `None` when FCS resolved nothing there.
fn fcs_resolved(json: &str, src: &str, probe: TextRange) -> Resolved {
    let start: usize = usize::from(probe.start());
    let end: usize = usize::from(probe.end());
    parse_fcs_uses(json, src)
        .into_iter()
        .filter(|u| u.start != u.end)
        .filter(|u| {
            matches!(
                u.assembly.as_deref(),
                Some("SemaAutoOpenFixture") | Some("SemaAssemblyEnvFixture")
            )
        })
        // The probe span, or a use covering it (the whole dotted path).
        .find(|u| u.start <= start && u.end >= end)
        .and_then(|u| u.full_name)
}

/// Our reading of the probe: the full name of the entity/member we resolve it to.
/// A `Deferred` — or nothing recorded — is `None`: we resolved nothing.
fn our_resolved(env: &AssemblyEnv, src: &str, probe: TextRange) -> Resolved {
    fn full(ns: &[String], name: &str) -> String {
        if ns.is_empty() {
            name.to_string()
        } else {
            format!("{}.{}", ns.join("."), name)
        }
    }
    fn member_name(m: &Member) -> &str {
        match m {
            Member::Method(x) => &x.name,
            Member::Field(x) => &x.name,
            Member::Property(x) => &x.name,
            Member::Event(x) => &x.name,
        }
    }
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "parse errors in {src:?}");
    let file = ImplFile::cast(parsed.root).expect("impl file");
    let rf = resolve_file(&file, &ProjectItems::default(), env);
    match rf.resolution_at(probe) {
        Some(Resolution::Entity(h)) => {
            let e = env.entity(h);
            Some(full(&e.namespace, &e.name))
        }
        Some(Resolution::Member { parent, idx }) => {
            let e = env.entity(parent);
            let m = env.member_at(parent, idx);
            // Compare on the F# *source* name FCS reports, not the IL name.
            let member = match m {
                Member::Method(x) => x.source_name.as_deref().unwrap_or(&x.name),
                _ => member_name(m),
            };
            Some(format!("{}.{}", full(&e.namespace, &e.name), member))
        }
        None | Some(Resolution::Deferred(_)) => None,
        // A probe naming an assembly symbol can never be an in-file binding, and
        // `Unresolved` is reserved for Phase-4 diagnostics (never produced).
        Some(other) => panic!("unexpected resolution for an assembly probe: {other:?}"),
    }
}

/// The whole grid, in one assertion: every cell's probe must resolve on our side to
/// exactly what FCS resolves it to — including "to nothing".
#[test]
fn extension_visibility_matches_fcs_on_every_cell() {
    let fsharp = ensure_autoopen_fixture_built();
    let csharp = ensure_assembly_fixture_built();

    let env = {
        let fs_bytes = std::fs::read(fsharp).expect("read F# fixture dll");
        let cs_bytes = std::fs::read(csharp).expect("read C# fixture dll");
        let fs_view = Ecma335Assembly::parse(&fs_bytes).expect("parse F# fixture");
        let cs_view = Ecma335Assembly::parse(&cs_bytes).expect("parse C# fixture");
        AssemblyEnv::from_views(&[fs_view, cs_view]).expect("build AssemblyEnv")
    };

    let mut mismatches: Vec<String> = Vec::new();
    for cell in CELLS {
        let (src, probe) = cell_source(cell);
        let path = temp_fs_file("ext_matrix", &src);
        let json = invoke_fcs_dump_with_refs("uses", &path, &[fsharp, csharp]);
        let _ = std::fs::remove_file(&path);

        let fcs = fcs_resolved(&json, &src, probe);
        let ours = our_resolved(&env, &src, probe);

        if let Some((_, reason)) = KNOWN_GAPS.iter().find(|(label, _)| *label == cell.label) {
            // A known gap must still be exactly a *deferral against a resolving FCS*.
            // If we started naming a target, that is a wrong resolution; if FCS stopped
            // resolving, or we started, the entry is stale and must be deleted.
            if ours.is_some() || fcs.is_none() {
                mismatches.push(format!(
                    "  {} [KNOWN GAP: {}]\n    this cell no longer behaves as the gap describes — \
                     if it is fixed, delete its KNOWN_GAPS entry; if we now name a target, that is \
                     a wrong resolution\n    FCS:  {:?}\n    ours: {:?}",
                    cell.label, reason, fcs, ours
                ));
            }
            continue;
        }

        if fcs != ours {
            mismatches.push(format!(
                "  {}\n    source: {:?}\n    FCS:  {:?}\n    ours: {:?}",
                cell.label,
                src.replace('\n', "\\n"),
                fcs,
                ours
            ));
        }
    }

    assert!(
        mismatches.is_empty(),
        "{} of {} extension-visibility cells disagree with FCS:\n{}",
        mismatches.len(),
        CELLS.len(),
        mismatches.join("\n")
    );
}
