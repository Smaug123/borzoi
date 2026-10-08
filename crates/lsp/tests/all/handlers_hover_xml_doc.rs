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

use std::fs;
use std::path::{Path, PathBuf};

use crate::common::ensure_system_runtime_dll;
use borzoi::handlers::hover;
use borzoi::sdk_discovery::SdkDiscoveryEnv;
use borzoi::server::State;
use borzoi::workspace::Workspace;
use lsp_types::{
    HoverContents, HoverParams, Position, TextDocumentIdentifier, TextDocumentPositionParams, Url,
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
    uri: Url,
    runtime_xml: PathBuf,
    _tmp: TempDir,
}

fn project(src: &str, runtime_xml: Xml, fsharp_core_xml: Option<Xml>) -> Project {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let dotnet_root = root.join("dotnet");
    let pack = dotnet_root.join("packs/Microsoft.NETCore.App.Ref/10.0.0/ref/net10.0");
    stage(&system_runtime(), &pack, &runtime_xml);

    let pkgs = root.join("pkgs");
    fs::create_dir_all(&pkgs).unwrap();
    let (targets, libraries) = match &fsharp_core_xml {
        Some(xml) => {
            stage(
                &sdk_fsharp_core(),
                &pkgs.join("fsharp.core/10.0.0/lib/netstandard2.1"),
                xml,
            );
            (
                serde_json::json!({ "net10.0": { "FSharp.Core/10.0.0": {
                    "type": "package",
                    "compile": { "lib/netstandard2.1/FSharp.Core.dll": {} }
                } } }),
                serde_json::json!({ "FSharp.Core/10.0.0": {
                    "type": "package", "path": "fsharp.core/10.0.0"
                } }),
            )
        }
        None => (serde_json::json!({ "net10.0": {} }), serde_json::json!({})),
    };
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
    let uri = Url::from_file_path(&src_path).unwrap();
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

#[test]
fn an_fsharp_core_function_shows_its_whole_doc() {
    let src = "module M\nlet ys = List.map (fun x -> x + 1) [1]\n";
    let mut p = project(src, Xml::Absent, Some(Xml::Shipped));
    let body = hover_at(&mut p, 1, "map");
    let (signature, doc) = split(&body);
    assert!(signature.contains("map"), "signature first:\n{body}");
    let doc = doc.unwrap_or_else(|| panic!("List.map is documented:\n{body}"));
    for part in [
        "Builds a new collection whose elements are the results of applying the given function \
         to each of the elements of the collection.",
        "**Parameters**",
        "- `mapping` — The function to transform elements from the input list.",
        "- `list` — The input list.",
        "**Returns**: The list of transformed elements.",
        "**Remarks**: This is an O(n) operation, where n is the length of the list.",
        "**Example**",
        "```fsharp\nlet inputs = [ \"a\"; \"bbb\"; \"cc\" ]\n\ninputs |> List.map (fun x -> x.Length)\n```",
        "Evaluates to `[ 1; 3; 2 ]`",
    ] {
        assert!(doc.contains(part), "missing {part:?} in:\n{doc}");
    }
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

// ---------------------------------------------------------------------------
// Keys from the env
// ---------------------------------------------------------------------------

mod keys {
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::Path;

    use borzoi::xml_doc::key::{DocTarget, KeyCensus, KeyError, doc_key};
    use borzoi_assembly::doc_id::walk_doc_ids;
    use borzoi_assembly::{Ecma335Assembly, EcmaView, Entity};
    use borzoi_sema::AssemblyEnv;

    use super::{sdk_fsharp_core, system_runtime};

    fn roots(dll: &Path) -> Vec<Entity> {
        let bytes = std::fs::read(dll).unwrap();
        Ecma335Assembly::parse(&bytes)
            .unwrap()
            .enumerate_type_defs()
            .unwrap()
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
    /// committed to, and how many targets were refused as sharing a key.
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
                Err(e) => panic!("{target:?}: {e:?}"),
            }
        }
        (keyed, shared)
    }

    /// Keying a handle walks the env's enclosing chain; the generator's own
    /// walk recurses down the entity tree. Over every type and member of two
    /// real assemblies the two must produce the same keys — so the chain walk
    /// threads nested types' arity exactly as the generator does — and the
    /// keys committed to must be injective over every target.
    #[test]
    fn every_target_keys_as_the_generator_walk_does_and_injectively() {
        for dll in [system_runtime().dll, sdk_fsharp_core().dll] {
            let roots = roots(&dll);
            let mut walked: BTreeMap<String, usize> = BTreeMap::new();
            for root in &roots {
                walk_doc_ids(root, None, &mut |id| *walked.entry(id).or_default() += 1);
            }
            let env = AssemblyEnv::from_assemblies(vec![(dll.clone(), roots)]);
            let (keyed, shared) = key_all(&env);
            let prefix = format!("{}|", dll.display());
            let keyed: BTreeSet<&str> = keyed.keys().map(|k| &k[prefix.len()..]).collect();
            let unique: BTreeSet<&str> = walked
                .iter()
                .filter(|(_, n)| **n == 1)
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
        let env = AssemblyEnv::from_assemblies(vec![(dll.clone(), roots)]);
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
        let env = AssemblyEnv::from_assemblies(vec![(dll.clone(), roots)]);
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
