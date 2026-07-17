//! The glob-expansion seam (`GlobResolver`): routing wildcard/`Exclude`
//! specs through the resolver vs. the no-resolver diagnostics, plus the
//! splice property test.

use super::*;
use proptest::prelude::*;
use tempfile::TempDir;

// -------------------------------------------------------------------------
// Phase 9a: the glob-expansion seam (`GlobResolver`)
// -------------------------------------------------------------------------
//
// `parse_fsproj_with_imports` takes an optional `GlobResolver`. When
// present, a `<Compile>`/`<ProjectReference>` element whose `Include`
// contains a wildcard, or that carries an `Exclude`, is routed through
// the resolver: the evaluator hands it a `GlobRequest { base_dir,
// include, excludes }` and splices the returned paths verbatim. When the
// resolver is absent the historical diagnostics (`UnsupportedGlob` for a
// wildcard, `UnsupportedItemOperation` for `Exclude`) are preserved.
//
// These tests use a stub resolver so the seam can be exercised without a
// real filesystem matcher (that lives in the LSP shell, phase 9b).

/// Owned snapshot of a [`GlobRequest`] the evaluator handed the stub
/// resolver, so tests can assert on what the seam carried.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CapturedGlob {
    base_dir: PathBuf,
    include: String,
    excludes: Vec<String>,
}

/// Run `parse_fsproj_with_imports` with a stub `GlobResolver` that
/// records every [`GlobRequest`] it receives and always returns `out`
/// (spliced verbatim). Returns the parsed project plus the captured
/// requests in call order.
fn parse_capturing_glob(
    project_path: &Path,
    source: &str,
    out: Vec<PathBuf>,
) -> (ParsedProject, Vec<CapturedGlob>) {
    let canon_project = canon(project_path);
    let captured = std::cell::RefCell::new(Vec::new());
    let resolver = |req: &GlobRequest<'_>| {
        captured.borrow_mut().push(CapturedGlob {
            base_dir: req.base_dir.to_path_buf(),
            include: req.include.to_string(),
            excludes: req.excludes.to_vec(),
        });
        out.clone()
    };
    let result = parse_fsproj_with_imports(
        source,
        &canon_project,
        &HashMap::new(),
        &HashMap::new(),
        None,
        Some(&resolver),
    )
    .expect("well-formed XML parses");
    (result, captured.into_inner())
}

/// [`parse_capturing_glob`], reading the body back from `project_path`
/// on disk (see [`parse_file`]).
fn parse_file_capturing_glob(
    project_path: &Path,
    out: Vec<PathBuf>,
) -> (ParsedProject, Vec<CapturedGlob>) {
    parse_capturing_glob(
        project_path,
        &std::fs::read_to_string(project_path).unwrap(),
        out,
    )
}

#[test]
fn glob_include_routes_through_resolver() {
    // THE fix: with a resolver present a wildcard `Include` no longer
    // yields `UnsupportedGlob` — it is expanded by the resolver and the
    // results become Compile items, in the order the resolver returns.
    let tmp = TempDir::new().unwrap();
    let dir = canon(tmp.path());
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <ItemGroup>
    <Compile Include="*.fs" />
  </ItemGroup>
</Project>"#,
    );
    let out = vec![dir.join("a.fs"), dir.join("b.fs")];
    let (result, requests) = parse_file_capturing_glob(&project_path, out.clone());
    assert_eq!(paths_of(&result.items), out);
    assert_eq!(requests.len(), 1, "resolver should be consulted once");
    assert!(
        result.diagnostics.is_empty(),
        "no diagnostics expected; got {:?}",
        result.diagnostics
    );
    assert!(!result.is_partial);
}

#[test]
fn glob_request_carries_base_dir_include_and_excludes() {
    // The seam is "fat": the resolver receives the full include spec
    // (post-`$(...)` expansion, refs stripped) and the split exclude
    // list, against the entry project directory.
    let tmp = TempDir::new().unwrap();
    let dir = canon(tmp.path());
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <ItemGroup>
    <Compile Include="*.fs;extra.fs" Exclude="skip.fs;*.g.fs" />
  </ItemGroup>
</Project>"#,
    );
    let (_result, requests) = parse_file_capturing_glob(&project_path, vec![]);
    assert_eq!(
        requests,
        vec![CapturedGlob {
            base_dir: dir,
            include: "*.fs;extra.fs".to_string(),
            excludes: vec!["skip.fs".to_string(), "*.g.fs".to_string()],
        }]
    );
}

