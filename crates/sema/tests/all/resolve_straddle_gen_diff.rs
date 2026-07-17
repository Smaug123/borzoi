//! Generative multi-file **namespace-straddle** differential against FCS.
//!
//! The existing generative differential (`resolve_diff`) emits no `namespace`
//! fragments carrying `[<AutoOpen>]` submodules / plain augmentations / nesting,
//! so the corner-prone cross-tier auto-open fold stayed unexercised while codex
//! kept finding corners in it (`docs/sema-accessibility-collapse-foundation.md`,
//! Stage 5). This harness closes that gap: it *enumerates* multi-file projects
//! that place a single probed name `X` at several `(container, file)` positions —
//! the namespace's own direct tier, `[<AutoOpen>]`/plain module fragments, and
//! nested modules — permutes their Compile order, and checks FCS's resolution of
//! a bare `open N; X` against ours.
//!
//! **Both namespaces are swept.** The fold feeds the value (expression) namespace
//! and the constructor (pattern) namespace through the *same* per-fragment,
//! file-ordered machinery, so both must be exercised:
//!
//! - the **value sweep** declares `X` as a `let` value in each module fragment
//!   (the namespace-tier `Direct` is always an `exception`, since a namespace
//!   holds no values) and probes the *expression* `open N; X`;
//! - the **case sweep** declares `X` as an `exception` everywhere and probes
//!   *both* the expression and a *pattern* `match v with X _` — the constructor
//!   namespace, where a bare value would never appear.
//!
//! **The oracle is per-reference, not per-FCS-use.** The ordinary
//! `uses-project` agree-or-defer harness (`resolve_project_diff`) iterates the
//! uses *FCS resolved*, so it cannot see a reference FCS left **unbound** where
//! we wrongly commit a target (the auto-open-under-a-plain-parent corner). Here
//! we know exactly where each `X` is written, so we read FCS's answer *at that
//! site* — resolved-to-`(file, range)` or absent (unbound) — and assert:
//!
//! - FCS resolved → we resolve to the **same** file+range, or honestly defer;
//! - FCS unbound  → we **defer** (never commit a target).
//!
//! Both directions are gated to zero. Deferring is always allowed (sound
//! availability loss); a wrong or divergent target fails.

use std::path::{Path, PathBuf};

use crate::common::{invoke_fcs_dump_project, parse_fcs_uses_project, temp_fs_file};
use borzoi_cst::parser::parse;
use borzoi_cst::syntax::{AstNode, ImplFile};
use borzoi_sema::{AssemblyEnv, Resolution, resolve_project};
use rowan::TextRange;

fn impl_file(src: &str) -> ImplFile {
    let p = parse(src);
    assert!(
        p.errors.is_empty(),
        "parse errors in {src:?}: {:?}",
        p.errors
    );
    ImplFile::cast(p.root).expect("impl file")
}

/// How the probed name `X` is declared: a `let` value or an `exception` case.
/// A case lives in *both* namespaces (a value in expression position, a
/// constructor in pattern position); a value lives only in the value namespace.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Value,
    Case,
}

/// One way to place the probed name `X` in a single file under `namespace N`.
/// Each variant declares `X` (or its containing module) exactly once, so a
/// scenario built from placements with pairwise-distinct [`Placement::container`]s
/// is a valid F# program (no duplicate definition); the enumerator enforces that.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Placement {
    /// A case at the namespace's own direct tier (`exception X of int`) — always
    /// an `exception`, since a namespace holds no `let` values.
    Direct,
    /// `[<AutoOpen>] module A = <X>` — auto-opened; folds at this file.
    AoA,
    /// `module A = <X>` — a plain fragment; **not** auto-opened.
    PlainA,
    /// `[<AutoOpen>] module A = [<AutoOpen>] module C = <X>` — a nested auto-open
    /// under an auto-open parent; folds at this file.
    AoANest,
    /// `module A = [<AutoOpen>] module C = <X>` — a nested auto-open under a
    /// **plain** parent; the parent is not folded, so neither is the child.
    PlainANest,
    /// `[<AutoOpen>] module B = <X>` — a second auto-open submodule.
    AoB,
    /// `[<AutoOpen>] module A = exception Y of int` — a second `[<AutoOpen>]`
    /// fragment of `A` declaring a **different** name (`Y`, always a case). Used
    /// only in the explicit case corners to make `A` multi-fragment without a
    /// duplicate `X` (which an `exception X` in two `A` fragments would be); the
    /// enumerator never emits it.
    AoAFillerY,
}

