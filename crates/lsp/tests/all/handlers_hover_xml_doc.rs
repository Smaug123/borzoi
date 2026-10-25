//! End-to-end: hover on a referenced-assembly symbol shows its XML
//! documentation, read from the `.xml` beside the DLL the project's assembly
//! env actually read.
//!
//! Each test stages a "restored" project whose env carries a real
//! `System.Runtime.dll` (from the SDK's reference pack) and, where needed, a
//! real `FSharp.Core.dll` as a NuGet package — each with the `.xml` the test
//! chooses to put beside it: the shipped one, a doctored one, or none. That the
//! `.xml` is the *only* thing varied is what makes "no doc shown" mean "this
//! lookup found nothing" rather than "the symbol did not resolve": every
//! negative case first checks the signature hover is still there.

use borzoi::uri::UriExt as _;
use std::fs;
use std::path::{Path, PathBuf};

use crate::common::ensure_system_runtime_dll;
use borzoi::handlers::hover;
use borzoi::sdk_discovery::SdkDiscoveryEnv;
use borzoi::server::State;
use borzoi::workspace::Workspace;
use lsp_types::{
    HoverContents, HoverParams, Position, TextDocumentIdentifier, TextDocumentPositionParams, Uri,
    WorkDoneProgressParams,
};
use tempfile::TempDir;

/// The separator hover puts between a symbol's signature and its documentation.
const DOC_SEPARATOR: &str = "\n\n---\n\n";

/// What to put beside a staged DLL.
enum Xml {
    /// The `.xml` shipped beside the real DLL.
    Shipped,
    /// No `.xml` at all.
    Absent,
    /// This text.
    Text(String),
}

/// A pair of a real DLL and its shipped `.xml`.
struct Shipped {
    dll: PathBuf,
    xml: PathBuf,
}

fn system_runtime() -> Shipped {
    let dll = ensure_system_runtime_dll();
    let xml = dll.with_extension("xml");
    assert!(xml.is_file(), "the reference pack ships {}", xml.display());
    Shipped { dll, xml }
}

/// The SDK's own `FSharp.Core.dll` + `FSharp.Core.xml` (a matched pair: the
/// fcs-dump build output carries the DLL but not the XML).
fn sdk_fsharp_core() -> Shipped {
    let dotnet_root = std::env::var_os("DOTNET_ROOT")
        .map(PathBuf::from)
        .expect("DOTNET_ROOT unset (run under `nix develop`)");
    let sdk = dotnet_root.join("sdk");
    let mut pairs: Vec<Shipped> = fs::read_dir(&sdk)
        .unwrap_or_else(|e| panic!("read {}: {e}", sdk.display()))
        .filter_map(Result::ok)
        .map(|e| {
            let fsharp = e.path().join("FSharp");
            Shipped {
                dll: fsharp.join("FSharp.Core.dll"),
                xml: fsharp.join("FSharp.Core.xml"),
            }
        })
        .filter(|p| p.dll.is_file() && p.xml.is_file())
        .collect();
    pairs.sort_by(|a, b| a.dll.cmp(&b.dll));
    pairs.pop().unwrap_or_else(|| {
        panic!(
            "no SDK under {} ships FSharp.Core.dll + .xml",
            sdk.display()
        )
    })
}

fn stage(dll: &Shipped, into: &Path, xml: &Xml) {
    fs::create_dir_all(into).unwrap();
    let name = dll.dll.file_name().unwrap();
    fs::copy(&dll.dll, into.join(name)).unwrap();
    let target = into.join(name).with_extension("xml");
    match xml {
        Xml::Shipped => {
            fs::copy(&dll.xml, &target).unwrap();
        }
        Xml::Absent => {}
        Xml::Text(text) => fs::write(&target, text).unwrap(),
    }
}

/// A staged project: its `State`, the source file's URI, and the paths of the
/// two `.xml` slots (whether or not a file was put there).
struct Project {
    state: State,
    uri: Uri,
    runtime_xml: PathBuf,
    _tmp: TempDir,
}

