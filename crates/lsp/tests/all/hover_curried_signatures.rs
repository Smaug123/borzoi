//! The **curried-signature** hover differential: the signature hover shows for
//! a referenced F# function or member, against the signature FCS shows for the
//! same use.
//!
//! An F# `let f a b` and `let f (a, b)` both compile to a two-parameter method,
//! and so do `member x.M a b` and `member x.M(a, b)`. A label read off the IL
//! alone therefore shows a curried function tupled (`a: int * b: int -> int`
//! where FCS shows `a: int -> b: int -> int`): a wrong type, which every check
//! that compares names passes.
//!
//! The universe is generated here: argument-group shapes (one group of one,
//! two or three arguments, two and three curried groups of every such size, a
//! `()` group alone, first and last, a tuple-typed parameter, a wildcard, a
//! function-typed return so the IL arity is not the type's, generic
//! parameters, optional and `[<ParamArray>]` arguments where F# allows them) ×
//! every carrier a referenced F# function sits on (a module `let`, an instance
//! and a static member, an instance and a static augmentation of a plain type
//! and of a generic one), plus module values. Each is used unapplied in a probe, hovered on its name through the
//! real hover handler against a "restored" project referencing the built
//! fixture, and graded against FCS's tooltip at the same use
//! (`fcs-dump tooltips`).
//!
//! # The currency
//!
//! Both labels are read into one structure: the argument groups (the
//! top-level `->` pieces before the last), each group's arguments (its
//! top-level `*` pieces) with their names, `?` and attributes, and the return.
//! Each type is parsed, so the comparison is blind to what does not change a
//! type: redundant parentheses inside one, `'T[]` against `'T array`, a type
//! parameter's name (compared up to renaming), an attribute's namespace. It is
//! not blind to the group structure: a function-typed return FCS parenthesises
//! (`a: int -> (int -> int)`) is one piece, and a label that drops the
//! parentheses reads as one more group. The head's keyword, its qualifying
//! type (`member C.M` against `member M`) and an explicit type-parameter list
//! are not compared.
//!
//! # The properties
//!
//! 1. **Soundness.** A label that shows a signature shows FCS's.
//! 2. **Identity survives.** Every site hovers as its member.
//! 3. **Non-vacuity.** Every site shows a signature: each fixture member's val
//!    is in the pickle, so none has a grouping to withhold.
//! 4. **Own type parameters.** A label's type-parameter list names exactly
//!    the type variables of its signature that are not its declaring type's.
//!    An augmentation of `GTarget<'t>` compiles to a module method generic
//!    over `'t` as well as its own, so a list read off the IL names `'t` too;
//!    FCS shows no list for an implicitly generic member, so property 1 cannot
//!    see it.

use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use borzoi::handlers::hover;
use borzoi::sdk_discovery::SdkDiscoveryEnv;
use borzoi::server::State;
use borzoi::uri::UriExt as _;
use borzoi::workspace::Workspace;
use borzoi_spawn::BoundedCommand;
use lsp_types::{
    HoverContents, HoverParams, Position, TextDocumentIdentifier, TextDocumentPositionParams, Uri,
    WorkDoneProgressParams,
};
use serde::Deserialize;
use tempfile::TempDir;

use crate::common::{ensure_fsharp_core_dll, ensure_system_runtime_dll, fcs_dump_command};

const FIXTURE_ASM: &str = "CurriedFixture";
const NS: &str = "CurriedFixture";

// ---- the generated universe ----------------------------------------------

/// One parameter as the fixture declares it.
#[derive(Debug, Clone, Copy)]
enum Param {
    /// `name: T`.
    Named,
    /// `_: T`.
    Wild,
    /// `()`, which is a group of its own.
    Unit,
    /// `?name: T` — members only.
    Optional,
    /// `[<ParamArray>] name: T[]` — members only, last in its group.
    Variadic,
}

/// How a declaration returns.
#[derive(Debug, Clone, Copy)]
enum Ret {
    /// `= 0`.
    Int,
    /// `: int -> int = id`, so the IL method takes fewer parameters than the
    /// F# type has arrows.
    Function,
    /// `: int * string = (0, "")`.
    Tuple,
}

/// Where a declaration sits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Carrier {
    /// `let fN …` in module `Fns`.
    Let,
    /// `member _.MN …` on class `C`.
    Instance,
    /// `static member SN …` on class `C`.
    Static,
    /// `member _.XN …` in `type Target with` in module `Aug`.
    InstanceAug,
    /// `static member YN …` in `type Target with` in module `Aug`.
    StaticAug,
    /// `member _.GXN …` in `type GTarget<'t> with` in module `Aug`.
    GenericInstanceAug,
    /// `static member GYN …` in `type GTarget<'t> with` in module `Aug`.
    GenericStaticAug,
    /// An `abstract` slot on class `V`, with or without a `default`: F#
    /// pickles a slot and its default as two vals for one IL method.
    Virtual,
}

const CARRIERS: [Carrier; 7] = [
    Carrier::Let,
    Carrier::Instance,
    Carrier::Static,
    Carrier::InstanceAug,
    Carrier::StaticAug,
    Carrier::GenericInstanceAug,
    Carrier::GenericStaticAug,
];

impl Carrier {
    /// Whether the declaring type is `GTarget<'t>`, whose `'t` a member's
    /// parameters may use without it being the member's own.
    fn on_generic_target(self) -> bool {
        matches!(
            self,
            Carrier::GenericInstanceAug | Carrier::GenericStaticAug
        )
    }
}

/// The parameter types the fixture cycles through: primitives, generic
/// parameters, a postfix application, and the two types that need
/// parenthesising as a parameter.
const TYPES: &[&str] = &[
    "int",
    "string",
    "'a",
    "int list",
    "(int -> int)",
    "float",
    "(int * string)",
    "bool",
    "'b",
    "string option",
];

