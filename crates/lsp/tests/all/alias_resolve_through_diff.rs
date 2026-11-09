//! The **multi-DLL alias differential**: a consumer naming referenced
//! assemblies' F# type abbreviations, resolved through the LSP's runtime chain
//! and through FCS, under reference sets that vary the hazards an alias's
//! resolve-through meets.
//!
//! An abbreviation (`type WidgetX = AtTarget.Widget`) is pickle-only: no
//! TypeDef carries it, and its target is a CCU-qualified reference into
//! whichever referenced assembly declares it. So reading `AlLib.WidgetX.Make`
//! means trusting that the referenced set names that CCU once, that nothing
//! else contests the alias's own full name, that no companion module reads the
//! tail first, and so on. Each of those was found by review, one round at a
//! time (#142, #158); this grid makes them inputs instead.
//!
//! The grid crosses
//!
//! - the **alias**: of a class, a union and a record, declared in the alias's
//!   own assembly or in another one; an alias of an alias; one with a
//!   `[<CompilationRepresentation(ModuleSuffix)>]` companion module (which
//!   FCS reads `Alias.Make` from ahead of the target's static when the alias
//!   is reached through an `open`, and not when it is written in full); one
//!   beside a generic type of its name; and aliases nested in a module
//!   ([`ALIASES`]);
//! - the **use**: a type annotation, a static member tail, a constructor
//!   call, the bare name as a value, a union case, a case pattern and an
//!   instance member reached through an annotated parameter — each fully
//!   qualified and, for the annotation and the static, shortened by an `open`
//!   ([`Use`]);
//! - the **reference set** ([`configs`]): the plain set in both orders; a
//!   second assembly declaring a real type at an alias's full name, and a real
//!   type inside a module both assemblies declare (merged parent modules), in
//!   both orders; a module both declare without colliding; a second assembly
//!   declaring the alias *targets'* full names; a byte-identical sibling of
//!   the target assembly and of the alias assembly; an assembly with the
//!   target's simple name and no public types (a rootless projection) at a
//!   lower, equal and higher version; the target assembly absent; an assembly
//!   the projector cannot read; and the alias assembly built without F#
//!   signature data.
//!
//! Our side is what the LSP serves: the env is built by
//! [`build_env_from_dll_paths`] (version unification, skip attribution and
//! all), the project is folded by `resolve_project_files` and inferred by
//! `infer_project`, and each segment of each written path is read through
//! [`served_resolution_with_range`] — the rule go-to-definition answers by.
//! FCS checks the same files against **exactly** the references the env was
//! built from (`OracleRefScope::Exclusive`), so a name only FCS can see cannot
//! pass as a decline.
//!
//! Four checks ride on every run:
//!
//! - **certain-implies-exact**: where we serve an answer, FCS records the
//!   same symbol — assembly, full name and kind — over the path ending there,
//!   save the sites pinned in [`KNOWN_WRONG`] against the issue tracking each;
//! - **must-commit controls** ([`MUST_COMMIT`]), so the grid cannot pass by
//!   declining everything;
//! - **premises**: under the plain set FCS checks every use the fixture means
//!   to be valid, and reads `Alias.Make` from the companion module where there
//!   is one and from the target otherwise, so a fixture that does not set up
//!   the question fails rather than grading another one;
//! - an exact **manifest** of both answers per segment
//!   (`tests/manifests/alias_resolve_through_diff.txt`).
//!
//! An assembly the projector cannot read is simulated by handing the chain a
//! path it cannot open where FCS is handed the real file: that is the state a
//! projection failure leaves the env in (`SkipCause::UnknownIdentity`), with
//! the oracle still able to read what we could not.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use borzoi::assembly_cache::AssemblyCache;
use borzoi::handlers::served_resolution_with_range;
use borzoi::semantic::build_env_from_dll_paths;
use borzoi_cst::parser::parse;
use borzoi_cst::syntax::{AstNode, ImplFile};
use borzoi_oracle_harness::BoundedCommand;
use borzoi_oracle_harness::manifest::{Manifest, UPDATE_ENV, check};
use borzoi_sema::test_support::{assembly_full_name_agrees, certified_expected};
use borzoi_sema::{
    AssemblyEnv, InferredFile, ProjectFile, Resolution, ResolvedFile, SourceFile, SyntaxRecovery,
    infer_project, qualified_names, resolve_project_files,
};
use rowan::{TextRange, TextSize};
use tempfile::TempDir;

use crate::common::{
    FileUses, NormalisedProjectUse, OracleRefScope, ensure_fsharp_core_dll,
    ensure_system_runtime_dll, invoke_fcs_dump_project_with_refs, parse_fcs_uses_project,
};

// ---- the fixture assemblies ------------------------------------------------

/// The assembly declaring the cross-assembly alias targets.
const TARGET_ASM: &str = "SemaAliasTarget";
/// The assembly declaring the aliases (and their same-assembly targets).
const LIB_ASM: &str = "SemaAliasLib";
/// [`LIB_ASM`]'s source, built without F# signature data.
const LIB_IL_ASM: &str = "SemaAliasLibIl";
/// Real types at aliases' full names, and a real type inside the module
/// [`LIB_ASM`] nests aliases in.
const COLLIDE_ASM: &str = "SemaAliasCollide";
/// The module [`LIB_ASM`] nests aliases in, declared again with nothing in it
/// that collides.
const MERGE_ASM: &str = "SemaAliasMerge";
/// Real types at the alias *targets'* full names.
const TWIN_ASM: &str = "SemaAliasTargetTwin";

