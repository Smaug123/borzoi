//! FCS attribute-resolution oracle — stage-1 pinning tests.
//!
//! EX-3 §2(d) (`docs/extension-scope-enumeration-plan.md`) needs the resolver
//! to answer "which type does this written attribute resolve to?" through its
//! own type-path walk, gated by a differential against FCS. This module hosts
//! that differential; at this stage it *pins the oracle itself* — the
//! `fcs-dump attrs` op — before anything depends on it:
//!
//! - FCS resolves an attribute via `ResolveAttributeType`
//!   (`CheckExpressions.fs`): the written last segment with the `Attribute`
//!   suffix appended is tried *first*, then the name as written, through the
//!   general `ResolveTypeLongIdent` — so opens and type abbreviations are
//!   honoured, and `[<Literal>]` resolves to `LiteralAttribute`.
//! - The resolution is recorded to the sink with
//!   `ItemOccurrence.UseInAttribute` at the written name's source range, and
//!   surfaced by `GetAllUsesOfAllSymbolsInFile` as an `FSharpEntity`. The op
//!   filters to exactly those records (each attribute also records its *ctor*
//!   at the last segment as a plain `Use`; that must not appear here — every
//!   "exactly one record" assertion below is pinning that filter).
//! - An attribute FCS cannot resolve produces *no* record (decline by
//!   absence) plus a check error — the shape the differential's
//!   certain-implies-exact property needs.
//! - The occurrence alone does not identify an attribute: `TcNameOfExpr`
//!   resolves a type argument of `nameof` with the same
//!   `ItemOccurrence.UseInAttribute`, so the op intersects the sink records
//!   with the parse tree's syntactic attribute-name ranges.
//! - `TargetFullName`/`TargetAssembly` chase an abbreviation chain to the
//!   terminal entity: `type MyExt = ExtensionAttribute` reports both the
//!   abbreviation it resolved to and the `ExtensionAttribute` it stands for.
//!   The gate consumer (stage 5) keys on the terminal. `None` means the chase
//!   found no terminal (an opaque or over-long chain) — unknowable, never
//!   "not of interest".

use crate::common::{
    AttrsOracle, ensure_fsharp_core_dll, invoke_fcs_dump, parse_fcs_attrs, temp_fs_file,
};

use borzoi_assembly::{Ecma335Assembly, EcmaView};
use borzoi_cst::parser::parse;
use borzoi_cst::syntax::{AstNode, ImplFile};
use borzoi_sema::{AssemblyEnv, ProjectItems, Resolution, ResolvedFile, resolve_file};
use rowan::TextRange;

/// Dump and normalise the attribute resolutions FCS records for `src`.
fn attrs_of(label: &str, src: &str) -> AttrsOracle {
    let path = temp_fs_file(label, src);
    let json = invoke_fcs_dump("attrs", &path);
    let _ = std::fs::remove_file(&path);
    parse_fcs_attrs(&json, src)
}

/// Byte range of the first occurrence of `needle` in `src`.
fn span_of(src: &str, needle: &str) -> (usize, usize) {
    let start = src.find(needle).expect("needle in src");
    (start, start + needle.len())
}

#[test]
fn bare_name_resolves_via_the_attribute_suffix() {
    let src = "module Test\n\n[<Literal>]\nlet X = 5\n";
    let oracle = attrs_of("attr_literal", src);
    assert_eq!(oracle.errors, vec![], "clean check expected");
    // Exactly one record: the ctor `Use` FCS also sinks at the same span must
    // have been filtered out.
    assert_eq!(oracle.attrs.len(), 1, "attrs: {:#?}", oracle.attrs);
    let a = &oracle.attrs[0];
    // The record sits on the *written* token, while the resolved entity is the
    // suffixed `LiteralAttribute` — the suffix-first probe of the oracle.
    assert_eq!((a.start, a.end), span_of(src, "Literal"));
    assert_eq!(
        a.full_name.as_deref(),
        Some("Microsoft.FSharp.Core.LiteralAttribute")
    );
    assert_eq!(a.assembly.as_deref(), Some("FSharp.Core"));
    // Not an abbreviation: the terminal type is the resolved type itself.
    assert_eq!(a.target_full_name, a.full_name);
    assert_eq!(a.target_assembly, a.assembly);
}

#[test]
fn written_suffix_form_resolves_identically() {
    let src = "module Test\n\n[<LiteralAttribute>]\nlet X = 5\n";
    let oracle = attrs_of("attr_literal_suffix", src);
    assert_eq!(oracle.errors, vec![], "clean check expected");
    assert_eq!(oracle.attrs.len(), 1, "attrs: {:#?}", oracle.attrs);
    let a = &oracle.attrs[0];
    assert_eq!((a.start, a.end), span_of(src, "LiteralAttribute"));
    assert_eq!(
        a.full_name.as_deref(),
        Some("Microsoft.FSharp.Core.LiteralAttribute")
    );
}

#[test]
fn qualified_attribute_spans_the_whole_path() {
    let src = "module Test\n\n[<Microsoft.FSharp.Core.Literal>]\nlet X = 5\n";
    let oracle = attrs_of("attr_qualified", src);
    assert_eq!(oracle.errors, vec![], "clean check expected");
    assert_eq!(oracle.attrs.len(), 1, "attrs: {:#?}", oracle.attrs);
    let a = &oracle.attrs[0];
    assert_eq!(
        (a.start, a.end),
        span_of(src, "Microsoft.FSharp.Core.Literal")
    );
    assert_eq!(
        a.full_name.as_deref(),
        Some("Microsoft.FSharp.Core.LiteralAttribute")
    );
}

#[test]
fn qualified_bcl_attribute_with_arguments() {
    let src = "module Test\n\n[<System.Obsolete(\"gone\")>]\nlet f () = 1\n";
    let oracle = attrs_of("attr_obsolete", src);
    assert_eq!(oracle.errors, vec![], "clean check expected");
    assert_eq!(oracle.attrs.len(), 1, "attrs: {:#?}", oracle.attrs);
    let a = &oracle.attrs[0];
    assert_eq!((a.start, a.end), span_of(src, "System.Obsolete"));
    assert_eq!(a.full_name.as_deref(), Some("System.ObsoleteAttribute"));
}

