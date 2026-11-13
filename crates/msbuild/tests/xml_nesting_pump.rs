//! Every project file the evaluator reads is held to bounded recursion: no
//! element path, repeated thousands of levels deep, may overflow the stack.
//!
//! A `.fsproj`, a `Directory.Build.*` file, or a `.props`/`.targets` shipped in
//! a NuGet package's `build/` folder is input the user need not control, and a
//! stack overflow aborts the LSP outright. `roxmltree` parses iteratively; the
//! hazard is a walk of ours that recurses per element along some path of the
//! MSBuild grammar. [`borzoi_oracle_harness::xml_pump`] repeats every short
//! path of the inputs below, so a recursion through any of them is found
//! without this file naming it. The inputs between them hold every element of
//! the MSBuild project schema.
//!
//! A failure aborts this test binary; the last `pump:` line on stderr names the
//! path that overflowed.

use borzoi_msbuild::observed::host_fs::HostPath;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use borzoi_msbuild::{parse_fsproj, parse_fsproj_with_imports};
use borzoi_oracle_harness::xml_pump::{on_pump_stack, pumps};

/// Every element of the MSBuild project schema, in every position it may take.
const EVERY_CONSTRUCT: &str = r#"<Project Sdk="Microsoft.NET.Sdk" DefaultTargets="Build" InitialTargets="Init" TreatAsLocalProperty="P">
  <Sdk Name="Other.Sdk" Version="1.0.0" />
  <PropertyGroup Condition="'$(A)' == ''" Label="L">
    <P Condition="true">x$(P)<!-- c --><![CDATA[y]]></P>
    <Body><Inner a="b"><Deeper>t</Deeper></Inner></Body>
  </PropertyGroup>
  <ItemDefinitionGroup>
    <Compile><Visible>false</Visible></Compile>
  </ItemDefinitionGroup>
  <ItemGroup Condition="true">
    <Compile Include="a.fs" Condition="true"><Link>b.fs</Link><Meta><Nested>n</Nested></Meta></Compile>
    <Compile Remove="c.fs" />
    <Compile Update="a.fs" Link="d.fs" />
    <ProjectReference Include="../q/q.fsproj"><Private>false</Private></ProjectReference>
    <PackageReference Include="Pkg" Version="1.0.0"><PrivateAssets>all</PrivateAssets></PackageReference>
    <PackageVersion Include="Pkg" Version="1.0.0" />
  </ItemGroup>
  <Choose>
    <When Condition="'$(A)' == 'x'">
      <PropertyGroup><Q>w</Q></PropertyGroup>
      <ItemGroup><Compile Include="w.fs" /></ItemGroup>
      <Choose>
        <When Condition="'$(B)' == ''"><PropertyGroup><R>w</R></PropertyGroup></When>
        <Otherwise><ItemGroup><Compile Include="o.fs" /></ItemGroup></Otherwise>
      </Choose>
    </When>
    <Otherwise>
      <Choose>
        <When Condition="true"><PropertyGroup><S>s</S></PropertyGroup></When>
      </Choose>
      <PropertyGroup><Q>o</Q></PropertyGroup>
    </Otherwise>
  </Choose>
  <Import Project="missing.props" Condition="Exists('missing.props')" />
  <Import Project="Sdk.targets" Sdk="Microsoft.NET.Sdk" />
  <ImportGroup Condition="true" Label="G">
    <Import Project="also-missing.props" />
  </ImportGroup>
  <UsingTask TaskName="T" AssemblyFile="t.dll" TaskFactory="F">
    <ParameterGroup><Input ParameterType="System.String" Required="true" /></ParameterGroup>
    <Task><Code Type="Fragment" Language="cs"><![CDATA[ ]]></Code></Task>
  </UsingTask>
  <Target Name="Init" BeforeTargets="Build" DependsOnTargets="X" Condition="true">
    <PropertyGroup><TP>t</TP></PropertyGroup>
    <ItemGroup><TI Include="t" /></ItemGroup>
    <Message Text="m"><Output TaskParameter="Text" PropertyName="O" /></Message>
    <CallTarget Targets="Y" />
    <OnError ExecuteTargets="Z" />
  </Target>
  <ProjectExtensions><VisualStudio><Any><Xml>here</Xml></Any></VisualStudio></ProjectExtensions>
</Project>
"#;