/// What an alias ultimately names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Target {
    /// `AtTarget.Widget`, in [`TARGET_ASM`]: a class.
    Widget,
    /// `AtTarget.Shape`: a union.
    Shape,
    /// `AtTarget.Point`: a record.
    Point,
    /// `AlLib.LWidget`, beside the alias: a class.
    LWidget,
    /// `AlLib.LShape`: a union.
    LShape,
    /// `AlLib.LPoint`: a record.
    LPoint,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shape {
    Class,
    Union,
    Record,
}

impl Target {
    fn written(self) -> &'static str {
        match self {
            Target::Widget => "AtTarget.Widget",
            Target::Shape => "AtTarget.Shape",
            Target::Point => "AtTarget.Point",
            Target::LWidget => "LWidget",
            Target::LShape => "LShape",
            Target::LPoint => "LPoint",
        }
    }

    /// The target's assembly and full name, as FCS renders them.
    fn identity(self) -> (&'static str, &'static str) {
        match self {
            Target::Widget => (TARGET_ASM, "AtTarget.Widget"),
            Target::Shape => (TARGET_ASM, "AtTarget.Shape"),
            Target::Point => (TARGET_ASM, "AtTarget.Point"),
            Target::LWidget => (LIB_ASM, "AlLib.LWidget"),
            Target::LShape => (LIB_ASM, "AlLib.LShape"),
            Target::LPoint => (LIB_ASM, "AlLib.LPoint"),
        }
    }

    fn shape(self) -> Shape {
        match self {
            Target::Widget | Target::LWidget => Shape::Class,
            Target::Shape | Target::LShape => Shape::Union,
            Target::Point | Target::LPoint => Shape::Record,
        }
    }

    /// The nullary case a union target declares.
    fn case(self) -> Option<&'static str> {
        match self {
            Target::Shape => Some("Dot"),
            Target::LShape => Some("LDot"),
            _ => None,
        }
    }
}

/// What an alias abbreviates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Aliased {
    Type(Target),
    /// Another alias of [`ALIASES`], by name.
    Alias(&'static str),
}

/// One alias the alias assembly declares.
#[derive(Clone, Copy, Debug)]
struct Alias {
    name: &'static str,
    /// The module of `AlLib` it is declared in, if any.
    holder: Option<&'static str>,
    aliased: Aliased,
    /// With a `[<CompilationRepresentation(ModuleSuffix)>]` module of its name
    /// declaring `Make`.
    companion: bool,
    /// Beside a generic record of its name.
    overloaded: bool,
}

const fn alias(name: &'static str, aliased: Aliased) -> Alias {
    Alias {
        name,
        holder: None,
        aliased,
        companion: false,
        overloaded: false,
    }
}

const fn nested(name: &'static str, target: Target) -> Alias {
    Alias {
        name,
        holder: Some("Nest"),
        aliased: Aliased::Type(target),
        companion: false,
        overloaded: false,
    }
}

const fn companion(name: &'static str, target: Target) -> Alias {
    Alias {
        name,
        holder: None,
        aliased: Aliased::Type(target),
        companion: true,
        overloaded: false,
    }
}

/// Every alias the alias assembly declares.
const ALIASES: &[Alias] = &[
    alias("WidgetX", Aliased::Type(Target::Widget)),
    alias("ShapeX", Aliased::Type(Target::Shape)),
    alias("PointX", Aliased::Type(Target::Point)),
    alias("WidgetL", Aliased::Type(Target::LWidget)),
    alias("ShapeL", Aliased::Type(Target::LShape)),
    alias("PointL", Aliased::Type(Target::LPoint)),
    alias("WidgetChain", Aliased::Alias("WidgetX")),
    alias("ShapeChain", Aliased::Alias("ShapeL")),
    companion("WidgetComp", Target::LWidget),
    companion("WidgetCompX", Target::Widget),
    Alias {
        name: "WidgetOver",
        holder: None,
        aliased: Aliased::Type(Target::LWidget),
        companion: false,
        overloaded: true,
    },
    // [`COLLIDE_ASM`] declares a real class at this full name.
    alias("WidgetHit", Aliased::Type(Target::Widget)),
    // [`COLLIDE_ASM`] declares a real class at `AlLib.Nest.WidgetN`.
    nested("WidgetN", Target::LWidget),
    nested("WidgetNX", Target::Widget),
    nested("ShapeN", Target::Shape),
];

impl Alias {
    fn target(self) -> Target {
        match self.aliased {
            Aliased::Type(t) => t,
            Aliased::Alias(name) => ALIASES
                .iter()
                .find(|a| a.name == name)
                .expect("an alias of a declared alias")
                .target(),
        }
    }

    /// The alias's path below `AlLib`.
    fn short(self) -> String {
        match self.holder {
            Some(m) => format!("{m}.{}", self.name),
            None => self.name.to_string(),
        }
    }

    fn path(self) -> String {
        format!("AlLib.{}", self.short())
    }
}

const TARGET_SOURCE: &str = "namespace AtTarget

type Widget() =
    member _.Ping () = 1
    static member Make () = 1

type Shape =
    | Circle of float
    | Dot
    static member Make () = Dot

type Point =
    { X: int }
    member p.Ping () = p.X
    static member Make () = { X = 1 }
";