fn project(src: &str, runtime_xml: Xml, fsharp_core_xml: Option<Xml>) -> Project {
    let packages = match fsharp_core_xml {
        Some(xml) => vec![Package {
            id: "FSharp.Core",
            dll: sdk_fsharp_core(),
            xml,
        }],
        None => Vec::new(),
    };
    project_with(src, runtime_xml, packages)
}

/// A DLL staged as a NuGet package's `lib/netstandard2.1` compile asset.
struct Package {
    id: &'static str,
    dll: Shipped,
    xml: Xml,
}

fn project_with(src: &str, runtime_xml: Xml, packages: Vec<Package>) -> Project {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let dotnet_root = root.join("dotnet");
    let pack = dotnet_root.join("packs/Microsoft.NETCore.App.Ref/10.0.0/ref/net10.0");
    stage(&system_runtime(), &pack, &runtime_xml);

    let pkgs = root.join("pkgs");
    fs::create_dir_all(&pkgs).unwrap();
    let mut target = serde_json::Map::new();
    let mut libraries = serde_json::Map::new();
    for package in &packages {
        let folder = format!("{}/10.0.0", package.id.to_lowercase());
        stage(
            &package.dll,
            &pkgs.join(&folder).join("lib/netstandard2.1"),
            &package.xml,
        );
        let file = package.dll.dll.file_name().unwrap().to_str().unwrap();
        let key = format!("{}/10.0.0", package.id);
        target.insert(
            key.clone(),
            serde_json::json!({
                "type": "package",
                "compile": { format!("lib/netstandard2.1/{file}"): {} }
            }),
        );
        libraries.insert(
            key,
            serde_json::json!({ "type": "package", "path": folder }),
        );
    }
    let targets = serde_json::json!({ "net10.0": target });
    let libraries = serde_json::Value::Object(libraries);
    let assets = serde_json::json!({
        "version": 3,
        "targets": targets,
        "libraries": libraries,
        "packageFolders": { pkgs.to_str().unwrap(): {} },
        "project": { "frameworks": { "net10.0": {
            "frameworkReferences": { "Microsoft.NETCore.App": {} }
        } } }
    });
    fs::create_dir_all(root.join("obj")).unwrap();
    fs::write(root.join("obj/project.assets.json"), assets.to_string()).unwrap();
    fs::write(
        root.join("P.fsproj"),
        r#"<Project><ItemGroup><Compile Include="Lib.fs" /></ItemGroup></Project>"#,
    )
    .unwrap();
    let src_path = root.join("Lib.fs");
    fs::write(&src_path, src).unwrap();

    let mut state = State::default();
    state.workspace = Workspace::with_env(SdkDiscoveryEnv {
        dotnet_root: Some(dotnet_root),
        ..SdkDiscoveryEnv::default()
    });
    let uri = Uri::from_file_path(&src_path).unwrap();
    state.docs.insert(uri.clone(), src.to_string());
    Project {
        state,
        uri,
        runtime_xml: pack.join("System.Runtime.xml"),
        _tmp: tmp,
    }
}

/// The hover body at the first occurrence of `needle` on `line` (cursor one
/// character into it).
fn hover_at(p: &mut Project, line: u32, needle: &str) -> String {
    let text = p.state.docs[&p.uri].clone();
    let line_text = text.lines().nth(line as usize).expect("line exists");
    let col = line_text.find(needle).expect("needle on line") as u32 + 1;
    let params = HoverParams {
        text_document_position_params: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier { uri: p.uri.clone() },
            position: Position {
                line,
                character: col,
            },
        },
        work_done_progress_params: WorkDoneProgressParams::default(),
    };
    let hover = hover::handle(&mut p.state, params).expect("a hover");
    match hover.contents {
        HoverContents::Markup(m) => m.value,
        other => panic!("expected markup, got {other:?}"),
    }
}

/// Split a body into its signature part and its documentation (if any).
fn split(body: &str) -> (&str, Option<&str>) {
    match body.split_once(DOC_SEPARATOR) {
        Some((signature, doc)) => (signature, Some(doc)),
        None => (body, None),
    }
}

const IS_NULL_OR_EMPTY: &str = "module M\nlet b = System.String.IsNullOrEmpty \"x\"\n";

