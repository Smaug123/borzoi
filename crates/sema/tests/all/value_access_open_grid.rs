//! A generated, FCS-graded grid of **project accessibility through `open`**
//! (issue #57, `docs/assembly-module-open-plan.md` "Value-binding
//! accessibility").
//!
//! Two halves, both graded per reference site against `fcs-dump
//! uses-project` and its diagnostics:
//!
//! - **Values.** A `secret` binding under every accessibility (`private`,
//!   `internal`, `public`, none) in every container shape that restricts it
//!   differently — a plain module, a nested module, a `module private` at
//!   either depth, an `[<AutoOpen>]` module (public or `private`) and an
//!   `[<AutoOpen>]` nested module — referenced from inside its module, from
//!   the enclosing module, from a sibling module of the same file, and from a
//!   descendant and an unrelated namespace in the same file and in a later
//!   file, each through an `open` of its module, a qualified path, a bare name
//!   with no `open`, and an `open` of the enclosing namespace.
//! - **Type contestants.** A constructible type `X` under each accessibility
//!   next to an `[<AutoOpen>]` module's value `X`, in every file and block
//!   order, referenced bare from the namespace, a descendant and an unrelated
//!   namespace. FCS's `open` adds only the types accessible where it is
//!   written (`AddModuleOrNamespaceContentsToNameEnv` filters by
//!   `IsEntityAccessible`), and a type it adds takes the unqualified slot from
//!   a same-named value, so whether a `type private X` evicts the value
//!   depends on the reference site.
//!
//! The oracle is the diagnostic-aware one of
//! `resolve_qualified_path_access_gen_diff`: FCS error-recovers an
//! inaccessible use to its declaration (FS1092/1094 and kin), so a use-site
//! accessibility error means the only correct answer is to decline. Otherwise
//! certain-implies-exact: a committed target must be FCS's `(file, range)`,
//! and a site FCS leaves unbound must not commit.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::common::{LineIndex, invoke_fcs_dump_project, parse_fcs_uses_project, temp_fs_tree};
use borzoi_cst::parser::{parse, parse_sig};
use borzoi_cst::syntax::{AstNode, ImplFile, SigFile};
use borzoi_sema::{
    AssemblyEnv, ProjectFile, Resolution, SourceFile, SyntaxRecovery, qualified_names,
    resolve_project_files,
};
use rowan::TextRange;

/// A probed reference: which generated file, the byte range FCS reports the
/// use at (the whole dotted path for a qualified reference, the identifier
/// otherwise), and a label naming the grid cell.
#[derive(Clone, Debug)]
struct Site {
    file: usize,
    range: TextRange,
    label: String,
}

/// A generated file under construction, recording probe sites as lines are
/// appended.
struct FileBuilder {
    index: usize,
    text: String,
}

impl FileBuilder {
    fn new(index: usize) -> Self {
        Self {
            index,
            text: String::new(),
        }
    }

    fn line(&mut self, line: &str) {
        self.text.push_str(line);
        self.text.push('\n');
    }

    /// Append `line` and record a probe at the last occurrence of `probe` in
    /// it.
    fn probe(&mut self, line: &str, probe: &str, label: String, sites: &mut Vec<Site>) {
        let at = line.rfind(probe).expect("probe text occurs in its line");
        let start = self.text.len() + at;
        self.line(line);
        sites.push(Site {
            file: self.index,
            range: TextRange::new(
                u32::try_from(start).unwrap().into(),
                u32::try_from(start + probe.len()).unwrap().into(),
            ),
            label,
        });
    }
}

// ---- the value half ---------------------------------------------------------

#[derive(Clone, Copy, Debug)]
enum Access {
    None,
    Private,
    Internal,
    Public,
}

impl Access {
    const ALL: [Access; 4] = [
        Access::None,
        Access::Private,
        Access::Internal,
        Access::Public,
    ];

    fn keyword(self) -> &'static str {
        match self {
            Access::None => "",
            Access::Private => "private ",
            Access::Internal => "internal ",
            Access::Public => "public ",
        }
    }

    fn tag(self) -> &'static str {
        match self {
            Access::None => "none",
            Access::Private => "private",
            Access::Internal => "internal",
            Access::Public => "public",
        }
    }
}