/// A shape: its groups, and whether only a member may declare it.
struct Shape {
    groups: Vec<Vec<Param>>,
    members_only: bool,
}

fn shapes() -> Vec<Shape> {
    let mut out = Vec::new();
    // Every 1–3 groups of 1–3 named arguments.
    let mut sizes: Vec<Vec<usize>> = vec![vec![]];
    for _ in 0..3 {
        sizes = sizes
            .iter()
            .flat_map(|s| {
                (1..=3).map(move |n| {
                    let mut s = s.clone();
                    s.push(n);
                    s
                })
            })
            .collect();
        out.extend(sizes.iter().map(|s| Shape {
            groups: s.iter().map(|&n| vec![Param::Named; n]).collect(),
            members_only: false,
        }));
    }
    let named = |groups: Vec<Vec<Param>>| Shape {
        groups,
        members_only: false,
    };
    out.push(named(vec![vec![Param::Unit]]));
    out.push(named(vec![vec![Param::Named], vec![Param::Unit]]));
    out.push(named(vec![vec![Param::Unit], vec![Param::Named]]));
    out.push(named(vec![
        vec![Param::Named, Param::Named],
        vec![Param::Unit],
    ]));
    out.push(named(vec![vec![Param::Wild], vec![Param::Named]]));
    out.push(named(vec![vec![Param::Named, Param::Wild]]));
    out.push(named(vec![vec![Param::Wild]]));
    out.push(Shape {
        groups: vec![vec![Param::Named, Param::Optional]],
        members_only: true,
    });
    out.push(Shape {
        groups: vec![vec![Param::Optional]],
        members_only: true,
    });
    out.push(Shape {
        groups: vec![vec![Param::Named, Param::Variadic]],
        members_only: true,
    });
    out
}

/// One probe site: a declaration and its unapplied use.
#[derive(Debug)]
struct Site {
    /// `Fns.f3 : (int) (string, 'a)`, for messages.
    label: String,
    /// The declaring entity's full name, as the env names it.
    owner: &'static str,
    /// Whether the resolver reaches the use, so that the handler hovers it.
    /// It does not reach an instance member or an augmentation: their uses
    /// need the receiver's type and the call's overload resolution, which
    /// defers on every F# member of two or more parameters (the curried-member
    /// gate). Those are graded through the label the handler would render.
    reached: bool,
    /// The declaring type's type parameters, which the member's signature
    /// may use but its type-parameter list must not name.
    target_typars: &'static [&'static str],
    /// 1-based line of the use in the probe.
    line: usize,
    /// Byte range of the member name in the probe.
    name: (usize, usize),
}

/// One use the probe makes: `head` (`c.M3`) ends in the member's `name`, and
/// `args` applies it.
struct Use {
    label: String,
    carrier: Carrier,
    head: String,
    args: String,
    name: String,
}

struct Universe {
    fixture: String,
    probe: String,
    sites: Vec<Site>,
}