/// The load-bearing case for the extension gate: an abbreviation of
/// `ExtensionAttribute`, applied at both type and member level. The oracle
/// must surface the *terminal* type so a consumer can recognise the alias, and
/// must report member-level attributes (a `[<Extension>]` static member is an
/// extension with no container attribute under
/// `CSharpExtensionAttributeNotRequired`).
#[test]
fn abbreviation_of_extension_attribute_reports_the_terminal_type() {
    let src = "module Test\n\nopen System.Runtime.CompilerServices\n\ntype MyExt = ExtensionAttribute\n\n[<MyExt>]\ntype Helpers =\n    [<MyExt>]\n    static member Twice (s: string) = s + s\n";
    let oracle = attrs_of("attr_alias", src);
    assert_eq!(oracle.errors, vec![], "clean check expected");
    // One record per written `[<MyExt>]` (type-level and member-level).
    assert_eq!(oracle.attrs.len(), 2, "attrs: {:#?}", oracle.attrs);
    for a in &oracle.attrs {
        assert_eq!(&src[a.start..a.end], "MyExt");
        assert_eq!(
            a.target_full_name.as_deref(),
            Some("System.Runtime.CompilerServices.ExtensionAttribute"),
            "terminal type after chasing the abbreviation: {a:#?}"
        );
    }
}

/// An attribute on a **type parameter** (`type R<[<Literal>] 'T>`) makes FCS
/// sink two DISTINCT entities at one range — the built-in special attribute
/// and the local type. The oracle must represent that as an `Ambiguous`
/// no-claim record, not abort the dump (codex on stage 4).
#[test]
fn typar_special_attribute_is_ambiguous_not_fatal() {
    let src = "module Test\n\ntype LiteralAttribute() =\n    inherit System.Attribute()\n\ntype R<[<Literal>] 'T> = { x: 'T }\n";
    let oracle = attrs_of("attr_typar", src);
    let lit = oracle
        .attrs
        .iter()
        .find(|a| {
            let token = &src[a.start..a.end];
            token == "Literal"
        })
        .expect("a record at the typar attribute");
    assert!(
        lit.ambiguous,
        "distinct entities at one range must surface as Ambiguous: {lit:#?}"
    );
    assert_eq!(lit.full_name, None, "an ambiguous record names no target");
}

/// `nameof (T)` resolves its type argument with the *same*
/// `ItemOccurrence.UseInAttribute` occurrence that real attributes get
/// (`TcNameOfExpr`), so the op must not report it — neither standalone nor as
/// an attribute argument. Only the syntactic attribute name may appear.
#[test]
fn nameof_type_argument_is_not_an_attribute() {
    let src = "module Test\n\n[<System.Obsolete(nameof (System.Int32))>]\nlet f () = 1\n\nlet n = nameof (System.String)\n";
    let oracle = attrs_of("attr_nameof", src);
    assert_eq!(oracle.errors, vec![], "clean check expected");
    assert_eq!(oracle.attrs.len(), 1, "attrs: {:#?}", oracle.attrs);
    let a = &oracle.attrs[0];
    assert_eq!((a.start, a.end), span_of(src, "System.Obsolete"));
    assert_eq!(a.full_name.as_deref(), Some("System.ObsoleteAttribute"));
}

/// A deep (but legal) abbreviation chain still chases to the terminal — the
/// fuel bound declines (`None`) rather than reporting an intermediate
/// abbreviation, and must be far above anything real code writes.
#[test]
fn deep_abbreviation_chain_chases_to_the_terminal() {
    let mut src = String::from(
        "module Test\n\nopen System.Runtime.CompilerServices\n\ntype A0 = ExtensionAttribute\n",
    );
    for i in 1..=40 {
        src.push_str(&format!("type A{i} = A{}\n", i - 1));
    }
    src.push_str("\n[<A40>]\ntype Helpers =\n    static member Twice (s: string) = s + s\n");
    let oracle = attrs_of("attr_deep_chain", &src);
    assert_eq!(oracle.errors, vec![], "clean check expected");
    assert_eq!(oracle.attrs.len(), 1, "attrs: {:#?}", oracle.attrs);
    assert_eq!(
        oracle.attrs[0].target_full_name.as_deref(),
        Some("System.Runtime.CompilerServices.ExtensionAttribute")
    );
}

#[test]
fn unresolvable_attribute_declines_with_an_error() {
    let src = "module Test\n\n[<ThisAttributeDoesNotExist>]\nlet x = 5\n";
    let oracle = attrs_of("attr_unresolved", src);
    assert_eq!(
        oracle.attrs,
        vec![],
        "an unresolved attribute must sink no record"
    );
    assert!(
        !oracle.errors.is_empty(),
        "FCS reports the failed resolution as a check error"
    );
}

// ============================================================================
// The differential proper (stage 3): our resolver's attribute-type
// resolutions vs FCS — certain-implies-exact, with per-case commit floors.
// ============================================================================

/// An [`AssemblyEnv`] over the real, shipped FSharp.Core, so bare BCL-less
/// snippets (`[<Literal>]`) resolve on our side the way FCS's SDK references
/// resolve them on its side.
pub(crate) fn fsharp_core_env() -> AssemblyEnv {
    let dll = ensure_fsharp_core_dll();
    let bytes = std::fs::read(&dll).unwrap_or_else(|e| panic!("read {dll:?}: {e}"));
    let view = Ecma335Assembly::parse(&bytes).expect("parse FSharp.Core.dll");
    AssemblyEnv::from_views(std::slice::from_ref(&view))
        .expect("FSharp.Core must project end-to-end into an AssemblyEnv")
}

pub(crate) fn resolve(src: &str, env: &AssemblyEnv) -> ResolvedFile {
    let p = parse(src);
    assert!(
        p.errors.is_empty(),
        "parse errors in {src:?}: {:?}",
        p.errors
    );
    let file = ImplFile::cast(p.root).expect("impl file");
    resolve_file(&file, &ProjectItems::default(), env)
}

/// The `(assembly simple name, dotted full name)` an [`Resolution::Entity`]
/// commit names — the FCS matching currency. The full name comes from
/// [`AssemblyEnv::entity_full_name`], which carries enclosing entity names for
/// a nested type and the source name for a `[<CompiledName>]`-renamed one —
/// hand-joining `namespace + name` would reject correct commits on exactly
/// those shapes and leave them ungated.
fn entity_full(env: &AssemblyEnv, res: Resolution) -> (String, String) {
    let Resolution::Entity(h) = res else {
        unreachable!("only Entity reaches here")
    };
    (env.entity(h).assembly.name.clone(), env.entity_full_name(h))
}

