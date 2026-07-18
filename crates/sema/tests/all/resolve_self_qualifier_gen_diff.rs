//! Generative **module self-qualifier** differential vs FCS, over the real
//! FSharp.Core.
//!
//! A module that references its *own* name as a qualifier (`List.fold` inside
//! `module List`) is FS0039 for a non-recursive module — the name falls through
//! to the auto-opened `Microsoft.FSharp.Collections.List` — *unless* some
//! **non-self** project entity named `List` supplies the tail, in which case FCS
//! binds that, per member. Getting the relaxation sound was a codex whack-a-mole
//! (cross-file member, project type, same-file child, `module rec`, an outward
//! cousin — one review round each), the same "curated tests find one corner at a
//! time" trap `resolve_qualified_path_access_gen_diff.rs` closes for the sibling
//! path. This harness closes it here: it *enumerates* the ways a non-self `List`
//! can (or cannot) provide the tail and diffs FCS's resolution of the
//! self-qualified head against ours.
//!
//! Dimensions swept (the `Provider` of a non-self `List`, × the probed tail):
//! - **Leaf** — no other `List`; both tails fall to FSharp.Core (the WoofWare case);
//! - **child** module value / **child** type — a `module List` / `type`-owning
//!   child inside the self module;
//! - **cousin** — an outward `module List` in an enclosing scope;
//! - **companion** — a `type List` beside the module (FSharp.Core's own shape);
//! - **cross-file fragment** — an earlier file's `module N.List`, merged;
//! - **recursive** — `module rec List`, where the own name *is* in scope.
//!
//! Each provider owns a member `rev` (a name FSharp.Core *also* supplies — the
//! collision that makes a wrong commit visible) and lacks `length` (a name only
//! FSharp.Core supplies — the per-member fall-through). Probing `List.rev` and
//! `List.length` at each provider therefore exercises both directions.
//!
//! **The head is the probe.** FCS reports the head `List`'s binding — a project
//! decl (a non-self `List`), FSharp.Core, or unbound. We read our head resolution
//! (`Item`/`Local` = project, `Entity`/`Member` = a referenced assembly = here
//! FSharp.Core, else declined) and assert **certain-implies-exact**:
//! - FCS bound a **project** `List` ⇒ we must bind the **same** project decl or
//!   decline — a FSharp.Core `Entity` here is the wrong go-to-def the whole audit
//!   is about;
//! - FCS bound **FSharp.Core** ⇒ we must bind FSharp.Core or decline — a project
//!   target here is wrong;
//! - FCS left it **unbound** ⇒ we must decline.
//!
//! Declining is always the fail-safe (an availability loss, not a soundness bug);
//! a wrong or divergent target fails.

use std::path::{Path, PathBuf};

use crate::common::{
    ensure_fsharp_core_dll, invoke_fcs_dump_project, parse_fcs_uses_project, temp_fs_file,
};
use borzoi_assembly::Ecma335Assembly;
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

/// The real, shipped FSharp.Core as an [`AssemblyEnv`], parsed once.
fn fsharp_core_env() -> AssemblyEnv {
    let dll = ensure_fsharp_core_dll();
    let bytes = std::fs::read(&dll).unwrap_or_else(|e| panic!("read {dll:?}: {e}"));
    let view = Ecma335Assembly::parse(&bytes).expect("parse FSharp.Core.dll");
    AssemblyEnv::from_views(std::slice::from_ref(&view)).expect("FSharp.Core env")
}

/// How a **non-self** `List` (which the self-qualified head may bind ahead of the
/// current module) is provided, if at all. Each provider owns `rev` and lacks
/// `length`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Provider {
    Leaf,
    AncestorHelper,
    ChildValue,
    ChildType,
    Cousin,
    CousinType,
    DeepUncle,
    Companion,
    CrossFile,
    Recursive,
}