fn universe() -> Universe {
    let mut lets = String::new();
    let mut class = String::new();
    let mut aug = String::new();
    let mut gaug = String::new();
    let mut uses: Vec<Use> = Vec::new();
    let mut ty_cursor = 0usize;
    let mut next_ty = || {
        let t = TYPES[ty_cursor % TYPES.len()];
        ty_cursor += 1;
        t
    };
    let rets = [Ret::Int, Ret::Function, Ret::Tuple];
    let mut n = 0usize;
    for shape in shapes() {
        for &carrier in &CARRIERS {
            if shape.members_only && !matches!(carrier, Carrier::Instance | Carrier::Static) {
                continue;
            }
            let mut arg_no = 0usize;
            // On `GTarget<'t>`, `'b` is the target's `'t`.
            let ty = |t: &'static str| match t {
                "'b" if carrier.on_generic_target() => "'t",
                t => t,
            };
            let groups: Vec<String> = shape
                .groups
                .iter()
                .map(|group| {
                    let args: Vec<String> = group
                        .iter()
                        .map(|p| {
                            arg_no += 1;
                            match p {
                                Param::Named => format!("a{arg_no}: {}", ty(next_ty())),
                                Param::Wild => format!("_: {}", ty(next_ty())),
                                Param::Unit => String::new(),
                                Param::Optional => format!("?o{arg_no}: int"),
                                Param::Variadic => {
                                    format!("[<System.ParamArray>] xs{arg_no}: int[]")
                                }
                            }
                        })
                        .collect();
                    format!("({})", args.join(", "))
                })
                .collect();
            let ret = match rets[n % rets.len()] {
                Ret::Int => " = 0".to_string(),
                Ret::Function => " : int -> int = id".to_string(),
                Ret::Tuple => " : int * string = (0, \"\")".to_string(),
            };
            let params = groups.join(" ");
            let (decl, access, name) = match carrier {
                Carrier::Let => (
                    format!("    let f{n} {params}{ret}\n"),
                    format!("Fns.f{n}"),
                    format!("f{n}"),
                ),
                Carrier::Instance => (
                    format!("    member _.M{n} {params}{ret}\n"),
                    format!("c.M{n}"),
                    format!("M{n}"),
                ),
                Carrier::Static => (
                    format!("    static member S{n} {params}{ret}\n"),
                    format!("C.S{n}"),
                    format!("S{n}"),
                ),
                Carrier::InstanceAug => (
                    format!("        member _.X{n} {params}{ret}\n"),
                    format!("t.X{n}"),
                    format!("X{n}"),
                ),
                Carrier::Virtual => unreachable!("declared apart from the shape product"),
                Carrier::StaticAug => (
                    format!("        static member Y{n} {params}{ret}\n"),
                    format!("Target.Y{n}"),
                    format!("Y{n}"),
                ),
                Carrier::GenericInstanceAug => (
                    format!("        member _.GX{n} {params}{ret}\n"),
                    format!("g.GX{n}"),
                    format!("GX{n}"),
                ),
                Carrier::GenericStaticAug => (
                    format!("        static member GY{n} {params}{ret}\n"),
                    format!("GTarget<'x>.GY{n}"),
                    format!("GY{n}"),
                ),
            };
            match carrier {
                Carrier::Let => lets.push_str(&decl),
                Carrier::Instance | Carrier::Static => class.push_str(&decl),
                Carrier::InstanceAug | Carrier::StaticAug => aug.push_str(&decl),
                Carrier::GenericInstanceAug | Carrier::GenericStaticAug => gaug.push_str(&decl),
                Carrier::Virtual => unreachable!("declared apart from the shape product"),
            }
            // A member is used applied, every group given, since the resolver
            // reaches a member only through a call; a module function is
            // used as a value. Either way the use is the member, and FCS's
            // tooltip there is its signature.
            let args = if carrier == Carrier::Let {
                String::new()
            } else {
                shape
                    .groups
                    .iter()
                    .map(|group| {
                        let given: Vec<&str> = group
                            .iter()
                            .filter(|p| matches!(p, Param::Named | Param::Wild))
                            .map(|_| "Unchecked.defaultof<_>")
                            .collect();
                        format!("({})", given.join(", "))
                    })
                    .collect()
            };
            uses.push(Use {
                label: format!("{access} : {params}{ret}"),
                carrier,
                head: access,
                args,
                name,
            });
            n += 1;
        }
    }
    // Abstract slots, curried, tupled and mixed, each with a default (two vals,
    // one IL method) and without (an abstract IL method).
    let mut virtuals = String::new();
    for (i, (slot, params)) in [
        ("a1: int -> a2: string -> int", "(a1: int) (a2: string)"),
        ("a1: int * a2: string -> int", "(a1: int, a2: string)"),
        (
            "a1: int -> a2: string * a3: float -> int",
            "(a1: int) (a2: string, a3: float)",
        ),
    ]
    .iter()
    .enumerate()
    {
        virtuals.push_str(&format!(
            "    abstract D{i}: {slot}\n    default _.D{i} {params} = 0\n    abstract A{i}: {slot}\n"
        ));
        for name in [format!("D{i}"), format!("A{i}")] {
            uses.push(Use {
                label: format!("v.{name} : {slot}"),
                carrier: Carrier::Virtual,
                head: format!("v.{name}"),
                args: String::new(),
                name,
            });
        }
    }
    // Module values, including function-typed ones: a value is no group at
    // all, and one bound to a lambda is a function of the lambda's arity.
    for (i, decl) in [
        "let v0 = 1",
        "let v1 : int -> int = id",
        "let v2 = fun (x: int) (y: string) -> x",
        "let v3 : int * string = (0, \"\")",
        "let mutable v4 = 0",
        "let v5 : int -> string -> int = fun x -> fun _ -> x",
        "let v6 : (int -> int) list = []",
    ]
    .iter()
    .enumerate()
    {
        lets.push_str(&format!("    {decl}\n"));
        uses.push(Use {
            label: format!("Fns.v{i} : {decl}"),
            carrier: Carrier::Let,
            head: format!("Fns.v{i}"),
            args: String::new(),
            name: format!("v{i}"),
        });
    }

    let fixture = format!(
        "// GENERATED by crates/lsp/tests/all/hover_curried_signatures.rs — do not edit.\n\
         namespace {NS}\n\
         \n\
         type Target() =\n\
         \x20   member _.Self = 0\n\
         \n\
         type GTarget<'t>() =\n\
         \x20   member _.Self = 0\n\
         \n\
         module Fns =\n\
         {lets}\n\
         type C() =\n\
         {class}\n\
         [<AbstractClass>]\n\
         type V() =\n\
         {virtuals}\n\
         module Aug =\n\
         \x20   type Target with\n\
         {aug}\
         \x20   type GTarget<'t> with\n\
         {gaug}"
    );

    let mut probe = format!(
        "module Probe\n\
         open {NS}\n\
         open {NS}.Aug\n\
         \n\
         let c : C = failwith \"fixture\"\n\
         let t : Target = failwith \"fixture\"\n\
         let v : V = failwith \"fixture\"\n\
         \n"
    );
    let mut sites = Vec::new();
    for (i, u) in uses.into_iter().enumerate() {
        // A use on `GTarget` sits in a function generic over the target's
        // argument, so FCS's tooltip there keeps the target's type parameter.
        let prefix = if u.carrier.on_generic_target() {
            format!("let r{i} (g: GTarget<'x>) = ")
        } else {
            format!("let r{i} = ")
        };
        let line = probe.lines().count() + 1;
        let start = probe.len() + prefix.len() + u.head.len() - u.name.len();
        sites.push(Site {
            label: u.label,
            owner: match u.carrier {
                Carrier::Let => "CurriedFixture.Fns",
                Carrier::Instance | Carrier::Static => "CurriedFixture.C",
                Carrier::InstanceAug
                | Carrier::StaticAug
                | Carrier::GenericInstanceAug
                | Carrier::GenericStaticAug => "CurriedFixture.Aug",
                Carrier::Virtual => "CurriedFixture.V",
            },
            target_typars: if u.carrier.on_generic_target() {
                &["'t"]
            } else {
                &[]
            },
            reached: matches!(u.carrier, Carrier::Let | Carrier::Static),
            line,
            name: (start, start + u.name.len()),
        });
        probe.push_str(&format!("{prefix}{}{}\n", u.head, u.args));
        debug_assert_eq!(&probe[start..start + u.name.len()], u.name);
    }
    Universe {
        fixture,
        probe,
        sites,
    }
}