/// Where `secret` is declared, relative to the cell's namespace.
#[derive(Clone, Copy, Debug)]
enum Container {
    /// `module A = let secret`.
    Mod,
    /// `module private A = let secret`.
    PrivMod,
    /// `[<AutoOpen>] module A = let secret`.
    AutoOpen,
    /// `[<AutoOpen>] module private A = let secret`.
    PrivAutoOpen,
    /// `module A = module B = let secret`.
    Nested,
    /// `module A = module private B = let secret` (issue #43's value half: the
    /// value inherits `B`'s restriction).
    PrivNested,
    /// `module A = [<AutoOpen>] module B = let secret`.
    AutoOpenNested,
}

impl Container {
    const ALL: [Container; 7] = [
        Container::Mod,
        Container::PrivMod,
        Container::AutoOpen,
        Container::PrivAutoOpen,
        Container::Nested,
        Container::PrivNested,
        Container::AutoOpenNested,
    ];

    fn tag(self) -> &'static str {
        match self {
            Container::Mod => "mod",
            Container::PrivMod => "privMod",
            Container::AutoOpen => "autoOpen",
            Container::PrivAutoOpen => "privAutoOpen",
            Container::Nested => "nested",
            Container::PrivNested => "privNested",
            Container::AutoOpenNested => "autoOpenNested",
        }
    }

    /// The module path holding `secret`, relative to the cell's namespace.
    fn rel(self) -> &'static str {
        match self {
            Container::Mod | Container::PrivMod | Container::AutoOpen | Container::PrivAutoOpen => {
                "A"
            }
            Container::Nested | Container::PrivNested | Container::AutoOpenNested => "A.B",
        }
    }

    fn nested(self) -> bool {
        self.rel() == "A.B"
    }
}

/// The three routes a sibling site can take to `secret`, plus the namespace
/// `open` (which only an auto-open module answers).
fn sibling_sites(
    f: &mut FileBuilder,
    sites: &mut Vec<Site>,
    prefix: &str,
    open_path: &str,
    namespace: Option<&str>,
    label: &str,
) {
    f.line(&format!("module {prefix}Open ="));
    f.line(&format!("    open {open_path}"));
    f.probe(
        "    let v = secret",
        "secret",
        format!("{label}/open"),
        sites,
    );
    f.line(&format!("module {prefix}Qual ="));
    let qual = format!("{open_path}.secret");
    f.probe(
        &format!("    let v = {qual}"),
        &qual,
        format!("{label}/qualified"),
        sites,
    );
    f.line(&format!("module {prefix}Bare ="));
    f.probe(
        "    let v = secret",
        "secret",
        format!("{label}/bare"),
        sites,
    );
    if let Some(ns) = namespace {
        f.line(&format!("module {prefix}NsOpen ="));
        f.line(&format!("    open {ns}"));
        f.probe(
            "    let v = secret",
            "secret",
            format!("{label}/nsOpen"),
            sites,
        );
    }
}