impl Provider {
    fn tag(self) -> &'static str {
        match self {
            Provider::Leaf => "leaf",
            Provider::AncestorHelper => "ancestor",
            Provider::ChildValue => "childVal",
            Provider::ChildType => "childType",
            Provider::Cousin => "cousin",
            Provider::CousinType => "cousinType",
            Provider::DeepUncle => "deepUncle",
            Provider::Companion => "companion",
            Provider::CrossFile => "crossFile",
            Provider::Recursive => "rec",
        }
    }

    /// A **pure self-qualifier** — no non-self `List` exists, so FCS binds *both*
    /// tails to FSharp.Core and we must resolve them there too (declining would be
    /// the availability regression the fix exists to remove). The providers that
    /// *do* introduce a non-self `List` may legitimately decline (a conservative
    /// deferral), so this gates the availability assertion to the leaf/ancestor
    /// shapes only.
    fn pure_self(self) -> bool {
        matches!(self, Provider::Leaf | Provider::AncestorHelper)
    }

    /// The files (`(label, source)`), and the index of the
    /// file holding the two `List.rev` / `List.length` references. `gi` keeps the
    /// per-scenario module/namespace name distinct so scenarios combine in one FCS
    /// invocation without colliding.
    fn program(self, gi: usize) -> (Vec<(String, String)>, usize) {
        // The two references live in the self `module List`'s body.
        let refs = "    let a = List.rev\n    let b = List.length [ 1 ]\n";
        // `rev` invoked as a call/value per how the provider declares it; a bare
        // `List.rev` reference is enough to bind the head either way.
        match self {
            Provider::Leaf => (
                vec![(
                    format!("sq{gi}"),
                    format!("namespace N{gi}\n\nmodule List =\n{refs}"),
                )],
                0,
            ),
            // `List` is an ANCESTOR (the enclosing module), still a self-qualifier:
            // FS0039, so `List.rev`/`List.length` both fall through to FSharp.Core.
            Provider::AncestorHelper => (
                vec![(
                    format!("sq{gi}"),
                    format!("namespace N{gi}\n\nmodule List =\n    module Helpers =\n    {refs}"),
                )],
                0,
            ),
            Provider::ChildValue => (
                vec![(
                    format!("sq{gi}"),
                    format!(
                        "namespace N{gi}\n\nmodule List =\n    module List =\n        let rev = 0\n{refs}"
                    ),
                )],
                0,
            ),
            Provider::ChildType => (
                vec![(
                    format!("sq{gi}"),
                    format!(
                        "namespace N{gi}\n\nmodule List =\n    module List =\n        type rev() = class end\n{refs}"
                    ),
                )],
                0,
            ),
            Provider::Cousin => (
                vec![(
                    format!("sq{gi}"),
                    format!(
                        "module Root{gi}\n\nmodule List =\n    let rev = 0\n\nmodule Outer =\n    module List =\n    {refs}"
                    ),
                )],
                0,
            ),
            Provider::CousinType => (
                vec![(
                    format!("sq{gi}"),
                    format!(
                        "module Root{gi}\n\nmodule List =\n    type rev() = class end\n\nmodule Outer =\n    module List =\n    {refs}"
                    ),
                )],
                0,
            ),
            Provider::DeepUncle => (
                vec![(
                    format!("sq{gi}"),
                    format!(
                        "module Root{gi}\n\nmodule List =\n    let rev = 0\n\nmodule A =\n    module B =\n        module List =\n        {refs}"
                    ),
                )],
                0,
            ),
            Provider::Companion => (
                vec![(
                    format!("sq{gi}"),
                    format!(
                        "namespace N{gi}\n\ntype List =\n    static member rev = 0\n\nmodule List =\n{refs}"
                    ),
                )],
                0,
            ),
            Provider::CrossFile => (
                vec![
                    (
                        format!("sq{gi}_0"),
                        format!("namespace N{gi}\n\nmodule List =\n    let rev = 0\n"),
                    ),
                    (
                        format!("sq{gi}_1"),
                        format!("namespace N{gi}\n\nmodule List =\n{refs}"),
                    ),
                ],
                1,
            ),
            Provider::Recursive => (
                vec![(
                    format!("sq{gi}"),
                    format!("namespace N{gi}\n\nmodule rec List =\n    let rev = 0\n{refs}"),
                )],
                0,
            ),
        }
    }
}