// ---- building and querying ------------------------------------------------

/// The fixture, built once per test binary into a directory of its own.
fn fixture_dll() -> &'static Path {
    static BUILT: OnceLock<(TempDir, PathBuf)> = OnceLock::new();
    let (_, dll) = BUILT.get_or_init(|| {
        let source = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/curried_env/CurriedFixture.fsproj");
        let build = TempDir::new().unwrap();
        fs::copy(&source, build.path().join("CurriedFixture.fsproj")).unwrap();
        fs::write(build.path().join("Generated.fs"), &run_universe().fixture).unwrap();
        let out = build.path().join("out");
        let mut cmd = std::process::Command::new("dotnet");
        cmd.args(["build", "-c", "Release", "--nologo", "-o"])
            .arg(&out)
            .arg(build.path().join("CurriedFixture.fsproj"));
        BoundedCommand::new(cmd)
            .timeout(std::time::Duration::from_secs(1800))
            .run_ok("dotnet build curried fixture");
        let dll = out.join(format!("{FIXTURE_ASM}.dll"));
        (build, dll)
    });
    dll
}

fn run_universe() -> &'static Universe {
    static U: OnceLock<Universe> = OnceLock::new();
    U.get_or_init(universe)
}

/// A "restored" project compiling `src` against the framework's
/// `System.Runtime` and `netstandard`, FSharp.Core, and the fixture — the last
/// two as NuGet packages' compile assets.
fn project(src: &str) -> (State, Uri, TempDir) {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let dotnet_root = root.join("dotnet");
    let pack = dotnet_root.join("packs/Microsoft.NETCore.App.Ref/10.0.0/ref/net10.0");
    fs::create_dir_all(&pack).unwrap();
    let runtime = ensure_system_runtime_dll();
    let ref_dir = runtime
        .parent()
        .expect("System.Runtime.dll sits in a ref dir");
    for dll in ["System.Runtime.dll", "netstandard.dll"] {
        fs::copy(ref_dir.join(dll), pack.join(dll)).unwrap();
    }
    let pkgs = root.join("pkgs");
    let mut target = serde_json::Map::new();
    let mut libraries = serde_json::Map::new();
    for (id, dll) in [
        ("FSharp.Core", ensure_fsharp_core_dll()),
        (FIXTURE_ASM, fixture_dll().to_path_buf()),
    ] {
        let folder = format!("{}/10.0.0", id.to_lowercase());
        let lib = pkgs.join(&folder).join("lib/netstandard2.1");
        fs::create_dir_all(&lib).unwrap();
        let file = dll.file_name().unwrap().to_str().unwrap().to_owned();
        fs::copy(&dll, lib.join(&file)).unwrap();
        let key = format!("{id}/10.0.0");
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
    let assets = serde_json::json!({
        "version": 3,
        "targets": { "net10.0": target },
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
        r#"<Project><ItemGroup><Compile Include="Probe.fs" /></ItemGroup></Project>"#,
    )
    .unwrap();
    let src_path = root.join("Probe.fs");
    fs::write(&src_path, src).unwrap();

    let mut state = State::default();
    state.workspace = Workspace::with_env(SdkDiscoveryEnv {
        dotnet_root: Some(dotnet_root),
        ..SdkDiscoveryEnv::default()
    });
    let uri = Uri::from_file_path(&src_path).unwrap();
    state.docs.insert(uri.clone(), src.to_string());
    (state, uri, tmp)
}

#[derive(Deserialize)]
struct TooltipsDump {
    #[serde(rename = "Tooltips")]
    tooltips: Vec<RawTooltip>,
    #[serde(rename = "Errors")]
    errors: Vec<RawError>,
}

#[derive(Deserialize)]
struct RawTooltip {
    #[serde(rename = "Range")]
    range: RawRange,
    #[serde(rename = "IsFromDefinition")]
    is_from_definition: bool,
    #[serde(rename = "Descriptions")]
    descriptions: Vec<String>,
}

#[derive(Deserialize)]
struct RawRange {
    #[serde(rename = "End")]
    end: RawPos,
}

#[derive(Deserialize)]
struct RawPos {
    #[serde(rename = "Line")]
    line: u32,
    #[serde(rename = "Col")]
    col: u32,
}

#[derive(Deserialize, Debug)]
#[allow(dead_code)] // Read through `Debug`, in the rejection message.
struct RawError {
    #[serde(rename = "Line")]
    line: u32,
    #[serde(rename = "Message")]
    message: String,
}

/// FCS's tooltip descriptions for every use, keyed by the (1-based line,
/// 0-based column) its range ends at, and the errors it reported.
/// FCS's tooltip descriptions per use, keyed by where its range ends.
type Tooltips = HashMap<(usize, usize), Vec<String>>;

fn fcs_tooltips(probe: &str) -> (Tooltips, Vec<RawError>) {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("CurriedProbe.fs");
    fs::write(&path, probe).unwrap();
    let mut cmd = fcs_dump_command("tooltips");
    cmd.arg(&path)
        .env("BORZOI_FCS_EXTRA_REFS", fixture_dll())
        .env_remove("BORZOI_FCS_EXCLUSIVE_REFS");
    let out = BoundedCommand::new(cmd).run_ok("fcs-dump tooltips (curried probe)");
    let json = String::from_utf8(out.stdout).expect("fcs-dump stdout is UTF-8");
    let dump: TooltipsDump = serde_json::from_str(&json).expect("fcs-dump tooltips JSON shape");
    let tips = dump
        .tooltips
        .into_iter()
        .filter(|t| !t.is_from_definition)
        .map(|t| {
            (
                (t.range.end.line as usize, t.range.end.col as usize),
                t.descriptions,
            )
        })
        .collect();
    (tips, dump.errors)
}

struct Run {
    universe: &'static Universe,
    /// Per site, the hover body at its name (`None`: no hover).
    hovers: Vec<Option<String>>,
    /// Per site, the member label the handler renders for its member, read
    /// from the env directly ([`hover::member_hover_label`]).
    labels: Vec<String>,
    /// Per site, FCS's tooltip descriptions at its use.
    fcs: Vec<Vec<String>>,
}

fn run() -> &'static Run {
    static RUN: OnceLock<Run> = OnceLock::new();
    RUN.get_or_init(|| {
        let universe = run_universe();
        let probe = &universe.probe;
        let (mut state, uri, _tmp) = project(probe);
        let (tips, errors) = fcs_tooltips(probe);
        assert!(
            errors.is_empty(),
            "FCS rejects the probe, so its tooltips grade nothing:\n{errors:#?}"
        );
        let mut hovers = Vec::new();
        let mut fcs = Vec::new();
        for site in &universe.sites {
            let line_start = probe[..site.name.0].rfind('\n').map_or(0, |i| i + 1);
            let params = HoverParams {
                text_document_position_params: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier { uri: uri.clone() },
                    position: Position {
                        line: (site.line - 1) as u32,
                        character: (site.name.0 - line_start) as u32,
                    },
                },
                work_done_progress_params: WorkDoneProgressParams::default(),
            };
            hovers.push(hover::handle(&mut state, params).map(|h| match h.contents {
                HoverContents::Markup(m) => m.value,
                other => panic!("expected markup, got {other:?}"),
            }));
            fcs.push(
                tips.get(&(site.line, site.name.1 - line_start))
                    .cloned()
                    .unwrap_or_default(),
            );
        }
        let env = fixture_env();
        let labels = universe
            .sites
            .iter()
            .zip(0..)
            .map(|(site, i)| {
                let name = &probe[site.name.0..site.name.1];
                let owner = env
                    .all_handles()
                    .find(|&h| env.entity_full_name(h) == site.owner)
                    .unwrap_or_else(|| panic!("the env declares {}", site.owner));
                let mut found = env
                    .member_indices(owner)
                    .filter(|&idx| env.member_display_name(owner, idx) == name);
                let idx = found
                    .next()
                    .unwrap_or_else(|| panic!("{}: {} declares no `{name}`", i, site.owner));
                assert!(
                    found.next().is_none(),
                    "{} declares `{name}` twice",
                    site.owner
                );
                hover::member_hover_label(&env, owner, idx)
            })
            .collect();
        Run {
            universe,
            hovers,
            labels,
            fcs,
        }
    })
}