/// The alias assembly's source, from [`ALIASES`].
fn lib_source() -> String {
    let mut src = String::from(
        "namespace AlLib

type LWidget() =
    member _.Ping () = 2
    static member Make () = 2

type LShape =
    | LCircle of float
    | LDot
    static member Make () = LDot

type LPoint =
    { LX: int }
    member p.Ping () = p.LX
    static member Make () = { LX = 2 }

",
    );
    let decl = |a: &Alias| match a.aliased {
        Aliased::Type(t) => format!("type {} = {}\n", a.name, t.written()),
        Aliased::Alias(other) => format!("type {} = {other}\n", a.name),
    };
    for a in ALIASES.iter().filter(|a| a.holder.is_none()) {
        src.push_str(&decl(a));
        if a.companion {
            src.push_str(&format!(
                "[<CompilationRepresentation(CompilationRepresentationFlags.ModuleSuffix)>]\n\
                 module {} =\n    let Make () = 3\n",
                a.name
            ));
        }
        if a.overloaded {
            src.push_str(&format!("type {}<'T> = {{ OV: 'T }}\n", a.name));
        }
        src.push('\n');
    }
    let mut holders: Vec<&str> = ALIASES.iter().filter_map(|a| a.holder).collect();
    holders.dedup();
    for holder in holders {
        src.push_str(&format!("module {holder} =\n"));
        for a in ALIASES.iter().filter(|a| a.holder == Some(holder)) {
            src.push_str("    ");
            src.push_str(&decl(a));
        }
        src.push('\n');
    }
    src
}

const COLLIDE_SOURCE: &str = "namespace AlLib

type WidgetHit() =
    member _.Ping () = 9
    static member Make () = 9

module Nest =
    type WidgetN() =
        member _.Ping () = 9
        static member Make () = 9
";

const MERGE_SOURCE: &str = "namespace AlLib

module Nest =
    let extra = 8
";

const TWIN_SOURCE: &str = "namespace AtTarget

type Widget() =
    member _.Ping () = 7
    static member Make () = 7

type Shape =
    | Circle of float
    | Dot
    static member Make () = Dot

type Point =
    { X: int }
    member p.Ping () = p.X
    static member Make () = { X = 7 }
";

/// An assembly whose projection has no roots: nothing public.
const ROOTLESS_SOURCE: &str = "namespace AtTarget.Hidden

type internal Nothing() =
    member _.Value = 0
";

/// One fixture project.
struct Project {
    /// Its directory under the build root.
    dir: &'static str,
    assembly: &'static str,
    version: &'static str,
    source: String,
    /// References the target assembly's built DLL.
    references_target: bool,
    interface_data: bool,
}

/// The built fixture DLLs, by project directory.
struct Fixtures {
    _root: TempDir,
    dlls: BTreeMap<&'static str, PathBuf>,
}

impl Fixtures {
    fn dll(&self, dir: &str) -> &Path {
        &self.dlls[dir]
    }
}

fn fsproj(p: &Project, target: Option<&Path>) -> String {
    let reference = match target {
        Some(t) => format!(
            "  <ItemGroup>\n    <Reference Include=\"{}\" />\n  </ItemGroup>\n",
            t.display()
        ),
        None => String::new(),
    };
    let flags = if p.interface_data {
        ""
    } else {
        "    <OtherFlags>$(OtherFlags) --nointerfacedata</OtherFlags>\n"
    };
    format!(
        "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <PropertyGroup>\n    \
         <TargetFramework>net10.0</TargetFramework>\n    \
         <AssemblyName>{}</AssemblyName>\n    \
         <AssemblyVersion>{}</AssemblyVersion>\n    \
         <FileVersion>{}</FileVersion>\n    \
         <Deterministic>true</Deterministic>\n    \
         <GenerateDocumentationFile>false</GenerateDocumentationFile>\n    \
         <DebugType>none</DebugType>\n    \
         <DebugSymbols>false</DebugSymbols>\n{flags}  </PropertyGroup>\n  \
         <ItemGroup>\n    <Compile Include=\"Generated.fs\" />\n  </ItemGroup>\n\
         {reference}</Project>\n",
        p.assembly, p.version, p.version
    )
}

fn build(root: &Path, p: &Project, target: Option<&Path>) -> PathBuf {
    let dir = root.join(p.dir);
    fs::create_dir_all(&dir).unwrap();
    let proj = dir.join(format!("{}.fsproj", p.assembly));
    fs::write(&proj, fsproj(p, target)).unwrap();
    fs::write(dir.join("Generated.fs"), &p.source).unwrap();
    let out = dir.join("out");
    let mut cmd = std::process::Command::new("dotnet");
    cmd.args(["build", "-c", "Release", "--nologo", "-o"])
        .arg(&out)
        .arg(&proj);
    BoundedCommand::new(cmd)
        .timeout(std::time::Duration::from_secs(1800))
        .run_ok(format_args!("dotnet build alias fixture {}", p.dir));
    out.join(format!("{}.dll", p.assembly))
}