#[test]
fn a_bcl_method_shows_its_whole_doc() {
    let mut p = project(IS_NULL_OR_EMPTY, Xml::Shipped, None);
    let body = hover_at(&mut p, 1, "IsNullOrEmpty");
    let (signature, doc) = split(&body);
    assert!(
        signature.contains("IsNullOrEmpty"),
        "signature first:\n{body}"
    );
    assert_eq!(
        doc,
        Some(
            "Indicates whether the specified string is `null` or an empty string (\"\").\n\n\
             **Parameters**\n\n\
             - `value` — The string to test.\n\n\
             **Returns**: `true` if the `value` parameter is `null` or an empty string (\"\"); \
             otherwise, `false`."
        ),
        "full body:\n{body}"
    );
}

#[test]
fn a_bcl_type_shows_its_doc() {
    let mut p = project(IS_NULL_OR_EMPTY, Xml::Shipped, None);
    let body = hover_at(&mut p, 1, "String");
    let (signature, doc) = split(&body);
    assert!(signature.contains("String"), "signature first:\n{body}");
    assert_eq!(
        doc,
        Some("Represents text as a sequence of UTF-16 code units."),
        "full body:\n{body}"
    );
}

/// An F# module is a type, keyed by its compiled name in either compiler's
/// dialect: FSharp.Core's `List` shows its documentation.
#[test]
fn an_fsharp_core_module_shows_its_doc() {
    let src = "module M\nlet ys = List.map (fun x -> x + 1) [1]\n";
    let mut p = project(src, Xml::Absent, Some(Xml::Shipped));
    let body = hover_at(&mut p, 1, "List");
    let (signature, doc) = split(&body);
    assert!(signature.contains("List"), "signature first:\n{body}");
    let doc = doc.unwrap_or_else(|| panic!("the List module is documented:\n{body}"));
    assert!(
        doc.starts_with("Contains operations for working with values of type"),
        "{doc}"
    );
}