// ---- the currency ---------------------------------------------------------

/// A type, parsed: blind to redundant parentheses, with type parameters
/// numbered by first appearance across the signature.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Ty {
    Fun(Box<Ty>, Box<Ty>),
    Tuple(Vec<Ty>),
    Struct(Vec<Ty>),
    /// A named type applied to its arguments; `'T[]` and `'T array` are both
    /// `array` of `'T`.
    App(String, Vec<Ty>),
    Var(usize),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Arg {
    attrs: Vec<String>,
    optional: bool,
    name: Option<String>,
    ty: Ty,
}

/// A signature: its argument groups, then the type after the last.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Sig {
    groups: Vec<Vec<Arg>>,
    ret: Ty,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Tok {
    Open,
    Close,
    Lt,
    Gt,
    Comma,
    Star,
    Arrow,
    Colon,
    Question,
    /// `[<`.
    AttrOpen,
    /// `>]`.
    AttrClose,
    /// `[]`, `[,]`, …: an array suffix of this rank.
    Array(usize),
    Ident(String),
}

fn lex(s: &str) -> Result<Vec<Tok>, String> {
    let b = s.as_bytes();
    let mut i = 0;
    let mut out = Vec::new();
    while i < b.len() {
        let c = b[i] as char;
        let rest = &s[i..];
        if c.is_whitespace() {
            i += 1;
        } else if rest.starts_with("->") {
            out.push(Tok::Arrow);
            i += 2;
        } else if rest.starts_with("[<") {
            out.push(Tok::AttrOpen);
            i += 2;
        } else if rest.starts_with(">]") {
            out.push(Tok::AttrClose);
            i += 2;
        } else if c == '[' {
            let close = rest
                .find(']')
                .ok_or_else(|| format!("unclosed `[` in `{s}`"))?;
            let inner = &rest[1..close];
            if !inner.chars().all(|c| c == ',') {
                return Err(format!("`[{inner}]` in `{s}`"));
            }
            out.push(Tok::Array(inner.len() + 1));
            i += close + 1;
        } else {
            let single = match c {
                '(' => Some(Tok::Open),
                ')' => Some(Tok::Close),
                '<' => Some(Tok::Lt),
                '>' => Some(Tok::Gt),
                ',' => Some(Tok::Comma),
                '*' => Some(Tok::Star),
                ':' => Some(Tok::Colon),
                '?' => Some(Tok::Question),
                _ => None,
            };
            if let Some(t) = single {
                out.push(t);
                i += 1;
                continue;
            }
            let ident_char = |c: char| c.is_alphanumeric() || c == '_' || c == '\'' || c == '.';
            if !ident_char(c) {
                return Err(format!("unexpected `{c}` in `{s}`"));
            }
            let len = rest.find(|c: char| !ident_char(c)).unwrap_or(rest.len());
            out.push(Tok::Ident(rest[..len].to_string()));
            i += len;
        }
    }
    Ok(out)
}