#[test]
fn mixed_literal_and_glob_routes_whole_spec() {
    // A literal entry alongside a glob is NOT pushed separately: the
    // whole include spec (literal + glob) goes to the resolver, which is
    // the single source of truth for the resulting item set and order.
    let tmp = TempDir::new().unwrap();
    let dir = canon(tmp.path());
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <ItemGroup>
    <Compile Include="lit.fs;*.fs" />
  </ItemGroup>
</Project>"#,
    );
    let out = vec![dir.join("lit.fs"), dir.join("globbed.fs")];
    let (result, requests) = parse_file_capturing_glob(&project_path, out.clone());
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].include, "lit.fs;*.fs");
    assert_eq!(paths_of(&result.items), out);
}

#[test]
fn item_reference_in_glob_include_diagnosed_and_stripped() {
    // An `@(...)` entry mixed into a globbing include keeps its
    // `UnresolvedItemReference` diagnostic and is removed from the spec
    // handed to the resolver (we never silently feed it through).
    let tmp = TempDir::new().unwrap();
    let dir = canon(tmp.path());
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <ItemGroup>
    <Compile Include="*.fs;@(Other)" />
  </ItemGroup>
</Project>"#,
    );
    let out = vec![dir.join("a.fs")];
    let (result, requests) = parse_file_capturing_glob(&project_path, out.clone());
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].include, "*.fs", "@(Other) must be stripped");
    assert_eq!(paths_of(&result.items), out);
    assert_eq!(result.diagnostics.len(), 1);
    assert!(matches!(
        result.diagnostics[0].kind,
        DiagnosticKind::UnresolvedItemReference { .. }
    ));
    assert!(result.is_partial);
}

#[test]
fn metadata_reference_in_glob_include_diagnosed_and_stripped() {
    let tmp = TempDir::new().unwrap();
    let dir = canon(tmp.path());
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <ItemGroup>
    <Compile Include="*.fs;%(Foo.Bar)" />
  </ItemGroup>
</Project>"#,
    );
    let out = vec![dir.join("a.fs")];
    let (result, requests) = parse_file_capturing_glob(&project_path, out.clone());
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].include, "*.fs");
    assert_eq!(paths_of(&result.items), out);
    assert_eq!(result.diagnostics.len(), 1);
    assert!(matches!(
        result.diagnostics[0].kind,
        DiagnosticKind::UnresolvedMetadataReference { .. }
    ));
}

#[test]
fn literal_minus_exclude_routes_through_resolver() {
    // An `Exclude` with no wildcard anywhere still routes (Exclude
    // applies to literals too). MSBuild here yields nothing; the stub
    // models that by returning an empty list.
    let tmp = TempDir::new().unwrap();
    let dir = canon(tmp.path());
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <ItemGroup>
    <Compile Include="a.fs" Exclude="a.fs" />
  </ItemGroup>
</Project>"#,
    );
    let (result, requests) = parse_file_capturing_glob(&project_path, vec![]);
    assert_eq!(
        requests,
        vec![CapturedGlob {
            base_dir: dir,
            include: "a.fs".to_string(),
            excludes: vec!["a.fs".to_string()],
        }]
    );
    assert!(result.items.is_empty());
    assert!(
        result.diagnostics.is_empty(),
        "Exclude must not be UnsupportedItemOperation when a resolver is present; got {:?}",
        result.diagnostics
    );
    assert!(!result.is_partial);
}

#[test]
fn glob_without_resolver_keeps_unsupported_glob() {
    // Regression guard: with no resolver, a wildcard include is still
    // `UnsupportedGlob` (phase-8 behaviour, exercised by `parse`).
    let tmp = TempDir::new().unwrap();
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <ItemGroup>
    <Compile Include="*.fs" />
  </ItemGroup>
</Project>"#,
    );
    let result = parse_file(&project_path);
    assert!(result.items.is_empty());
    assert_eq!(result.diagnostics.len(), 1);
    assert!(matches!(
        result.diagnostics[0].kind,
        DiagnosticKind::UnsupportedGlob { .. }
    ));
}