/// The head-`List` byte range of the `n`th occurrence of `needle` (`"List.rev"` /
/// `"List.length"`) in `src`.
fn head_range(src: &str, needle: &str) -> TextRange {
    let off = src
        .find(needle)
        .unwrap_or_else(|| panic!("{needle:?} in {src:?}"));
    TextRange::new(
        u32::try_from(off).unwrap().into(),
        u32::try_from(off + "List".len()).unwrap().into(),
    )
}

/// What FCS bound the head to.
#[derive(Debug)]
enum FcsHead {
    Project {
        file: PathBuf,
        start: usize,
        end: usize,
    },
    FsharpCore,
    Unbound,
}

/// What we bound the head to.
#[derive(Debug)]
enum OurHead {
    Project { file: PathBuf, range: TextRange },
    FsharpCore,
    Decline,
}

struct Divergence {
    label: String,
    fcs: String,
    ours: String,
}

#[derive(Default)]
struct Tally {
    fcs_project: usize,
    fcs_fsharp_core: usize,
    our_fsharp_core_agreed: usize,
}

#[test]
fn self_qualifier_head_agrees_with_fcs() {
    let env = fsharp_core_env();

    const PROVIDERS: [Provider; 10] = [
        Provider::Leaf,
        Provider::AncestorHelper,
        Provider::ChildValue,
        Provider::ChildType,
        Provider::Cousin,
        Provider::CousinType,
        Provider::DeepUncle,
        Provider::Companion,
        Provider::CrossFile,
        Provider::Recursive,
    ];

    // Materialise every scenario's files into one FCS invocation (per-scenario
    // module/namespace names keep them from colliding).
    struct Probe {
        label: String,
        ref_global: usize,
        pure_self: bool,
        heads: Vec<(&'static str, TextRange)>, // (needle, head range)
    }
    let mut written: Vec<(PathBuf, String)> = Vec::new();
    let mut probes: Vec<Probe> = Vec::new();
    for (gi, provider) in PROVIDERS.iter().enumerate() {
        let (files, ref_local) = provider.program(gi);
        let base = written.len();
        let ref_src = files[ref_local].1.clone();
        for (name, src) in &files {
            written.push((temp_fs_file(name, src), src.clone()));
        }
        probes.push(Probe {
            label: provider.tag().to_string(),
            ref_global: base + ref_local,
            pure_self: provider.pure_self(),
            heads: vec![
                ("List.rev", head_range(&ref_src, "List.rev")),
                ("List.length", head_range(&ref_src, "List.length")),
            ],
        });
    }

    let paths: Vec<&Path> = written.iter().map(|(p, _)| p.as_path()).collect();
    let json = invoke_fcs_dump_project(&paths);
    let fcs = parse_fcs_uses_project(&json, &written);

    let asts: Vec<ImplFile> = written.iter().map(|(_, s)| impl_file(s)).collect();
    let proj = resolve_project(&asts, &env);

    for (p, _) in &written {
        let _ = std::fs::remove_file(p);
    }

    let mut divergences: Vec<Divergence> = Vec::new();
    let mut tally = Tally::default();

    for probe in &probes {
        let ref_path = &written[probe.ref_global].0;
        let fcs_file = fcs
            .iter()
            .find(|f| f.path.file_name() == ref_path.file_name());
        for (needle, head) in &probe.heads {
            let label = format!("{}/{needle}", probe.label);
            let start = usize::from(head.start());
            let end = usize::from(head.end());

            // FCS's head binding: a project decl, FSharp.Core, or unbound.
            let fcs_use = fcs_file.and_then(|f| {
                f.uses
                    .iter()
                    .find(|u| u.start == start && u.end == end && !u.is_from_definition)
            });
            let fcs_head = match fcs_use {
                Some(u) if u.decl.is_some() => {
                    let d = u.decl.as_ref().unwrap();
                    FcsHead::Project {
                        file: d.file.clone(),
                        start: d.start,
                        end: d.end,
                    }
                }
                Some(u) if u.assembly.as_deref() == Some("FSharp.Core") => FcsHead::FsharpCore,
                // A resolved use into some other assembly is out of scope; treat as
                // FSharp.Core-side (we only ever commit FSharp.Core here).
                Some(_) => FcsHead::FsharpCore,
                None => FcsHead::Unbound,
            };

            // Our head binding.
            let ours = proj.file(probe.ref_global).resolution_at(*head);
            let our_head = match ours {
                Some(Resolution::Item(_)) => proj
                    .item_def(ours.unwrap())
                    .map(|(idx, def)| OurHead::Project {
                        file: written[idx].0.clone(),
                        range: def.range,
                    })
                    .unwrap_or(OurHead::Decline),
                Some(Resolution::Local(_)) => proj
                    .file(probe.ref_global)
                    .resolved_def(ours.unwrap())
                    .map(|def| OurHead::Project {
                        file: ref_path.clone(),
                        range: def.range,
                    })
                    .unwrap_or(OurHead::Decline),
                Some(Resolution::Entity(_)) | Some(Resolution::Member { .. }) => {
                    OurHead::FsharpCore
                }
                _ => OurHead::Decline,
            };

            match (&fcs_head, &our_head) {
                (FcsHead::Project { file, start, end }, _) => {
                    tally.fcs_project += 1;
                    match &our_head {
                        OurHead::Decline => {}
                        OurHead::FsharpCore => divergences.push(Divergence {
                            label,
                            fcs: format!("project {:?} @{start}..{end}", file.file_name()),
                            ours: "FSharp.Core".to_string(),
                        }),
                        OurHead::Project { file: of, range } => {
                            let agree = of.file_name() == file.file_name()
                                && usize::from(range.start()) == *start
                                && usize::from(range.end()) == *end;
                            if !agree {
                                divergences.push(Divergence {
                                    label,
                                    fcs: format!("project {:?} @{start}..{end}", file.file_name()),
                                    ours: format!("project {:?} @{range:?}", of.file_name()),
                                });
                            }
                        }
                    }
                }
                (FcsHead::FsharpCore, _) => {
                    tally.fcs_fsharp_core += 1;
                    match &our_head {
                        OurHead::FsharpCore => tally.our_fsharp_core_agreed += 1,
                        // A pure self-qualifier (no non-self `List`) *must* resolve
                        // to FSharp.Core — declining is the availability regression
                        // the fix removes (the ancestor `List.rev` over-defer). Other
                        // providers may conservatively decline.
                        OurHead::Decline if probe.pure_self => divergences.push(Divergence {
                            label,
                            fcs: "FSharp.Core".to_string(),
                            ours: "declined (should resolve FSharp.Core)".to_string(),
                        }),
                        OurHead::Decline => {}
                        OurHead::Project { file, range } => divergences.push(Divergence {
                            label,
                            fcs: "FSharp.Core".to_string(),
                            ours: format!("project {:?} @{range:?}", file.file_name()),
                        }),
                    }
                }
                (FcsHead::Unbound, OurHead::Decline) => {}
                (FcsHead::Unbound, _) => divergences.push(Divergence {
                    label,
                    fcs: "unbound".to_string(),
                    ours: format!("{our_head:?}"),
                }),
            }
        }
    }

    // Non-vacuity: the sweep must exercise both a project-bound self-qualifier (the
    // wrong-commit direction) and a FSharp.Core fall-through we actually resolve
    // (the availability the fix delivers).
    assert!(
        tally.fcs_project > 0,
        "vacuous: FCS bound no self-qualified head to a project `List`"
    );
    assert!(
        tally.fcs_fsharp_core > 0,
        "vacuous: FCS bound no self-qualified head to FSharp.Core"
    );
    assert!(
        tally.our_fsharp_core_agreed > 0,
        "vacuous: we resolved no self-qualified head to FSharp.Core (the WoofWare win)"
    );

    assert!(
        divergences.is_empty(),
        "{} self-qualifier head divergence(s) vs FCS \
         (fcs_project={}, fcs_fsharp_core={}, we_agreed_fsharp_core={}):\n{}",
        divergences.len(),
        tally.fcs_project,
        tally.fcs_fsharp_core,
        tally.our_fsharp_core_agreed,
        divergences
            .iter()
            .map(|d| format!("  [{}] FCS {} | we gave {}", d.label, d.fcs, d.ours))
            .collect::<Vec<_>>()
            .join("\n")
    );
}