impl Placement {
    /// The file body (below the `namespace` header) that declares `X`, with `X`
    /// itself a `let`/`exception` per `kind` (the `Direct` tier is always an
    /// `exception`).
    fn body(self, kind: Kind) -> String {
        // The `X` declaration at the given indent — a value or a case.
        let decl = |indent: &str| match kind {
            Kind::Value => format!("{indent}let X = 0\n"),
            Kind::Case => format!("{indent}exception X of int\n"),
        };
        match self {
            Placement::Direct => "exception X of int\n".to_string(),
            Placement::AoA => format!("[<AutoOpen>]\nmodule A =\n{}", decl("    ")),
            Placement::PlainA => format!("module A =\n{}", decl("    ")),
            Placement::AoANest => format!(
                "[<AutoOpen>]\nmodule A =\n    [<AutoOpen>]\n    module C =\n{}",
                decl("        ")
            ),
            Placement::PlainANest => format!(
                "module A =\n    [<AutoOpen>]\n    module C =\n{}",
                decl("        ")
            ),
            Placement::AoB => format!("[<AutoOpen>]\nmodule B =\n{}", decl("    ")),
            // Always declares `Y` (a case), regardless of `kind` — a filler that
            // makes `A` multi-fragment without redeclaring `X`.
            Placement::AoAFillerY => {
                "[<AutoOpen>]\nmodule A =\n    exception Y of int\n".to_string()
            }
        }
    }

    /// The qualified container that ends up owning `X` — the dedup key that keeps
    /// a generated scenario free of duplicate definitions (two placements sharing
    /// a container would define `X` twice there).
    fn container(self) -> &'static str {
        match self {
            Placement::Direct => "N",
            Placement::AoA | Placement::PlainA => "A",
            Placement::AoANest | Placement::PlainANest => "A.C",
            Placement::AoB => "B",
            // The filler owns `A.Y`, a different name than the probed `A.X`.
            Placement::AoAFillerY => "A.Y",
        }
    }

    fn tag(self) -> &'static str {
        match self {
            Placement::Direct => "direct",
            Placement::AoA => "aoA",
            Placement::PlainA => "plainA",
            Placement::AoANest => "aoA.C",
            Placement::PlainANest => "plainA.C",
            Placement::AoB => "aoB",
            Placement::AoAFillerY => "aoA:Y",
        }
    }
}

/// A generated scenario: an ordered list of `X`-placements plus the Compile-order
/// file each lands in, and a human label for diagnostics.
#[derive(Clone)]
struct Scenario {
    decls: Vec<Placement>,
    /// `groups[i]` is the Compile-order file index for `decls[i]`. Decls sharing
    /// an index are emitted in **one** file, in `decls` order (their source block
    /// order), under a single `namespace` header. The cross-file sweep uses the
    /// default `0..n` (one decl per file); the same-file sweep collapses indices
    /// to exercise the within-file fold — where the direct tier folds *before* the
    /// auto-open fragments regardless of block order, which used to defer.
    groups: Vec<usize>,
    label: String,
}

impl Scenario {
    /// A scenario with each placement in its own file (the cross-file sweep).
    fn cross_file(decls: Vec<Placement>, label: String) -> Scenario {
        let groups = (0..decls.len()).collect();
        Scenario {
            decls,
            groups,
            label,
        }
    }
}