/// The certain-implies-exact differential over one snippet:
///
/// - **FCS→ours**: for every attribute FCS resolved, our verdict at the same
///   written range is a decline (`None` / `Deferred` — no claim), or an exact
///   agreement: an [`Resolution::Entity`] naming FCS's
///   `(assembly, full name)`, or a [`Resolution::Local`] whose binder range
///   is FCS's in-file declaration site.
/// - **ours→FCS**: every commit *we* made sits on a range FCS reported an
///   attribute at (we never invent one, e.g. from a `nameof`).
/// - **floor**: exactly `expected_commits` agreements, so refinements cannot
///   silently decay into wholesale deferral.
///
/// FCS check errors fail the harness (these cases are meant to be clean) —
/// except `allow_errors`, for pinning decline-on-unresolvable.
fn assert_attrs_match_fcs_with(
    src: &str,
    env: &AssemblyEnv,
    expected_commits: usize,
    allow_errors: bool,
) -> ResolvedFile {
    let rf = resolve(src, env);
    let path = temp_fs_file("attr_res_diff", src);
    let json = invoke_fcs_dump("attrs", &path);
    let _ = std::fs::remove_file(&path);
    let oracle = parse_fcs_attrs(&json, src);
    if !allow_errors {
        assert_eq!(oracle.errors, vec![], "FCS check must be clean for {src:?}");
    }

    let commits = check_attrs_agree(src, env, &rf, &oracle, true);

    assert_eq!(
        commits, expected_commits,
        "agreement floor for {src:?}: {commits} commits, expected {expected_commits}"
    );
    rf
}

/// The core certain-implies-exact comparator, shared with the generative and
/// corpus sweeps (`attr_resolution_sweep`): panics on any disagreement,
/// returns the agreement (commit) count.
///
/// `check_reverse` additionally asserts every commit *we* made sits on a
/// range FCS reported an attribute at. Callers pass `false` when FCS's check
/// errored — an aborted or erroring check can under-report the sink without
/// implicating us.
pub(crate) fn check_attrs_agree(
    src: &str,
    env: &AssemblyEnv,
    rf: &ResolvedFile,
    oracle: &AttrsOracle,
    check_reverse: bool,
) -> usize {
    let mut commits = 0usize;
    for a in &oracle.attrs {
        // An ambiguous record (FCS sank distinct entities at the range — the
        // typar special-attribute shape) makes no claim either way.
        if a.ambiguous {
            continue;
        }
        let span = TextRange::new(
            u32::try_from(a.start).unwrap().into(),
            u32::try_from(a.end).unwrap().into(),
        );
        match rf.attribute_resolution_at(span) {
            // An honest decline: no claim to check.
            None | Some(Resolution::Deferred(_)) => {}
            Some(res @ Resolution::Entity(_)) => {
                let (asm, full) = entity_full(env, res);
                assert_eq!(
                    (Some(asm.as_str()), Some(full.as_str())),
                    (a.assembly.as_deref(), a.full_name.as_deref()),
                    "attribute at {span:?} in {src:?}: we committed a different type"
                );
                commits += 1;
            }
            Some(Resolution::Local(id)) => {
                let def = rf.def(id);
                let our_decl = (usize::from(def.range.start()), usize::from(def.range.end()));
                assert_eq!(
                    Some(our_decl),
                    a.decl,
                    "attribute at {span:?} in {src:?}: we committed a different in-file binder"
                );
                commits += 1;
            }
            Some(other) => {
                panic!("attribute at {span:?} in {src:?}: impossible verdict {other:?}")
            }
        }
    }

    // ours→FCS: a commit where FCS reported no attribute is an invention.
    if check_reverse {
        for (range, res) in rf.attribute_resolutions() {
            if matches!(res, Resolution::Entity(_) | Resolution::Local(_)) {
                let (s, e) = (usize::from(range.start()), usize::from(range.end()));
                assert!(
                    oracle.attrs.iter().any(|a| (a.start, a.end) == (s, e)),
                    "we committed an attribute resolution at {range:?} in {src:?}, \
                     where FCS reported no attribute"
                );
            }
        }
    }

    commits
}

fn assert_attrs_match_fcs(src: &str, env: &AssemblyEnv, expected_commits: usize) -> ResolvedFile {
    assert_attrs_match_fcs_with(src, env, expected_commits, false)
}

/// The completeness floor of the whole stage: the bare common case must
/// *commit*, not defer — a gate consumer gains nothing from a resolver that
/// declines `[<Literal>]`.
#[test]
fn diff_bare_literal_commits_via_the_suffix() {
    let env = fsharp_core_env();
    let src = "module Test\n\n[<Literal>]\nlet X = 5\n";
    let rf = assert_attrs_match_fcs(src, &env, 1);
    let span = {
        let start = src.find("Literal").unwrap();
        TextRange::new(
            u32::try_from(start).unwrap().into(),
            u32::try_from(start + "Literal".len()).unwrap().into(),
        )
    };
    assert!(
        matches!(
            rf.attribute_resolution_at(span),
            Some(Resolution::Entity(_))
        ),
        "the suffix-first candidate must commit the assembly entity"
    );
}

/// The written-suffix form commits; a **qualified** form defers wholesale —
/// committing multi-segment paths soundly means every qualifier segment
/// participating in every shadow guard, the per-segment enumeration the
/// abandoned classifiers drowned in, and real attribute names are
/// overwhelmingly bare (review on stage 4).
#[test]
fn diff_written_suffix_commits_and_qualified_defers() {
    let env = fsharp_core_env();
    let src = "module Test\n\n[<LiteralAttribute>]\nlet X = 5\n\n[<Microsoft.FSharp.Core.Literal>]\nlet Y = 6\n";
    let rf = assert_attrs_match_fcs(src, &env, 1);
    let start = src.find("[<Microsoft").unwrap() + 2;
    let span = TextRange::new(
        u32::try_from(start).unwrap().into(),
        u32::try_from(start + "Microsoft.FSharp.Core.Literal".len())
            .unwrap()
            .into(),
    );
    assert!(
        matches!(
            rf.attribute_resolution_at(span),
            Some(Resolution::Deferred(_))
        ),
        "a qualified attribute path defers wholesale, got {:?}",
        rf.attribute_resolution_at(span)
    );
}

/// An in-file attribute *class* reached through the suffix candidate: both
/// sides bind the project type (compared by declaration site).
#[test]
fn diff_in_file_attribute_class_commits_locally() {
    let env = fsharp_core_env();
    let src = "module Test\n\ntype MyAttrAttribute() =\n    inherit System.Attribute()\n\n[<MyAttr>]\nlet x = 1\n";
    assert_attrs_match_fcs(src, &env, 1);
}

/// The load-bearing alias shape for the extension gate: an in-file
/// abbreviation of an attribute type, found via the *written* candidate after
/// the suffixed one cleanly misses.
#[test]
fn diff_in_file_alias_commits_locally() {
    let env = fsharp_core_env();
    let src = "module Test\n\ntype MyLit = Microsoft.FSharp.Core.LiteralAttribute\n\n[<MyLit>]\nlet X = 5\n";
    assert_attrs_match_fcs(src, &env, 1);
}