/// Every fixture, built once per test binary: the target assembly first (the
/// alias assemblies reference its DLL), then the rest in parallel.
fn fixtures() -> &'static Fixtures {
    static F: OnceLock<Fixtures> = OnceLock::new();
    F.get_or_init(|| {
        let root = TempDir::new().unwrap();
        let project = |dir, assembly, version, source: String, refs, idata| Project {
            dir,
            assembly,
            version,
            source,
            references_target: refs,
            interface_data: idata,
        };
        let target = build(
            root.path(),
            &project(
                "target",
                TARGET_ASM,
                "1.0.0.0",
                TARGET_SOURCE.into(),
                false,
                true,
            ),
            None,
        );
        let rest = [
            project("lib", LIB_ASM, "1.0.0.0", lib_source(), true, true),
            project("lib_il", LIB_IL_ASM, "1.0.0.0", lib_source(), true, false),
            project(
                "collide",
                COLLIDE_ASM,
                "1.0.0.0",
                COLLIDE_SOURCE.into(),
                false,
                true,
            ),
            project(
                "merge",
                MERGE_ASM,
                "1.0.0.0",
                MERGE_SOURCE.into(),
                false,
                true,
            ),
            project("twin", TWIN_ASM, "1.0.0.0", TWIN_SOURCE.into(), false, true),
            project(
                "rootless_lower",
                TARGET_ASM,
                "0.5.0.0",
                ROOTLESS_SOURCE.into(),
                false,
                true,
            ),
            project(
                "rootless_equal",
                TARGET_ASM,
                "1.0.0.0",
                ROOTLESS_SOURCE.into(),
                false,
                true,
            ),
            project(
                "rootless_higher",
                TARGET_ASM,
                "2.0.0.0",
                ROOTLESS_SOURCE.into(),
                false,
                true,
            ),
        ];
        let mut dlls: BTreeMap<&'static str, PathBuf> = std::thread::scope(|s| {
            let handles: Vec<_> = rest
                .iter()
                .map(|p| {
                    let root = root.path();
                    let target = target.as_path();
                    s.spawn(move || (p.dir, build(root, p, p.references_target.then_some(target))))
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        // Byte-identical siblings: the same file at a second path.
        for (dir, from) in [
            ("target_copy", target.clone()),
            ("lib_copy", dlls["lib"].clone()),
        ] {
            let to = root.path().join(dir);
            fs::create_dir_all(&to).unwrap();
            let dst = to.join(from.file_name().unwrap());
            fs::copy(&from, &dst).unwrap();
            dlls.insert(dir, dst);
        }
        dlls.insert("target", target);
        Fixtures { _root: root, dlls }
    })
}

// ---- the reference sets ----------------------------------------------------

/// How one reference reaches the two sides.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Seen {
    /// Both read it.
    Both,
    /// FCS reads it; the LSP chain is handed a path it cannot open, as when
    /// the projector fails on a DLL.
    OracleOnly,
}

/// One reference set: fixture directories in reference order, after
/// FSharp.Core and the framework.
struct Config {
    name: &'static str,
    refs: &'static [(&'static str, Seen)],
}

use Seen::{Both, OracleOnly};

fn configs() -> Vec<Config> {
    let c = |name, refs| Config { name, refs };
    vec![
        c("base", &[("target", Both), ("lib", Both)]),
        c("lib-first", &[("lib", Both), ("target", Both)]),
        c(
            "collide-after",
            &[("target", Both), ("lib", Both), ("collide", Both)],
        ),
        c(
            "collide-before",
            &[("collide", Both), ("target", Both), ("lib", Both)],
        ),
        c("merge", &[("target", Both), ("lib", Both), ("merge", Both)]),
        c(
            "twin-target",
            &[("target", Both), ("lib", Both), ("twin", Both)],
        ),
        c(
            "sibling-target",
            &[("target", Both), ("target_copy", Both), ("lib", Both)],
        ),
        c(
            "sibling-lib",
            &[("target", Both), ("lib", Both), ("lib_copy", Both)],
        ),
        c(
            "rootless-lower",
            &[("target", Both), ("lib", Both), ("rootless_lower", Both)],
        ),
        c(
            "rootless-equal",
            &[("target", Both), ("lib", Both), ("rootless_equal", Both)],
        ),
        c(
            "rootless-higher",
            &[("target", Both), ("lib", Both), ("rootless_higher", Both)],
        ),
        c(
            "skipped",
            &[("target", Both), ("lib", Both), ("collide", OracleOnly)],
        ),
        c("target-missing", &[("lib", Both)]),
        c("il-only", &[("target", Both), ("lib_il", Both)]),
    ]
}

// ---- the probes ------------------------------------------------------------

/// How a probe uses an alias.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Use {
    /// `let probe (x: AlLib.A) = ignore x`.
    Annot,
    /// The same after `open AlLib`.
    OpenAnnot,
    /// `let probe = AlLib.A.Make ()`.
    Static,
    /// The same after `open AlLib`.
    OpenStatic,
    /// `let probe = AlLib.A ()`.
    Ctor,
    /// `let probe = AlLib.A`.
    Bare,
    /// `let probe = AlLib.A.Dot`: unions only.
    Case,
    /// `match x with AlLib.A.Dot -> …`: unions only.
    Pattern,
    /// `let probe (x: AlLib.A) = x.Ping ()`: classes and records only.
    Instance,
}

impl Use {
    const ALL: [Use; 9] = [
        Use::Annot,
        Use::OpenAnnot,
        Use::Static,
        Use::OpenStatic,
        Use::Ctor,
        Use::Bare,
        Use::Case,
        Use::Pattern,
        Use::Instance,
    ];

    fn applies(self, shape: Shape) -> bool {
        match self {
            Use::Case | Use::Pattern => shape == Shape::Union,
            Use::Instance => shape != Shape::Union,
            _ => true,
        }
    }

    /// Whether the use is valid F# for a target of `shape`: what the fixture
    /// means FCS to check cleanly under the plain reference set.
    fn valid(self, shape: Shape) -> bool {
        match self {
            Use::Ctor => shape == Shape::Class,
            // A constructor as a first-class function: left ungraded as a
            // premise.
            Use::Bare => false,
            _ => self.applies(shape),
        }
    }
}

/// One probe file: the alias and use, the source, and the dotted paths whose
/// segments are graded.
struct Probe {
    alias: Alias,
    use_: Use,
    file: String,
    src: String,
    paths: Vec<TextRange>,
}