/// A materialised scenario's files and the reference sites to probe in its user
/// file — `(site label, byte range)`: an `"expr"` site always, and a `"pat"` site
/// for a case sweep (where `X` is a constructor and `X _` is a case pattern, not a
/// binder).
struct Built {
    files: Vec<(String, String)>,
    user_local: usize,
    sites: Vec<(&'static str, TextRange)>,
}

fn one_char(off: usize) -> TextRange {
    TextRange::new(
        u32::try_from(off).unwrap().into(),
        u32::try_from(off + 1).unwrap().into(),
    )
}

/// Emit a scenario's files under a per-scenario namespace `Ns{gi}` (so scenarios
/// combined into one FCS invocation cannot interfere — distinct namespaces, and
/// each user opens only its own).
fn build(gi: usize, sc: &Scenario, kind: Kind) -> Built {
    let ns = format!("Ns{gi}");
    // One file per distinct group index. A file's body concatenates the bodies of
    // every placement in that group, in `decls` order (source block order), under
    // one `namespace {ns}` header — so a same-file group materialises the direct
    // tier and auto-open fragments as sibling blocks in one file.
    let num_files = sc.groups.iter().copied().max().map_or(0, |m| m + 1);
    let mut files: Vec<(String, String)> = (0..num_files)
        .map(|fi| {
            let body = sc
                .decls
                .iter()
                .zip(&sc.groups)
                .filter(|&(_, &g)| g == fi)
                .map(|(p, _)| p.body(kind))
                .collect::<Vec<_>>()
                .join("\n");
            (format!("g{gi}_{fi}"), format!("namespace {ns}\n\n{body}"))
        })
        .collect();
    // Expression reference always; a pattern reference whenever `X` is a
    // constructor *somewhere* in the scenario — the whole case sweep, and any
    // value-sweep scenario that includes the `Direct` exception. Probing the
    // pattern in that **mixed** value/case case is what forces value and
    // constructor lookup apart: the expression may bind a later `let X`, but the
    // pattern must bind the (case) exception, so a value masquerading as a
    // constructor is caught. A pattern `X _` where no case `X` exists would be an
    // invalid program (not a case pattern), so it is not emitted then.
    let probe_pattern = kind == Kind::Case || sc.decls.contains(&Placement::Direct);
    let mut user_src =
        format!("namespace User{gi}\n\nmodule U{gi} =\n    open {ns}\n    let ey = X\n");
    let expr_off = user_src.rfind('X').expect("expression `X`");
    let mut sites = vec![("expr", one_char(expr_off))];
    if probe_pattern {
        let before = user_src.len();
        user_src.push_str("    let pf v = match v with | X _ -> 0 | _ -> 1\n");
        let pat_off = before + user_src[before..].find('X').expect("pattern `X`");
        sites.push(("pat", one_char(pat_off)));
    }
    let user_local = files.len();
    files.push((format!("g{gi}_user"), user_src));
    Built {
        files,
        user_local,
        sites,
    }
}

/// A disagreement between our resolution and FCS at one probed reference.
struct Divergence {
    label: String,
    fcs: String,
    ours: String,
}

/// The FCS-resolved probe-site counts for a chunk — the vacuity guards. Split by
/// namespace so the pattern (constructor) sweep cannot pass *silently* if a range
/// bug ever stops the pattern site from lining up with FCS's use.
#[derive(Default)]
struct Resolved {
    expr: usize,
    pat: usize,
}

/// Resolve and FCS-diff one chunk of scenarios (combined into a single
/// `uses-project` invocation). Returns every divergence found and how many
/// expression / pattern sites FCS resolved.
fn diff_chunk(scenarios: &[Scenario], kind: Kind) -> (Vec<Divergence>, Resolved) {
    // Materialise every scenario's files, remembering each probe's global user
    // file index, its reference sites, and its label.
    let mut written: Vec<(PathBuf, String)> = Vec::new();
    struct Probe {
        label: String,
        user_global: usize,
        sites: Vec<(&'static str, TextRange)>,
    }
    let mut probes: Vec<Probe> = Vec::new();
    for (gi, sc) in scenarios.iter().enumerate() {
        let built = build(gi, sc, kind);
        let base = written.len();
        for (label, src) in &built.files {
            let path = temp_fs_file(label, src);
            written.push((path, src.clone()));
        }
        probes.push(Probe {
            label: sc.label.clone(),
            user_global: base + built.user_local,
            sites: built.sites,
        });
    }

    let paths: Vec<&Path> = written.iter().map(|(p, _)| p.as_path()).collect();
    let json = invoke_fcs_dump_project(&paths);
    let fcs = parse_fcs_uses_project(&json, &written);

    let asts: Vec<ImplFile> = written.iter().map(|(_, s)| impl_file(s)).collect();
    let proj = resolve_project(&asts, &AssemblyEnv::default());

    for (p, _) in &written {
        let _ = std::fs::remove_file(p);
    }

    let mut divergences = Vec::new();
    let mut resolved = Resolved::default();
    for probe in &probes {
        let user_path = &written[probe.user_global].0;
        for (site, range) in &probe.sites {
            let bump_resolved = |r: &mut Resolved| {
                if *site == "pat" {
                    r.pat += 1;
                } else {
                    r.expr += 1;
                }
            };
            // FCS's answer at this reference site: the resolved use there, if any.
            let fcs_decl = fcs
                .iter()
                .find(|f| f.path.file_name() == user_path.file_name())
                .and_then(|f| {
                    f.uses.iter().find(|u| {
                        u.start == usize::from(range.start()) && u.end == usize::from(range.end())
                    })
                })
                .and_then(|u| u.decl.as_ref());

            // Our answer, normalised to (declaring file name, def range).
            let ours = proj.file(probe.user_global).resolution_at(*range);
            let our_target: Option<(PathBuf, TextRange)> = match ours {
                Some(Resolution::Item(_)) => proj
                    .item_def(ours.unwrap())
                    .map(|(idx, def)| (written[idx].0.clone(), def.range)),
                Some(Resolution::Local(_)) => proj
                    .file(probe.user_global)
                    .resolved_def(ours.unwrap())
                    .map(|def| (user_path.clone(), def.range)),
                _ => None, // Deferred / Entity / Member / Unresolved / unrecorded
            };

            let label = format!("{}/{site}", probe.label);
            match (fcs_decl, our_target) {
                // FCS resolved to a project decl; we must agree or defer.
                (Some(decl), Some((our_file, our_range))) => {
                    bump_resolved(&mut resolved);
                    let agree = our_file.file_name() == decl.file.file_name()
                        && our_range
                            == TextRange::new(
                                u32::try_from(decl.start).unwrap().into(),
                                u32::try_from(decl.end).unwrap().into(),
                            );
                    if !agree {
                        divergences.push(Divergence {
                            label,
                            fcs: format!(
                                "{:?} @{}..{}",
                                decl.file.file_name(),
                                decl.start,
                                decl.end
                            ),
                            ours: format!("{:?} @{:?}", our_file.file_name(), our_range),
                        });
                    }
                }
                // FCS resolved but we deferred — allowed (availability).
                (Some(_), None) => bump_resolved(&mut resolved),
                // FCS left it unbound but we committed a target — a wrong target.
                (None, Some((our_file, our_range))) => divergences.push(Divergence {
                    label,
                    fcs: "unbound".to_string(),
                    ours: format!("{:?} @{:?}", our_file.file_name(), our_range),
                }),
                // Both decline — fine.
                (None, None) => {}
            }
        }
    }
    (divergences, resolved)
}

/// Every ordering (permutation) of `items`.
fn permutations<T: Clone>(items: &[T]) -> Vec<Vec<T>> {
    if items.len() <= 1 {
        return vec![items.to_vec()];
    }
    let mut out = Vec::new();
    for i in 0..items.len() {
        let mut rest = items.to_vec();
        let head = rest.remove(i);
        for mut tail in permutations(&rest) {
            tail.insert(0, head.clone());
            out.push(tail);
        }
    }
    out
}

/// Every combination of `k` placements with pairwise-distinct containers, each in
/// every Compile order — the enumerated straddle space. The index loops pick
/// strictly-increasing tuples (`a < b < c`), so the by-index form is exactly the
/// point (an iterator-combinator would be less clear).
#[allow(clippy::needless_range_loop)]
fn enumerate() -> Vec<Scenario> {
    const ALL: [Placement; 6] = [
        Placement::Direct,
        Placement::AoA,
        Placement::PlainA,
        Placement::AoANest,
        Placement::PlainANest,
        Placement::AoB,
    ];
    let mut scenarios = Vec::new();
    // Choose ordered index tuples of length 2 and 3; keep those whose containers
    // are pairwise distinct (valid, duplicate-free) and whose placement set is a
    // set (no repeated placement). Permuting is implicit in the ordered choice.
    let n = ALL.len();
    let mut push_combo = |combo: Vec<Placement>| {
        // pairwise-distinct containers
        let mut containers: Vec<&str> = combo.iter().map(|p| p.container()).collect();
        containers.sort_unstable();
        let distinct = containers.windows(2).all(|w| w[0] != w[1]);
        if !distinct {
            return;
        }
        for order in permutations(&combo) {
            let label = order.iter().map(|p| p.tag()).collect::<Vec<_>>().join(">");
            scenarios.push(Scenario::cross_file(order, label));
        }
    };
    for a in 0..n {
        for b in (a + 1)..n {
            push_combo(vec![ALL[a], ALL[b]]);
            for c in (b + 1)..n {
                push_combo(vec![ALL[a], ALL[b], ALL[c]]);
            }
        }
    }
    scenarios
}

/// The three FCS-grounded corners that motivated the fragment restructure (Stage
/// 5) — same-container `[<AutoOpen>]`+plain duplicates and multi-fragment modules
/// the enumerator excludes for validity, so they are pinned explicitly. Each was
/// a wrong target under the first (per-module-path) fragment attempt. Value-only:
/// FCS tolerates a `let X` redeclared across a module's fragments, but an
/// `exception X` redeclared there is a duplicate-type error, so the case sweep
/// omits them (its enumerated scenarios keep containers distinct).
fn explicit_corners() -> Vec<Scenario> {
    vec![
        // Corner 1: auto-open A.X@f1, plain A.X@f2, direct N.X@f0 → FCS binds A.X@f1.
        Scenario::cross_file(
            vec![Placement::Direct, Placement::AoA, Placement::PlainA],
            "corner1:direct>aoA>plainA".to_string(),
        ),
        // Corner 2: direct@f0, aoA.X@f1, aoB.X@f2, aoA(only Y — modelled here as a
        // second aoA fragment)@f3 → FCS binds B.X@f2. The duplicate aoA is what the
        // enumerator forbids; here the second aoA carries `X` again, still binding
        // B.X (latest file) — a stricter pin than the doc's `Y`-only fragment.
        Scenario::cross_file(
            vec![
                Placement::Direct,
                Placement::AoA,
                Placement::AoB,
                Placement::AoA,
            ],
            "corner2:direct>aoA>aoB>aoA".to_string(),
        ),
        // Corner 3: direct N.X@f0, auto-open A(no X)@f0 is folded into direct's
        // file; a nested auto-open C.X under a *plain* A@f1 → FCS binds N.X@f0.
        Scenario::cross_file(
            vec![Placement::Direct, Placement::PlainANest],
            "corner3:direct>plainA.C".to_string(),
        ),
    ]
}

/// Same-file straddle scenarios: the direct tier and one or more auto-open
/// fragments in **one file**, in every source block order — the fold path that
/// used to defer for lack of within-file provenance. FCS folds a file's direct
/// tier *before* its auto-open fragments regardless of block order (fcs-dump
/// probes A/B/D/E/F/G), so these must resolve, not defer: the expression binds
/// the latest-folded value (an auto-open, since it folds after the direct case),
/// while the pattern binds the direct case unless an auto-open also supplies a
/// case. Also includes a *split* grouping (a same-file tie in file0 plus a later
/// cross-file auto-open in file1) so the same-file and cross-file arms compose.
fn same_file_straddles() -> Vec<Scenario> {
    let tag_join =
        |order: &[Placement]| order.iter().map(|p| p.tag()).collect::<Vec<_>>().join(">");
    let mut out = Vec::new();
    // Direct + one auto-open placement, both block orders, both in file0.
    for ao in [Placement::AoA, Placement::AoANest, Placement::AoB] {
        for order in permutations(&[Placement::Direct, ao]) {
            out.push(Scenario {
                label: format!("sameFile:{}", tag_join(&order)),
                groups: vec![0; order.len()],
                decls: order,
            });
        }
    }
    // Direct + two auto-opens (A, B) in one file: the direct tier folds before
    // both; among the two auto-opens the later *block* wins.
    for order in permutations(&[Placement::Direct, Placement::AoA, Placement::AoB]) {
        out.push(Scenario {
            label: format!("sameFile3:{}", tag_join(&order)),
            groups: vec![0; order.len()],
            decls: order,
        });
    }
    // Split grouping: direct + AoA share file0 (a same-file tie), a later AoB is a
    // cross-file fragment in file1 — so the within-file "auto-open after direct"
    // rule and the cross-file "later file wins" rule must compose (AoB@f1 wins).
    for order in permutations(&[Placement::Direct, Placement::AoA]) {
        let label = format!("split:{}@f0,aoB@f1", tag_join(&order));
        let mut decls = order;
        decls.push(Placement::AoB);
        out.push(Scenario {
            decls,
            groups: vec![0, 0, 1],
            label,
        });
    }
    out
}

/// Case-sweep corners exercising **repeated-fragment** constructor provenance,
/// which the enumerator's distinct-container filter excludes and which a duplicate
/// `exception X` cannot express. `A` gets two `[<AutoOpen>]` fragments — one with
/// `X`, one with the `Y` filler — so `A` is multi-fragment without a duplicate.
/// If constructor lookup collapsed a module's fragments (or the fold replayed all
/// of `A`'s members at its later fragment), `A.X`@f0 would wrongly out-position
/// `B.X`@f1; the fragment-file-ordered fold binds the latest `X`, `B.X`.
fn case_corners() -> Vec<Scenario> {
    vec![
        // `[<AutoOpen>] module A = exception X`@f0, `[<AutoOpen>] module B = exception X`@f1,
        // `[<AutoOpen>] module A = exception Y`@f2 → `open N; X` binds B.X@f1.
        Scenario::cross_file(
            vec![Placement::AoA, Placement::AoB, Placement::AoAFillerY],
            "caseCorner:aoA(X)>aoB(X)>aoA(Y)".to_string(),
        ),
        // Order swapped: the multi-fragment `A` is split around `B`. `A.X`@f0,
        // `A.Y`@f1 (filler), `B.X`@f2 → binds B.X@f2 (latest `X`).
        Scenario::cross_file(
            vec![Placement::AoA, Placement::AoAFillerY, Placement::AoB],
            "caseCorner:aoA(X)>aoA(Y)>aoB(X)".to_string(),
        ),
    ]
}

/// The full straddle sweep, over both namespaces: the value sweep (enumerated
/// distinct-container scenarios + explicit corners, expression probe — plus a
/// pattern probe on the mixed value/case scenarios) and the case sweep
/// (enumerated scenarios + repeated-fragment case corners, with `X` an
/// `exception` everywhere, expression *and* pattern probes), each diffed against
/// FCS in chunks.
#[test]
fn straddle_resolution_agrees_with_fcs() {
    let mut all_divergences: Vec<Divergence> = Vec::new();
    let mut resolved = Resolved::default();

    // Chunk so each FCS invocation stays a modest Compile order; distinct
    // per-scenario namespaces make the combination sound.
    let mut value_scenarios = enumerate();
    value_scenarios.extend(explicit_corners());
    value_scenarios.extend(same_file_straddles());
    for chunk in value_scenarios.chunks(24) {
        let (divs, r) = diff_chunk(chunk, Kind::Value);
        all_divergences.extend(divs);
        resolved.expr += r.expr;
        resolved.pat += r.pat;
    }
    let mut case_scenarios = enumerate();
    case_scenarios.extend(case_corners());
    case_scenarios.extend(same_file_straddles());
    for chunk in case_scenarios.chunks(24) {
        let (divs, r) = diff_chunk(chunk, Kind::Case);
        all_divergences.extend(divs);
        resolved.expr += r.expr;
        resolved.pat += r.pat;
    }

    // Non-vacuity, split by namespace: the case sweep must genuinely resolve
    // pattern sites (else a range bug could let the constructor sweep pass
    // without checking anything).
    assert!(
        resolved.expr > 0,
        "vacuous: FCS resolved no expression `X` across the sweep"
    );
    assert!(
        resolved.pat > 0,
        "vacuous: FCS resolved no pattern `X` — the constructor namespace was not exercised"
    );
    assert!(
        all_divergences.is_empty(),
        "{} straddle divergence(s) vs FCS:\n{}",
        all_divergences.len(),
        all_divergences
            .iter()
            .map(|d| format!("  [{}] FCS {} | we gave {}", d.label, d.fcs, d.ours))
            .collect::<Vec<_>>()
            .join("\n")
    );
}