/// A self-referential attribute (`[<MyAttr>]` on `type MyAttrAttribute`) —
/// FCS checks a tycon's attributes *after* entering it, and the resolver's
/// post-dispatch ordering must match.
#[test]
fn diff_self_referential_attribute_commits() {
    let env = fsharp_core_env();
    let src =
        "module Test\n\n[<MyAttr>]\ntype MyAttrAttribute() =\n    inherit System.Attribute()\n";
    assert_attrs_match_fcs(src, &env, 1);
}

/// An unresolvable attribute: FCS errors and sinks nothing; we record nothing
/// — absence agrees with absence, and neither side invents a claim.
#[test]
fn diff_unresolvable_attribute_is_no_claim_on_both_sides() {
    let env = fsharp_core_env();
    let src = "module Test\n\n[<ThisAttributeDoesNotExist>]\nlet x = 5\n";
    let rf = assert_attrs_match_fcs_with(src, &env, 0, true);
    assert_eq!(
        rf.attribute_resolutions().len(),
        0,
        "both candidates miss everywhere: no record at all"
    );
}

/// A `global.`-rooted attribute anchors at the root tier only — a walk the
/// decision core does not model, so the verdict is an explicit deferral (no
/// claim), never a commit.
#[test]
fn diff_global_rooted_attribute_defers() {
    let env = fsharp_core_env();
    let src = "module Test\n\n[<global.Microsoft.FSharp.Core.Literal>]\nlet X = 5\n";
    let rf = assert_attrs_match_fcs(src, &env, 0);
    let start = src.find("global.Microsoft").unwrap();
    let span = TextRange::new(
        u32::try_from(start).unwrap().into(),
        u32::try_from(start + "global.Microsoft.FSharp.Core.Literal".len())
            .unwrap()
            .into(),
    );
    assert!(
        matches!(
            rf.attribute_resolution_at(span),
            Some(Resolution::Deferred(_))
        ),
        "a global.-rooted attribute must defer explicitly, got {:?}",
        rf.attribute_resolution_at(span)
    );
}

/// A synthetic assembly entity for the shadow-source fixtures below: the real
/// `System.String` re-labelled, so every metadata field the projector needs is
/// populated without hand-building an `Entity`.
fn synthetic_type(
    namespace: &[&str],
    name: &str,
    access: borzoi_assembly::Access,
    kind: borzoi_assembly::EntityKind,
) -> borzoi_assembly::Entity {
    let dll = crate::common::ensure_system_runtime_dll();
    let bytes = std::fs::read(&dll).expect("read System.Runtime.dll");
    let asm = Ecma335Assembly::parse(&bytes).expect("parse System.Runtime.dll");
    let mut e = asm
        .enumerate_type_defs()
        .expect("enumerate")
        .into_iter()
        .find(|e| e.namespace == ["System"] && e.name == "String")
        .expect("String template");
    e.namespace = namespace.iter().map(|s| s.to_string()).collect();
    e.name = name.to_string();
    e.source_name = None;
    e.access = access;
    e.kind = kind;
    e.members = Vec::new();
    e.nested_types = Vec::new();
    e
}

/// Our verdict for the attribute written as `[<written…>]` in `src` under
/// `env`. Anchored on the `[<` bracket so a same-named type/exception
/// *definition* earlier in the source cannot be mistaken for the attribute.
fn verdict_at(env: &AssemblyEnv, src: &str, written: &str) -> Option<Resolution> {
    let rf = resolve(src, env);
    let start = src
        .find(&format!("[<{written}"))
        .expect("written attr in src")
        + 2;
    let span = TextRange::new(
        u32::try_from(start).unwrap().into(),
        u32::try_from(start + written.len()).unwrap().into(),
    );
    rf.attribute_resolution_at(span)
}

/// Two referenced assemblies contribute the same type key: an inaccessible
/// ordinary type in the first-wins slot, a public `FooAttribute`
/// **abbreviation** in the second. The slot answer misreports both a match and
/// a miss (FCS merges same-FQN types latest-wins and binds the alias), so the
/// complete-scan guard must defer — the doom-loop round-4 collision, found
/// again by codex on this stage.
#[test]
fn cross_assembly_colliding_key_defers() {
    use std::path::PathBuf;
    let inaccessible = synthetic_type(
        &["Lib"],
        "FooAttribute",
        borzoi_assembly::Access::Internal,
        borzoi_assembly::EntityKind::Class,
    );
    let public_alias = synthetic_type(
        &["Lib"],
        "FooAttribute",
        borzoi_assembly::Access::Public,
        borzoi_assembly::EntityKind::Abbreviation,
    );
    let env = AssemblyEnv::from_assemblies_with_abbreviation_visibility(vec![
        (
            PathBuf::from("a.dll"),
            vec![inaccessible],
            borzoi_sema::AbbreviationVisibility::Modelled,
            vec![],
        ),
        (
            PathBuf::from("b.dll"),
            vec![public_alias],
            borzoi_sema::AbbreviationVisibility::Modelled,
            vec![],
        ),
    ]);
    let verdict = verdict_at(&env, "namespace Lib\n\n[<Foo>]\ntype X = A | B\n", "Foo");
    assert!(
        matches!(verdict, Some(Resolution::Deferred(_))),
        "a contested type key must defer, got {verdict:?}"
    );
}

/// A referenced-assembly `module N` merges with `namespace N` (FCS), so its
/// nested types — e.g. a `type FooAttribute = …` — are bare-visible there,
/// invisible to the top-level type index. Any candidate searched under that
/// namespace must defer (doom-loop round 5).
#[test]
fn assembly_module_namespace_merge_defers() {
    use std::path::PathBuf;
    let module_n = synthetic_type(
        &[],
        "N",
        borzoi_assembly::Access::Public,
        borzoi_assembly::EntityKind::Module,
    );
    let env = AssemblyEnv::from_assemblies_with_abbreviation_visibility(vec![(
        PathBuf::from("n.dll"),
        vec![module_n],
        borzoi_sema::AbbreviationVisibility::Modelled,
        vec![],
    )]);
    let verdict = verdict_at(&env, "namespace N\n\n[<Foo>]\ntype X = A | B\n", "Foo");
    assert!(
        matches!(verdict, Some(Resolution::Deferred(_))),
        "a namespace an assembly module merges into must defer, got {verdict:?}"
    );
}

