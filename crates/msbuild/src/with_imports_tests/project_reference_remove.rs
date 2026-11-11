//! `<ProjectReference Remove="…">`: the item operation that strips a reference
//! from the evaluated list, modelled exactly where its item spec is.
//!
//! The motivating idiom drops references whose project file is missing:
//!
//! ```xml
//! <ProjectReferenceNonExistent Include="@(ProjectReference)"
//!                              Exclude="@(ProjectReference->Exists())" />
//! <ProjectReference Remove="@(ProjectReferenceNonExistent)" />
//! ```
//!
//! Every expectation here was pinned against `dotnet msbuild -getItem` on a
//! scratch layout (2026-11-11); `tests/fsproj_project_reference_remove_diff.rs`
//! holds the generated space to the real evaluator.

use super::*;
use tempfile::TempDir;

/// A scratch layout: `A/A.fsproj` and `B/B.fsproj` exist, `M/M.fsproj` does
/// not, and the project under test is `proj/Demo.fsproj` with `body` inside
/// `<Project>`.
fn layout(body: &str) -> (TempDir, PathBuf, ParsedProject) {
    let tmp = TempDir::new().unwrap();
    write_at(tmp.path(), "A/A.fsproj", "<Project />");
    write_at(tmp.path(), "B/B.fsproj", "<Project />");
    let source = format!("<Project>\n{body}\n</Project>");
    let project_path = write_at(tmp.path(), "proj/Demo.fsproj", &source);
    let parsed = parse_file(&project_path);
    let root = canon(tmp.path());
    (tmp, root, parsed)
}

/// The captured reference list, lexically normalised (MSBuild's `FullPath`
/// collapses `.`/`..`; ours keeps the joined spelling).
fn references(parsed: &ParsedProject) -> Vec<PathBuf> {
    parsed
        .project_references
        .iter()
        .map(|item| {
            let mut out = PathBuf::new();
            for component in item.include.components() {
                match component {
                    std::path::Component::ParentDir => {
                        out.pop();
                    }
                    std::path::Component::CurDir => {}
                    other => out.push(other),
                }
            }
            out
        })
        .collect()
}

const NON_EXISTENT_IDIOM: &str = r#"  <ItemGroup Condition="'$(DeclaredReferencesOnly)' != 'true'">
    <ProjectReferenceNonExistent Include="@(ProjectReference)" Exclude="@(ProjectReference->Exists())" />
    <ProjectReference Remove="@(ProjectReferenceNonExistent)" />
  </ItemGroup>"#;

#[test]
fn the_non_existent_idiom_is_a_no_op_when_every_reference_exists() {
    let (_tmp, root, parsed) = layout(&format!(
        r#"  <ItemGroup>
    <ProjectReference Include="../A/A.fsproj" />
    <ProjectReference Include="../B/B.fsproj" />
  </ItemGroup>
{NON_EXISTENT_IDIOM}"#
    ));
    assert!(
        !parsed.project_references_uncertain,
        "a complete checkout makes the idiom a no-op we can model; diags: {:?}",
        parsed.diagnostics
    );
    assert_eq!(
        references(&parsed),
        vec![root.join("A/A.fsproj"), root.join("B/B.fsproj")]
    );
}

#[test]
fn the_non_existent_idiom_removes_exactly_the_missing_reference() {
    let (_tmp, root, parsed) = layout(&format!(
        r#"  <ItemGroup>
    <ProjectReference Include="../A/A.fsproj" />
    <ProjectReference Include="../M/M.fsproj" />
    <ProjectReference Include="../B/B.fsproj" />
  </ItemGroup>
{NON_EXISTENT_IDIOM}"#
    ));
    assert!(
        !parsed.project_references_uncertain,
        "diags: {:?}",
        parsed.diagnostics
    );
    assert_eq!(
        references(&parsed),
        vec![root.join("A/A.fsproj"), root.join("B/B.fsproj")]
    );
}

#[test]
fn a_reference_included_after_the_remove_is_kept() {
    // Item operations are ordered: the Remove sees only the list so far.
    let (_tmp, root, parsed) = layout(&format!(
        r#"  <ItemGroup>
    <ProjectReference Include="../A/A.fsproj" />
  </ItemGroup>
{NON_EXISTENT_IDIOM}
  <ItemGroup>
    <ProjectReference Include="../M/M.fsproj" />
  </ItemGroup>"#
    ));
    assert!(!parsed.project_references_uncertain);
    assert_eq!(
        references(&parsed),
        vec![root.join("A/A.fsproj"), root.join("M/M.fsproj")]
    );
}

#[test]
fn a_literal_remove_matches_by_normalised_path() {
    // MSBuild compares the full paths each spelling names, ignoring case.
    let (_tmp, root, parsed) = layout(
        r#"  <ItemGroup>
    <ProjectReference Include="../A/A.fsproj" />
    <ProjectReference Include="../B/B.fsproj" />
    <ProjectReference Remove="./../X/../a/./A.FSPROJ" />
  </ItemGroup>"#,
    );
    assert!(
        !parsed.project_references_uncertain,
        "diags: {:?}",
        parsed.diagnostics
    );
    assert_eq!(references(&parsed), vec![root.join("B/B.fsproj")]);
}

