use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::{
    CompileConditionReason, Diagnostic, DiagnosticKind, DiagnosticOrigin, ItemKind,
    ItemMetadataValue, PackageRefOp, PackageReference, PackageReferenceUncertaintyCauseKind,
    ParsedProject, ResolvedItem, parse_fsproj,
};

fn parse(source: &str) -> ParsedProject {
    parse_fsproj(
        source,
        Path::new("/repo/proj/Demo.fsproj"),
        &HashMap::new(),
        &HashMap::new(),
    )
    .expect("well-formed XML parses")
}

/// [`parse`] with a caller-supplied environment snapshot.
fn parse_with_environment(source: &str, environment: &HashMap<String, String>) -> ParsedProject {
    parse_fsproj(
        source,
        Path::new("/repo/proj/Demo.fsproj"),
        &HashMap::new(),
        environment,
    )
    .expect("well-formed XML parses")
}

fn paths(items: &[ResolvedItem]) -> Vec<&Path> {
    items.iter().map(|i| i.include.as_path()).collect()
}

fn file_names(items: &[ResolvedItem]) -> Vec<&str> {
    items
        .iter()
        .map(|i| i.include.file_name().unwrap().to_str().unwrap())
        .collect()
}

fn diag_kinds(diags: &[Diagnostic]) -> Vec<&DiagnosticKind> {
    diags.iter().map(|d| &d.kind).collect()
}

#[test]
fn empty_project_has_no_items() {
    // No Sdk attribute on the root: we'd flag that as
    // UnsupportedConstruct (see
    // `root_project_sdk_attribute_emits_unsupported_construct`).
    // The other tests in this file follow the same convention —
    // they're exercising the body walker, not the SDK shorthand.
    let p = parse("<Project></Project>");
    assert!(p.items.is_empty());
    assert!(p.diagnostics.is_empty());
    assert!(!p.is_partial);
}

#[test]
fn single_compile_resolves_relative_to_project_dir() {
    let src = r#"<Project>
  <ItemGroup>
    <Compile Include="Program.fs" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert_eq!(p.items.len(), 1);
    assert_eq!(p.items[0].kind, ItemKind::Compile);
    assert_eq!(p.items[0].include, PathBuf::from("/repo/proj/Program.fs"));
    assert!(p.items[0].link.is_none());
    assert!(p.diagnostics.is_empty());
}

#[test]
fn multiple_compiles_preserve_document_order() {
    let src = r#"<Project>
  <ItemGroup>
    <Compile Include="A.fs" />
    <Compile Include="B.fs" />
  </ItemGroup>
  <ItemGroup>
    <Compile Include="C.fs" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert_eq!(
        paths(&p.items),
        [
            Path::new("/repo/proj/A.fs"),
            Path::new("/repo/proj/B.fs"),
            Path::new("/repo/proj/C.fs"),
        ]
    );
}

#[test]
fn compile_before_main_after_ordering() {
    // Document order: After, Compile, Before. Output order: Before, Compile, After.
    let src = r#"<Project>
  <ItemGroup>
    <CompileAfter Include="z.fs" />
    <Compile Include="m.fs" />
    <CompileBefore Include="a.fs" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    let names: Vec<_> = p
        .items
        .iter()
        .map(|i| i.include.file_name().unwrap().to_str().unwrap())
        .collect();
    assert_eq!(names, ["a.fs", "m.fs", "z.fs"]);
    assert_eq!(
        p.items.iter().map(|i| i.kind).collect::<Vec<_>>(),
        [
            ItemKind::CompileBefore,
            ItemKind::Compile,
            ItemKind::CompileAfter
        ]
    );
}

#[test]
fn compile_order_metadata_uses_fsharp_source_order() {
    let src = r#"<Project>
  <ItemGroup>
    <Compile Include="Ord.fs" />
    <Compile Include="First.fs" CompileOrder="CompileFirst" />
    <CompileBefore Include="ExplicitBefore.fs" />
    <Compile Include="Before.fs" CompileOrder="CompileBefore" />
    <Compile Include="After.fs">
      <CompileOrder>CompileAfter</CompileOrder>
    </Compile>
    <CompileAfter Include="ExplicitAfter.fs" />
    <Compile Include="Last.fs" CompileOrder="CompileLast" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert_eq!(
        file_names(&p.items),
        [
            "First.fs",
            "ExplicitBefore.fs",
            "Before.fs",
            "Ord.fs",
            "After.fs",
            "ExplicitAfter.fs",
            "Last.fs",
        ]
    );
    assert_eq!(
        p.items.iter().map(|i| i.kind).collect::<Vec<_>>(),
        [
            ItemKind::Compile,
            ItemKind::CompileBefore,
            ItemKind::Compile,
            ItemKind::Compile,
            ItemKind::Compile,
            ItemKind::CompileAfter,
            ItemKind::Compile,
        ]
    );
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    assert!(!p.items_uncertain);
}

#[test]
fn compile_order_child_metadata_overrides_attribute() {
    let src = r#"<Project>
  <ItemGroup>
    <Compile Include="B.fs" />
    <Compile Include="A.fs" CompileOrder="CompileLast">
      <CompileOrder>CompileFirst</CompileOrder>
    </Compile>
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert_eq!(file_names(&p.items), ["A.fs", "B.fs"]);
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    assert!(!p.items_uncertain);
}

#[test]
fn compile_order_metadata_value_is_case_insensitive() {
    let src = r#"<Project>
  <ItemGroup>
    <Compile Include="B.fs" />
    <Compile Include="C.fs" CompileOrder="compilefirst" />
    <Compile Include="A.fs" CompileOrder="compilelast" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert_eq!(file_names(&p.items), ["C.fs", "B.fs", "A.fs"]);
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    assert!(!p.items_uncertain);
}

#[test]
fn compile_order_metadata_name_is_case_insensitive() {
    let src = r#"<Project>
  <ItemGroup>
    <compile Include="Late.fs" compileorder="CompileLast" />
    <Compile Include="Early.fs">
      <compileorder>CompileFirst</compileorder>
    </Compile>
    <Compile Include="Ord.fs" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert_eq!(file_names(&p.items), ["Early.fs", "Ord.fs", "Late.fs"]);
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    assert!(!p.items_uncertain);
}

#[test]
fn compile_update_compile_order_metadata_moves_existing_item() {
    let src = r#"<Project>
  <ItemGroup>
    <Compile Include="A.fs" />
    <Compile Include="B.fs" />
    <Compile Update="B.fs" CompileOrder="CompileFirst" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert_eq!(file_names(&p.items), ["B.fs", "A.fs"]);
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    assert!(!p.items_uncertain);
}

#[test]
fn compile_update_compile_order_keeps_original_order_within_bucket() {
    let src = r#"<Project>
  <ItemGroup>
    <Compile Include="A.fs" />
    <Compile Include="B.fs" CompileOrder="CompileFirst" />
    <Compile Update="A.fs" CompileOrder="CompileFirst" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert_eq!(file_names(&p.items), ["A.fs", "B.fs"]);
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    assert!(!p.items_uncertain);
}

#[test]
fn compile_update_compile_order_can_reinclude_unknown_slot_item() {
    let src = r#"<Project>
  <ItemGroup>
    <Compile Include="A.fs" CompileOrder="SomeUnknownSlot" />
    <Compile Include="B.fs" />
    <Compile Update="A.fs" CompileOrder="CompileLast" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert_eq!(file_names(&p.items), ["B.fs", "A.fs"]);
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    assert!(!p.items_uncertain);
}

#[test]
fn compile_update_compile_order_with_item_reference_marks_items_uncertain() {
    let src = r#"<Project>
  <ItemGroup>
    <Compile Include="A.fs" />
    <Compile Update="@(Compile)" CompileOrder="CompileFirst" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert_eq!(file_names(&p.items), ["A.fs"]);
    assert_eq!(
        diag_kinds(&p.diagnostics),
        [&DiagnosticKind::UnresolvedItemReference {
            reference: "@(Compile)".to_string()
        }]
    );
    assert!(p.items_uncertain);
}

#[test]
fn unknown_compile_order_value_is_excluded_like_fsharp_target() {
    let src = r#"<Project>
  <ItemGroup>
    <Compile Include="Dropped.fs" CompileOrder="SomeUnknownSlot" />
    <Compile Include="Kept.fs" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert_eq!(file_names(&p.items), ["Kept.fs"]);
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    assert!(!p.items_uncertain);
}

#[test]
fn unresolved_compile_order_metadata_marks_items_uncertain_and_skips_item() {
    // `TargetFramework` is a consumer-contract carve-out (never provably
    // unset), so the metadata value stays unresolvable.
    let src = r#"<Project>
  <ItemGroup>
    <Compile Include="A.fs" CompileOrder="$(TargetFramework)" />
    <Compile Include="B.fs" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert_eq!(file_names(&p.items), ["B.fs"]);
    assert_eq!(
        diag_kinds(&p.diagnostics),
        [&DiagnosticKind::UndefinedProperty {
            name: "TargetFramework".to_string()
        }]
    );
    assert!(p.is_partial);
    assert!(p.items_uncertain);
    assert_eq!(p.compile_item_uncertainties.len(), 1);
}

#[test]
fn item_definition_compile_order_default_marks_items_uncertain() {
    let src = r#"<Project>
  <ItemDefinitionGroup>
    <Compile>
      <CompileOrder>CompileFirst</CompileOrder>
    </Compile>
  </ItemDefinitionGroup>
  <ItemGroup>
    <Compile Include="A.fs" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert_eq!(file_names(&p.items), ["A.fs"]);
    assert_eq!(
        diag_kinds(&p.diagnostics),
        [&DiagnosticKind::UnsupportedConstruct {
            element: "ItemDefinitionGroup".to_string()
        }]
    );
    assert!(p.is_partial);
    assert!(p.items_uncertain);
    assert_eq!(p.compile_item_uncertainties.len(), 1);
}

#[test]
fn link_metadata_attribute_form() {
    let src = r#"<Project>
  <ItemGroup>
    <Compile Include="prim-types.fs" Link="Primitives/prim-types.fs" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert_eq!(p.items.len(), 1);
    assert_eq!(p.items[0].link.as_deref(), Some("Primitives/prim-types.fs"));
}

#[test]
fn link_metadata_child_element_form() {
    let src = r#"<Project>
  <ItemGroup>
    <Compile Include="prim-types.fs">
      <Link>Primitives/prim-types.fs</Link>
    </Compile>
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert_eq!(p.items.len(), 1);
    assert_eq!(p.items[0].link.as_deref(), Some("Primitives/prim-types.fs"));
}

#[test]
fn backslashes_in_include_are_normalised() {
    let src = r#"<Project>
  <ItemGroup>
    <Compile Include="sub\nested\file.fs" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert_eq!(
        p.items[0].include,
        PathBuf::from("/repo/proj/sub/nested/file.fs")
    );
}

#[test]
fn import_emits_diagnostic_and_marks_partial() {
    let src = r#"<Project>
  <Import Project="..\common.props" />
  <ItemGroup>
    <Compile Include="A.fs" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert_eq!(p.items.len(), 1);
    assert_eq!(
        diag_kinds(&p.diagnostics),
        [&DiagnosticKind::UnresolvedImport {
            path: "..\\common.props".to_string()
        }]
    );
    assert!(p.is_partial);
}

// --- <Choose>/<When>/<Otherwise>: first-match-wins condition-gated groups ---
//
// Semantics pinned against `dotnet msbuild` (SDK 10.0.300) with per-case
// stub projects, 2026-07-09 (docs/completed/sdk-chain-exactness-plan.md, Stage A):
//   * first matching <When> wins; later <When> conditions are NEVER
//     evaluated (an illegal condition after the match does not error);
//   * a *reached* illegal <When> condition is an evaluation error
//     (MSB4092) — conservative degrade for us;
//   * the branch decision uses the property-pass table at the Choose's
//     document position, and is REUSED by the item pass (an ItemGroup in
//     a chosen branch contributes even if the gating property changes
//     later in the file);
//   * ItemGroups inside the chosen branch still evaluate their own
//     conditions against the FINAL property table, like any ItemGroup;
//   * skipped branches are fully lazy (their contents — including
//     illegal inner conditions — are never looked at, and their writes
//     never land);
//   * <When> without a Condition attribute is a hard MSBuild error
//     (MSB4035) — conservative degrade for us.

#[test]
fn choose_first_match_wins_and_later_conditions_are_not_evaluated() {
    // Pinned: `dotnet msbuild -getProperty:R` = "first" — both for two true
    // Whens and when the second When's condition is MSBuild-illegal.
    let src = r#"<Project>
  <Choose>
    <When Condition="'a' == 'a'">
      <PropertyGroup><R>first</R></PropertyGroup>
    </When>
    <When Condition="!!!garbage!!!">
      <PropertyGroup><R>second</R></PropertyGroup>
    </When>
    <Otherwise>
      <PropertyGroup><R>otherwise</R></PropertyGroup>
    </Otherwise>
  </Choose>
</Project>"#;
    let p = parse(src);
    assert_eq!(p.properties.get("R").map(String::as_str), Some("first"));
    assert!(
        p.diagnostics.is_empty(),
        "the losing When's illegal condition is never evaluated: {:?}",
        p.diagnostics
    );
    assert!(!p.is_partial);
}

#[test]
fn choose_otherwise_runs_when_no_when_matches() {
    let src = r#"<Project>
  <PropertyGroup><P>set</P></PropertyGroup>
  <Choose>
    <When Condition="'$(P)' == 'other'">
      <PropertyGroup><R>when</R></PropertyGroup>
    </When>
    <Otherwise>
      <PropertyGroup><R>otherwise</R></PropertyGroup>
    </Otherwise>
  </Choose>
</Project>"#;
    let p = parse(src);
    assert_eq!(p.properties.get("R").map(String::as_str), Some("otherwise"));
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    assert!(!p.is_partial);
}

#[test]
fn choose_no_match_no_otherwise_is_a_clean_noop() {
    let src = r#"<Project>
  <PropertyGroup><P>set</P></PropertyGroup>
  <Choose>
    <When Condition="'$(P)' == 'other'">
      <PropertyGroup><R>when</R></PropertyGroup>
    </When>
  </Choose>
</Project>"#;
    let p = parse(src);
    assert!(!p.properties.contains_key("R"));
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    assert!(!p.is_partial);
}

#[test]
fn choose_when_decision_uses_pass1_table_at_document_position() {
    // Pinned: the When reads P as it stands at the Choose's position
    // ("early"), not the final value ("late") — `-getProperty:R` = "taken".
    let src = r#"<Project>
  <PropertyGroup><P>early</P></PropertyGroup>
  <Choose>
    <When Condition="'$(P)' == 'early'">
      <PropertyGroup><R>taken</R></PropertyGroup>
    </When>
    <Otherwise>
      <PropertyGroup><R>not-taken</R></PropertyGroup>
    </Otherwise>
  </Choose>
  <PropertyGroup><P>late</P></PropertyGroup>
</Project>"#;
    let p = parse(src);
    assert_eq!(p.properties.get("R").map(String::as_str), Some("taken"));
    assert_eq!(p.properties.get("P").map(String::as_str), Some("late"));
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
}

#[test]
fn choose_pass1_decision_is_reused_by_the_item_pass() {
    // Pinned: the item appears even though the gating property changes
    // after the Choose — the pass-1 branch decision is reused, and the
    // chosen ItemGroup's own condition sees the FINAL table.
    let src = r#"<Project>
  <PropertyGroup><P>early</P></PropertyGroup>
  <Choose>
    <When Condition="'$(P)' == 'early'">
      <ItemGroup>
        <Compile Include="A.fs" />
        <Compile Include="B.fs" Condition="'$(P)' == 'late'" />
        <Compile Include="C.fs" Condition="'$(P)' == 'early'" />
      </ItemGroup>
    </When>
  </Choose>
  <PropertyGroup><P>late</P></PropertyGroup>
</Project>"#;
    let p = parse(src);
    assert_eq!(
        paths(&p.items),
        [Path::new("/repo/proj/A.fs"), Path::new("/repo/proj/B.fs")]
    );
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    assert!(!p.items_uncertain);
}

#[test]
fn choose_skipped_branches_are_fully_lazy() {
    // Pinned: a skipped branch's contents are never evaluated (its illegal
    // inner condition raises no error) and its writes never land.
    let src = r#"<Project>
  <Choose>
    <When Condition="'a' == 'b'">
      <PropertyGroup><P>skipped</P></PropertyGroup>
      <ItemGroup><Compile Include="Bad.fs" Condition="!!!garbage!!!" /></ItemGroup>
    </When>
    <Otherwise>
      <PropertyGroup><Q>other</Q></PropertyGroup>
    </Otherwise>
  </Choose>
</Project>"#;
    let p = parse(src);
    assert!(!p.properties.contains_key("P"));
    assert_eq!(p.properties.get("Q").map(String::as_str), Some("other"));
    assert!(p.items.is_empty());
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    assert!(!p.is_partial);
}

#[test]
fn choose_chosen_branch_write_is_visible_downstream_in_pass1() {
    let src = r#"<Project>
  <Choose>
    <When Condition="'a' == 'a'">
      <PropertyGroup><P>set-in-choose</P></PropertyGroup>
    </When>
  </Choose>
  <PropertyGroup><R>$(P)</R></PropertyGroup>
</Project>"#;
    let p = parse(src);
    assert_eq!(
        p.properties.get("R").map(String::as_str),
        Some("set-in-choose")
    );
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
}

#[test]
fn choose_nests() {
    let src = r#"<Project>
  <Choose>
    <When Condition="'a' == 'a'">
      <Choose>
        <When Condition="'x' == 'y'">
          <PropertyGroup><R>inner-when</R></PropertyGroup>
        </When>
        <Otherwise>
          <PropertyGroup><R>inner-otherwise</R></PropertyGroup>
        </Otherwise>
      </Choose>
    </When>
  </Choose>
</Project>"#;
    let p = parse(src);
    assert_eq!(
        p.properties.get("R").map(String::as_str),
        Some("inner-otherwise")
    );
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
}

#[test]
fn choose_interleaves_item_groups_in_document_order() {
    let src = r#"<Project>
  <ItemGroup><Compile Include="A.fs" /></ItemGroup>
  <Choose>
    <When Condition="'a' == 'a'">
      <ItemGroup><Compile Include="B.fs" /></ItemGroup>
    </When>
  </Choose>
  <ItemGroup><Compile Include="C.fs" /></ItemGroup>
</Project>"#;
    let p = parse(src);
    assert_eq!(
        paths(&p.items),
        [
            Path::new("/repo/proj/A.fs"),
            Path::new("/repo/proj/B.fs"),
            Path::new("/repo/proj/C.fs")
        ]
    );
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
}

#[test]
fn choose_undecidable_when_condition_degrades_conservatively() {
    // `$(TargetFramework)` is a consumer-contract carve-out (never provably
    // unset), so the read surfaces as a divergence risk and the branch
    // decision cannot be pinned. Nothing is descended (branches may or may
    // not run) and the item set is structurally uncertain.
    let src = r#"<Project>
  <Choose>
    <When Condition="'$(TargetFramework)' == 'Bar'">
      <PropertyGroup><R>maybe</R></PropertyGroup>
      <ItemGroup><Compile Include="A.fs" /></ItemGroup>
    </When>
  </Choose>
</Project>"#;
    let p = parse(src);
    assert!(p.items.is_empty());
    assert!(!p.properties.contains_key("R"));
    assert!(
        p.diagnostics.iter().any(|d| matches!(
            &d.kind,
            DiagnosticKind::UndefinedProperty { name } if name == "TargetFramework"
        )),
        "{:?}",
        p.diagnostics
    );
    assert!(p.is_partial);
    assert!(p.items_uncertain);
}

#[test]
fn choose_reached_illegal_when_condition_degrades_conservatively() {
    // Pinned: MSBuild errors (MSB4092) when it *reaches* an illegal When
    // condition — the first When is false, so the second is evaluated. We
    // degrade instead of erroring, and make no claim about the contents.
    let src = r#"<Project>
  <Choose>
    <When Condition="'a' == 'b'">
      <PropertyGroup><R>first</R></PropertyGroup>
    </When>
    <When Condition="!!!garbage!!!">
      <ItemGroup><Compile Include="A.fs" /></ItemGroup>
    </When>
  </Choose>
</Project>"#;
    let p = parse(src);
    assert!(p.items.is_empty());
    assert!(!p.properties.contains_key("R"));
    assert!(
        p.diagnostics
            .iter()
            .any(|d| matches!(&d.kind, DiagnosticKind::UnsupportedCondition { .. })),
        "{:?}",
        p.diagnostics
    );
    assert!(p.is_partial);
    assert!(p.items_uncertain);
}

#[test]
fn choose_bare_when_degrades_conservatively() {
    // Pinned: `<When>` without a Condition attribute is a hard MSBuild
    // error (MSB4035); the whole evaluation fails. Degrade.
    let src = r#"<Project>
  <Choose>
    <When>
      <PropertyGroup><R>bare</R></PropertyGroup>
    </When>
  </Choose>
</Project>"#;
    let p = parse(src);
    assert!(!p.properties.contains_key("R"));
    assert!(p.is_partial);
    assert!(p.items_uncertain);
}

#[test]
fn choose_malformed_shape_degrades_conservatively() {
    // Otherwise-before-When / multiple Otherwise / stray children are all
    // MSBuild schema errors; any of them degrades the whole Choose.
    for src in [
        // Otherwise not last.
        r#"<Project>
  <Choose>
    <Otherwise><PropertyGroup><R>o</R></PropertyGroup></Otherwise>
    <When Condition="'a' == 'a'"><PropertyGroup><R>w</R></PropertyGroup></When>
  </Choose>
</Project>"#,
        // Two Otherwise blocks.
        r#"<Project>
  <Choose>
    <When Condition="'a' == 'b'"><PropertyGroup><R>w</R></PropertyGroup></When>
    <Otherwise><PropertyGroup><R>o1</R></PropertyGroup></Otherwise>
    <Otherwise><PropertyGroup><R>o2</R></PropertyGroup></Otherwise>
  </Choose>
</Project>"#,
        // A child that is neither When nor Otherwise.
        r#"<Project>
  <Choose>
    <PropertyGroup><R>stray</R></PropertyGroup>
    <When Condition="'a' == 'a'"><PropertyGroup><R>w</R></PropertyGroup></When>
  </Choose>
</Project>"#,
        // No When at all.
        r#"<Project>
  <Choose>
    <Otherwise><PropertyGroup><R>o</R></PropertyGroup></Otherwise>
  </Choose>
</Project>"#,
        // A schema-illegal element inside a branch is an MSBuild *load*
        // error (MSB4067, pinned) — even in a branch that would never be
        // chosen, and even after earlier legal children of a chosen
        // branch. Nothing from the project evaluates in a real build, so
        // nothing from the Choose may land here.
        r#"<Project>
  <Choose>
    <When Condition="'a' == 'a'"><PropertyGroup><R>chosen</R></PropertyGroup></When>
    <When Condition="'a' == 'b'"><Foo /></When>
  </Choose>
</Project>"#,
        r#"<Project>
  <Choose>
    <When Condition="'a' == 'a'">
      <PropertyGroup><R>written</R></PropertyGroup>
      <Foo />
    </When>
  </Choose>
</Project>"#,
        // A malformed *nested* Choose is just as much a load error.
        r#"<Project>
  <Choose>
    <When Condition="'a' == 'a'">
      <PropertyGroup><R>written</R></PropertyGroup>
      <Choose><Otherwise><PropertyGroup><Q>o</Q></PropertyGroup></Otherwise></Choose>
    </When>
  </Choose>
</Project>"#,
        // MSB4035 is "empty or missing": an empty Condition is an error.
        r#"<Project>
  <Choose>
    <When Condition=""><PropertyGroup><R>w</R></PropertyGroup></When>
  </Choose>
</Project>"#,
        // `Condition` is an unrecognized attribute on <Otherwise> and on
        // <Choose> itself (MSB4066, pinned) — a load error, not a gate.
        r#"<Project>
  <Choose>
    <When Condition="'a' == 'b'"><PropertyGroup><R>w</R></PropertyGroup></When>
    <Otherwise Condition="'a' == 'a'"><PropertyGroup><R>o</R></PropertyGroup></Otherwise>
  </Choose>
</Project>"#,
        r#"<Project>
  <Choose Condition="'a' == 'a'">
    <When Condition="'a' == 'a'"><PropertyGroup><R>w</R></PropertyGroup></When>
  </Choose>
</Project>"#,
        // Any other unrecognized attribute is MSB4066 just the same —
        // and <Choose>/<Otherwise> recognize no attributes at all, not
        // even Label (both pinned).
        r#"<Project>
  <Choose>
    <When Condition="'a' == 'a'" Exclude="x"><PropertyGroup><R>w</R></PropertyGroup></When>
  </Choose>
</Project>"#,
        r#"<Project>
  <Choose Label="lbl">
    <When Condition="'a' == 'a'"><PropertyGroup><R>w</R></PropertyGroup></When>
  </Choose>
</Project>"#,
        r#"<Project>
  <Choose>
    <When Condition="'a' == 'b'"><PropertyGroup><R>w</R></PropertyGroup></When>
    <Otherwise Label="lbl"><PropertyGroup><R>o</R></PropertyGroup></Otherwise>
  </Choose>
</Project>"#,
    ] {
        let p = parse(src);
        assert!(
            !p.properties.contains_key("R"),
            "malformed Choose must not evaluate: {src}"
        );
        assert!(p.is_partial, "malformed Choose must degrade: {src}");
    }
}

#[test]
fn choose_undecidable_gate_over_nested_define_write_is_define_uncertain() {
    // The DefineConstants write sits inside a *nested* Choose, but the
    // undecidable OUTER gate (reading carved-out, never-provably-unset
    // `TargetFramework`) is what decides whether it ever runs — the
    // preprocessor symbol set must be flagged untrustworthy exactly as it
    // would be for a direct `<PropertyGroup><DefineConstants>` child.
    let src = r#"<Project>
  <Choose>
    <When Condition="'$(TargetFramework)' == 'Bar'">
      <Choose>
        <When Condition="'a' == 'a'">
          <PropertyGroup><DefineConstants>CUSTOM</DefineConstants></PropertyGroup>
        </When>
      </Choose>
    </When>
  </Choose>
</Project>"#;
    let p = parse(src);
    assert!(!p.define_constants.contains(&"CUSTOM".to_string()));
    assert!(
        p.define_constants_uncertain,
        "a maybe-run nested DefineConstants write must not leave the \
         symbol set trusted; diags: {:?}",
        p.diagnostics
    );
}

#[test]
fn choose_undecidable_gate_over_nested_cpm_flag_is_package_uncertain() {
    // The CPM analogue of the nested-define case: the flag write is
    // nested, the outer gate is undecidable (carved-out `TargetFramework`
    // is never provably unset), so whether Central Package Management
    // turns on is unknowable and the dependency set degrades.
    let src = r#"<Project>
  <Choose>
    <When Condition="'$(TargetFramework)' == 'Bar'">
      <Choose>
        <When Condition="'a' == 'a'">
          <PropertyGroup><ManagePackageVersionsCentrally>true</ManagePackageVersionsCentrally></PropertyGroup>
        </When>
      </Choose>
    </When>
  </Choose>
  <ItemGroup>
    <PackageReference Include="Newtonsoft.Json" Version="13.0.3" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert!(
        p.package_references_uncertain,
        "a maybe-run nested CPM flag write must degrade the dependency \
         set; causes: {:?}",
        p.package_reference_uncertainties
    );
}

#[test]
fn choose_label_attribute_is_tolerated() {
    // `Label` is a recognized attribute on <When> (pinned) — it must not
    // trip the malformed-shape degrade.
    let src = r#"<Project>
  <Choose>
    <When Condition="'a' == 'a'" Label="lbl">
      <PropertyGroup><R>w</R></PropertyGroup>
    </When>
  </Choose>
</Project>"#;
    let p = parse(src);
    assert_eq!(p.properties.get("R").map(String::as_str), Some("w"));
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
}

#[test]
fn choose_dead_branch_writes_do_not_taint_later_gates() {
    // The CPM/define write sits in a branch that is already *cleanly
    // false* by the time the undecidable gate (reading carved-out
    // `TargetFramework`) is reached — MSBuild can never run it, so it must
    // not make the later gate package- or preprocessor-affecting.
    let src = r#"<Project>
  <Choose>
    <When Condition="'a' == 'b'">
      <PropertyGroup>
        <ManagePackageVersionsCentrally>true</ManagePackageVersionsCentrally>
        <DefineConstants>DEAD</DefineConstants>
      </PropertyGroup>
    </When>
    <When Condition="'$(TargetFramework)' == 'Bar'">
      <PropertyGroup><Unrelated>x</Unrelated></PropertyGroup>
    </When>
  </Choose>
  <ItemGroup>
    <PackageReference Include="Newtonsoft.Json" Version="13.0.3" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert!(
        !p.package_references_uncertain,
        "a dead branch's CPM write cannot poison the dependency set; \
         causes: {:?}",
        p.package_reference_uncertainties
    );
    assert!(
        !p.define_constants_uncertain,
        "a dead branch's DefineConstants write cannot poison the symbol \
         set; diags: {:?}",
        p.diagnostics
    );
    // The undecidable gate itself still marks the evaluation partial.
    assert!(p.is_partial);
}

#[test]
fn choose_chosen_branch_captures_package_references_certainly() {
    let src = r#"<Project>
  <Choose>
    <When Condition="'a' == 'a'">
      <ItemGroup>
        <PackageReference Include="Newtonsoft.Json" Version="13.0.3" />
      </ItemGroup>
    </When>
    <Otherwise>
      <ItemGroup>
        <PackageReference Include="Wrong.Package" Version="1.0.0" />
      </ItemGroup>
    </Otherwise>
  </Choose>
</Project>"#;
    let p = parse(src);
    assert_eq!(p.package_references.len(), 1);
    assert_eq!(p.package_references[0].id, "Newtonsoft.Json");
    assert_eq!(p.package_references[0].version.as_deref(), Some("13.0.3"));
    assert!(
        !p.package_references_uncertain,
        "a cleanly-decided Choose leaves the dependency set certain: {:?}",
        p.package_reference_uncertainties
    );
}

#[test]
fn glob_in_include_emits_diagnostic_and_skips_item() {
    let src = r#"<Project>
  <ItemGroup>
    <Compile Include="src/**/*.fs" />
    <Compile Include="Real.fs" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert_eq!(paths(&p.items), [Path::new("/repo/proj/Real.fs")]);
    assert_eq!(
        diag_kinds(&p.diagnostics),
        [&DiagnosticKind::UnsupportedGlob {
            pattern: "src/**/*.fs".to_string()
        }]
    );
    assert!(p.is_partial);
}

#[test]
fn inexact_property_in_include_emits_diagnostic_and_skips_item() {
    // Phase 2 *attempts* substitution; `TargetFramework` is a
    // consumer-contract carve-out (never provably unset), so the failure
    // leaves the resulting path malformed (the unknown value cannot be
    // substituted), and we refuse to emit a corrupt item alongside the
    // diagnostic.
    let src = r#"<Project>
  <ItemGroup>
    <Compile Include="$(TargetFramework)Generated.fs" />
    <Compile Include="Real.fs" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert_eq!(paths(&p.items), [Path::new("/repo/proj/Real.fs")]);
    assert_eq!(
        diag_kinds(&p.diagnostics),
        [&DiagnosticKind::UndefinedProperty {
            name: "TargetFramework".to_string()
        }]
    );
    assert!(p.is_partial);
}

#[test]
fn item_group_with_evaluable_false_condition_silently_excludes_items_when_props_known() {
    // Phase 3 evaluates conditions. `'$(Configuration)' == 'Debug'` with
    // Configuration *supplied as a different value* reduces to a
    // successful exclusion — no diagnostic needed, exactly what MSBuild
    // would have produced.
    let src = r#"<Project>
  <ItemGroup Condition="'$(Configuration)' == 'Debug'">
    <Compile Include="DebugOnly.fs" />
  </ItemGroup>
</Project>"#;
    let p = parse_with(src, &[("Configuration", "Release")]);
    assert!(p.items.is_empty());
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    assert!(!p.is_partial);
}

#[test]
fn item_group_condition_referencing_inexact_property_excludes_and_diagnoses() {
    // Phase 3: when a condition references a carved-out property
    // (`TargetFramework` is never provably unset — MSBuild might know it
    // from a source we don't follow), we still compute a truth value by
    // treating it as "" (MSBuild semantics for unset properties). Emit
    // UndefinedProperty so the project is marked partial — the gate
    // fired, but the user should know we picked a branch under
    // uncertainty.
    let src = r#"<Project>
  <ItemGroup Condition="'$(TargetFramework)' == 'Debug'">
    <Compile Include="DebugOnly.fs" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert!(p.items.is_empty(), "{:?}", p.items);
    assert_eq!(
        diag_kinds(&p.diagnostics),
        [&DiagnosticKind::UndefinedProperty {
            name: "TargetFramework".to_string()
        }]
    );
    assert!(p.is_partial);
}

#[test]
fn item_group_with_evaluable_true_condition_includes_items() {
    let src = r#"<Project>
  <ItemGroup Condition="'$(Configuration)' == 'Debug'">
    <Compile Include="DebugOnly.fs" />
  </ItemGroup>
</Project>"#;
    let p = parse_with(src, &[("Configuration", "Debug")]);
    assert_eq!(paths(&p.items), [Path::new("/repo/proj/DebugOnly.fs")]);
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    assert!(!p.is_partial);
}

#[test]
fn item_group_with_unsupported_condition_excludes_items_and_diagnoses() {
    // Plan D5: when we can't evaluate the condition we must NOT proceed
    // as if it were true — the group is treated as excluded, with an
    // UnsupportedCondition diagnostic so callers know the result may
    // diverge from MSBuild.
    let src = r#"<Project>
  <ItemGroup Condition="Exists('foo')">
    <Compile Include="DebugOnly.fs" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert!(
        p.items.is_empty(),
        "items leaked through unsupported condition: {:?}",
        p.items
    );
    assert_eq!(
        diag_kinds(&p.diagnostics),
        [&DiagnosticKind::UnsupportedCondition {
            condition: "Exists('foo')".to_string()
        }]
    );
    assert!(p.is_partial);
}

#[test]
fn item_level_evaluable_false_condition_silently_excludes_when_props_known() {
    let src = r#"<Project>
  <ItemGroup>
    <Compile Include="MaybeDebug.fs" Condition="'$(Configuration)' == 'Debug'" />
  </ItemGroup>
</Project>"#;
    let p = parse_with(src, &[("Configuration", "Release")]);
    assert!(p.items.is_empty());
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    assert!(!p.is_partial);
}

#[test]
fn item_level_condition_referencing_inexact_property_excludes_and_diagnoses() {
    // Same reasoning as the ItemGroup-level test: the gate fired
    // (`'' == 'Debug'` is false, so the item is excluded), but the
    // user must know we picked the branch on a carved-out property
    // (`TargetFramework` is never provably unset).
    let src = r#"<Project>
  <ItemGroup>
    <Compile Include="MaybeDebug.fs" Condition="'$(TargetFramework)' == 'Debug'" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert!(p.items.is_empty());
    assert_eq!(
        diag_kinds(&p.diagnostics),
        [&DiagnosticKind::UndefinedProperty {
            name: "TargetFramework".to_string()
        }]
    );
    assert!(p.is_partial);
}

#[test]
fn item_level_unsupported_condition_excludes_and_diagnoses() {
    let src = r#"<Project>
  <ItemGroup>
    <Compile Include="MaybeDebug.fs" Condition="Exists('MaybeDebug.fs')" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert!(p.items.is_empty());
    assert!(matches!(
        p.diagnostics[0].kind,
        DiagnosticKind::UnsupportedCondition { .. }
    ));
    assert!(p.is_partial);
}

#[test]
fn both_sides_undefined_condition_evaluates_exactly_true() {
    // A condition where both sides expand to "" because both
    // properties are absent from our map. The walk is clean and the
    // caller's environment snapshot is empty, so both names are
    // *provably* undefined in the real build too — MSBuild would
    // expand them to "" as well and take the same `'' == ''` = true
    // branch. The read is exact: items included, no diagnostics, not
    // partial.
    let src = r#"<Project>
  <ItemGroup Condition="'$(FcsTargetNetCoreFramework)' == '$(FcsTargetNetFxFramework)'">
    <Compile Include="Pinned.fs" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert_eq!(paths(&p.items), [Path::new("/repo/proj/Pinned.fs")]);
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    assert!(!p.is_partial);
}

#[test]
fn item_list_reference_inside_condition_string_excludes_and_diagnoses() {
    // Quoted `@(Items)` in a condition is not a `$(...)` expansion,
    // so `substitute` leaves it intact. Without the post-substitution
    // scan added in `expand_for_condition`, the `Compare` would treat
    // it as the literal text `@(Compile)` and silently resolve to
    // true (here), letting the item through with no diagnostic. Plan
    // D5 mandates the opposite: poison the whole condition and skip
    // the construct.
    let src = r#"<Project>
  <ItemGroup>
    <Compile Include="Maybe.fs" Condition="'@(Compile)' != ''" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert!(
        p.items.is_empty(),
        "items leaked through item-list-referencing condition: {:?}",
        p.items
    );
    assert!(matches!(
        p.diagnostics[0].kind,
        DiagnosticKind::UnsupportedCondition { .. }
    ));
    assert!(p.is_partial);
}

#[test]
fn compile_update_with_include_emits_diagnostic_and_skips_item() {
    let src = r#"<Project>
  <ItemGroup>
    <Compile Include="A.fs" Update="Existing.fs">
      <CopyToOutputDirectory>Always</CopyToOutputDirectory>
    </Compile>
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert!(p.items.is_empty());
    assert!(matches!(
        p.diagnostics[0].kind,
        DiagnosticKind::UnsupportedItemOperation { .. }
    ));
}

#[test]
fn compile_remove_emits_diagnostic_and_skips_item() {
    let src = r#"<Project>
  <ItemGroup>
    <Compile Remove="Skip.fs" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert!(p.items.is_empty());
    assert!(matches!(
        p.diagnostics[0].kind,
        DiagnosticKind::UnsupportedItemOperation { .. }
    ));
}

#[test]
fn irrelevant_item_kinds_are_silently_ignored() {
    let src = r#"<Project>
  <ItemGroup>
    <PackageReference Include="FSharp.Core" Version="8.0.0" />
    <ProjectReference Include="..\Other\Other.fsproj" />
    <EmbeddedResource Include="Resources.resx" />
    <None Include="README.md" />
    <Compile Include="A.fs" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert_eq!(paths(&p.items), [Path::new("/repo/proj/A.fs")]);
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    assert!(!p.is_partial);
}

#[test]
fn property_group_writes_populate_the_properties_map() {
    let src = r#"<Project>
  <PropertyGroup>
    <OutputType>Exe</OutputType>
    <TargetFramework>net8.0</TargetFramework>
  </PropertyGroup>
  <ItemGroup>
    <Compile Include="Program.fs" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert_eq!(p.items.len(), 1);
    assert_eq!(
        p.properties.get("OutputType").map(String::as_str),
        Some("Exe")
    );
    assert_eq!(
        p.properties.get("TargetFramework").map(String::as_str),
        Some("net8.0")
    );
    // Well-known properties are seeded for substitution but excluded
    // from the project-defined map exposed to callers.
    assert!(!p.properties.contains_key("MSBuildProjectName"));
    assert!(p.diagnostics.is_empty());
}

#[test]
fn span_covers_the_compile_element() {
    let src = "<Project><ItemGroup><Compile Include=\"A.fs\" /></ItemGroup></Project>";
    let p = parse(src);
    let span = p.items[0].span.clone();
    let element_text = &src[span.clone()];
    assert!(
        element_text.starts_with("<Compile"),
        "span start should anchor at element open: got {element_text:?} for {span:?}"
    );
    assert!(
        element_text.ends_with("/>") || element_text.ends_with("</Compile>"),
        "span end should cover element close: got {element_text:?}"
    );
}

#[test]
fn malformed_xml_returns_error() {
    let err = parse_fsproj(
        "<Project><ItemGroup><Compile Include=\"A.fs\" /></Project>",
        Path::new("/repo/proj/Demo.fsproj"),
        &HashMap::new(),
        &HashMap::new(),
    )
    .expect_err("unbalanced tags should fail");
    // Just check it's our XML variant; the inner message comes from roxmltree.
    let s = err.to_string();
    assert!(s.starts_with("malformed XML:"), "got {s}");
}

#[test]
fn semicolon_list_in_include_splits_into_items() {
    let src = r#"<Project>
  <ItemGroup>
    <Compile Include="A.fs;B.fs ; C.fs" Link="Shared" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert_eq!(
        paths(&p.items),
        [
            Path::new("/repo/proj/A.fs"),
            Path::new("/repo/proj/B.fs"),
            Path::new("/repo/proj/C.fs"),
        ]
    );
    // Link metadata applies to every entry.
    assert!(p.items.iter().all(|i| i.link.as_deref() == Some("Shared")));
    assert!(p.diagnostics.is_empty());
}

#[test]
fn semicolon_list_diagnoses_per_post_substitution_entry() {
    // Post-substitution diagnostics (glob, item-list ref) are reported
    // per surviving entry; literal entries on either side still resolve.
    let src = r#"<Project>
  <ItemGroup>
    <Compile Include="ok.fs;**/*.fs;@(Other)" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert_eq!(paths(&p.items), [Path::new("/repo/proj/ok.fs")]);
    assert_eq!(
        diag_kinds(&p.diagnostics),
        [
            &DiagnosticKind::UnsupportedGlob {
                pattern: "**/*.fs".to_string()
            },
            &DiagnosticKind::UnresolvedItemReference {
                reference: "@(Other)".to_string()
            },
        ]
    );
}

#[test]
fn substitution_failure_anywhere_in_include_skips_the_whole_attribute() {
    // We substitute the whole Include attribute *before* splitting on
    // `;`, because a property value is allowed to be a list. That means
    // an inexact property read mid-list (`TargetFramework` is carved out,
    // never provably unset) corrupts the whole expansion (would leave
    // empty fragments / unaligned semicolons), so we drop every entry
    // from the attribute rather than keep some-and-not-others based on
    // luck of where the broken reference happened to sit.
    let src = r#"<Project>
  <ItemGroup>
    <Compile Include="ok.fs;$(TargetFramework).fs" />
    <Compile Include="Real.fs" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert_eq!(paths(&p.items), [Path::new("/repo/proj/Real.fs")]);
    assert_eq!(
        diag_kinds(&p.diagnostics),
        [&DiagnosticKind::UndefinedProperty {
            name: "TargetFramework".to_string()
        }]
    );
}

#[test]
fn compile_exclude_emits_diagnostic_and_skips_item() {
    // `Exclude` subtracts paths from the Include list. We can't replicate
    // that without globbing, so refuse the whole item rather than emit a
    // list MSBuild would filter further.
    let src = r#"<Project>
  <ItemGroup>
    <Compile Include="A.fs;B.fs" Exclude="B.fs" />
    <Compile Include="C.fs" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert_eq!(paths(&p.items), [Path::new("/repo/proj/C.fs")]);
    assert!(matches!(
        p.diagnostics[0].kind,
        DiagnosticKind::UnsupportedItemOperation { .. }
    ));
    assert!(p.is_partial);
}

#[test]
fn item_reference_in_include_emits_diagnostic_and_skips_entry() {
    let src = r#"<Project>
  <ItemGroup>
    <Compile Include="@(GeneratedCompile);real.fs" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert_eq!(paths(&p.items), [Path::new("/repo/proj/real.fs")]);
    assert_eq!(
        diag_kinds(&p.diagnostics),
        [&DiagnosticKind::UnresolvedItemReference {
            reference: "@(GeneratedCompile)".to_string()
        }]
    );
    assert!(p.is_partial);
}

#[test]
fn metadata_reference_in_include_emits_diagnostic_and_skips_entry() {
    let src = r#"<Project>
  <ItemGroup>
    <Compile Include="%(Filename).fs;real.fs" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert_eq!(paths(&p.items), [Path::new("/repo/proj/real.fs")]);
    assert_eq!(
        diag_kinds(&p.diagnostics),
        [&DiagnosticKind::UnresolvedMetadataReference {
            reference: "%(Filename).fs".to_string()
        }]
    );
    assert!(p.is_partial);
}

#[test]
fn import_group_flags_each_nested_import() {
    // ImportGroup with no condition (or one we evaluate true) — every
    // Import inside still routes through the per-import handler and
    // produces an UnresolvedImport in the pure walker.
    let src = r#"<Project>
  <ImportGroup>
    <Import Project="..\one.props" />
    <Import Project="..\two.props" />
  </ImportGroup>
  <ItemGroup>
    <Compile Include="A.fs" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert_eq!(paths(&p.items), [Path::new("/repo/proj/A.fs")]);
    assert_eq!(
        diag_kinds(&p.diagnostics),
        [
            &DiagnosticKind::UnresolvedImport {
                path: "..\\one.props".to_string()
            },
            &DiagnosticKind::UnresolvedImport {
                path: "..\\two.props".to_string()
            },
        ]
    );
    assert!(p.is_partial);
}

#[test]
fn import_group_condition_false_skips_nested_imports() {
    // An ImportGroup whose condition evaluates to false skips every
    // Import inside — proceeding as if true would silently introduce
    // UnresolvedImport diagnostics for files MSBuild itself would have
    // ignored. The condition references `Configuration`, which is in
    // the protected set seeded from `extra_properties` below, so it
    // resolves to a known value and the condition evaluator returns
    // False without emitting any UndefinedProperty noise.
    let src = r#"<Project>
  <ImportGroup Condition="'$(Configuration)' == 'Release'">
    <Import Project="..\one.props" />
    <Import Project="..\two.props" />
  </ImportGroup>
  <ItemGroup>
    <Compile Include="A.fs" />
  </ItemGroup>
</Project>"#;
    let mut extras = std::collections::HashMap::new();
    extras.insert("Configuration".to_string(), "Debug".to_string());
    let p = parse_fsproj(
        src,
        Path::new("/repo/proj/Demo.fsproj"),
        &extras,
        &HashMap::new(),
    )
    .unwrap();
    assert_eq!(paths(&p.items), [Path::new("/repo/proj/A.fs")]);
    assert!(
        p.diagnostics.is_empty(),
        "expected no diagnostics, got {:?}",
        p.diagnostics
    );
    assert!(!p.is_partial);
}

#[test]
fn import_group_unsupported_condition_skips_and_flags() {
    // An ImportGroup whose condition uses an unsupported construct is
    // exclusionary: we never silently include the inner Imports. The
    // diagnostic carries the raw condition text so the user knows
    // *why* the inner imports were skipped.
    let src = r#"<Project>
  <ImportGroup Condition="Exists('foo.props')">
    <Import Project="..\one.props" />
  </ImportGroup>
</Project>"#;
    let p = parse(src);
    assert_eq!(
        diag_kinds(&p.diagnostics),
        [&DiagnosticKind::UnsupportedCondition {
            condition: "Exists('foo.props')".to_string()
        }]
    );
    assert!(p.is_partial);
}

#[test]
fn root_project_sdk_attribute_emits_unsupported_construct() {
    // A `<Project Sdk="Microsoft.NET.Sdk">` is shorthand for two
    // implicit SDK imports the pure walker can't follow. Silently
    // ignoring the attribute would let the walker return an
    // incomplete item list with `is_partial == false`; instead we
    // emit an UnsupportedConstruct so callers know defaults are
    // missing.
    let src = r#"<Project Sdk="Microsoft.NET.Sdk">
  <ItemGroup>
    <Compile Include="A.fs" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert!(
        p.diagnostics.iter().any(|d| matches!(
            &d.kind,
            DiagnosticKind::UnsupportedConstruct { element }
                if element == "Project Sdk=\"Microsoft.NET.Sdk\""
        )),
        "expected UnsupportedConstruct for root Sdk, got: {:?}",
        p.diagnostics
    );
    assert!(p.is_partial);
}

#[test]
fn import_group_with_unknown_child_flags_unsupported_construct() {
    let src = r#"<Project>
  <ImportGroup>
    <Whatever />
  </ImportGroup>
</Project>"#;
    let p = parse(src);
    assert_eq!(
        diag_kinds(&p.diagnostics),
        [&DiagnosticKind::UnsupportedConstruct {
            element: "Whatever".to_string()
        }]
    );
    assert!(p.is_partial);
}

#[test]
fn relative_project_path_is_rejected() {
    // A path without a filesystem root would let
    // `$(MSBuildProjectDirectory)/A.fs` expand into a path with the
    // project_dir component appearing twice after the final
    // `project_dir.join(...)`. Rather than silently emit corrupt
    // items, refuse the input — real callers (LSP, build tools) always
    // have a rooted path on hand. We check `has_root` rather than
    // `is_absolute` so rooted-but-not-drive-qualified Windows paths
    // (`/foo`) are still accepted; see `parse_fsproj`.
    let err = parse_fsproj(
        "<Project><ItemGroup><Compile Include=\"A.fs\" /></ItemGroup></Project>",
        Path::new("proj/Demo.fsproj"),
        &HashMap::new(),
        &HashMap::new(),
    )
    .expect_err("relative project_path should be rejected");
    let s = err.to_string();
    assert!(
        s.contains("project_path") && s.contains("rooted"),
        "got {s}"
    );
    // The bare-filename case has no parent at all — same rejection,
    // because there's no directory we could resolve relative includes
    // against in any consistent way.
    let err2 = parse_fsproj(
        "<Project/>",
        Path::new("Demo.fsproj"),
        &HashMap::new(),
        &HashMap::new(),
    )
    .expect_err("bare-filename project_path should be rejected");
    assert!(err2.to_string().contains("rooted"));
}

// -- Phase 2: property substitution -----------------------------------

fn parse_with(source: &str, extras: &[(&str, &str)]) -> ParsedProject {
    let map: HashMap<String, String> = extras
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect();
    parse_fsproj(
        source,
        Path::new("/repo/proj/Demo.fsproj"),
        &map,
        &HashMap::new(),
    )
    .expect("well-formed XML parses")
}

#[test]
fn property_defined_in_propertygroup_expands_in_later_include() {
    let src = r#"<Project>
  <PropertyGroup>
    <SourceRoot>src</SourceRoot>
  </PropertyGroup>
  <ItemGroup>
    <Compile Include="$(SourceRoot)/Lib.fs" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert_eq!(paths(&p.items), [Path::new("/repo/proj/src/Lib.fs")]);
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
}

#[test]
fn property_forward_reference_to_later_propertygroup_reads_exactly_empty() {
    // The property pass evaluates in document order: a property VALUE
    // referencing a name defined later in the file reads empty. Real
    // MSBuild does exactly the same (document order is its semantics
    // too), and with an empty environment snapshot the name is provably
    // unset at the read point — so the read is exact: no diagnostic.
    // (Items are different — they evaluate in a later pass against the
    // FINAL property table; see the pass-ordering tests at the bottom
    // of this file.)
    let src = r#"<Project>
  <PropertyGroup>
    <Early>$(SourceRoot)/Early.fs</Early>
  </PropertyGroup>
  <PropertyGroup>
    <SourceRoot>src</SourceRoot>
  </PropertyGroup>
</Project>"#;
    let p = parse(src);
    assert_eq!(
        p.properties.get("Early").map(String::as_str),
        Some("/Early.fs")
    );
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
}

#[test]
fn self_reference_uses_prior_value() {
    // <NoWarn>$(NoWarn);75</NoWarn> is the canonical MSBuild idiom for
    // append-to-a-list. Phase 2 must use the *prior* binding of NoWarn
    // when evaluating the new value — otherwise this would either
    // infinite-loop or report NoWarn as undefined.
    let src = r#"<Project>
  <PropertyGroup>
    <NoWarn>40;52</NoWarn>
    <NoWarn>$(NoWarn);75</NoWarn>
  </PropertyGroup>
</Project>"#;
    let p = parse(src);
    assert_eq!(
        p.properties.get("NoWarn").map(String::as_str),
        Some("40;52;75")
    );
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
}

#[test]
fn self_reference_on_first_definition_reads_exactly_empty() {
    // No prior binding exists, so $(Foo) on the right-hand side of the
    // first Foo definition is genuinely undefined — and with a clean
    // walk and an empty environment snapshot that is *provable*, so
    // the read is exactly "" (MSBuild's expansion) with no diagnostic.
    let src = r#"<Project>
  <PropertyGroup>
    <Foo>$(Foo);initial</Foo>
  </PropertyGroup>
</Project>"#;
    let p = parse(src);
    assert_eq!(
        p.properties.get("Foo").map(String::as_str),
        Some(";initial")
    );
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
}

#[test]
fn properties_chain_through_each_other() {
    let src = r#"<Project>
  <PropertyGroup>
    <Base>libs</Base>
    <Sub>$(Base)/core</Sub>
  </PropertyGroup>
  <ItemGroup>
    <Compile Include="$(Sub)/Mod.fs" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert_eq!(
        p.properties.get("Sub").map(String::as_str),
        Some("libs/core")
    );
    assert_eq!(paths(&p.items), [Path::new("/repo/proj/libs/core/Mod.fs")]);
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
}

#[test]
fn extra_properties_are_visible_to_substitution() {
    let src = r#"<Project>
  <ItemGroup>
    <Compile Include="$(Configuration)/Gen.fs" />
  </ItemGroup>
</Project>"#;
    let p = parse_with(src, &[("Configuration", "Release")]);
    assert_eq!(paths(&p.items), [Path::new("/repo/proj/Release/Gen.fs")]);
    assert!(p.diagnostics.is_empty());
    // extras are inputs, not project-defined output.
    assert!(!p.properties.contains_key("Configuration"));
}

#[test]
fn an_escape_in_a_caller_global_is_live_not_literal() {
    // A caller global is *not* evaluator-computed text: MSBuild unescapes global
    // property values on the way in (`dotnet msbuild -p:G=a%20b` makes `$(G)`
    // the string `a b` — probed against 10.0.301, 2026-07-11). So an escape in
    // one is live, exactly as in project XML — the global enters the escaped
    // domain verbatim, and the item identity decodes it at the leaf. Contrast
    // the evaluator's *own* path seeds, which are escaped on the way in, so a
    // `%` in them is inert (`escape_from_a_trusted_path_property_is_literal`).
    //
    // This used to degrade, because raw text was all we had; now the item is
    // captured with the identity MSBuild gives it.
    let src = r#"<Project>
  <ItemGroup>
    <Compile Include="$(G)/Gen.fs" />
  </ItemGroup>
</Project>"#;
    let p = parse_with(src, &[("G", "a%20b")]);
    assert!(
        p.diagnostics.is_empty(),
        "a live escape is modelled, not degraded: {:?}",
        p.diagnostics
    );
    assert_eq!(paths(&p.items), vec!["/repo/proj/a b/Gen.fs"]);
}

#[test]
fn extra_properties_cannot_be_overridden_by_propertygroup() {
    // MSBuild treats command-line / API-supplied properties as globals
    // that the project file cannot rebind. We mirror that: a
    // <Configuration>Debug</Configuration> in the file is silently
    // ignored when the caller already supplied Configuration=Release.
    let src = r#"<Project>
  <PropertyGroup>
    <Configuration>Debug</Configuration>
  </PropertyGroup>
  <ItemGroup>
    <Compile Include="$(Configuration)/Gen.fs" />
  </ItemGroup>
</Project>"#;
    let p = parse_with(src, &[("Configuration", "Release")]);
    assert_eq!(paths(&p.items), [Path::new("/repo/proj/Release/Gen.fs")]);
    assert!(!p.properties.contains_key("Configuration"));
}

#[test]
fn well_known_properties_cannot_be_overridden_by_propertygroup() {
    // Reserved properties are read-only in real MSBuild; project-side
    // writes to MSBuildProjectName must not shadow the path-derived
    // value used by every other substitution.
    let src = r#"<Project>
  <PropertyGroup>
    <MSBuildProjectName>OtherName</MSBuildProjectName>
  </PropertyGroup>
  <ItemGroup>
    <Compile Include="$(MSBuildProjectName).fs" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert_eq!(paths(&p.items), [Path::new("/repo/proj/Demo.fs")]);
    assert!(!p.properties.contains_key("MSBuildProjectName"));
}

#[test]
fn well_known_msbuild_project_seeds_are_derived_from_project_path() {
    // Each Include happens to substitute to an absolute path
    // (MSBuild's project-directory properties resolve to one), so
    // `project_dir.join(absolute)` collapses to just the absolute
    // path — that's documented PathBuf::join behaviour and exactly
    // what we want here.
    let src = r#"<Project>
  <ItemGroup>
    <Compile Include="$(MSBuildProjectDirectory)/$(MSBuildProjectName)$(MSBuildProjectExtension).bak" />
    <Compile Include="$(MSBuildThisFileDirectory)$(MSBuildThisFile)" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert_eq!(
        paths(&p.items),
        [
            Path::new("/repo/proj/Demo.fsproj.bak"),
            // MSBuildThisFileDirectory carries a trailing separator
            // (the spec distinguishes it from MSBuildProjectDirectory
            // on this point), so the joined result has no doubled `/`.
            Path::new("/repo/proj/Demo.fsproj"),
        ]
    );
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
}

#[test]
fn self_reference_outside_empty_comparison_evaluates_exactly() {
    // With an empty environment snapshot and a clean walk, Foo is
    // *provably* unset at the gate, so `'$(Foo)' != 'bar'` is exactly
    // `'' != 'bar'` = true — the same branch the real build takes.
    // The write runs, and no undefined-property signal is needed.
    let src = r#"<Project>
  <PropertyGroup>
    <Foo Condition="'$(Foo)' != 'bar'">baz</Foo>
  </PropertyGroup>
</Project>"#;
    let p = parse(src);
    assert_eq!(p.properties.get("Foo").map(String::as_str), Some("baz"));
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    assert!(!p.is_partial);
}

#[test]
fn self_reference_in_composite_condition_evaluates_exactly() {
    // `'$(Foo)' == '' Or '$(Foo)' == 'x'`: with an empty environment
    // snapshot and a clean walk, Foo is *provably* unset, so both arms
    // are exactly computable ('' == '' is true) and the gate is exactly
    // true — the same branch the real build takes. No diagnostic.
    let src = r#"<Project>
  <PropertyGroup>
    <Foo Condition="'$(Foo)' == '' Or '$(Foo)' == 'x'">baz</Foo>
  </PropertyGroup>
</Project>"#;
    let p = parse(src);
    assert_eq!(p.properties.get("Foo").map(String::as_str), Some("baz"));
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    assert!(!p.is_partial);
}

#[test]
fn self_reference_compared_against_space_evaluates_exactly_false() {
    // `' '` is a space literal, not the empty literal — but with an
    // empty environment snapshot and a clean walk, Foo is *provably*
    // unset, so `'' == ' '` is exactly false: the write is exactly
    // skipped (as in the real build) with no diagnostic.
    let src = r#"<Project>
  <PropertyGroup>
    <Foo Condition="'$(Foo)' == ' '">baz</Foo>
  </PropertyGroup>
</Project>"#;
    let p = parse(src);
    assert!(!p.properties.contains_key("Foo"), "{:?}", p.properties);
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    assert!(!p.is_partial);
}

#[test]
fn reserved_toolset_property_in_extras_is_rejected() {
    // `dotnet msbuild -p:MSBuildToolsVersion=Foo` errors: the toolset
    // names are reserved from process start, exactly like the
    // path-derived ones. Accepting them would let a caller global
    // redirect `$(MSBuildExtensionsPath)\$(MSBuildToolsVersion)\…`
    // imports to a bogus path.
    for name in [
        "MSBuildToolsVersion",
        "msbuildtoolspath",
        "MSBuildBinPath",
        "MSBuildRuntimeType",
    ] {
        let mut extras = HashMap::new();
        extras.insert(name.to_string(), "hijack".to_string());
        let err = parse_fsproj(
            "<Project/>",
            Path::new("/repo/Demo.fsproj"),
            &extras,
            &HashMap::new(),
        )
        .expect_err("reserved toolset name must be rejected");
        assert!(
            matches!(&err, crate::ParseError::ReservedPropertyInExtras(n) if n == name),
            "{name}: got {err:?}"
        );
    }
}

#[test]
fn get_directory_name_of_file_above_is_unsupported_in_pure_parse() {
    // The pure `parse_fsproj` contract promises no filesystem access, and
    // `GetDirectoryNameOfFileAbove` is a filesystem probe — in this mode
    // it must stay a visibly unsupported expression, never evaluate.
    let src = r#"<Project>
  <PropertyGroup>
    <Above>$([MSBuild]::GetDirectoryNameOfFileAbove('/', 'anything'))</Above>
  </PropertyGroup>
</Project>"#;
    let p = parse(src);
    assert!(
        p.diagnostics.iter().any(|d| matches!(
            &d.kind,
            DiagnosticKind::UnsupportedPropertyExpression { .. }
        )),
        "expected UnsupportedPropertyExpression, got {:?}",
        p.diagnostics
    );
}

#[test]
fn unsupported_property_function_is_left_literal_and_diagnosed() {
    let src = r#"<Project>
  <ItemGroup>
    <Compile Include="$([System.IO.Path]::GetFullPath('a'))" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert!(p.items.is_empty());
    assert_eq!(
        diag_kinds(&p.diagnostics),
        [&DiagnosticKind::UnsupportedPropertyExpression {
            expression: "$([System.IO.Path]::GetFullPath('a'))".to_string()
        }]
    );
}

#[test]
fn property_value_expanding_to_semicolon_list_creates_multiple_items() {
    // The whole point of substitute-then-split: a single Include
    // attribute whose property expands to `a.fs;b.fs` must yield two
    // items, not one literal entry.
    let src = r#"<Project>
  <PropertyGroup>
    <Sources>a.fs;b.fs</Sources>
  </PropertyGroup>
  <ItemGroup>
    <Compile Include="$(Sources)" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert_eq!(
        paths(&p.items),
        [Path::new("/repo/proj/a.fs"), Path::new("/repo/proj/b.fs")]
    );
}

#[test]
fn substitution_applies_to_link_attribute() {
    let src = r#"<Project>
  <PropertyGroup>
    <Folder>Shared</Folder>
  </PropertyGroup>
  <ItemGroup>
    <Compile Include="Mod.fs" Link="$(Folder)/Mod.fs" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert_eq!(p.items[0].link.as_deref(), Some("Shared/Mod.fs"));
}

#[test]
fn substitution_applies_to_link_child_element() {
    let src = r#"<Project>
  <PropertyGroup>
    <Folder>Shared</Folder>
  </PropertyGroup>
  <ItemGroup>
    <Compile Include="Mod.fs">
      <Link>$(Folder)/Mod.fs</Link>
    </Compile>
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert_eq!(p.items[0].link.as_deref(), Some("Shared/Mod.fs"));
}

#[test]
fn link_with_inexact_property_drops_link_keeps_item() {
    // The Include itself was fine; only the Link decoration failed to
    // substitute (`TargetFramework` is carved out, never provably unset).
    // We strip the Link rather than dropping the whole item, since the
    // file is still being compiled — just without the IDE grouping hint.
    let src = r#"<Project>
  <ItemGroup>
    <Compile Include="Mod.fs" Link="$(TargetFramework)/Mod.fs" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert_eq!(paths(&p.items), [Path::new("/repo/proj/Mod.fs")]);
    assert!(p.items[0].link.is_none());
    assert_eq!(
        diag_kinds(&p.diagnostics),
        [&DiagnosticKind::UndefinedProperty {
            name: "TargetFramework".to_string()
        }]
    );
}

#[test]
fn property_element_with_evaluable_false_condition_is_silently_skipped_when_props_known() {
    // Phase 3: when Configuration is supplied as a non-matching value,
    // `'$(Configuration)' == 'Release'` is false and `<Optimize>` is
    // not written. No diagnostic — a successful evaluation that
    // excludes the write matches MSBuild's behaviour exactly.
    let src = r#"<Project>
  <PropertyGroup>
    <Optimize Condition="'$(Configuration)' == 'Release'">true</Optimize>
  </PropertyGroup>
</Project>"#;
    let p = parse_with(src, &[("Configuration", "Debug")]);
    assert!(
        !p.properties.contains_key("Optimize"),
        "Optimize was written despite false condition: {:?}",
        p.properties
    );
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    assert!(!p.is_partial);
}

#[test]
fn property_element_condition_referencing_provably_unset_property_skips_exactly() {
    // Optimize is not a protected name, so the condition is
    // evaluated. `Configuration` is absent from the property map, the
    // walk is clean and the environment snapshot is empty, so the
    // name is *provably* unset: the comparison is exactly
    // `'' == 'Release'` = false — the same branch the real build
    // takes. Optimize is not written and no diagnostic is needed.
    let src = r#"<Project>
  <PropertyGroup>
    <Optimize Condition="'$(Configuration)' == 'Release'">true</Optimize>
  </PropertyGroup>
</Project>"#;
    let p = parse(src);
    assert!(
        !p.properties.contains_key("Optimize"),
        "Optimize was written despite false condition: {:?}",
        p.properties
    );
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    assert!(!p.is_partial);
}

#[test]
fn property_element_with_evaluable_true_condition_is_assigned() {
    let src = r#"<Project>
  <PropertyGroup>
    <Optimize Condition="'$(Configuration)' == 'Release'">true</Optimize>
  </PropertyGroup>
</Project>"#;
    let p = parse_with(src, &[("Configuration", "Release")]);
    assert_eq!(
        p.properties.get("Optimize").map(String::as_str),
        Some("true")
    );
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    assert!(!p.is_partial);
}

#[test]
fn property_element_with_unsupported_condition_is_excluded_and_diagnosed() {
    // We can't tell whether `Exists(...)` is true; per plan D5 the
    // safe move is to exclude the write so we never silently set a
    // property MSBuild would have left alone.
    let src = r#"<Project>
  <PropertyGroup>
    <Optimize Condition="Exists('foo')">true</Optimize>
  </PropertyGroup>
</Project>"#;
    let p = parse(src);
    assert!(!p.properties.contains_key("Optimize"));
    assert!(matches!(
        p.diagnostics[0].kind,
        DiagnosticKind::UnsupportedCondition { .. }
    ));
    assert!(p.is_partial);
}

#[test]
fn all_whitespace_element_body_collapses_to_empty() {
    // MSBuild collapses a property or metadata element body that is
    // *entirely* XML whitespace to `""` at the XML layer, before any
    // expansion. Pinned against dotnet msbuild 10.0.301 (2026-07-11):
    // `<P> </P>` → `$(P.Length)` is 0 and `'$(P)' == ''` is true; a tab
    // body collapses too; but a body with any non-whitespace content
    // keeps its surrounding whitespace verbatim (`<Q>  x  </Q>` → 5),
    // a non-breaking space is *not* XML whitespace (`&#160;` → 1), and
    // the collapse does not re-apply after expansion (`<P> $(Undef) </P>`
    // → 2, the two literal spaces). Storing the raw `" "` would make
    // `'$(P)' == ''` commit False where the real build says True.
    let src = "<Project>\n  <PropertyGroup>\n    <Space> </Space>\n    \
               <Tab>\t</Tab>\n    <Content>  x  </Content>\n    \
               <Nbsp>\u{a0}</Nbsp>\n    <Expanded> $(Undefined) </Expanded>\n  \
               </PropertyGroup>\n</Project>";
    let p = parse(src);
    let get = |name: &str| p.properties.get(name).map(String::as_str);
    assert_eq!(get("Space"), Some(""), "all-whitespace body collapses");
    assert_eq!(get("Tab"), Some(""), "a tab is XML whitespace");
    assert_eq!(get("Content"), Some("  x  "), "content keeps its padding");
    assert_eq!(get("Nbsp"), Some("\u{a0}"), "NBSP is not XML whitespace");
    assert_eq!(
        get("Expanded"),
        Some("  "),
        "the collapse is pre-expansion, not re-applied to the result"
    );
}

#[test]
fn whitespace_only_text_child_drops_per_node_not_per_value() {
    // A comment splits the text children; MSBuild drops the whitespace-only
    // one on its own, so `<R>  <!-- c -->x</R>` is "x", not "  x" (pinned,
    // dotnet msbuild 10.0.301). A whole-value collapse would get this wrong.
    let src = "<Project>\n  <PropertyGroup>\n    <R>  <!-- c -->x</R>\n    \
               <S>a<!-- c --> </S>\n    <CM><!-- c --> <!-- d --></CM>\n  \
               </PropertyGroup>\n</Project>";
    let p = parse(src);
    let get = |name: &str| p.properties.get(name).map(String::as_str);
    assert_eq!(get("R"), Some("x"));
    assert_eq!(get("S"), Some("a"));
    assert_eq!(get("CM"), Some(""));
}

#[test]
fn unmodellable_element_bodies_degrade() {
    // Two shapes we cannot derive a value for, so we must not commit one:
    //
    // * CDATA. MSBuild keeps CDATA content verbatim while dropping adjacent
    //   literal whitespace (`<P> <![CDATA[ ]]> </P>` is " ", length 1), but
    //   roxmltree merges CDATA into its neighbouring text node and truncates
    //   the node's source range, so we cannot tell that apart from `<P>   </P>`
    //   (which is ""). CDATA in a property body is vanishingly rare: one file
    //   in the whole SDK chain, none in the F# corpus.
    // * Entity-encoded whitespace. MSBuild is inconsistent — `&#32;` and
    //   `&#9;` are kept (length 1), `&#x20;` is dropped (length 0) — so we
    //   decline rather than pick a side.
    for body in ["<![CDATA[ ]]>", " <![CDATA[ ]]> ", "&#32;", "&#x20;"] {
        let src = format!(
            "<Project>\n  <PropertyGroup>\n    <P>{body}</P>\n  </PropertyGroup>\n</Project>"
        );
        let p = parse(&src);
        assert!(
            !p.properties.contains_key("P"),
            "body {body:?} must not commit a value"
        );
        assert!(
            p.diagnostics.iter().any(|d| matches!(
                &d.kind,
                DiagnosticKind::UnsupportedPropertyExpression { .. }
            )),
            "body {body:?}: {:?}",
            p.diagnostics
        );
    }
    // A CDATA body carrying real content degrades too — sound, and it costs
    // nothing: the shape does not occur in any project we evaluate.
    let src = "<Project>\n  <PropertyGroup>\n    <P><![CDATA[x]]></P>\n  \
               </PropertyGroup>\n</Project>";
    assert!(!parse(src).properties.contains_key("P"));
}

#[test]
fn all_whitespace_metadata_body_reads_as_unset() {
    // The same XML-layer collapse applies to item metadata (`<Meta> </Meta>`
    // → `""`, oracle-pinned via `-getItem`). The metadata layer already
    // trimmed its way to the right answer, and folds empty into *unset*
    // because MSBuild's own `GetMetadataValue` reports `""` for set-empty and
    // unset alike (see `resolve_string_metadata`) — so the observable is
    // `None`, never a literal `" "`.
    let src = "<Project>\n  <ItemGroup>\n    <PackageReference Include=\"Alpha\">\n      \
               <Version> </Version>\n    </PackageReference>\n  </ItemGroup>\n</Project>";
    let p = parse(src);
    let alpha = p
        .package_references
        .iter()
        .find(|r| r.id == "Alpha")
        .expect("Alpha captured");
    assert_eq!(alpha.version, None);
}

#[test]
fn empty_property_element_defines_name_as_empty_string() {
    // `<NoWarn />` and `<NoWarn></NoWarn>` clear an inherited value to
    // empty. Distinct from undefined: subsequent $(NoWarn) substitutions
    // return "" with no diagnostic.
    let src = r#"<Project>
  <PropertyGroup>
    <NoWarn></NoWarn>
  </PropertyGroup>
  <ItemGroup>
    <Compile Include="A$(NoWarn).fs" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert_eq!(paths(&p.items), [Path::new("/repo/proj/A.fs")]);
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    assert_eq!(p.properties.get("NoWarn").map(String::as_str), Some(""));
}

#[test]
fn property_lookup_is_case_insensitive() {
    // MSBuild property names use OrdinalIgnoreCase comparison. A
    // reference written `$(sourceroot)` must resolve against a binding
    // written `<SourceRoot>`. Real-world projects mix cases — e.g. the
    // SDK references `$(Configuration)` while user property bags often
    // use `configuration`.
    let src = r#"<Project>
  <PropertyGroup>
    <SourceRoot>src</SourceRoot>
  </PropertyGroup>
  <ItemGroup>
    <Compile Include="$(sourceroot)/Lib.fs" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert_eq!(paths(&p.items), [Path::new("/repo/proj/src/Lib.fs")]);
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
}

#[test]
fn extra_properties_lookup_is_case_insensitive() {
    let src = r#"<Project>
  <ItemGroup>
    <Compile Include="$(Configuration)/Gen.fs" />
  </ItemGroup>
</Project>"#;
    let p = parse_with(src, &[("configuration", "Release")]);
    assert_eq!(paths(&p.items), [Path::new("/repo/proj/Release/Gen.fs")]);
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
}

#[test]
fn case_insensitive_extra_blocks_propertygroup_write_in_any_case() {
    // The caller supplied `configuration=Release`; the project's
    // <Configuration>Debug</Configuration> targets the same property
    // under a different casing and must still be ignored.
    let src = r#"<Project>
  <PropertyGroup>
    <Configuration>Debug</Configuration>
  </PropertyGroup>
  <ItemGroup>
    <Compile Include="$(Configuration)/Gen.fs" />
  </ItemGroup>
</Project>"#;
    let p = parse_with(src, &[("configuration", "Release")]);
    assert_eq!(paths(&p.items), [Path::new("/repo/proj/Release/Gen.fs")]);
}

#[test]
fn tainted_property_value_does_not_silently_corrupt_downstream() {
    // <Gen>$([…]::Method())</Gen> emits UnsupportedPropertyExpression
    // while computing Gen's value. The literal residual `$([…])` must
    // NOT be stored as Gen's value, because then `$(Gen)` in a later
    // Include would expand cleanly to that residual and produce a
    // garbage compile path. Two acceptable outcomes:
    //   * Gen is not bound at all, so later $(Gen) emits Undefined.
    //   * Gen is bound to "", so later $(Gen) substitutes to empty.
    // Either way, the downstream Include must NOT contain `$([…])`.
    let src = r#"<Project>
  <PropertyGroup>
    <Gen>$([System.IO.Path]::GetFullPath('a'))</Gen>
  </PropertyGroup>
  <ItemGroup>
    <Compile Include="$(Gen)/Out.fs" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    for item in &p.items {
        let s = item.include.to_string_lossy();
        assert!(
            !s.contains("$("),
            "downstream include must not carry the tainted property's residual: {s}"
        );
    }
    // The original failure to compute Gen should still be diagnosed.
    assert!(
        p.diagnostics
            .iter()
            .any(|d| matches!(d.kind, DiagnosticKind::UnsupportedPropertyExpression { .. })),
        "expected UnsupportedPropertyExpression in diagnostics, got {:?}",
        p.diagnostics
    );
}

#[test]
fn undefined_only_property_value_is_still_stored_but_unpins_consumers() {
    // <Foo>$(TargetFramework)</Foo> substitutes to the empty string
    // (MSBuild's behaviour for undefined refs), so Foo IS bound and the
    // item is captured best-effort. But `TargetFramework` is a
    // consumer-contract carve-out that never counts as provably unset (a real
    // build may set it, e.g. the multi-TFM shape), so the item-pass
    // consumption surfaces the root `TargetFramework` again at the
    // item's own span and the Compile set degrades to uncertain — the
    // same envelope as a direct `$(TargetFramework)` in the Include.
    let src = r#"<Project>
  <PropertyGroup>
    <Foo>$(TargetFramework)</Foo>
  </PropertyGroup>
  <ItemGroup>
    <Compile Include="prefix$(Foo)suffix.fs" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert_eq!(paths(&p.items), [Path::new("/repo/proj/prefixsuffix.fs")]);
    assert_eq!(
        diag_kinds(&p.diagnostics),
        [
            &DiagnosticKind::UndefinedProperty {
                name: "TargetFramework".to_string()
            },
            &DiagnosticKind::UndefinedProperty {
                name: "TargetFramework".to_string()
            }
        ]
    );
    assert!(
        p.items_uncertain,
        "an Include leaning on an unpinned property may differ in a real build"
    );
}

#[test]
fn protected_write_with_unsupported_condition_emits_no_diagnostic() {
    // The standard SDK idiom for defaulting a global property:
    //   <Configuration Condition="'$(Configuration)' == ''">Debug</Configuration>
    // When a caller supplies `Configuration` as a global, MSBuild
    // discards the whole assignment without evaluating the condition.
    // Mirroring that, we must NOT emit an UnsupportedCondition for the
    // condition we never would have honoured anyway — otherwise every
    // real-world project trips `is_partial` for SDK-standard boilerplate.
    let src = r#"<Project>
  <PropertyGroup>
    <Configuration Condition="'$(Configuration)' == ''">Debug</Configuration>
  </PropertyGroup>
  <ItemGroup>
    <Compile Include="$(Configuration)/Gen.fs" />
  </ItemGroup>
</Project>"#;
    let p = parse_with(src, &[("Configuration", "Release")]);
    assert_eq!(paths(&p.items), [Path::new("/repo/proj/Release/Gen.fs")]);
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    assert!(!p.is_partial);
}

#[test]
fn protected_write_with_unsupported_value_emits_no_diagnostic() {
    // Same principle, value side. If the caller has reserved a
    // property name, the project's write to that name is wholly
    // discarded — the value's expansion is irrelevant. Don't bill
    // the project for an UnsupportedPropertyExpression MSBuild would
    // never have evaluated.
    let src = r#"<Project>
  <PropertyGroup>
    <MSBuildProjectName>$([System.IO.Path]::Combine('x', 'y'))</MSBuildProjectName>
  </PropertyGroup>
  <ItemGroup>
    <Compile Include="$(MSBuildProjectName).fs" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert_eq!(paths(&p.items), [Path::new("/repo/proj/Demo.fs")]);
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    assert!(!p.is_partial);
}

#[test]
fn dotted_reference_is_member_access_not_a_property_name() {
    // A `.` inside `$(…)` is *member access*, never part of a property
    // name: dotted property/element names are illegal in MSBuild itself
    // (`<Out.Dir>` → MSB5016, `-p:Out.Dir=…` → MSB4177; verified against
    // dotnet msbuild 10.0.300). So `$(Out.Dir)` parses as member `.Dir` on
    // property `Out` — an unmodelled member — and stays
    // UnsupportedPropertyExpression, degrading the item rather than
    // resolving a name MSBuild would reject. (`-` *is* legal in names; see
    // `property_name_with_hyphen_substitutes`.)
    let src = r#"<Project>
  <PropertyGroup>
    <Out.Dir>gen</Out.Dir>
  </PropertyGroup>
  <ItemGroup>
    <Compile Include="$(Out.Dir)/A.fs" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert!(p.items.is_empty(), "{:?}", p.items);
    assert_eq!(
        diag_kinds(&p.diagnostics),
        [&DiagnosticKind::UnsupportedPropertyExpression {
            expression: "$(Out.Dir)".to_string()
        }]
    );
    assert!(p.is_partial);
}

#[test]
fn property_name_with_hyphen_substitutes() {
    let src = r#"<Project>
  <PropertyGroup>
    <Out-Dir>gen</Out-Dir>
  </PropertyGroup>
  <ItemGroup>
    <Compile Include="$(Out-Dir)/A.fs" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert_eq!(paths(&p.items), [Path::new("/repo/proj/gen/A.fs")]);
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
}

#[test]
fn property_function_on_dotted_name_still_unsupported() {
    // The fast-path widening must NOT swallow `$(Foo.Method())` — the
    // trailing `(` after `Foo.Method` proves it's a property function,
    // not a bare reference. Slow path catches it and emits Unsupported.
    let src = r#"<Project>
  <ItemGroup>
    <Compile Include="$(Foo.Bar())/A.fs" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert!(p.items.is_empty());
    assert!(
        p.diagnostics
            .iter()
            .any(|d| matches!(d.kind, DiagnosticKind::UnsupportedPropertyExpression { .. })),
        "{:?}",
        p.diagnostics
    );
}

#[test]
fn extras_with_reserved_name_are_rejected() {
    // Empirically, MSBuild responds to `dotnet msbuild -p:MSBuildProjectName=Bad`
    // with `MSB4177: Invalid property. The "MSBuildProjectName" property
    // name is reserved.` — reserved (path-derived) properties are
    // off-limits even to command-line globals. Mirror that contract:
    // accepting the override would let extras silently change
    // `$(MSBuildProjectName)` from the path-derived value to whatever
    // the caller passed, producing wrong compile paths.
    let mut extras = HashMap::new();
    extras.insert("MSBuildProjectName".to_string(), "Bad".to_string());
    let err = parse_fsproj(
        "<Project/>",
        Path::new("/repo/proj/Demo.fsproj"),
        &extras,
        &HashMap::new(),
    )
    .expect_err("reserved-name extra should be rejected");
    let s = err.to_string();
    assert!(
        s.contains("MSBuildProjectName") && s.contains("reserved"),
        "got {s}"
    );
}

#[test]
fn extras_with_reserved_name_case_insensitive_rejected() {
    // Lookup is OrdinalIgnoreCase, so passing `msbuildprojectname` (or
    // any other casing) must trip the same guard.
    let mut extras = HashMap::new();
    extras.insert("msbuildprojectname".to_string(), "Bad".to_string());
    let err = parse_fsproj(
        "<Project/>",
        Path::new("/repo/proj/Demo.fsproj"),
        &extras,
        &HashMap::new(),
    )
    .expect_err("reserved-name extra (lowercased) should be rejected");
    assert!(err.to_string().contains("reserved"));
}

#[test]
fn extras_with_case_variant_duplicate_keys_are_rejected() {
    // Property lookup is OrdinalIgnoreCase, so `Configuration` and
    // `configuration` (or any other casing pair) collide in the
    // case-insensitive PropertyMap. HashMap iteration order is
    // *unspecified*, so silently picking one would let
    // `$(Configuration)` resolve to a different value across runs —
    // and across builds of Rust's std. Reject ambiguous extras up
    // front so the API is deterministic.
    let mut extras = HashMap::new();
    extras.insert("Configuration".to_string(), "Debug".to_string());
    extras.insert("configuration".to_string(), "Release".to_string());
    let err = parse_fsproj(
        "<Project/>",
        Path::new("/repo/proj/Demo.fsproj"),
        &extras,
        &HashMap::new(),
    )
    .expect_err("case-variant duplicate extras should be rejected");
    let s = err.to_string();
    // Both spellings must be surfaced so the caller knows which key
    // to drop — saying just "duplicate" leaves them guessing.
    assert!(
        s.contains("Configuration") && s.contains("configuration"),
        "error should mention both colliding spellings, got {s}"
    );
    assert!(s.contains("duplicate") || s.contains("case"), "got {s}");
}

#[test]
fn extras_with_exact_same_key_is_not_a_duplicate() {
    // Sanity check: a single key under one spelling is fine — the
    // duplicate guard must only fire on *two distinct* keys that
    // collide case-insensitively. (HashMap can't even hold two
    // identical String keys, but this pins the contract.)
    let mut extras = HashMap::new();
    extras.insert("Configuration".to_string(), "Debug".to_string());
    parse_fsproj(
        "<Project/>",
        Path::new("/repo/proj/Demo.fsproj"),
        &extras,
        &HashMap::new(),
    )
    .expect("single extra should parse cleanly");
}

#[test]
fn extras_duplicate_rejection_is_order_independent() {
    // The error must surface deterministically regardless of which
    // spelling HashMap iteration visits first. Run the same pair many
    // times and assert the produced error string is stable.
    let mut first_err: Option<String> = None;
    for _ in 0..32 {
        let mut extras = HashMap::new();
        extras.insert("Configuration".to_string(), "Debug".to_string());
        extras.insert("CONFIGURATION".to_string(), "Release".to_string());
        let err = parse_fsproj(
            "<Project/>",
            Path::new("/repo/proj/Demo.fsproj"),
            &extras,
            &HashMap::new(),
        )
        .expect_err("must reject case-variant duplicates");
        let s = err.to_string();
        match &first_err {
            None => first_err = Some(s),
            Some(prior) => assert_eq!(
                prior, &s,
                "duplicate-extras error must be deterministic across HashMap iteration orders"
            ),
        }
    }
}

#[test]
fn tainted_redefinition_unbinds_previously_clean_value() {
    // Earlier we fixed tainted FIRST-time writes (don't store).
    // Tainted REDEFINITIONS are the same problem in reverse: leaving
    // the prior clean value in lookup makes `$(Dir)` later resolve to
    // it, even though MSBuild would have replaced Dir with whatever
    // the new unsupported expression evaluated to — never the old
    // value. Drop the binding entirely so subsequent refs emit
    // Undefined (mirroring the "tainted property's value is not
    // trusted downstream" guarantee).
    let src = r#"<Project>
  <PropertyGroup>
    <Dir>src</Dir>
    <Dir>$([System.IO.Path]::GetFullPath('gen'))</Dir>
  </PropertyGroup>
  <ItemGroup>
    <Compile Include="$(Dir)/A.fs" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    // The downstream Include must not silently emit the stale `src/A.fs`.
    for item in &p.items {
        let s = item.include.to_string_lossy();
        assert!(
            !s.contains("src/A.fs"),
            "downstream include carried the stale prior value: {s}"
        );
    }
    // And the diagnostics surface both the failed write and the
    // resulting undefined reference.
    assert!(
        p.diagnostics
            .iter()
            .any(|d| matches!(d.kind, DiagnosticKind::UnsupportedPropertyExpression { .. })),
        "expected UnsupportedPropertyExpression, got {:?}",
        p.diagnostics
    );
    assert!(
        p.diagnostics.iter().any(
            |d| matches!(&d.kind, DiagnosticKind::UndefinedProperty { name } if name == "Dir")
        ),
        "expected UndefinedProperty(Dir), got {:?}",
        p.diagnostics
    );
}

#[test]
fn well_known_seeds_msbuild_project_file_alias() {
    // MSBuild defines BOTH `MSBuildThisFile` and `MSBuildProjectFile` as
    // the project's filename (the latter is the canonical project-scoped
    // alias). Omitting the alias silently drops Includes that reference
    // it — there is no diagnostic the caller can act on because the
    // single-paren `$(MSBuildProjectFile)` is a syntactically valid
    // reference, so it merely reports as Undefined-substitutes-to-empty.
    let src = r#"<Project>
  <ItemGroup>
    <Compile Include="$(MSBuildProjectFile).bak" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert_eq!(paths(&p.items), [Path::new("/repo/proj/Demo.fsproj.bak")]);
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
}

#[test]
fn msbuild_project_file_alias_cannot_be_overridden_by_propertygroup() {
    // Same reserved-property guarantee MSBuildThisFile gets: project-side
    // writes are silently discarded, no diagnostic, no shadowing.
    let src = r#"<Project>
  <PropertyGroup>
    <MSBuildProjectFile>Other.fsproj</MSBuildProjectFile>
  </PropertyGroup>
  <ItemGroup>
    <Compile Include="$(MSBuildProjectFile)" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert_eq!(paths(&p.items), [Path::new("/repo/proj/Demo.fsproj")]);
    assert!(!p.properties.contains_key("MSBuildProjectFile"));
}

#[test]
fn property_text_split_by_comment_concatenates_full_value() {
    // roxmltree's `Node::text()` only returns the *first* text child,
    // so a property value broken up by a comment was previously
    // truncated. MSBuild treats the element's entire concatenated text
    // as the value, so `<Sources>A.fs;<!-- comment -->B.fs</Sources>`
    // must expand to "A.fs;B.fs" — losing the tail would silently drop
    // compile items with no diagnostic the caller could detect.
    let src = r#"<Project>
  <PropertyGroup>
    <Sources>A.fs;<!-- keep sorted -->B.fs</Sources>
  </PropertyGroup>
  <ItemGroup>
    <Compile Include="$(Sources)" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert_eq!(
        paths(&p.items),
        [Path::new("/repo/proj/A.fs"), Path::new("/repo/proj/B.fs")]
    );
    assert_eq!(
        p.properties.get("Sources").map(String::as_str),
        Some("A.fs;B.fs")
    );
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
}

#[test]
fn link_child_text_split_by_comment_concatenates_full_value() {
    // The same trap exists for `<Link>` child elements: we read its
    // text via `Node::text()`, so comments inside the Link child would
    // truncate the metadata. Make sure the full inner text survives.
    let src = r#"<Project>
  <ItemGroup>
    <Compile Include="A.fs">
      <Link>src/<!-- bucket -->A.fs</Link>
    </Compile>
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert_eq!(p.items.len(), 1);
    assert_eq!(p.items[0].link.as_deref(), Some("src/A.fs"));
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
}

#[test]
fn treat_as_local_property_lets_project_override_extras() {
    // TreatAsLocalProperty is the documented escape hatch that lets a
    // project rebind a name that was supplied as a global property
    // (which we model as `extra_properties`). Without honouring it we
    // silently swallow the project's write — `$(Configuration)` would
    // keep substituting the caller's value with no diagnostic and no
    // way for downstream code to notice the divergence.
    let src = r#"<Project TreatAsLocalProperty="Configuration">
  <PropertyGroup>
    <Configuration>Debug</Configuration>
  </PropertyGroup>
  <ItemGroup>
    <Compile Include="$(Configuration)/A.fs" />
  </ItemGroup>
</Project>"#;
    let extras: HashMap<String, String> = [("Configuration".to_string(), "Release".to_string())]
        .into_iter()
        .collect();
    let p = parse_fsproj(
        src,
        Path::new("/repo/proj/Demo.fsproj"),
        &extras,
        &HashMap::new(),
    )
    .expect("well-formed XML parses");
    assert_eq!(paths(&p.items), [Path::new("/repo/proj/Debug/A.fs")]);
    assert_eq!(
        p.properties.get("Configuration").map(String::as_str),
        Some("Debug")
    );
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
}

#[test]
fn treat_as_local_property_does_not_apply_when_name_omitted() {
    // Listing only "Foo" must not unprotect "Configuration".
    let src = r#"<Project TreatAsLocalProperty="Foo">
  <PropertyGroup>
    <Configuration>Debug</Configuration>
  </PropertyGroup>
  <ItemGroup>
    <Compile Include="$(Configuration)/A.fs" />
  </ItemGroup>
</Project>"#;
    let extras: HashMap<String, String> = [("Configuration".to_string(), "Release".to_string())]
        .into_iter()
        .collect();
    let p = parse_fsproj(
        src,
        Path::new("/repo/proj/Demo.fsproj"),
        &extras,
        &HashMap::new(),
    )
    .expect("well-formed XML parses");
    // Write is dropped, substitution still uses the caller's value.
    assert_eq!(paths(&p.items), [Path::new("/repo/proj/Release/A.fs")]);
    assert!(!p.properties.contains_key("Configuration"));
}

#[test]
fn treat_as_local_property_cannot_unprotect_reserved_names() {
    // The attribute is documented to apply to globals; reserved
    // (well-known) properties remain read-only. A project listing
    // MSBuildProjectName in TreatAsLocalProperty must not be able to
    // shadow the path-derived seed.
    let src = r#"<Project TreatAsLocalProperty="MSBuildProjectName">
  <PropertyGroup>
    <MSBuildProjectName>OtherName</MSBuildProjectName>
  </PropertyGroup>
  <ItemGroup>
    <Compile Include="$(MSBuildProjectName).fs" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert_eq!(paths(&p.items), [Path::new("/repo/proj/Demo.fs")]);
    assert!(!p.properties.contains_key("MSBuildProjectName"));
}

#[test]
fn treat_as_local_property_name_not_written_stays_out_of_properties() {
    // If the name is listed but the project never actually writes it,
    // the caller's value remains in use during substitution but
    // shouldn't surface in `properties` — that map is documented as
    // "what the project wrote", and an unwritten extra is exactly the
    // case it's meant to exclude.
    let src = r#"<Project TreatAsLocalProperty="Configuration">
  <ItemGroup>
    <Compile Include="$(Configuration)/A.fs" />
  </ItemGroup>
</Project>"#;
    let extras: HashMap<String, String> = [("Configuration".to_string(), "Release".to_string())]
        .into_iter()
        .collect();
    let p = parse_fsproj(
        src,
        Path::new("/repo/proj/Demo.fsproj"),
        &extras,
        &HashMap::new(),
    )
    .expect("well-formed XML parses");
    assert_eq!(paths(&p.items), [Path::new("/repo/proj/Release/A.fs")]);
    assert!(
        !p.properties.contains_key("Configuration"),
        "Configuration was never written by the project but appears in properties"
    );
}

#[test]
fn treat_as_local_property_list_uses_case_insensitive_match() {
    // Property names are OrdinalIgnoreCase throughout MSBuild — the
    // list inside TreatAsLocalProperty is no exception.
    let src = r#"<Project TreatAsLocalProperty="configuration">
  <PropertyGroup>
    <Configuration>Debug</Configuration>
  </PropertyGroup>
  <ItemGroup>
    <Compile Include="$(Configuration)/A.fs" />
  </ItemGroup>
</Project>"#;
    let extras: HashMap<String, String> = [("Configuration".to_string(), "Release".to_string())]
        .into_iter()
        .collect();
    let p = parse_fsproj(
        src,
        Path::new("/repo/proj/Demo.fsproj"),
        &extras,
        &HashMap::new(),
    )
    .expect("well-formed XML parses");
    assert_eq!(paths(&p.items), [Path::new("/repo/proj/Debug/A.fs")]);
}

#[test]
fn rooted_but_not_drive_qualified_project_path_is_accepted() {
    // On Windows, `Path::new("/repo/proj/Demo.fsproj")` has a root but
    // no drive prefix, so `is_absolute()` reports false. We only need
    // to refuse paths that would cause `project_dir.join(Include)` to
    // double-join the directory component — i.e., genuinely relative
    // paths. A rooted path replaces during join even without a drive,
    // so accepting `has_root() == true` is sufficient and doesn't
    // break the test fixtures (or real Windows callers).
    parse_fsproj(
        "<Project/>",
        Path::new("/repo/proj/Demo.fsproj"),
        &HashMap::new(),
        &HashMap::new(),
    )
    .expect("rooted project_path should be accepted on every platform");
}

#[test]
fn property_function_with_quoted_paren_argument_is_diagnosed() {
    // `find_balanced_close` must not count parens inside MSBuild string
    // literals — otherwise `$([System.String]::Copy('('))` runs the
    // depth counter past the real closing paren and the expression
    // silently passes through as a successful (`is_partial=false`)
    // substitution, leaving literal `$([…])` in downstream paths.
    let expr = "$([System.String]::Copy('('))";
    let src = format!(
        r#"<Project>
  <ItemGroup>
    <Compile Include="{expr}/A.fs" />
  </ItemGroup>
</Project>"#
    );
    let p = parse(&src);
    // Item is skipped because the expansion is unsupported.
    assert!(p.items.is_empty(), "{:?}", p.items);
    assert!(
        p.diagnostics.iter().any(|d| matches!(
            &d.kind,
            DiagnosticKind::UnsupportedPropertyExpression { expression } if expression == expr
        )),
        "expected UnsupportedPropertyExpression({expr}), got {:?}",
        p.diagnostics
    );
    assert!(p.is_partial);
}

#[test]
fn item_reference_in_property_value_is_diagnosed() {
    // `<Foo>@(Items)</Foo>` stores an unevaluated `@(Items)` in the
    // properties map unless the property happens to be used in an
    // Include. The map is documented as evaluated project state, so
    // values containing item-list references must emit a diagnostic
    // and be dropped, the same way Include attributes already do.
    let src = r#"<Project>
  <PropertyGroup>
    <Sources>@(SourceFiles)</Sources>
  </PropertyGroup>
</Project>"#;
    let p = parse(src);
    assert!(
        !p.properties.contains_key("Sources"),
        "Sources held an unevaluated @(SourceFiles): {:?}",
        p.properties
    );
    assert!(
        p.diagnostics.iter().any(|d| matches!(
            &d.kind,
            DiagnosticKind::UnresolvedItemReference { reference } if reference.contains("@(SourceFiles)")
        )),
        "expected UnresolvedItemReference, got {:?}",
        p.diagnostics
    );
    assert!(p.is_partial);
}

#[test]
fn metadata_reference_in_property_value_is_diagnosed() {
    let src = r#"<Project>
  <PropertyGroup>
    <ItemPath>%(Identity)</ItemPath>
  </PropertyGroup>
</Project>"#;
    let p = parse(src);
    assert!(
        !p.properties.contains_key("ItemPath"),
        "ItemPath held an unevaluated %(Identity): {:?}",
        p.properties
    );
    assert!(
        p.diagnostics.iter().any(|d| matches!(
            &d.kind,
            DiagnosticKind::UnresolvedMetadataReference { reference } if reference.contains("%(Identity)")
        )),
        "expected UnresolvedMetadataReference, got {:?}",
        p.diagnostics
    );
    assert!(p.is_partial);
}

#[test]
fn treat_as_local_property_records_project_casing_not_extras_casing() {
    // With extras supplying the name in lowercase and the project
    // writing the canonical mixed case under TreatAsLocalProperty, the
    // `properties` map should be keyed by the *project's* spelling —
    // that's the casing a caller looking up "what did the project
    // write?" expects. Reporting the extras-side casing makes an exact
    // `properties.get("Configuration")` miss the write entirely.
    let src = r#"<Project TreatAsLocalProperty="Configuration">
  <PropertyGroup>
    <Configuration>Debug</Configuration>
  </PropertyGroup>
</Project>"#;
    let extras: HashMap<String, String> = [("configuration".to_string(), "Release".to_string())]
        .into_iter()
        .collect();
    let p = parse_fsproj(
        src,
        Path::new("/repo/proj/Demo.fsproj"),
        &extras,
        &HashMap::new(),
    )
    .expect("well-formed XML parses");
    assert_eq!(
        p.properties.get("Configuration").map(String::as_str),
        Some("Debug"),
        "properties keyed under wrong casing: {:?}",
        p.properties
    );
    assert!(
        !p.properties.contains_key("configuration"),
        "properties leaked the extras casing: {:?}",
        p.properties
    );
}

#[test]
fn property_group_condition_over_protected_only_writes_does_not_mark_partial() {
    // The classic MSBuild idiom for "default if not set" is a
    // PropertyGroup whose condition checks `'$(Foo)' == ''` and whose
    // only child writes to that same name. When the caller supplied
    // Foo as an extra (so it's protected), the group's child write is
    // already silently discarded — but we previously still emitted
    // `UnsupportedCondition` for the group, flipping `is_partial=true`
    // for a condition that cannot affect any unprotected assignment.
    // Mirror the per-element rule: suppress the diagnostic when every
    // child is a protected-name write.
    let src = r#"<Project>
  <PropertyGroup Condition="'$(Configuration)' == ''">
    <Configuration>Debug</Configuration>
  </PropertyGroup>
  <ItemGroup>
    <Compile Include="$(Configuration)/A.fs" />
  </ItemGroup>
</Project>"#;
    let extras: HashMap<String, String> = [("Configuration".to_string(), "Release".to_string())]
        .into_iter()
        .collect();
    let p = parse_fsproj(
        src,
        Path::new("/repo/proj/Demo.fsproj"),
        &extras,
        &HashMap::new(),
    )
    .expect("well-formed XML parses");
    assert_eq!(paths(&p.items), [Path::new("/repo/proj/Release/A.fs")]);
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    assert!(!p.is_partial);
}

#[test]
fn property_group_condition_evaluable_false_skips_unprotected_writes_too() {
    // Phase 3: with Configuration supplied as `Release`, the
    // condition `'$(Configuration)' == ''` evaluates to false, so
    // the whole group is skipped — including the unprotected
    // `SomethingElse` write. The protected-only optimization is no
    // longer the only path that suppresses spurious diagnostics; an
    // evaluable false condition does the right thing on its own.
    let src = r#"<Project>
  <PropertyGroup Condition="'$(Configuration)' == ''">
    <Configuration>Debug</Configuration>
    <SomethingElse>x</SomethingElse>
  </PropertyGroup>
</Project>"#;
    let extras: HashMap<String, String> = [("Configuration".to_string(), "Release".to_string())]
        .into_iter()
        .collect();
    let p = parse_fsproj(
        src,
        Path::new("/repo/proj/Demo.fsproj"),
        &extras,
        &HashMap::new(),
    )
    .expect("well-formed XML parses");
    assert!(
        !p.properties.contains_key("SomethingElse"),
        "SomethingElse was written despite false group condition: {:?}",
        p.properties
    );
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    assert!(!p.is_partial);
}

#[test]
fn property_group_condition_evaluable_true_walks_children() {
    let src = r#"<Project>
  <PropertyGroup Condition="'$(Configuration)' == 'Release'">
    <SomethingElse>x</SomethingElse>
  </PropertyGroup>
</Project>"#;
    let p = parse_with(src, &[("Configuration", "Release")]);
    assert_eq!(
        p.properties.get("SomethingElse").map(String::as_str),
        Some("x")
    );
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    assert!(!p.is_partial);
}

#[test]
fn property_group_unsupported_condition_skips_unprotected_writes_and_diagnoses() {
    // When the condition is genuinely unevaluable AND at least one
    // child write is unprotected, plan D5 requires excluding the
    // whole group (with a diagnostic) — proceeding as if true would
    // silently set a property MSBuild might have excluded.
    let src = r#"<Project>
  <PropertyGroup Condition="Exists('something')">
    <SomethingElse>x</SomethingElse>
  </PropertyGroup>
</Project>"#;
    let p = parse(src);
    assert!(!p.properties.contains_key("SomethingElse"));
    assert!(
        p.diagnostics
            .iter()
            .any(|d| matches!(&d.kind, DiagnosticKind::UnsupportedCondition { .. })),
        "expected UnsupportedCondition, got {:?}",
        p.diagnostics
    );
    assert!(p.is_partial);
}

#[test]
fn link_attribute_with_metadata_reference_is_diagnosed_and_dropped() {
    // After $(...) expansion, a Link value may still contain `@(...)`
    // or `%(...)` — neither of which we evaluate. Quietly returning
    // the literal exposes unevaluated MSBuild syntax in
    // ResolvedItem::link with `is_partial=false`; the symmetric
    // treatment (drop + diagnose) matches Include and PropertyGroup.
    let src = r#"<Project>
  <ItemGroup>
    <Compile Include="A.fs" Link="$(LinkPattern)" />
  </ItemGroup>
</Project>"#;
    let extras: HashMap<String, String> =
        [("LinkPattern".to_string(), "%(Filename).fs".to_string())]
            .into_iter()
            .collect();
    let p = parse_fsproj(
        src,
        Path::new("/repo/proj/Demo.fsproj"),
        &extras,
        &HashMap::new(),
    )
    .expect("well-formed XML parses");
    assert_eq!(p.items.len(), 1);
    assert!(
        p.items[0].link.is_none(),
        "link should be dropped, got {:?}",
        p.items[0].link
    );
    assert!(
        p.diagnostics
            .iter()
            .any(|d| matches!(&d.kind, DiagnosticKind::UnresolvedMetadataReference { .. })),
        "expected UnresolvedMetadataReference, got {:?}",
        p.diagnostics
    );
    assert!(p.is_partial);
}

#[test]
fn link_child_with_item_reference_is_diagnosed_and_dropped() {
    let src = r#"<Project>
  <ItemGroup>
    <Compile Include="A.fs">
      <Link>@(OtherItems)</Link>
    </Compile>
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert_eq!(p.items.len(), 1);
    assert!(p.items[0].link.is_none());
    assert!(
        p.diagnostics
            .iter()
            .any(|d| matches!(&d.kind, DiagnosticKind::UnresolvedItemReference { .. })),
        "expected UnresolvedItemReference, got {:?}",
        p.diagnostics
    );
    assert!(p.is_partial);
}

#[test]
fn unbalanced_dollar_paren_declines_the_include() {
    // An unclosed `$(` used to pass through with no issue and resolve as
    // a path with a literal `$(` in it — but MSBuild's scanner can find
    // a close (and then error) on quote nestings ours gives up on
    // (generative-sweep finding), so the no-claim passthrough was a
    // wrong commit waiting to happen. The include now degrades: dropped
    // from the capture, with a diagnostic.
    let src = r#"<Project>
  <ItemGroup>
    <Compile Include="A$(Unclosed.fs" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert_eq!(paths(&p.items), [] as [&Path; 0]);
    assert!(
        p.diagnostics.iter().any(|d| matches!(
            &d.kind,
            DiagnosticKind::UnsupportedPropertyExpression { expression } if expression.contains("$(Unclosed")
        )),
        "{:?}",
        p.diagnostics
    );
}

// -----------------------------------------------------------------
// ProjectReference: separate bucket, same evaluation rules as Compile.
// -----------------------------------------------------------------

#[test]
fn project_reference_lands_in_project_references_bucket() {
    let src = r#"<Project>
  <ItemGroup>
    <ProjectReference Include="../lib/Lib.csproj" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert!(
        p.items.is_empty(),
        "ProjectReference must not leak into items"
    );
    assert_eq!(p.project_references.len(), 1);
    assert_eq!(p.project_references[0].kind, ItemKind::ProjectReference);
    assert_eq!(
        p.project_references[0].include,
        PathBuf::from("/repo/proj/../lib/Lib.csproj"),
    );
    assert!(p.project_references[0].link.is_none());
    assert!(p.diagnostics.is_empty());
}

#[test]
fn project_reference_carries_reference_output_assembly_and_exclude_assets() {
    // Attribute form on one item; child-element form (with $() expansion)
    // on the other. Both shapes are legal MSBuild metadata spellings.
    let src = r#"<Project>
  <PropertyGroup>
    <NoCompile>compile</NoCompile>
  </PropertyGroup>
  <ItemGroup>
    <ProjectReference Include="../Tool/Tool.fsproj" ReferenceOutputAssembly="false" />
    <ProjectReference Include="../Lib/Lib.fsproj">
      <ExcludeAssets>$(NoCompile)</ExcludeAssets>
    </ProjectReference>
    <ProjectReference Include="../Plain/Plain.fsproj" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert_eq!(p.project_references.len(), 3);
    assert_eq!(
        p.project_references[0].reference_output_assembly,
        ItemMetadataValue::known("false"),
    );
    assert!(p.project_references[0].exclude_assets == ItemMetadataValue::ABSENT);
    assert_eq!(
        p.project_references[1].exclude_assets,
        ItemMetadataValue::known("compile"),
    );
    assert!(p.project_references[1].reference_output_assembly == ItemMetadataValue::ABSENT);
    assert!(p.project_references[2].reference_output_assembly == ItemMetadataValue::ABSENT);
    assert!(p.project_references[2].exclude_assets == ItemMetadataValue::ABSENT);
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
}

#[test]
fn project_reference_metadata_child_overrides_attribute_case_insensitively() {
    // MSBuild item-metadata semantics: names compare case-insensitively, the
    // attribute form is the first assignment, and child elements are *later*
    // writes that overwrite it. Getting the precedence wrong keeps a
    // reference MSBuild drops (or vice versa) in the compile-closure graph.
    let src = r#"<Project>
  <ItemGroup>
    <ProjectReference Include="../A/A.fsproj" ReferenceOutputAssembly="true">
      <ReferenceOutputAssembly>false</ReferenceOutputAssembly>
    </ProjectReference>
    <ProjectReference Include="../B/B.fsproj" referenceoutputassembly="false" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert_eq!(p.project_references.len(), 2);
    assert_eq!(
        p.project_references[0].reference_output_assembly,
        ItemMetadataValue::known("false"),
        "the child element is a later write and must override the attribute"
    );
    assert_eq!(
        p.project_references[1].reference_output_assembly,
        ItemMetadataValue::known("false"),
        "metadata names are case-insensitive"
    );
}

#[test]
fn case_variant_duplicate_metadata_attribute_is_last_write_wins() {
    // Valid XML: attribute names differing only by case are distinct XML
    // names. MSBuild metadata names are case-insensitive and the LATER
    // attribute wins (dotnet 10 probe, 2026-07-10: `<X Foo="one"
    // foo="two"/>` evaluates to two; the reversed order gives one). Reading
    // the first would fold a compile reference the real build drops.
    let src = r#"<Project>
  <ItemGroup>
    <ProjectReference Include="../A/A.fsproj" ReferenceOutputAssembly="true" referenceoutputassembly="false" />
    <ProjectReference Include="../B/B.fsproj" referenceoutputassembly="false" ReferenceOutputAssembly="true" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert_eq!(
        p.project_references[0].reference_output_assembly,
        ItemMetadataValue::known("false"),
        "the later case-variant attribute must win"
    );
    assert_eq!(
        p.project_references[1].reference_output_assembly,
        ItemMetadataValue::known("true"),
        "the later case-variant attribute must win (reversed order)"
    );
}

#[test]
fn project_reference_metadata_child_honours_its_condition() {
    // MSBuild evaluates a Condition on metadata child elements: a false one
    // means the metadatum is simply not set. Treating it as set would make
    // the compile-closure walk drop a reference the real build keeps.
    let src = r#"<Project>
  <ItemGroup>
    <ProjectReference Include="../Lib/Lib.fsproj">
      <ReferenceOutputAssembly Condition="'$(NoSuchProp)' == 'on'">false</ReferenceOutputAssembly>
    </ProjectReference>
    <ProjectReference Include="../Tool/Tool.fsproj">
      <ReferenceOutputAssembly Condition="'$(NoSuchProp)' == ''">false</ReferenceOutputAssembly>
    </ProjectReference>
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert_eq!(p.project_references.len(), 2);
    assert!(
        p.project_references[0].reference_output_assembly == ItemMetadataValue::ABSENT,
        "a false-conditioned metadata child must read as absent"
    );
    assert_eq!(
        p.project_references[1].reference_output_assembly,
        ItemMetadataValue::known("false"),
        "a true-conditioned metadata child applies"
    );
}

#[test]
fn project_reference_mutations_mark_the_list_uncertain() {
    // We don't model item mutation, so earlier Includes stand un-mutated in
    // `project_references` — but MSBuild honours the mutation (dotnet 10
    // probe: a Remove, or an Update writing ReferenceOutputAssembly=false,
    // strips the reference from ReferencePath). The flag is what stops a
    // reference-semantics consumer folding a DLL from the stale list.
    for mutation in [
        r#"<ProjectReference Remove="../B/B.fsproj" />"#,
        r#"<ProjectReference Update="../B/B.fsproj" ReferenceOutputAssembly="false" />"#,
        // A mutation behind a condition we can't evaluate may still run in
        // the real build.
        r#"<ProjectReference Remove="../B/B.fsproj" Condition="Exists('x')" />"#,
    ] {
        let src = format!(
            r#"<Project>
  <ItemGroup>
    <ProjectReference Include="../B/B.fsproj" />
    {mutation}
  </ItemGroup>
</Project>"#
        );
        let p = parse(&src);
        assert!(
            p.project_references_uncertain,
            "{mutation}: the un-mutated list must not be trusted"
        );
    }

    // A false-conditioned mutation is definitely not executed; a Compile
    // mutation can't change the reference list. Both stay certain.
    for benign in [
        r#"<ProjectReference Remove="../B/B.fsproj" Condition="'$(NoSuchProp)' == 'on'" />"#,
        r#"<Compile Update="A.fs"><Link>display.fs</Link></Compile>"#,
    ] {
        let src = format!(
            r#"<Project>
  <ItemGroup>
    <Compile Include="A.fs" />
    <ProjectReference Include="../B/B.fsproj" />
    {benign}
  </ItemGroup>
</Project>"#
        );
        let p = parse(&src);
        assert!(
            !p.project_references_uncertain,
            "{benign}: must not poison the reference list"
        );
        assert_eq!(p.project_references.len(), 1);
    }
}

#[test]
fn item_definition_group_project_reference_default_marks_the_list_uncertain() {
    // dotnet 10 probe (2026-07-10): an `<ItemDefinitionGroup>` default —
    // here `ReferenceOutputAssembly=false` — lands on every
    // `<ProjectReference>` item (visible in `-getItem:ProjectReference`)
    // and empties `ReferencePath`. We don't thread item-definition
    // defaults into captured items, so the captured metadata still reads
    // as a full reference; the flag is what stops a reference-semantics
    // consumer folding a DLL MSBuild excludes.
    let src = r#"<Project>
  <ItemDefinitionGroup>
    <ProjectReference>
      <ReferenceOutputAssembly>false</ReferenceOutputAssembly>
    </ProjectReference>
  </ItemDefinitionGroup>
  <ItemGroup>
    <ProjectReference Include="../B/B.fsproj" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert!(
        p.project_references_uncertain,
        "an item-definition default may rewrite every reference's metadata"
    );

    // The default reaches items declared *before* the group too: MSBuild
    // evaluates all ItemDefinitionGroups (pass 2) before any item (pass 3).
    let src = r#"<Project>
  <ItemGroup>
    <ProjectReference Include="../B/B.fsproj" />
  </ItemGroup>
  <ItemDefinitionGroup>
    <ProjectReference>
      <ReferenceOutputAssembly>false</ReferenceOutputAssembly>
    </ProjectReference>
  </ItemDefinitionGroup>
</Project>"#;
    let p = parse(src);
    assert!(
        p.project_references_uncertain,
        "item definitions apply regardless of document order"
    );

    // Benign shapes: a cleanly-false group or metadata condition means the
    // default cannot apply in any build; a definition with no metadata
    // defines nothing; a Compile-only definition can't touch references.
    for benign in [
        r#"<ItemDefinitionGroup Condition="'$(NoSuchProp)' == 'on'">
    <ProjectReference><ReferenceOutputAssembly>false</ReferenceOutputAssembly></ProjectReference>
  </ItemDefinitionGroup>"#,
        r#"<ItemDefinitionGroup>
    <ProjectReference><ReferenceOutputAssembly Condition="'$(NoSuchProp)' == 'on'">false</ReferenceOutputAssembly></ProjectReference>
  </ItemDefinitionGroup>"#,
        r#"<ItemDefinitionGroup>
    <ProjectReference></ProjectReference>
  </ItemDefinitionGroup>"#,
        r#"<ItemDefinitionGroup>
    <Compile><Visible>false</Visible></Compile>
  </ItemDefinitionGroup>"#,
    ] {
        let src = format!(
            r#"<Project>
  {benign}
  <ItemGroup>
    <ProjectReference Include="../B/B.fsproj" />
  </ItemGroup>
</Project>"#
        );
        let p = parse(&src);
        assert!(
            !p.project_references_uncertain,
            "{benign}: cannot change any reference's semantics"
        );
    }
}

#[test]
fn item_definition_group_project_reference_inert_default_stays_certain() {
    // The real F# SDK's `<ItemDefinitionGroup><ProjectReference>` (dotnet 10,
    // `Microsoft.Common.CurrentVersion.targets`) sets only inert defaults:
    // `<Targets>$(ProjectReferenceBuildTargets)</Targets>` (that property is
    // blank by default → MSBuild's own default targets), an empty
    // `<OutputItemType/>`, and `<ReferenceSourceTarget>ProjectReference</…>`.
    // Probed (dotnet 10): none of these removes the target from
    // `ReferencePath`, so the captured reference is exactly what the real build
    // sees. A guard that flagged *any* ItemDefinition metadata would poison the
    // reference list of essentially every real SDK project; only an
    // *edge-affecting* default (a modelled asset/output metadatum, or the
    // significant P2P vocabulary with a non-empty value) may.
    let src = r#"<Project>
  <ItemDefinitionGroup>
    <ProjectReference>
      <Targets>$(ProjectReferenceBuildTargets)</Targets>
      <OutputItemType />
      <ReferenceSourceTarget>ProjectReference</ReferenceSourceTarget>
    </ProjectReference>
  </ItemDefinitionGroup>
  <ItemGroup>
    <ProjectReference Include="../B/B.fsproj" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert!(
        !p.project_references_uncertain,
        "the SDK's own inert ProjectReference item-definition must not poison the list"
    );
    assert_eq!(
        p.project_references.len(),
        1,
        "the reference is captured, not dropped"
    );

    // Each inert shape in isolation stays certain: a name that is inert in
    // MSBuild's P2P protocol regardless of value (`OutputItemType`,
    // `ReferenceSourceTarget`); or any name whose evaluated value is empty —
    // MSBuild's own default (an explicit empty element, or an exact-empty
    // property expansion).
    for inert in [
        r#"<ProjectReference><OutputItemType>Content</OutputItemType></ProjectReference>"#,
        r#"<ProjectReference><ReferenceSourceTarget>ProjectReference</ReferenceSourceTarget></ProjectReference>"#,
        r#"<ProjectReference><Targets></Targets></ProjectReference>"#,
        r#"<ProjectReference><Targets>$(NoSuchProp)</Targets></ProjectReference>"#,
        r#"<ProjectReference OutputItemType="Content" />"#,
    ] {
        let src = format!(
            r#"<Project>
  <ItemDefinitionGroup>
    {inert}
  </ItemDefinitionGroup>
  <ItemGroup>
    <ProjectReference Include="../B/B.fsproj" />
  </ItemGroup>
</Project>"#
        );
        let p = parse(&src);
        assert!(
            !p.project_references_uncertain,
            "{inert}: inert default must not poison the reference list"
        );
    }
}

#[test]
fn item_definition_group_project_reference_edge_affecting_default_marks_uncertain() {
    // The soundness half: a non-inert-by-name default with a real value still
    // poisons the captured list, because we do not thread item-definition
    // defaults into the capture. This covers the modelled asset/output metadata
    // and the significant P2P vocabulary (`Targets="Clean"`,
    // `SetTargetFramework`, `SkipGetPlatformProperties`, …), *and*, in the
    // conservative direction, any unmodelled name we have not proven inert
    // (`Private`, a custom name): declining those only under-resolves, and the
    // house rule prefers that to trusting an edge MSBuild redirected. An
    // untrusted/inexact value (the `$(TargetFramework)` carve-out) is likewise
    // unknowable and declines.
    for edge_affecting in [
        r#"<ProjectReference><ReferenceOutputAssembly>false</ReferenceOutputAssembly></ProjectReference>"#,
        r#"<ProjectReference><PrivateAssets>all</PrivateAssets></ProjectReference>"#,
        r#"<ProjectReference><ExcludeAssets>compile</ExcludeAssets></ProjectReference>"#,
        r#"<ProjectReference><IncludeAssets>runtime</IncludeAssets></ProjectReference>"#,
        r#"<ProjectReference><Targets>Clean</Targets></ProjectReference>"#,
        r#"<ProjectReference><SetTargetFramework>TargetFramework=net8.0</SetTargetFramework></ProjectReference>"#,
        r#"<ProjectReference><SkipGetPlatformProperties>true</SkipGetPlatformProperties></ProjectReference>"#,
        r#"<ProjectReference ReferenceOutputAssembly="false" />"#,
        // Conservatively declined (unmodelled-but-inert in MSBuild):
        r#"<ProjectReference><Private>false</Private></ProjectReference>"#,
        r#"<ProjectReference><CustomThing>whatever</CustomThing></ProjectReference>"#,
        // Inexact/untrusted value on a non-inert name: unknowable → decline.
        r#"<ProjectReference><Targets>$(TargetFramework)</Targets></ProjectReference>"#,
    ] {
        let src = format!(
            r#"<Project>
  <ItemDefinitionGroup>
    {edge_affecting}
  </ItemDefinitionGroup>
  <ItemGroup>
    <ProjectReference Include="../B/B.fsproj" />
  </ItemGroup>
</Project>"#
        );
        let p = parse(&src);
        assert!(
            p.project_references_uncertain,
            "{edge_affecting}: an edge-affecting default may rewrite the reference"
        );
    }
}

#[test]
fn untrusted_group_condition_hiding_a_reference_mutation_marks_uncertain() {
    // dotnet 10 probe (2026-07-10): an `<ItemGroup>` gated on a condition
    // outside our grammar (`$([MSBuild]::Add(1, 1)) == 2`, true in the real
    // build) containing `<ProjectReference Update
    // ReferenceOutputAssembly=false>` empties `ReferencePath`. We skip the
    // whole group, leaving the earlier Include captured un-mutated — the
    // list can't be trusted.
    for hidden_mutation in [
        r#"<ProjectReference Update="../B/B.fsproj" ReferenceOutputAssembly="false" />"#,
        r#"<ProjectReference Remove="../B/B.fsproj" />"#,
    ] {
        let src = format!(
            r#"<Project>
  <ItemGroup>
    <ProjectReference Include="../B/B.fsproj" />
  </ItemGroup>
  <ItemGroup Condition="Exists('x')">
    {hidden_mutation}
  </ItemGroup>
</Project>"#
        );
        let p = parse(&src);
        assert!(
            p.project_references_uncertain,
            "{hidden_mutation}: may run in the real build"
        );
    }

    // Benign shapes: an Include-only child under an unsupported group gate
    // is at worst a missed reference (under-resolve, never wrong); a
    // mutation whose own condition is cleanly false cannot run; a cleanly
    // false group gate is skipped by MSBuild too.
    for (group_condition, child) in [
        (
            "Exists('x')",
            r#"<ProjectReference Include="../C/C.fsproj" />"#,
        ),
        (
            "Exists('x')",
            r#"<ProjectReference Remove="../B/B.fsproj" Condition="'$(NoSuchProp)' == 'on'" />"#,
        ),
        (
            "'$(NoSuchProp)' == 'on'",
            r#"<ProjectReference Remove="../B/B.fsproj" />"#,
        ),
    ] {
        let src = format!(
            r#"<Project>
  <ItemGroup>
    <ProjectReference Include="../B/B.fsproj" />
  </ItemGroup>
  <ItemGroup Condition="{group_condition}">
    {child}
  </ItemGroup>
</Project>"#
        );
        let p = parse(&src);
        assert!(
            !p.project_references_uncertain,
            "Condition={group_condition} child={child}: must not poison the list"
        );
        assert_eq!(p.project_references.len(), 1);
    }
}

#[test]
fn undecided_choose_branch_hiding_a_reference_mutation_marks_uncertain() {
    // An undecided `<When>` gate means every still-possible branch may run
    // in the real build; a `<ProjectReference Update/Remove>` inside one
    // mutates a list we captured un-mutated.
    for (when_condition, branch_item, otherwise) in [
        (
            "Exists('x')",
            r#"<ProjectReference Remove="../B/B.fsproj" />"#,
            "",
        ),
        (
            "Exists('x')",
            r#"<ProjectReference Update="../B/B.fsproj" ReferenceOutputAssembly="false" />"#,
            "",
        ),
        // The mutation hides in the <Otherwise> of an undecided chain.
        (
            "Exists('x')",
            r#"<ProjectReference Include="../C/C.fsproj" />"#,
            r#"<Otherwise><ItemGroup><ProjectReference Remove="../B/B.fsproj" /></ItemGroup></Otherwise>"#,
        ),
    ] {
        let src = format!(
            r#"<Project>
  <ItemGroup>
    <ProjectReference Include="../B/B.fsproj" />
  </ItemGroup>
  <Choose>
    <When Condition="{when_condition}">
      <ItemGroup>
        {branch_item}
      </ItemGroup>
    </When>
    {otherwise}
  </Choose>
</Project>"#
        );
        let p = parse(&src);
        assert!(
            p.project_references_uncertain,
            "When={when_condition} item={branch_item} otherwise={otherwise}: \
             a possible branch mutates the list"
        );
    }

    // Choose's gate policy is stricter than an item gate's: an inexact
    // read makes the *decision itself* undecided (`maybe_wrong` in
    // `handle_choose`), the same treatment the Compile and package sets
    // already get there — so a mutation in that branch still flags. The
    // vehicle is the `TargetFramework` carve-out, which stays inexact under
    // C.2b (a plain undefined name would now decide the gate exactly).
    let src = r#"<Project>
  <ItemGroup>
    <ProjectReference Include="../B/B.fsproj" />
  </ItemGroup>
  <Choose>
    <When Condition="'$(TargetFramework)' == 'on'">
      <ItemGroup>
        <ProjectReference Remove="../B/B.fsproj" />
      </ItemGroup>
    </When>
  </Choose>
</Project>"#;
    let p = parse(src);
    assert!(
        p.project_references_uncertain,
        "an inexact read undecides the whole Choose"
    );

    // Benign shapes: an Include-only undecided branch is at worst a missed
    // reference; a When cleanly false on a *defined* property never runs.
    for (prelude, when_condition, branch_item) in [
        (
            "",
            "Exists('x')",
            r#"<ProjectReference Include="../C/C.fsproj" />"#,
        ),
        (
            "<PropertyGroup><Flag>off</Flag></PropertyGroup>",
            "'$(Flag)' == 'on'",
            r#"<ProjectReference Remove="../B/B.fsproj" />"#,
        ),
    ] {
        let src = format!(
            r#"<Project>
  {prelude}
  <ItemGroup>
    <ProjectReference Include="../B/B.fsproj" />
  </ItemGroup>
  <Choose>
    <When Condition="{when_condition}">
      <ItemGroup>
        {branch_item}
      </ItemGroup>
    </When>
  </Choose>
</Project>"#
        );
        let p = parse(&src);
        assert!(
            !p.project_references_uncertain,
            "When={when_condition} item={branch_item}: must not poison the list"
        );
    }
}

#[test]
fn unpinned_condition_on_a_reference_operation_marks_uncertain() {
    // `KeepB` is written under a gate we can't evaluate, so its value is
    // unpinned: any condition reading it may take the other branch in a
    // real build. A cleanly-false mutation gate reading it is therefore
    // not a trustworthy exclusion (contrast the pinned
    // `'$(NoSuchProp)' == 'on'` cases in
    // `project_reference_mutations_mark_the_list_uncertain`: a genuinely
    // unset property is deterministic under the environment model).
    let unpinned_prelude = r#"<PropertyGroup Condition="Exists('q')">
    <KeepB>true</KeepB>
  </PropertyGroup>"#;
    for op in [
        // Element-level: the mutation's own gate leans on the unpinned value.
        r#"<ItemGroup>
    <ProjectReference Remove="../B/B.fsproj" Condition="'$(KeepB)' == 'true'" />
  </ItemGroup>"#,
        // Group-level: the enclosing gate leans on it.
        r#"<ItemGroup Condition="'$(KeepB)' == 'true'">
    <ProjectReference Remove="../B/B.fsproj" />
  </ItemGroup>"#,
        // Capture side: an Include we *kept* under an unpinned-true gate may
        // not exist in the real build — a phantom edge, not an under-resolve.
        r#"<ItemGroup>
    <ProjectReference Include="../C/C.fsproj" Condition="'$(KeepB)' == ''" />
  </ItemGroup>"#,
        r#"<ItemGroup Condition="'$(KeepB)' == ''">
    <ProjectReference Include="../C/C.fsproj" />
  </ItemGroup>"#,
    ] {
        let src = format!(
            r#"<Project>
  {unpinned_prelude}
  <ItemGroup>
    <ProjectReference Include="../B/B.fsproj" />
  </ItemGroup>
  {op}
</Project>"#
        );
        let p = parse(&src);
        assert!(
            p.project_references_uncertain,
            "{op}: a gate leaning on an unpinned property is not trustworthy"
        );
    }

    // An Include *dropped* by an unpinned-false gate is at worst a missed
    // reference: the captured list over-claims nothing.
    let src = format!(
        r#"<Project>
  {unpinned_prelude}
  <ItemGroup>
    <ProjectReference Include="../B/B.fsproj" />
    <ProjectReference Include="../C/C.fsproj" Condition="'$(KeepB)' == 'true'" />
  </ItemGroup>
</Project>"#
    );
    let p = parse(&src);
    assert!(
        !p.project_references_uncertain,
        "a dropped Include under-resolves; it cannot fabricate"
    );
    assert_eq!(p.project_references.len(), 1);
}

#[test]
fn unmodelled_significant_reference_metadata_is_captured() {
    // dotnet 10 probes (2026-07-10, prebuilt target, entry edge,
    // `-t:ResolveReferences -getItem:ReferencePath`): `BuildReference="false"`
    // and `Targets="Clean"` both remove the target from `ReferencePath`,
    // while custom metadata, `OutputItemType`, `Private="false"`, and
    // `SetTargetFramework` keep it. The suppressing / evaluation-mutating
    // vocabulary is significant to the P2P protocol but unmodelled here, so
    // its presence must surface for the compile-closure walk to drop the
    // edge rather than fold a DLL the compiler never sees.
    for (metadata, flagged) in [
        (r#"BuildReference="false""#, true),
        (r#"Targets="Clean""#, true),
        (r#"SetTargetFramework="TargetFramework=net8.0""#, true),
        (r#"AdditionalProperties="Configuration=Release""#, true),
        // Probed-inert names stay trusted.
        (r#"Private="false""#, false),
        (r#"Foo="bar""#, false),
        (r#"OutputItemType="Analyzer""#, false),
    ] {
        let src = format!(
            r#"<Project>
  <ItemGroup>
    <ProjectReference Include="../B/B.fsproj" {metadata} />
  </ItemGroup>
</Project>"#
        );
        let p = parse(&src);
        assert_eq!(
            p.project_references[0].unmodelled_reference_metadata, flagged,
            "{metadata}"
        );
        assert!(
            !p.project_references_uncertain,
            "{metadata}: per-item, not global"
        );
    }

    // Child-element form: a cleanly-false condition means the metadatum
    // cannot apply; an applicable one flags.
    for (condition, flagged) in [("'$(NoSuchProp)' == 'on'", false), ("", true)] {
        let cond_attr = if condition.is_empty() {
            String::new()
        } else {
            format!(r#" Condition="{condition}""#)
        };
        let src = format!(
            r#"<Project>
  <ItemGroup>
    <ProjectReference Include="../B/B.fsproj">
      <BuildReference{cond_attr}>false</BuildReference>
    </ProjectReference>
  </ItemGroup>
</Project>"#
        );
        let p = parse(&src);
        assert_eq!(
            p.project_references[0].unmodelled_reference_metadata, flagged,
            "condition={condition:?}"
        );
    }
}

#[test]
fn malformed_choose_marks_the_reference_list_uncertain() {
    // MSBuild validates the whole `<Choose>` tree at load time (pinned with
    // stub projects — see `handle_choose`): a malformed one (here a `<When>`
    // without `Condition`, MSB4035) fails the real evaluation before
    // anything runs, so there is no build for the captured reference list
    // to describe. The undecided-branch mutation scan never runs on this
    // path, so the flag must be set structurally.
    let src = r#"<Project>
  <ItemGroup>
    <ProjectReference Include="../B/B.fsproj" />
  </ItemGroup>
  <Choose>
    <When>
      <ItemGroup>
        <ProjectReference Remove="../B/B.fsproj" />
      </ItemGroup>
    </When>
  </Choose>
</Project>"#;
    let p = parse(src);
    assert!(
        p.project_references_uncertain,
        "a malformed Choose fails the whole real evaluation"
    );
}

#[test]
fn unpinned_include_value_marks_the_reference_list_uncertain() {
    // `RefPath` is *written* (the gate evaluated true) but only by treating
    // `$(TargetFramework)` — the carve-out that stays inexact under C.2b —
    // as empty; the real build may skip that group, leaving `RefPath` empty
    // and the reference unmade. The expansion is clean
    // (`had_issue() == false`), so without consulting the pin state the
    // captured edge would look trustworthy — a phantom edge.
    let src = r#"<Project>
  <PropertyGroup Condition="'$(TargetFramework)' == ''">
    <RefPath>../B/B.fsproj</RefPath>
  </PropertyGroup>
  <ItemGroup>
    <ProjectReference Include="$(RefPath)" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert_eq!(p.project_references.len(), 1, "the edge is still captured");
    assert!(
        p.project_references_uncertain,
        "an Include expanded from an unpinned property may not exist in the real build"
    );

    // A cleanly-pinned property value keeps the list certain.
    let src = r#"<Project>
  <PropertyGroup>
    <RefPath>../B/B.fsproj</RefPath>
  </PropertyGroup>
  <ItemGroup>
    <ProjectReference Include="$(RefPath)" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert_eq!(p.project_references.len(), 1);
    assert!(
        !p.project_references_uncertain,
        "a pinned property value is exact"
    );
}

#[test]
fn untrusted_metadata_gate_or_value_reads_as_unknown() {
    // `Flag` is written under a gate decided only by an inexact read (the
    // `TargetFramework` carve-out), so its value is unpinned: a metadata
    // write conditioned on it (in either direction), or a metadata value
    // expanded from it, may differ in the real build. Reading such metadata
    // as Known would let the compile-closure walk keep an edge MSBuild drops
    // (probed: `ReferenceOutputAssembly=false` empties `ReferencePath`).
    let prelude = r#"<PropertyGroup Condition="'$(TargetFramework)' == ''">
    <Flag>true</Flag>
  </PropertyGroup>"#;
    // Untrusted-false metadata condition: the real build may take the write.
    let src = format!(
        r#"<Project>
  {prelude}
  <ItemGroup>
    <ProjectReference Include="../B/B.fsproj">
      <ReferenceOutputAssembly Condition="'$(Flag)' == 'x'">false</ReferenceOutputAssembly>
    </ProjectReference>
  </ItemGroup>
</Project>"#
    );
    let p = parse(&src);
    assert_eq!(
        p.project_references[0].reference_output_assembly,
        ItemMetadataValue::Unknown,
        "a metadata write behind an untrusted-false gate may apply in the real build"
    );

    // Untrusted-true metadata condition: the real build may *skip* the write.
    let src = format!(
        r#"<Project>
  {prelude}
  <ItemGroup>
    <ProjectReference Include="../B/B.fsproj">
      <ReferenceOutputAssembly Condition="'$(Flag)' == 'true'">false</ReferenceOutputAssembly>
    </ProjectReference>
  </ItemGroup>
</Project>"#
    );
    let p = parse(&src);
    assert_eq!(
        p.project_references[0].reference_output_assembly,
        ItemMetadataValue::Unknown,
        "a metadata write behind an untrusted-true gate may not apply in the real build"
    );

    // Unpinned metadata VALUE — child element and attribute forms.
    for metadata in [
        r#"<ProjectReference Include="../B/B.fsproj"><ExcludeAssets>$(Flag)</ExcludeAssets></ProjectReference>"#,
        r#"<ProjectReference Include="../B/B.fsproj" ExcludeAssets="$(Flag)" />"#,
    ] {
        let src = format!(
            r#"<Project>
  {prelude}
  <ItemGroup>
    {metadata}
  </ItemGroup>
</Project>"#
        );
        let p = parse(&src);
        assert_eq!(
            p.project_references[0].exclude_assets,
            ItemMetadataValue::Unknown,
            "{metadata}: a value expanded from an unpinned property may differ in the real build"
        );
    }

    // A later clean write still restores Known (round-10 semantics).
    let src = format!(
        r#"<Project>
  {prelude}
  <ItemGroup>
    <ProjectReference Include="../B/B.fsproj">
      <ExcludeAssets>$(Flag)</ExcludeAssets>
      <ExcludeAssets>runtime</ExcludeAssets>
    </ProjectReference>
  </ItemGroup>
</Project>"#
    );
    let p = parse(&src);
    assert_eq!(
        p.project_references[0].exclude_assets,
        ItemMetadataValue::known("runtime"),
        "a later clean write overwrites whatever the untrusted one did"
    );
}

#[test]
fn unresolved_import_marks_the_reference_list_uncertain() {
    // Pure mode never follows imports, so every `<Import>` is a file we
    // never see — it may contain anything, `<ProjectReference
    // Update/Remove>` included: the same structural risk that already
    // poisons the Compile and package sets.
    let src = r#"<Project>
  <Import Project="custom.props" />
  <ItemGroup>
    <ProjectReference Include="../B/B.fsproj" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert!(
        p.project_references_uncertain,
        "an unfollowed import may mutate the reference list"
    );
}

#[test]
fn untrusted_import_gate_marks_the_reference_list_uncertain() {
    // Follow mode: the import's own gate decides. A gate outside our
    // grammar may be true in the real build, importing a file that mutates
    // the reference list; a cleanly-false gate (undefined property, or a
    // real `Exists` miss) is skipped by MSBuild too.
    let tmp = tempfile::TempDir::new().unwrap();
    let fsproj = tmp.path().join("Gate.fsproj");
    std::fs::write(tmp.path().join("side.props"), "<Project></Project>").unwrap();
    for (condition, uncertain) in [
        ("$([MSBuild]::Add(1, 1)) == 2", true),
        ("'$(NoSuchProp)' == 'on'", false),
        ("Exists('missing.props')", false),
    ] {
        let source = format!(
            r#"<Project>
  <Import Project="side.props" Condition="{condition}" />
  <ItemGroup>
    <ProjectReference Include="../B/B.fsproj" />
  </ItemGroup>
</Project>"#
        );
        std::fs::write(&fsproj, &source).unwrap();
        let p = super::parse_fsproj_with_imports(
            &source,
            &fsproj,
            &HashMap::new(),
            &HashMap::new(),
            None,
            None,
        )
        .expect("parses");
        assert_eq!(
            p.project_references_uncertain, uncertain,
            "Condition={condition}"
        );
    }
}

#[test]
fn unevaluable_metadata_write_reads_as_unknown() {
    // A write whose applicability or value we can't evaluate may take
    // effect in the real build; reading it as "absent" would let the
    // compile-closure walk keep an edge MSBuild drops. A later clean write
    // overwrites whatever the unevaluable one did, restoring Known.
    let src = r#"<Project>
  <ItemGroup>
    <ProjectReference Include="../A/A.fsproj">
      <ReferenceOutputAssembly Condition="Exists('tool.lock')">false</ReferenceOutputAssembly>
    </ProjectReference>
    <ProjectReference Include="../B/B.fsproj">
      <ExcludeAssets>$(TargetFramework)</ExcludeAssets>
    </ProjectReference>
    <ProjectReference Include="../C/C.fsproj">
      <ExcludeAssets>@(SomeItems)</ExcludeAssets>
    </ProjectReference>
    <ProjectReference Include="../D/D.fsproj">
      <ReferenceOutputAssembly Condition="Exists('tool.lock')">false</ReferenceOutputAssembly>
      <ReferenceOutputAssembly>true</ReferenceOutputAssembly>
    </ProjectReference>
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert_eq!(p.project_references.len(), 4);
    assert_eq!(
        p.project_references[0].reference_output_assembly,
        ItemMetadataValue::Unknown,
        "unsupported condition: the real build may satisfy it"
    );
    assert_eq!(
        p.project_references[1].exclude_assets,
        ItemMetadataValue::Unknown,
        "inexact carve-out read: the value is not pinned"
    );
    assert_eq!(
        p.project_references[2].exclude_assets,
        ItemMetadataValue::Unknown,
        "item reference: we don't evaluate @()"
    );
    assert_eq!(
        p.project_references[3].reference_output_assembly,
        ItemMetadataValue::known("true"),
        "a later clean write overwrites the unevaluable one"
    );
}

#[test]
fn duplicate_identity_package_update_marks_the_set_uncertain() {
    // MSBuild quirk (dotnet 10 probe, found by the generative oracle): a
    // `<PackageReference Update>` whose spec lists the same identity twice
    // (`Update="Gamma;Gamma"`) goes through the lazy evaluator's
    // dictionary path, which is position-independent — it updates even an
    // `Include` declared *later*. A unique spec stays position-sensitive
    // (an Update before the Include is a no-op: probed for `Update="Gamma"`
    // and `Update="Delta;Gamma"`). We model only the ordered semantics, so
    // a duplicate-identity Update must mark the captured set uncertain
    // rather than silently disagree with the real build.
    let src = r#"<Project>
  <ItemGroup>
    <PackageReference Update="Gamma;Gamma" VersionOverride="1.0" />
    <PackageReference Include="Alpha;Gamma" Version="1.0" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert!(
        p.package_references_uncertain,
        "a duplicate-identity Update engages MSBuild's position-independent \
         path we don't model"
    );
    assert!(p.package_reference_uncertainties.iter().any(|c| matches!(
        &c.kind,
        PackageReferenceUncertaintyCauseKind::DuplicateUpdateIdentity { id } if id == "Gamma"
    )));

    // The unique-spec forms keep the ordered semantics and stay certain: an
    // Update preceding the Include modifies nothing.
    for update in ["Gamma", "Delta;Gamma"] {
        let src = format!(
            r#"<Project>
  <ItemGroup>
    <PackageReference Update="{update}" VersionOverride="1.0" />
    <PackageReference Include="Alpha;Gamma" Version="1.0" />
  </ItemGroup>
</Project>"#
        );
        let p = parse(&src);
        assert!(
            !p.package_references_uncertain,
            "unique Update spec {update:?} must stay certain: {:?}",
            p.package_reference_uncertainties
        );
        let gamma = p
            .package_references
            .iter()
            .find(|r| r.id == "Gamma")
            .expect("Gamma captured");
        assert_eq!(
            gamma.version_override, None,
            "an Update before the Include is a no-op for spec {update:?}"
        );
    }
}

#[test]
fn project_reference_carries_include_and_private_assets() {
    // `IncludeAssets` / `PrivateAssets` control what flows *through* a
    // `<ProjectReference>` (dotnet 10 probes: a non-entry edge whose compile
    // asset is not included, or is private, is invisible to grandparents).
    // Both metadata spellings must be captured; Compile items don't carry
    // them.
    let src = r#"<Project>
  <ItemGroup>
    <ProjectReference Include="../A/A.fsproj" IncludeAssets="runtime" PrivateAssets="all" />
    <ProjectReference Include="../B/B.fsproj">
      <IncludeAssets>compile;runtime</IncludeAssets>
      <PrivateAssets>compile</PrivateAssets>
    </ProjectReference>
    <ProjectReference Include="../C/C.fsproj" />
    <Compile Include="X.fs" IncludeAssets="all" PrivateAssets="all" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert_eq!(p.project_references.len(), 3);
    assert_eq!(
        p.project_references[0].include_assets,
        ItemMetadataValue::known("runtime"),
    );
    assert_eq!(
        p.project_references[0].private_assets,
        ItemMetadataValue::known("all"),
    );
    assert_eq!(
        p.project_references[1].include_assets,
        ItemMetadataValue::known("compile;runtime"),
    );
    assert_eq!(
        p.project_references[1].private_assets,
        ItemMetadataValue::known("compile"),
    );
    assert!(p.project_references[2].include_assets == ItemMetadataValue::ABSENT);
    assert!(p.project_references[2].private_assets == ItemMetadataValue::ABSENT);
    assert_eq!(p.items.len(), 1);
    assert!(p.items[0].include_assets == ItemMetadataValue::ABSENT);
    assert!(p.items[0].private_assets == ItemMetadataValue::ABSENT);
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
}

#[test]
fn empty_metadata_child_clears_an_earlier_attribute_write() {
    // MSBuild ground truth (dotnet 10 probe, 2026-07): a metadata child
    // element whose condition holds is a later write even when its text is
    // empty — the metadata value becomes "" (indistinguishable from unset in
    // %() expansion), so `ReferenceOutputAssembly="false"` followed by an
    // empty `<ReferenceOutputAssembly/>` child leaves a reference the real
    // build *keeps*. Whitespace-only inner text also reads as empty (the
    // XML loader treats it as insignificant), and so does comment-only
    // content.
    let src = r#"<Project>
  <ItemGroup>
    <ProjectReference Include="../A/A.fsproj" ReferenceOutputAssembly="false">
      <ReferenceOutputAssembly></ReferenceOutputAssembly>
    </ProjectReference>
    <ProjectReference Include="../B/B.fsproj" ReferenceOutputAssembly="false">
      <ReferenceOutputAssembly />
    </ProjectReference>
    <ProjectReference Include="../C/C.fsproj" ExcludeAssets="compile">
      <ExcludeAssets>
      </ExcludeAssets>
    </ProjectReference>
    <ProjectReference Include="../D/D.fsproj" ReferenceOutputAssembly="false">
      <ReferenceOutputAssembly><!-- cleared --></ReferenceOutputAssembly>
    </ProjectReference>
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert_eq!(p.project_references.len(), 4);
    for (i, field) in [
        (0, &p.project_references[0].reference_output_assembly),
        (1, &p.project_references[1].reference_output_assembly),
        (2, &p.project_references[2].exclude_assets),
        (3, &p.project_references[3].reference_output_assembly),
    ] {
        assert!(
            *field == ItemMetadataValue::ABSENT,
            "item {i}: an empty child element is a later write that clears \
             the earlier attribute value, got {field:?}"
        );
    }
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
}

#[test]
fn empty_then_value_metadata_children_are_last_write_wins() {
    // An empty child clears, but a subsequent non-empty child is a still
    // later write — the last assignment wins.
    let src = r#"<Project>
  <ItemGroup>
    <ProjectReference Include="../A/A.fsproj" ExcludeAssets="runtime">
      <ExcludeAssets></ExcludeAssets>
      <ExcludeAssets>compile</ExcludeAssets>
    </ProjectReference>
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert_eq!(p.project_references.len(), 1);
    assert_eq!(
        p.project_references[0].exclude_assets,
        ItemMetadataValue::known("compile"),
    );
}

#[test]
fn false_conditioned_empty_metadata_child_is_not_a_write() {
    // The clearing write is still gated on the child's own Condition: a
    // false-conditioned empty child is simply not an assignment, so the
    // attribute value survives (MSBuild probe: Bar stays 'attr').
    let src = r#"<Project>
  <ItemGroup>
    <ProjectReference Include="../A/A.fsproj" ReferenceOutputAssembly="false">
      <ReferenceOutputAssembly Condition="'$(NoSuchProp)' == 'on'"></ReferenceOutputAssembly>
    </ProjectReference>
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert_eq!(p.project_references.len(), 1);
    assert_eq!(
        p.project_references[0].reference_output_assembly,
        ItemMetadataValue::known("false"),
    );
}

#[test]
fn metadata_expanding_to_empty_reads_as_unset() {
    // A value that *expands* to empty is the same empty write as a literally
    // empty element — and an attribute written as "" was never a value at
    // all. Neither may surface as Some("") when MSBuild's own
    // GetMetadataValue reports "" for set-empty and unset alike.
    let src = r#"<Project>
  <PropertyGroup>
    <Empty></Empty>
  </PropertyGroup>
  <ItemGroup>
    <ProjectReference Include="../A/A.fsproj" ReferenceOutputAssembly="false">
      <ReferenceOutputAssembly>$(Empty)</ReferenceOutputAssembly>
    </ProjectReference>
    <ProjectReference Include="../B/B.fsproj" ExcludeAssets="" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert_eq!(p.project_references.len(), 2);
    assert!(
        p.project_references[0].reference_output_assembly == ItemMetadataValue::ABSENT,
        "expands-to-empty child must clear the attribute, got {:?}",
        p.project_references[0].reference_output_assembly
    );
    assert!(
        p.project_references[1].exclude_assets == ItemMetadataValue::ABSENT,
        "an empty attribute is not a value, got {:?}",
        p.project_references[1].exclude_assets
    );
}

#[test]
fn project_reference_and_compile_coexist_in_same_itemgroup() {
    let src = r#"<Project>
  <ItemGroup>
    <Compile Include="A.fs" />
    <ProjectReference Include="../Other/Other.fsproj" />
    <Compile Include="B.fs" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert_eq!(
        paths(&p.items),
        [Path::new("/repo/proj/A.fs"), Path::new("/repo/proj/B.fs")],
    );
    let pr_paths: Vec<&Path> = p
        .project_references
        .iter()
        .map(|i| i.include.as_path())
        .collect();
    assert_eq!(pr_paths, [Path::new("/repo/proj/../Other/Other.fsproj")],);
}

#[test]
fn project_reference_substitutes_msbuild_thisfiledirectory() {
    let src = r#"<Project>
  <ItemGroup>
    <ProjectReference Include="$(MSBuildThisFileDirectory)../lib/Lib.csproj" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    // `$(MSBuildThisFileDirectory)` expands to the project directory
    // *with a trailing separator*; `PathBuf::join` collapses adjacent
    // separators but keeps `..` components literal.
    assert_eq!(
        p.project_references[0].include,
        PathBuf::from("/repo/proj/../lib/Lib.csproj"),
    );
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
}

#[test]
fn project_reference_with_semicolon_list_splits_into_multiple_items() {
    let src = r#"<Project>
  <ItemGroup>
    <ProjectReference Include="../A/A.csproj;../B/B.csproj" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    let pr_paths: Vec<&Path> = p
        .project_references
        .iter()
        .map(|i| i.include.as_path())
        .collect();
    assert_eq!(
        pr_paths,
        [
            Path::new("/repo/proj/../A/A.csproj"),
            Path::new("/repo/proj/../B/B.csproj"),
        ],
    );
}

#[test]
fn project_reference_backslashes_are_normalised() {
    let src = r#"<Project>
  <ItemGroup>
    <ProjectReference Include="..\lib\Lib.csproj" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert_eq!(
        p.project_references[0].include,
        PathBuf::from("/repo/proj/../lib/Lib.csproj"),
    );
}

#[test]
fn project_reference_obeys_condition_gating() {
    let src = r#"<Project>
  <ItemGroup>
    <ProjectReference Include="../always/Always.csproj" />
    <ProjectReference Include="../never/Never.csproj" Condition=" '$(X)' == 'yes' " />
  </ItemGroup>
</Project>"#;
    // `X` is supplied as a global property with the value "no", so the
    // condition evaluates to false and the second item is silently
    // excluded — same gate behaviour as `<Compile>`. Setting `X`
    // explicitly avoids the `UndefinedProperty` diagnostic an unset
    // reference would emit (a different aspect of the gate; tested
    // elsewhere).
    let mut extras = HashMap::new();
    extras.insert("X".to_string(), "no".to_string());
    let p = parse_fsproj(
        src,
        Path::new("/repo/proj/Demo.fsproj"),
        &extras,
        &HashMap::new(),
    )
    .expect("well-formed XML parses");
    let pr_paths: Vec<&Path> = p
        .project_references
        .iter()
        .map(|i| i.include.as_path())
        .collect();
    assert_eq!(pr_paths, [Path::new("/repo/proj/../always/Always.csproj")]);
    assert!(p.diagnostics.is_empty());
}

#[test]
fn project_reference_link_attribute_is_ignored() {
    // `<Link>` is meaningful for Compile but MSBuild does not treat it
    // as significant for ProjectReference; our parser drops it on the
    // floor (rather than surfacing a value with no effect on a real
    // build).
    let src = r#"<Project>
  <ItemGroup>
    <ProjectReference Include="../lib/Lib.csproj" Link="renamed/Lib.csproj" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert_eq!(p.project_references.len(), 1);
    assert!(p.project_references[0].link.is_none());
}

#[test]
fn project_reference_with_inexact_property_is_dropped() {
    // Same Include-resolution rule as Compile: an inexact property read
    // (`TargetFramework` is carved out, never provably unset) makes the
    // path corrupt, so the item is dropped and the project is marked
    // partial.
    let src = r#"<Project>
  <ItemGroup>
    <ProjectReference Include="$(TargetFramework)/lib/Lib.csproj" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert!(p.project_references.is_empty());
    assert_eq!(
        diag_kinds(&p.diagnostics),
        [&DiagnosticKind::UndefinedProperty {
            name: "TargetFramework".to_string(),
        }],
    );
    assert!(p.is_partial);
}

#[test]
fn project_reference_glob_is_diagnosed() {
    let src = r#"<Project>
  <ItemGroup>
    <ProjectReference Include="../**/*.csproj" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert!(p.project_references.is_empty());
    assert_eq!(
        diag_kinds(&p.diagnostics),
        [&DiagnosticKind::UnsupportedGlob {
            pattern: "../**/*.csproj".to_string()
        }]
    );
}

// -----------------------------------------------------------------
// Property test: bucket separation under random interleaving.
// -----------------------------------------------------------------

/// Build an fsproj XML string whose single `<ItemGroup>` lists `kinds`
/// in document order. Each `kind` slot becomes one item — `true` for
/// `<Compile>`, `false` for `<ProjectReference>` — with the index `i`
/// baked into the include path so the test can verify document-order
/// preservation per-bucket.
fn build_mixed_fsproj(kinds: &[bool]) -> (String, Vec<PathBuf>, Vec<PathBuf>) {
    let mut xml = String::from("<Project>\n  <ItemGroup>\n");
    let mut compile_paths = Vec::new();
    let mut pr_paths = Vec::new();
    for (i, &is_compile) in kinds.iter().enumerate() {
        if is_compile {
            xml.push_str(&format!("    <Compile Include=\"C{i}.fs\" />\n"));
            compile_paths.push(PathBuf::from(format!("/repo/proj/C{i}.fs")));
        } else {
            xml.push_str(&format!(
                "    <ProjectReference Include=\"../p{i}/P{i}.csproj\" />\n"
            ));
            pr_paths.push(PathBuf::from(format!("/repo/proj/../p{i}/P{i}.csproj")));
        }
    }
    xml.push_str("  </ItemGroup>\n</Project>\n");
    (xml, compile_paths, pr_paths)
}

proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config {
        cases: 128,
        ..proptest::test_runner::Config::default()
    })]

    /// For any interleaving of `<Compile>` and `<ProjectReference>` in
    /// a single ItemGroup, the parser must:
    ///   - place every Compile in `items` (and nothing else there);
    ///   - place every ProjectReference in `project_references` (and
    ///     nothing else there);
    ///   - preserve document order within each bucket.
    /// This is the bucket-separation invariant the slice is designed to
    /// enforce; mixing them up would silently corrupt either the
    /// Compile list (extra references treated as inputs) or the
    /// dependency list (Compile inputs treated as project deps).
    #[test]
    fn project_reference_and_compile_buckets_are_separated(
        kinds in proptest::collection::vec(proptest::bool::ANY, 1usize..=6)
    ) {
        let (xml, want_compile, want_pr) = build_mixed_fsproj(&kinds);
        let p = parse(&xml);
        proptest::prop_assert!(p.diagnostics.is_empty(), "diagnostics: {:?}", p.diagnostics);
        let got_compile: Vec<PathBuf> = p.items.iter().map(|i| i.include.clone()).collect();
        let got_pr: Vec<PathBuf> = p
            .project_references
            .iter()
            .map(|i| i.include.clone())
            .collect();
        proptest::prop_assert_eq!(got_compile, want_compile);
        proptest::prop_assert_eq!(got_pr, want_pr);
        for item in &p.items {
            proptest::prop_assert_eq!(item.kind, ItemKind::Compile);
        }
        for item in &p.project_references {
            proptest::prop_assert_eq!(item.kind, ItemKind::ProjectReference);
        }
    }
}

/// Distribution check (gospel.md: "assert the actual observed
/// distribution"). The bucket-separation property above only proves
/// useful if generated cases actually exercise the *mixed* regime —
/// otherwise the test could regress to all-Compile or all-ProjectReference
/// without anyone noticing.
///
/// With N uniform in 2..=6 and each slot independently 50/50, the
/// per-N probability of "both buckets non-empty" is
/// `1 - 2/2^N` ∈ {0.5, 0.75, 0.875, 0.9375, 0.96875}; the unweighted
/// mean over 2..=6 is ≈ 0.806. For 200 samples that's an expected count
/// of ≈ 161 with SD ≈ 5.6 — asserting ≥ 60 puts us ~18σ below the mean,
/// far below the 1e-11 false-positive budget.
#[test]
fn bucket_separation_distribution_explores_mixed_cases() {
    use proptest::strategy::{Strategy, ValueTree};
    let mut runner = proptest::test_runner::TestRunner::default();
    let strategy = proptest::collection::vec(proptest::bool::ANY, 2usize..=6);
    let mut mixed = 0;
    let total = 200;
    for _ in 0..total {
        let kinds = strategy.new_tree(&mut runner).unwrap().current();
        let any_compile = kinds.iter().any(|&b| b);
        let any_pr = kinds.iter().any(|&b| !b);
        if any_compile && any_pr {
            mixed += 1;
        }
    }
    assert!(
        mixed >= 60,
        "expected ≥60/{total} cases to have both Compile and ProjectReference; \
         got {mixed} — the strategy may have regressed to a single-bucket regime",
    );
}

proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config {
        cases: 128,
        ..proptest::test_runner::Config::default()
    })]

    /// For any list of `<PackageReference Include="id" Version="v">` (each
    /// version optional), the parser captures exactly those references, in
    /// document order, with the ids and versions verbatim — no leakage into
    /// the Compile / ProjectReference buckets, no spurious uncertainty.
    #[test]
    fn package_references_captured_verbatim_and_in_order(
        refs in proptest::collection::vec(
            (
                // A package-id-ish token: letters/digits/'.'/'-', non-empty,
                // free of the ';'/'$'/whitespace that would change item count
                // or trigger substitution.
                "[A-Za-z][A-Za-z0-9.-]{0,12}",
                proptest::option::of("[0-9]{1,2}\\.[0-9]{1,2}\\.[0-9]{1,2}"),
            ),
            0usize..=6,
        )
    ) {
        let mut xml = String::from("<Project>\n  <ItemGroup>\n");
        for (id, version) in &refs {
            match version {
                Some(v) => xml.push_str(&format!(
                    "    <PackageReference Include=\"{id}\" Version=\"{v}\" />\n"
                )),
                None => xml.push_str(&format!("    <PackageReference Include=\"{id}\" />\n")),
            }
        }
        xml.push_str("  </ItemGroup>\n</Project>");

        let p = parse(&xml);
        proptest::prop_assert!(p.diagnostics.is_empty(), "diagnostics: {:?}", p.diagnostics);
        // A versionless Include is the CPM/incomplete symptom → uncertain; an
        // all-versioned set stays certain. Assert the rule exactly, both ways.
        let expect_uncertain = refs.iter().any(|(_, v)| v.is_none());
        proptest::prop_assert_eq!(p.package_references_uncertain, expect_uncertain);
        proptest::prop_assert!(p.items.is_empty());
        proptest::prop_assert!(p.project_references.is_empty());
        proptest::prop_assert!(p.framework_references.is_empty());

        let got: Vec<(String, Option<String>)> = p
            .package_references
            .iter()
            .map(|r| (r.id.clone(), r.version.clone()))
            .collect();
        let want: Vec<(String, Option<String>)> = refs
            .iter()
            .map(|(id, v)| ((*id).clone(), v.clone()))
            .collect();
        proptest::prop_assert_eq!(got, want);
        for r in &p.package_references {
            proptest::prop_assert_eq!(r.op, PackageRefOp::Include);
        }
    }
}

// -- Phase 7: DefineConstants extraction ------------------------------

#[test]
fn lang_version_extracted_verbatim() {
    let src = r#"<Project>
  <PropertyGroup>
    <LangVersion>11.0</LangVersion>
  </PropertyGroup>
</Project>"#;
    assert_eq!(parse(src).lang_version.as_deref(), Some("11.0"));
}

#[test]
fn lang_version_alias_preserved_for_consumer() {
    // We hand back the raw string; resolving `latest`/`preview`/etc. is the
    // consumer's job (the LSP, via `LanguageVersion::from_lang_version_text`).
    let src = r#"<Project>
  <PropertyGroup>
    <LangVersion>preview</LangVersion>
  </PropertyGroup>
</Project>"#;
    assert_eq!(parse(src).lang_version.as_deref(), Some("preview"));
}

#[test]
fn lang_version_expands_and_trims() {
    // `$(…)` substitution applies like any property, and surrounding
    // whitespace is trimmed.
    let src = r#"<Project>
  <PropertyGroup>
    <Ver>9.0</Ver>
    <LangVersion>  $(Ver)  </LangVersion>
  </PropertyGroup>
</Project>"#;
    assert_eq!(parse(src).lang_version.as_deref(), Some("9.0"));
}

#[test]
fn lang_version_missing_is_none() {
    let src = r#"<Project>
  <PropertyGroup>
    <DefineConstants>FOO</DefineConstants>
  </PropertyGroup>
</Project>"#;
    assert_eq!(parse(src).lang_version, None);
}

#[test]
fn lang_version_empty_is_none() {
    let src = r#"<Project>
  <PropertyGroup>
    <LangVersion></LangVersion>
  </PropertyGroup>
</Project>"#;
    assert_eq!(parse(src).lang_version, None);
}

#[test]
fn target_name_defaults_to_assembly_name() {
    let src = r#"<Project>
  <PropertyGroup>
    <AssemblyName>Renamed</AssemblyName>
  </PropertyGroup>
</Project>"#;
    assert_eq!(parse(src).target_name, ItemMetadataValue::known("Renamed"));
}

#[test]
fn target_name_override_beats_assembly_name() {
    // MSBuild writes `$(TargetName)$(TargetExt)`, and `TargetName` defaults
    // to `AssemblyName` only when empty (probed, dotnet 10.0.301:
    // `AssemblyName=Identity` + `TargetName=FileName` builds
    // `FileName.dll`).
    let src = r#"<Project>
  <PropertyGroup>
    <AssemblyName>Identity</AssemblyName>
    <TargetName>FileName</TargetName>
  </PropertyGroup>
</Project>"#;
    assert_eq!(parse(src).target_name, ItemMetadataValue::known("FileName"));
}

#[test]
fn target_name_missing_or_empty_is_known_none() {
    // Unset and set-to-empty both mean "MSBuild's default applies" (the
    // project-file stem) — a consumer can trust the stem.
    let p = parse("<Project></Project>");
    assert_eq!(p.target_name, ItemMetadataValue::Known(None));

    let src = r#"<Project>
  <PropertyGroup>
    <AssemblyName></AssemblyName>
  </PropertyGroup>
</Project>"#;
    assert_eq!(parse(src).target_name, ItemMetadataValue::Known(None));
}

#[test]
fn target_name_expands_pinned_property_values() {
    let src = r#"<Project>
  <PropertyGroup>
    <Base>Lib</Base>
    <AssemblyName>$(Base).Core</AssemblyName>
  </PropertyGroup>
</Project>"#;
    assert_eq!(parse(src).target_name, ItemMetadataValue::known("Lib.Core"));
}

#[test]
fn target_name_preserves_padding_verbatim() {
    // MSBuild puts the padding in the output filename (probed, dotnet
    // 10.0.301: `<AssemblyName> Padded </AssemblyName>` builds
    // ` Padded .dll`), so trimming here would send a locator after a file
    // that doesn't exist.
    let src = r#"<Project>
  <PropertyGroup>
    <AssemblyName> Padded </AssemblyName>
  </PropertyGroup>
</Project>"#;
    assert_eq!(parse(src).target_name, ItemMetadataValue::known(" Padded "));
}

#[test]
fn target_name_written_under_untrusted_gate_is_unknown() {
    // `AssemblyName` is *written* (the gate evaluated true) but only by
    // treating `$(TargetFramework)` — the carve-out that stays inexact under
    // C.2b — as empty; the real build may skip the group and produce the
    // stem-named output instead. Locating a DLL by either name would be a
    // guess; the verdict must decline.
    let src = r#"<Project>
  <PropertyGroup Condition="'$(TargetFramework)' == ''">
    <AssemblyName>Renamed</AssemblyName>
  </PropertyGroup>
</Project>"#;
    assert_eq!(parse(src).target_name, ItemMetadataValue::Unknown);
}

#[test]
fn target_name_untrusted_assembly_name_is_irrelevant_under_an_override() {
    // A trusted explicit `TargetName` decides the filename alone — an
    // untrusted `AssemblyName` underneath it can no longer change what
    // MSBuild writes to `bin/`.
    let src = r#"<Project>
  <PropertyGroup Condition="'$(Undef)' == ''">
    <AssemblyName>Maybe</AssemblyName>
  </PropertyGroup>
  <PropertyGroup>
    <TargetName>Fixed</TargetName>
  </PropertyGroup>
</Project>"#;
    assert_eq!(parse(src).target_name, ItemMetadataValue::known("Fixed"));
}

#[test]
fn target_name_from_unpinned_value_is_unknown() {
    // The write itself is ungated, but its VALUE reads a property that was
    // written under a gate we couldn't pin — same taint, one hop removed.
    // The gate reads `TargetFramework`, the consumer-contract carve-out that
    // stays inexact under C.2b (a plain undefined name would now read exactly
    // empty and pin the gate).
    let src = r#"<Project>
  <PropertyGroup Condition="'$(TargetFramework)' == ''">
    <Suffix>.Special</Suffix>
  </PropertyGroup>
  <PropertyGroup>
    <AssemblyName>Lib$(Suffix)</AssemblyName>
  </PropertyGroup>
</Project>"#;
    assert_eq!(parse(src).target_name, ItemMetadataValue::Unknown);
}

#[test]
fn target_name_repinned_by_later_clean_write() {
    // A later unconditional write overwrites whatever the untrusted one
    // did — the effective value is trustworthy again, mirroring
    // `resolve_string_metadata`'s Known-restoration rule.
    let src = r#"<Project>
  <PropertyGroup Condition="'$(Undef)' == ''">
    <AssemblyName>Maybe</AssemblyName>
  </PropertyGroup>
  <PropertyGroup>
    <AssemblyName>Final</AssemblyName>
  </PropertyGroup>
</Project>"#;
    assert_eq!(parse(src).target_name, ItemMetadataValue::known("Final"));
}

#[test]
fn untrusted_property_provenance_is_queryable() {
    // The generic seam behind the per-consumer verdicts: a property written
    // under a gate we couldn't pin reads as untrusted (case-insensitively);
    // a cleanly-written or never-written one does not. The gate reads the
    // `TargetFramework` carve-out (undefined at gate time, and inexact under
    // C.2b), so the write it guards is untrusted.
    let src = r#"<Project>
  <PropertyGroup Condition="'$(TargetFramework)' == ''">
    <Marked>net8.0</Marked>
  </PropertyGroup>
  <PropertyGroup>
    <Clean>yes</Clean>
  </PropertyGroup>
</Project>"#;
    let p = parse(src);
    assert!(p.property_provenance_untrusted("marked"));
    assert!(p.property_provenance_untrusted("MARKED"));
    assert!(!p.property_provenance_untrusted("Clean"));
    assert!(!p.property_provenance_untrusted("NeverWritten"));
}

#[test]
fn exact_undefined_child_gate_is_not_unpinned_by_an_unpinned_group() {
    // `mark_property_group_children_provenance` skips a child whose own
    // condition is provably false. Under C.2b that proof includes an
    // exact-undefined read: `'$(NoSuch)' != ''` is exactly False (`NoSuch`
    // substitutes to ""), so a child carrying it cannot write whichever way
    // the unpinnable group gate goes — it must not be unpinned. The group
    // gate reads the `TargetFramework` carve-out, so the group itself stays
    // unpinned; only the child's exact-undefined gate is under test.
    let src = r#"<Project>
  <PropertyGroup Condition="'$(TargetFramework)' == 'net8.0'">
    <Marked Condition="'$(NoSuch)' != ''">x</Marked>
  </PropertyGroup>
</Project>"#;
    let p = parse(src);
    assert!(
        !p.property_provenance_untrusted("marked"),
        "a child gated on a provably-false exact-undefined read cannot write, \
         so the unpinnable group must not taint it"
    );
}

#[test]
fn define_constants_simple_list() {
    let src = r#"<Project>
  <PropertyGroup>
    <DefineConstants>FOO;BAR</DefineConstants>
  </PropertyGroup>
</Project>"#;
    let p = parse(src);
    assert_eq!(p.define_constants, vec!["FOO", "BAR"]);
}

#[test]
fn define_constants_empty_segments_dropped() {
    // MSBuild treats `;;` and trailing `;` as separators with empty
    // payload — F# would never see those as symbol names, so we strip
    // them rather than surfacing the empty string.
    let src = r#"<Project>
  <PropertyGroup>
    <DefineConstants>FOO;;BAR;</DefineConstants>
  </PropertyGroup>
</Project>"#;
    let p = parse(src);
    assert_eq!(p.define_constants, vec!["FOO", "BAR"]);
}

#[test]
fn define_constants_whitespace_trimmed() {
    let src = r#"<Project>
  <PropertyGroup>
    <DefineConstants>  FOO ; BAR  </DefineConstants>
  </PropertyGroup>
</Project>"#;
    let p = parse(src);
    assert_eq!(p.define_constants, vec!["FOO", "BAR"]);
}

#[test]
fn define_constants_missing_property_is_empty() {
    let p = parse("<Project></Project>");
    assert!(p.define_constants.is_empty());
}

#[test]
fn define_constants_empty_property_is_empty() {
    let src = r#"<Project>
  <PropertyGroup>
    <DefineConstants></DefineConstants>
  </PropertyGroup>
</Project>"#;
    let p = parse(src);
    assert!(p.define_constants.is_empty());
}

#[test]
fn define_constants_conditional_group_selects_active_arm() {
    let src = r#"<Project>
  <PropertyGroup Condition="'$(Configuration)' == 'Debug'">
    <DefineConstants>DEBUG;TRACE</DefineConstants>
  </PropertyGroup>
  <PropertyGroup Condition="'$(Configuration)' == 'Release'">
    <DefineConstants>RELEASE</DefineConstants>
  </PropertyGroup>
</Project>"#;
    let p = parse_with(src, &[("Configuration", "Debug")]);
    assert_eq!(p.define_constants, vec!["DEBUG", "TRACE"]);
    let p = parse_with(src, &[("Configuration", "Release")]);
    assert_eq!(p.define_constants, vec!["RELEASE"]);
}

#[test]
fn define_constants_append_self_reference() {
    // <DefineConstants>$(DefineConstants);BAZ</DefineConstants> is the
    // canonical MSBuild append idiom (mirrors `self_reference_uses_prior_value`
    // for NoWarn). Stage 7 must surface the final accumulated list.
    let src = r#"<Project>
  <PropertyGroup>
    <DefineConstants>FOO;BAR</DefineConstants>
    <DefineConstants>$(DefineConstants);BAZ</DefineConstants>
  </PropertyGroup>
</Project>"#;
    let p = parse(src);
    assert_eq!(p.define_constants, vec!["FOO", "BAR", "BAZ"]);
}

#[test]
fn define_constants_case_insensitive_property_lookup() {
    // MSBuild property names are case-insensitive. A project that writes
    // <defineConstants> must still surface through `define_constants()`.
    let src = r#"<Project>
  <PropertyGroup>
    <defineConstants>FOO;BAR</defineConstants>
  </PropertyGroup>
</Project>"#;
    let p = parse(src);
    assert_eq!(p.define_constants, vec!["FOO", "BAR"]);
}

#[test]
fn define_constants_preserves_value_case() {
    // F# preprocessor symbols are case-sensitive (`#if FOO` ≠ `#if foo`),
    // so we must preserve the casing of each segment even though the
    // property name lookup is case-insensitive.
    let src = r#"<Project>
  <PropertyGroup>
    <DefineConstants>Foo;bar;BAZ</DefineConstants>
  </PropertyGroup>
</Project>"#;
    let p = parse(src);
    assert_eq!(p.define_constants, vec!["Foo", "bar", "BAZ"]);
}

#[test]
fn define_constants_from_extra_properties() {
    // MSBuild global properties (e.g. `-p:DefineConstants=DEBUG`) live
    // only in the evaluator's lookup map and are deliberately absent
    // from `ParsedProject::properties` (which documents "what the
    // project wrote"). `define_constants` must still incorporate them
    // — otherwise the preprocessor would skip code guarded by symbols
    // the caller supplied as globals.
    let p = parse_with("<Project/>", &[("DefineConstants", "DEBUG;TRACE")]);
    assert_eq!(p.define_constants, vec!["DEBUG", "TRACE"]);
    // The "what the project wrote" map should *not* contain the
    // caller-supplied value (sanity-check the documented split).
    assert!(!p.properties.contains_key("DefineConstants"));
}

#[test]
fn define_constants_global_protects_against_project_write() {
    // MSBuild global properties are *protected*: a project-side write
    // to a name supplied via extras is silently discarded. So the
    // project's append (`$(DefineConstants);LOCAL`) is ignored and the
    // final value is just the global. This documents the contract; the
    // companion test below shows how `TreatAsLocalProperty` opts the
    // name out of protection so the append takes effect.
    let src = r#"<Project>
  <PropertyGroup>
    <DefineConstants>$(DefineConstants);LOCAL</DefineConstants>
  </PropertyGroup>
</Project>"#;
    let p = parse_with(src, &[("DefineConstants", "DEBUG")]);
    assert_eq!(p.define_constants, vec!["DEBUG"]);
}

#[test]
fn define_constants_treat_as_local_lets_project_append() {
    // `<Project TreatAsLocalProperty="DefineConstants">` opts the name
    // out of the global-properties protection (see the evaluator's
    // `collect_local_overrides`). The project's append now lands, and
    // the final value is the global followed by the project's segment.
    let src = r#"<Project TreatAsLocalProperty="DefineConstants">
  <PropertyGroup>
    <DefineConstants>$(DefineConstants);LOCAL</DefineConstants>
  </PropertyGroup>
</Project>"#;
    let p = parse_with(src, &[("DefineConstants", "DEBUG")]);
    assert_eq!(p.define_constants, vec!["DEBUG", "LOCAL"]);
}

#[test]
fn define_constants_preserves_duplicates() {
    // Caller (Stage 8) collects into a HashSet for the preprocessor;
    // this layer keeps the raw list so other consumers see what the
    // project actually wrote.
    let src = r#"<Project>
  <PropertyGroup>
    <DefineConstants>FOO;BAR;FOO</DefineConstants>
  </PropertyGroup>
</Project>"#;
    let p = parse(src);
    assert_eq!(p.define_constants, vec!["FOO", "BAR", "FOO"]);
}

// --- items_uncertain: the narrow "is the Compile set trustworthy?" signal ----

#[test]
fn clean_sdkless_project_is_not_items_uncertain() {
    let src = r#"<Project>
  <ItemGroup>
    <Compile Include="A.fs" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert!(!p.is_partial);
    assert!(!p.items_uncertain);
    assert!(p.compile_condition_uncertainties.is_empty());
}

#[test]
fn target_and_undefined_property_in_body_are_not_items_uncertain() {
    // A `<Target>` and an undefined property in a (non-Compile) PropertyGroup
    // both flip `is_partial`, but neither can change the Compile item set —
    // this is the noise every real SDK project's imported targets emit, and it
    // must not disable Compile-order-dependent resolution.
    let src = r#"<Project>
  <Target Name="Stamp" />
  <PropertyGroup>
    <Foo>$(Undefined)</Foo>
  </PropertyGroup>
  <ItemGroup>
    <Compile Include="A.fs" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert_eq!(paths(&p.items), [Path::new("/repo/proj/A.fs")]);
    assert!(
        p.is_partial,
        "a skipped Target + undefined property still diverge from MSBuild"
    );
    assert!(!p.items_uncertain, "but the Compile set is unaffected");
    assert!(p.compile_condition_uncertainties.is_empty());
}

#[test]
fn compile_item_condition_with_inexact_property_is_uncertain_and_recorded() {
    // The correctness carve-out: the item's inclusion depends on a property we
    // couldn't resolve (`TargetFramework` is carved out, never provably
    // unset), so our exclude decision may be wrong.
    let src = r#"<Project>
  <ItemGroup>
    <Compile Include="A.fs" Condition="'$(TargetFramework)' == 'bar'" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    // TargetFramework unknown -> '' == 'bar' -> false -> item dropped.
    assert!(p.items.is_empty());
    assert!(p.items_uncertain);
    assert_eq!(p.compile_condition_uncertainties.len(), 1);
    let u = &p.compile_condition_uncertainties[0];
    assert_eq!(u.condition, "'$(TargetFramework)' == 'bar'");
    assert_eq!(
        u.reason,
        CompileConditionReason::UndefinedProperties(vec!["TargetFramework".to_string()])
    );
    assert_eq!(u.origin, DiagnosticOrigin::Buffer);
}

#[test]
fn item_group_inexact_condition_wrapping_compile_is_uncertain() {
    let src = r#"<Project>
  <ItemGroup Condition="'$(TargetFramework)' == 'bar'">
    <Compile Include="A.fs" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert!(p.items.is_empty(), "group skipped");
    assert!(p.items_uncertain);
    assert_eq!(p.compile_condition_uncertainties.len(), 1);
    assert_eq!(
        p.compile_condition_uncertainties[0].reason,
        CompileConditionReason::UndefinedProperties(vec!["TargetFramework".to_string()])
    );
}

#[test]
fn item_group_inexact_condition_wrapping_only_non_compile_is_not_uncertain() {
    // The inexact property read (carved-out `TargetFramework`) still flips
    // `is_partial`, but no Compile item is gated, so the Compile set is
    // safe and we must not fall back.
    let src = r#"<Project>
  <ItemGroup Condition="'$(TargetFramework)' == 'bar'">
    <PackageReference Include="X" Version="1.0.0" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert!(p.is_partial);
    assert!(!p.items_uncertain);
    assert!(p.compile_condition_uncertainties.is_empty());
}

#[test]
fn unsupported_condition_on_compile_is_uncertain_and_recorded() {
    // `Exists(...)` is outside our grammar -> exclusionary, so A.fs is dropped;
    // we record the carve-out with the `Unsupported` reason.
    let src = r#"<Project>
  <ItemGroup>
    <Compile Include="A.fs" Condition="Exists('A.fs')" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert!(p.items.is_empty());
    assert!(p.items_uncertain);
    assert_eq!(p.compile_condition_uncertainties.len(), 1);
    assert_eq!(
        p.compile_condition_uncertainties[0].reason,
        CompileConditionReason::Unsupported
    );
}

#[test]
fn compile_remove_makes_items_uncertain_without_a_condition_carve_out() {
    let src = r#"<Project>
  <ItemGroup>
    <Compile Include="A.fs" />
    <Compile Remove="A.fs" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert_eq!(paths(&p.items), [Path::new("/repo/proj/A.fs")]);
    assert!(
        p.items_uncertain,
        "an ignored Remove may leave a stale item"
    );
    assert!(p.compile_condition_uncertainties.is_empty());
}

#[test]
fn compile_update_metadata_does_not_make_items_uncertain() {
    // The gate reads carved-out `TargetFramework` (never provably unset),
    // so the group's inclusion is a divergence risk — but it only carries
    // a metadata-only Update.
    let src = r#"<Project>
  <ItemGroup>
    <Compile Include="A.fs" />
  </ItemGroup>
  <ItemGroup Condition="'$(TargetFramework)' != 'false'">
    <Compile Update="@(Compile)">
      <Link>%(Filename)%(Extension)</Link>
    </Compile>
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert_eq!(paths(&p.items), [Path::new("/repo/proj/A.fs")]);
    assert!(p.is_partial, "the inexact property read still diverges");
    assert!(
        !p.items_uncertain,
        "metadata-only Compile Update cannot change the Compile item set"
    );
    assert!(p.compile_condition_uncertainties.is_empty());
    assert!(
        !p.diagnostics.iter().any(|diag| matches!(
            &diag.kind,
            DiagnosticKind::UnsupportedItemOperation { operation }
                if operation == "Update=@(Compile)"
        )),
        "metadata-only Compile Update should not be diagnosed as unsupported: {:?}",
        p.diagnostics
    );
}

#[test]
fn choose_with_compile_makes_items_uncertain() {
    // `<Choose>` can carry `<ItemGroup><Compile>`; this one's When gate reads
    // carved-out `TargetFramework` (never provably unset), so the branch
    // decision can't be pinned and we don't descend — the Compile set is
    // untrustworthy.
    let src = r#"<Project>
  <Choose>
    <When Condition="'$(TargetFramework)' == 'Bar'">
      <ItemGroup><Compile Include="A.fs" /></ItemGroup>
    </When>
  </Choose>
</Project>"#;
    let p = parse(src);
    assert!(p.items_uncertain);
    assert!(p.compile_condition_uncertainties.is_empty());
}

#[test]
fn project_write_cannot_influence_are_features_enabled() {
    // MSBuildDisableFeaturesFromVersion is reserved: real MSBuild fails
    // the whole load with MSB4004 on this project (probed against
    // dotnet msbuild 10.0.300), and the walker's long-standing reserved-
    // write tolerance instead drops the write and keeps going. What must
    // hold under that tolerance is that the dropped write never reaches
    // AreFeaturesEnabled's guard: the guard runs against the trusted
    // environment seeding — empty snapshot ⇒ no waves disabled ⇒ every
    // wave enabled (oracle: `AreFeaturesEnabled('17.10')` is TRUE with
    // the variable unset). If the project's "17.0" landed, 17.10 would
    // be at-or-above the threshold and `R` would stay unset.
    let src = r#"<Project>
  <PropertyGroup>
    <MSBuildDisableFeaturesFromVersion>17.0</MSBuildDisableFeaturesFromVersion>
    <R Condition="$([MSBuild]::AreFeaturesEnabled('17.10'))">armed</R>
  </PropertyGroup>
</Project>"#;
    let p = parse(src);
    assert_eq!(
        p.properties.get("R").map(String::as_str),
        Some("armed"),
        "diagnostics: {:?}",
        p.diagnostics
    );
}

#[test]
fn env_disable_threshold_declines_are_features_enabled() {
    // A non-empty MSBUILDDISABLEFEATURESFROMVERSION genuinely disables
    // waves at-or-above the threshold (oracle: with the variable set to
    // 17.4, `AreFeaturesEnabled('17.10')` is FALSE). We don't model
    // MSBuild's clamping of the value against its wave rotation, so a
    // set variable leaves the property undefined and the function
    // declines: the gated write is dropped conservatively with an
    // UnsupportedCondition diagnostic.
    let src = r#"<Project>
  <PropertyGroup>
    <R Condition="$([MSBuild]::AreFeaturesEnabled('17.10'))">armed</R>
  </PropertyGroup>
</Project>"#;
    let env = HashMap::from([(
        "MSBUILDDISABLEFEATURESFROMVERSION".to_string(),
        "17.4".to_string(),
    )]);
    let p = parse_with_environment(src, &env);
    assert!(!p.properties.contains_key("R"));
    assert!(
        p.diagnostics
            .iter()
            .any(|d| matches!(&d.kind, DiagnosticKind::UnsupportedCondition { .. })),
        "{:?}",
        p.diagnostics
    );
}

#[test]
fn env_extensions_path_declines_without_a_resolved_toolset() {
    // What MSBuild does with an environment `MSBuildExtensionsPath` depends on
    // the toolset: MSBuild ≤ 17 (SDK 8, 9) overwrites it with the toolset
    // directory, MSBuild 18 (SDK 10) lets it stand — both probed against
    // `dotnet msbuild`. Here no SDK resolves, so no toolset is known, and
    // either answer would be a guess: promoting the environment value would
    // commit MSBuild 18's behaviour to a build that may well run MSBuild 17.
    // Decline instead.
    let src = r#"<Project>
  <PropertyGroup>
    <R>$(MSBuildExtensionsPath)</R>
  </PropertyGroup>
</Project>"#;
    let env = HashMap::from([(
        "MSBuildExtensionsPath".to_string(),
        "/spoof/ext".to_string(),
    )]);
    let p = parse_with_environment(src, &env);
    assert_eq!(p.properties.get("R").map(String::as_str), Some(""));
    assert!(
        p.diagnostics.iter().any(|d| matches!(
            &d.kind,
            DiagnosticKind::UndefinedProperty { name }
                if name.eq_ignore_ascii_case("MSBuildExtensionsPath")
        )),
        "{:?}",
        p.diagnostics
    );
}

#[test]
fn changewaves_property_reads_the_visible_sentinel() {
    // Probed (dotnet msbuild 10.0.300): with the variable unset,
    // `$(MSBuildDisableFeaturesFromVersion)` reads `999.999` — NOT the
    // empty string — and `'$(MSBuildDisableFeaturesFromVersion)' == ''`
    // is False.
    let p = parse(
        r#"<Project>
  <PropertyGroup>
    <R>[$(MSBuildDisableFeaturesFromVersion)]</R>
    <E>FALSE</E>
    <E Condition="'$(MSBuildDisableFeaturesFromVersion)' == ''">TRUE</E>
  </PropertyGroup>
</Project>"#,
    );
    assert_eq!(p.properties.get("R").map(String::as_str), Some("[999.999]"));
    assert_eq!(p.properties.get("E").map(String::as_str), Some("FALSE"));
    assert!(!p.is_partial, "{:?}", p.diagnostics);
}

#[test]
fn changewaves_variable_lookup_follows_the_host_case_rule() {
    // Probed: .NET's Unix environment lookup is case-sensitive, so a
    // lowercase or mixed-case spelling of the variable is invisible to
    // ChangeWaves — the sentinel stays and waves stay enabled. Windows folds
    // case, so the same spellings *are* seen: the variable counts as set, and
    // since we don't model the wave rotation the property is left undefined
    // (the read degrades rather than committing the raw value).
    let expected = if cfg!(windows) { "[]" } else { "[999.999]" };
    for spelling in [
        "msbuilddisablefeaturesfromversion",
        "MSBuildDisableFeaturesFromVersion",
    ] {
        let env = HashMap::from([(spelling.to_string(), "17.4".to_string())]);
        let p = parse_with_environment(
            r#"<Project>
  <PropertyGroup>
    <R>[$(MSBuildDisableFeaturesFromVersion)]</R>
  </PropertyGroup>
</Project>"#,
            &env,
        );
        assert_eq!(
            p.properties.get("R").map(String::as_str),
            Some(expected),
            "{spelling}"
        );
    }
}

#[test]
fn empty_changewaves_variable_seeds_the_sentinel() {
    // Probed: `MSBUILDDISABLEFEATURESFROMVERSION=` (set but empty) is
    // treated as unset — the property reads `999.999`.
    let env = HashMap::from([(
        "MSBUILDDISABLEFEATURESFROMVERSION".to_string(),
        String::new(),
    )]);
    let p = parse_with_environment(
        r#"<Project>
  <PropertyGroup>
    <R>[$(MSBuildDisableFeaturesFromVersion)]</R>
  </PropertyGroup>
</Project>"#,
        &env,
    );
    assert_eq!(p.properties.get("R").map(String::as_str), Some("[999.999]"));
}

#[test]
fn set_changewaves_variable_leaves_the_property_undefined() {
    // MSBuild clamps a set value against its version-dependent wave
    // rotation (probed: 17.4 → 17.10, 5.0 → 17.10, banana → 999.999);
    // we don't model the rotation, so the property stays undefined and
    // reads surface conservatively rather than committing to the raw
    // (wrong) value.
    let env = HashMap::from([(
        "MSBUILDDISABLEFEATURESFROMVERSION".to_string(),
        "17.4".to_string(),
    )]);
    let p = parse_with_environment(
        r#"<Project>
  <PropertyGroup>
    <R>[$(MSBuildDisableFeaturesFromVersion)]</R>
  </PropertyGroup>
</Project>"#,
        &env,
    );
    assert_eq!(p.properties.get("R").map(String::as_str), Some("[]"));
    assert!(
        p.diagnostics.iter().any(|d| matches!(
            &d.kind,
            DiagnosticKind::UndefinedProperty { name } if name == "MSBuildDisableFeaturesFromVersion"
        )),
        "{:?}",
        p.diagnostics
    );
}

#[test]
fn case_colliding_environment_variables_are_not_promoted() {
    // Probed: with `ZZZTEST`/`zzztest`/`ZZZTest` all set, MSBuild's
    // winner *changed* when the environ order was reversed — the pick
    // is unspecified. Seeding any spelling could commit us to a value
    // the real build doesn't have, so colliding names stay undefined
    // (conservative: reads surface a diagnostic).
    let env = HashMap::from([
        ("CollideMe".to_string(), "upper".to_string()),
        ("COLLIDEME".to_string(), "shouty".to_string()),
        ("Fine".to_string(), "ok".to_string()),
    ]);
    let p = parse_with_environment(
        r#"<Project>
  <PropertyGroup>
    <R>[$(CollideMe)][$(Fine)]</R>
  </PropertyGroup>
</Project>"#,
        &env,
    );
    assert_eq!(p.properties.get("R").map(String::as_str), Some("[][ok]"));
    assert!(
        p.diagnostics.iter().any(|d| matches!(
            &d.kind,
            DiagnosticKind::UndefinedProperty { name } if name == "CollideMe"
        )),
        "{:?}",
        p.diagnostics
    );
}

// --- exact undefined reads (sdk-exactness C.2b) -----------------------------
//
// MSBuild expands an undefined `$(Name)` to the empty string; when the walk
// can prove the name is undefined in the *real* build too (it is absent from
// the trusted environment snapshot, is not a toolset-computed initial
// property, and no hidden or undecided write could have supplied it), the
// read is exactly empty: no diagnostic, no unpinning, no uncertainty.

#[test]
fn undefined_read_is_exactly_empty_on_clean_walk() {
    let p = parse(
        r#"<Project>
  <PropertyGroup>
    <R>[$(NotSetAnywhere)]</R>
  </PropertyGroup>
</Project>"#,
    );
    assert_eq!(p.properties.get("R").map(String::as_str), Some("[]"));
    assert!(!p.is_partial, "{:?}", p.diagnostics);
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
}

#[test]
fn forward_reference_reads_exactly_empty() {
    // Probed: property evaluation is a single forward pass, so a read
    // before the write sees empty and a read after sees the value.
    let p = parse(
        r#"<Project>
  <PropertyGroup>
    <R>[$(Later)]</R>
    <Later>defined-later</Later>
    <S>[$(Later)]</S>
  </PropertyGroup>
</Project>"#,
    );
    assert_eq!(p.properties.get("R").map(String::as_str), Some("[]"));
    assert_eq!(
        p.properties.get("S").map(String::as_str),
        Some("[defined-later]")
    );
    assert!(!p.is_partial, "{:?}", p.diagnostics);
}

#[test]
fn package_reference_gated_on_undefined_property_is_exact() {
    // The SDK-chain shape this exists for: dependency items gated on
    // `'$(SomeOptInProperty)' == ''` (or `!= 'true'`) where the property
    // is genuinely never set. Both polarities decide exactly.
    let p = parse(
        r#"<Project>
  <ItemGroup>
    <PackageReference Include="Kept" Version="1.0" Condition="'$(NotSet)' == ''" />
    <PackageReference Include="Dropped" Version="1.0" Condition="'$(NotSet)' == 'true'" />
  </ItemGroup>
</Project>"#,
    );
    assert!(
        !p.package_references_uncertain,
        "{:?}",
        p.package_reference_uncertainties
    );
    let ids: Vec<&str> = p.package_references.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(ids, vec!["Kept"]);
}

#[test]
fn undefined_read_of_toolset_initial_name_stays_conservative() {
    // MSBuild defines these before evaluation starts (toolset facts we
    // don't know), so "undefined in our walk" is NOT "empty in the real
    // build".
    for name in [
        "MSBuildBinPath",
        "MSBuildUserExtensionsPath",
        "VisualStudioVersion",
        "RoslynTargetsPath",
        "DOTNET_HOST_PATH",
    ] {
        let p = parse(&format!(
            r#"<Project>
  <PropertyGroup>
    <R>[$({name})]</R>
  </PropertyGroup>
</Project>"#
        ));
        assert!(
            p.diagnostics.iter().any(|d| matches!(
                &d.kind,
                DiagnosticKind::UndefinedProperty { name: n } if n == name
            )),
            "{name}: {:?}",
            p.diagnostics
        );
    }
}

#[test]
fn msbuild_is_restoring_reads_exactly_empty_at_build_time() {
    // Probed: `[$(MSBuildIsRestoring)]` is empty on the build /
    // `-getProperty` entrypoint under a scrubbed environment — NuGet's
    // restore entrypoint injects it as a global, but this walker models
    // the build-time evaluation. The pinned exception to the `msbuild`
    // never-exact prefix.
    let p = parse(
        r#"<Project>
  <PropertyGroup>
    <R>[$(MSBuildIsRestoring)]</R>
    <E>FALSE</E>
    <E Condition="'$(MSBuildIsRestoring)' == 'true'">TRUE</E>
  </PropertyGroup>
</Project>"#,
    );
    assert_eq!(p.properties.get("R").map(String::as_str), Some("[]"));
    assert_eq!(p.properties.get("E").map(String::as_str), Some("FALSE"));
    assert!(!p.is_partial, "{:?}", p.diagnostics);
}

#[test]
fn os_reads_the_host_value_exactly() {
    // Probed: MSBuild defines `OS` (`Unix` on unix hosts) even with no
    // such environment variable, and an environment `OS` displaces it.
    let p = parse(
        r#"<Project>
  <PropertyGroup>
    <R>[$(OS)]</R>
  </PropertyGroup>
</Project>"#,
    );
    assert_eq!(p.properties.get("R").map(String::as_str), Some("[Unix]"));
    assert!(!p.is_partial, "{:?}", p.diagnostics);

    let env = HashMap::from([("OS".to_string(), "SpoofOS".to_string())]);
    let p = parse_with_environment(
        r#"<Project>
  <PropertyGroup>
    <R>[$(OS)]</R>
  </PropertyGroup>
</Project>"#,
        &env,
    );
    assert_eq!(p.properties.get("R").map(String::as_str), Some("[SpoofOS]"));
}

#[test]
fn undefined_read_after_undecidable_conditional_write_stays_conservative() {
    // `<Maybe>` may or may not have been written (the gate is outside
    // the supported grammar), so a later read of it cannot be exact.
    let p = parse(
        r#"<Project>
  <PropertyGroup>
    <Maybe Condition="$([Custom.Type]::Choose())">x</Maybe>
    <R>[$(Maybe)]</R>
  </PropertyGroup>
</Project>"#,
    );
    assert!(
        p.diagnostics
            .iter()
            .any(|d| matches!(&d.kind, DiagnosticKind::UnsupportedCondition { .. })),
        "{:?}",
        p.diagnostics
    );
    assert!(p.is_partial);
}

#[test]
fn undefined_read_after_unevaluable_write_stays_conservative() {
    // The write to `Dropped` used an expression we couldn't evaluate, so
    // the stored binding was refused — but the real build HAS a value,
    // so a later read must not claim exact-empty.
    let p = parse(
        r#"<Project>
  <PropertyGroup>
    <Dropped>$([Custom.Type]::Compute())</Dropped>
    <R>[$(Dropped)]</R>
  </PropertyGroup>
</Project>"#,
    );
    assert!(
        p.diagnostics.iter().any(|d| matches!(
            &d.kind,
            DiagnosticKind::UndefinedProperty { name } if name == "Dropped"
        )),
        "{:?}",
        p.diagnostics
    );
}

#[test]
fn undefined_read_after_undecided_choose_branch_write_stays_conservative() {
    // `MaybeSet` is written in a Choose branch whose gate we could not
    // pin; a later read of it is not exact. A name written in NO branch
    // stays exact.
    let p = parse(
        r#"<Project>
  <Choose>
    <When Condition="$([Custom.Type]::Choose())">
      <PropertyGroup>
        <MaybeSet>x</MaybeSet>
      </PropertyGroup>
    </When>
  </Choose>
  <PropertyGroup>
    <R>[$(MaybeSet)]</R>
    <S>[$(NeverSet)]</S>
  </PropertyGroup>
</Project>"#,
    );
    assert!(
        p.diagnostics.iter().any(|d| matches!(
            &d.kind,
            DiagnosticKind::UndefinedProperty { name } if name == "MaybeSet"
        )) || p
            .diagnostics
            .iter()
            .any(|d| matches!(&d.kind, DiagnosticKind::UnsupportedCondition { .. })),
        "{:?}",
        p.diagnostics
    );
    assert!(
        !p.diagnostics.iter().any(|d| matches!(
            &d.kind,
            DiagnosticKind::UndefinedProperty { name } if name == "NeverSet"
        )),
        "{:?}",
        p.diagnostics
    );
}

#[test]
fn undefined_read_after_unresolved_import_stays_conservative() {
    // The pure walker cannot follow `<Import>`, so the imported file
    // could have defined anything: every subsequent undefined read is
    // suspect. A read *before* the import is unaffected (evaluation is
    // a single forward pass on both sides).
    let p = parse(
        r#"<Project>
  <PropertyGroup>
    <Before>[$(NotSet)]</Before>
  </PropertyGroup>
  <Import Project="other.props" />
  <PropertyGroup>
    <After>[$(AlsoNotSet)]</After>
  </PropertyGroup>
</Project>"#,
    );
    assert!(
        !p.diagnostics.iter().any(|d| matches!(
            &d.kind,
            DiagnosticKind::UndefinedProperty { name } if name == "NotSet"
        )),
        "reads before the hidden content stay exact: {:?}",
        p.diagnostics
    );
    assert!(
        p.diagnostics.iter().any(|d| matches!(
            &d.kind,
            DiagnosticKind::UndefinedProperty { name } if name == "AlsoNotSet"
        )),
        "reads after the hidden content are suspect: {:?}",
        p.diagnostics
    );
}

#[test]
fn env_ignored_toolset_names_are_not_promoted() {
    // Probed name-by-name: MSBuild overwrites these with toolset facts
    // after folding the environment in, so a same-named variable is
    // invisible to projects — while `MSBuildSDKsPath` (among others)
    // genuinely honours the environment and must stay promotable.
    let env = HashMap::from([
        ("MSBuildToolsPath".to_string(), "/spoof".to_string()),
        ("RoslynTargetsPath".to_string(), "/spoof2".to_string()),
        ("MSBuildSDKsPath".to_string(), "/real-sdks".to_string()),
    ]);
    let p = parse_with_environment(
        r#"<Project>
  <PropertyGroup>
    <R>[$(MSBuildToolsPath)][$(RoslynTargetsPath)][$(MSBuildSDKsPath)]</R>
  </PropertyGroup>
</Project>"#,
        &env,
    );
    assert_eq!(
        p.properties.get("R").map(String::as_str),
        Some("[][][/real-sdks]")
    );
}

// --- environment promotion (pinned against dotnet msbuild 10.0.300) --------
//
// Probed with stub projects under `nix develop`:
//   FOO=bar           → `$(FOO)` reads "bar"       [env vars are properties]
//   FOO=bar + <FOO>x> → `$(FOO)` reads "x"          [project writes override]
//   FOO=bar + -p:FOO=g → `$(FOO)` reads "g", and a
//     later <FOO>x</FOO> is ignored                 [globals win, stay sticky]
//   FOO=bar, $(foo)   → reads "bar"                 [reads case-insensitive]

#[test]
fn environment_variable_reads_as_property() {
    let env = HashMap::from([("FromEnv".to_string(), "bar".to_string())]);
    let p = parse_with_environment(
        r#"<Project>
  <PropertyGroup>
    <R>$(FromEnv)</R>
  </PropertyGroup>
</Project>"#,
        &env,
    );
    assert_eq!(p.properties.get("R").map(String::as_str), Some("bar"));
    assert!(!p.is_partial, "{:?}", p.diagnostics);
}

#[test]
fn environment_variable_read_is_case_insensitive() {
    let env = HashMap::from([("FROMENV".to_string(), "bar".to_string())]);
    let p = parse_with_environment(
        r#"<Project>
  <PropertyGroup>
    <R>$(fromenv)</R>
  </PropertyGroup>
</Project>"#,
        &env,
    );
    assert_eq!(p.properties.get("R").map(String::as_str), Some("bar"));
}

#[test]
fn project_write_overrides_environment_variable() {
    // Unlike caller globals, env-backed properties are ordinary
    // overridable starting values.
    let env = HashMap::from([("FromEnv".to_string(), "bar".to_string())]);
    let p = parse_with_environment(
        r#"<Project>
  <PropertyGroup>
    <FromEnv>overridden</FromEnv>
    <R>$(FromEnv)</R>
  </PropertyGroup>
</Project>"#,
        &env,
    );
    assert_eq!(
        p.properties.get("R").map(String::as_str),
        Some("overridden")
    );
}

#[test]
fn global_property_beats_environment_variable() {
    let env = HashMap::from([("Both".to_string(), "from-env".to_string())]);
    let extras = HashMap::from([("Both".to_string(), "from-global".to_string())]);
    let p = parse_fsproj(
        r#"<Project>
  <PropertyGroup>
    <R>$(Both)</R>
  </PropertyGroup>
</Project>"#,
        Path::new("/repo/proj/Demo.fsproj"),
        &extras,
        &env,
    )
    .expect("well-formed XML parses");
    assert_eq!(
        p.properties.get("R").map(String::as_str),
        Some("from-global")
    );
}

#[test]
fn reserved_names_in_environment_are_ignored() {
    // A path-derived reserved property must come from the project path,
    // never from the caller's environment snapshot.
    let env = HashMap::from([("MSBuildProjectName".to_string(), "Spoofed".to_string())]);
    let p = parse_with_environment(
        r#"<Project>
  <PropertyGroup>
    <R>$(MSBuildProjectName)</R>
  </PropertyGroup>
</Project>"#,
        &env,
    );
    assert_eq!(p.properties.get("R").map(String::as_str), Some("Demo"));
}

#[test]
fn changewave_env_lookup_follows_the_host_casing_rules() {
    use super::evaluator::changewave_env_value;

    const EXACT: &str = "MSBUILDDISABLEFEATURESFROMVERSION";
    let exact = HashMap::from([(EXACT.to_string(), "17.4".to_string())]);
    // The exact spelling is found on either host.
    for case_insensitive in [false, true] {
        assert_eq!(
            changewave_env_value(&exact, case_insensitive),
            Some("17.4"),
            "exact spelling must be found (case_insensitive={case_insensitive})"
        );
    }

    // A lowercase / mixed-case spelling: .NET env lookups are
    // case-sensitive on Unix (MSBuild does not see it, so the enable-all
    // sentinel applies) but case-insensitive on Windows (MSBuild *does*
    // see it, so waves are genuinely disabled and we must not seed the
    // sentinel — seeding it would commit AreFeaturesEnabled to true on a
    // build where MSBuild disables the wave).
    for spelling in [
        "msbuilddisablefeaturesfromversion",
        "MsBuildDisableFeaturesFromVersion",
    ] {
        let env = HashMap::from([(spelling.to_string(), "17.4".to_string())]);
        assert_eq!(
            changewave_env_value(&env, false),
            None,
            "{spelling}: Unix lookup is case-sensitive"
        );
        assert_eq!(
            changewave_env_value(&env, true),
            Some("17.4"),
            "{spelling}: Windows lookup is case-insensitive"
        );
    }
}

#[test]
fn environment_values_live_in_the_escaped_domain() {
    // MSBuild folds in the environment value as *escaped-domain* text
    // (`AddEnvironmentProperties` stores `EvaluatedValueEscaped`), so a `%XX`
    // in one is an escape and is unescaped at the point of use — probed:
    // `FOO=%54rue` makes `'$(FOO)' == 'True'` fire, and `$(FOO)` reads `True`.
    // Since E1 the walker models that domain, so these *commit* rather than
    // degrade. Seeding via `insert_computed` instead would escape the `%` on
    // the way in and wrongly read back the literal `%54rue`.
    let env = HashMap::from([
        ("FOO".to_string(), "%54rue".to_string()),
        ("SPACED".to_string(), "a%20b".to_string()),
    ]);
    let p = parse_with_environment(
        r#"<Project>
  <PropertyGroup>
    <R>$(FOO)</R>
    <S>$(SPACED)</S>
    <Gated Condition="'$(FOO)' == 'True'">fired</Gated>
  </PropertyGroup>
</Project>"#,
        &env,
    );
    assert_eq!(p.properties.get("R").map(String::as_str), Some("True"));
    assert_eq!(p.properties.get("S").map(String::as_str), Some("a b"));
    assert_eq!(p.properties.get("Gated").map(String::as_str), Some("fired"));
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
}

#[test]
fn every_msbuild_reserved_name_in_environment_is_ignored() {
    // MSBuild filters the environment against its *whole*
    // `ReservedPropertyNames.ReservedProperties` set before promoting
    // (`Utilities.GetEnvironmentProperties`:
    // `!ReservedPropertyNames.IsReservedProperty(name)`), not just the
    // subset this crate happens to seed from the project path. So a
    // spoofed reserved name — `MSBuildThisFileFullPath` is the dangerous
    // one, since the SDK chain builds import paths out of it — must never
    // become readable, even for names we leave undefined. Transcribed from
    // dotnet/msbuild `src/Build/Resources/Constants.cs`.
    for name in [
        "MSBuildProjectDirectory",
        "MSBuildProjectDirectoryNoRoot",
        "MSBuildProjectFile",
        "MSBuildProjectExtension",
        "MSBuildProjectFullPath",
        "MSBuildProjectName",
        "MSBuildThisFileDirectory",
        "MSBuildThisFileDirectoryNoRoot",
        "MSBuildThisFile",
        "MSBuildThisFileExtension",
        "MSBuildThisFileFullPath",
        "MSBuildThisFileName",
        "MSBuildBinPath",
        "MSBuildProjectDefaultTargets",
        "MSBuildToolsPath",
        "MSBuildToolsVersion",
        "MSBuildRuntimeType",
        "MSBuildStartupDirectory",
        "MSBuildNodeCount",
        "MSBuildLastTaskResult",
        "MSBuildProgramFiles32",
        "MSBuildAssemblyVersion",
        "MSBuildVersion",
        "MSBuildInteractive",
        "MSBuildDisableFeaturesFromVersion",
    ] {
        let env = HashMap::from([(name.to_string(), "SPOOFED".to_string())]);
        let src = format!(
            r#"<Project>
  <PropertyGroup>
    <R>$({name})</R>
  </PropertyGroup>
</Project>"#
        );
        let p = parse_with_environment(&src, &env);
        assert_ne!(
            p.properties.get("R").map(String::as_str),
            Some("SPOOFED"),
            "{name} was promoted from the environment; MSBuild filters it as reserved"
        );
    }
}

#[test]
fn unreferenceable_environment_names_are_skipped() {
    // Names outside the `$(…)` reference grammar (shell exports like
    // `%exit_code` or dotted names) can never be read, so they are not
    // promoted — and must not panic the walk.
    let env = HashMap::from([
        ("%exit_code".to_string(), "x".to_string()),
        ("a.b".to_string(), "y".to_string()),
        ("".to_string(), "z".to_string()),
        ("Fine".to_string(), "ok".to_string()),
    ]);
    let p = parse_with_environment(
        r#"<Project>
  <PropertyGroup>
    <R>$(Fine)</R>
  </PropertyGroup>
</Project>"#,
        &env,
    );
    assert_eq!(p.properties.get("R").map(String::as_str), Some("ok"));
}

// --- define_constants_uncertain: the "are the #if symbols trustworthy?" axis -

#[test]
fn clean_define_constants_is_not_uncertain() {
    let p = parse(
        r#"<Project>
  <PropertyGroup>
    <DefineConstants>DEBUG;TRACE</DefineConstants>
  </PropertyGroup>
</Project>"#,
    );
    assert!(!p.is_partial);
    assert!(!p.define_constants_uncertain);
    assert_eq!(p.define_constants, vec!["DEBUG", "TRACE"]);
}

#[test]
fn define_constants_gated_on_undefined_property_is_uncertain() {
    // The multi-target shape: a define gated on `$(TargetFramework)`, which is
    // unresolved (only `<TargetFrameworks>` plural is set). We can't tell which
    // branch applies, so the `#if` symbol set is uncertain.
    let p = parse(
        r#"<Project>
  <PropertyGroup Condition="'$(TargetFramework)' == 'net6.0'">
    <DefineConstants>NET6</DefineConstants>
  </PropertyGroup>
</Project>"#,
    );
    assert!(p.is_partial);
    assert!(p.define_constants_uncertain);
    assert!(!p.items_uncertain, "no Compile item involved");
}

#[test]
fn define_constants_value_with_inexact_non_self_property_is_uncertain() {
    // An inexact *non-self* ref (`$(TargetFramework)`, not the
    // `$(DefineConstants)` accumulator) may be a property the real build
    // sets (TargetFramework is a consumer-contract carve-out: never
    // provably unset), so substituting it to "" can drop defines that
    // should be present. Note: a *provably*-unset name here would now
    // read exactly empty and leave the defines certain.
    let p = parse(
        r#"<Project>
  <PropertyGroup>
    <DefineConstants>$(TargetFramework);FOO</DefineConstants>
  </PropertyGroup>
</Project>"#,
    );
    assert!(p.is_partial);
    assert!(p.define_constants_uncertain);
}

#[test]
fn define_constants_value_with_unsupported_expression_is_uncertain() {
    // A property function in the value leaves a residual literal (MSBuild would
    // evaluate it), so our `#if` symbols diverge — unlike an undefined ref.
    let p = parse(
        r#"<Project>
  <PropertyGroup>
    <DefineConstants>$([System.String]::Copy('FOO'))</DefineConstants>
  </PropertyGroup>
</Project>"#,
    );
    assert!(p.define_constants_uncertain);
}

#[test]
fn define_constants_self_reference_in_a_condition_is_uncertain() {
    // The "default if not already set" idiom: a *condition* on
    // `$(DefineConstants)`. Unlike the value self-append, a condition is a real
    // branch decision — if the real build set DefineConstants (e.g. DEBUG via
    // the SDK), the group wouldn't run and our defines diverge. Must flag.
    let p = parse(
        r#"<Project>
  <PropertyGroup Condition="'$(DefineConstants)' == ''">
    <DefineConstants>FOO</DefineConstants>
  </PropertyGroup>
</Project>"#,
    );
    assert!(p.define_constants_uncertain);
}

#[test]
fn define_constants_self_append_is_not_uncertain() {
    // The canonical append idiom with no prior project-level DefineConstants:
    // `$(DefineConstants)` is unset → "" (as MSBuild treats it) → must not flag.
    let p = parse(
        r#"<Project>
  <PropertyGroup>
    <DefineConstants>$(DefineConstants);FOO</DefineConstants>
  </PropertyGroup>
</Project>"#,
    );
    assert!(!p.define_constants_uncertain);
    assert_eq!(p.define_constants, vec!["FOO"]);
}

#[test]
fn sibling_property_undefined_in_a_define_group_is_not_define_uncertain() {
    // A non-DefineConstants property's inexact undefined reference in the
    // same group (`TargetFramework` is carved out, so its read still
    // diagnoses) must not taint the define-constants signal (scoped
    // context, like the Compile side).
    let p = parse(
        r#"<Project>
  <PropertyGroup>
    <DefineConstants>FOO</DefineConstants>
    <OtherProp>$(TargetFramework)</OtherProp>
  </PropertyGroup>
</Project>"#,
    );
    assert!(
        p.is_partial,
        "the sibling's undefined property still diverges"
    );
    assert!(
        !p.define_constants_uncertain,
        "but DefineConstants itself is clean"
    );
    assert_eq!(p.define_constants, vec!["FOO"]);
}

#[test]
fn sdk_shorthand_without_a_resolver_is_items_uncertain() {
    // No SDK resolver → the SDK never loads, so any default-item Compile
    // contributions are missing and the Compile set is untrustworthy.
    let p = parse(
        r#"<Project Sdk="Microsoft.NET.Sdk">
  <ItemGroup>
    <Compile Include="A.fs" />
  </ItemGroup>
</Project>"#,
    );
    assert!(p.is_partial);
    assert!(
        p.items_uncertain,
        "an unevaluated SDK leaves the Compile set incomplete"
    );
}

#[test]
fn link_metadata_inexact_property_does_not_make_items_uncertain() {
    // `<Link>` is display-only; an inexact property read there (carved-out
    // `TargetFramework`, never provably unset) must not flag the Compile
    // item, whose include (A.fs) is fully known.
    let p = parse(
        r#"<Project>
  <ItemGroup>
    <Compile Include="A.fs" Link="$(TargetFramework)/A.fs" />
  </ItemGroup>
</Project>"#,
    );
    assert_eq!(paths(&p.items), [Path::new("/repo/proj/A.fs")]);
    assert!(p.is_partial, "the inexact property read still diverges");
    assert!(!p.items_uncertain, "but which file compiles is known");
}

#[test]
fn project_reference_problem_in_a_compile_group_does_not_make_items_uncertain() {
    // A `<ProjectReference>` sibling's inexact-property condition (carved-out
    // `TargetFramework`, never provably unset) shares the group with a
    // `<Compile>`, but it doesn't change which sources compile.
    let p = parse(
        r#"<Project>
  <ItemGroup>
    <Compile Include="A.fs" />
    <ProjectReference Include="X.fsproj" Condition="'$(TargetFramework)' == 'y'" />
  </ItemGroup>
</Project>"#,
    );
    assert_eq!(paths(&p.items), [Path::new("/repo/proj/A.fs")]);
    assert!(p.is_partial);
    assert!(
        !p.items_uncertain,
        "a ProjectReference condition is not Compile-affecting"
    );
}

#[test]
fn item_group_known_false_condition_is_not_items_uncertain() {
    // A condition we *can* evaluate (property supplied) is a clean exclusion —
    // no divergence, so neither `is_partial` nor `items_uncertain`.
    let src = r#"<Project>
  <ItemGroup Condition="'$(Configuration)' == 'Debug'">
    <Compile Include="DebugOnly.fs" />
  </ItemGroup>
</Project>"#;
    let p = parse_with(src, &[("Configuration", "Release")]);
    assert!(p.items.is_empty());
    assert!(!p.is_partial);
    assert!(!p.items_uncertain);
    assert!(p.compile_condition_uncertainties.is_empty());
}

// -----------------------------------------------------------------
// PackageReference / FrameworkReference capture (slice 4a).
// -----------------------------------------------------------------

fn only_package(p: &ParsedProject) -> &PackageReference {
    assert_eq!(
        p.package_references.len(),
        1,
        "expected exactly one package ref"
    );
    &p.package_references[0]
}

fn ascii_case_variant(
    canonical: &'static str,
) -> impl proptest::strategy::Strategy<Value = String> {
    use proptest::strategy::Strategy;
    proptest::collection::vec(proptest::bool::ANY, canonical.len()).prop_map(move |uppercase| {
        canonical
            .bytes()
            .zip(uppercase)
            .map(|(byte, uppercase)| {
                let c = byte as char;
                if uppercase {
                    c.to_ascii_uppercase()
                } else {
                    c.to_ascii_lowercase()
                }
            })
            .collect()
    })
}

proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config {
        cases: 128,
        ..proptest::test_runner::Config::default()
    })]

    #[test]
    fn package_reference_item_type_casing_is_ignored(
        item_type in ascii_case_variant("PackageReference")
    ) {
        let src = format!(
            r#"<Project>
  <ItemGroup>
    <{item_type} Include="Newtonsoft.Json" Version="13.0.1" />
  </ItemGroup>
</Project>"#
        );
        let p = parse(&src);
        proptest::prop_assert!(p.items.is_empty(), "package ref must not leak into items");
        proptest::prop_assert!(p.project_references.is_empty());
        proptest::prop_assert_eq!(p.package_references.len(), 1);
        proptest::prop_assert_eq!(p.package_references[0].op, PackageRefOp::Include);
        proptest::prop_assert_eq!(p.package_references[0].id.as_str(), "Newtonsoft.Json");
        proptest::prop_assert_eq!(
            p.package_references[0].version.as_deref(),
            Some("13.0.1")
        );
        proptest::prop_assert!(!p.package_references_uncertain);
    }

    #[test]
    fn compile_item_type_casing_stays_modelled_when_consumed_as_package_identity(
        item_type in ascii_case_variant("Compile"),
        item_ref in ascii_case_variant("Compile"),
    ) {
        let src = format!(
            r#"<Project>
  <ItemGroup>
    <{item_type} Include="Alpha.Package" />
  </ItemGroup>
  <ItemGroup>
    <PackageReference Include="@({item_ref})" Version="1.0" />
  </ItemGroup>
</Project>"#
        );
        let p = parse(&src);
        proptest::prop_assert!(p.package_references.is_empty());
        proptest::prop_assert!(
            p.package_references_uncertain,
            "Compile is modelled separately and must not be helper-expanded into packages"
        );
        let expected_cause = PackageReferenceUncertaintyCauseKind::UnevaluableIdentity {
            value: format!("@({item_ref})"),
        };
        proptest::prop_assert!(
            p.package_reference_uncertainties
                .iter()
                .any(|cause| cause.kind == expected_cause)
        );
    }

    #[test]
    fn package_version_item_type_casing_is_ignored(
        item_type in ascii_case_variant("PackageVersion")
    ) {
        let src = format!(
            r#"<Project>
  <ItemGroup>
    <{item_type} Include="Newtonsoft.Json" Version="13.0.1" />
  </ItemGroup>
  <ItemGroup>
    <PackageReference Include="Newtonsoft.Json" Version="13.0.1" />
  </ItemGroup>
</Project>"#
        );
        let p = parse(&src);
        proptest::prop_assert!(
            p.package_references_uncertain,
            "PackageVersion is a CPM item regardless of item-type casing"
        );
    }

    #[test]
    fn disabled_cpm_version_override_never_becomes_effective(
        property_name in ascii_case_variant("CentralPackageVersionOverrideEnabled"),
        false_value in ascii_case_variant("false"),
    ) {
        let src = format!(
            r#"<Project>
  <PropertyGroup>
    <CentralPackageVersionsFileImported>true</CentralPackageVersionsFileImported>
    <ManagePackageVersionsCentrally>true</ManagePackageVersionsCentrally>
    <{property_name}>{false_value}</{property_name}>
  </PropertyGroup>
  <ItemGroup>
    <PackageVersion Include="Newtonsoft.Json" Version="13.0.1" />
  </ItemGroup>
  <ItemGroup>
    <PackageReference Include="Newtonsoft.Json" VersionOverride="14.0.0" />
  </ItemGroup>
</Project>"#
        );
        let p = parse(&src);
        proptest::prop_assert!(
            p.package_references_uncertain,
            "disabled VersionOverride is a NuGet restore error, not an effective version"
        );
        proptest::prop_assert_eq!(only_package(&p).version.as_deref(), None);
        proptest::prop_assert_eq!(
            only_package(&p).version_override.as_deref(),
            Some("14.0.0")
        );
    }
}

#[test]
fn package_reference_version_attribute() {
    let src = r#"<Project>
  <ItemGroup>
    <PackageReference Include="Newtonsoft.Json" Version="13.0.1" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert!(p.items.is_empty(), "package ref must not leak into items");
    assert!(p.project_references.is_empty());
    let pr = only_package(&p);
    assert_eq!(pr.op, PackageRefOp::Include);
    assert_eq!(pr.id, "Newtonsoft.Json");
    assert_eq!(pr.version.as_deref(), Some("13.0.1"));
    assert!(pr.version_override.is_none());
    assert!(pr.private_assets.is_none());
    assert!(!p.package_references_uncertain);
    assert!(p.diagnostics.is_empty());
}

#[test]
fn package_reference_item_type_is_case_insensitive() {
    let src = r#"<Project>
  <ItemGroup>
    <packagereference Include="Newtonsoft.Json" Version="13.0.1" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert!(p.items.is_empty(), "package ref must not leak into items");
    assert!(p.project_references.is_empty());
    let pr = only_package(&p);
    assert_eq!(pr.op, PackageRefOp::Include);
    assert_eq!(pr.id, "Newtonsoft.Json");
    assert_eq!(pr.version.as_deref(), Some("13.0.1"));
    assert!(!p.package_references_uncertain);
}

#[test]
fn package_reference_version_child_element() {
    let src = r#"<Project>
  <ItemGroup>
    <PackageReference Include="Serilog">
      <Version>3.1.1</Version>
    </PackageReference>
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    let pr = only_package(&p);
    assert_eq!(pr.id, "Serilog");
    assert_eq!(pr.version.as_deref(), Some("3.1.1"));
}

#[test]
fn package_reference_no_version_is_none() {
    // The CPM shape: no Version on the reference. The id is still captured with
    // version=None and no diagnostic is raised — but because the version is
    // determined elsewhere (a central PackageVersion) which we don't hold, the
    // *set* is flagged uncertain so the resolver declines rather than resolving
    // a versionless dependency.
    let src = r#"<Project>
  <ItemGroup>
    <PackageReference Include="Some.Package" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    let pr = only_package(&p);
    assert_eq!(pr.id, "Some.Package");
    assert!(pr.version.is_none());
    assert!(p.diagnostics.is_empty(), "versionless is not a hard error");
    assert!(p.package_references_uncertain);
}

#[test]
fn package_reference_semicolon_split_shares_version() {
    // MSBuild item semantics: `Include="A;B"` is two items, both v1.
    let src = r#"<Project>
  <ItemGroup>
    <PackageReference Include="A;B" Version="1.0.0" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    let ids: Vec<&str> = p.package_references.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(ids, ["A", "B"]);
    assert!(
        p.package_references
            .iter()
            .all(|r| r.version.as_deref() == Some("1.0.0"))
    );
}

#[test]
fn package_reference_expands_properties_in_id_and_version() {
    let src = r#"<Project>
  <PropertyGroup>
    <PkgId>Foo.Bar</PkgId>
    <PkgVer>2.3.4</PkgVer>
  </PropertyGroup>
  <ItemGroup>
    <PackageReference Include="$(PkgId)" Version="$(PkgVer)" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    let pr = only_package(&p);
    assert_eq!(pr.id, "Foo.Bar");
    assert_eq!(pr.version.as_deref(), Some("2.3.4"));
}

#[test]
fn item_consuming_unpinned_property_is_uncertain_in_both_layouts() {
    // The property's stored value ("src/.fs") leaned on carved-out
    // $(TargetFramework) (never provably unset); an item Include consuming
    // it inherits the divergence risk regardless of which side of the
    // ItemGroup the property was written on — the item pass reads the
    // final table either way.
    let property_group = r#"  <PropertyGroup>
    <Src>src/$(TargetFramework).fs</Src>
  </PropertyGroup>
"#;
    let item_group = r#"  <ItemGroup>
    <Compile Include="$(Src)" />
  </ItemGroup>
"#;
    for (name, first, second) in [
        ("property first", property_group, item_group),
        ("item first", item_group, property_group),
    ] {
        let p = parse(&format!("<Project>\n{first}{second}</Project>"));
        assert_eq!(paths(&p.items), [Path::new("/repo/proj/src/.fs")], "{name}");
        assert!(
            p.items_uncertain,
            "{name}: the Include leaned on inexact $(TargetFramework); causes: {:?}",
            p.compile_item_uncertainties
        );
    }
}

#[test]
fn package_metadata_consuming_unpinned_property_is_uncertain() {
    let src = r#"<Project>
  <ItemGroup>
    <PackageReference Include="Foo" Version="$(Ver)" />
  </ItemGroup>
  <PropertyGroup>
    <Ver>1.$(TargetFramework)</Ver>
  </PropertyGroup>
</Project>"#;
    let p = parse(src);
    let pr = only_package(&p);
    assert_eq!(pr.version.as_deref(), Some("1."));
    assert!(
        p.package_references_uncertain,
        "the Version leaned on inexact $(TargetFramework); causes: {:?}",
        p.package_reference_uncertainties
    );
}

#[test]
fn unpinned_state_propagates_through_property_chains() {
    // B never references $(TargetFramework) (carved out: never provably
    // unset) directly, but its value was assembled from A, which did —
    // the root cause travels with the chain.
    let src = r#"<Project>
  <PropertyGroup>
    <A>$(TargetFramework)</A>
    <B>lib/$(A)</B>
  </PropertyGroup>
  <ItemGroup>
    <Compile Include="$(B)core.fs" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert_eq!(paths(&p.items), [Path::new("/repo/proj/lib/core.fs")]);
    assert!(
        p.items_uncertain,
        "B inherited A's unpinned state; causes: {:?}",
        p.compile_item_uncertainties
    );
}

#[test]
fn clean_overwrite_re_pins_a_property() {
    // The unpinned first write is superseded by a clean unconditional
    // write; the final value owes nothing to $(Missing), so the item is
    // certain (one diagnostic from the first write still marks the parse
    // partial).
    let src = r#"<Project>
  <PropertyGroup>
    <Src>src/$(Missing).fs</Src>
    <Src>src/Real.fs</Src>
  </PropertyGroup>
  <ItemGroup>
    <Compile Include="$(Src)" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert_eq!(paths(&p.items), [Path::new("/repo/proj/src/Real.fs")]);
    assert!(
        !p.items_uncertain,
        "the clean overwrite re-pinned Src; causes: {:?}",
        p.compile_item_uncertainties
    );
}

#[test]
fn compile_condition_reading_unpinned_property_records_condition_carve_out() {
    // The LSP's compile-uncertainty warning surfaces
    // `compile_condition_uncertainties`; a condition whose risk arrives via
    // an unpinned property (rather than a directly-inexact one; carved-out
    // `TargetFramework` is never provably unset) must populate it the same
    // way, naming the root cause.
    let src = r#"<Project>
  <PropertyGroup>
    <Use>$(TargetFramework)</Use>
  </PropertyGroup>
  <ItemGroup>
    <Compile Include="A.fs" Condition="'$(Use)' == ''" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert!(p.items_uncertain);
    assert!(
        p.compile_condition_uncertainties.iter().any(|carve_out| {
            carve_out.condition == "'$(Use)' == ''"
                && matches!(
                    &carve_out.reason,
                    CompileConditionReason::UndefinedProperties(names)
                        if names.iter().any(|n| n == "TargetFramework")
                )
        }),
        "expected a condition carve-out naming the root cause, got: {:?}",
        p.compile_condition_uncertainties
    );
}

#[test]
fn compile_condition_reading_unpinned_property_via_split_idiom_is_uncertain() {
    // The `$(V.Split('-')[0])` idiom inside `[System.Version]::Parse` reads
    // V through a non-simple reference. The unpinned scan must still see it:
    // V's value depends on a maybe-wrong gate, so a Compile condition
    // consuming it cannot leave the item set certain, even though the
    // condition itself evaluates cleanly.
    let src = r#"<Project>
  <PropertyGroup>
    <V>10.0.26200.1-preview</V>
  </PropertyGroup>
  <PropertyGroup Condition="'$(TargetFramework)' != 'no'">
    <V>10.0.26000.1-preview</V>
  </PropertyGroup>
  <ItemGroup>
    <Compile Include="A.fs" Condition="$([System.Version]::Parse('$(V.Split('-')[0])').Build) &lt;= 26100" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert!(
        p.items_uncertain,
        "condition consumes unpinned V via the Split idiom; causes: {:?}",
        p.compile_item_uncertainties
    );
}

#[test]
fn compile_condition_reading_pinned_property_via_split_idiom_stays_certain() {
    // Companion certainty pin: with V cleanly written, the same idiom
    // evaluates exactly and must not degrade the item set.
    let src = r#"<Project>
  <PropertyGroup>
    <V>10.0.26000.1-preview</V>
  </PropertyGroup>
  <ItemGroup>
    <Compile Include="A.fs" Condition="$([System.Version]::Parse('$(V.Split('-')[0])').Build) &lt;= 26100" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert_eq!(paths(&p.items), [Path::new("/repo/proj/A.fs")]);
    assert!(
        !p.items_uncertain,
        "pinned V through the Split idiom is exact; causes: {:?}",
        p.compile_item_uncertainties
    );
}

#[test]
fn child_with_cleanly_false_condition_stays_pinned_under_uncertain_group_gate() {
    // The group gate is unpinnable, but the child's own condition is
    // cleanly false — the write cannot happen whichever way the group goes,
    // so P's final value is exact and the item stays certain.
    let src = r#"<Project>
  <PropertyGroup>
    <P>Old.fs</P>
  </PropertyGroup>
  <PropertyGroup Condition="'$(Undef)' == 'true'">
    <P Condition="'false' == 'true'">New.fs</P>
  </PropertyGroup>
  <ItemGroup>
    <Compile Include="$(P)" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert_eq!(paths(&p.items), [Path::new("/repo/proj/Old.fs")]);
    assert!(
        !p.items_uncertain,
        "the child cannot write regardless of the gate; causes: {:?}",
        p.compile_item_uncertainties
    );
}

#[test]
fn maybe_run_property_write_unpins_the_property_for_item_reads() {
    // The gate evaluates TRUE here (undefined -> "" -> equal), and
    // `TargetFramework` is carved out (never provably unset) — a real
    // build that supplies it skips that write and compiles Old.fs
    // instead. The captured value is our best evaluation; the item
    // consuming it must degrade to uncertain.
    let src = r#"<Project>
  <PropertyGroup>
    <Src>Old.fs</Src>
    <Src Condition="'$(TargetFramework)' == ''">New.fs</Src>
  </PropertyGroup>
  <ItemGroup>
    <Compile Include="$(Src)" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert_eq!(paths(&p.items), [Path::new("/repo/proj/New.fs")]);
    assert!(
        p.items_uncertain,
        "a maybe-run write leaves the property unpinned; causes: {:?}",
        p.compile_item_uncertainties
    );
}

#[test]
fn maybe_run_group_write_unpins_written_properties() {
    // Group-level flavour of the above: the group's own gate is the
    // unpinnable one (`TargetFramework` is carved out, never provably
    // unset); its writes inherit the root.
    let src = r#"<Project>
  <PropertyGroup>
    <Ver>1.0</Ver>
  </PropertyGroup>
  <PropertyGroup Condition="'$(TargetFramework)' == ''">
    <Ver>2.0</Ver>
  </PropertyGroup>
  <ItemGroup>
    <PackageReference Include="Foo" Version="$(Ver)" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    let pr = only_package(&p);
    assert_eq!(pr.version.as_deref(), Some("2.0"));
    assert!(
        p.package_references_uncertain,
        "a maybe-run group write leaves Ver unpinned; causes: {:?}",
        p.package_reference_uncertainties
    );
}

#[test]
fn helper_item_consuming_unpinned_identity_degrades_the_consumer() {
    // The helper's identity is captured through the silent expansion path;
    // it must still see that PkgId's stored value leaned on carved-out
    // $(TargetFramework) (never provably unset) — a real build with
    // TargetFramework=Bar would reference FooBar.
    let src = r#"<Project>
  <PropertyGroup>
    <PkgId>Foo$(TargetFramework)</PkgId>
  </PropertyGroup>
  <ItemGroup>
    <ImplicitPackage Include="$(PkgId)" />
  </ItemGroup>
  <ItemGroup>
    <PackageReference Include="@(ImplicitPackage)" Version="1.0" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert!(
        p.package_references_uncertain,
        "helper identity leaned on unpinned PkgId; causes: {:?}",
        p.package_reference_uncertainties
    );
}

#[test]
fn helper_item_condition_reading_unpinned_property_degrades_the_consumer() {
    // The silent condition probe on helper items must also see unpinned
    // reads: UseIt's value leaned on carved-out $(TargetFramework), so
    // whether the helper item exists is not pinned down.
    let src = r#"<Project>
  <PropertyGroup>
    <UseIt>x$(TargetFramework)</UseIt>
  </PropertyGroup>
  <ItemGroup>
    <ImplicitPackage Include="Foo" Condition="'$(UseIt)' == 'x'" />
  </ItemGroup>
  <ItemGroup>
    <PackageReference Include="@(ImplicitPackage)" Version="1.0" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert!(
        p.package_references_uncertain,
        "helper gate leaned on unpinned UseIt; causes: {:?}",
        p.package_reference_uncertainties
    );
}

#[test]
fn maybe_skipped_property_write_taints_package_metadata_read() {
    // The gate reads a property we cannot pin — `TargetFramework` is a
    // consumer-contract carve-out (never provably unset), so the real
    // build may supply it, run the group, and change PkgVersion. Our
    // property pass skips it, so the captured 1.0 is our best evaluation,
    // but the value is not trustworthy for package purposes: the set must
    // degrade to uncertain. Both document orders (write-gate before or
    // after the item) carry the same risk — the item pass reads final
    // properties either way.
    for (name, src) in [
        (
            "read before maybe-skipped write",
            r#"<Project>
  <PropertyGroup><PkgVersion>1.0</PkgVersion></PropertyGroup>
  <ItemGroup><PackageReference Include="Foo" Version="$(PkgVersion)"/></ItemGroup>
  <PropertyGroup Condition="'$(TargetFramework)' == 'true'"><PkgVersion>2.0</PkgVersion></PropertyGroup>
</Project>"#,
        ),
        (
            "read after maybe-skipped write",
            r#"<Project>
  <PropertyGroup><PkgVersion>1.0</PkgVersion></PropertyGroup>
  <PropertyGroup Condition="'$(TargetFramework)' == 'true'"><PkgVersion>2.0</PkgVersion></PropertyGroup>
  <ItemGroup><PackageReference Include="Foo" Version="$(PkgVersion)"/></ItemGroup>
</Project>"#,
        ),
    ] {
        let p = parse(src);
        let pr = only_package(&p);
        assert_eq!(pr.version.as_deref(), Some("1.0"), "{name}");
        assert!(
            p.package_references_uncertain,
            "{name}: a maybe-skipped write must degrade the set; causes: {:?}",
            p.package_reference_uncertainties
        );
        assert!(
            p.package_reference_uncertainties.iter().any(|cause| {
                matches!(
                    &cause.kind,
                    PackageReferenceUncertaintyCauseKind::SdkDependencyItemPropertyEvaluation
                )
            }),
            "{name}: expected the tainted read to be recorded, got: {:?}",
            p.package_reference_uncertainties
        );
    }
}

#[test]
fn maybe_skipped_write_to_global_property_leaves_package_metadata_certain() {
    // `PkgVersion` is caller-supplied (a global): MSBuild discards the
    // project's write to it without even evaluating the gate, so the
    // maybe-skipped group cannot change it — the read stays certain even
    // though an unprotected sibling in the same group is rightly tainted.
    let src = r#"<Project>
  <PropertyGroup Condition="'$(Undef)' == 'true'">
    <PkgVersion>2.0</PkgVersion>
    <SomethingElse>x</SomethingElse>
  </PropertyGroup>
  <ItemGroup><PackageReference Include="Foo" Version="$(PkgVersion)"/></ItemGroup>
</Project>"#;
    let p = parse_fsproj(
        src,
        Path::new("/repo/proj/Demo.fsproj"),
        &HashMap::from([("PkgVersion".to_string(), "1.0".to_string())]),
        &HashMap::new(),
    )
    .expect("well-formed XML parses");
    let pr = only_package(&p);
    assert_eq!(pr.version.as_deref(), Some("1.0"));
    assert!(
        !p.package_references_uncertain,
        "a global-pinned property cannot be changed by a skipped write; causes: {:?}",
        p.package_reference_uncertainties
    );
}

#[test]
fn cleanly_skipped_property_write_leaves_package_metadata_certain() {
    // The gate reads a *defined* property, so the skip verdict is exact —
    // no taint, no uncertainty. Guards the fix above from over-tainting.
    let src = r#"<Project>
  <PropertyGroup><UseNew>no</UseNew><PkgVersion>1.0</PkgVersion></PropertyGroup>
  <PropertyGroup Condition="'$(UseNew)' == 'yes'"><PkgVersion>2.0</PkgVersion></PropertyGroup>
  <ItemGroup><PackageReference Include="Foo" Version="$(PkgVersion)"/></ItemGroup>
</Project>"#;
    let p = parse(src);
    let pr = only_package(&p);
    assert_eq!(pr.version.as_deref(), Some("1.0"));
    assert!(
        !p.package_references_uncertain,
        "a cleanly-false gate is exact; causes: {:?}",
        p.package_reference_uncertainties
    );
}

#[test]
fn package_metadata_uses_final_property_value_after_redefinition() {
    // MSBuild evaluates all properties before all items, so this
    // PackageReference sees the FINAL PkgVer=2.0 even though 1.0 was live at
    // the item's document position. The item pass captures exactly that —
    // no stale value, no uncertainty. (Verified against
    // `dotnet msbuild -getItem`.)
    let src = r#"<Project>
  <PropertyGroup>
    <PkgVer>1.0</PkgVer>
  </PropertyGroup>
  <ItemGroup>
    <PackageReference Include="Foo.Bar" Version="$(PkgVer)" />
  </ItemGroup>
  <PropertyGroup>
    <PkgVer>2.0</PkgVer>
  </PropertyGroup>
</Project>"#;
    let p = parse(src);
    let pr = only_package(&p);
    assert_eq!(pr.version.as_deref(), Some("2.0"));
    assert!(
        !p.package_references_uncertain,
        "the item pass reads final properties, so the capture is exact: {:?}",
        p.package_reference_uncertainties
    );
}

#[test]
fn consumed_helper_identity_uses_final_property_value() {
    // The helper item itself evaluates in the item pass, so its identity
    // reads the FINAL ImplicitPackageId=Bar — and the consuming
    // `PackageReference Include="@(ImplicitPackage)"` sees exactly that.
    // (Verified against `dotnet msbuild -getItem`: Identity=Bar.)
    let src = r#"<Project>
  <PropertyGroup>
    <ImplicitPackageId>Foo</ImplicitPackageId>
  </PropertyGroup>
  <ItemGroup>
    <ImplicitPackage Include="$(ImplicitPackageId)" />
  </ItemGroup>
  <ItemGroup>
    <PackageReference Include="@(ImplicitPackage)" Version="1.0" />
  </ItemGroup>
  <PropertyGroup>
    <ImplicitPackageId>Bar</ImplicitPackageId>
  </PropertyGroup>
</Project>"#;
    let p = parse(src);
    let pr = only_package(&p);
    assert_eq!(pr.id, "Bar");
    assert_eq!(pr.version.as_deref(), Some("1.0"));
    assert!(
        !p.package_references_uncertain,
        "helper and consumer both read final properties: {:?}",
        p.package_reference_uncertainties
    );
}

#[test]
fn consumed_helper_metadata_uses_final_property_value() {
    // Inherited helper metadata evaluates in the item pass too: the FINAL
    // ImplicitPackageVersion=2.0 flows through `@(ImplicitPackage)` to the
    // consuming PackageReference. (Verified against
    // `dotnet msbuild -getItem`: Version=2.0.)
    let src = r#"<Project>
  <PropertyGroup>
    <ImplicitPackageVersion>1.0</ImplicitPackageVersion>
  </PropertyGroup>
  <ItemGroup>
    <ImplicitPackage Include="Foo" Version="$(ImplicitPackageVersion)" />
  </ItemGroup>
  <ItemGroup>
    <PackageReference Include="@(ImplicitPackage)" />
  </ItemGroup>
  <PropertyGroup>
    <ImplicitPackageVersion>2.0</ImplicitPackageVersion>
  </PropertyGroup>
</Project>"#;
    let p = parse(src);
    let pr = only_package(&p);
    assert_eq!(pr.id, "Foo");
    assert_eq!(pr.version.as_deref(), Some("2.0"));
    assert!(
        !p.package_references_uncertain,
        "helper metadata reads final properties: {:?}",
        p.package_reference_uncertainties
    );
}

#[test]
fn package_reference_all_metadata() {
    let src = r#"<Project>
  <ItemGroup>
    <PackageReference Include="X" Version="1.0" VersionOverride="2.0"
                      IncludeAssets="compile" ExcludeAssets="runtime" PrivateAssets="all" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    let pr = only_package(&p);
    assert_eq!(pr.version.as_deref(), Some("1.0"));
    assert_eq!(pr.version_override.as_deref(), Some("2.0"));
    assert_eq!(pr.include_assets.as_deref(), Some("compile"));
    assert_eq!(pr.exclude_assets.as_deref(), Some("runtime"));
    assert_eq!(pr.private_assets.as_deref(), Some("all"));
}

#[test]
fn package_reference_expands_item_list_with_metadata() {
    let src = r#"<Project>
  <ItemGroup>
    <MIBCPackage Include="Alpha" Version="1.0" PrivateAssets="all" />
    <MIBCPackage Include="Beta">
      <Version>2.0</Version>
      <IncludeAssets>compile</IncludeAssets>
    </MIBCPackage>
  </ItemGroup>
  <ItemGroup>
    <PackageReference Include="@(MIBCPackage)" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert!(!p.package_references_uncertain);
    let ids: Vec<&str> = p.package_references.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(ids, ["Alpha", "Beta"]);
    assert_eq!(p.package_references[0].version.as_deref(), Some("1.0"));
    assert_eq!(
        p.package_references[0].private_assets.as_deref(),
        Some("all")
    );
    assert_eq!(p.package_references[1].version.as_deref(), Some("2.0"));
    assert_eq!(
        p.package_references[1].include_assets.as_deref(),
        Some("compile")
    );
}

#[test]
fn package_reference_item_list_local_metadata_overrides_source_metadata() {
    let src = r#"<Project>
  <ItemGroup>
    <MIBCPackage Include="Alpha" Version="1.0" PrivateAssets="all" />
  </ItemGroup>
  <ItemGroup>
    <PackageReference Include="@(MIBCPackage)" Version="9.9" ExcludeAssets="runtime" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    let pr = only_package(&p);
    assert_eq!(pr.id, "Alpha");
    assert_eq!(pr.version.as_deref(), Some("9.9"));
    assert_eq!(pr.private_assets.as_deref(), Some("all"));
    assert_eq!(pr.exclude_assets.as_deref(), Some("runtime"));
    assert!(!p.package_references_uncertain);
}

#[test]
fn package_reference_item_list_lowercase_local_metadata_overrides_source_metadata() {
    let src = r#"<Project>
  <ItemGroup>
    <MIBCPackage Include="Alpha" Version="1.0" PrivateAssets="all" />
  </ItemGroup>
  <ItemGroup>
    <PackageReference Include="@(MIBCPackage)" version="9.9" excludeassets="runtime" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    let pr = only_package(&p);
    assert_eq!(pr.id, "Alpha");
    assert_eq!(pr.version.as_deref(), Some("9.9"));
    assert_eq!(pr.private_assets.as_deref(), Some("all"));
    assert_eq!(pr.exclude_assets.as_deref(), Some("runtime"));
    assert!(!p.package_references_uncertain);
}

#[test]
fn package_reference_item_list_lowercase_child_metadata_overrides_source_metadata() {
    let src = r#"<Project>
  <ItemGroup>
    <MIBCPackage Include="Alpha" Version="1.0" />
  </ItemGroup>
  <ItemGroup>
    <PackageReference Include="@(MIBCPackage)">
      <version>9.9</version>
    </PackageReference>
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    let pr = only_package(&p);
    assert_eq!(pr.id, "Alpha");
    assert_eq!(pr.version.as_deref(), Some("9.9"));
    assert!(!p.package_references_uncertain);
}

#[test]
fn package_reference_item_list_empty_local_version_clears_source_metadata() {
    let src = r#"<Project>
  <ItemGroup>
    <MIBCPackage Include="Alpha" Version="1.0" PrivateAssets="all" />
  </ItemGroup>
  <ItemGroup>
    <PackageReference Include="@(MIBCPackage)" Version="" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    let pr = only_package(&p);
    assert_eq!(pr.id, "Alpha");
    assert_eq!(pr.version, None);
    assert_eq!(pr.private_assets.as_deref(), Some("all"));
    assert!(
        p.package_references_uncertain,
        "the local empty Version write makes the Include versionless"
    );
}

#[test]
fn package_reference_item_list_empty_child_version_clears_source_metadata() {
    let src = r#"<Project>
  <ItemGroup>
    <MIBCPackage Include="Alpha" Version="1.0" />
  </ItemGroup>
  <ItemGroup>
    <PackageReference Include="@(MIBCPackage)">
      <Version />
    </PackageReference>
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    let pr = only_package(&p);
    assert_eq!(pr.id, "Alpha");
    assert_eq!(pr.version, None);
    assert!(
        p.package_references_uncertain,
        "the local empty child Version write makes the Include versionless"
    );
}

#[test]
fn package_reference_item_list_mixed_with_literals_preserves_order() {
    let src = r#"<Project>
  <ItemGroup>
    <MIBCPackage Include="Middle" Version="2.0" />
  </ItemGroup>
  <ItemGroup>
    <PackageReference Include="First;@(MIBCPackage);Last" Version="1.0" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    let ids: Vec<&str> = p.package_references.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(ids, ["First", "Middle", "Last"]);
    assert_eq!(p.package_references[0].version.as_deref(), Some("1.0"));
    assert_eq!(p.package_references[1].version.as_deref(), Some("1.0"));
    assert_eq!(p.package_references[2].version.as_deref(), Some("1.0"));
    assert!(!p.package_references_uncertain);
}

#[test]
fn package_reference_exact_reference_to_prior_package_references_is_uncertain() {
    let src = r#"<Project>
  <ItemGroup>
    <PackageReference Include="Alpha" Version="1.0" />
    <PackageReference Include="@(PackageReference)" Version="2.0" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert_eq!(p.package_references.len(), 1);
    assert_eq!(p.package_references[0].id, "Alpha");
    assert!(
        p.package_references_uncertain,
        "the live PackageReference item table is not modelled for item-list expansion"
    );
    assert!(p.package_reference_uncertainties.iter().any(|cause| {
        cause.kind
            == PackageReferenceUncertaintyCauseKind::UnevaluableIdentity {
                value: "@(PackageReference)".to_string(),
            }
    }));
}

#[test]
fn package_reference_exact_reference_to_compile_items_is_uncertain() {
    let src = r#"<Project>
  <ItemGroup>
    <Compile Include="Alpha.Package" />
  </ItemGroup>
  <ItemGroup>
    <PackageReference Include="@(Compile)" Version="1.0" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert!(p.package_references.is_empty());
    assert!(
        p.package_references_uncertain,
        "the live Compile item table is modelled separately, not available for package expansion"
    );
    assert!(p.package_reference_uncertainties.iter().any(|cause| {
        cause.kind
            == PackageReferenceUncertaintyCauseKind::UnevaluableIdentity {
                value: "@(Compile)".to_string(),
            }
    }));
}

#[test]
fn package_reference_to_differently_cased_compile_items_is_uncertain() {
    let src = r#"<Project>
  <ItemGroup>
    <compile Include="Alpha.Package" />
  </ItemGroup>
  <ItemGroup>
    <PackageReference Include="@(Compile)" Version="1.0" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert!(p.package_references.is_empty());
    assert!(
        p.package_references_uncertain,
        "MSBuild item type names are case-insensitive; lowercase compile is still the modelled Compile list"
    );
    assert!(p.package_reference_uncertainties.iter().any(|cause| {
        cause.kind
            == PackageReferenceUncertaintyCauseKind::UnevaluableIdentity {
                value: "@(Compile)".to_string(),
            }
    }));
}

#[test]
fn package_reference_to_compile_items_from_untrusted_item_group_is_uncertain() {
    // Carved-out `TargetFramework` is never provably unset, so the gate is
    // untrusted.
    let src = r#"<Project>
  <ItemGroup Condition="'$(TargetFramework)' == 'true'">
    <Compile Include="Alpha.Package" />
  </ItemGroup>
  <ItemGroup>
    <PackageReference Include="@(Compile)" Version="1.0" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert!(p.package_references.is_empty());
    assert!(
        p.package_references_uncertain,
        "an untrusted ItemGroup condition can change a modelled list consumed as package identities"
    );
    assert!(p.package_reference_uncertainties.iter().any(|cause| {
        cause.kind
            == PackageReferenceUncertaintyCauseKind::UnevaluableIdentity {
                value: "@(Compile)".to_string(),
            }
    }));
}

#[test]
fn package_reference_to_conditioned_compile_item_is_uncertain() {
    let src = r#"<Project>
  <ItemGroup>
    <Compile Include="Alpha.Package" Condition="'$(TargetFramework)' == 'true'" />
  </ItemGroup>
  <ItemGroup>
    <PackageReference Include="@(Compile)" Version="1.0" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert!(p.package_references.is_empty());
    assert!(
        p.package_references_uncertain,
        "an untrusted modelled item condition can change a list consumed as package identities"
    );
    assert!(p.package_reference_uncertainties.iter().any(|cause| {
        cause.kind
            == PackageReferenceUncertaintyCauseKind::UnevaluableIdentity {
                value: "@(Compile)".to_string(),
            }
    }));
}

#[test]
fn package_reference_exact_reference_to_project_references_is_uncertain() {
    let src = r#"<Project>
  <ItemGroup>
    <ProjectReference Include="Alpha.Package" />
  </ItemGroup>
  <ItemGroup>
    <PackageReference Include="@(ProjectReference)" Version="1.0" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert!(p.package_references.is_empty());
    assert!(
        p.package_references_uncertain,
        "the live ProjectReference item table is modelled separately, not available for package expansion"
    );
    assert!(p.package_reference_uncertainties.iter().any(|cause| {
        cause.kind
            == PackageReferenceUncertaintyCauseKind::UnevaluableIdentity {
                value: "@(ProjectReference)".to_string(),
            }
    }));
}

#[test]
fn tainted_package_item_list_is_uncertain_when_consumed() {
    // The helper item's Version reads carved-out `TargetFramework` (never
    // provably unset), so the metadata is tainted; a provably-unset name
    // would instead give an exactly-empty Version, leaving the consumed
    // reference uncertain only via the versionless rule.
    let src = r#"<Project>
  <ItemGroup>
    <MIBCPackage Include="Alpha" Version="$(TargetFramework)" />
  </ItemGroup>
  <ItemGroup>
    <PackageReference Include="@(MIBCPackage)" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert!(p.package_references_uncertain);
    let pr = only_package(&p);
    assert_eq!(pr.id, "Alpha");
    assert_eq!(pr.version, None);
}

#[test]
fn helper_item_group_under_inexact_condition_is_uncertain_when_consumed() {
    // Carved-out `TargetFramework` is never provably unset, so the gate is
    // untrusted.
    let src = r#"<Project>
  <ItemGroup Condition="'$(TargetFramework)' == 'true'">
    <MIBCPackage Include="Alpha" Version="1.0" />
  </ItemGroup>
  <ItemGroup>
    <PackageReference Include="@(MIBCPackage)" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert!(p.package_references.is_empty());
    assert!(
        p.package_references_uncertain,
        "an untrusted helper ItemGroup condition can change a consumed package list"
    );
}

#[test]
fn helper_item_group_under_unsupported_condition_is_uncertain_when_consumed() {
    let src = r#"<Project>
  <ItemGroup Condition="Exists('mibc.props')">
    <MIBCPackage Include="Alpha" Version="1.0" />
  </ItemGroup>
  <ItemGroup>
    <PackageReference Include="@(MIBCPackage)" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert!(p.package_references.is_empty());
    assert!(p.package_references_uncertain);
}

#[test]
fn false_conditioned_helper_item_remove_does_not_taint_consumed_list() {
    let src = r#"<Project>
  <PropertyGroup>
    <Configuration>Debug</Configuration>
  </PropertyGroup>
  <ItemGroup>
    <MIBCPackage Include="Alpha" Version="1.0" />
    <MIBCPackage Remove="Alpha" Condition="'$(Configuration)' == 'Never'" />
  </ItemGroup>
  <ItemGroup>
    <PackageReference Include="@(MIBCPackage)" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    let pr = only_package(&p);
    assert_eq!(pr.id, "Alpha");
    assert_eq!(pr.version.as_deref(), Some("1.0"));
    assert!(
        !p.package_references_uncertain,
        "a cleanly false helper mutation is ignored by MSBuild"
    );
}

#[test]
fn helper_item_after_package_consumption_does_not_reach_earlier_package() {
    // Items evaluate in document order *within* the item pass, against final
    // properties: AddBeta finalises true, so Beta joins the list — but only
    // after the PackageReference already consumed it. MSBuild agrees, so the
    // capture is exact and certain.
    let src = r#"<Project>
  <ItemGroup>
    <MIBCPackage Include="Alpha" Version="1.0" />
    <PackageReference Include="@(MIBCPackage)" Version="9.0" />
    <MIBCPackage Include="Beta" Condition="'$(AddBeta)' == 'true'" />
  </ItemGroup>
  <PropertyGroup>
    <AddBeta>true</AddBeta>
  </PropertyGroup>
</Project>"#;
    let p = parse(src);
    let pr = only_package(&p);
    assert_eq!(pr.id, "Alpha");
    assert_eq!(pr.version.as_deref(), Some("9.0"));
    assert!(
        !p.package_references_uncertain,
        "a later helper operation cannot change an earlier consumption; causes: {:?}",
        p.package_reference_uncertainties
    );
}

#[test]
fn helper_item_before_later_package_consumption_flows_into_that_consumption() {
    // The counterpart: a second consumption after Beta's (final-property-
    // gated) inclusion sees both identities. Same document, one more
    // PackageReference — the two consumptions capture different snapshots of
    // the helper list, exactly as MSBuild evaluates them.
    let src = r#"<Project>
  <ItemGroup>
    <MIBCPackage Include="Alpha" Version="1.0" />
    <PackageReference Include="@(MIBCPackage)" Version="9.0" />
    <MIBCPackage Include="Beta" Condition="'$(AddBeta)' == 'true'" />
    <PackageReference Include="@(MIBCPackage)" Version="9.0" />
  </ItemGroup>
  <PropertyGroup>
    <AddBeta>true</AddBeta>
  </PropertyGroup>
</Project>"#;
    let p = parse(src);
    let ids: Vec<&str> = p
        .package_references
        .iter()
        .map(|pr| pr.id.as_str())
        .collect();
    assert_eq!(ids, vec!["Alpha", "Alpha", "Beta"]);
    assert!(
        !p.package_references_uncertain,
        "both consumptions evaluate exactly against document-order snapshots; causes: {:?}",
        p.package_reference_uncertainties
    );
}

#[test]
fn helper_item_remove_updates_captured_package_list() {
    let src = r#"<Project>
  <ItemGroup>
    <MIBCPackage Include="Alpha" Version="1.0" />
    <MIBCPackage Include="Beta" Version="2.0" />
    <MIBCPackage Remove="alpha" />
  </ItemGroup>
  <ItemGroup>
    <PackageReference Include="@(MIBCPackage)" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    let pr = only_package(&p);
    assert_eq!(pr.id, "Beta");
    assert_eq!(pr.version.as_deref(), Some("2.0"));
    assert!(
        !p.package_references_uncertain,
        "a literal helper Remove is applied to the captured helper table"
    );
}

#[test]
fn helper_item_exclude_updates_captured_package_list() {
    let src = r#"<Project>
  <ItemGroup>
    <MIBCPackage Include="Alpha;Beta" Exclude="beta" Version="1.0" />
  </ItemGroup>
  <ItemGroup>
    <PackageReference Include="@(MIBCPackage)" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    let pr = only_package(&p);
    assert_eq!(pr.id, "Alpha");
    assert_eq!(pr.version.as_deref(), Some("1.0"));
    assert!(!p.package_references_uncertain);
}

#[test]
fn helper_item_update_invalidates_captured_package_list() {
    let src = r#"<Project>
  <ItemGroup>
    <MIBCPackage Include="Alpha" Version="1.0" />
    <MIBCPackage Update="Alpha" Version="2.0" />
  </ItemGroup>
  <ItemGroup>
    <PackageReference Include="@(MIBCPackage)" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert!(
        p.package_references.is_empty(),
        "do not expand stale helper metadata after an unsupported Update"
    );
    assert!(p.package_references_uncertain);
}

#[test]
fn condition_tainted_helper_item_remove_invalidates_captured_package_list() {
    let src = r#"<Project>
  <ItemGroup>
    <MIBCPackage Include="Alpha" Version="1.0" />
    <MIBCPackage Remove="Alpha" Condition="'$(TargetFramework)' == 'true'" />
  </ItemGroup>
  <ItemGroup>
    <PackageReference Include="@(MIBCPackage)" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert!(
        p.package_references.is_empty(),
        "do not expand entries that a condition-tainted helper Remove may delete"
    );
    assert!(p.package_references_uncertain);
}

#[test]
fn framework_reference_expands_item_list() {
    let src = r#"<Project>
  <ItemGroup>
    <MyFramework Include="Microsoft.AspNetCore.App" />
    <MyFramework Include="Microsoft.WindowsDesktop.App" />
  </ItemGroup>
  <ItemGroup>
    <FrameworkReference Include="@(MyFramework)" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    let names: Vec<&str> = p
        .framework_references
        .iter()
        .map(|r| r.name.as_str())
        .collect();
    assert_eq!(
        names,
        ["Microsoft.AspNetCore.App", "Microsoft.WindowsDesktop.App"]
    );
    assert!(!p.package_references_uncertain);
}

#[test]
fn lone_package_reference_update_is_dropped() {
    // An `Update` matching no prior `Include` modifies nothing, so MSBuild's
    // evaluated item view (and ours) drops it entirely.
    let src = r#"<Project>
  <ItemGroup>
    <PackageReference Update="Central.Package" Version="9.9.9" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert!(
        p.package_references.is_empty(),
        "orphan Update must not surface as a reference"
    );
}

#[test]
fn package_reference_update_merges_local_metadata_not_helper_metadata() {
    // `Update="@(MIBCPackage)"` selects the identities Alpha/Beta but transfers
    // only the Update's *own* local metadata (PrivateAssets=none), never the
    // helper items' metadata. With matching Includes present, that local
    // metadata is folded onto each Include.
    let src = r#"<Project>
  <ItemGroup>
    <MIBCPackage Include="Alpha" Version="1.0" PrivateAssets="all" />
    <MIBCPackage Include="Beta">
      <Version>2.0</Version>
      <IncludeAssets>compile</IncludeAssets>
    </MIBCPackage>
  </ItemGroup>
  <ItemGroup>
    <PackageReference Include="Alpha" Version="1.0" />
    <PackageReference Include="Beta" Version="2.0" />
    <PackageReference Update="@(MIBCPackage)" PrivateAssets="none" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert!(!p.package_references_uncertain);
    let ids: Vec<&str> = p.package_references.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(ids, ["Alpha", "Beta"]);
    for pr in &p.package_references {
        assert_eq!(pr.op, PackageRefOp::Include);
        assert_eq!(pr.version_override, None);
        assert_eq!(pr.exclude_assets, None);
        // Helper `IncludeAssets`/`PrivateAssets` are NOT inherited by the
        // Update; only the Update's own PrivateAssets=none is merged on.
        assert_eq!(pr.include_assets, None);
        assert_eq!(pr.private_assets.as_deref(), Some("none"));
    }
    assert_eq!(p.package_references[0].version.as_deref(), Some("1.0"));
    assert_eq!(p.package_references[1].version.as_deref(), Some("2.0"));
}

#[test]
fn framework_reference_captured() {
    let src = r#"<Project>
  <ItemGroup>
    <FrameworkReference Include="Microsoft.AspNetCore.App" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert!(p.package_references.is_empty());
    assert_eq!(p.framework_references.len(), 1);
    assert_eq!(p.framework_references[0].name, "Microsoft.AspNetCore.App");
    assert!(!p.package_references_uncertain);
}

#[test]
fn package_reference_with_escaped_condition_is_gated_in() {
    // MSBuild unescapes `%XX` inside conditions, so `'%74rue' == 'true'` gates
    // the item IN (oracle-verified). We model that now (stage E2 of
    // `docs/msbuild-escaped-value-plan.md`); it used to degrade to unsupported —
    // fail-safe, but it cost a package MSBuild really does resolve.
    let src = r#"<Project>
  <ItemGroup>
    <PackageReference Include="Alpha" Version="1.0" Condition="'%74rue' == 'true'" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    let ids: Vec<&str> = p.package_references.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(ids, vec!["Alpha"], "the gate is true, so the item is in");
    assert!(!p.package_references_uncertain, "and it is certain");
}

#[test]
fn compile_item_with_escaped_condition_is_gated_in() {
    // Same class as the package test above, on the Compile side: the gate is
    // true, so the source file MSBuild compiles is the one we capture.
    let src = r#"<Project>
  <ItemGroup>
    <Compile Include="A.fs" Condition="'%74rue' == 'true'" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert_eq!(file_names(&p.items), vec!["A.fs"]);
    assert!(!p.items_uncertain);
}

#[test]
fn package_reference_false_condition_skipped_cleanly() {
    let src = r#"<Project>
  <ItemGroup>
    <PackageReference Include="Only.In.Debug" Version="1.0"
                      Condition="'$(Configuration)' == 'Debug'" />
  </ItemGroup>
</Project>"#;
    let p = parse_with(src, &[("Configuration", "Release")]);
    assert!(p.package_references.is_empty());
    assert!(
        !p.package_references_uncertain,
        "a clean exclusion is not uncertain"
    );
    assert!(!p.is_partial);
}

#[test]
fn package_reference_unsupported_condition_is_uncertain() {
    // An unsupported condition (a property function) gating a package ref:
    // we can't tell if it's in the set, so the package set is uncertain.
    let src = r#"<Project>
  <ItemGroup>
    <PackageReference Include="Maybe" Version="1.0"
                      Condition="Exists('foo.txt')" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert!(p.package_references.is_empty());
    assert!(p.package_references_uncertain);
    // Compile set is unaffected.
    assert!(!p.items_uncertain);
}

#[test]
fn package_reference_inexact_property_condition_is_uncertain() {
    // Carved-out `TargetFramework` is never provably unset, so the gate is
    // a divergence risk and the (empty) captured set cannot be trusted.
    let src = r#"<Project>
  <ItemGroup Condition="'$(TargetFramework)' == 'true'">
    <PackageReference Include="Maybe" Version="1.0" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert!(p.package_references.is_empty());
    assert!(p.package_references_uncertain);
}

#[test]
fn package_reference_remove_is_uncertain() {
    // We don't model item removal; a Remove could change the set.
    let src = r#"<Project>
  <ItemGroup>
    <PackageReference Include="A" Version="1.0" />
    <PackageReference Remove="A" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert!(p.package_references_uncertain);
}

#[test]
fn package_reference_exclude_removes_id() {
    let src = r#"<Project>
  <ItemGroup>
    <PackageReference Include="A;B;C" Exclude="B" Version="1.0" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    let ids: Vec<&str> = p.package_references.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(ids, ["A", "C"]);
    assert!(!p.package_references_uncertain);
}

#[test]
fn package_reference_exclude_glob_is_uncertain() {
    let src = r#"<Project>
  <ItemGroup>
    <PackageReference Include="A;B" Exclude="B*" Version="1.0" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert!(
        p.package_references_uncertain,
        "unresolvable Exclude → uncertain"
    );
}

#[test]
fn package_reference_conditioned_metadata_child_last_true_wins() {
    // Foo is unset → the first Version's condition is false, the second true.
    let src = r#"<Project>
  <ItemGroup>
    <PackageReference Include="X">
      <Version Condition="'$(Foo)' == 'yes'">1.0</Version>
      <Version Condition="'$(Foo)' != 'yes'">2.0</Version>
    </PackageReference>
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    let pr = only_package(&p);
    assert_eq!(pr.version.as_deref(), Some("2.0"));
}

#[test]
fn package_reference_unsupported_metadata_child_condition_is_uncertain() {
    let src = r#"<Project>
  <ItemGroup>
    <PackageReference Include="X">
      <Version Condition="Exists('foo.txt')">1.0</Version>
    </PackageReference>
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert!(p.package_references_uncertain);
}

#[test]
fn unresolved_import_marks_package_set_uncertain() {
    // An import we don't follow (pure parse) may carry PackageReferences, so
    // the captured set can't be trusted even though it's currently empty.
    let src = r#"<Project>
  <Import Project="Deps.props" />
</Project>"#;
    let p = parse(src);
    assert!(p.package_references.is_empty());
    assert!(
        p.package_references_uncertain,
        "an unfollowed import can carry package refs — set is untrustworthy"
    );
    // And the Compile analogue holds too (same structural risk).
    assert!(p.items_uncertain);
}

#[test]
fn choose_containing_package_ref_marks_uncertain() {
    // This Choose's When gate reads carved-out `TargetFramework` (never
    // provably unset), so the branch decision can't be pinned and we don't
    // descend; the PackageReference inside is hidden and the captured set
    // must be flagged untrustworthy (not silently empty).
    let src = r#"<Project>
  <Choose>
    <When Condition="'$(TargetFramework)' == '1'">
      <ItemGroup>
        <PackageReference Include="Hidden" Version="1.0" />
      </ItemGroup>
    </When>
  </Choose>
</Project>"#;
    let p = parse(src);
    assert!(p.package_references.is_empty());
    assert!(
        p.package_references_uncertain,
        "an un-descended <Choose> can hide package refs"
    );
    assert!(p.items_uncertain, "and the Compile analogue holds");
}

#[test]
fn no_resolver_sdk_marks_package_set_uncertain() {
    // `<Project Sdk="...">` with no SDK resolver never runs the SDK's implicit
    // package machinery (e.g. FSharp.Core), so the package set is incomplete.
    let src = r#"<Project Sdk="Microsoft.NET.Sdk">
  <ItemGroup>
    <PackageReference Include="Explicit" Version="1.0" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert!(
        p.package_references_uncertain,
        "an unrun SDK can inject implicit packages we can't see"
    );
}

#[test]
fn item_reference_in_package_metadata_is_uncertain() {
    // `Version="@(Versions)"` needs item evaluation MSBuild does and we don't,
    // so the captured version would be a raw expression — mark uncertain and
    // drop the metadata rather than reporting a wrong version as trusted.
    let src = r#"<Project>
  <ItemGroup>
    <PackageReference Include="X" Version="@(Versions)" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert!(p.package_references_uncertain);
    let pr = only_package(&p);
    assert_eq!(
        pr.version, None,
        "unevaluable metadata is dropped, not captured raw"
    );
}

#[test]
fn exact_item_list_reference_in_package_identity_is_uncertain() {
    // Exact item-list identities like @(Compile) are still item evaluation.
    // This evaluator does not use modelled Compile/ProjectReference tables as
    // package identities, so the dependency set must degrade to uncertain
    // rather than treating the reference as confidently empty.
    let src = r#"<Project>
  <ItemGroup>
    <Compile Include="A.fs" />
    <PackageReference Include="@(Compile)" Version="1.0" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert!(p.package_references.is_empty());
    assert!(p.package_references_uncertain);
    assert!(p.package_reference_uncertainties.iter().any(|cause| {
        cause.kind
            == PackageReferenceUncertaintyCauseKind::UnevaluableIdentity {
                value: "@(Compile)".to_string(),
            }
    }));
}

#[test]
fn exact_item_list_reference_in_framework_identity_is_uncertain() {
    let src = r#"<Project>
  <ItemGroup>
    <ProjectReference Include="../Lib/Lib.fsproj" />
    <FrameworkReference Include="@(ProjectReference)" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert!(p.framework_references.is_empty());
    assert!(p.package_references_uncertain);
    assert!(p.package_reference_uncertainties.iter().any(|cause| {
        cause.kind
            == PackageReferenceUncertaintyCauseKind::UnevaluableIdentity {
                value: "@(ProjectReference)".to_string(),
            }
    }));
}

#[test]
fn metadata_reference_in_package_metadata_is_uncertain() {
    let src = r#"<Project>
  <ItemGroup>
    <PackageReference Include="X" PrivateAssets="%(Identity)" Version="1.0" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert!(p.package_references_uncertain);
    let pr = only_package(&p);
    assert_eq!(pr.private_assets, None);
    // The clean Version is still captured.
    assert_eq!(pr.version.as_deref(), Some("1.0"));
}

#[test]
fn child_metadata_overrides_attribute() {
    // Attribute is the first write; a same-named child is a later write that
    // wins (MSBuild last-write-wins item metadata).
    let src = r#"<Project>
  <ItemGroup>
    <PackageReference Include="X" Version="1.0"><Version>2.0</Version></PackageReference>
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    let pr = only_package(&p);
    assert_eq!(pr.version.as_deref(), Some("2.0"));
    assert!(!p.package_references_uncertain);
}

#[test]
fn false_conditioned_child_leaves_attribute_intact() {
    // A child whose condition is false does not override the attribute.
    let src = r#"<Project>
  <ItemGroup>
    <PackageReference Include="X" Version="1.0">
      <Version Condition="'$(Foo)' == 'yes'">2.0</Version>
    </PackageReference>
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    let pr = only_package(&p);
    assert_eq!(pr.version.as_deref(), Some("1.0"));
}

#[test]
fn glob_in_package_id_is_uncertain() {
    // MSBuild globs the filesystem for a wildcard Include; a dependency id
    // like `NoSuch*` matches nothing, so capturing it literally would be wrong.
    let src = r#"<Project>
  <ItemGroup>
    <PackageReference Include="NoSuch*" Version="1.0" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert!(
        p.package_references.is_empty(),
        "a glob id is not captured literally"
    );
    assert!(p.package_references_uncertain);
}

#[test]
fn glob_in_framework_ref_is_uncertain() {
    let src = r#"<Project>
  <ItemGroup>
    <FrameworkReference Include="Microsoft.*" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert!(p.framework_references.is_empty());
    assert!(p.package_references_uncertain);
}

#[test]
fn exclude_is_case_insensitive() {
    // `Exclude="beta"` must remove `Beta` — else we over-resolve it.
    let src = r#"<Project>
  <ItemGroup>
    <PackageReference Include="Alpha;Beta" Exclude="beta" Version="1.0" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    let ids: Vec<&str> = p.package_references.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(ids, ["Alpha"]);
}

#[test]
fn item_definition_group_package_default_marks_uncertain() {
    // `<ItemDefinitionGroup>` sets a default <Version> MSBuild applies to
    // later package refs; we don't thread that through, so a ref relying on it
    // would carry the wrong metadata. Mark uncertain rather than report a
    // trusted version=None.
    let src = r#"<Project>
  <ItemDefinitionGroup>
    <PackageReference>
      <Version>1.2.3</Version>
    </PackageReference>
  </ItemDefinitionGroup>
  <ItemGroup>
    <PackageReference Include="A" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert!(
        p.package_references_uncertain,
        "an ItemDefinitionGroup package default we don't apply → uncertain"
    );
}

#[test]
fn item_definition_group_condition_reads_final_property_value() {
    // MSBuild evaluates item definitions in their own pass, after ALL
    // properties — so this gate reads the final UseDefaults=true even
    // though it is written later in the document, and the default applies
    // (verified against `dotnet msbuild -getItem`: Version=9.9.9). We
    // don't thread defaults, so the set degrades to uncertain.
    let src = r#"<Project>
  <ItemDefinitionGroup Condition="'$(UseDefaults)' == 'true'">
    <PackageReference>
      <Version>9.9.9</Version>
    </PackageReference>
  </ItemDefinitionGroup>
  <ItemGroup>
    <PackageReference Include="Foo" />
  </ItemGroup>
  <PropertyGroup>
    <UseDefaults>true</UseDefaults>
  </PropertyGroup>
</Project>"#;
    let p = parse(src);
    assert!(
        p.package_references_uncertain,
        "the finally-true gate applies the default in MSBuild; causes: {:?}",
        p.package_reference_uncertainties
    );
    assert!(p.package_reference_uncertainties.iter().any(|cause| {
        cause.kind == PackageReferenceUncertaintyCauseKind::ItemDefinitionDefault
    }));
}

#[test]
fn item_definition_group_with_finally_false_condition_stays_certain() {
    // The document-order value at the group's position is true, but the
    // FINAL value is false — MSBuild skips the definitions (verified
    // against `dotnet msbuild -getItem`: no Version on Foo), so nothing
    // degrades. The versionless include is the only remaining uncertainty
    // source, so pin the whole shape with a versioned one.
    let src = r#"<Project>
  <PropertyGroup>
    <UseDefaults>true</UseDefaults>
  </PropertyGroup>
  <ItemDefinitionGroup Condition="'$(UseDefaults)' == 'true'">
    <PackageReference>
      <IncludeAssets>compile</IncludeAssets>
    </PackageReference>
  </ItemDefinitionGroup>
  <ItemGroup>
    <PackageReference Include="Foo" Version="1.0" />
  </ItemGroup>
  <PropertyGroup>
    <UseDefaults>false</UseDefaults>
  </PropertyGroup>
</Project>"#;
    let p = parse(src);
    let pr = only_package(&p);
    assert_eq!(pr.id, "Foo");
    assert!(
        !p.package_references_uncertain,
        "a finally-false gate skips the definitions in every build; causes: {:?}",
        p.package_reference_uncertainties
    );
}

#[test]
fn item_definition_group_after_item_group_still_marks_uncertain() {
    // Item definitions evaluate before item groups regardless of document
    // order (verified against `dotnet msbuild -getItem`: Version=9.9.9 on
    // Foo), so a document-late group affects a document-early item.
    let src = r#"<Project>
  <ItemGroup>
    <PackageReference Include="Foo" />
  </ItemGroup>
  <ItemDefinitionGroup>
    <PackageReference>
      <Version>9.9.9</Version>
    </PackageReference>
  </ItemDefinitionGroup>
</Project>"#;
    let p = parse(src);
    assert!(
        p.package_references_uncertain,
        "the definition pass precedes the item pass; causes: {:?}",
        p.package_reference_uncertainties
    );
    assert!(p.package_reference_uncertainties.iter().any(|cause| {
        cause.kind == PackageReferenceUncertaintyCauseKind::ItemDefinitionDefault
    }));
}

#[test]
fn item_definition_group_helper_package_metadata_marks_consumed_list_uncertain() {
    let src = r#"<Project>
  <ItemDefinitionGroup>
    <MIBCPackage>
      <PrivateAssets>all</PrivateAssets>
    </MIBCPackage>
  </ItemDefinitionGroup>
  <ItemGroup>
    <MIBCPackage Include="Alpha" Version="1.0" />
  </ItemGroup>
  <ItemGroup>
    <PackageReference Include="@(MIBCPackage)" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    let pr = only_package(&p);
    assert_eq!(pr.id, "Alpha");
    assert_eq!(pr.private_assets, None);
    assert!(
        p.package_references_uncertain,
        "helper item defaults can flow package metadata into consumed lists"
    );
}

#[test]
fn false_conditioned_item_definition_group_helper_metadata_stays_certain() {
    let src = r#"<Project>
  <PropertyGroup>
    <UseHelperDefaults>false</UseHelperDefaults>
  </PropertyGroup>
  <ItemDefinitionGroup Condition="'$(UseHelperDefaults)' == 'true'">
    <MIBCPackage>
      <PrivateAssets>all</PrivateAssets>
    </MIBCPackage>
  </ItemDefinitionGroup>
  <ItemGroup>
    <MIBCPackage Include="Alpha" Version="1.0" />
  </ItemGroup>
  <ItemGroup>
    <PackageReference Include="@(MIBCPackage)" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    let pr = only_package(&p);
    assert_eq!(pr.id, "Alpha");
    assert_eq!(pr.private_assets, None);
    assert!(
        !p.package_references_uncertain,
        "a cleanly false helper default must not force package fallback"
    );
}

#[test]
fn speculative_helper_item_definition_default_does_not_shadow_later_real_default() {
    let src = r#"<Project>
  <PropertyGroup>
    <UseDefault>false</UseDefault>
  </PropertyGroup>
  <ItemDefinitionGroup>
    <MIBCPackage>
      <IncludeAssets Condition="'$(UseDefault)' == 'true'">compile</IncludeAssets>
      <IncludeAssets>runtime</IncludeAssets>
    </MIBCPackage>
  </ItemDefinitionGroup>
  <ItemGroup>
    <MIBCPackage Include="Alpha" Version="1.0" />
  </ItemGroup>
  <ItemGroup>
    <PackageReference Include="@(MIBCPackage)" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    let pr = only_package(&p);
    assert_eq!(pr.id, "Alpha");
    assert_eq!(pr.include_assets, None);
    assert!(
        p.package_references_uncertain,
        "the unconditional helper default applies in MSBuild and must be tracked; causes: {:?}",
        p.package_reference_uncertainties
    );
    assert!(p.package_reference_uncertainties.iter().any(|cause| {
        cause.kind == PackageReferenceUncertaintyCauseKind::ItemDefinitionDefault
    }));
}

#[test]
fn item_definition_group_helper_package_metadata_after_consumption_marks_uncertain() {
    let src = r#"<Project>
  <ItemGroup>
    <MIBCPackage Include="Alpha" Version="1.0" />
  </ItemGroup>
  <ItemGroup>
    <PackageReference Include="@(MIBCPackage)" />
  </ItemGroup>
  <ItemDefinitionGroup>
    <MIBCPackage>
      <PrivateAssets>all</PrivateAssets>
    </MIBCPackage>
  </ItemDefinitionGroup>
</Project>"#;
    let p = parse(src);
    let pr = only_package(&p);
    assert_eq!(pr.id, "Alpha");
    assert_eq!(pr.private_assets, None);
    assert!(
        p.package_references_uncertain,
        "MSBuild applies item definitions before item groups, so a later helper default can affect an already-expanded package ref"
    );
    assert!(p.package_reference_uncertainties.iter().any(|cause| {
        cause.kind == PackageReferenceUncertaintyCauseKind::ItemDefinitionDefault
    }));
}

#[test]
fn helper_empty_metadata_overrides_item_definition_default_stays_certain() {
    let src = r#"<Project>
  <ItemDefinitionGroup>
    <MIBCPackage>
      <PrivateAssets>all</PrivateAssets>
    </MIBCPackage>
  </ItemDefinitionGroup>
  <ItemGroup>
    <MIBCPackage Include="Alpha" Version="1.0" PrivateAssets="" />
  </ItemGroup>
  <ItemGroup>
    <PackageReference Include="@(MIBCPackage)" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    let pr = only_package(&p);
    assert_eq!(pr.id, "Alpha");
    assert_eq!(pr.private_assets, None);
    assert!(
        !p.package_references_uncertain,
        "an explicit empty helper metadata write suppresses the helper default"
    );
}

#[test]
fn item_definition_group_helper_metadata_overridden_by_package_reference_stays_certain() {
    let src = r#"<Project>
  <ItemDefinitionGroup>
    <MIBCPackage>
      <Version>1.0</Version>
    </MIBCPackage>
  </ItemDefinitionGroup>
  <ItemGroup>
    <MIBCPackage Include="Alpha" />
  </ItemGroup>
  <ItemGroup>
    <PackageReference Include="@(MIBCPackage)" Version="2.0" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    let pr = only_package(&p);
    assert_eq!(pr.id, "Alpha");
    assert_eq!(pr.version.as_deref(), Some("2.0"));
    assert!(
        !p.package_references_uncertain,
        "a local PackageReference metadata write overrides the helper default"
    );
}

#[test]
fn item_definition_group_helper_metadata_ignored_by_package_update_stays_certain() {
    // `PackageReference Update="@(MIBCPackage)"` selects the identity Alpha via
    // the helper list but has no matching `PackageReference Include`, so it is
    // a lone Update: it modifies nothing, inherits neither the helper's nor the
    // ItemDefinitionGroup's metadata, and is dropped. The set stays certain.
    let src = r#"<Project>
  <ItemDefinitionGroup>
    <MIBCPackage>
      <Version>1.0</Version>
    </MIBCPackage>
  </ItemDefinitionGroup>
  <ItemGroup>
    <MIBCPackage Include="Alpha" />
  </ItemGroup>
  <ItemGroup>
    <PackageReference Update="@(MIBCPackage)" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert!(
        p.package_references.is_empty(),
        "lone Update selecting Alpha modifies nothing and is dropped"
    );
    assert!(
        !p.package_references_uncertain,
        "PackageReference Update uses the helper list only for identities"
    );
}

#[test]
fn item_definition_group_helper_metadata_ignored_by_framework_reference_stays_certain() {
    let src = r#"<Project>
  <ItemDefinitionGroup>
    <MyFramework>
      <PrivateAssets>all</PrivateAssets>
    </MyFramework>
  </ItemDefinitionGroup>
  <ItemGroup>
    <MyFramework Include="Microsoft.NETCore.App" />
  </ItemGroup>
  <ItemGroup>
    <FrameworkReference Include="@(MyFramework)" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert_eq!(p.framework_references.len(), 1);
    assert_eq!(p.framework_references[0].name, "Microsoft.NETCore.App");
    assert!(
        !p.package_references_uncertain,
        "FrameworkReference consumes helper identities but not package metadata"
    );
}

#[test]
fn inherited_helper_package_metadata_uncertainty_marks_package_uncertain() {
    let src = r#"<Project>
  <ItemGroup>
    <MIBCPackage Include="Alpha" Version="$(TargetFramework)" />
  </ItemGroup>
  <ItemGroup>
    <PackageReference Include="@(MIBCPackage)" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    let pr = only_package(&p);
    assert_eq!(pr.id, "Alpha");
    assert_eq!(pr.version, None);
    assert!(p.package_references_uncertain);
    assert!(p.package_reference_uncertainties.iter().any(|cause| {
        cause.kind
            == PackageReferenceUncertaintyCauseKind::UnevaluableMetadata {
                name: "Version".to_string(),
                value: "$(TargetFramework)".to_string(),
            }
    }));
}

#[test]
fn item_definition_group_without_package_child_is_not_package_uncertain() {
    // A Compile-only ItemDefinitionGroup must not spuriously flag the package
    // set (it carries no dependency defaults).
    let src = r#"<Project>
  <ItemDefinitionGroup>
    <Compile>
      <Visible>false</Visible>
    </Compile>
  </ItemDefinitionGroup>
  <ItemGroup>
    <PackageReference Include="A" Version="1.0" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert!(!p.package_references_uncertain);
}

#[test]
fn package_version_item_marks_uncertain() {
    // A bare `<PackageVersion>` item is only a CPM input if central package
    // management is actually enabled. Without that opt-in, a versionless
    // PackageReference is still not safe to resolve.
    let src = r#"<Project>
  <ItemGroup>
    <PackageVersion Include="Newtonsoft.Json" Version="13.0.1" />
  </ItemGroup>
  <ItemGroup>
    <PackageReference Include="Newtonsoft.Json" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert!(p.package_references_uncertain);
    assert_eq!(p.package_versions.len(), 1);
    assert_eq!(p.package_versions[0].id, "Newtonsoft.Json");
    assert_eq!(p.package_versions[0].version.as_deref(), Some("13.0.1"));
}

#[test]
fn inline_cpm_applies_central_version_and_clears_uncertainty() {
    let src = r#"<Project>
  <PropertyGroup>
    <CentralPackageVersionsFileImported>true</CentralPackageVersionsFileImported>
    <ManagePackageVersionsCentrally>true</ManagePackageVersionsCentrally>
  </PropertyGroup>
  <ItemGroup>
    <PackageVersion Include="Newtonsoft.Json" Version="13.0.1" />
  </ItemGroup>
  <ItemGroup>
    <PackageReference Include="Newtonsoft.Json" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert!(!p.package_references_uncertain);
    assert!(p.package_reference_uncertainties.is_empty());
    assert_eq!(only_package(&p).id, "Newtonsoft.Json");
    assert_eq!(only_package(&p).version.as_deref(), Some("13.0.1"));
    assert_eq!(only_package(&p).version_override, None);
}

#[test]
fn inline_cpm_tainted_manage_flag_stays_uncertain() {
    let src = r#"<Project>
  <PropertyGroup>
    <CentralPackageVersionsFileImported>true</CentralPackageVersionsFileImported>
    <ManagePackageVersionsCentrally Condition="'$(TargetFramework)' == ''">true</ManagePackageVersionsCentrally>
  </PropertyGroup>
  <ItemGroup>
    <PackageVersion Include="Newtonsoft.Json" Version="13.0.1" />
  </ItemGroup>
  <ItemGroup>
    <PackageReference Include="Newtonsoft.Json" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert!(p.package_references_uncertain);
    assert_eq!(only_package(&p).version.as_deref(), Some("13.0.1"));
    assert!(p.package_reference_uncertainties.iter().any(|cause| {
        matches!(
            &cause.kind,
            PackageReferenceUncertaintyCauseKind::Diagnostic(
                DiagnosticKind::UndefinedProperty { name }
            ) if name == "TargetFramework"
        )
    }));
}

#[test]
fn inline_cpm_tainted_import_marker_stays_uncertain() {
    let src = r#"<Project>
  <PropertyGroup>
    <CentralPackageVersionsFileImported>true$(TargetFramework)</CentralPackageVersionsFileImported>
    <ManagePackageVersionsCentrally>true</ManagePackageVersionsCentrally>
  </PropertyGroup>
  <ItemGroup>
    <PackageVersion Include="Newtonsoft.Json" Version="13.0.1" />
  </ItemGroup>
  <ItemGroup>
    <PackageReference Include="Newtonsoft.Json" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert!(p.package_references_uncertain);
    assert_eq!(only_package(&p).version.as_deref(), Some("13.0.1"));
    assert!(p.package_reference_uncertainties.iter().any(|cause| {
        matches!(
            &cause.kind,
            PackageReferenceUncertaintyCauseKind::Diagnostic(
                DiagnosticKind::UndefinedProperty { name }
            ) if name == "TargetFramework"
        )
    }));
}

#[test]
fn inline_cpm_version_override_wins_over_central_version() {
    let src = r#"<Project>
  <PropertyGroup>
    <CentralPackageVersionsFileImported>true</CentralPackageVersionsFileImported>
    <ManagePackageVersionsCentrally>true</ManagePackageVersionsCentrally>
  </PropertyGroup>
  <ItemGroup>
    <PackageVersion Include="Newtonsoft.Json" Version="13.0.1" />
  </ItemGroup>
  <ItemGroup>
    <PackageReference Include="Newtonsoft.Json" VersionOverride="14.0.0" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert!(!p.package_references_uncertain);
    assert_eq!(only_package(&p).version.as_deref(), Some("14.0.0"));
    assert_eq!(only_package(&p).version_override.as_deref(), Some("14.0.0"));
}

#[test]
fn inline_cpm_disabled_version_override_stays_uncertain() {
    let src = r#"<Project>
  <PropertyGroup>
    <CentralPackageVersionsFileImported>true</CentralPackageVersionsFileImported>
    <ManagePackageVersionsCentrally>true</ManagePackageVersionsCentrally>
    <CentralPackageVersionOverrideEnabled>false</CentralPackageVersionOverrideEnabled>
  </PropertyGroup>
  <ItemGroup>
    <PackageVersion Include="Newtonsoft.Json" Version="13.0.1" />
  </ItemGroup>
  <ItemGroup>
    <PackageReference Include="Newtonsoft.Json" VersionOverride="14.0.0" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert!(
        p.package_references_uncertain,
        "disabled VersionOverride is a NuGet restore error, not an effective version"
    );
    assert_eq!(only_package(&p).version, None);
    assert_eq!(only_package(&p).version_override.as_deref(), Some("14.0.0"));
}

#[test]
fn inline_cpm_local_version_stays_uncertain_under_marked_cpm() {
    let src = r#"<Project>
  <PropertyGroup>
    <CentralPackageVersionsFileImported>true</CentralPackageVersionsFileImported>
    <ManagePackageVersionsCentrally>true</ManagePackageVersionsCentrally>
  </PropertyGroup>
  <ItemGroup>
    <PackageVersion Include="Newtonsoft.Json" Version="13.0.1" />
  </ItemGroup>
  <ItemGroup>
    <PackageReference Include="Newtonsoft.Json" Version="12.0.0" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert!(p.package_references_uncertain);
    assert_eq!(only_package(&p).version.as_deref(), Some("12.0.0"));
    assert_eq!(only_package(&p).version_override, None);
}

#[test]
fn inline_cpm_without_import_marker_stays_uncertain() {
    let src = r#"<Project>
  <PropertyGroup>
    <ManagePackageVersionsCentrally>true</ManagePackageVersionsCentrally>
  </PropertyGroup>
  <ItemGroup>
    <PackageVersion Include="Newtonsoft.Json" Version="13.0.1" />
  </ItemGroup>
  <ItemGroup>
    <PackageReference Include="Newtonsoft.Json" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert!(p.package_references_uncertain);
    assert_eq!(only_package(&p).version, None);
    assert!(p.package_reference_uncertainties.iter().any(|cause| {
        cause.kind == PackageReferenceUncertaintyCauseKind::ManagePackageVersionsCentrally
    }));
}

#[test]
fn inline_cpm_missing_central_version_stays_uncertain() {
    let src = r#"<Project>
  <PropertyGroup>
    <CentralPackageVersionsFileImported>true</CentralPackageVersionsFileImported>
    <ManagePackageVersionsCentrally>true</ManagePackageVersionsCentrally>
  </PropertyGroup>
  <ItemGroup>
    <PackageVersion Include="Other.Package" Version="1.0.0" />
  </ItemGroup>
  <ItemGroup>
    <PackageReference Include="Newtonsoft.Json" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert!(p.package_references_uncertain);
    assert_eq!(only_package(&p).version, None);
    assert!(p.package_reference_uncertainties.iter().any(|cause| {
        cause.kind
            == PackageReferenceUncertaintyCauseKind::VersionlessPackageReference {
                id: "Newtonsoft.Json".to_string(),
            }
    }));
}

#[test]
fn inline_cpm_duplicate_central_versions_stay_uncertain() {
    let src = r#"<Project>
  <PropertyGroup>
    <CentralPackageVersionsFileImported>true</CentralPackageVersionsFileImported>
    <ManagePackageVersionsCentrally>true</ManagePackageVersionsCentrally>
  </PropertyGroup>
  <ItemGroup>
    <PackageVersion Include="Newtonsoft.Json" Version="13.0.1" />
    <PackageVersion Include="newtonsoft.json" Version="14.0.0" />
  </ItemGroup>
  <ItemGroup>
    <PackageReference Include="Newtonsoft.Json" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert!(p.package_references_uncertain);
    let include = p
        .package_references
        .iter()
        .find(|reference| reference.op == PackageRefOp::Include)
        .expect("include reference");
    assert_eq!(include.version, None);
}

#[test]
fn inline_cpm_package_reference_update_version_override_merges_and_resolves() {
    // An `Update` supplying `VersionOverride` merges onto the versionless
    // `Include`; CPM then applies the (enabled) override, so the reference
    // resolves and the set is certain.
    let src = r#"<Project>
  <PropertyGroup>
    <CentralPackageVersionsFileImported>true</CentralPackageVersionsFileImported>
    <ManagePackageVersionsCentrally>true</ManagePackageVersionsCentrally>
  </PropertyGroup>
  <ItemGroup>
    <PackageVersion Include="Newtonsoft.Json" Version="13.0.1" />
  </ItemGroup>
  <ItemGroup>
    <PackageReference Include="Newtonsoft.Json" />
    <PackageReference Update="Newtonsoft.Json" VersionOverride="14.0.0" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert!(!p.package_references_uncertain);
    assert!(p.package_reference_uncertainties.is_empty());
    assert_eq!(only_package(&p).id, "Newtonsoft.Json");
    assert_eq!(only_package(&p).version.as_deref(), Some("14.0.0"));
    assert_eq!(only_package(&p).version_override.as_deref(), Some("14.0.0"));
}

#[test]
fn inline_cpm_package_reference_update_version_stays_uncertain() {
    // An `Update` supplying a bare `Version` (not `VersionOverride`) merges a
    // local version onto the `Include` — the NU1008 error shape under CPM.
    // We keep the central-version machinery from firing and stay uncertain.
    let src = r#"<Project>
  <PropertyGroup>
    <CentralPackageVersionsFileImported>true</CentralPackageVersionsFileImported>
    <ManagePackageVersionsCentrally>true</ManagePackageVersionsCentrally>
  </PropertyGroup>
  <ItemGroup>
    <PackageVersion Include="Newtonsoft.Json" Version="13.0.1" />
  </ItemGroup>
  <ItemGroup>
    <PackageReference Include="Newtonsoft.Json" />
    <PackageReference Update="Newtonsoft.Json" Version="14.0.0" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert!(p.package_references_uncertain);
    assert_eq!(only_package(&p).version.as_deref(), Some("14.0.0"));
}

#[test]
fn non_cpm_update_version_override_stays_uncertain() {
    // Outside CPM a `VersionOverride` is inert (NuGet ignores it), so an
    // `Include` completed only by a merged `Update VersionOverride=` has no
    // determined version — the versionless uncertainty must survive.
    let src = r#"<Project>
  <ItemGroup>
    <PackageReference Include="A" />
    <PackageReference Update="A" VersionOverride="2.0" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert!(
        p.package_references_uncertain,
        "override-only completion is not an effective version outside CPM"
    );
    assert_eq!(only_package(&p).version, None);
    assert_eq!(only_package(&p).version_override.as_deref(), Some("2.0"));
}

#[test]
fn non_cpm_update_version_resolves_and_is_certain() {
    // A concrete `Version` merged from an `Update` *does* determine the
    // effective version, so the versionless uncertainty is retracted.
    let src = r#"<Project>
  <ItemGroup>
    <PackageReference Include="A" />
    <PackageReference Update="A" Version="2.0" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert!(!p.package_references_uncertain);
    assert_eq!(only_package(&p).version.as_deref(), Some("2.0"));
}

#[test]
fn update_clearing_prior_include_version_becomes_versionless() {
    // `Update Version=""` clears the Include's version in MSBuild (the item
    // ends versionless). The merge folds the clear onto the Include, so the
    // effective reference is versionless — uncertain, never a stale `1.0`.
    let src = r#"<Project>
  <ItemGroup>
    <PackageReference Include="A" Version="1.0" />
    <PackageReference Update="A" Version="" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert!(p.package_references_uncertain);
    assert_eq!(only_package(&p).version, None);
}

#[test]
fn update_clearing_version_from_earlier_update_becomes_versionless() {
    // The value being cleared was introduced by an *earlier* Update, not the
    // Include. The three-state merge still folds each Update in order, so the
    // final clear wins and the effective reference is versionless.
    let src = r#"<Project>
  <ItemGroup>
    <PackageReference Include="A" />
    <PackageReference Update="A" Version="1.0" />
    <PackageReference Update="A" Version="" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert!(p.package_references_uncertain);
    assert_eq!(only_package(&p).version, None);
}

#[test]
fn update_clearing_absent_metadata_is_a_noop_and_stays_certain() {
    // A clear that erases nothing (the Include never set PrivateAssets) is a
    // genuine no-op: no divergence, so no degrade.
    let src = r#"<Project>
  <ItemGroup>
    <PackageReference Include="A" Version="1.0" />
    <PackageReference Update="A" PrivateAssets="" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert!(!p.package_references_uncertain);
    assert_eq!(only_package(&p).version.as_deref(), Some("1.0"));
}

#[test]
fn lone_update_clear_does_not_degrade() {
    // A clear on a lone Update (no matching Include) erases nothing — the
    // Update is dropped — so it must not degrade an otherwise-certain project.
    let src = r#"<Project>
  <ItemGroup>
    <PackageReference Include="A" Version="1.0" />
    <PackageReference Update="Other" Version="" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert!(!p.package_references_uncertain);
    assert_eq!(only_package(&p).id, "A");
    assert_eq!(only_package(&p).version.as_deref(), Some("1.0"));
}

#[test]
fn inline_cpm_metadata_only_update_still_resolves_from_central() {
    // A metadata-only `Update` (no version, no override) can no longer veto
    // CPM version application: the versionless `Include` still resolves from
    // the central `PackageVersion`, and the Update's asset metadata merges on.
    let src = r#"<Project>
  <PropertyGroup>
    <CentralPackageVersionsFileImported>true</CentralPackageVersionsFileImported>
    <ManagePackageVersionsCentrally>true</ManagePackageVersionsCentrally>
  </PropertyGroup>
  <ItemGroup>
    <PackageVersion Include="Newtonsoft.Json" Version="13.0.1" />
  </ItemGroup>
  <ItemGroup>
    <PackageReference Include="Newtonsoft.Json" />
    <PackageReference Update="Newtonsoft.Json" PrivateAssets="all" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert!(!p.package_references_uncertain);
    assert!(p.package_reference_uncertainties.is_empty());
    assert_eq!(only_package(&p).version.as_deref(), Some("13.0.1"));
    assert_eq!(only_package(&p).private_assets.as_deref(), Some("all"));
}

#[test]
fn inline_cpm_package_version_update_stays_uncertain() {
    let src = r#"<Project>
  <PropertyGroup>
    <CentralPackageVersionsFileImported>true</CentralPackageVersionsFileImported>
    <ManagePackageVersionsCentrally>true</ManagePackageVersionsCentrally>
  </PropertyGroup>
  <ItemGroup>
    <PackageVersion Include="Newtonsoft.Json" Version="13.0.1" />
    <PackageVersion Update="Newtonsoft.Json" Version="14.0.0" />
  </ItemGroup>
  <ItemGroup>
    <PackageReference Include="Newtonsoft.Json" Version="12.0.0" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert!(p.package_references_uncertain);
    assert_eq!(only_package(&p).version.as_deref(), Some("12.0.0"));
}

#[test]
fn package_version_item_type_is_case_insensitive() {
    let src = r#"<Project>
  <ItemGroup>
    <packageversion Include="Newtonsoft.Json" Version="13.0.1" />
  </ItemGroup>
  <ItemGroup>
    <PackageReference Include="Newtonsoft.Json" Version="13.0.1" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert!(
        p.package_references_uncertain,
        "lowercase PackageVersion is still a CPM item, not a generic helper"
    );
    assert_eq!(p.package_versions.len(), 1);
    assert_eq!(p.package_versions[0].id, "Newtonsoft.Json");
    assert_eq!(p.package_versions[0].version.as_deref(), Some("13.0.1"));
}

#[test]
fn package_version_item_list_inherits_helper_version() {
    let src = r#"<Project>
  <ItemGroup>
    <CentralVersion Include="Newtonsoft.Json" Version="13.0.1" />
    <CentralVersion Include="Serilog">
      <Version>3.1.1</Version>
    </CentralVersion>
  </ItemGroup>
  <ItemGroup>
    <PackageVersion Include="@(CentralVersion)" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert!(p.package_references_uncertain);
    let versions: Vec<(&str, Option<&str>)> = p
        .package_versions
        .iter()
        .map(|v| (v.id.as_str(), v.version.as_deref()))
        .collect();
    assert_eq!(
        versions,
        [
            ("Newtonsoft.Json", Some("13.0.1")),
            ("Serilog", Some("3.1.1"))
        ]
    );
}

#[test]
fn package_reference_exact_reference_to_package_versions_is_uncertain() {
    let src = r#"<Project>
  <ItemGroup>
    <PackageVersion Include="Alpha" Version="1.0" />
  </ItemGroup>
  <ItemGroup>
    <PackageReference Include="@(PackageVersion)" Version="2.0" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert!(p.package_references.is_empty());
    assert!(p.package_references_uncertain);
    assert!(p.package_reference_uncertainties.iter().any(|cause| {
        cause.kind
            == PackageReferenceUncertaintyCauseKind::UnevaluableIdentity {
                value: "@(PackageVersion)".to_string(),
            }
    }));
}

#[test]
fn global_package_reference_marks_uncertain() {
    let src = r#"<Project>
  <ItemGroup>
    <GlobalPackageReference Include="Some.Analyzer" Version="1.0" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert!(p.package_references_uncertain);
    assert_eq!(p.global_package_references.len(), 1);
    assert_eq!(p.global_package_references[0].id, "Some.Analyzer");
    assert_eq!(
        p.global_package_references[0].version.as_deref(),
        Some("1.0")
    );
}

#[test]
fn global_package_reference_item_list_inherits_helper_metadata() {
    let src = r#"<Project>
  <ItemGroup>
    <GlobalAnalyzer Include="Some.Analyzer" Version="1.0" PrivateAssets="all" />
  </ItemGroup>
  <ItemGroup>
    <GlobalPackageReference Include="@(GlobalAnalyzer)" IncludeAssets="compile" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert!(p.package_references_uncertain);
    assert_eq!(p.global_package_references.len(), 1);
    let global = &p.global_package_references[0];
    assert_eq!(global.id, "Some.Analyzer");
    assert_eq!(global.version.as_deref(), Some("1.0"));
    assert_eq!(global.private_assets.as_deref(), Some("all"));
    assert_eq!(global.include_assets.as_deref(), Some("compile"));
}

#[test]
fn manage_package_versions_centrally_marks_uncertain() {
    // The property-driven CPM opt-in (versions live in Directory.Packages.props
    // we don't yet fold in) → uncertain, even with no inline PackageVersion.
    let src = r#"<Project>
  <PropertyGroup>
    <ManagePackageVersionsCentrally>true</ManagePackageVersionsCentrally>
  </PropertyGroup>
  <ItemGroup>
    <PackageReference Include="Newtonsoft.Json" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert!(p.package_references_uncertain);
}

#[test]
fn manage_package_versions_centrally_false_is_not_uncertain() {
    // Explicitly opting out must not flag a bare, fully-versioned project.
    let src = r#"<Project>
  <PropertyGroup>
    <ManagePackageVersionsCentrally>false</ManagePackageVersionsCentrally>
  </PropertyGroup>
  <ItemGroup>
    <PackageReference Include="Newtonsoft.Json" Version="13.0.1" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert!(!p.package_references_uncertain);
}

#[test]
fn versionless_package_reference_is_uncertain() {
    // A versionless Include means the version is determined elsewhere (CPM,
    // ItemDefinitionGroup, SDK) or is missing — we don't hold it → uncertain.
    let src = r#"<Project>
  <ItemGroup>
    <PackageReference Include="X" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert!(p.package_references_uncertain);
    assert_eq!(
        p.package_reference_uncertainties[0].kind,
        PackageReferenceUncertaintyCauseKind::VersionlessPackageReference {
            id: "X".to_string()
        }
    );
    // Still captured (id known, version unknown).
    assert_eq!(only_package(&p).id, "X");
    assert_eq!(only_package(&p).version, None);
}

#[test]
fn versionless_include_flags_even_with_undetected_cpm_optin() {
    // The reviewer's case: a conditioned CPM opt-in we can't evaluate (so the
    // property never lands in the bag) plus a versionless ref. The symptom
    // (versionless Include) flags regardless of the undetected opt-in.
    let src = r#"<Project>
  <PropertyGroup Condition="Exists('nope.txt')">
    <ManagePackageVersionsCentrally>true</ManagePackageVersionsCentrally>
  </PropertyGroup>
  <ItemGroup>
    <PackageReference Include="X" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert!(p.package_references_uncertain);
}

#[test]
fn versionless_update_is_not_uncertain() {
    // A versionless Update is a metadata-only modification of an existing ref,
    // not a dependency declaration — it must not flag on the versionless rule.
    let src = r#"<Project>
  <ItemGroup>
    <PackageReference Update="X" PrivateAssets="all" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert!(!p.package_references_uncertain);
}

#[test]
fn false_conditioned_cpm_item_stays_certain() {
    // A *cleanly*-false Condition (a defined property) on a PackageVersion means
    // MSBuild excludes it, so it must not force a fallback for an otherwise
    // fully-explicit project. (An *undefined*-property condition is a different
    // case — unknown, not clean-false — covered below.)
    let src = r#"<Project>
  <PropertyGroup>
    <Foo>no</Foo>
  </PropertyGroup>
  <ItemGroup>
    <PackageVersion Include="X" Version="1.0" Condition="'$(Foo)' == 'yes'" />
  </ItemGroup>
  <ItemGroup>
    <PackageReference Include="Y" Version="2.0" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert!(
        !p.package_references_uncertain,
        "a cleanly-disabled CPM item must not flag"
    );
}

#[test]
fn inexact_property_conditioned_cpm_item_is_uncertain() {
    // A carved-out property in the condition (`TargetFramework` is never
    // provably unset) is unknown, not clean-false — MSBuild may include the
    // item if that property is supplied, so flag.
    let src = r#"<Project>
  <ItemGroup>
    <PackageVersion Include="X" Version="1.0" Condition="'$(TargetFramework)' == 'yes'" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert!(p.package_references_uncertain);
}

#[test]
fn cpm_optin_under_unevaluable_condition_is_uncertain() {
    // ManagePackageVersionsCentrally gated on an unsupported condition: we
    // can't tell if CPM turns on, which changes version interpretation for the
    // whole (here fully-versioned) set. Must flag rather than trust.
    let src = r#"<Project>
  <PropertyGroup Condition="Exists('maybe.props')">
    <ManagePackageVersionsCentrally>true</ManagePackageVersionsCentrally>
  </PropertyGroup>
  <ItemGroup>
    <PackageReference Include="X" Version="1.0" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert!(p.package_references_uncertain);
}

#[test]
fn cpm_optin_under_inexact_property_condition_is_uncertain() {
    // Carved-out `TargetFramework` is never provably unset, so whether the
    // opt-in group runs is unknown.
    let src = r#"<Project>
  <PropertyGroup Condition="'$(TargetFramework)' == 'true'">
    <ManagePackageVersionsCentrally>true</ManagePackageVersionsCentrally>
  </PropertyGroup>
  <ItemGroup>
    <PackageReference Include="X" Version="1.0" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert!(p.package_references_uncertain);
}

#[test]
fn cpm_optin_cleanly_false_condition_stays_certain() {
    // A defined property makes the opt-in genuinely off → the versioned set is
    // certain.
    let src = r#"<Project>
  <PropertyGroup>
    <UseCpm>no</UseCpm>
  </PropertyGroup>
  <PropertyGroup Condition="'$(UseCpm)' == 'yes'">
    <ManagePackageVersionsCentrally>true</ManagePackageVersionsCentrally>
  </PropertyGroup>
  <ItemGroup>
    <PackageReference Include="X" Version="1.0" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert!(!p.package_references_uncertain);
}

#[test]
fn later_write_after_false_cpm_property_group_gate_stays_certain() {
    // PropertyGroup conditions are evaluated in the property pass, in document
    // order. The later UseCpm write cannot retroactively make this false CPM
    // opt-in group run, so it must not be tracked as an item-pass read.
    let src = r#"<Project>
  <PropertyGroup>
    <UseCpm>no</UseCpm>
  </PropertyGroup>
  <PropertyGroup Condition="'$(UseCpm)' == 'yes'">
    <ManagePackageVersionsCentrally>true</ManagePackageVersionsCentrally>
  </PropertyGroup>
  <PropertyGroup>
    <UseCpm>yes</UseCpm>
  </PropertyGroup>
  <ItemGroup>
    <PackageReference Include="X" Version="1.0" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert!(
        !p.package_references_uncertain,
        "a later property write cannot change an already-false CPM PropertyGroup gate; causes: {:?}",
        p.package_reference_uncertainties
    );
}

#[test]
fn later_write_after_false_cpm_property_condition_stays_certain() {
    // Individual property-element Conditions are property-pass decisions too.
    // A later write cannot retroactively make this false CPM opt-in run, so
    // the condition read must not be tracked as an item-pass package read.
    let src = r#"<Project>
  <PropertyGroup>
    <UseCpm>no</UseCpm>
    <ManagePackageVersionsCentrally Condition="'$(UseCpm)' == 'yes'">true</ManagePackageVersionsCentrally>
  </PropertyGroup>
  <PropertyGroup>
    <UseCpm>yes</UseCpm>
  </PropertyGroup>
  <ItemGroup>
    <PackageReference Include="X" Version="1.0" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert!(
        !p.package_references_uncertain,
        "a later property write cannot change an already-false CPM property condition; causes: {:?}",
        p.package_reference_uncertainties
    );
}

#[test]
fn later_write_after_false_cpm_property_value_stays_certain() {
    // CPM flag values are property-pass reads too. The later UseCpm write
    // cannot retroactively change the value this property element assigned.
    let src = r#"<Project>
  <PropertyGroup>
    <UseCpm>false</UseCpm>
    <ManagePackageVersionsCentrally>$(UseCpm)</ManagePackageVersionsCentrally>
  </PropertyGroup>
  <PropertyGroup>
    <UseCpm>true</UseCpm>
  </PropertyGroup>
  <ItemGroup>
    <PackageReference Include="X" Version="1.0" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert!(
        !p.package_references_uncertain,
        "a later property write cannot change an already-false CPM property value; causes: {:?}",
        p.package_reference_uncertainties
    );
}

#[test]
fn global_package_reference_under_inexact_condition_is_uncertain() {
    // Finding: a GlobalPackageReference gated on a carved-out property
    // (`TargetFramework` is never provably unset) evaluates false and would
    // otherwise produce a trusted empty set — but MSBuild may include it if
    // the property is supplied by imports/globals.
    let src = r#"<Project>
  <ItemGroup>
    <GlobalPackageReference Include="Some.Analyzer" Version="1.0"
                            Condition="'$(TargetFramework)' == 'true'" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert!(p.package_references_uncertain);
}

#[test]
fn global_package_reference_cleanly_false_stays_certain() {
    let src = r#"<Project>
  <PropertyGroup>
    <EnableAnalyzers>no</EnableAnalyzers>
  </PropertyGroup>
  <ItemGroup>
    <GlobalPackageReference Include="Some.Analyzer" Version="1.0"
                            Condition="'$(EnableAnalyzers)' == 'true'" />
  </ItemGroup>
  <ItemGroup>
    <PackageReference Include="Y" Version="2.0" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert!(!p.package_references_uncertain);
}

// ---------------------------------------------------------------------------
// MSBuild pass ordering: properties finalise before ANY item evaluates.
//
// MSBuild evaluates in ordered passes — every `<PropertyGroup>` (across the
// project and all imports) runs before any `<ItemGroup>` is looked at. So an
// item's `Include`, `Condition`, and metadata all see the FINAL property
// table, regardless of where in the document the property was written.
// Expectations below were validated against `dotnet msbuild -getItem` (see
// also the differential pins in `tests/fsproj_packageref_diff.rs`).
// ---------------------------------------------------------------------------

#[test]
fn item_include_sees_property_defined_later_in_document() {
    let src = r#"<Project>
  <ItemGroup>
    <Compile Include="$(Src)" />
  </ItemGroup>
  <PropertyGroup>
    <Src>Program.fs</Src>
  </PropertyGroup>
</Project>"#;
    let p = parse(src);
    assert_eq!(paths(&p.items), [Path::new("/repo/proj/Program.fs")]);
    assert!(
        p.diagnostics.is_empty(),
        "forward reference is not a divergence: {:?}",
        p.diagnostics
    );
    assert!(!p.items_uncertain);
}

#[test]
fn item_group_condition_sees_property_defined_later_in_document() {
    let src = r#"<Project>
  <ItemGroup Condition="'$(UseExtra)' == 'true'">
    <Compile Include="Extra.fs" />
  </ItemGroup>
  <PropertyGroup>
    <UseExtra>true</UseExtra>
  </PropertyGroup>
</Project>"#;
    let p = parse(src);
    assert_eq!(paths(&p.items), [Path::new("/repo/proj/Extra.fs")]);
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    assert!(!p.items_uncertain);
}

#[test]
fn item_condition_sees_final_property_value_not_document_position_value() {
    // The sharpest pass-ordering case: at the ItemGroup's document position
    // the property reads "true", but the FINAL value is "false" — MSBuild
    // excludes the item (verified against `dotnet msbuild -getItem`). A
    // document-position evaluation would wrongly include it.
    let src = r#"<Project>
  <PropertyGroup>
    <Flag>true</Flag>
  </PropertyGroup>
  <ItemGroup Condition="'$(Flag)' == 'true'">
    <PackageReference Include="A" Version="1.0.0" />
  </ItemGroup>
  <PropertyGroup>
    <Flag>false</Flag>
  </PropertyGroup>
</Project>"#;
    let p = parse(src);
    assert!(p.package_references.is_empty());
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    assert!(!p.package_references_uncertain);
}

#[test]
fn package_reference_version_sees_property_defined_later_in_document() {
    let src = r#"<Project>
  <ItemGroup>
    <PackageReference Include="B" Version="$(BVer)" />
  </ItemGroup>
  <PropertyGroup>
    <BVer>2.1.0</BVer>
  </PropertyGroup>
</Project>"#;
    let p = parse(src);
    assert_eq!(p.package_references.len(), 1);
    assert_eq!(p.package_references[0].id, "B");
    assert_eq!(p.package_references[0].version.as_deref(), Some("2.1.0"));
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    assert!(!p.package_references_uncertain);
}

#[test]
fn compile_link_metadata_sees_property_defined_later_in_document() {
    let src = r#"<Project>
  <ItemGroup>
    <Compile Include="A.fs" Link="$(LinkDir)/A.fs" />
  </ItemGroup>
  <PropertyGroup>
    <LinkDir>shown</LinkDir>
  </PropertyGroup>
</Project>"#;
    let p = parse(src);
    assert_eq!(p.items.len(), 1);
    assert_eq!(p.items[0].link.as_deref(), Some("shown/A.fs"));
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
}

#[test]
fn inexact_property_in_item_include_still_diagnosed() {
    // Pass ordering must not swallow real divergence risks: a carved-out
    // property (`TargetFramework` is never provably unset, however late in
    // the document you look) still surfaces as UndefinedProperty and the
    // item is still refused.
    let src = r#"<Project>
  <ItemGroup>
    <Compile Include="$(TargetFramework)" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert!(p.items.is_empty());
    assert_eq!(
        diag_kinds(&p.diagnostics),
        [&DiagnosticKind::UndefinedProperty {
            name: "TargetFramework".to_string()
        }]
    );
    assert!(p.items_uncertain);
}

#[test]
fn items_still_evaluate_in_document_order_relative_to_each_other() {
    // Pass ordering reorders items relative to *properties*, never items
    // relative to *items*: the item pass runs in document order.
    let src = r#"<Project>
  <ItemGroup>
    <Compile Include="A.fs" />
  </ItemGroup>
  <PropertyGroup>
    <Mid>B</Mid>
  </PropertyGroup>
  <ItemGroup>
    <Compile Include="$(Mid).fs" />
    <Compile Include="C.fs" />
  </ItemGroup>
</Project>"#;
    let p = parse(src);
    assert_eq!(file_names(&p.items), ["A.fs", "B.fs", "C.fs"]);
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
}

/// Build a small project whose single `<PropertyGroup>` is placed either
/// before or after the `<ItemGroup>` that consumes the property. Pass
/// ordering makes the two placements semantically identical.
#[cfg(test)]
fn build_ordering_fsproj(property_first: bool, name: &str, value: &str, usage: &str) -> String {
    let property_group =
        format!("  <PropertyGroup>\n    <{name}>{value}</{name}>\n  </PropertyGroup>\n");
    let item_group = format!("  <ItemGroup>\n    {usage}\n  </ItemGroup>\n");
    let (first, second) = if property_first {
        (&property_group, &item_group)
    } else {
        (&item_group, &property_group)
    };
    format!("<Project>\n{first}{second}</Project>\n")
}

proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config {
        cases: 128,
        ..proptest::test_runner::Config::default()
    })]

    /// Order-invariance: because MSBuild finalises properties before
    /// evaluating any item, swapping a `<PropertyGroup>` past an
    /// `<ItemGroup>` never changes the evaluated result. (Property-vs-
    /// property and import-vs-property order DO matter and are exercised
    /// elsewhere; this property is deliberately scoped to item-vs-property
    /// placement.) Spans differ between the two layouts, so the assertion
    /// compares the semantic content, not the raw structs.
    #[test]
    fn item_evaluation_is_invariant_under_property_group_placement(
        value in "[A-Za-z][A-Za-z0-9]{0,8}",
        use_condition in proptest::bool::ANY,
    ) {
        let usage = if use_condition {
            format!(
                "<Compile Include=\"Cond.fs\" Condition=\"'$(P)' == '{value}'\" />"
            )
        } else {
            "<Compile Include=\"$(P).fs\" />".to_string()
        };
        let before = parse(&build_ordering_fsproj(true, "P", &value, &usage));
        let after = parse(&build_ordering_fsproj(false, "P", &value, &usage));
        let includes = |p: &ParsedProject| -> Vec<PathBuf> {
            p.items.iter().map(|i| i.include.clone()).collect()
        };
        proptest::prop_assert_eq!(includes(&before), includes(&after));
        proptest::prop_assert_eq!(before.items_uncertain, after.items_uncertain);
        proptest::prop_assert_eq!(
            diag_kinds(&before.diagnostics),
            diag_kinds(&after.diagnostics)
        );
        // And the property-first layout is the long-standing supported shape:
        // it must actually produce the item.
        proptest::prop_assert!(!before.items.is_empty());
    }
}

// --- The `OS` pseudo-environment property ---
//
// Real MSBuild fakes the `OS` environment variable on non-Windows hosts
// (dotnet/msbuild `src/Build/Evaluation/Evaluator.cs`, "Fake OS env
// variables when not on Windows": `SetBuiltInProperty(osName, "Unix")`;
// on Windows the genuine `OS=Windows_NT` env var flows through). Pinned
// against `dotnet msbuild` 10.0.301 on macOS, 2026-07-09, per-case stub
// projects:
//   * `-getProperty:OS` with nothing set prints `Unix`;
//   * a project `<OS>Custom</OS>` write overrides it (prints `Custom`);
//   * `-p:OS=Global` beats the project write (ordinary global-property
//     immutability).
// The F# compiler corpus gates on exactly this
// (`FSharp.Compiler.ComponentTests.fsproj`:
// `<TargetFrameworks Condition="'$(OS)' == 'Unix' or …">`), so leaving
// `OS` undefined mis-evaluates TargetFrameworks on every unix host.

#[test]
fn os_default_under_an_empty_environment_follows_the_host() {
    // MSBuild *synthesises* `OS` only on non-Windows (`Evaluator.cs`:
    // `if (!NativeMethodsShared.IsWindows)`). On Windows there is no built-in
    // at all — `OS=Windows_NT` arrives as an ordinary environment variable —
    // so under the *empty* snapshot this project is parsed with, the two hosts
    // must disagree: `Unix` on unix, undefined on Windows. Inventing
    // `Windows_NT` for an env-cleared Windows caller would commit a
    // Windows-only branch the real build never takes.
    let src = r#"<Project>
  <PropertyGroup>
    <R>$(OS)</R>
  </PropertyGroup>
</Project>"#;
    let p = parse(src);

    if cfg!(windows) {
        assert_eq!(p.properties.get("R").map(String::as_str), Some(""));
        assert!(
            p.diagnostics
                .iter()
                .any(|d| matches!(&d.kind, DiagnosticKind::UndefinedProperty { name } if name.eq_ignore_ascii_case("OS"))),
            "an unseeded OS must degrade, not commit: {:?}",
            p.diagnostics
        );
    } else {
        assert_eq!(p.properties.get("R").map(String::as_str), Some("Unix"));
        assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
        assert!(!p.is_partial);
    }
}

#[test]
fn os_promotes_from_the_environment_on_every_host() {
    // Wherever the variable *is* in the snapshot it is promoted (it is not
    // reserved), overwriting the non-Windows fake. Probed: `OS=Windows_NT` on
    // a unix host makes `$(OS)` read `Windows_NT`.
    let env = HashMap::from([("OS".to_string(), "Windows_NT".to_string())]);
    let p = parse_with_environment(
        r#"<Project>
  <PropertyGroup>
    <R>$(OS)</R>
  </PropertyGroup>
</Project>"#,
        &env,
    );
    assert_eq!(
        p.properties.get("R").map(String::as_str),
        Some("Windows_NT")
    );
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
}

#[test]
fn os_condition_selects_target_frameworks_branch() {
    // The corpus shape that motivated this: on unix the conditional
    // (single-TFM) write must beat the unconditional two-TFM one.
    let src = r#"<Project>
  <PropertyGroup>
    <TargetFrameworks>net472;net10.0</TargetFrameworks>
    <TargetFrameworks Condition="'$(OS)' == 'Unix' or '$(BUILDING_USING_DOTNET)' == 'true'">net10.0</TargetFrameworks>
  </PropertyGroup>
</Project>"#;
    let p = parse(src);
    let expected = if cfg!(windows) {
        "net472;net10.0"
    } else {
        "net10.0"
    };
    assert_eq!(
        p.properties.get("TargetFrameworks").map(String::as_str),
        Some(expected)
    );
}

#[test]
fn os_property_project_write_overrides_default() {
    // Pinned: a project `<OS>` write is legal and wins over the faked
    // environment default (`dotnet msbuild -getProperty:R` = `Custom`).
    let src = r#"<Project>
  <PropertyGroup>
    <OS>Custom</OS>
    <R>$(OS)</R>
  </PropertyGroup>
</Project>"#;
    let p = parse(src);
    assert_eq!(p.properties.get("R").map(String::as_str), Some("Custom"));
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
}

#[test]
fn os_property_global_wins_over_project_write() {
    // Pinned: `-p:OS=Global` stays `Global` even after the project's own
    // `<OS>` write — ordinary global-property immutability applies.
    let src = r#"<Project>
  <PropertyGroup>
    <OS>Custom</OS>
    <R1>$(OS)</R1>
  </PropertyGroup>
</Project>"#;
    let p = parse_with(src, &[("OS", "Global")]);
    assert_eq!(p.properties.get("R1").map(String::as_str), Some("Global"));
}

/// The `Microsoft.Common.props` `MSBuildProjectExtensionsPath` normalisation
/// block, structurally verbatim. This is the **keystone** of Stage C of
/// `docs/completed/sdk-chain-exactness-plan.md`: in the real SDK chain this is the first
/// place `State::walk_opaque` latches (the `exists('$(MSBuildProjectExtensionsPath)')`
/// import gate re-surfaces MSBPEP as unpinned), and once it latches every
/// downstream `undefined_read_is_exact` is blinded — the whole 270-cause
/// cascade in the ignored `sdk_style_*` fixtures (`TargetPlatformVersion`,
/// `PublishAot`, `RuntimeIdentifier`, `TargetsNet*`, …) is collateral of this
/// one decline. Forcing exact reads past the latch drops that fixture to 8
/// causes, so this block gates the rest.
///
/// The decline is the missing unix path fixup (`MaybeAdjustFilePath`,
/// `docs/msbuild-unix-path-fixup-plan.md`, stubbed at P0): `IsPathRooted` and
/// `Combine` bail on `has_unix_backslash` because MSBPEP still carries the
/// literal `\` from `BaseIntermediateOutputPath`'s `obj\` default. MSBuild runs
/// the fixup and resolves MSBPEP to `<projdir>/obj/` — and, crucially,
/// **cwd-independently**: oracle-pinned 2026-07-13 to `/…/obj/` whether or not
/// an `obj/` directory exists in the process cwd (the two fixup worlds collapse
/// to the same value once the piece is consumed as a path). So this case needs
/// no cwd knowledge to commit — it is the safe two-world-agreement subset the
/// plan's "conditions" arm targets.
// Non-Windows only: `MaybeAdjustFilePath` is inert on Windows and `Path.Combine`
// uses `\`, so the expected `/repo/proj/obj/` (and the whole fixup) is a unix
// fact. When this un-ignores it must not become a Windows-only failure.
#[cfg(not(windows))]
#[test]
fn msbuild_project_extensions_path_normalises_the_obj_default() {
    // `parse` seeds MSBuildProjectDirectory = /repo/proj from the project path.
    // The final `Marker` write is the load-bearing part of this regression. It
    // reads `$(MSBuildProjectExtensionsPath)` in a *downstream* condition that is
    // deliberately an **exact-value** comparison, not `!= ''`, so it distinguishes
    // the two fixup worlds. The test therefore fails on all three ways the
    // keystone can be got wrong:
    //   1. the normalisation *declines* (today's bug) — the read re-surfaces an
    //      `UnsupportedCondition` and `diagnostics` is non-empty;
    //   2. a fix computes the value but leaves MSBPEP *unpinned* — the read
    //      re-surfaces the unpinned root as a diagnostic;
    //   3. a fix *over-approximates* and retains both `…/obj/` and `…/obj\`
    //      worlds — `== '/repo/proj/obj/'` then disagrees across the worlds, so
    //      the gate cannot commit and `Marker` never resolves to `yes`.
    // Only a determinate, pinned MSBPEP (the real behaviour — see the oracle note
    // above) satisfies both assertions, which is exactly what keeps the real
    // `exists('$(MSBuildProjectExtensionsPath)')` import gate trusted.
    let src = r#"<Project>
  <PropertyGroup>
    <BaseIntermediateOutputPath Condition="'$(BaseIntermediateOutputPath)' == ''">obj\</BaseIntermediateOutputPath>
    <MSBuildProjectExtensionsPath Condition="'$(MSBuildProjectExtensionsPath)' == ''">$(BaseIntermediateOutputPath)</MSBuildProjectExtensionsPath>
    <MSBuildProjectExtensionsPath Condition="'$([System.IO.Path]::IsPathRooted($(MSBuildProjectExtensionsPath)))' == 'false'">$([System.IO.Path]::Combine('$(MSBuildProjectDirectory)', '$(MSBuildProjectExtensionsPath)'))</MSBuildProjectExtensionsPath>
    <MSBuildProjectExtensionsPath Condition="!HasTrailingSlash('$(MSBuildProjectExtensionsPath)')">$(MSBuildProjectExtensionsPath)\</MSBuildProjectExtensionsPath>
    <Marker Condition="'$(MSBuildProjectExtensionsPath)' == '/repo/proj/obj/'">yes</Marker>
  </PropertyGroup>
</Project>"#;
    let p = parse(src);
    assert!(
        p.diagnostics.is_empty(),
        "expected a clean, pinned normalisation, got: {:?}",
        p.diagnostics
    );
    // Oracle-pinned (`dotnet msbuild` 10.0.301, both cwds, 2026-07-13):
    // MSBuildProjectDirectory `/repo/proj` ⇒ `/repo/proj/obj/`, cwd-independent.
    // (This is the keystone's *specific* answer; the *general* collapse rule of
    // MaybeAdjustFilePath is not yet pinned — see `docs/msbuild-unix-path-fixup-plan.md`
    // P3, which makes the two-cwd oracle the prerequisite for the fix.)
    assert_eq!(
        p.properties
            .get("MSBuildProjectExtensionsPath")
            .map(String::as_str),
        Some("/repo/proj/obj/"),
    );
    // The exact-value gate commits ⇒ MSBPEP is a single determinate, trusted
    // world — the whole point of the keystone (see rationale above).
    assert_eq!(p.properties.get("Marker").map(String::as_str), Some("yes"));
    // NB `BaseIntermediateOutputPath` stays genuinely fixup-divergent
    // (`obj\` vs `obj/` by cwd) and is *not* asserted here: it is never consumed
    // as an exact string in this chain. Its property-table degradation needs a
    // *two-cwd* table differential that does not yet exist (today's
    // `fsproj_property_table_diff.rs` is single-cwd); building it is part of
    // milestone 1 — see `docs/msbuild-unix-path-fixup-plan.md` P3.
}

/// The leaf-boundary regressions codex found after E1 landed the escaped
/// domain, each pinned end to end. They are one rule with five faces: **scan and
/// split on escaped text, decode at the leaf** — decoding first turns data into
/// syntax, and decoding never turns syntax into data.
///
/// All oracle-pinned against `dotnet msbuild` 10.0.301 (2026-07-12).
mod escaped_leaf_boundaries {
    use super::*;

    /// A `;`-delimited list splits on the semicolons of the *escaped* value, so
    /// `A%3bB` is the single define `A;B` (oracle: `<X Include="$(D)"/>` with
    /// `<D>A%3bB</D>` is one item, `A;B`). Decoding first would make it two.
    #[test]
    fn an_escaped_semicolon_does_not_split_define_constants() {
        let src = r#"<Project>
  <PropertyGroup>
    <DefineConstants>A%3bB;C</DefineConstants>
  </PropertyGroup>
</Project>"#;
        let p = parse_with(src, &[]);
        assert_eq!(p.define_constants, vec!["A;B".to_string(), "C".to_string()]);
    }

    /// The same rule for `TargetFrameworks`: `net8.0%3bnet9.0` is one (bogus)
    /// framework whose name contains a semicolon, not two valid ones.
    #[test]
    fn an_escaped_semicolon_does_not_split_target_frameworks() {
        let src = r#"<Project>
  <PropertyGroup>
    <TargetFrameworks>net8.0%3bnet9.0</TargetFrameworks>
  </PropertyGroup>
</Project>"#;
        let p = parse_with(src, &[]);
        assert_eq!(
            crate::target_frameworks(&p),
            vec!["net8.0;net9.0".to_string()],
            "an escaped `;` is data, so this is one framework — not two"
        );
    }

    /// An item identity is decoded at its point of use, so `Foo%2eBar` is the
    /// package `Foo.Bar` — and a `Remove` naming it either way must match.
    #[test]
    fn a_dependency_identity_is_decoded() {
        let src = r#"<Project>
  <ItemGroup>
    <PackageReference Include="Foo%2eBar" Version="1.0.0" />
  </ItemGroup>
</Project>"#;
        let p = parse_with(src, &[]);
        let ids: Vec<&str> = p.package_references.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, vec!["Foo.Bar"]);
    }

    /// An `Exclude` is compared against identities that were decoded when their
    /// `Include` captured them, so it decodes too — otherwise it silently fails
    /// to exclude, leaving a phantom dependency behind.
    #[test]
    fn a_dependency_exclude_is_decoded_like_the_identity_it_names() {
        let src = r#"<Project>
  <ItemGroup>
    <PackageReference Include="Foo%2eBar;Keep" Exclude="Foo%2eBar" Version="1.0.0" />
  </ItemGroup>
</Project>"#;
        let p = parse_with(src, &[]);
        let ids: Vec<&str> = p.package_references.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, vec!["Keep"], "the Exclude must find what it names");
    }

    /// A compile-order `Update` target is compared against identities that were
    /// decoded when their `Include` captured them, so it decodes too. Escaped on
    /// one side and decoded on the other would silently match nothing — the
    /// wrong compile order, with no diagnostic.
    #[test]
    fn a_compile_order_update_target_matches_its_escaped_include() {
        let src = r#"<Project>
  <ItemGroup>
    <Compile Include="a%20b.fs" />
    <Compile Include="z.fs" />
    <Compile Update="a%20b.fs" CompileOrder="CompileLast" />
  </ItemGroup>
</Project>"#;
        let p = parse_with(src, &[]);
        assert_eq!(
            file_names(&p.items),
            vec!["z.fs", "a b.fs"],
            "the Update must find the item its escape names"
        );
    }
}