/// One value cell: its definitions and same-file sites appended to `defs`, its
/// later-file sites to `later`.
fn value_cell(
    i: usize,
    access: Access,
    container: Container,
    defs: &mut FileBuilder,
    later: &mut FileBuilder,
    sites: &mut Vec<Site>,
) {
    let ns = format!("N{i}");
    let label = format!("{}/{}", container.tag(), access.tag());
    let acc = access.keyword();
    defs.line(&format!("namespace {ns}"));
    defs.line("");
    match container {
        Container::Mod | Container::PrivMod | Container::AutoOpen | Container::PrivAutoOpen => {
            if matches!(container, Container::AutoOpen | Container::PrivAutoOpen) {
                defs.line("[<AutoOpen>]");
            }
            let private = matches!(container, Container::PrivMod | Container::PrivAutoOpen);
            defs.line(&format!(
                "module {}A =",
                if private { "private " } else { "" }
            ));
            defs.line(&format!("    let {acc}secret = 1"));
            defs.probe(
                "    let inside = secret",
                "secret",
                format!("{label}/inside/bare"),
                sites,
            );
        }
        Container::Nested | Container::PrivNested | Container::AutoOpenNested => {
            defs.line("module A =");
            if matches!(container, Container::AutoOpenNested) {
                defs.line("    [<AutoOpen>]");
            }
            let private = matches!(container, Container::PrivNested);
            defs.line(&format!(
                "    module {}B =",
                if private { "private " } else { "" }
            ));
            defs.line(&format!("        let {acc}secret = 1"));
            defs.probe(
                "        let inside = secret",
                "secret",
                format!("{label}/inside/bare"),
                sites,
            );
            // The enclosing module, after `B`.
            defs.line("    module POpen =");
            defs.line("        open B");
            defs.probe(
                "        let v = secret",
                "secret",
                format!("{label}/parent/open"),
                sites,
            );
            defs.line("    module PQual =");
            defs.probe(
                "        let v = B.secret",
                "B.secret",
                format!("{label}/parent/qualified"),
                sites,
            );
            defs.line("    module PBare =");
            defs.probe(
                "        let v = secret",
                "secret",
                format!("{label}/parent/bare"),
                sites,
            );
        }
    }
    debug_assert_eq!(container.nested(), container.rel() == "A.B");
    let rel = container.rel();
    let full = format!("{ns}.{rel}");
    sibling_sites(
        defs,
        sites,
        "S",
        rel,
        None,
        &format!("{label}/sameFileSibling"),
    );
    defs.line(&format!("namespace {ns}.Sub"));
    defs.line("");
    sibling_sites(
        defs,
        sites,
        "S",
        &full,
        Some(&ns),
        &format!("{label}/sameFileSubNs"),
    );
    defs.line(&format!("namespace O{i}"));
    defs.line("");
    sibling_sites(
        defs,
        sites,
        "S",
        &full,
        Some(&ns),
        &format!("{label}/sameFileOtherNs"),
    );

    later.line(&format!("namespace {ns}"));
    later.line("");
    sibling_sites(
        later,
        sites,
        "L",
        rel,
        None,
        &format!("{label}/laterSameNs"),
    );
    later.line(&format!("namespace {ns}.Sub"));
    later.line("");
    sibling_sites(
        later,
        sites,
        "L",
        &full,
        Some(&ns),
        &format!("{label}/laterSubNs"),
    );
    later.line(&format!("namespace O{i}"));
    later.line("");
    sibling_sites(
        later,
        sites,
        "L",
        &full,
        Some(&ns),
        &format!("{label}/laterOtherNs"),
    );
}

// ---- the type-contestant half ----------------------------------------------

/// Where the constructible type `X` sits relative to the `[<AutoOpen>]`
/// module `A` whose value `X` it may evict.
#[derive(Clone, Copy, Debug)]
enum TypePlacement {
    /// Same file, the type's block after `A`'s.
    SameFileAfter,
    /// Same file, the type's block before `A`'s.
    SameFileBefore,
    /// `A` in the first file, the type in the second.
    LaterFile,
    /// The type in the first file, `A` in the second.
    EarlierFile,
    /// The type inside a later `[<AutoOpen>]` module `B` of the same file.
    LaterFragment,
    /// The type inside an `[<AutoOpen>]` module `B` of the second file.
    LaterFileFragment,
}

impl TypePlacement {
    const ALL: [TypePlacement; 6] = [
        TypePlacement::SameFileAfter,
        TypePlacement::SameFileBefore,
        TypePlacement::LaterFile,
        TypePlacement::EarlierFile,
        TypePlacement::LaterFragment,
        TypePlacement::LaterFileFragment,
    ];

    fn tag(self) -> &'static str {
        match self {
            TypePlacement::SameFileAfter => "sameFileAfter",
            TypePlacement::SameFileBefore => "sameFileBefore",
            TypePlacement::LaterFile => "laterFile",
            TypePlacement::EarlierFile => "earlierFile",
            TypePlacement::LaterFragment => "laterFragment",
            TypePlacement::LaterFileFragment => "laterFileFragment",
        }
    }
}

fn auto_open_value(f: &mut FileBuilder) {
    f.line("[<AutoOpen>]");
    f.line("module A =");
    f.line("    let X = {| M = 1 |}");
}