#[test]
fn exclude_without_resolver_keeps_unsupported_item_operation() {
    let tmp = TempDir::new().unwrap();
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <ItemGroup>
    <Compile Include="a.fs" Exclude="b.fs" />
  </ItemGroup>
</Project>"#,
    );
    let result = parse_file(&project_path);
    assert!(result.items.is_empty());
    assert_eq!(result.diagnostics.len(), 1);
    assert!(matches!(
        result.diagnostics[0].kind,
        DiagnosticKind::UnsupportedItemOperation { .. }
    ));
}

#[test]
fn empty_glob_match_emits_no_diagnostic() {
    // MSBuild is silent when a glob matches nothing — zero items, no
    // diagnostic, not partial.
    let tmp = TempDir::new().unwrap();
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <ItemGroup>
    <Compile Include="*.fs" />
  </ItemGroup>
</Project>"#,
    );
    let (result, requests) = parse_file_capturing_glob(&project_path, vec![]);
    assert_eq!(requests.len(), 1);
    assert!(result.items.is_empty());
    assert!(result.diagnostics.is_empty());
    assert!(!result.is_partial);
}

#[test]
fn globbed_items_carry_compile_node_span() {
    // Every expanded item carries the span of the originating `<Compile>`
    // element (the same span the literal path uses), so diagnostics and
    // navigation point at the element the user wrote.
    let tmp = TempDir::new().unwrap();
    let dir = canon(tmp.path());
    let element = r#"<Compile Include="*.fs" />"#;
    let source = format!("<Project>\n  <ItemGroup>\n    {element}\n  </ItemGroup>\n</Project>");
    let project_path = write_at(tmp.path(), "Demo.fsproj", &source);
    let out = vec![dir.join("a.fs"), dir.join("b.fs")];
    let (result, _) = parse_capturing_glob(&project_path, &source, out);
    let start = source.find(element).unwrap();
    let span = start..start + element.len();
    assert_eq!(result.items.len(), 2);
    for item in &result.items {
        assert_eq!(item.span, span);
    }
}

#[test]
fn link_applied_to_each_globbed_item() {
    let tmp = TempDir::new().unwrap();
    let dir = canon(tmp.path());
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <ItemGroup>
    <Compile Include="*.fs" Link="Linked" />
  </ItemGroup>
</Project>"#,
    );
    let out = vec![dir.join("a.fs"), dir.join("b.fs")];
    let (result, _) = parse_file_capturing_glob(&project_path, out);
    assert_eq!(result.items.len(), 2);
    for item in &result.items {
        assert_eq!(item.link.as_deref(), Some("Linked"));
    }
}

#[test]
fn exclude_item_reference_skips_whole_item() {
    // We can't know what an unresolved `@(...)` exclude removes, and
    // excluding nothing would over-include — so the whole item is
    // skipped and the resolver is never consulted.
    let tmp = TempDir::new().unwrap();
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <ItemGroup>
    <Compile Include="*.fs" Exclude="@(Skip)" />
  </ItemGroup>
</Project>"#,
    );
    let (result, requests) = parse_file_capturing_glob(&project_path, vec![]);
    assert!(
        requests.is_empty(),
        "resolver must not be consulted when the exclude can't be resolved"
    );
    assert!(result.items.is_empty());
    assert_eq!(result.diagnostics.len(), 1);
    assert!(matches!(
        result.diagnostics[0].kind,
        DiagnosticKind::UnresolvedItemReference { .. }
    ));
    assert!(result.is_partial);
}

#[test]
fn exclude_metadata_reference_skips_whole_item() {
    let tmp = TempDir::new().unwrap();
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <ItemGroup>
    <Compile Include="*.fs" Exclude="%(Skip.x)" />
  </ItemGroup>
</Project>"#,
    );
    let (result, requests) = parse_file_capturing_glob(&project_path, vec![]);
    assert!(requests.is_empty());
    assert!(result.items.is_empty());
    assert_eq!(result.diagnostics.len(), 1);
    assert!(matches!(
        result.diagnostics[0].kind,
        DiagnosticKind::UnresolvedMetadataReference { .. }
    ));
}