/// Split `toks` at every `sep` outside brackets.
fn split_top(toks: &[Tok], sep: &Tok) -> Vec<Vec<Tok>> {
    let mut out = vec![Vec::new()];
    let mut depth = 0i32;
    for t in toks {
        match t {
            Tok::Open | Tok::Lt | Tok::AttrOpen => depth += 1,
            Tok::Close | Tok::Gt | Tok::AttrClose => depth -= 1,
            _ => {}
        }
        if depth == 0 && t == sep {
            out.push(Vec::new());
        } else {
            out.last_mut().expect("never empty").push(t.clone());
        }
    }
    out
}

struct TyParser<'a> {
    toks: &'a [Tok],
    pos: usize,
    vars: &'a mut HashMap<String, usize>,
}

impl TyParser<'_> {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }

    fn eat(&mut self, t: &Tok) -> Result<(), String> {
        if self.peek() == Some(t) {
            self.pos += 1;
            Ok(())
        } else {
            Err(format!("expected {t:?} at {:?}", &self.toks[self.pos..]))
        }
    }

    fn arrow(&mut self) -> Result<Ty, String> {
        let from = self.tuple()?;
        if self.peek() == Some(&Tok::Arrow) {
            self.pos += 1;
            Ok(Ty::Fun(Box::new(from), Box::new(self.arrow()?)))
        } else {
            Ok(from)
        }
    }

    fn tuple(&mut self) -> Result<Ty, String> {
        let mut items = vec![self.postfix()?];
        while self.peek() == Some(&Tok::Star) {
            self.pos += 1;
            items.push(self.postfix()?);
        }
        Ok(if items.len() == 1 {
            items.pop().expect("one item")
        } else {
            Ty::Tuple(items)
        })
    }

    fn postfix(&mut self) -> Result<Ty, String> {
        let mut ty = self.atom()?;
        loop {
            match self.peek() {
                Some(Tok::Ident(name)) if !name.starts_with('\'') => {
                    let name = if name == "array" {
                        "array1".to_string()
                    } else {
                        name.clone()
                    };
                    self.pos += 1;
                    ty = Ty::App(name, vec![ty]);
                }
                Some(Tok::Array(rank)) => {
                    let name = format!("array{rank}");
                    self.pos += 1;
                    ty = Ty::App(name, vec![ty]);
                }
                _ => return Ok(ty),
            }
        }
    }

    fn atom(&mut self) -> Result<Ty, String> {
        match self.peek().cloned() {
            Some(Tok::Open) => {
                self.pos += 1;
                let ty = self.arrow()?;
                self.eat(&Tok::Close)?;
                Ok(ty)
            }
            Some(Tok::Ident(name)) if name == "struct" => {
                self.pos += 1;
                self.eat(&Tok::Open)?;
                let ty = self.arrow()?;
                self.eat(&Tok::Close)?;
                match ty {
                    Ty::Tuple(items) => Ok(Ty::Struct(items)),
                    other => Err(format!("struct over a non-tuple {other:?}")),
                }
            }
            Some(Tok::Ident(name)) if name.starts_with('\'') => {
                self.pos += 1;
                let next = self.vars.len();
                Ok(Ty::Var(*self.vars.entry(name).or_insert(next)))
            }
            Some(Tok::Ident(name)) => {
                self.pos += 1;
                let mut args = Vec::new();
                if self.peek() == Some(&Tok::Lt) {
                    self.pos += 1;
                    loop {
                        args.push(self.arrow()?);
                        match self.peek() {
                            Some(Tok::Comma) => self.pos += 1,
                            _ => break,
                        }
                    }
                    self.eat(&Tok::Gt)?;
                }
                Ok(Ty::App(name, args))
            }
            other => Err(format!("expected a type, found {other:?}")),
        }
    }
}

fn parse_ty(toks: &[Tok], vars: &mut HashMap<String, usize>) -> Result<Ty, String> {
    let mut p = TyParser { toks, pos: 0, vars };
    let ty = p.arrow()?;
    if p.pos != toks.len() {
        return Err(format!("trailing {:?}", &toks[p.pos..]));
    }
    Ok(ty)
}

fn parse_arg(toks: &[Tok], vars: &mut HashMap<String, usize>) -> Result<Arg, String> {
    let mut toks = toks;
    let mut attrs = Vec::new();
    while toks.first() == Some(&Tok::AttrOpen) {
        let close = toks
            .iter()
            .position(|t| *t == Tok::AttrClose)
            .ok_or("unclosed attribute")?;
        for t in &toks[1..close] {
            if let Tok::Ident(name) = t {
                let last = name.rsplit('.').next().expect("a segment");
                attrs.push(last.strip_suffix("Attribute").unwrap_or(last).to_string());
            }
        }
        toks = &toks[close + 1..];
    }
    let optional = toks.first() == Some(&Tok::Question);
    if optional {
        toks = &toks[1..];
    }
    let name = match toks {
        [Tok::Ident(name), Tok::Colon, ..] => {
            let name = name.clone();
            toks = &toks[2..];
            Some(name)
        }
        _ => None,
    };
    Ok(Arg {
        attrs,
        optional,
        name,
        ty: parse_ty(toks, vars)?,
    })
}