fn constructible_type(f: &mut FileBuilder, acc: &str, fragment: bool) {
    if fragment {
        f.line("[<AutoOpen>]");
        f.line("module B =");
        f.line(&format!("    type {acc}X() ="));
        f.line("        static member M = 0");
    } else {
        f.line(&format!("type {acc}X() ="));
        f.line("    static member M = 0");
    }
}

/// A bare `X` and the head of `X.M`, as two probes.
fn probe_x(f: &mut FileBuilder, sites: &mut Vec<Site>, label: String) {
    f.probe("    let v = X", "X", format!("{label}/bare"), sites);
    f.probe("    let w = X.M", "X", format!("{label}/head"), sites);
}

/// The sites of a type cell: the namespace itself, a descendant namespace
/// (bare, and through an `open` of the namespace) and an unrelated namespace
/// that opens it, each probing a bare `X` and the head of `X.M` (member access
/// on the value, or the type's static member).
fn contestant_sites(
    f: &mut FileBuilder,
    sites: &mut Vec<Site>,
    ns: &str,
    i: usize,
    prefix: &str,
    label: &str,
) {
    f.line(&format!("namespace {ns}"));
    f.line("");
    f.line(&format!("module {prefix}Here ="));
    probe_x(f, sites, format!("{label}/sameNs"));
    f.line(&format!("namespace {ns}.Sub"));
    f.line("");
    f.line(&format!("module {prefix}Bare ="));
    probe_x(f, sites, format!("{label}/subNs/noOpen"));
    f.line(&format!("module {prefix}Open ="));
    f.line(&format!("    open {ns}"));
    probe_x(f, sites, format!("{label}/subNs/nsOpen"));
    f.line(&format!("namespace Q{i}"));
    f.line("");
    f.line(&format!("module {prefix}Open ="));
    f.line(&format!("    open {ns}"));
    probe_x(f, sites, format!("{label}/otherNs/nsOpen"));
}

/// One type cell over three files: the two definition files (with same-file
/// sites closing the second) and a later file of sites.
fn type_cell(
    i: usize,
    access: Access,
    placement: TypePlacement,
    files: &mut [FileBuilder; 3],
    sites: &mut Vec<Site>,
) {
    let ns = format!("T{i}");
    let label = format!("type/{}/{}", placement.tag(), access.tag());
    let acc = access.keyword();
    let [first, second, third] = files;
    first.line(&format!("namespace {ns}"));
    first.line("");
    let two_files = match placement {
        TypePlacement::SameFileAfter => {
            auto_open_value(first);
            constructible_type(first, acc, false);
            false
        }
        TypePlacement::SameFileBefore => {
            constructible_type(first, acc, false);
            auto_open_value(first);
            false
        }
        TypePlacement::LaterFragment => {
            auto_open_value(first);
            constructible_type(first, acc, true);
            false
        }
        TypePlacement::LaterFile
        | TypePlacement::EarlierFile
        | TypePlacement::LaterFileFragment => {
            second.line(&format!("namespace {ns}"));
            second.line("");
            match placement {
                TypePlacement::LaterFile => {
                    auto_open_value(first);
                    constructible_type(second, acc, false);
                }
                TypePlacement::EarlierFile => {
                    constructible_type(first, acc, false);
                    auto_open_value(second);
                }
                _ => {
                    auto_open_value(first);
                    constructible_type(second, acc, true);
                }
            }
            true
        }
    };
    // Same-file sites follow the last definition; later-file sites get a file
    // of their own.
    let last_def = if two_files { second } else { first };
    contestant_sites(last_def, sites, &ns, i, "S", &format!("{label}/sameFile"));
    contestant_sites(third, sites, &ns, i, "L", &format!("{label}/laterFile"));
}

// ---- the signature half -----------------------------------------------------

/// How a signature file declares the implementation's public `secret`.
#[derive(Clone, Copy, Debug)]
enum SigAccess {
    /// `val secret`, `val private secret`, `val internal secret`.
    Declared(Access),
    /// Not declared at all.
    Hidden,
}