fn range(start: usize, end: usize) -> TextRange {
    TextRange::new(
        TextSize::from(u32::try_from(start).unwrap()),
        TextSize::from(u32::try_from(end).unwrap()),
    )
}

fn probes() -> Vec<Probe> {
    let mut out = Vec::new();
    for (k, a) in ALIASES.iter().enumerate() {
        let target = a.target();
        for use_ in Use::ALL {
            if !use_.applies(target.shape()) {
                continue;
            }
            let module = format!("AlProbe{k}{use_:?}");
            let mut src = format!("module {module}\n\n");
            let opened = matches!(use_, Use::OpenAnnot | Use::OpenStatic);
            if opened {
                src.push_str("open AlLib\n\n");
            }
            let written = if opened { a.short() } else { a.path() };
            let case = target.case().unwrap_or("");
            // The line, with `@` marking where each graded path starts and
            // `#` where it ends.
            let line = match use_ {
                Use::Annot | Use::OpenAnnot => format!("let probe (x: @{written}#) = ignore x"),
                Use::Static | Use::OpenStatic => format!("let probe = @{written}.Make# ()"),
                Use::Ctor => format!("let probe = @{written}# ()"),
                Use::Bare => format!("let probe = @{written}#"),
                Use::Case => format!("let probe = @{written}.{case}#"),
                Use::Pattern => {
                    format!(
                        "let probe (x: @{written}#) = match x with @{written}.{case}# -> 1 | _ -> 0"
                    )
                }
                Use::Instance => format!("let probe (x: @{written}#) = @x.Ping# ()"),
            };
            let mut paths = Vec::new();
            let mut start = None;
            for c in line.chars() {
                match c {
                    '@' => start = Some(src.len()),
                    '#' => paths.push(range(start.take().unwrap(), src.len())),
                    c => src.push(c),
                }
            }
            src.push('\n');
            out.push(Probe {
                alias: *a,
                use_,
                file: format!("{module}.fs"),
                src,
                paths,
            });
        }
    }
    out
}

/// Each dot-separated segment of the path at `span`.
fn segments(src: &str, span: TextRange) -> Vec<TextRange> {
    let start = usize::from(span.start());
    let text = &src[start..usize::from(span.end())];
    let mut out = Vec::new();
    let mut seg = 0;
    for (i, c) in text.char_indices() {
        if c == '.' {
            out.push(range(start + seg, start + i));
            seg = i + 1;
        }
    }
    out.push(range(start + seg, start + text.len()));
    out
}

// ---- grading ---------------------------------------------------------------

/// FCS's records for the segment `seg` of `path`: at exactly `seg` if FCS
/// has one there, else the longest one inside `path` ending where `seg` ends.
/// FCS records a qualified name over the path that reaches it
/// (`AlLib.WidgetX.Make`), where we record it at its own segment.
fn fcs_records(fu: &FileUses, path: TextRange, seg: TextRange) -> Vec<&NormalisedProjectUse> {
    let ours = seg;
    let inside: Vec<&NormalisedProjectUse> = fu
        .uses
        .iter()
        .filter(|u| {
            !u.is_from_definition
                && u.end == usize::from(ours.end())
                && u.start >= usize::from(path.start())
                && u.start <= usize::from(ours.start())
        })
        .collect();
    let exact: Vec<_> = inside
        .iter()
        .copied()
        .filter(|u| u.start == usize::from(ours.start()))
        .collect();
    if !exact.is_empty() {
        return exact;
    }
    let Some(longest) = inside.iter().map(|u| u.start).min() else {
        return Vec::new();
    };
    inside.into_iter().filter(|u| u.start == longest).collect()
}

/// FCS's record, rendered for the manifest.
fn render_fcs(u: Option<&NormalisedProjectUse>) -> String {
    let Some(u) = u else {
        return "-".to_string();
    };
    let kind = if u.is_constructor {
        "ctor"
    } else {
        u.kind.as_deref().unwrap_or("?")
    };
    let origin = match (&u.decl, &u.assembly) {
        (Some(_), _) => "local".to_string(),
        (None, Some(a)) => a.clone(),
        (None, None) => "?".to_string(),
    };
    format!("{origin}:{}:{kind}", u.full_name.as_deref().unwrap_or("?"))
}

/// Our resolution, rendered for a failure report.
fn render_ours(env: &AssemblyEnv, res: Resolution) -> String {
    match res {
        Resolution::Entity(h) => format!(
            "{}:{}:{}",
            env.entity(h).assembly.name,
            env.entity_full_name(h),
            if env.is_authoritative_module(h) {
                "module"
            } else {
                "type"
            }
        ),
        Resolution::Member { parent, idx } => format!(
            "{}:{}.{}:member",
            env.entity(parent).assembly.name,
            env.entity_full_name(parent),
            env.member_display_name(parent, idx)
        ),
        Resolution::UnionCase { union, case } => format!(
            "{}:{}.{}:unioncase",
            env.entity(union).assembly.name,
            env.entity_full_name(union),
            env.union_case_name(union, case)
        ),
        other => format!("{other:?}"),
    }
}