#[test]
fn project_reference_glob_routes_through_resolver() {
    // ProjectReference globs route too, but land in the
    // `project_references` bucket (not `items`), and never carry a Link.
    let tmp = TempDir::new().unwrap();
    let dir = canon(tmp.path());
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <ItemGroup>
    <ProjectReference Include="*/*.fsproj" />
  </ItemGroup>
</Project>"#,
    );
    let out = vec![dir.join("a/a.fsproj"), dir.join("b/b.fsproj")];
    let (result, requests) = parse_file_capturing_glob(&project_path, out.clone());
    assert_eq!(requests.len(), 1);
    assert!(result.items.is_empty(), "globbed PR must not land in items");
    assert_eq!(paths_of(&result.project_references), out);
    for pr in &result.project_references {
        assert_eq!(pr.kind, ItemKind::ProjectReference);
        assert_eq!(pr.link, None);
    }
}

#[test]
fn unpinned_exclude_value_marks_the_reference_list_uncertain() {
    // `Skip` is *written* (the gate evaluated true) but only by treating
    // `$(TargetFramework)` — the carve-out that stays inexact under C.2b —
    // as empty; the real build may skip that group, leaving `Skip` empty
    // and B *not* excluded, or (as here under our model) excluding it.
    // Either way the captured list and the real build can disagree in the
    // fabricating direction, exactly like an Include expanded from an
    // unpinned property. The expansion itself is clean
    // (`had_issue() == false`), so without consulting the pin state the
    // exclusion would look trustworthy.
    let tmp = TempDir::new().unwrap();
    let dir = canon(tmp.path());
    let project_path = write_at(
        tmp.path(),
        "Demo.fsproj",
        r#"<Project>
  <PropertyGroup Condition="'$(TargetFramework)' == ''">
    <Skip>../B/B.fsproj</Skip>
  </PropertyGroup>
  <ItemGroup>
    <ProjectReference Include="../B/B.fsproj;../C/C.fsproj" Exclude="$(Skip)" />
  </ItemGroup>
</Project>"#,
    );
    let out = vec![dir.join("C/C.fsproj")];
    let (result, requests) = parse_file_capturing_glob(&project_path, out);
    assert_eq!(requests.len(), 1, "the Exclude routes through the resolver");
    assert!(
        result.project_references_uncertain,
        "an Exclude expanded from an unpinned property may go the other \
         way in the real build"
    );

    // A cleanly-pinned Exclude value keeps the list certain: the resolver
    // honours it exactly.
    let project_path = write_at(
        tmp.path(),
        "Demo2.fsproj",
        r#"<Project>
  <PropertyGroup>
    <Skip>../B/B.fsproj</Skip>
  </PropertyGroup>
  <ItemGroup>
    <ProjectReference Include="../B/B.fsproj;../C/C.fsproj" Exclude="$(Skip)" />
  </ItemGroup>
</Project>"#,
    );
    let out = vec![dir.join("C/C.fsproj")];
    let (result, _) = parse_file_capturing_glob(&project_path, out);
    assert!(
        !result.project_references_uncertain,
        "a pinned Exclude value is exact"
    );
}

// ---- property test: the seam splices resolver output verbatim ----

#[derive(Debug, Clone)]
enum IncludeFrag {
    Literal(u8),
    Glob(u8),
    ItemRef(u8),
    MetaRef(u8),
}

impl IncludeFrag {
    fn text(&self) -> String {
        match self {
            IncludeFrag::Literal(i) => format!("lit{i}.fs"),
            IncludeFrag::Glob(i) => format!("g{i}*.fs"),
            IncludeFrag::ItemRef(i) => format!("@(Ref{i})"),
            IncludeFrag::MetaRef(i) => format!("%(Meta{i}.x)"),
        }
    }
    fn is_glob(&self) -> bool {
        matches!(self, IncludeFrag::Glob(_))
    }
    fn is_survivor(&self) -> bool {
        matches!(self, IncludeFrag::Literal(_) | IncludeFrag::Glob(_))
    }
    fn is_item_ref(&self) -> bool {
        matches!(self, IncludeFrag::ItemRef(_))
    }
    fn is_meta_ref(&self) -> bool {
        matches!(self, IncludeFrag::MetaRef(_))
    }
}