impl SigAccess {
    const ALL: [SigAccess; 4] = [
        SigAccess::Declared(Access::None),
        SigAccess::Declared(Access::Private),
        SigAccess::Declared(Access::Internal),
        SigAccess::Hidden,
    ];

    fn tag(self) -> &'static str {
        match self {
            SigAccess::Declared(a) => a.tag(),
            SigAccess::Hidden => "hidden",
        }
    }
}

/// One signature cell: the signature and implementation of `secret` in a plain
/// or nested module, with every site in a later file (the implementation file
/// itself sees the implementation, not the signature).
fn sig_cell(
    i: usize,
    access: SigAccess,
    nested: bool,
    sig: &mut FileBuilder,
    imp: &mut FileBuilder,
    later: &mut FileBuilder,
    sites: &mut Vec<Site>,
) {
    let ns = format!("N{i}");
    let label = format!(
        "sig/{}/{}",
        if nested { "nested" } else { "mod" },
        access.tag()
    );
    let declared = match access {
        SigAccess::Declared(a) => Some(format!("val {}secret: int", a.keyword())),
        SigAccess::Hidden => None,
    };
    for f in [&mut *sig, &mut *imp] {
        f.line(&format!("namespace {ns}"));
        f.line("");
        f.line("module A =");
    }
    let indent = if nested {
        sig.line("    module B =");
        imp.line("    module B =");
        "        "
    } else {
        "    "
    };
    sig.line(&format!("{indent}val filler: int"));
    if let Some(declared) = &declared {
        sig.line(&format!("{indent}{declared}"));
    }
    imp.line(&format!("{indent}let filler = 0"));
    imp.line(&format!("{indent}let secret = 1"));
    let rel = if nested { "A.B" } else { "A" };
    let full = format!("{ns}.{rel}");
    later.line(&format!("namespace {ns}"));
    later.line("");
    sibling_sites(
        later,
        sites,
        "L",
        rel,
        None,
        &format!("{label}/laterSameNs"),
    );
    later.line(&format!("namespace {ns}.Sub"));
    later.line("");
    sibling_sites(
        later,
        sites,
        "L",
        &full,
        Some(&ns),
        &format!("{label}/laterSubNs"),
    );
    later.line(&format!("namespace O{i}"));
    later.line("");
    sibling_sites(
        later,
        sites,
        "L",
        &full,
        Some(&ns),
        &format!("{label}/laterOtherNs"),
    );
}

// ---- grading ----------------------------------------------------------------

/// FCS's use-site accessibility errors (type, union case or field, value,
/// member "not accessible").
fn is_accessibility_error(number: i64) -> bool {
    matches!(number, 491 | 1092 | 1093 | 1094)
}

/// Every error diagnostic FCS reported, as `(file name, byte range, number)`.
fn error_diagnostics(
    json: &str,
    sources: &[(PathBuf, String)],
) -> Vec<(PathBuf, usize, usize, i64)> {
    let dump: serde_json::Value = serde_json::from_str(json).expect("uses-project JSON");
    let mut out = Vec::new();
    for file in dump["Files"].as_array().expect("Files array") {
        for d in file["Diagnostics"].as_array().into_iter().flatten() {
            if d["Severity"] != "Error" {
                continue;
            }
            let number = d["ErrorNumber"].as_i64().unwrap_or(-1);
            let range = &d["Range"];
            let name = PathBuf::from(
                Path::new(range["File"].as_str().expect("diagnostic file"))
                    .file_name()
                    .expect("file name"),
            );
            let Some((_, src)) = sources
                .iter()
                .find(|(p, _)| p.file_name() == Some(name.as_os_str()))
            else {
                continue;
            };
            let idx = LineIndex::new(src);
            let pos = |p: &serde_json::Value| {
                idx.offset(
                    u32::try_from(p["Line"].as_u64().unwrap()).unwrap(),
                    u32::try_from(p["Col"].as_u64().unwrap()).unwrap(),
                )
            };
            out.push((name, pos(&range["Start"]), pos(&range["End"]), number));
        }
    }
    out
}

/// What FCS says at one site.
#[derive(Clone, Debug, PartialEq, Eq)]
enum FcsVerdict {
    /// Resolved, cleanly, to a declaration in `(file index, range)`.
    Resolved(usize, TextRange),
    /// A use-site accessibility error: inaccessible here.
    Inaccessible,
    /// No use recorded at the site (unbound), or an error at the site.
    Rejected,
}