/// A **retained manifest auto-open** — here a module-shaped assembly-level
/// `[<AutoOpen>]` target, kept in `auto_open_module_handles` and so absent
/// from every namespace-prefix walk — could supply `FooAttribute` bare at
/// higher priority than any modeled tier. Any candidate segment such a
/// surface could supply must defer (doom-loop round 6).
#[test]
fn retained_manifest_auto_open_defers() {
    use std::path::PathBuf;
    let nested = synthetic_type(
        &["Helpers"],
        "FooAttribute",
        borzoi_assembly::Access::Public,
        borzoi_assembly::EntityKind::Class,
    );
    let mut module_m = synthetic_type(
        &["Helpers"],
        "M",
        borzoi_assembly::Access::Public,
        borzoi_assembly::EntityKind::Module,
    );
    module_m.nested_types = vec![nested];
    let env = AssemblyEnv::from_assemblies_with_abbreviation_visibility(vec![(
        PathBuf::from("helpers.dll"),
        vec![module_m],
        borzoi_sema::AbbreviationVisibility::Modelled,
        vec!["Helpers.M".to_string()],
    )]);
    let verdict = verdict_at(&env, "module Test\n\n[<Foo>]\nlet x = 1\n", "Foo");
    assert!(
        matches!(verdict, Some(Resolution::Deferred(_))),
        "a retained manifest auto-open that could supply the candidate must defer, got {verdict:?}"
    );
}

/// An in-namespace `[<AutoOpen>]` module can supply the **head** of a
/// qualified candidate: `[<A.B>]` in `namespace Ns` where `Ns.Auto` is
/// auto-open and holds `module A` (with `BAttribute`) re-roots the whole path
/// at higher priority than the root `A.BAttribute` the tiered walk would
/// otherwise commit. The per-split shadow check must ask about the segment
/// supplied *at that split*, not the candidate's leaf (codex round 3).
#[test]
fn auto_open_supplied_head_of_a_qualified_candidate_defers() {
    use std::path::PathBuf;
    // The decoy the walk WOULD commit: a root-namespace `A.BAttribute`.
    let decoy = synthetic_type(
        &["A"],
        "BAttribute",
        borzoi_assembly::Access::Public,
        borzoi_assembly::EntityKind::Class,
    );
    // The auto-open module in `Ns` whose child `A` holds a `BAttribute` FCS
    // binds first.
    let shadowing_b = synthetic_type(
        &["Ns"],
        "BAttribute",
        borzoi_assembly::Access::Public,
        borzoi_assembly::EntityKind::Class,
    );
    let mut inner_a = synthetic_type(
        &["Ns"],
        "A",
        borzoi_assembly::Access::Public,
        borzoi_assembly::EntityKind::Module,
    );
    inner_a.nested_types = vec![shadowing_b];
    let mut auto = synthetic_type(
        &["Ns"],
        "Auto",
        borzoi_assembly::Access::Public,
        borzoi_assembly::EntityKind::Module,
    );
    auto.is_auto_open = true;
    auto.nested_types = vec![inner_a];
    let env = AssemblyEnv::from_assemblies_with_abbreviation_visibility(vec![(
        PathBuf::from("shadow.dll"),
        vec![decoy, auto],
        borzoi_sema::AbbreviationVisibility::Modelled,
        vec![],
    )]);
    let verdict = verdict_at(&env, "namespace Ns\n\n[<A.B>]\ntype X = C | D\n", "A.B");
    assert!(
        matches!(verdict, Some(Resolution::Deferred(_))),
        "an auto-open-supplied head must defer the qualified candidate, got {verdict:?}"
    );
}

/// A module-shaped leaf is not an attribute type. It is a clean miss for the
/// suffixed candidate, so attribute lookup falls through to the written
/// candidate; when that is absent too, neither FCS nor sema records a target.
#[test]
fn module_shaped_leaf_is_not_an_attribute_candidate() {
    use std::path::PathBuf;
    let module_foo = synthetic_type(
        &["Lib"],
        "FooAttribute",
        borzoi_assembly::Access::Public,
        borzoi_assembly::EntityKind::Module,
    );
    let env = AssemblyEnv::from_assemblies_with_abbreviation_visibility(vec![(
        PathBuf::from("lib.dll"),
        vec![module_foo],
        borzoi_sema::AbbreviationVisibility::Modelled,
        vec![],
    )]);
    let verdict = verdict_at(&env, "namespace Lib\n\n[<Foo>]\ntype X = A | B\n", "Foo");
    assert!(
        verdict.is_none(),
        "a module-shaped suffixed candidate must be a clean miss, got {verdict:?}"
    );
}

/// An **inaccessible** occupant of the suffixed key is not a clean miss: FCS
/// resolves the internal `FooAttribute` (then reports accessibility errors)
/// rather than falling through to the public written `Foo`, so committing the
/// written candidate would name a type FCS never binds (codex round 4).
#[test]
fn internal_suffix_occupant_defers_the_fallthrough() {
    use std::path::PathBuf;
    let internal_suffixed = synthetic_type(
        &["Lib"],
        "FooAttribute",
        borzoi_assembly::Access::Internal,
        borzoi_assembly::EntityKind::Class,
    );
    let public_written = synthetic_type(
        &["Lib"],
        "Foo",
        borzoi_assembly::Access::Public,
        borzoi_assembly::EntityKind::Class,
    );
    let env = AssemblyEnv::from_assemblies_with_abbreviation_visibility(vec![(
        PathBuf::from("lib.dll"),
        vec![internal_suffixed, public_written],
        borzoi_sema::AbbreviationVisibility::Modelled,
        vec![],
    )]);
    let verdict = verdict_at(&env, "namespace Lib\n\n[<Foo>]\ntype X = A | B\n", "Foo");
    assert!(
        matches!(verdict, Some(Resolution::Deferred(_))),
        "an internal occupant of the suffixed key must defer, got {verdict:?}"
    );
}

/// A **globally unknowable auto-open surface** — an assembly whose `AutoOpen`
/// list could not be read, or a skipped projection — could hide an auto-open
/// supplying any name at higher priority than every modeled tier, so every
/// attribute candidate must defer (codex round 5).
#[test]
fn unknowable_auto_open_surface_defers_every_candidate() {
    let mut env = fsharp_core_env();
    env.mark_extension_surface_unknowable();
    let verdict = verdict_at(&env, "module Test\n\n[<Literal>]\nlet X = 5\n", "Literal");
    assert!(
        matches!(verdict, Some(Resolution::Deferred(_))),
        "an unknowable auto-open surface must defer even [<Literal>], got {verdict:?}"
    );
}