#[derive(Debug, Clone)]
struct GlobCase {
    kind_idx: usize,
    include_frags: Vec<IncludeFrag>,
    exclude_frags: Vec<String>,
    resolver_out: Vec<String>,
    link: Option<String>,
}

const GLOB_KINDS: [ItemKind; 4] = [
    ItemKind::Compile,
    ItemKind::CompileBefore,
    ItemKind::CompileAfter,
    ItemKind::ProjectReference,
];

fn glob_kind_tag(kind: ItemKind) -> &'static str {
    match kind {
        ItemKind::Compile => "Compile",
        ItemKind::CompileBefore => "CompileBefore",
        ItemKind::CompileAfter => "CompileAfter",
        ItemKind::ProjectReference => "ProjectReference",
    }
}

fn include_frag_strategy() -> impl Strategy<Value = IncludeFrag> {
    prop_oneof![
        (0u8..4).prop_map(IncludeFrag::Literal),
        (0u8..4).prop_map(IncludeFrag::Glob),
        (0u8..4).prop_map(IncludeFrag::ItemRef),
        (0u8..4).prop_map(IncludeFrag::MetaRef),
    ]
}

fn glob_case_strategy() -> impl Strategy<Value = GlobCase> {
    let include = prop::collection::vec(include_frag_strategy(), 0..5).prop_map(|mut v| {
        // Force at least one glob so the item always routes through the
        // resolver (the fast literal path is covered by other tests).
        if !v.iter().any(IncludeFrag::is_glob) {
            v.push(IncludeFrag::Glob(0));
        }
        v
    });
    let exclude = prop::collection::vec(
        prop_oneof![
            (0u8..4).prop_map(|i| format!("ex{i}.fs")),
            (0u8..4).prop_map(|i| format!("e{i}*.fs")),
        ],
        0..3,
    );
    let out = prop::collection::vec((0u8..6).prop_map(|i| format!("out{i}.fs")), 0..5);
    let link = prop::option::of((0u8..4).prop_map(|i| format!("link{i}")));
    (0usize..4, include, exclude, out, link).prop_map(
        |(kind_idx, include_frags, exclude_frags, resolver_out, link)| GlobCase {
            kind_idx,
            include_frags,
            exclude_frags,
            resolver_out,
            link,
        },
    )
}