/// What we say at one site.
#[derive(Clone, Debug, PartialEq, Eq)]
enum OurVerdict {
    Committed(usize, TextRange),
    Declined,
}

struct Graded {
    label: String,
    fcs: FcsVerdict,
    ours: OurVerdict,
}

/// Fold `files` (Compile order, `(relative name, source)`; a `.fsi` is a
/// signature) as one project, both through FCS and through sema, and grade
/// every site.
fn grade(files: &[(&str, String)], sites: &[Site]) -> Vec<Graded> {
    let tree: Vec<(&str, &str)> = files.iter().map(|(n, s)| (*n, s.as_str())).collect();
    let (root, written) = temp_fs_tree("vaog", &tree);
    let paths: Vec<&Path> = written.iter().map(|(p, _)| p.as_path()).collect();
    let json = invoke_fcs_dump_project(&paths);
    let fcs = parse_fcs_uses_project(&json, &written);
    let errors = error_diagnostics(&json, &written);
    let parsed: Vec<(SourceFile, SyntaxRecovery)> = files
        .iter()
        .map(|(name, src)| {
            if name.ends_with(".fsi") {
                let p = parse_sig(src);
                assert!(p.errors.is_empty(), "{name}: {:?}\n{src}", p.errors);
                let recovery = SyntaxRecovery::of(&p);
                (
                    SourceFile::Sig(SigFile::cast(p.root).expect("sig file")),
                    recovery,
                )
            } else {
                let p = parse(src);
                assert!(p.errors.is_empty(), "{name}: {:?}\n{src}", p.errors);
                let recovery = SyntaxRecovery::of(&p);
                (
                    SourceFile::Impl(ImplFile::cast(p.root).expect("impl file")),
                    recovery,
                )
            }
        })
        .collect();
    let disk: Vec<PathBuf> = written.iter().map(|(p, _)| p.clone()).collect();
    let sources: Vec<SourceFile> = parsed.iter().map(|(f, _)| f.clone()).collect();
    let qnofs = qualified_names(&sources, &disk);
    let project: Vec<ProjectFile> = parsed
        .into_iter()
        .zip(qnofs)
        .map(|((file, recovery), qnof)| ProjectFile::new(file, qnof, recovery))
        .collect();
    let proj = resolve_project_files(&project, &AssemblyEnv::default());
    let _ = std::fs::remove_dir_all(&root);
    let index_of = |path: &Path| {
        written
            .iter()
            .position(|(p, _)| p.file_name() == path.file_name())
            .expect("declaration in a generated file")
    };

    sites
        .iter()
        .map(|site| {
            let path = &written[site.file].0;
            let start = usize::from(site.range.start());
            let end = usize::from(site.range.end());
            let here: Vec<i64> = errors
                .iter()
                .filter(|(f, s, e, _)| {
                    path.file_name() == Some(f.as_os_str()) && *s < end && start < *e
                })
                .map(|(_, _, _, n)| *n)
                .collect();
            let decl = fcs
                .iter()
                .find(|f| f.path.file_name() == path.file_name())
                .and_then(|f| {
                    f.uses
                        .iter()
                        .find(|u| u.start == start && u.end == end && !u.is_from_definition)
                })
                .and_then(|u| u.decl.as_ref());
            let fcs = if here.iter().any(|n| is_accessibility_error(*n)) {
                FcsVerdict::Inaccessible
            } else if !here.is_empty() {
                FcsVerdict::Rejected
            } else {
                match decl {
                    Some(d) => FcsVerdict::Resolved(
                        index_of(&d.file),
                        TextRange::new(
                            u32::try_from(d.start).unwrap().into(),
                            u32::try_from(d.end).unwrap().into(),
                        ),
                    ),
                    None => FcsVerdict::Rejected,
                }
            };
            let res = proj.file(site.file).resolution_at(site.range);
            let ours = match res {
                Some(r @ Resolution::Item(_)) => {
                    proj.item_def(r).map_or(OurVerdict::Declined, |(f, def)| {
                        OurVerdict::Committed(f, def.range)
                    })
                }
                Some(r @ Resolution::Local(_)) => proj
                    .file(site.file)
                    .resolved_def(r)
                    .map_or(OurVerdict::Declined, |def| {
                        OurVerdict::Committed(site.file, def.range)
                    }),
                _ => OurVerdict::Declined,
            };
            Graded {
                label: site.label.clone(),
                fcs,
                ours,
            }
        })
        .collect()
}