/// Parse the text after a label's `name:` into a [`Sig`].
fn parse_sig(text: &str) -> Result<Sig, String> {
    let toks = lex(text)?;
    let mut vars = HashMap::new();
    let mut pieces = split_top(&toks, &Tok::Arrow);
    let ret = pieces.pop().expect("split yields one piece at least");
    let groups = pieces
        .iter()
        .map(|piece| {
            split_top(piece, &Tok::Star)
                .iter()
                .map(|arg| parse_arg(arg, &mut vars))
                .collect::<Result<Vec<_>, _>>()
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Sig {
        groups,
        ret: parse_ty(&ret, &mut vars)?,
    })
}

/// The keywords a member label can open with, longest first.
const KEYWORDS: &[&str] = &[
    "static abstract member ",
    "abstract member ",
    "abstract ",
    "override ",
    "static member ",
    "val mutable ",
    "property ",
    "member ",
    "val ",
];

/// A label's member name, and the signature text after it (`None` for a label
/// that claims no type). The name is the last dotted segment of what follows
/// the keyword (`member C.M3:` names `M3`), and a type-parameter list after it
/// is skipped.
fn split_label(head: &str) -> Option<(&str, Option<&str>)> {
    let rest = KEYWORDS.iter().find_map(|k| head.strip_prefix(k))?;
    let name_end = rest.find([':', '<', ' ']).unwrap_or(rest.len());
    let name = rest[..name_end].rsplit('.').next()?;
    let mut after = &rest[name_end..];
    if after.starts_with('<') {
        let close = after.find('>')?;
        after = &after[close + 1..];
    }
    let after = after.trim_start();
    if after.is_empty() || after.starts_with("with ") {
        return Some((name, None));
    }
    let sig = after.strip_prefix(':')?.trim();
    let sig = [" with get, set", " with get", " with set"]
        .iter()
        .find_map(|s| sig.strip_suffix(s))
        .unwrap_or(sig);
    Some((name, Some(sig)))
}

/// The hover's head line with its code-span fence removed.
fn label_head(body: &str) -> Option<&str> {
    let first = body.split("\n\n").next()?;
    let inner = first.trim_matches('`');
    (inner.len() + 2 <= first.len()).then_some(inner.trim())
}

/// The env the handler builds for the probe's project, built directly: the
/// framework's `System.Runtime` and `netstandard`, FSharp.Core and the fixture.
fn fixture_env() -> borzoi_sema::AssemblyEnv {
    use borzoi_assembly::Ecma335Assembly;
    let runtime = ensure_system_runtime_dll();
    let netstandard = runtime
        .parent()
        .expect("System.Runtime.dll sits in a ref dir")
        .join("netstandard.dll");
    let bytes: Vec<Vec<u8>> = [
        runtime.clone(),
        netstandard,
        ensure_fsharp_core_dll(),
        fixture_dll().to_path_buf(),
    ]
    .iter()
    .map(|p| fs::read(p).unwrap())
    .collect();
    let views: Vec<_> = bytes
        .iter()
        .map(|b| Ecma335Assembly::parse(b).unwrap())
        .collect();
    borzoi_sema::AssemblyEnv::from_views(&views).unwrap()
}

/// Where a label came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    /// The hover handler, at the use.
    Handler,
    /// [`hover::member_hover_label`] on the use's member.
    Label,
}

impl Run {
    fn name(&self, i: usize) -> &str {
        let site = &self.universe.sites[i];
        &self.universe.probe[site.name.0..site.name.1]
    }

    /// The site's label from `source`, split: `Some((name, signature))` for a
    /// member label.
    fn label_from(&self, i: usize, source: Source) -> Option<(&str, Option<&str>)> {
        let body = match source {
            Source::Handler => self.hovers[i].as_deref()?,
            Source::Label => &self.labels[i],
        };
        split_label(label_head(body)?)
    }

    /// FCS's signature text at the site: the one description naming the site's
    /// member.
    fn fcs_sig(&self, i: usize) -> Result<&str, String> {
        let name = self.name(i);
        let mut found = self.fcs[i]
            .iter()
            .filter_map(|d| split_label(d))
            .filter(|(n, _)| *n == name);
        match (found.next(), found.next()) {
            (Some((_, Some(sig))), None) => Ok(sig),
            _ => Err(format!(
                "FCS's tooltip {:?} names no one signature",
                self.fcs[i]
            )),
        }
    }
}

/// Property 1: a label that shows a signature shows FCS's — the handler's at
/// a use, and the label it renders for the use's member.
#[test]
fn no_label_shows_a_signature_fcs_contradicts() {
    let run = run();
    let mut wrong = Vec::new();
    for (i, site) in run.universe.sites.iter().enumerate() {
        for source in [Source::Handler, Source::Label] {
            let Some((_, Some(ours))) = run.label_from(i, source) else {
                continue;
            };
            let theirs = match run.fcs_sig(i) {
                Ok(theirs) => theirs,
                Err(why) => {
                    wrong.push(format!(
                        "  {} ({source:?}): ours `{ours}`; {why}",
                        site.label
                    ));
                    continue;
                }
            };
            let verdict = match (parse_sig(ours), parse_sig(theirs)) {
                (Ok(a), Ok(b)) if a == b => continue,
                (Ok(_), Ok(_)) => "differs".to_string(),
                (Err(e), _) => format!("ours does not parse: {e}"),
                (_, Err(e)) => format!("FCS's does not parse: {e}"),
            };
            wrong.push(format!(
                "  {} ({source:?}):\n      ours `{ours}`\n      FCS  `{theirs}`\n      ({verdict})",
                site.label
            ));
        }
    }
    assert!(
        wrong.is_empty(),
        "{} label(s) over {} site(s) show a signature FCS does not:\n{}",
        wrong.len(),
        run.universe.sites.len(),
        wrong.join("\n")
    );
}