/// An F# function's documentation is keyed by its F# signature, which the
/// IL-derived key need not match (see
/// [`an_fsharp_member_never_shows_another_members_doc`]), so FSharp.Core's
/// members show none — though `List.map` has an entry.
#[test]
fn an_fsharp_core_function_shows_no_doc() {
    let shipped = fs::read_to_string(sdk_fsharp_core().xml).unwrap();
    assert!(shipped.contains(r#"<member name="M:Microsoft.FSharp.Collections.ListModule.Map``2("#));
    let src = "module M\nlet ys = List.map (fun x -> x + 1) [1]\n";
    let mut p = project(src, Xml::Absent, Some(Xml::Shipped));
    let body = hover_at(&mut p, 1, "map");
    let (signature, doc) = split(&body);
    assert!(signature.contains("map"), "signature first:\n{body}");
    assert_eq!(doc, None, "{body}");
}

/// The `.xml` is read, but carries no entry for this symbol: no doc, and the
/// signature still stands.
#[test]
fn a_member_with_no_entry_shows_no_doc() {
    let shipped = fs::read_to_string(system_runtime().xml).unwrap();
    let entry = "<member name=\"M:System.String.IsNullOrEmpty(System.String)\">";
    let start = shipped.find(entry).expect("shipped entry");
    let end = start + shipped[start..].find("</member>").unwrap() + "</member>".len();
    let doctored = format!("{}{}", &shipped[..start], &shipped[end..]);
    let mut p = project(IS_NULL_OR_EMPTY, Xml::Text(doctored), None);

    let body = hover_at(&mut p, 1, "IsNullOrEmpty");
    let (signature, doc) = split(&body);
    assert!(signature.contains("IsNullOrEmpty"), "{body}");
    assert_eq!(doc, None, "{body}");
    // The rest of the file was read: the type's doc is still there.
    assert!(split(&hover_at(&mut p, 1, "String")).1.is_some());
}

#[test]
fn no_xml_file_shows_no_doc() {
    let mut p = project(IS_NULL_OR_EMPTY, Xml::Absent, None);
    let body = hover_at(&mut p, 1, "IsNullOrEmpty");
    let (signature, doc) = split(&body);
    assert!(signature.contains("IsNullOrEmpty"), "{body}");
    assert_eq!(doc, None, "{body}");
}

/// A key documented twice, differently: either could be the right one, so
/// neither is shown.
#[test]
fn an_ambiguous_entry_shows_no_doc() {
    let xml = r#"<doc><assembly><name>System.Runtime</name></assembly><members>
        <member name="T:System.String"><summary>One.</summary></member>
        <member name="T:System.String"><summary>Two.</summary></member>
        </members></doc>"#;
    let mut p = project(IS_NULL_OR_EMPTY, Xml::Text(xml.to_string()), None);
    assert_eq!(split(&hover_at(&mut p, 1, "String")).1, None);
}

/// A file that does not parse yields no docs at all — not the entries before
/// the error.
#[test]
fn a_malformed_xml_shows_no_doc() {
    let xml = r#"<doc><assembly><name>System.Runtime</name></assembly><members>
        <member name="T:System.String"><summary>Represents text.</summary></member>
        <member name="T:System.Int32"><summary>Broken"#;
    let mut p = project(IS_NULL_OR_EMPTY, Xml::Text(xml.to_string()), None);
    let body = hover_at(&mut p, 1, "String");
    assert!(split(&body).0.contains("String"), "{body}");
    assert_eq!(split(&body).1, None, "{body}");
}

/// A doc file declaring a different assembly is not this DLL's documentation
/// (a stale or misplaced file), however well its keys happen to match.
#[test]
fn a_doc_file_for_another_assembly_shows_no_doc() {
    let xml = r#"<doc><assembly><name>Something.Else</name></assembly><members>
        <member name="T:System.String"><summary>Not this one.</summary></member>
        </members></doc>"#;
    let mut p = project(IS_NULL_OR_EMPTY, Xml::Text(xml.to_string()), None);
    assert_eq!(split(&hover_at(&mut p, 1, "String")).1, None);
}

/// The doc cache is validated against the file, not only against watched-file
/// events: a project's own build output can rewrite its `.xml` without the DLL
/// changing (a doc-comment edit does not change IL).
#[test]
fn a_rewritten_xml_is_reread() {
    let first = r#"<doc><assembly><name>System.Runtime</name></assembly><members>
        <member name="T:System.String"><summary>First.</summary></member>
        </members></doc>"#;
    let mut p = project(IS_NULL_OR_EMPTY, Xml::Text(first.to_string()), None);
    assert_eq!(split(&hover_at(&mut p, 1, "String")).1, Some("First."));
    let second = first.replace("First.", "Second, and longer.");
    fs::write(&p.runtime_xml, second).unwrap();
    assert_eq!(
        split(&hover_at(&mut p, 1, "String")).1,
        Some("Second, and longer.")
    );
}

/// How to build the `MeasureKey` fixture.
#[derive(Clone, Copy)]
enum Build {
    Plain,
    /// With no F# signature or optimisation data, which also drops the
    /// assembly-level `FSharpInterfaceDataVersionAttribute`: the image's only
    /// mark of the F# compiler is then its types' `CompilationMappingAttribute`.
    Stripped,
}

/// The `MeasureKey` fixture (`tests/fixtures/xml_doc/MeasureKey`), built once
/// per [`Build`] with its fsc-written `.xml`.
fn measure_key(how: Build) -> Shipped {
    static PLAIN: std::sync::OnceLock<(TempDir, PathBuf)> = std::sync::OnceLock::new();
    static STRIPPED: std::sync::OnceLock<(TempDir, PathBuf)> = std::sync::OnceLock::new();
    let cell = match how {
        Build::Plain => &PLAIN,
        Build::Stripped => &STRIPPED,
    };
    let (_, dll) = cell.get_or_init(|| {
        let fixture =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/xml_doc/MeasureKey");
        let build = TempDir::new().unwrap();
        for file in ["MeasureKey.fsproj", "Library.fs"] {
            fs::copy(fixture.join(file), build.path().join(file)).unwrap();
        }
        let out = build.path().join("out");
        let mut cmd = std::process::Command::new("dotnet");
        cmd.args(["build", "-c", "Release", "--nologo", "-o"])
            .arg(&out)
            .arg(build.path().join("MeasureKey.fsproj"));
        if let Build::Stripped = how {
            cmd.arg("-p:OtherFlags=--nointerfacedata%20--nooptimizationdata");
        }
        borzoi_spawn::BoundedCommand::new(cmd)
            .timeout(std::time::Duration::from_secs(1800))
            .run_ok("dotnet build MeasureKey fixture");
        let dll = out.join("MeasureKey.dll");
        (build, dll)
    });
    Shipped {
        dll: dll.clone(),
        xml: dll.with_extension("xml"),
    }
}

/// Both builds of the fixture read as the F# dialect — the stripped one by
/// its types' `CompilationMappingAttribute` alone, as it carries no other mark.
#[test]
fn every_build_of_an_fsharp_assembly_is_the_fsharp_dialect() {
    use borzoi_assembly::doc_id::DocIdDialect;
    use borzoi_assembly::{Ecma335Assembly, EcmaView};
    for how in [Build::Plain, Build::Stripped] {
        let bytes = fs::read(measure_key(how).dll).unwrap();
        let view = Ecma335Assembly::parse(&bytes).unwrap();
        if let Build::Stripped = how {
            assert_eq!(view.fsharp_resources().unwrap(), Vec::new());
            let (_, skips) = view.enumerate_type_defs_with_skips().unwrap();
            assert!(skips.fsharp_signature_non_authoritative);
        }
        assert_eq!(view.doc_id_dialect().unwrap(), DocIdDialect::FSharp);
    }
}

/// fsc keys a member from its F# signature, where a measure type parameter
/// counts towards the generic arity although IL erases it. `D.Measured` (IL
/// `F(float64)`) is keyed ``M:MeasureKey.D.F``1(System.Double)`` — which is
/// also the key computed from `D.Plain`'s IL (``F<T>(float64)``), and no other
/// target's computed key, so no census of computed keys sees the clash.
/// `Plain` is undocumented, so must not show `Measured`'s documentation; and
/// `Measured`'s own entry is out of reach of an IL-derived key, so neither
/// shows any.
#[test]
fn an_fsharp_member_never_shows_another_members_doc() {
    let fixture = measure_key(Build::Plain);
    let shipped = fs::read_to_string(&fixture.xml).unwrap();
    assert!(
        shipped.contains(r#"<member name="M:MeasureKey.D.F``1(System.Double)">"#),
        "fsc keys `measured` with its measure parameter counted:\n{shipped}"
    );
    let src =
        "module M\nlet a = MeasureKey.D.Plain<int>(1.0)\nlet b = MeasureKey.D.Measured(1.0)\n";
    let mut p = project_with(
        src,
        Xml::Absent,
        vec![
            Package {
                id: "FSharp.Core",
                dll: sdk_fsharp_core(),
                xml: Xml::Absent,
            },
            Package {
                id: "MeasureKey",
                dll: fixture,
                xml: Xml::Shipped,
            },
        ],
    );
    for (line, name) in [(1, "Plain"), (2, "Measured")] {
        let body = hover_at(&mut p, line, name);
        let (signature, doc) = split(&body);
        assert!(signature.contains(name), "{name} resolves:\n{body}");
        assert_eq!(doc, None, "{name} shows no documentation:\n{body}");
    }
}

// ---------------------------------------------------------------------------
// Keys from the env
// ---------------------------------------------------------------------------

mod keys {
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::Path;

    use borzoi::xml_doc::key::{DocTarget, KeyCensus, KeyError, doc_key};
    use borzoi_assembly::doc_id::{DocIdDialect, TypeDocName, type_doc_name, walk_doc_ids};
    use borzoi_assembly::{Ecma335Assembly, EcmaView, Entity};
    use borzoi_sema::AssemblyEnv;

    use crate::common::doc_key_env_of;

    use super::{sdk_fsharp_core, system_runtime};

    fn roots(dll: &Path) -> Vec<Entity> {
        let bytes = std::fs::read(dll).unwrap();
        Ecma335Assembly::parse(&bytes)
            .unwrap()
            .enumerate_type_defs()
            .unwrap()
    }

    fn dialect(dll: &Path) -> DocIdDialect {
        let bytes = std::fs::read(dll).unwrap();
        Ecma335Assembly::parse(&bytes)
            .unwrap()
            .doc_id_dialect()
            .unwrap()
    }

    /// The env over `roots`, read from `dll` (possibly doctored since), with
    /// `dll`'s own dialect.
    fn env_of(dll: &Path, roots: Vec<Entity>) -> AssemblyEnv {
        doc_key_env_of(vec![(dll.to_path_buf(), roots, dialect(dll))])
    }

    /// The `T:` key of every type in the tree under `entity`. A union case keys
    /// `T:` too, but is not a type: in the F# dialect it is a member's key.
    fn type_keys(entity: &Entity, enclosing: Option<&TypeDocName>, out: &mut BTreeSet<String>) {
        let decl = type_doc_name(entity, enclosing);
        out.insert(decl.type_id());
        for nested in &entity.nested_types {
            type_keys(nested, Some(&decl), out);
        }
    }

    /// Every target the env exposes: each entity, and each member of each.
    fn targets(env: &AssemblyEnv) -> Vec<DocTarget> {
        let mut out = Vec::new();
        for handle in env.all_handles() {
            out.push(DocTarget::Entity(handle));
            for idx in env.member_indices(handle) {
                out.push(DocTarget::Member {
                    parent: handle,
                    idx,
                });
            }
        }
        out
    }

    /// Key every target of `env`, census taken per assembly: the keys
    /// committed to, and how many targets were refused as sharing a key. A
    /// member refused as F# is neither.
    fn key_all(env: &AssemblyEnv) -> (BTreeMap<String, DocTarget>, usize) {
        let mut censuses: BTreeMap<std::path::PathBuf, KeyCensus> = BTreeMap::new();
        let mut keyed: BTreeMap<String, DocTarget> = BTreeMap::new();
        let mut shared = 0;
        for target in targets(env) {
            let dll = env
                .assembly_path(target.owner())
                .expect("path-bearing env")
                .to_path_buf();
            let census = censuses
                .entry(dll.clone())
                .or_insert_with(|| KeyCensus::of_assembly(env, &dll));
            match doc_key(env, census, target) {
                Ok(key) => {
                    // Injectivity: one key, one target — within an assembly
                    // (one doc file). Keys are prefixed by the assembly here
                    // so two assemblies' identical keys do not collide.
                    let scoped = format!("{}|{key}", dll.display());
                    if let Some(previous) = keyed.insert(scoped.clone(), target) {
                        panic!("{scoped} keys both {previous:?} and {target:?}");
                    }
                }
                Err(KeyError::Shared) => shared += 1,
                Err(KeyError::FSharpMember) => {}
                Err(e) => panic!("{target:?}: {e:?}"),
            }
        }
        (keyed, shared)
    }

    /// Keying a handle walks the env's enclosing chain; the generator's own
    /// walk recurses down the entity tree. Over every type and member of two
    /// real assemblies the two must produce the same keys — so the chain walk
    /// threads nested types' arity exactly as the generator does — and the
    /// keys committed to must be injective over every target. FSharp.Core is
    /// in the F# dialect, so of its keys only the types' are committed.
    #[test]
    fn every_target_keys_as_the_generator_walk_does_and_injectively() {
        assert_eq!(dialect(&system_runtime().dll), DocIdDialect::Metadata);
        assert_eq!(dialect(&sdk_fsharp_core().dll), DocIdDialect::FSharp);
        for dll in [system_runtime().dll, sdk_fsharp_core().dll] {
            let fsharp = dialect(&dll) == DocIdDialect::FSharp;
            let roots = roots(&dll);
            let mut walked: BTreeMap<String, usize> = BTreeMap::new();
            let mut types: BTreeSet<String> = BTreeSet::new();
            for root in &roots {
                walk_doc_ids(root, None, &mut |id| *walked.entry(id).or_default() += 1);
                type_keys(root, None, &mut types);
            }
            let env = env_of(&dll, roots);
            let (keyed, shared) = key_all(&env);
            let prefix = format!("{}|", dll.display());
            let keyed: BTreeSet<&str> = keyed.keys().map(|k| &k[prefix.len()..]).collect();
            let unique: BTreeSet<&str> = walked
                .iter()
                .filter(|(k, n)| **n == 1 && (!fsharp || types.contains(*k)))
                .map(|(k, _)| k.as_str())
                .collect();
            assert_eq!(
                keyed,
                unique,
                "{}: keyed ≠ the walk's once-generated keys",
                dll.display()
            );
            eprintln!(
                "{}: {} keys, {shared} targets refused as sharing one",
                dll.display(),
                keyed.len()
            );
        }
    }

    /// `T:A.B` is the key of both `namespace A { type B }` and a type `B`
    /// nested in a type `A`, and `M:A.B.F` of a member `F` of either. An
    /// assembly holding both cannot tell them apart by key, so every target
    /// whose key is shared — type or member — is refused; the rest of the
    /// assembly keys as before.
    #[test]
    fn a_type_key_shared_by_a_namespace_and_a_nesting_is_refused() {
        let dll = system_runtime().dll;
        let mut roots = roots(&dll);
        // A non-generic top-level type with a non-generic nested type: the
        // nested one, re-declared top-level in namespace `<ns>.<Outer>`,
        // generates the same keys.
        let (outer, nested) = roots
            .iter()
            .find_map(|r| {
                (r.generic_parameters.is_empty())
                    .then(|| {
                        r.nested_types
                            .iter()
                            .find(|n| n.generic_parameters.is_empty())
                    })
                    .flatten()
                    .map(|n| (r.clone(), n.clone()))
            })
            .expect("System.Runtime has a non-generic nested type");
        let mut twin = nested.clone();
        twin.namespace = outer
            .namespace
            .iter()
            .cloned()
            .chain([outer.name.clone()])
            .collect();
        roots.push(twin);
        let env = env_of(&dll, roots);
        let census = KeyCensus::of_assembly(&env, &dll);

        let named = |h: borzoi_sema::EntityHandle| env.entity(h).name == nested.name;
        let clashing: Vec<_> = env
            .all_handles()
            .filter(|&h| {
                named(h)
                    && doc_key(&env, &KeyCensus::default(), DocTarget::Entity(h)).ok()
                        == Some(format!(
                            "T:{}.{}.{}",
                            outer.namespace.join("."),
                            outer.name,
                            nested.name
                        ))
            })
            .collect();
        assert_eq!(
            clashing.len(),
            2,
            "the namespace twin and the nested original"
        );
        for h in clashing {
            assert_eq!(
                doc_key(&env, &census, DocTarget::Entity(h)),
                Err(KeyError::Shared)
            );
            for idx in env.member_indices(h) {
                assert_eq!(
                    doc_key(&env, &census, DocTarget::Member { parent: h, idx }),
                    Err(KeyError::Shared)
                );
            }
        }
        let (keyed, shared) = key_all(&env);
        assert!(
            keyed.len() > 10_000 && shared >= 2,
            "{} keyed, {shared} refused",
            keyed.len()
        );
    }

    /// Two members of one type generating one key cannot be told apart by it,
    /// so neither is keyed — whichever entry the file has is not provably
    /// either's. (No real assembly swept has such a pair; this builds one.)
    #[test]
    fn members_sharing_a_key_are_both_refused() {
        let dll = system_runtime().dll;
        let mut roots = roots(&dll);
        let string = roots
            .iter_mut()
            .find(|e| e.name == "String" && e.namespace == ["System"])
            .expect("System.String");
        let twin = string.members[0].clone();
        string.members.push(twin);
        let env = env_of(&dll, roots);
        let census = KeyCensus::of_assembly(&env, &dll);
        let handle = env
            .all_handles()
            .find(|&h| env.entity(h).name == "String" && env.entity(h).namespace == ["System"])
            .unwrap();
        let indices: Vec<_> = env.member_indices(handle).collect();
        for idx in [indices[0], *indices.last().unwrap()] {
            assert_eq!(
                doc_key(
                    &env,
                    &census,
                    DocTarget::Member {
                        parent: handle,
                        idx
                    }
                ),
                Err(KeyError::Shared)
            );
        }
        assert!(
            doc_key(
                &env,
                &census,
                DocTarget::Member {
                    parent: handle,
                    idx: indices[1]
                }
            )
            .is_ok()
        );
    }
}