/// Whether our served `res` names exactly FCS's record `u`: the same
/// assembly, full name (or the structural declaration our resolution
/// certifies), and kind.
fn agrees(env: &AssemblyEnv, rf: &ResolvedFile, res: Resolution, u: &NormalisedProjectUse) -> bool {
    let names = |assembly: &str, ours: String| {
        u.decl.is_none()
            && u.assembly.as_deref() == Some(assembly)
            && u.full_name.as_deref().is_some_and(|full| {
                assembly_full_name_agrees(&ours, full)
                    || u.structural
                        .as_ref()
                        .and_then(|s| certified_expected(env, res, s))
                        .is_some_and(|c| assembly_full_name_agrees(&ours, &c))
            })
    };
    match res {
        Resolution::Local(_) => {
            let (Some(decl), Some(def)) = (&u.decl, rf.resolved_def(res)) else {
                return false;
            };
            (decl.start, decl.end) == (usize::from(def.range.start()), usize::from(def.range.end()))
        }
        Resolution::Entity(h) => {
            let module = env.is_authoritative_module(h);
            let kind = if u.is_constructor {
                !module
            } else {
                match u.kind.as_deref() {
                    Some("module") => module,
                    Some("type") => !module,
                    _ => false,
                }
            };
            let arity = u.is_constructor
                || u.generic_arity
                    .is_none_or(|n| n == env.entity(h).generic_parameters.len());
            kind && arity && names(&env.entity(h).assembly.name, env.entity_full_name(h))
        }
        Resolution::Member { parent, idx } => {
            !u.is_constructor
                && matches!(u.kind.as_deref(), Some("member" | "field"))
                && names(
                    &env.entity(parent).assembly.name,
                    format!(
                        "{}.{}",
                        env.entity_full_name(parent),
                        env.member_display_name(parent, idx)
                    ),
                )
        }
        Resolution::UnionCase { union, case } => {
            u.kind.as_deref() == Some("unioncase")
                && names(
                    &env.entity(union).assembly.name,
                    format!(
                        "{}.{}",
                        env.entity_full_name(union),
                        env.union_case_name(union, case)
                    ),
                )
        }
        // Nothing in a probe names another project file.
        Resolution::Item(_) | Resolution::EarlierFileType { .. } => false,
        Resolution::Deferred(_) | Resolution::Unresolved => false,
    }
}

/// One graded segment's outcome.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Verdict {
    Decline,
    Agree,
    /// We serve a qualifier FCS records nothing at, in a file FCS rejects:
    /// error recovery drops the records of a rejected path's qualifiers, so
    /// there is nothing to grade it against.
    Ungraded,
    Wrong(String),
}

/// The sites (`config alias use segment@path`) that must serve FCS's answer:
/// resolve-through to each target shape, in its own assembly and another, a
/// chain, a case tail, the opened companion, the overloaded name, the nested
/// aliases and the annotations under the plain set; and, under the hazard
/// sets, the readings the hazard leaves provable — another assembly's
/// collision does not poison an alias it does not touch, nor does a type at
/// the target's full name in an assembly the CCU does not name, a merged
/// parent the alias does not sit in, a duplicate of an assembly the alias does
/// not chase into, a unified-away lower version, or a target CCU that is
/// absent altogether.
const MUST_COMMIT: &[&str] = &[
    "base WidgetX Static Make@0",
    "base WidgetX OpenStatic Make@0",
    "base ShapeX Static Make@0",
    "base PointX Static Make@0",
    "base WidgetL Static Make@0",
    "base ShapeX Case Dot@0",
    "base ShapeL Case LDot@0",
    "base WidgetChain Static Make@0",
    "base ShapeChain Case LDot@0",
    "base WidgetComp OpenStatic Make@0",
    "base WidgetOver Static Make@0",
    "base WidgetN Static Make@0",
    "base WidgetNX Static Make@0",
    "base ShapeN Case Dot@0",
    "base WidgetX Annot WidgetX@0",
    "base WidgetN OpenAnnot WidgetN@0",
    "lib-first WidgetX Static Make@0",
    "collide-before WidgetX Static Make@0",
    "merge WidgetHit Static Make@0",
    "twin-target WidgetX Static Make@0",
    "twin-target WidgetN Static Make@0",
    "sibling-target WidgetL Static Make@0",
    "rootless-lower WidgetX Static Make@0",
    "rootless-equal WidgetL Static Make@0",
    "target-missing WidgetL Static Make@0",
];

/// Pre-existing wrong answers, each with the issue tracking it. A pinned site
/// that stops being wrong fails until its pin goes.
///
/// - #505: without F# signature data, FCS imports the alias assembly through
///   IL, where no `WidgetComp` exists (the companion module is the class
///   `WidgetCompModule`); we still name it by its F# source name.
/// - #514: with an assembly of the target's name that lacks the target, FCS's
///   `open AlLib` fails as a whole (FS0193) and adds nothing; we fold its
///   names.
const KNOWN_WRONG: &[(&str, &str)] = &[
    ("il-only WidgetComp Annot WidgetComp@0", "#505"),
    ("il-only WidgetComp Bare WidgetComp@0", "#505"),
    ("il-only WidgetComp Ctor WidgetComp@0", "#505"),
    ("il-only WidgetComp Instance WidgetComp@0", "#505"),
    ("il-only WidgetComp Static Make@0", "#505"),
    ("il-only WidgetCompX Annot WidgetCompX@0", "#505"),
    ("il-only WidgetCompX Bare WidgetCompX@0", "#505"),
    ("il-only WidgetCompX Ctor WidgetCompX@0", "#505"),
    ("il-only WidgetCompX Instance WidgetCompX@0", "#505"),
    ("il-only WidgetCompX Static Make@0", "#505"),
    ("rootless-higher PointL OpenAnnot PointL@0", "#514"),
    ("rootless-higher PointL OpenStatic Make@0", "#514"),
    ("rootless-higher ShapeChain OpenAnnot ShapeChain@0", "#514"),
    ("rootless-higher ShapeChain OpenStatic Make@0", "#514"),
    ("rootless-higher ShapeL OpenAnnot ShapeL@0", "#514"),
    ("rootless-higher ShapeL OpenStatic Make@0", "#514"),
    ("rootless-higher WidgetComp OpenAnnot WidgetComp@0", "#514"),
    ("rootless-higher WidgetComp OpenStatic Make@0", "#514"),
    ("rootless-higher WidgetCompX OpenStatic Make@0", "#514"),
    ("rootless-higher WidgetL OpenAnnot WidgetL@0", "#514"),
    ("rootless-higher WidgetL OpenStatic Make@0", "#514"),
    ("rootless-higher WidgetN OpenAnnot WidgetN@0", "#514"),
    ("rootless-higher WidgetOver OpenAnnot WidgetOver@0", "#514"),
    ("rootless-higher WidgetOver OpenStatic Make@0", "#514"),
];