#[test]
fn a_bare_self_reference_remove_clears_the_list() {
    let (_tmp, _root, parsed) = layout(
        r#"  <ItemGroup>
    <ProjectReference Include="../A/A.fsproj" />
    <ProjectReference Include="../M/M.fsproj" />
    <ProjectReference Remove="@(ProjectReference)" />
  </ItemGroup>"#,
    );
    assert!(!parsed.project_references_uncertain);
    assert!(parsed.project_references.is_empty());
}

#[test]
fn an_exists_filter_can_drive_the_remove_directly() {
    let (_tmp, root, parsed) = layout(
        r#"  <ItemGroup>
    <ProjectReference Include="../A/A.fsproj" />
    <ProjectReference Include="../M/M.fsproj" />
    <ProjectReference Remove="@(ProjectReference->Exists())" />
  </ItemGroup>"#,
    );
    assert!(!parsed.project_references_uncertain);
    assert_eq!(references(&parsed), vec![root.join("M/M.fsproj")]);
}

#[test]
fn a_hidden_include_makes_the_derived_helper_inexact() {
    // A wildcard Include we cannot expand (no glob resolver) may contribute a
    // missing project MSBuild then removes — along with any item whose path it
    // matches. The helper derived from the list is incomplete, so the Remove
    // cannot be trusted.
    let (_tmp, _root, parsed) = layout(&format!(
        r#"  <ItemGroup>
    <ProjectReference Include="../A/A.fsproj" />
    <ProjectReference Include="../*/Nothing.fsproj" />
  </ItemGroup>
{NON_EXISTENT_IDIOM}"#
    ));
    assert!(parsed.project_references_uncertain);
}

#[test]
fn a_backslash_spelling_declines_the_exists_probe() {
    // MSBuild rewrites backslashes in an evaluated value only when the first
    // path segment exists relative to its working directory, so which path
    // `->Exists()` probes is environment-dependent. We decline.
    let (_tmp, _root, parsed) = layout(&format!(
        r#"  <ItemGroup>
    <ProjectReference Include="..\A\A.fsproj" />
  </ItemGroup>
{NON_EXISTENT_IDIOM}"#
    ));
    assert!(parsed.project_references_uncertain);
}

#[test]
fn a_helper_in_an_undecided_choose_branch_cannot_be_read_as_empty() {
    // The branch may run in the real build and fill the helper, so a Remove of
    // it may strip a reference we would otherwise keep.
    let (_tmp, _root, parsed) = layout(
        r#"  <Choose>
    <When Condition="'$([System.Guid]::NewGuid())' == ''">
      <ItemGroup>
        <Dropped Include="../A/A.fsproj" />
      </ItemGroup>
    </When>
  </Choose>
  <ItemGroup>
    <ProjectReference Include="../A/A.fsproj" />
    <ProjectReference Remove="@(Dropped)" />
  </ItemGroup>"#,
    );
    assert!(parsed.project_references_uncertain);
}

#[test]
fn a_remove_with_match_on_metadata_declines() {
    let (_tmp, _root, parsed) = layout(
        r#"  <ItemGroup>
    <ProjectReference Include="../A/A.fsproj" Tag="x" />
    <Helper Include="h" Tag="x" />
    <ProjectReference Remove="@(Helper)" MatchOnMetadata="Tag" />
  </ItemGroup>"#,
    );
    assert!(parsed.project_references_uncertain);
}

#[test]
fn an_include_the_walk_may_have_skipped_makes_a_derived_remove_decline() {
    // `KeepC` is written under a gate we cannot evaluate, so a gate reading it
    // is untrusted: here it reads false, and the real build may include
    // `../C/C.fsproj`. A missed reference is allowed — but a `Remove` driven by
    // `@(ProjectReference)` then works from an incomplete list.
    let prelude = r#"  <PropertyGroup Condition="'$([System.Guid]::NewGuid())' != ''">
    <KeepC>true</KeepC>
  </PropertyGroup>"#;
    for hidden in [
        // Element-level untrusted false.
        r#"<ItemGroup>
    <ProjectReference Include="../C/C.fsproj" Condition="'$(KeepC)' == 'true'" />
  </ItemGroup>"#,
        // Group-level untrusted false.
        r#"<ItemGroup Condition="'$(KeepC)' == 'true'">
    <ProjectReference Include="../C/C.fsproj" />
  </ItemGroup>"#,
        // Group-level gate outside the grammar.
        r#"<ItemGroup Condition="'$([System.Guid]::NewGuid())' != ''">
    <ProjectReference Include="../C/C.fsproj" />
  </ItemGroup>"#,
    ] {
        let without_remove = layout(&format!(
            r#"{prelude}
  <ItemGroup>
    <ProjectReference Include="../A/A.fsproj" />
  </ItemGroup>
  {hidden}"#
        ))
        .2;
        assert!(
            !without_remove.project_references_uncertain,
            "{hidden}: a skipped Include alone is a missed reference, not uncertainty"
        );
        let (_tmp, _root, parsed) = layout(&format!(
            r#"{prelude}
  <ItemGroup>
    <ProjectReference Include="../A/A.fsproj" />
  </ItemGroup>
  {hidden}
  <ItemGroup>
    <ProjectReference Remove="@(ProjectReference->Exists())" />
  </ItemGroup>"#
        ));
        assert!(
            parsed.project_references_uncertain,
            "{hidden}: the Remove reads a list that may be missing an item"
        );
    }
}