/// Build the project source and run the case. Returns the parsed
/// project, the captured requests (the case always routes, so exactly
/// one), the expected resolver output paths, the element kind, and the
/// element's source span.
fn run_glob_case(
    case: &GlobCase,
    tmp: &Path,
) -> (
    ParsedProject,
    Vec<CapturedGlob>,
    Vec<PathBuf>,
    ItemKind,
    std::ops::Range<usize>,
) {
    let dir = canon(tmp);
    let kind = GLOB_KINDS[case.kind_idx];
    let tag = glob_kind_tag(kind);
    let include_attr = case
        .include_frags
        .iter()
        .map(IncludeFrag::text)
        .collect::<Vec<_>>()
        .join(";");
    let mut element = format!("<{tag} Include=\"{include_attr}\"");
    if !case.exclude_frags.is_empty() {
        element += &format!(" Exclude=\"{}\"", case.exclude_frags.join(";"));
    }
    if kind != ItemKind::ProjectReference
        && let Some(l) = &case.link
    {
        element += &format!(" Link=\"{l}\"");
    }
    element += " />";
    let source = format!("<Project>\n  <ItemGroup>\n    {element}\n  </ItemGroup>\n</Project>");
    let project_path = write_at(tmp, "Demo.fsproj", &source);
    let out: Vec<PathBuf> = case.resolver_out.iter().map(|n| dir.join(n)).collect();
    let (result, requests) = parse_capturing_glob(&project_path, &source, out.clone());
    let start = source.find(&element).unwrap();
    let span = start..start + element.len();
    (result, requests, out, kind, span)
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 96, ..ProptestConfig::default() })]

    /// For any item that routes (always >=1 glob), the evaluator:
    ///   - consults the resolver exactly once;
    ///   - hands it the surviving (ref-stripped) include spec and the
    ///     split exclude list, against the project directory;
    ///   - splices the resolver's output verbatim (order, paths) into
    ///     the correct bucket, each item carrying the element's kind,
    ///     link and span — adding, dropping, reordering nothing;
    ///   - emits exactly one diagnostic per `@()`/`%()` include ref.
    #[test]
    fn route_splices_resolver_output_verbatim(case in glob_case_strategy()) {
        let tmp = TempDir::new().unwrap();
        let (result, requests, out, kind, span) = run_glob_case(&case, tmp.path());

        prop_assert_eq!(requests.len(), 1);
        let req = &requests[0];
        let expected_include = case
            .include_frags
            .iter()
            .filter(|f| f.is_survivor())
            .map(IncludeFrag::text)
            .collect::<Vec<_>>()
            .join(";");
        prop_assert_eq!(&req.include, &expected_include);
        prop_assert_eq!(&req.excludes, &case.exclude_frags);
        prop_assert_eq!(&req.base_dir, &canon(tmp.path()));

        let (bucket, other): (&[ResolvedItem], &[ResolvedItem]) =
            if kind == ItemKind::ProjectReference {
                (&result.project_references, &result.items)
            } else {
                (&result.items, &result.project_references)
            };
        prop_assert!(other.is_empty(), "wrong bucket received items: {:?}", other);
        prop_assert_eq!(paths_of(bucket), out);
        let expected_link = if kind == ItemKind::ProjectReference {
            None
        } else {
            case.link.clone()
        };
        for item in bucket {
            prop_assert_eq!(item.kind, kind);
            prop_assert_eq!(&item.link, &expected_link);
            prop_assert_eq!(item.span.clone(), span.clone());
        }

        let n_item_ref = case.include_frags.iter().filter(|f| f.is_item_ref()).count();
        let n_meta_ref = case.include_frags.iter().filter(|f| f.is_meta_ref()).count();
        let got_item_ref = result
            .diagnostics
            .iter()
            .filter(|d| matches!(d.kind, DiagnosticKind::UnresolvedItemReference { .. }))
            .count();
        let got_meta_ref = result
            .diagnostics
            .iter()
            .filter(|d| matches!(d.kind, DiagnosticKind::UnresolvedMetadataReference { .. }))
            .count();
        prop_assert_eq!(got_item_ref, n_item_ref);
        prop_assert_eq!(got_meta_ref, n_meta_ref);
        prop_assert_eq!(result.diagnostics.len(), n_item_ref + n_meta_ref);
        prop_assert_eq!(result.is_partial, n_item_ref + n_meta_ref > 0);
    }
}

#[test]
fn glob_case_strategy_distribution_is_non_trivial() {
    // Pin the generator so `route_splices_resolver_output_verbatim`
    // actually explores the cases it claims to: include-with-refs and
    // without, excludes present and absent, empty and non-empty resolver
    // output. Each bucket's per-sample probability is >= ~0.2, so over
    // 256 samples a threshold of 5 sits far below any plausible
    // regression (false-positive probability << 1e-11).
    use proptest::strategy::{Strategy, ValueTree};
    let mut runner = proptest::test_runner::TestRunner::default();
    let strategy = glob_case_strategy();
    let mut with_refs = 0;
    let mut without_refs = 0;
    let mut excludes_present = 0;
    let mut excludes_absent = 0;
    let mut out_empty = 0;
    let mut out_nonempty = 0;
    for _ in 0..256 {
        let case = strategy.new_tree(&mut runner).unwrap().current();
        let has_ref = case
            .include_frags
            .iter()
            .any(|f| f.is_item_ref() || f.is_meta_ref());
        if has_ref {
            with_refs += 1;
        } else {
            without_refs += 1;
        }
        if case.exclude_frags.is_empty() {
            excludes_absent += 1;
        } else {
            excludes_present += 1;
        }
        if case.resolver_out.is_empty() {
            out_empty += 1;
        } else {
            out_nonempty += 1;
        }
    }
    for (name, count) in [
        ("include with refs", with_refs),
        ("include without refs", without_refs),
        ("excludes present", excludes_present),
        ("excludes absent", excludes_absent),
        ("empty resolver output", out_empty),
        ("non-empty resolver output", out_nonempty),
    ] {
        assert!(
            count >= 5,
            "generator under-explored '{name}': only {count}/256"
        );
    }
}