/// How many levels each pump adds, and the most text one may repeat. At ~200
/// bytes of stack per level a 2 MiB stack overflows by 10,000 levels, and an
/// unoptimised frame is far larger than that.
const LEVELS: usize = 10_000;
const BYTE_BUDGET: usize = 512 << 10;

/// The real project files vendored from the F# tree.
fn corpus_projects() -> Vec<String> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fsproj-corpus");
    let mut out = Vec::new();
    for entry in
        borzoi_msbuild::observed::host_fs::read_dir(&root).expect("the fsproj fixture corpus")
    {
        let dir = entry.expect("a corpus entry").path();
        if !dir.host_is_dir() {
            continue;
        }
        for file in
            borzoi_msbuild::observed::host_fs::read_dir(&dir).expect("a corpus project directory")
        {
            let file = file.expect("a corpus file").path();
            if file.extension().is_some_and(|e| e == "fsproj") {
                out.push(
                    borzoi_msbuild::observed::host_fs::read_to_string(&file)
                        .expect("a corpus project"),
                );
            }
        }
    }
    assert!(out.len() >= 4, "the vendored corpus holds four projects");
    out
}

/// Evaluate `text` as the project, returning its evaluated `Inner`, if any.
fn evaluate_as_project(text: String) -> Option<String> {
    parse_fsproj(
        &text,
        Path::new("/nonexistent/Pumped.fsproj"),
        &HashMap::new(),
        &HashMap::new(),
    )
    .ok()?
    .properties
    .remove("Inner")
}

/// The same document read as an `<Import>`, which takes the evaluator's other
/// route into an XML file.
fn evaluate_as_import(dir: PathBuf, text: String) -> Option<String> {
    let props = dir.join("Pumped.props");
    std::fs::write(&props, text).expect("write the pumped import");
    let project = dir.join("Host.fsproj");
    let host = r#"<Project><Import Project="Pumped.props" /></Project>"#;
    parse_fsproj_with_imports(host, &project, &HashMap::new(), &HashMap::new(), None, None)
        .ok()?
        .properties
        .remove("Inner")
}

/// The ceiling that bounds the recursion through `<Choose>` only helps if the
/// deepest nesting MSBuild loads fits the stack: 50 levels, every gate taken
/// (`fsproj_property_table_diff` pins 50 as MSBuild's ceiling), evaluated all
/// the way in on both routes.
#[test]
fn the_deepest_choose_msbuild_loads_evaluates_on_the_pump_stack() {
    let depth = 50;
    let text = format!(
        "<Project>{}<PropertyGroup><Inner>reached</Inner></PropertyGroup>{}</Project>",
        "<Choose><When Condition=\"'$(Unset)' == ''\">".repeat(depth),
        "</When></Choose>".repeat(depth),
    );
    let tmp = tempfile::tempdir().expect("a temp dir");
    let dir = tmp.path().host_canonicalize().expect("canonical temp dir");
    let as_project = on_pump_stack("deepest Choose as the project", {
        let text = text.clone();
        move || evaluate_as_project(text)
    });
    assert_eq!(as_project.as_deref(), Some("reached"));
    let as_import = on_pump_stack("deepest Choose as an import", move || {
        evaluate_as_import(dir, text)
    });
    assert_eq!(as_import.as_deref(), Some("reached"));
}

#[test]
fn no_pumped_path_overflows_the_evaluator() {
    let tmp = tempfile::tempdir().expect("a temp dir");
    let dir = tmp.path().host_canonicalize().expect("canonical temp dir");
    let mut inputs = vec![EVERY_CONSTRUCT.to_owned()];
    inputs.extend(corpus_projects());
    let mut paths = Vec::new();
    for input in &inputs {
        for pump in pumps(input, 3, LEVELS, BYTE_BUDGET) {
            let label = format!("{} x{}", pump.path, pump.repeats);
            let text = pump.text;
            on_pump_stack(&format!("project {label}"), {
                let text = text.clone();
                move || evaluate_as_project(text)
            });
            on_pump_stack(&format!("import {label}"), {
                let dir = dir.clone();
                move || evaluate_as_import(dir, text)
            });
            paths.push(pump.path);
        }
    }
    eprintln!("{} paths pumped", paths.len());
    // The pumps reach the one recursive path of the grammar, and plenty else.
    assert!(
        paths.iter().any(|p| p == "Choose/When/Choose"),
        "the inputs must nest a Choose in a When: {paths:?}"
    );
    assert!(paths.len() > 100, "only {} paths pumped", paths.len());
}