/// F# is latest-wins across bindings and opens alike: an in-file attribute
/// type declared *before* an `open` loses the contest to anything the open
/// could supply, so the local commit must defer — while the conventional
/// opens-first shape still commits (codex round 6).
#[test]
fn later_open_defers_an_earlier_in_file_type() {
    let env = fsharp_core_env();
    // Type BEFORE the open: FCS binds Microsoft.FSharp.Core.LiteralAttribute
    // through the later open, not the in-file type — we must not commit.
    let shadowed = "module Test\n\ntype LiteralAttribute() =\n    inherit System.Attribute()\n\nopen Microsoft.FSharp.Core\n\n[<Literal>]\nlet f () = 1\n";
    let verdict = verdict_at(&env, shadowed, "Literal");
    assert!(
        matches!(verdict, Some(Resolution::Deferred(_))),
        "an in-file type older than an open must defer, got {verdict:?}"
    );
    // Open BEFORE the type: the in-file definition is the latest introduction
    // and wins in FCS — the local commit stands.
    let winning = "module Test\n\nopen Microsoft.FSharp.Core\n\ntype LiteralAttribute() =\n    inherit System.Attribute()\n\n[<Literal>]\nlet f () = 1\n";
    let verdict = verdict_at(&env, winning, "Literal");
    assert!(
        matches!(verdict, Some(Resolution::Local(_))),
        "an in-file type younger than every open still commits, got {verdict:?}"
    );
}

/// FCS's attribute lookup is arity-0: a *generic* in-file
/// `LiteralAttribute<'T>` is skipped, and FCS binds FSharp.Core's arity-0
/// type — the arity-agnostic in-file lookup must not hand the generic local
/// back as a commit (codex round 7).
#[test]
fn generic_in_file_type_defers_the_candidate() {
    let env = fsharp_core_env();
    let src = "module Test\n\nopen Microsoft.FSharp.Core\n\ntype LiteralAttribute<'T>() =\n    inherit System.Attribute()\n\n[<Literal>]\nlet x = 1\n";
    let verdict = verdict_at(&env, src, "Literal");
    assert!(
        matches!(verdict, Some(Resolution::Deferred(_))),
        "a generic local sharing the candidate name must defer, got {verdict:?}"
    );
}

/// An outer in-file type must not commit past a **closer** declaration our
/// in-file table cannot see: a forward-declared type in a `rec` block, or an
/// exception (never in `type_defs`) — FCS binds the closer one
/// (codex on stage 4).
#[test]
fn closer_shadow_defers_the_outer_in_file_hit() {
    let env = fsharp_core_env();
    // Inside a rec module, a forward-declared `FooAttribute` outranks the
    // outer one our source-ordered table returns.
    let rec_src = "module Test\n\ntype FooAttribute() =\n    inherit System.Attribute()\n\nmodule rec Inner =\n    [<Foo>]\n    type X = { y: int }\n\n    type FooAttribute() =\n        inherit System.Attribute()\n";
    let verdict = verdict_at(&env, rec_src, "Foo");
    assert!(
        matches!(verdict, Some(Resolution::Deferred(_))),
        "a rec-block attribute must defer the outer in-file hit, got {verdict:?}"
    );
    // A closer exception of the suffixed name outranks the outer type.
    let exn_src = "module Test\n\ntype LitAttribute() =\n    inherit System.Attribute()\n\nmodule Inner =\n    exception LitAttribute of string\n\n    [<Lit>]\n    let x = 5\n";
    let verdict = verdict_at(&env, exn_src, "Lit");
    assert!(
        matches!(verdict, Some(Resolution::Deferred(_))),
        "a closer exception must defer the outer in-file hit, got {verdict:?}"
    );
}

/// An `exception E` occupies the *type* namespace too (FCS resolves `[<E>]`
/// to the exception, then errors on its constructor), so exception names must
/// feed the project-type guard (codex round 6).
#[test]
fn exception_name_defers_the_candidate() {
    let env = fsharp_core_env();
    let src = "module Test\n\nexception LitAttribute of string\n\n[<Lit>]\nlet X = 5\n";
    let verdict = verdict_at(&env, src, "Lit");
    assert!(
        matches!(verdict, Some(Resolution::Deferred(_))),
        "an exception occupying the suffixed key must defer, got {verdict:?}"
    );
}

/// A retained (module-shaped) manifest auto-open whose assembly's signature
/// pickle is **undecodable** hides its module-scoped aliases from the tree
/// walk — the surface must defer every candidate name, not just the visible
/// ones (codex on stage 4).
#[test]
fn unknowable_pickle_behind_a_retained_auto_open_defers() {
    use std::path::PathBuf;
    let mut auto = synthetic_type(
        &["Helpers"],
        "M",
        borzoi_assembly::Access::Public,
        borzoi_assembly::EntityKind::Module,
    );
    auto.is_auto_open = true;
    let env = AssemblyEnv::from_assemblies_with_abbreviation_visibility(vec![(
        PathBuf::from("helpers.dll"),
        vec![auto],
        borzoi_sema::AbbreviationVisibility::Unknowable,
        vec!["Helpers.M".to_string()],
    )]);
    let verdict = verdict_at(&env, "module Test\n\n[<Foo>]\nlet x = 1\n", "Foo");
    assert!(
        matches!(verdict, Some(Resolution::Deferred(_))),
        "an unknowable pickle behind a retained auto-open must defer, got {verdict:?}"
    );
}

/// A **headerless** (anonymous-module) file exports no qualified type paths,
/// but F# still exposes its types to later files through the implicit
/// filename module — so its type names must feed the cross-file guard all the
/// same (found by codex on this stage).
#[test]
fn headerless_file_type_defers_a_later_candidate() {
    let env = fsharp_core_env();
    let f1 =
        "type HidedAttribute = Microsoft.FSharp.Core.LiteralAttribute\n\nlet placeholder = 1\n";
    let f2 = "module B\n\n[<Hided>]\nlet X = 5\n";
    let files: Vec<ImplFile> = [f1, f2]
        .iter()
        .map(|s| {
            let p = parse(s);
            assert!(p.errors.is_empty());
            ImplFile::cast(p.root).expect("impl file")
        })
        .collect();
    let project = borzoi_sema::resolve_project(&files, &env);
    let rf2 = &project.files()[1];
    let start = f2.find("Hided").unwrap();
    let span = TextRange::new(
        u32::try_from(start).unwrap().into(),
        u32::try_from(start + "Hided".len()).unwrap().into(),
    );
    assert!(
        matches!(
            rf2.attribute_resolution_at(span),
            Some(Resolution::Deferred(_))
        ),
        "a headerless file's type must still guard later candidates, got {:?}",
        rf2.attribute_resolution_at(span)
    );
}