fn project_files(written: &[(PathBuf, String)]) -> Vec<ProjectFile> {
    let srcs: Vec<SourceFile> = written
        .iter()
        .map(|(_, src)| {
            let p = parse(src);
            assert!(p.errors.is_empty(), "{src}\n{:?}", p.errors);
            SourceFile::Impl(ImplFile::cast(p.root).expect("impl file"))
        })
        .collect();
    let paths: Vec<PathBuf> = written.iter().map(|(p, _)| p.clone()).collect();
    let qnofs = qualified_names(&srcs, &paths);
    srcs.into_iter()
        .zip(qnofs)
        .map(|(file, qnof)| ProjectFile::new(file, qnof, SyntaxRecovery::Unretained))
        .collect()
}

/// FSharp.Core and the whole net10.0 reference pack: what every reference set
/// starts with, as a real project's does. A thinner framework leaves FCS
/// unable to check FSharp.Core itself (`BigInteger` lives in
/// `System.Runtime.Numerics`).
fn base_refs() -> Vec<PathBuf> {
    let runtime = ensure_system_runtime_dll();
    let pack = runtime
        .parent()
        .expect("System.Runtime.dll sits in a ref dir");
    let mut framework: Vec<PathBuf> = fs::read_dir(pack)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "dll"))
        .collect();
    framework.sort();
    let mut refs = vec![ensure_fsharp_core_dll()];
    refs.extend(framework);
    refs
}