/// A site's verdict pair is wrong when we commit anything FCS does not
/// resolve to exactly that declaration.
fn is_wrong(g: &Graded) -> bool {
    match (&g.fcs, &g.ours) {
        (_, OurVerdict::Declined) => false,
        (FcsVerdict::Resolved(f, r), OurVerdict::Committed(of, or)) => (f, r) != (of, or),
        (FcsVerdict::Inaccessible | FcsVerdict::Rejected, OurVerdict::Committed(..)) => true,
    }
}

fn census(graded: &[Graded]) -> String {
    let mut by: BTreeMap<String, usize> = BTreeMap::new();
    for g in graded {
        let fcs = match g.fcs {
            FcsVerdict::Resolved(..) => "resolved",
            FcsVerdict::Inaccessible => "inaccessible",
            FcsVerdict::Rejected => "rejected",
        };
        let ours = match (&g.fcs, &g.ours) {
            (_, OurVerdict::Declined) => "declined",
            _ if is_wrong(g) => "WRONG",
            _ => "agreed",
        };
        *by.entry(format!("fcs {fcs:<12} ours {ours}")).or_default() += 1;
    }
    by.into_iter()
        .map(|(k, n)| format!("  {k}: {n}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn report(graded: &[Graded]) -> String {
    graded
        .iter()
        .map(|g| format!("  {:<60} fcs {:?} | ours {:?}", g.label, g.fcs, g.ours))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Assert that the sites committing a target FCS does not bind are exactly
/// `known` — a pin, so a site that starts committing wrongly fails, and so
/// does one a fix corrects until it leaves the list.
fn assert_wrong_commits_are(what: &str, graded: &[Graded], known: &[&str]) {
    eprintln!("{what} census:\n{}", census(graded));
    if std::env::var_os("BORZOI_GRID_REPORT").is_some() {
        eprintln!("{what} cells:\n{}", report(graded));
    }
    let wrong: Vec<&Graded> = graded
        .iter()
        .filter(|g| is_wrong(g) && !known.contains(&g.label.as_str()))
        .collect();
    assert!(
        wrong.is_empty(),
        "{} {what} site(s) commit a target FCS does not bind there:\n{}",
        wrong.len(),
        wrong
            .iter()
            .map(|g| format!("  [{}] FCS {:?} | we gave {:?}", g.label, g.fcs, g.ours))
            .collect::<Vec<_>>()
            .join("\n")
    );
    let corrected: Vec<&&str> = known
        .iter()
        .filter(|label| !graded.iter().any(|g| g.label == **label && is_wrong(g)))
        .collect();
    assert!(
        corrected.is_empty(),
        "{what} sites pinned as wrong no longer are; remove them from the pin:\n{corrected:#?}"
    );
}

fn assert_no_wrong_commits(what: &str, graded: &[Graded]) {
    assert_wrong_commits_are(what, graded, &[]);
}

/// The type grid's sites that commit wrongly through a defect outside this
/// grid's subject: a dotted head `X.M` written in a later `namespace` block of
/// the file declaring the type `X` commits that type, as if the earlier
/// block's types were in lexical scope. FCS starts each block afresh: a block
/// of the same namespace sees the type only through the implicit `open` of
/// its namespace, which folds the `[<AutoOpen>]` module's value `X` over it,
/// and a block of a descendant namespace does not see it at all.
const SAME_FILE_LATER_BLOCK_HEADS: &[&str] = &[
    "type/sameFileAfter/none/sameFile/sameNs/head",
    "type/sameFileAfter/none/sameFile/subNs/noOpen/head",
    "type/sameFileAfter/private/sameFile/sameNs/head",
    "type/sameFileAfter/private/sameFile/subNs/noOpen/head",
    "type/sameFileAfter/internal/sameFile/sameNs/head",
    "type/sameFileAfter/internal/sameFile/subNs/noOpen/head",
    "type/sameFileBefore/none/sameFile/sameNs/head",
    "type/sameFileBefore/none/sameFile/subNs/noOpen/head",
    "type/sameFileBefore/private/sameFile/sameNs/head",
    "type/sameFileBefore/private/sameFile/subNs/noOpen/head",
    "type/sameFileBefore/internal/sameFile/sameNs/head",
    "type/sameFileBefore/internal/sameFile/subNs/noOpen/head",
    "type/laterFile/none/sameFile/subNs/noOpen/head",
    "type/laterFile/private/sameFile/subNs/noOpen/head",
    "type/laterFile/internal/sameFile/subNs/noOpen/head",
];

#[test]
fn value_accessibility_through_open_agrees_with_fcs() {
    let mut defs = FileBuilder::new(0);
    let mut later = FileBuilder::new(1);
    let mut sites = Vec::new();
    let mut i = 0;
    for container in Container::ALL {
        for access in Access::ALL {
            value_cell(i, access, container, &mut defs, &mut later, &mut sites);
            i += 1;
        }
    }
    let graded = grade(&[("defs.fs", defs.text), ("later.fs", later.text)], &sites);

    // Non-vacuity: every arm of the oracle is exercised, and agreement is
    // reached across files.
    let count = |p: &dyn Fn(&Graded) -> bool| graded.iter().filter(|g| p(g)).count();
    assert!(count(&|g| matches!(g.fcs, FcsVerdict::Inaccessible)) > 0);
    assert!(count(&|g| matches!(g.fcs, FcsVerdict::Rejected)) > 0);
    assert!(
        count(&|g| matches!(
            (&g.fcs, &g.ours),
            (FcsVerdict::Resolved(0, _), OurVerdict::Committed(0, _))
        ) && sites.iter().any(|s| s.label == g.label && s.file == 1))
            > 0,
        "no later-file site agreed on a first-file declaration"
    );
    assert_no_wrong_commits("value", &graded);
}

#[test]
fn type_contestant_accessibility_agrees_with_fcs() {
    let mut files = [
        FileBuilder::new(0),
        FileBuilder::new(1),
        FileBuilder::new(2),
    ];
    let mut sites = Vec::new();
    let mut i = 0;
    for placement in TypePlacement::ALL {
        for access in [Access::None, Access::Private, Access::Internal] {
            type_cell(i, access, placement, &mut files, &mut sites);
            i += 1;
        }
    }
    let [a, b, c] = files;
    let graded = grade(
        &[("a.fs", a.text), ("b.fs", b.text), ("c.fs", c.text)],
        &sites,
    );
    let count = |p: &dyn Fn(&Graded) -> bool| graded.iter().filter(|g| p(g)).count();
    assert!(count(&|g| matches!(g.fcs, FcsVerdict::Resolved(..))) > 0);
    assert_wrong_commits_are("type contestant", &graded, SAME_FILE_LATER_BLOCK_HEADS);
}

#[test]
fn signature_restricted_values_agree_with_fcs() {
    let mut sig = FileBuilder::new(0);
    let mut imp = FileBuilder::new(1);
    let mut later = FileBuilder::new(2);
    let mut sites = Vec::new();
    let mut i = 0;
    for nested in [false, true] {
        for access in SigAccess::ALL {
            sig_cell(
                i, access, nested, &mut sig, &mut imp, &mut later, &mut sites,
            );
            i += 1;
        }
    }
    let graded = grade(
        &[
            ("defs.fsi", sig.text),
            ("defs.fs", imp.text),
            ("later.fs", later.text),
        ],
        &sites,
    );
    let count = |p: &dyn Fn(&Graded) -> bool| graded.iter().filter(|g| p(g)).count();
    assert!(count(&|g| matches!(g.fcs, FcsVerdict::Resolved(..))) > 0);
    assert!(count(&|g| !matches!(g.fcs, FcsVerdict::Resolved(..))) > 0);
    assert_no_wrong_commits("signature", &graded);
}