/// A cross-file project alias (`type HidedAttribute = …` in an earlier file)
/// is invisible to the tiered walk, so a later file's `[<Hided>]` must defer —
/// the bare-cross-file hole that sank the doom-loop classifiers, closed here
/// by the project-type-simple-name guard.
#[test]
fn cross_file_project_alias_defers() {
    let env = fsharp_core_env();
    let f1 = "module A\n\ntype HidedAttribute = Microsoft.FSharp.Core.LiteralAttribute\n";
    let f2 = "module B\n\n[<Hided>]\nlet X = 5\n";
    let files: Vec<ImplFile> = [f1, f2]
        .iter()
        .map(|s| {
            let p = parse(s);
            assert!(p.errors.is_empty());
            ImplFile::cast(p.root).expect("impl file")
        })
        .collect();
    let project = borzoi_sema::resolve_project(&files, &env);
    let rf2 = &project.files()[1];
    let start = f2.find("Hided").unwrap();
    let span = TextRange::new(
        u32::try_from(start).unwrap().into(),
        u32::try_from(start + "Hided".len()).unwrap().into(),
    );
    assert!(
        matches!(
            rf2.attribute_resolution_at(span),
            Some(Resolution::Deferred(_))
        ),
        "a candidate a preceding file's type could alias must defer, got {:?}",
        rf2.attribute_resolution_at(span)
    );
}

/// The same guard within one file, order-independently: a `type LitAttribute`
/// declared *after* the attribute (illegal forward reference for FCS, but our
/// pre-scan is whole-file) must still force a defer rather than let the walk
/// commit FSharp.Core's `LitAttribute`-less miss to the written candidate.
#[test]
fn own_file_later_type_defers_the_candidate() {
    let env = fsharp_core_env();
    // `[<Lit>]` suffixes to `LitAttribute`, which this file declares later.
    let src = "module Test\n\n[<Lit>]\nlet X = 5\n\ntype LitAttribute = Microsoft.FSharp.Core.LiteralAttribute\n";
    let rf = resolve(src, &env);
    let start = src.find("Lit>").unwrap();
    let span = TextRange::new(
        u32::try_from(start).unwrap().into(),
        u32::try_from(start + "Lit".len()).unwrap().into(),
    );
    assert!(
        matches!(
            rf.attribute_resolution_at(span),
            Some(Resolution::Deferred(_))
        ),
        "own-file pre-scan must defer the candidate, got {:?}",
        rf.attribute_resolution_at(span)
    );
}

/// Attributes on the file's own top-level module go through a *speculative*
/// check pass (`TcAttributesCanFail`) before the real one; both passes sink.
/// The op must collapse identical re-records to exactly one per attribute.
#[test]
fn file_module_attribute_is_reported_once() {
    let src = "[<AutoOpen>]\nmodule Test\n\n[<Literal>]\nlet X = 5\n";
    let oracle = attrs_of("attr_module", src);
    assert_eq!(oracle.errors, vec![], "clean check expected");
    assert_eq!(oracle.attrs.len(), 2, "attrs: {:#?}", oracle.attrs);
    let auto = &oracle.attrs[0];
    assert_eq!((auto.start, auto.end), span_of(src, "AutoOpen"));
    assert_eq!(
        auto.full_name.as_deref(),
        Some("Microsoft.FSharp.Core.AutoOpenAttribute")
    );
    let lit = &oracle.attrs[1];
    assert_eq!(
        lit.full_name.as_deref(),
        Some("Microsoft.FSharp.Core.LiteralAttribute")
    );
}

/// The stage-5 gate bit, derived from the per-attribute verdicts: concrete
/// non-extension commits (assembly or in-file class) contribute nothing;
/// an in-file abbreviation (unchased target) or any deferral keeps the
/// presence defer.
#[test]
fn gate_bit_derivation_from_the_verdicts() {
    let env = fsharp_core_env();
    let bit = |src: &str| resolve(src, &env).attributes_may_declare_extension(&env);
    assert!(
        !bit("module Test\n\n[<Literal>]\nlet X = 5\n"),
        "a committed non-extension assembly type contributes nothing"
    );
    assert!(
        !bit(
            "module Test\n\ntype MyAttrAttribute() =\n    inherit System.Attribute()\n\n[<MyAttr>]\nlet x = 1\n"
        ),
        "a committed in-file concrete class is its own tycon, never the marker"
    );
    assert!(
        bit(
            "module Test\n\ntype MyLit = Microsoft.FSharp.Core.LiteralAttribute\n\n[<MyLit>]\nlet X = 5\n"
        ),
        "a committed in-file ABBREVIATION could alias the marker — defer"
    );
    assert!(
        bit("module Test\n\n[<global.Microsoft.FSharp.Core.Literal>]\nlet X = 5\n"),
        "a deferred attribute verdict keeps the presence defer"
    );
}

// ===== AO-2: the project-auto-open presence defer goes name-keyed =====
//
// Stage 4's scope-narrowing deferred EVERY attribute candidate in any file
// with a project `[<AutoOpen>]` module in scope-history. The defer is
// redundant: everything such a module can do to an attribute lookup is
// already guarded name-keyed —
//
// - *supplying* the candidate: any type or exception it declares, at any
//   depth, any block, is in the file-global §2(d) pre-scan
//   (`own_type_simple_names`, exceptions included) and threads cross-file as
//   `project_type_simple_names`, so `project_type_named` defers those
//   candidates in every non-in-file arm;
// - *contesting* an in-file hit: `decide_type_path`'s
//   `auto_open_type_shadow_names` guard models the positional latest-wins
//   contest for the names a same-block auto-open actually declares, and an
//   earlier block's or preceding file's import position is always earlier
//   than the current block's definition, so an in-file hit is FCS's winner;
//   an auto-open `exception` is covered by the in-file arm's
//   file-global exception guard.
//
// These tests pin both directions: unrelated auto-open content no longer
// defers (the corpus-recovery direction), and every supplying/contesting
// shape still does.

/// The recovery direction: an auto-open module declaring only *unrelated*
/// types must not defer the file's attributes — `[<AutoOpen>]` itself and
/// `[<Literal>]` both commit (FCS-diffed).
#[test]
fn diff_auto_open_of_unrelated_types_no_longer_defers_the_attribute() {
    let env = fsharp_core_env();
    let src = "module Test\n[<AutoOpen>]\nmodule Helpers =\n    type Widget() = class end\n[<Literal>]\nlet X = 5\n";
    assert_attrs_match_fcs(src, &env, 2);
}