/// Every alias, used every way, under every reference set: we serve FCS's
/// answer or decline, the controls serve it, FCS honours the fixture's
/// premises, and both answers are pinned.
#[test]
fn every_alias_use_agrees_with_fcs_under_every_reference_set() {
    let fixtures = fixtures();
    let probes = probes();
    let dir = TempDir::new().unwrap();
    let written: Vec<(PathBuf, String)> = probes
        .iter()
        .map(|p| {
            let path = dir.path().join(&p.file);
            fs::write(&path, &p.src).unwrap();
            (path, p.src.clone())
        })
        .collect();
    let files = project_files(&written);
    let paths: Vec<&Path> = written.iter().map(|(p, _)| p.as_path()).collect();
    let cache = AssemblyCache::disabled();
    let missing = dir.path().join("unreadable");

    let mut failures = Vec::new();
    let mut entries = Vec::new();
    let mut agreed: Vec<String> = Vec::new();
    let mut wrong_sites: Vec<String> = Vec::new();
    for config in configs() {
        // Our side: the chain is handed an unopenable path for an
        // oracle-only reference.
        let mut ours_refs = base_refs();
        let mut oracle_only = Vec::new();
        for (dir, seen) in config.refs {
            let dll = fixtures.dll(dir).to_path_buf();
            match seen {
                Both => ours_refs.push(dll),
                OracleOnly => {
                    ours_refs.push(missing.join(dll.file_name().unwrap()));
                    oracle_only.push(dll);
                }
            }
        }
        let (env, kept) = build_env_from_dll_paths(ours_refs.iter().map(PathBuf::as_path), &cache);
        // FCS reads exactly what the env was built from, plus what the chain
        // could not read, in reference order.
        let oracle_refs: Vec<PathBuf> = ours_refs
            .iter()
            .filter_map(|p| {
                if kept.contains(p) {
                    Some(p.clone())
                } else {
                    oracle_only
                        .iter()
                        .find(|o| o.file_name() == p.file_name() && p.starts_with(&missing))
                        .cloned()
                }
            })
            .collect();
        let refs: Vec<&Path> = oracle_refs.iter().map(PathBuf::as_path).collect();
        let json =
            invoke_fcs_dump_project_with_refs(&paths, &refs, OracleRefScope::Exclusive, &[], None);
        let fcs = parse_fcs_uses_project(&json, &written);
        let resolved = resolve_project_files(&files, &env);
        let inferred = infer_project(&files, &resolved, &env);

        for (idx, probe) in probes.iter().enumerate() {
            let a = probe.alias;
            let label = format!("{} {} {:?}", config.name, a.name, probe.use_);
            let fu = fcs
                .iter()
                .find(|f| f.path.file_name() == written[idx].0.file_name())
                .unwrap_or_else(|| panic!("no FCS uses for {label}"));
            let rf = resolved.file(idx);
            let inf: Option<&InferredFile> = inferred.file(idx);
            let clean = fu.errors.is_empty();

            if config.name == "base" {
                premises(probe, fu, &label, &mut failures);
            }

            let mut cells = Vec::new();
            for (p, &path) in probe.paths.iter().enumerate() {
                for seg in segments(&probe.src, path) {
                    let seg_text = &probe.src[usize::from(seg.start())..usize::from(seg.end())];
                    let site = format!("{label} {seg_text}@{p}");
                    let byte = usize::from(seg.start()) + usize::from(seg.len()) / 2;
                    let theirs = fcs_records(fu, path, seg);
                    let verdict = match served_resolution_with_range(rf, inf, byte) {
                        None | Some((_, Resolution::Deferred(_) | Resolution::Unresolved)) => {
                            Verdict::Decline
                        }
                        // Graded against FCS's record for the segment under
                        // the cursor, never the one the served range picks:
                        // an answer recorded over `WidgetX.Make` is what a
                        // cursor on `WidgetX` is shown, and must be FCS's
                        // reading of `AlLib.WidgetX`.
                        Some((at, res)) => {
                            let recs = &theirs;
                            let names: Vec<_> = recs.iter().map(|u| &u.full_name).collect();
                            if names.windows(2).any(|w| w[0] != w[1]) {
                                failures.push(format!(
                                    "  {site} [AMBIGUOUS] FCS records several symbols: {recs:?}"
                                ));
                            }
                            if recs.is_empty() && !clean && seg.end() != path.end() {
                                Verdict::Ungraded
                            } else if !path.contains_range(at) {
                                Verdict::Wrong(format!(
                                    "served {at:?} outside the path {path:?}: {}",
                                    render_ours(&env, res)
                                ))
                            } else if !recs.is_empty()
                                && recs.iter().all(|u| agrees(&env, rf, res, u))
                            {
                                Verdict::Agree
                            } else {
                                Verdict::Wrong(render_ours(&env, res))
                            }
                        }
                    };
                    let known = KNOWN_WRONG
                        .iter()
                        .find(|(s, _)| *s == site)
                        .map(|(_, i)| *i);
                    match (&verdict, known) {
                        (Verdict::Wrong(ours), None) => failures.push(format!(
                            "  {site} [WRONG]\n    FCS:  {}\n    ours: {ours}\n    errors: {:?}",
                            render_fcs(theirs.first().copied()),
                            fu.errors
                        )),
                        (Verdict::Wrong(_), Some(_)) => wrong_sites.push(site.clone()),
                        (_, Some(issue)) => failures.push(format!(
                            "  {site} [NOW RIGHT]: {issue} may be fixed; remove its pin"
                        )),
                        _ => {}
                    }
                    if verdict == Verdict::Agree {
                        agreed.push(site.clone());
                    }
                    let ours = match &verdict {
                        Verdict::Decline => "decline".to_string(),
                        Verdict::Agree => "agree".to_string(),
                        Verdict::Ungraded => "ungraded".to_string(),
                        Verdict::Wrong(o) => match known {
                            Some(issue) => format!("WRONG ({issue}) {o}"),
                            None => format!("WRONG {o}"),
                        },
                    };
                    cells.push(format!(
                        "{seg_text}={} ours {ours}",
                        render_fcs(theirs.first().copied())
                    ));
                }
            }
            entries.push(format!(
                "{label}\t{} | {}",
                if clean { "clean" } else { "erroring" },
                cells.join("; ")
            ));
        }
    }
    let missing_controls: Vec<&&str> = MUST_COMMIT
        .iter()
        .filter(|s| !agreed.iter().any(|a| a == **s))
        .collect();
    if !missing_controls.is_empty() {
        failures.push(format!(
            "  must-commit controls do not serve FCS's answer: {missing_controls:#?}"
        ));
    }
    if std::env::var_os("BORZOI_GRID_REPORT").is_some() || !failures.is_empty() {
        for e in &entries {
            eprintln!("cell: {e}");
        }
    }
    assert!(
        failures.is_empty(),
        "{} alias differential failure(s):\n{}",
        failures.len(),
        failures.join("\n")
    );
    let manifest = Manifest::from_entries(entries).expect("manifest entries");
    check(
        &Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/manifests/alias_resolve_through_diff.txt"),
        &manifest,
        &format!(
            "{UPDATE_ENV}=1 cargo test -p borzoi --test all \
             alias_resolve_through_diff::every_alias_use_agrees_with_fcs_under_every_reference_set"
        ),
    );
}

/// What the plain reference set must show for the fixture to pose the
/// question it is meant to: every valid use checks, and `Alias.Make` reads
/// the companion module only where the alias is reached through an `open` of
/// its namespace. Written in full (`AlLib.WidgetComp.Make`), the namespace is
/// searched for a type of the name before a module of it, so FCS reads the
/// target's static; through the `open`, the name environment's module of the
/// name answers first.
fn premises(probe: &Probe, fu: &FileUses, label: &str, failures: &mut Vec<String>) {
    let a = probe.alias;
    let target = a.target();
    if probe.use_.valid(target.shape()) && !fu.errors.is_empty() {
        failures.push(format!(
            "  {label} [PREMISE] a valid use does not check: {:?}\n{}",
            fu.errors, probe.src
        ));
    }
    if matches!(probe.use_, Use::Static | Use::OpenStatic) {
        let path = probe.paths[0];
        let make = segments(&probe.src, path).pop().unwrap();
        let read = fcs_records(fu, path, make);
        let (asm, full) = target.identity();
        let to_target = read.iter().any(|u| {
            u.assembly.as_deref() == Some(asm)
                && u.full_name.as_deref() == Some(&format!("{full}.Make"))
        });
        let to_companion = a.companion && probe.use_ == Use::OpenStatic;
        if to_target == to_companion {
            failures.push(format!(
                "  {label} [PREMISE] `Make` reads {} (companion module expected: {to_companion})",
                render_fcs(read.first().copied()),
            ));
        }
    }
}