/// Property 2: every site the resolver reaches hovers as its member, and every
/// site's member renders a label naming it.
#[test]
fn every_site_hovers_as_its_member() {
    let run = run();
    let mut missing = Vec::new();
    for (i, site) in run.universe.sites.iter().enumerate() {
        let mut sources = vec![Source::Label];
        if site.reached {
            sources.push(Source::Handler);
        }
        for source in sources {
            if run
                .label_from(i, source)
                .is_none_or(|(name, _)| name != run.name(i))
            {
                missing.push(format!(
                    "  {} ({source:?}): {:?}",
                    site.label,
                    match source {
                        Source::Handler => run.hovers[i].as_deref(),
                        Source::Label => Some(run.labels[i].as_str()),
                    }
                ));
            }
        }
    }
    assert!(
        run.universe.sites.iter().filter(|s| s.reached).count() > 100,
        "too few sites are reached for the handler to be graded"
    );
    assert!(
        missing.is_empty(),
        "{} label(s) do not name their member:\n{}",
        missing.len(),
        missing.join("\n")
    );
}

/// Property 3: every site's label shows a signature.
#[test]
fn every_site_shows_a_signature() {
    let run = run();
    assert!(run.universe.sites.len() > 150, "the universe is too small");
    let untyped: Vec<String> = (0..run.universe.sites.len())
        .filter(|&i| !matches!(run.label_from(i, Source::Label), Some((_, Some(_)))))
        .map(|i| format!("  {}: {:?}", run.universe.sites[i].label, run.labels[i]))
        .collect();
    assert!(
        untyped.is_empty(),
        "{} site(s) show no signature, so the labels beside them prove nothing by \
         showing one:\n{}",
        untyped.len(),
        untyped.join("\n")
    );
}

/// A label head's explicit type-parameter list (`member M<'a, 'b>: …`), empty
/// when it shows none.
fn label_typars(head: &str) -> Vec<&str> {
    let Some(rest) = KEYWORDS.iter().find_map(|k| head.strip_prefix(k)) else {
        return Vec::new();
    };
    let name_end = rest.find([':', '<', ' ']).unwrap_or(rest.len());
    let Some(list) = rest[name_end..].strip_prefix('<') else {
        return Vec::new();
    };
    let close = list.find('>').expect("a type-parameter list closes");
    list[..close].split(',').map(str::trim).collect()
}

/// The type variables a signature text mentions, each once.
fn sig_type_vars(sig: &str) -> BTreeSet<String> {
    lex(sig)
        .expect("a label's signature lexes")
        .into_iter()
        .filter_map(|t| match t {
            Tok::Ident(name) if name.starts_with('\'') => Some(name),
            _ => None,
        })
        .collect()
}

/// Property 4: a label's type-parameter list names exactly the type variables
/// of its signature that are not its declaring type's.
#[test]
fn a_type_parameter_list_names_only_the_members_own() {
    let run = run();
    let mut wrong = Vec::new();
    let mut graded_on_generic_target = 0usize;
    for (i, site) in run.universe.sites.iter().enumerate() {
        for source in [Source::Handler, Source::Label] {
            let body = match source {
                Source::Handler => run.hovers[i].as_deref(),
                Source::Label => Some(run.labels[i].as_str()),
            };
            let Some(head) = body.and_then(label_head) else {
                continue;
            };
            let Some((_, Some(sig))) = split_label(head) else {
                continue;
            };
            let vars = sig_type_vars(sig);
            let expected: BTreeSet<&str> = vars
                .iter()
                .map(String::as_str)
                .filter(|v| !site.target_typars.contains(v))
                .collect();
            let shown: BTreeSet<&str> = label_typars(head).into_iter().collect();
            if site.target_typars.iter().any(|t| vars.contains(*t)) {
                graded_on_generic_target += 1;
            }
            if shown != expected {
                wrong.push(format!(
                    "  {} ({source:?}): `{head}` lists {shown:?}, its own are {expected:?}",
                    site.label
                ));
            }
        }
    }
    assert!(
        graded_on_generic_target > 10,
        "too few labels use the generic target's type parameter to grade the list"
    );
    assert!(
        wrong.is_empty(),
        "{} label(s) list type parameters that are not the member's own:\n{}",
        wrong.len(),
        wrong.join("\n")
    );
}

/// The currency reads FCS's own spellings the way it reads ours.
#[test]
fn the_currency_is_blind_only_to_spelling() {
    let same = |a: &str, b: &str| assert_eq!(parse_sig(a), parse_sig(b), "`{a}` vs `{b}`");
    let differ = |a: &str, b: &str| assert_ne!(parse_sig(a), parse_sig(b), "`{a}` vs `{b}`");
    same("xs: int[] -> int", "xs: int array -> int");
    same("x: 'a -> 'a * 'b", "x: 'T -> ('T * 'U)");
    same(
        "[<ParamArray>] xs: int[] -> int",
        "[<System.ParamArray>] xs: int array -> int",
    );
    differ("a: int -> b: int -> int", "a: int * b: int -> int");
    differ("a: int -> (int -> int)", "a: int -> int -> int");
    differ("(int -> int)", "int -> int");
    differ("a: int -> int", "int -> int");
    differ("x: 'a -> 'b", "x: 'a -> 'a");
    differ("p: (int * string) -> int", "int * string -> int");
    assert_eq!(
        split_label("val fold<'T,'State> : folder: int -> int"),
        Some(("fold", Some("folder: int -> int")))
    );
    assert_eq!(
        split_label("member C.M3: a: int -> int"),
        Some(("M3", Some("a: int -> int")))
    );
    assert_eq!(split_label("member M3"), Some(("M3", None)));
}

/// The per-site table — a measurement, not a gate.
#[test]
#[ignore = "report generator"]
fn signature_report() {
    let run = run();
    for (i, site) in run.universe.sites.iter().enumerate() {
        let ours = label_head(&run.labels[i]).unwrap_or("—");
        let handler = run.hovers[i].as_deref().and_then(label_head).unwrap_or("—");
        println!(
            "{}\n    label   {ours}\n    handler {handler}\n    fcs     {:?}",
            site.label, run.fcs[i]
        );
    }
}