/// The supplying direction: an auto-open module declaring the candidate's
/// type still defers it — via the file-global project-type pre-scan, with or
/// without a colliding top-level definition — while `[<AutoOpen>]` itself
/// commits.
#[test]
fn diff_auto_open_supplying_the_candidate_still_defers() {
    let env = fsharp_core_env();
    // No top-level FooAttribute: FCS binds the auto-opened nested one; the
    // pre-scan defers the candidate in the assembly/no-match arms.
    let supplies = "module Test\n[<AutoOpen>]\nmodule M =\n    type FooAttribute() =\n        inherit System.Attribute()\n[<Foo>]\ntype X() = class end\n";
    assert_attrs_match_fcs(supplies, &env, 1);
    assert!(
        matches!(
            verdict_at(&env, supplies, "Foo"),
            Some(Resolution::Deferred(_))
        ),
        "an auto-open-supplied candidate must defer"
    );

    // A colliding top-level definition: FCS still binds the LATER auto-opened
    // one (the module opens at its declaration point); the in-file arm's
    // auto-open shadow guard defers the hit.
    let collides = "module Test\ntype FooAttribute() =\n    inherit System.Attribute()\n[<AutoOpen>]\nmodule M =\n    type FooAttribute() =\n        inherit System.Attribute()\n[<Foo>]\ntype X() = class end\n";
    assert_attrs_match_fcs(collides, &env, 1);
    assert!(
        matches!(
            verdict_at(&env, collides, "Foo"),
            Some(Resolution::Deferred(_))
        ),
        "an auto-open-contested in-file hit must defer"
    );
}

/// The anonymous-root variant of the contest: no auto-open module *path* is
/// ever recorded under an anonymous root (nothing is cross-file exportable
/// there), so this soundness never rested on the presence defer — the
/// name-keyed shadow guard is what defers, and deleting the presence defer
/// changes nothing here (pinned because the shapes are easy to conflate).
#[test]
fn diff_anonymous_root_auto_open_type_contest() {
    let env = fsharp_core_env();
    let src = "type FooAttribute() =\n    inherit System.Attribute()\n[<AutoOpen>]\nmodule M =\n    type FooAttribute() =\n        inherit System.Attribute()\n[<Foo>]\ntype X() = class end\n";
    assert_attrs_match_fcs(src, &env, 1);
    assert!(
        matches!(verdict_at(&env, src, "Foo"), Some(Resolution::Deferred(_))),
        "the anonymous-root auto-open contest must defer"
    );
}

/// The cross-block straddle the old presence defer named as its reason: a
/// **same-named** later namespace block sees the earlier block's auto-open
/// (FCS re-opens the namespace, whose auto-open modules re-open with it)
/// where the resolver's block-scoped shadow set is cleared — but the §2(d)
/// pre-scan is file-global, so the candidate still defers name-keyed. A
/// *differently*-named later block does NOT see it (FCS errors — probed), so
/// the defer there is decline-on-unresolvable: agreement either way.
#[test]
fn diff_cross_block_auto_open_straddle_still_defers() {
    let env = fsharp_core_env();
    let src = "namespace A\n[<AutoOpen>]\nmodule Helpers =\n    type FooAttribute() =\n        inherit System.Attribute()\nnamespace A\n[<Foo>]\ntype X() = class end\n";
    assert_attrs_match_fcs(src, &env, 1);
    assert!(
        matches!(verdict_at(&env, src, "Foo"), Some(Resolution::Deferred(_))),
        "a cross-block auto-open-supplied candidate must defer"
    );

    let unrelated_block = "namespace A\n[<AutoOpen>]\nmodule Helpers =\n    type FooAttribute() =\n        inherit System.Attribute()\nnamespace B\n[<Foo>]\ntype X() = class end\n";
    assert_attrs_match_fcs_with(unrelated_block, &env, 1, true);
    assert!(
        matches!(
            verdict_at(&env, unrelated_block, "Foo"),
            Some(Resolution::Deferred(_))
        ),
        "a differently-named block's candidate declines where FCS errors"
    );

    // The three-block straddle (codex on AO-2): a block-1 DIRECT type of the
    // name, a block-2 auto-open redeclaration, the attribute in block 3. FCS
    // binds the auto-open's type (its import outranks the earlier direct
    // definition, latest-wins), while `lookup_type_def` retains block 1's
    // direct type and the block-scoped shadow guard was cleared — so without
    // the file-global auto-open name guard the in-file arm commits the WRONG
    // binder. Must defer.
    let three_block = "namespace A\ntype FooAttribute() =\n    inherit System.Attribute()\nnamespace A\n[<AutoOpen>]\nmodule Helpers =\n    type FooAttribute() =\n        inherit System.Attribute()\nnamespace A\n[<Foo>]\ntype X() = class end\n";
    assert_attrs_match_fcs(three_block, &env, 1);
    assert!(
        matches!(
            verdict_at(&env, three_block, "Foo"),
            Some(Resolution::Deferred(_))
        ),
        "the three-block straddle must defer the in-file hit"
    );
}

/// Cross-file: a preceding file's exportable auto-open module defers exactly
/// the candidates it could supply — `[<Foo>]` (its `FooAttribute` threads in
/// `project_type_simple_names`) defers, `[<Literal>]` commits.
#[test]
fn cross_file_auto_open_defers_only_its_names() {
    use borzoi_sema::resolve_project;
    let env = fsharp_core_env();
    let f1 = "module A\n[<AutoOpen>]\nmodule Helpers =\n    type FooAttribute() =\n        inherit System.Attribute()\n";
    let f2 = "module B\n[<Foo>]\ntype X() = class end\n[<Literal>]\nlet L = 5\n";
    let files: Vec<ImplFile> = [f1, f2]
        .iter()
        .map(|s| {
            let p = parse(s);
            assert!(p.errors.is_empty(), "parse errors in {s:?}: {:?}", p.errors);
            ImplFile::cast(p.root).expect("impl file")
        })
        .collect();
    let project = resolve_project(&files, &env);
    let rf2 = &project.files()[1];
    let at = |written: &str| {
        let start = f2.find(&format!("[<{written}")).expect("attr in f2") + 2;
        let span = TextRange::new(
            u32::try_from(start).unwrap().into(),
            u32::try_from(start + written.len()).unwrap().into(),
        );
        rf2.attribute_resolution_at(span)
    };
    assert!(
        matches!(at("Foo"), Some(Resolution::Deferred(_))),
        "a preceding auto-open's type name must defer its candidate, got {:?}",
        at("Foo")
    );
    assert!(
        matches!(at("Literal"), Some(Resolution::Entity(_))),
        "an unrelated candidate must commit despite the preceding auto-open, got {:?}",
        at("Literal")
    );
}
