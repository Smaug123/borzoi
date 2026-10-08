//! A model-based state-machine test of the server's **incremental state**: the
//! document store, the per-file and per-project parse caches, the resolved-fold
//! caches, the assembly envs, the project-evaluation memo, and the file-watch
//! invalidation that is meant to keep them all honest.
//!
//! The reference implementation is a **fresh session**. After a generated
//! sequence of client actions — open, edit, close and save buffers; write and
//! delete files on disk; deliver the file-watch notifications for those writes —
//! every handler's answer from the long-lived ("warm") session must equal the
//! answer from brand-new sessions started over the same on-disk state with the
//! same buffers open. A cache is an optimisation, so no command sequence may make
//! it observable.
//!
//! The reference is several sessions, not one ([`Group`]): a single fresh
//! session answering every query would warm its own caches as it went, and so
//! share any bug the *order* of the queries can provoke.
//!
//! What is compared, at every [`Cmd::Check`] (and once more at the end):
//!
//! - **Direct queries**, every handler the server implements: for each open F#
//!   source, `hover`, `definition` and `references` at every identifier,
//!   `completion` after every `.`, `documentSymbol` and `semanticTokens/full`;
//!   `textDocument/diagnostic` on every source and project file, open or not;
//!   `workspace/diagnostic`; `workspace/symbol`.
//! - **What the client is showing**, which a direct query cannot see because the
//!   client only re-asks when told to. A push client shows the last
//!   `publishDiagnostics` per URI. A pull client shows the last report it pulled,
//!   re-pulling every open document after any text sync (the server advertises
//!   `interFileDependencies`) or on `workspace/diagnostic/refresh`, and keeping
//!   its cached report when the server answers `Unchanged` to the echoed
//!   `previousResultId`. Both clients show the semantic tokens they last pulled
//!   per open document: re-pulled for the document they edited, and for every
//!   open document on `workspace/semanticTokens/refresh`. So a missing refresh,
//!   a missing republish, or a `resultId` that omits an input is a failure here.
//!
//! No answer is normalised. The one history-dependent field a client sees, a
//! `publishDiagnostics` `version`, is left out of the displayed view (see
//! [`publish_view`]); `window/showMessage` toasts are deduplicated per session by
//! design and are not compared.
//!
//! The generated workspace is three SDK-less projects — `P/P.fsproj`,
//! `P/O.fsproj` beside it, and `Q/Q.fsproj` — over four sources, one of which
//! (`Shared/S.fs`) every project may link, a `common.props` any may import, and
//! per-directory `project.assets.json` files over a stub framework pack holding
//! the real `System.Runtime.dll`. Sources reference one another across files
//! (`A.x`, `open S`, `S.x.Length`), and their bindings sit under `#if FOO` /
//! `#if BAR` blocks keyed on each project's `DefineConstants`, so a stale define
//! set, Compile order, neighbour file or assembly changes what a name resolves
//! to. The initial world is healthy (see [`Damage`]); commands break it.
//!
//! The server watches nothing itself: file-watch events are whatever the client
//! sends in `workspace/didChangeWatchedFiles`. The model therefore queues one
//! event per disk write and delivers the queue on [`Cmd::Flush`]. Between a write
//! and its event the server *cannot* know the disk moved, so no comparison is
//! made while events are pending — but requests still are ([`Cmd::Peek`]), which
//! is how the server comes to cache state read from disk before the event that
//! should invalidate it.
//!
//! Text sync is FULL only (the server advertises `TextDocumentSyncKind::FULL`),
//! so every `didChange` carries the whole document.
//!
//! The default run is a fixed seed and case count, plus the [`pinned_scripts`]
//! found against planted defects. For a soak, set
//! `BORZOI_LSP_STATE_MACHINE_CASES` and `BORZOI_LSP_STATE_MACHINE_SEED`. A
//! failure prints the minimal script ([`minimise`]), ready to pin.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use borzoi::sdk_discovery::SdkDiscoveryEnv;
use borzoi::server::{State, run_with_fetcher};
use borzoi::workspace::Workspace;
use lsp_server::{Connection, Message, Notification, Request, RequestId, Response};
use lsp_types::{
    ClientCapabilities, DiagnosticClientCapabilities, DiagnosticWorkspaceClientCapabilities,
    DocumentSymbolClientCapabilities, PublishDiagnosticsClientCapabilities,
    SemanticTokensWorkspaceClientCapabilities, TextDocumentClientCapabilities, Url,
    WorkspaceClientCapabilities,
};
use proptest::prelude::*;
use proptest::test_runner::{Config, RngAlgorithm, TestCaseError, TestError, TestRng, TestRunner};
use serde_json::{Value, json};

// ---------------------------------------------------------------------------
// The workspace, as data.
// ---------------------------------------------------------------------------

/// An F# source file. Each declares `module <Self>`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum Src {
    A,
    B,
    C,
    S,
}

impl Src {
    fn module(self) -> &'static str {
        match self {
            Src::A => "A",
            Src::B => "B",
            Src::C => "C",
            Src::S => "S",
        }
    }

    fn rel(self) -> &'static str {
        match self {
            Src::A => "P/A.fs",
            Src::B => "P/B.fs",
            Src::C => "Q/C.fs",
            Src::S => "Shared/S.fs",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum Proj {
    /// `P/O.fsproj`, beside `P.fsproj`. Two projects in one directory spell a
    /// shared include identically, so they share the per-file parse cache's key
    /// for it — the only way one file reaches that cache under two `#if` symbol
    /// sets without a structural invalidation between them. It sorts before
    /// `P.fsproj`, so it owns whichever of the directory's sources it lists.
    O,
    P,
    Q,
}

impl Proj {
    fn rel(self) -> &'static str {
        match self {
            Proj::O => "P/O.fsproj",
            Proj::P => "P/P.fsproj",
            Proj::Q => "Q/Q.fsproj",
        }
    }

    fn dir(self) -> &'static str {
        match self {
            Proj::O | Proj::P => "P",
            Proj::Q => "Q",
        }
    }
}

/// Every file the model can touch.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum FileId {
    Src(Src),
    Proj(Proj),
    Props,
    /// `<dir>/obj/project.assets.json`: present means "restored", so the
    /// project's assembly env reads the framework pack. Keyed by directory,
    /// so `O` and `P` share `Assets(Proj::P)`.
    Assets(Proj),
    /// The framework pack's `System.Runtime.dll` (a copy of the real reference
    /// assembly), which gives `string` its members.
    Dll,
}

const FILES: [FileId; 11] = [
    FileId::Src(Src::A),
    FileId::Src(Src::B),
    FileId::Src(Src::C),
    FileId::Src(Src::S),
    FileId::Proj(Proj::O),
    FileId::Proj(Proj::P),
    FileId::Proj(Proj::Q),
    FileId::Props,
    FileId::Assets(Proj::P),
    FileId::Assets(Proj::Q),
    FileId::Dll,
];

/// Where the stub framework pack keeps its one assembly, under the session's
/// `$DOTNET_ROOT` (`<root>/dotnet`).
const DLL_REL: &str =
    "dotnet/packs/Microsoft.NETCore.App.Ref/10.0.0/ref/net10.0/System.Runtime.dll";

impl FileId {
    fn rel(self) -> &'static str {
        match self {
            FileId::Src(s) => s.rel(),
            FileId::Proj(p) => p.rel(),
            FileId::Props => "common.props",
            FileId::Assets(Proj::O | Proj::P) => "P/obj/project.assets.json",
            FileId::Assets(Proj::Q) => "Q/obj/project.assets.json",
            FileId::Dll => DLL_REL,
        }
    }

    /// Whether a client would hold this file in an editor buffer.
    fn is_buffer(self) -> bool {
        matches!(self, FileId::Src(_) | FileId::Proj(_) | FileId::Props)
    }
}

impl fmt::Debug for FileId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.rel())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Sym {
    Foo,
    Bar,
}

impl Sym {
    fn text(self) -> &'static str {
        match self {
            Sym::Foo => "FOO",
            Sym::Bar => "BAR",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Name {
    X,
    Y,
    Z,
}

impl Name {
    fn text(self) -> &'static str {
        match self {
            Name::X => "x",
            Name::Y => "y",
            Name::Z => "z",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Expr {
    Lit(u8),
    Str,
    /// `x.Length` — a member of a referenced-assembly type when `x` is a string.
    Length(Name),
    /// A bare name: an earlier binding in this file, or one an `open` brought in.
    Bare(Name),
    /// `M.x` — a cross-file reference when `M` is another file's module.
    Qual(Src, Name),
    /// `M.x.Length` — a member whose receiver's type is decided by another file
    /// (and by which of its `#if` branches is active).
    QualLength(Src, Name),
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Item {
    Let(Name, Expr),
    /// `let a = "s"` then `let b = a.Length`: a string-typed receiver in the
    /// same branch, so member hover and completion have something to answer.
    StrPair(Name, Name),
    Open(Src),
    IfDef(Sym, Vec<Item>, Vec<Item>),
    /// A line the parser must recover from, so diagnostics are non-empty.
    Garbage,
}

/// One `<Compile>` entry of a project.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Entry {
    File(Src),
    /// `<Compile Include="*.fs" />` — the project directory's sources, so a
    /// source's creation or deletion moves the Compile set.
    Glob,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ProjectSpec {
    import_props: bool,
    defines: Vec<Sym>,
    compile: Vec<Entry>,
    /// Malformed XML: the project fails to evaluate.
    broken: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PropsSpec {
    defines: Vec<Sym>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Content {
    Source(Vec<Item>),
    Project(ProjectSpec),
    Props(PropsSpec),
    /// A minimal `project.assets.json` naming the `Microsoft.NETCore.App`
    /// framework reference.
    Assets,
    /// The real `System.Runtime.dll`.
    Dll,
}

fn render_items(out: &mut String, items: &[Item]) {
    for item in items {
        match item {
            Item::Let(name, expr) => {
                let rhs = match expr {
                    Expr::Lit(n) => n.to_string(),
                    Expr::Str => "\"s\"".to_string(),
                    Expr::Length(n) => format!("{}.Length", n.text()),
                    Expr::Bare(n) => n.text().to_string(),
                    Expr::Qual(m, n) => format!("{}.{}", m.module(), n.text()),
                    Expr::QualLength(m, n) => format!("{}.{}.Length", m.module(), n.text()),
                };
                out.push_str(&format!("let {} = {rhs}\n", name.text()));
            }
            Item::StrPair(a, b) => {
                out.push_str(&format!("let {} = \"s\"\n", a.text()));
                out.push_str(&format!("let {} = {}.Length\n", b.text(), a.text()));
            }
            Item::Open(m) => out.push_str(&format!("open {}\n", m.module())),
            Item::IfDef(sym, then, els) => {
                out.push_str(&format!("#if {}\n", sym.text()));
                render_items(out, then);
                if !els.is_empty() {
                    out.push_str("#else\n");
                    render_items(out, els);
                }
                out.push_str("#endif\n");
            }
            Item::Garbage => out.push_str("let = )\n"),
        }
    }
}

/// The path of `src` relative to project `proj`'s directory, as a `<Compile>`
/// include spells it.
fn include_path(proj: Proj, src: Src) -> String {
    let rel = src.rel();
    match rel.strip_prefix(&format!("{}/", proj.dir())) {
        Some(local) => local.to_string(),
        None => format!("../{rel}"),
    }
}

fn define_constants(defines: &[Sym]) -> String {
    let mut s = "$(DefineConstants)".to_string();
    for d in defines {
        s.push(';');
        s.push_str(d.text());
    }
    s
}

impl Content {
    /// The file text for this content written at `file`. A source's module name
    /// comes from the file, a project's include paths from its directory.
    fn render(&self, file: FileId) -> String {
        match (self, file) {
            (Content::Source(items), FileId::Src(src)) => {
                let mut out = format!("module {}\n\n", src.module());
                render_items(&mut out, items);
                out
            }
            (Content::Project(spec), FileId::Proj(proj)) => {
                let mut out = String::from("<Project>\n");
                if spec.import_props {
                    out.push_str("  <Import Project=\"../common.props\" />\n");
                }
                if !spec.defines.is_empty() {
                    out.push_str(&format!(
                        "  <PropertyGroup>\n    <DefineConstants>{}</DefineConstants>\n  </PropertyGroup>\n",
                        define_constants(&spec.defines)
                    ));
                }
                out.push_str("  <ItemGroup>\n");
                for entry in &spec.compile {
                    let include = match entry {
                        Entry::File(src) => include_path(proj, *src),
                        Entry::Glob => "*.fs".to_string(),
                    };
                    out.push_str(&format!("    <Compile Include=\"{include}\" />\n"));
                }
                out.push_str("  </ItemGroup>\n");
                if !spec.broken {
                    out.push_str("</Project>\n");
                }
                out
            }
            (Content::Props(spec), FileId::Props) => {
                let mut out = String::from("<Project>\n");
                if !spec.defines.is_empty() {
                    out.push_str(&format!(
                        "  <PropertyGroup>\n    <DefineConstants>{}</DefineConstants>\n  </PropertyGroup>\n",
                        define_constants(&spec.defines)
                    ));
                }
                out.push_str("</Project>\n");
                out
            }
            (Content::Assets, FileId::Assets(_)) => json!({
                "version": 3,
                "targets": { "net10.0": {} },
                "libraries": {},
                "packageFolders": { "../../pkgs/": {} },
                "project": {
                    "frameworks": {
                        "net10.0": { "frameworkReferences": { "Microsoft.NETCore.App": {} } }
                    }
                }
            })
            .to_string(),
            (Content::Dll, FileId::Dll) => "<System.Runtime.dll>".to_string(),
            _ => unreachable!("content generated for the wrong kind of file"),
        }
    }

    /// The bytes written to disk for this content at `file`.
    fn bytes(&self, file: FileId) -> Vec<u8> {
        match self {
            Content::Dll => fs::read(system_runtime_dll()).expect("read System.Runtime.dll"),
            _ => self.render(file).into_bytes(),
        }
    }
}

/// The real reference-pack `System.Runtime.dll` the stub framework pack copies.
fn system_runtime_dll() -> &'static Path {
    static DLL: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    DLL.get_or_init(crate::common::ensure_system_runtime_dll)
}

// ---------------------------------------------------------------------------
// Commands.
// ---------------------------------------------------------------------------

#[derive(Clone)]
enum Cmd {
    /// `didOpen` with the file's current disk text. Skipped if already open or
    /// absent from disk.
    Open(FileId),
    /// `didChange` (full text). Skipped unless open.
    Edit(FileId, Content),
    /// `didClose`. Skipped unless open.
    Close(FileId),
    /// Write the open buffer to disk, queueing its watch event. Skipped unless
    /// open.
    Save(FileId),
    /// An external write to disk (another tool, `git checkout`), queueing its
    /// watch event.
    Write(FileId, Content),
    /// An external delete, queueing its watch event. Skipped if absent.
    Delete(FileId),
    /// Deliver the queued watch events as one `didChangeWatchedFiles`.
    Flush,
    /// Compare against fresh sessions. Skipped while watch events are queued.
    Check,
    /// Ask the warm session for one file's diagnostics (and, if it is open, its
    /// tokens and hovers) without comparing — warms caches, including from disk
    /// state whose watch event is still queued.
    Peek(Src),
}

impl fmt::Debug for Cmd {
    /// Content is shown as the text the server actually receives, so a shrunk
    /// failure reads as a replayable script.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Cmd::Open(file) => write!(f, "Open({file:?})"),
            Cmd::Edit(file, c) => write!(f, "Edit({file:?}, {:?})", c.render(*file)),
            Cmd::Close(file) => write!(f, "Close({file:?})"),
            Cmd::Save(file) => write!(f, "Save({file:?})"),
            Cmd::Write(file, c) => write!(f, "Write({file:?}, {:?})", c.render(*file)),
            Cmd::Delete(file) => write!(f, "Delete({file:?})"),
            Cmd::Flush => f.write_str("Flush"),
            Cmd::Check => f.write_str("Check"),
            Cmd::Peek(src) => write!(f, "Peek({:?})", FileId::Src(*src)),
        }
    }
}

/// How the client receives diagnostics. Both modes accept semantic-token
/// refreshes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Push,
    Pull,
}

impl Mode {
    fn caps(self) -> ClientCapabilities {
        let mut text_document = TextDocumentClientCapabilities {
            document_symbol: Some(DocumentSymbolClientCapabilities {
                hierarchical_document_symbol_support: Some(true),
                ..Default::default()
            }),
            publish_diagnostics: Some(PublishDiagnosticsClientCapabilities::default()),
            ..Default::default()
        };
        let mut workspace = WorkspaceClientCapabilities {
            semantic_tokens: Some(SemanticTokensWorkspaceClientCapabilities {
                refresh_support: Some(true),
            }),
            ..Default::default()
        };
        if self == Mode::Pull {
            text_document.diagnostic = Some(DiagnosticClientCapabilities::default());
            workspace.diagnostic = Some(DiagnosticWorkspaceClientCapabilities {
                refresh_support: Some(true),
            });
        }
        ClientCapabilities {
            text_document: Some(text_document),
            workspace: Some(workspace),
            ..Default::default()
        }
    }
}

/// One generated case: the starting disk, the client kind, and the script.
#[derive(Clone)]
struct Case {
    mode: Mode,
    disk: Vec<(FileId, Content)>,
    cmds: Vec<Cmd>,
}

impl fmt::Debug for Case {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "Case {{ mode: {:?},", self.mode)?;
        writeln!(f, "  disk: [")?;
        for (file, content) in &self.disk {
            writeln!(f, "    {file:?}: {:?},", content.render(*file))?;
        }
        writeln!(f, "  ],\n  cmds: [")?;
        for cmd in &self.cmds {
            writeln!(f, "    {cmd:?},")?;
        }
        write!(f, "  ] }}")
    }
}

// ---------------------------------------------------------------------------
// Strategies.
// ---------------------------------------------------------------------------

fn src_strategy() -> impl Strategy<Value = Src> {
    prop_oneof![Just(Src::A), Just(Src::B), Just(Src::C), Just(Src::S)]
}

/// The modules `src` may name. Mostly one that precedes it in its home
/// project's canonical Compile list (`S` before everything; `A` before `B`), so
/// the reference resolves while that list holds; sometimes itself; sometimes any
/// module, which resolves only under a reordered or relinked project. A name can
/// resolve across files only into an *earlier* file, so drawn uniformly almost
/// no reference would resolve, and the comparison would be between two sessions
/// agreeing that nothing is bound.
fn module_strategy(src: Src) -> BoxedStrategy<Src> {
    let earlier: Vec<Src> = match src {
        Src::S => vec![Src::S],
        Src::A | Src::C => vec![Src::S],
        Src::B => vec![Src::S, Src::A],
    };
    prop_oneof![
        6 => prop::sample::select(earlier),
        1 => Just(src),
        1 => src_strategy(),
    ]
    .boxed()
}
fn sym_strategy() -> impl Strategy<Value = Sym> {
    prop_oneof![Just(Sym::Foo), Just(Sym::Bar)]
}

fn name_strategy() -> impl Strategy<Value = Name> {
    // Skewed, so a qualified `M.x` usually names something `M` defines.
    prop_oneof![4 => Just(Name::X), 2 => Just(Name::Y), 1 => Just(Name::Z)]
}

fn expr_strategy(src: Src) -> impl Strategy<Value = Expr> {
    prop_oneof![
        1 => (0u8..3).prop_map(Expr::Lit),
        2 => Just(Expr::Str),
        2 => name_strategy().prop_map(Expr::Length),
        2 => name_strategy().prop_map(Expr::Bare),
        5 => (module_strategy(src), name_strategy()).prop_map(|(m, n)| Expr::Qual(m, n)),
        2 => (module_strategy(src), name_strategy()).prop_map(|(m, n)| Expr::QualLength(m, n)),
    ]
}

/// Whether generated content may be broken: a source line the parser must
/// recover from, a malformed project. The initial world is healthy, so most
/// cases start from projects that fold; damage arrives only through commands,
/// which is also the order a real session meets it in.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Damage {
    None,
    Allowed,
}

fn items_strategy(src: Src, damage: Damage) -> impl Strategy<Value = Vec<Item>> {
    let healthy = prop_oneof![
        6 => (name_strategy(), expr_strategy(src)).prop_map(|(n, e)| Item::Let(n, e)),
        2 => (name_strategy(), name_strategy()).prop_map(|(a, b)| Item::StrPair(a, b)),
        1 => module_strategy(src).prop_map(Item::Open),
    ];
    let leaf = match damage {
        Damage::None => healthy.boxed(),
        Damage::Allowed => prop_oneof![9 => healthy, 1 => Just(Item::Garbage)].boxed(),
    };
    // Inside an `#if` branch a parse error is allowed even in a healthy world:
    // it is what makes a define change change the *diagnostics* (a `resultId`,
    // a republish, a pull refresh), and a branch is inactive often enough not to
    // cost most folds their resolution.
    let item = leaf.prop_recursive(2, 12, 4, |inner| {
        let branch = prop_oneof![8 => inner, 1 => Just(Item::Garbage)];
        (
            sym_strategy(),
            prop::collection::vec(branch.clone(), 0..3),
            prop::collection::vec(branch, 0..3),
        )
            .prop_map(|(s, t, e)| Item::IfDef(s, t, e))
    });
    // A healthy source opens with an unconditional `x`, so a later file's `M.x`
    // resolves whenever `M` precedes it in a folding project; the rest of the
    // file decides what `#if` hides around it. An edit may drop it: removing a
    // binding a later file uses is the edit an invalidation bug hides.
    let head = prop_oneof![(0u8..3).prop_map(Expr::Lit), Just(Expr::Str)];
    let head = match damage {
        Damage::None => head.prop_map(Some).boxed(),
        Damage::Allowed => prop::option::weighted(0.6, head).boxed(),
    };
    (head, prop::collection::vec(item, 0..6)).prop_map(|(head, mut items)| {
        if let Some(head) = head {
            items.insert(0, Item::Let(Name::X, head));
        }
        items
    })
}

/// A project for `proj`: its own sources plus the shared one, in Compile order
/// (`S` first), then perturbed — an entry dropped, a foreign source linked, the order
/// shuffled, a glob added. Drawn uniformly instead, the list would rarely hold
/// both a referencing file and the file it references, and one missing Compile
/// item refuses the whole fold.
fn project_strategy(proj: Proj, damage: Damage) -> impl Strategy<Value = ProjectSpec> {
    let (base, foreign): (Vec<Src>, Vec<Src>) = match proj {
        Proj::O => (vec![Src::S, Src::B], vec![Src::A, Src::C]),
        Proj::P => (vec![Src::S, Src::A, Src::B], vec![Src::C]),
        Proj::Q => (vec![Src::S, Src::C], vec![Src::A, Src::B]),
    };
    let n = base.len();
    (
        prop::bool::weighted(0.3),
        prop::collection::vec(sym_strategy(), 0..3),
        prop::collection::vec(prop::bool::weighted(0.85), n),
        prop::option::weighted(0.2, prop::sample::select(foreign)),
        prop::option::weighted(0.25, any::<u8>()),
        prop::bool::weighted(0.05),
        prop::bool::weighted(if damage == Damage::Allowed { 0.05 } else { 0.0 }),
    )
        .prop_map(
            move |(import_props, defines, keep, link, shuffle, glob, broken)| {
                let mut compile: Vec<Entry> = base
                    .iter()
                    .zip(keep)
                    .filter(|(_, keep)| *keep)
                    .map(|(src, _)| Entry::File(*src))
                    .collect();
                if let Some(src) = link {
                    compile.push(Entry::File(src));
                }
                if let Some(seed) = shuffle {
                    // A deterministic rotation-and-swap from the seed: enough to
                    // put a referenced file after its user.
                    let len = compile.len();
                    if len > 1 {
                        compile.rotate_left(seed as usize % len);
                        compile.swap(0, (seed as usize / len) % len);
                    }
                }
                if glob {
                    compile.push(Entry::Glob);
                }
                ProjectSpec {
                    import_props,
                    defines,
                    compile,
                    broken,
                }
            },
        )
}

fn props_strategy() -> impl Strategy<Value = PropsSpec> {
    prop::collection::vec(sym_strategy(), 0..3).prop_map(|defines| PropsSpec { defines })
}

/// A file a client may hold in a buffer. `P`'s two sources dominate, so a
/// script's edits, closes and saves tend to land on files that already
/// interact (and on files that are open: an action on a closed buffer is a
/// no-op).
fn buffer_file_strategy() -> impl Strategy<Value = FileId> {
    prop_oneof![
        4 => Just(FileId::Src(Src::A)),
        4 => Just(FileId::Src(Src::B)),
        2 => Just(FileId::Src(Src::C)),
        2 => Just(FileId::Src(Src::S)),
        1 => Just(FileId::Proj(Proj::O)),
        2 => Just(FileId::Proj(Proj::P)),
        2 => Just(FileId::Proj(Proj::Q)),
        1 => Just(FileId::Props),
    ]
}

/// Any file, for disk writes and deletes.
fn disk_file_strategy() -> impl Strategy<Value = FileId> {
    prop_oneof![
        11 => buffer_file_strategy(),
        1 => Just(FileId::Assets(Proj::P)),
        1 => Just(FileId::Assets(Proj::Q)),
        1 => Just(FileId::Dll),
    ]
}

fn content_for(file: FileId) -> BoxedStrategy<Content> {
    match file {
        FileId::Src(src) => items_strategy(src, Damage::Allowed)
            .prop_map(Content::Source)
            .boxed(),
        FileId::Proj(p) => project_strategy(p, Damage::Allowed)
            .prop_map(Content::Project)
            .boxed(),
        FileId::Props => props_strategy().prop_map(Content::Props).boxed(),
        FileId::Assets(_) => Just(Content::Assets).boxed(),
        FileId::Dll => Just(Content::Dll).boxed(),
    }
}

fn with_content(file: impl Strategy<Value = FileId>) -> impl Strategy<Value = (FileId, Content)> {
    file.prop_flat_map(|file| content_for(file).prop_map(move |c| (file, c)))
}

fn cmd_strategy() -> impl Strategy<Value = Cmd> {
    prop_oneof![
        3 => buffer_file_strategy().prop_map(Cmd::Open),
        6 => with_content(buffer_file_strategy()).prop_map(|(f, c)| Cmd::Edit(f, c)),
        3 => buffer_file_strategy().prop_map(Cmd::Close),
        2 => buffer_file_strategy().prop_map(Cmd::Save),
        3 => with_content(disk_file_strategy()).prop_map(|(f, c)| Cmd::Write(f, c)),
        1 => disk_file_strategy().prop_map(Cmd::Delete),
        3 => Just(Cmd::Flush),
        3 => Just(Cmd::Check),
        2 => src_strategy().prop_map(Cmd::Peek),
    ]
}

fn case_strategy() -> impl Strategy<Value = Case> {
    let mode = prop_oneof![Just(Mode::Push), Just(Mode::Pull)];
    // A healthy world (see `Damage`): every source and `common.props` exist,
    // `P.fsproj` always and the other projects often, and most projects are
    // restored. Commands then delete, break and reorder it.
    let disk = (
        project_strategy(Proj::P, Damage::None),
        prop::option::weighted(0.3, project_strategy(Proj::O, Damage::None)),
        prop::option::weighted(0.6, project_strategy(Proj::Q, Damage::None)),
        props_strategy().prop_map(Some),
        (
            items_strategy(Src::A, Damage::None).prop_map(Some),
            items_strategy(Src::B, Damage::None).prop_map(Some),
            items_strategy(Src::C, Damage::None).prop_map(Some),
            items_strategy(Src::S, Damage::None).prop_map(Some),
        ),
        prop::collection::vec(prop::bool::weighted(0.8), 3),
    )
        .prop_map(|(p, o, q, props, sources, restored)| {
            let mut disk = vec![(FileId::Proj(Proj::P), Content::Project(p))];
            if let Some(o) = o {
                disk.push((FileId::Proj(Proj::O), Content::Project(o)));
            }
            if let Some(q) = q {
                disk.push((FileId::Proj(Proj::Q), Content::Project(q)));
            }
            if let Some(props) = props {
                disk.push((FileId::Props, Content::Props(props)));
            }
            let (a, b, c, s) = sources;
            for (src, items) in [(Src::A, a), (Src::B, b), (Src::C, c), (Src::S, s)] {
                if let Some(items) = items {
                    disk.push((FileId::Src(src), Content::Source(items)));
                }
            }
            for (file, present) in [
                FileId::Assets(Proj::P),
                FileId::Assets(Proj::Q),
                FileId::Dll,
            ]
            .into_iter()
            .zip(restored)
            {
                if present {
                    let content = if file == FileId::Dll {
                        Content::Dll
                    } else {
                        Content::Assets
                    };
                    disk.push((file, content));
                }
            }
            disk
        });
    // `documentSymbol`, `references` and `completion` answer only for open
    // buffers, so most scripts start by opening a few files.
    let opens = prop::collection::vec(buffer_file_strategy(), 0..5);
    (
        mode,
        disk,
        opens,
        prop::collection::vec(cmd_strategy(), 1..20),
    )
        .prop_map(|(mode, disk, opens, cmds)| Case {
            mode,
            disk,
            cmds: opens.into_iter().map(Cmd::Open).chain(cmds).collect(),
        })
}

// ---------------------------------------------------------------------------
// The client: a session over an in-memory connection, plus what it displays.
// ---------------------------------------------------------------------------

/// A generous bound: the machine running the suite may be heavily loaded, and a
/// timeout here is a hang in the server, never an expected outcome.
const RECV_TIMEOUT: Duration = Duration::from_secs(120);

/// Refresh rounds a single settle may take before we call it a loop. Each round
/// is one server→client refresh the client answered; the server holds at most
/// one in flight and owes at most one of each kind, so two suffice in practice.
const MAX_SETTLE_ROUNDS: usize = 8;

struct Client {
    conn: Connection,
    thread: Option<thread::JoinHandle<()>>,
    next_id: i64,
    mode: Mode,
    /// Push mode: the last `publishDiagnostics` per URI. Its `version` is left
    /// out (see [`publish_view`]).
    published: BTreeMap<String, Value>,
    /// Pull mode: the last report the client holds per open URI — the
    /// `resultId` it would echo, and the items it displays.
    pulled: BTreeMap<String, (Option<String>, Value)>,
    /// The semantic tokens the client holds per open URI.
    tokens: BTreeMap<String, Value>,
    /// A `workspace/diagnostic/refresh` arrived since the last re-pull.
    diagnostic_refresh_owed: bool,
    /// A `workspace/semanticTokens/refresh` arrived since the last re-pull.
    token_refresh_owed: bool,
    refreshes_seen: usize,
}

impl Client {
    fn start(root: &Path, mode: Mode) -> Self {
        let (server, conn) = Connection::memory();
        let root = root.to_path_buf();
        let thread = thread::spawn(move || {
            let mut state = State::new();
            // Hermetic SDK discovery: the generated projects are SDK-less, and the
            // host's `dotnet` must not leak into either session. `$DOTNET_ROOT`
            // is the stub framework pack inside the world, so a restored
            // project's assembly env is a function of files the model controls.
            state.workspace = Workspace::with_env(SdkDiscoveryEnv {
                dotnet_root: Some(root.join("dotnet")),
                ..SdkDiscoveryEnv::default()
            });
            state.set_client_capabilities(mode.caps());
            state.set_workspace_roots(vec![root]);
            run_with_fetcher(server, state, None).expect("server loop ends cleanly");
        });
        Client {
            conn,
            thread: Some(thread),
            next_id: 0,
            mode,
            published: BTreeMap::new(),
            pulled: BTreeMap::new(),
            tokens: BTreeMap::new(),
            diagnostic_refresh_owed: false,
            token_refresh_owed: false,
            refreshes_seen: 0,
        }
    }

    fn notify(&self, method: &str, params: Value) {
        self.conn
            .sender
            .send(Message::Notification(Notification {
                method: method.to_string(),
                params,
            }))
            .expect("send notification");
    }

    /// Handle one server-initiated message the way a client would.
    fn absorb(&mut self, msg: Message) {
        match msg {
            Message::Notification(not) => {
                if not.method == "textDocument/publishDiagnostics" {
                    let uri = not.params["uri"].as_str().expect("uri").to_string();
                    self.published.insert(uri, publish_view(&not.params));
                }
                // `window/showMessage` (the project-deferral toast) is deduped
                // against what this session already showed, so a warm and a
                // fresh session legitimately differ in whether it is sent.
            }
            Message::Request(req) => {
                match req.method.as_str() {
                    "workspace/diagnostic/refresh" => {
                        self.diagnostic_refresh_owed = true;
                        self.refreshes_seen += 1;
                    }
                    "workspace/semanticTokens/refresh" => {
                        self.token_refresh_owed = true;
                        self.refreshes_seen += 1;
                    }
                    _ => {}
                }
                self.conn
                    .sender
                    .send(Message::Response(Response {
                        id: req.id,
                        result: Some(Value::Null),
                        error: None,
                    }))
                    .expect("reply to server request");
            }
            Message::Response(resp) => panic!("unsolicited response {resp:?}"),
        }
    }

    /// Send a request and wait for its response, absorbing whatever the server
    /// sends first. Returns `{result, error}` as one comparable value.
    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = RequestId::from(format!("t{}", self.next_id));
        self.next_id += 1;
        self.conn
            .sender
            .send(Message::Request(Request {
                id: id.clone(),
                method: method.to_string(),
                params,
            }))
            .expect("send request");
        loop {
            let msg = self
                .conn
                .receiver
                .recv_timeout(RECV_TIMEOUT)
                .unwrap_or_else(|e| panic!("no response to {method}: {e}"));
            match msg {
                Message::Response(resp) if resp.id == id => {
                    return json!({ "result": resp.result, "error": resp.error.map(|e| json!({"code": e.code, "message": e.message})) });
                }
                other => self.absorb(other),
            }
        }
    }

    /// Wait until the server has processed everything sent so far, including
    /// the replies to its own refresh requests (each of which may release the
    /// next owed refresh). The server loop is serial, so a request's response
    /// follows everything the server emitted before it.
    fn settle(&mut self) {
        for _ in 0..MAX_SETTLE_ROUNDS {
            let before = self.refreshes_seen;
            // An unknown method is answered `MethodNotFound` without touching
            // any cache: a pure ordering barrier.
            self.request("borzoi-test/barrier", Value::Null);
            if self.refreshes_seen == before {
                return;
            }
        }
        panic!("the server kept sending refreshes after {MAX_SETTLE_ROUNDS} rounds");
    }

    /// Re-pull what a real client re-pulls after a text sync of `touched` (or a
    /// refresh), then settle; repeat while the pulls themselves release refreshes.
    fn sync_views(&mut self, open: &BTreeSet<String>, touched: Option<&str>) {
        self.settle();
        let mut repull_all_diags = touched.is_some();
        // Tokens are an F# feature; the client does not ask for them on a
        // project file.
        let mut repull_tokens: BTreeSet<String> = touched
            .filter(|u| open.contains(*u) && u.ends_with(".fs"))
            .map(|u| BTreeSet::from([u.to_string()]))
            .unwrap_or_default();
        for _ in 0..MAX_SETTLE_ROUNDS {
            repull_all_diags |= std::mem::take(&mut self.diagnostic_refresh_owed);
            if std::mem::take(&mut self.token_refresh_owed) {
                repull_tokens.extend(open.iter().filter(|u| u.ends_with(".fs")).cloned());
            }
            if !repull_all_diags && repull_tokens.is_empty() {
                return;
            }
            if self.mode == Mode::Pull && repull_all_diags {
                for uri in open {
                    let previous = self.pulled.get(uri).and_then(|(id, _)| id.clone());
                    let resp = self.request(
                        "textDocument/diagnostic",
                        json!({ "textDocument": { "uri": uri }, "previousResultId": previous }),
                    );
                    let report = &resp["result"];
                    match report["kind"].as_str() {
                        Some("unchanged") => {
                            let (_, items) = self
                                .pulled
                                .remove(uri)
                                .expect("`unchanged` answers only an echoed resultId");
                            let id = report["resultId"].as_str().map(str::to_string);
                            self.pulled.insert(uri.clone(), (id, items));
                        }
                        _ => {
                            let id = report["resultId"].as_str().map(str::to_string);
                            self.pulled
                                .insert(uri.clone(), (id, report["items"].clone()));
                        }
                    }
                }
            }
            repull_all_diags = false;
            for uri in std::mem::take(&mut repull_tokens) {
                let resp = self.request(
                    "textDocument/semanticTokens/full",
                    json!({ "textDocument": { "uri": uri } }),
                );
                self.tokens.insert(uri, resp);
            }
            self.settle();
        }
        panic!("re-pulls kept releasing refreshes after {MAX_SETTLE_ROUNDS} rounds");
    }

    fn forget(&mut self, uri: &str) {
        self.pulled.remove(uri);
        self.tokens.remove(uri);
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.conn.sender.send(Message::Request(Request {
            id: RequestId::from("shutdown".to_string()),
            method: "shutdown".to_string(),
            params: Value::Null,
        }));
        let deadline = std::time::Instant::now() + RECV_TIMEOUT;
        let mut acknowledged = false;
        while let Ok(msg) = self.conn.receiver.recv_deadline(deadline) {
            if matches!(&msg, Message::Response(r) if r.id == RequestId::from("shutdown".to_string()))
            {
                acknowledged = true;
                break;
            }
        }
        let _ = self.conn.sender.send(Message::Notification(Notification {
            method: "exit".to_string(),
            params: Value::Null,
        }));
        let Some(handle) = self.thread.take() else {
            return;
        };
        // Join only a thread that is certain to end: one that acknowledged
        // `shutdown` (so it is about to read `exit`) or one that has already
        // ended (a panic, whose payload the join recovers). A server wedged past
        // the deadline is left detached, so the bounded `recv` that reported it
        // stays bounded rather than hanging the test binary here.
        if !acknowledged && !handle.is_finished() {
            return;
        }
        if let Err(err) = handle.join()
            && !thread::panicking()
        {
            std::panic::resume_unwind(err);
        }
    }
}

/// A `publishDiagnostics` as the client displays it: the diagnostics only.
///
/// `version` is dropped deliberately. It echoes the document version the
/// diagnostics were computed for — a counter of how many edits this session has
/// seen, which a fresh session cannot share. (The server publishes `None` today;
/// the normalisation keeps that from being load-bearing.)
fn publish_view(params: &Value) -> Value {
    params["diagnostics"].clone()
}

// ---------------------------------------------------------------------------
// The world: the real disk, the model's buffers, the queued watch events.
// ---------------------------------------------------------------------------

struct World {
    root: PathBuf,
    /// Open buffers, by file, with their current text.
    open: BTreeMap<FileId, String>,
    /// Buffer versions, so each `didChange` is well-formed.
    versions: BTreeMap<FileId, i32>,
    /// Watch events for disk writes the server has not yet been told about.
    pending: Vec<Value>,
}

impl World {
    fn path(&self, file: FileId) -> PathBuf {
        self.root.join(file.rel())
    }

    fn uri(&self, file: FileId) -> String {
        Url::from_file_path(self.path(file))
            .expect("absolute path")
            .to_string()
    }

    /// The file's disk text; the binary DLL reads as its placeholder.
    fn disk(&self, file: FileId) -> Option<String> {
        match file {
            FileId::Dll => self.path(file).exists().then(|| Content::Dll.render(file)),
            _ => fs::read_to_string(self.path(file)).ok(),
        }
    }

    fn write(&mut self, file: FileId, bytes: &[u8]) {
        let path = self.path(file);
        let existed = path.exists();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, bytes).unwrap();
        // FileChangeType: 1 = Created, 2 = Changed, 3 = Deleted.
        let typ = if existed { 2 } else { 1 };
        self.pending
            .push(json!({ "uri": self.uri(file), "type": typ }));
    }

    fn delete(&mut self, file: FileId) -> bool {
        let path = self.path(file);
        if fs::remove_file(&path).is_err() {
            return false;
        }
        self.pending
            .push(json!({ "uri": self.uri(file), "type": 3 }));
        true
    }

    fn open_uris(&self) -> BTreeSet<String> {
        self.open.keys().map(|f| self.uri(*f)).collect()
    }

    /// What the server should treat as `file`'s text: the buffer if open, else
    /// disk.
    fn visible(&self, file: FileId) -> Option<String> {
        self.open.get(&file).cloned().or_else(|| self.disk(file))
    }

    fn describe(&self) -> String {
        let mut out = String::new();
        for file in FILES {
            let disk = self.disk(file);
            let buffer = self.open.get(&file);
            if disk.is_none() && buffer.is_none() {
                continue;
            }
            out.push_str(&format!("--- {file:?}"));
            match (&disk, buffer) {
                (_, Some(b)) if disk.as_ref() == Some(b) => out.push_str(" (open, = disk)\n"),
                (_, Some(_)) => out.push_str(" (open, buffer differs from disk)\n"),
                (_, None) => out.push_str(" (disk only)\n"),
            }
            if let Some(b) = buffer {
                out.push_str(b);
                if disk.as_ref() != Some(b) {
                    out.push_str(&format!(
                        "[disk: {}]\n",
                        disk.as_deref()
                            .map_or("absent".into(), |d| format!("{d:?}"))
                    ));
                }
            } else if let Some(d) = &disk {
                out.push_str(d);
            }
        }
        out
    }
}

fn did_open(client: &Client, uri: &str, text: &str) {
    client.notify(
        "textDocument/didOpen",
        json!({ "textDocument": { "uri": uri, "languageId": "fsharp", "version": 1, "text": text } }),
    );
}

// ---------------------------------------------------------------------------
// The comparison.
// ---------------------------------------------------------------------------

/// A zero-based `(line, character)` position.
type LineCol = (u32, u32);

/// Every identifier start, and every position just after a `.`, in `text`
/// (ASCII, so a byte column is a UTF-16 column).
///
/// Keywords and directive lines are skipped: the generator's keywords (`module`,
/// `let`, `open`) and `#if` symbols resolve to nothing in any session, and each
/// position costs a request per handler per session.
fn positions(text: &str) -> (Vec<LineCol>, Vec<LineCol>) {
    const KEYWORDS: [&str; 3] = ["module", "let", "open"];
    let mut idents = Vec::new();
    let mut dots = Vec::new();
    for (line_no, line) in text.lines().enumerate() {
        if line.starts_with('#') {
            continue;
        }
        let bytes = line.as_bytes();
        let is_ident = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
        for (col, &b) in bytes.iter().enumerate() {
            let prev_ident = col > 0 && is_ident(bytes[col - 1]);
            if (b.is_ascii_alphabetic() || b == b'_') && !prev_ident {
                let word: String = line[col..]
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                    .collect();
                if !KEYWORDS.contains(&word.as_str()) {
                    idents.push((line_no as u32, col as u32));
                }
            }
            if b == b'.' {
                dots.push((line_no as u32, col as u32 + 1));
            }
        }
    }
    (idents, dots)
}

/// Which reference session answers a query. Each group gets its own brand-new
/// session, so the reference's answer to one query can never be shaped by the
/// caches another query warmed — a reference that answered everything from one
/// long-lived session would share every history-dependent bug the order of the
/// queries could provoke (a linked file parsed under one project's defines and
/// then served to the other, say), and agree with the warm session about it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Group {
    /// Every per-file `textDocument/diagnostic`, and the push client's view.
    /// Diagnostics fold nothing, so one session serves them all.
    Diagnostics,
    /// The positional and whole-file queries of one open source, which fold only
    /// that file's project.
    File(FileId),
    /// `workspace/diagnostic` evaluates every project.
    WorkspaceDiagnostic,
    /// One `workspace/symbol` query, which folds every open buffer's project.
    WorkspaceSymbol(&'static str),
}

struct Query {
    label: String,
    method: &'static str,
    params: Value,
    group: Group,
}

/// The full query set for the current world.
///
/// Within a file's group the `references` queries come first: they fold the
/// whole project, so the reference session's fold is one cold fold rather than
/// a prefix fold extended later. The warm session asks them last (see
/// [`check`]), so the two take different routes through the prefix cache.
fn queries(world: &World) -> Vec<Query> {
    let mut out = Vec::new();
    for file in FILES.into_iter().filter(|f| f.is_buffer()) {
        if world.visible(file).is_none() {
            continue;
        }
        out.push(Query {
            label: format!("diagnostic {file:?}"),
            method: "textDocument/diagnostic",
            params: json!({ "textDocument": { "uri": world.uri(file) } }),
            group: Group::Diagnostics,
        });
    }
    // Every handler but diagnostics answers only for an open buffer (a closed
    // file is `null` in any session), so only open sources are worth asking.
    // Closed files still feed the open ones through the project fold.
    for (file, text) in &world.open {
        let FileId::Src(_) = file else {
            continue;
        };
        let group = Group::File(*file);
        let doc = json!({ "uri": world.uri(*file) });
        let (idents, dots) = positions(text);
        let at = |line: u32, character: u32| json!({ "textDocument": doc, "position": { "line": line, "character": character } });
        for &(line, character) in &idents {
            let mut params = at(line, character);
            params["context"] = json!({ "includeDeclaration": true });
            out.push(Query {
                label: format!("references {file:?}:{line}:{character}"),
                method: "textDocument/references",
                params,
                group,
            });
        }
        for &(line, character) in &idents {
            for (name, method) in [
                ("hover", "textDocument/hover"),
                ("definition", "textDocument/definition"),
            ] {
                out.push(Query {
                    label: format!("{name} {file:?}:{line}:{character}"),
                    method,
                    params: at(line, character),
                    group,
                });
            }
        }
        for &(line, character) in &dots {
            out.push(Query {
                label: format!("completion {file:?}:{line}:{character}"),
                method: "textDocument/completion",
                params: at(line, character),
                group,
            });
        }
        for (name, method) in [
            ("documentSymbol", "textDocument/documentSymbol"),
            ("semanticTokens", "textDocument/semanticTokens/full"),
        ] {
            out.push(Query {
                label: format!("{name} {file:?}"),
                method,
                params: json!({ "textDocument": doc }),
                group,
            });
        }
    }
    out.push(Query {
        label: "workspace/diagnostic".into(),
        method: "workspace/diagnostic",
        params: json!({ "previousResultIds": [] }),
        group: Group::WorkspaceDiagnostic,
    });
    for query in ["", "x"] {
        out.push(Query {
            label: format!("workspace/symbol {query:?}"),
            method: "workspace/symbol",
            params: json!({ "query": query }),
            group: Group::WorkspaceSymbol(query),
        });
    }
    out
}

/// Values keyed by a query label or a URI.
type ByKey = BTreeMap<String, Value>;

/// The reference: every query's answer (by label) from fresh sessions over the
/// same disk with the same buffers open, one session per [`Group`]; and the
/// diagnostics session's pushed view (by URI).
fn reference_answers(world: &World, mode: Mode, queries: &[Query]) -> (ByKey, ByKey) {
    let mut answers = BTreeMap::new();
    let mut published = BTreeMap::new();
    let mut groups: BTreeSet<Group> = queries.iter().map(|q| q.group).collect();
    // The push view comes from this session even when no file is visible.
    groups.insert(Group::Diagnostics);
    for group in groups {
        let mut fresh = Client::start(&world.root, mode);
        for (file, text) in &world.open {
            did_open(&fresh, &world.uri(*file), text);
        }
        fresh.settle();
        if group == Group::Diagnostics {
            published = fresh.published.clone();
        }
        for q in queries.iter().filter(|q| q.group == group) {
            answers.insert(q.label.clone(), fresh.request(q.method, q.params.clone()));
        }
    }
    (answers, published)
}

/// What the generated cases actually exercised, so a run that compares only
/// `null`s cannot pass silently. Counted from the reference's answers.
#[derive(Default, Debug)]
struct Coverage {
    checks: usize,
    queries: usize,
    /// A definition landing in a different file from the one asked about.
    cross_file_definitions: usize,
    /// A hover on a member of a referenced-assembly type.
    assembly_member_hovers: usize,
    completions: usize,
    nonempty_diagnostics: usize,
    references: usize,
}

impl Coverage {
    fn record(&mut self, query: &Query, answer: &Value) {
        self.queries += 1;
        let result = &answer["result"];
        let empty = result.is_null() || result == &json!([]);
        match query.method {
            "textDocument/definition" if !empty => {
                let asked = &query.params["textDocument"]["uri"];
                if result["uri"] != *asked {
                    self.cross_file_definitions += 1;
                }
            }
            "textDocument/hover" => {
                let text = result["contents"]["value"].as_str().unwrap_or("");
                if text.contains("from System.Runtime") {
                    self.assembly_member_hovers += 1;
                }
            }
            "textDocument/completion" if !empty => self.completions += 1,
            "textDocument/diagnostic" if result["items"] != json!([]) => {
                self.nonempty_diagnostics += 1;
            }
            "textDocument/references" if !empty => self.references += 1,
            _ => {}
        }
    }
}

fn short(v: &Value) -> String {
    let s = serde_json::to_string(v).unwrap();
    if s.len() > 600 {
        format!("{}…", &s[..600])
    } else {
        s
    }
}

/// Compare the warm session, and what its client is displaying, against fresh
/// sessions over the same world.
fn check(
    warm: &mut Client,
    world: &World,
    coverage: &RefCell<Coverage>,
) -> Result<(), TestCaseError> {
    let queries = queries(world);
    let (want, want_published) = reference_answers(world, warm.mode, &queries);
    let mut mismatches = Vec::new();
    let mut differ = |label: &str, got: &Value, want: &Value| {
        if got != want {
            mismatches.push(format!(
                "{label}\n  warm:  {}\n  fresh: {}",
                short(got),
                short(want)
            ));
        }
    };

    // What the client is displaying.
    for file in world.open.keys() {
        let uri = world.uri(*file);
        if warm.mode == Mode::Pull {
            let got = warm
                .pulled
                .get(&uri)
                .map_or(Value::Null, |(_, items)| items.clone());
            let want = &want[&format!("diagnostic {file:?}")]["result"]["items"];
            differ(&format!("displayed pull diagnostics {file:?}"), &got, want);
        }
        if let FileId::Src(_) = file {
            let got = warm.tokens.get(&uri).cloned().unwrap_or(Value::Null);
            let want = &want[&format!("semanticTokens {file:?}")];
            differ(&format!("displayed semantic tokens {file:?}"), &got, want);
        }
    }
    if warm.mode == Mode::Push {
        // A URI the client never received anything for displays nothing, the
        // same as an empty publish.
        let uris: BTreeSet<&String> = warm.published.keys().chain(want_published.keys()).collect();
        for uri in uris {
            let got = warm
                .published
                .get(uri)
                .cloned()
                .unwrap_or_else(|| json!([]));
            let want = want_published
                .get(uri)
                .cloned()
                .unwrap_or_else(|| json!([]));
            differ(
                &format!("displayed published diagnostics {uri}"),
                &got,
                &want,
            );
        }
    }

    // Direct queries, `references` last: the warm session extends whatever
    // prefix folds its history left, where the reference folds cold.
    let mut ordered: Vec<&Query> = queries.iter().collect();
    ordered.sort_by_key(|q| q.method == "textDocument/references");
    {
        let mut coverage = coverage.borrow_mut();
        coverage.checks += 1;
        for q in ordered {
            let got = warm.request(q.method, q.params.clone());
            let want = &want[&q.label];
            coverage.record(q, want);
            differ(&q.label, &got, want);
        }
    }
    // The comparison's own requests may have folded projects and so owed
    // refreshes; drain them so they do not leak into the next command.
    warm.sync_views(&world.open_uris(), None);
    if mismatches.is_empty() {
        return Ok(());
    }
    let total = mismatches.len();
    mismatches.truncate(6);
    Err(TestCaseError::fail(format!(
        "{total} answer(s) differ from a fresh session's.\n\n{}\n\nWorld:\n{}",
        mismatches.join("\n\n"),
        world.describe()
    )))
}

// ---------------------------------------------------------------------------
// Running a case.
// ---------------------------------------------------------------------------

/// A fresh directory for one world, under Cargo's per-target scratch directory
/// rather than the system temp dir. The server walks every ancestor of a source
/// file looking for a `.fsproj`, listing each directory on every request; a
/// shared system temp dir can hold thousands of entries (and, from unrelated
/// tools, stray `.fsproj` files the walk would then find).
fn world_dir() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("ism-")
        .tempdir_in(env!("CARGO_TARGET_TMPDIR"))
        .unwrap()
}

fn write_initial_disk(root: &Path, disk: &[(FileId, Content)]) {
    // The assets file's `packageFolders` entry must exist; nothing is in it.
    fs::create_dir_all(root.join("pkgs")).unwrap();
    for (file, content) in disk {
        let path = root.join(file.rel());
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, content.bytes(*file)).unwrap();
    }
}

fn new_world(disk: &[(FileId, Content)]) -> (tempfile::TempDir, World) {
    let tmp = world_dir();
    // Canonical, so every URI the model builds is the spelling the server's own
    // path walks produce (macOS temp dirs sit behind a `/var` symlink).
    let root = fs::canonicalize(tmp.path()).unwrap();
    write_initial_disk(&root, disk);
    let world = World {
        root,
        open: BTreeMap::new(),
        versions: BTreeMap::new(),
        pending: Vec::new(),
    };
    (tmp, world)
}

fn run_case(case: &Case, coverage: &RefCell<Coverage>) -> Result<(), TestCaseError> {
    let (_tmp, mut world) = new_world(&case.disk);
    let mut warm = Client::start(&world.root, case.mode);
    warm.settle();
    for cmd in &case.cmds {
        apply(&mut warm, &mut world, cmd, coverage)?;
    }
    apply(&mut warm, &mut world, &Cmd::Flush, coverage)?;
    apply(&mut warm, &mut world, &Cmd::Check, coverage)
}

fn apply(
    warm: &mut Client,
    world: &mut World,
    cmd: &Cmd,
    coverage: &RefCell<Coverage>,
) -> Result<(), TestCaseError> {
    match cmd {
        Cmd::Open(file) => {
            if world.open.contains_key(file) {
                return Ok(());
            }
            let Some(text) = world.disk(*file) else {
                return Ok(());
            };
            let uri = world.uri(*file);
            did_open(warm, &uri, &text);
            world.open.insert(*file, text);
            world.versions.insert(*file, 1);
            warm.sync_views(&world.open_uris(), Some(&uri));
        }
        Cmd::Edit(file, content) => {
            if !world.open.contains_key(file) {
                return Ok(());
            }
            let text = content.render(*file);
            let uri = world.uri(*file);
            let version = world.versions.entry(*file).or_insert(1);
            *version += 1;
            warm.notify(
                "textDocument/didChange",
                json!({
                    "textDocument": { "uri": uri, "version": *version },
                    "contentChanges": [{ "text": text }],
                }),
            );
            world.open.insert(*file, text);
            warm.sync_views(&world.open_uris(), Some(&uri));
        }
        Cmd::Close(file) => {
            if world.open.remove(file).is_none() {
                return Ok(());
            }
            world.versions.remove(file);
            let uri = world.uri(*file);
            warm.notify(
                "textDocument/didClose",
                json!({ "textDocument": { "uri": uri } }),
            );
            warm.forget(&uri);
            // A pull client re-pulls its other open documents on any sync.
            let open = world.open_uris();
            warm.sync_views(&open, Some(&uri));
        }
        Cmd::Save(file) => {
            let Some(text) = world.open.get(file).cloned() else {
                return Ok(());
            };
            world.write(*file, text.as_bytes());
        }
        Cmd::Write(file, content) => {
            world.write(*file, &content.bytes(*file));
        }
        Cmd::Delete(file) => {
            world.delete(*file);
        }
        Cmd::Flush => {
            if world.pending.is_empty() {
                return Ok(());
            }
            let changes = std::mem::take(&mut world.pending);
            warm.notify(
                "workspace/didChangeWatchedFiles",
                json!({ "changes": changes }),
            );
            warm.sync_views(&world.open_uris(), None);
        }
        Cmd::Check => {
            if world.pending.is_empty() {
                check(warm, world, coverage)?;
            }
        }
        Cmd::Peek(src) => {
            let file = FileId::Src(*src);
            let uri = world.uri(file);
            // Diagnostics read a closed file from disk, evaluating its project
            // on the way; the rest fold the project, but only for an open buffer.
            warm.request(
                "textDocument/diagnostic",
                json!({ "textDocument": { "uri": uri } }),
            );
            if let Some(text) = world.open.get(&file).cloned() {
                warm.request(
                    "textDocument/semanticTokens/full",
                    json!({ "textDocument": { "uri": uri } }),
                );
                for (line, character) in positions(&text).0 {
                    warm.request(
                        "textDocument/hover",
                        json!({ "textDocument": { "uri": uri }, "position": { "line": line, "character": character } }),
                    );
                }
            }
            warm.sync_views(&world.open_uris(), None);
        }
    }
    Ok(())
}

/// The default run's case count: small enough for the ordinary suite. A soak
/// sets `BORZOI_LSP_STATE_MACHINE_CASES` (and, to explore beyond the default
/// run's fixed seed, `BORZOI_LSP_STATE_MACHINE_SEED`).
const DEFAULT_CASES: u32 = 16;

/// The default run's seed. Fixed, so the default run — and the coverage floor it
/// asserts — is reproducible; a soak picks its own.
const DEFAULT_SEED: u64 = 0x5eed_0f15_7a7e;

fn env_u64(name: &str) -> Option<u64> {
    std::env::var(name)
        .ok()
        .map(|v| v.parse().unwrap_or_else(|_| panic!("{name} is a number")))
}

fn runner(cases: u32, seed: u64) -> TestRunner {
    let config = Config {
        cases,
        // A persisted seed replays only as long as the strategy is unchanged;
        // the shrunk script in the failure message is the durable record.
        failure_persistence: None,
        ..Config::default()
    };
    let mut bytes = [0u8; 32];
    bytes[..8].copy_from_slice(&seed.to_le_bytes());
    TestRunner::new_with_rng(config, TestRng::from_seed(RngAlgorithm::ChaCha, &bytes))
}

/// Every one-step simplification of a source: drop one item, or replace an
/// `#if` block by one of its branches, at any depth.
fn simpler_items(items: &[Item]) -> Vec<Vec<Item>> {
    let mut out = Vec::new();
    for (i, item) in items.iter().enumerate() {
        let splice = |with: &[Item]| {
            let mut v = items[..i].to_vec();
            v.extend_from_slice(with);
            v.extend_from_slice(&items[i + 1..]);
            v
        };
        out.push(splice(&[]));
        if let Item::IfDef(sym, then, els) = item {
            out.push(splice(then));
            out.push(splice(els));
            for t in simpler_items(then) {
                out.push(splice(&[Item::IfDef(*sym, t, els.clone())]));
            }
            for e in simpler_items(els) {
                out.push(splice(&[Item::IfDef(*sym, then.clone(), e)]));
            }
        }
    }
    out
}

/// Every one-step simplification of a case, cheapest-to-try first: drop a
/// command, then simplify one source a command or the initial disk carries.
fn simpler_cases(case: &Case) -> Vec<Case> {
    let mut out = Vec::new();
    for i in 0..case.cmds.len() {
        let mut c = case.clone();
        c.cmds.remove(i);
        out.push(c);
    }
    for i in 0..case.disk.len() {
        let mut c = case.clone();
        c.disk.remove(i);
        out.push(c);
    }
    for i in 0..case.cmds.len() {
        if let Cmd::Edit(_, Content::Source(items)) | Cmd::Write(_, Content::Source(items)) =
            &case.cmds[i]
        {
            for simpler in simpler_items(items) {
                let mut c = case.clone();
                match &mut c.cmds[i] {
                    Cmd::Edit(_, content) | Cmd::Write(_, content) => {
                        *content = Content::Source(simpler);
                    }
                    _ => unreachable!(),
                }
                out.push(c);
            }
        }
    }
    for i in 0..case.disk.len() {
        if let (_, Content::Source(items)) = &case.disk[i] {
            for simpler in simpler_items(items) {
                let mut c = case.clone();
                c.disk[i].1 = Content::Source(simpler);
                out.push(c);
            }
        }
    }
    out
}

/// Replays [`minimise`] may spend. Each is a whole case (around a second), so
/// this bounds how long a failure takes to report.
const MAX_MINIMISE_REPLAYS: usize = 400;

/// Finish what proptest's shrinker leaves undone. Its `Vec` shrinker stops
/// removing elements once it starts simplifying them, and it barely shrinks
/// through the `prop_flat_map` that pairs a file with its content, so a shrunk
/// script keeps commands that no longer matter and sources full of irrelevant
/// lines. Greedily take any one-step simplification that still fails, to a
/// fixpoint (or the replay budget).
fn minimise(mut case: Case) -> (Case, String) {
    let scratch = RefCell::new(Coverage::default());
    let failure = |c: &Case| run_case(c, &scratch).err().map(|e| e.to_string());
    let mut message = failure(&case).expect("the case proptest reported fails when replayed");
    let mut replays = 0;
    'outer: loop {
        for trial in simpler_cases(&case) {
            if replays == MAX_MINIMISE_REPLAYS {
                message.push_str("\n\n(minimisation stopped at its replay budget)");
                break 'outer;
            }
            replays += 1;
            if let Some(m) = failure(&trial) {
                case = trial;
                message = m;
                continue 'outer;
            }
        }
        break;
    }
    (case, message)
}

/// For all generated workspaces and client scripts, every answer the long-lived
/// session gives — and everything its client is displaying — equals what fresh
/// sessions over the same disk and buffers give.
#[test]
fn warm_session_agrees_with_a_fresh_one() {
    let cases = env_u64("BORZOI_LSP_STATE_MACHINE_CASES").map_or(DEFAULT_CASES, |n| n as u32);
    let seed = env_u64("BORZOI_LSP_STATE_MACHINE_SEED").unwrap_or(DEFAULT_SEED);
    let coverage = RefCell::new(Coverage::default());
    let result = runner(cases, seed).run(&case_strategy(), |case| run_case(&case, &coverage));
    let coverage = coverage.into_inner();
    eprintln!("seed {seed}: {coverage:#?}");
    match result {
        Ok(()) => {}
        Err(TestError::Fail(_, case)) => {
            let (case, message) = minimise(case);
            panic!("seed {seed}: {message}\n\nminimal script: {case:?}");
        }
        Err(err) => panic!("seed {seed}: {err}"),
    }
    // Non-vacuity: the comparison above passes trivially if every answer is
    // `null`, which a generator drift or a handler regression would make
    // silently true. Every kind of answer the property is meant to guard must
    // have been compared at least once.
    assert!(
        coverage.cross_file_definitions > 0
            && coverage.assembly_member_hovers > 0
            && coverage.completions > 0
            && coverage.nonempty_diagnostics > 0
            && coverage.references > 0,
        "the generated cases compared no answers of some kind: {coverage:#?}"
    );
}

/// The reference must be a function of the world, or the property above is
/// noise: a session that has just opened every buffer — with no history but
/// that — agrees with the per-group fresh sessions, even though it answers every
/// group's queries in one session and in a different order.
#[test]
fn fresh_sessions_agree_with_each_other() {
    let coverage = RefCell::new(Coverage::default());
    let result = runner(8, DEFAULT_SEED).run(&case_strategy(), |case| {
        let (_tmp, mut world) = new_world(&case.disk);
        let mut first = Client::start(&world.root, case.mode);
        first.settle();
        for file in FILES.into_iter().filter(|f| f.is_buffer()) {
            apply(&mut first, &mut world, &Cmd::Open(file), &coverage)?;
        }
        check(&mut first, &world, &coverage)
    });
    if let Err(err) = result {
        panic!("{err}");
    }
}

// ---------------------------------------------------------------------------
// Pinned scripts.
// ---------------------------------------------------------------------------

fn source(items: Vec<Item>) -> Content {
    Content::Source(items)
}

fn project(defines: Vec<Sym>, compile: Vec<Src>) -> Content {
    Content::Project(ProjectSpec {
        import_props: false,
        defines,
        compile: compile.into_iter().map(Entry::File).collect(),
        broken: false,
    })
}

fn x_is_zero() -> Item {
    Item::Let(Name::X, Expr::Lit(0))
}

/// Minimal scripts the generated run found against planted defects, replayed on
/// every run. The generated run finds each of these from its fixed seed today,
/// but that is a property of the current generator weights; a pinned script
/// keeps catching its defect however the generator drifts. Each names the
/// defect it was found against.
fn pinned_scripts() -> Vec<(&'static str, Case)> {
    vec![
        (
            // The per-file parse cache keyed without the `#if` symbols: `P`
            // folds `B` (for `A`'s queries) under no symbols, and `O` — which
            // owns `B` and defines `FOO` — is then served that tree, so `x`
            // shows through the `#if FOO … #else` that hides it.
            "a shared file parsed under one project's defines is not served to another",
            Case {
                mode: Mode::Push,
                disk: vec![
                    (FileId::Proj(Proj::P), project(vec![], vec![Src::A, Src::B])),
                    (FileId::Proj(Proj::O), project(vec![Sym::Foo], vec![Src::B])),
                    (FileId::Src(Src::A), source(vec![])),
                    (
                        FileId::Src(Src::B),
                        source(vec![Item::IfDef(Sym::Foo, vec![], vec![x_is_zero()])]),
                    ),
                ],
                cmds: vec![
                    Cmd::Open(FileId::Src(Src::A)),
                    Cmd::Open(FileId::Src(Src::B)),
                ],
            },
        ),
        (
            // `didClose` not invalidating: `O`'s fold keeps `B`'s buffer text
            // after the buffer closes, so `workspace/symbol` (reached through
            // `A`, which `O` owns by the ancestor fallback) misses disk's `x`.
            "closing a buffer reverts its project to the disk text",
            Case {
                mode: Mode::Push,
                disk: vec![
                    (FileId::Proj(Proj::P), project(vec![], vec![])),
                    (FileId::Proj(Proj::O), project(vec![], vec![Src::B])),
                    (FileId::Src(Src::A), source(vec![])),
                    (FileId::Src(Src::B), source(vec![x_is_zero()])),
                ],
                cmds: vec![
                    Cmd::Open(FileId::Src(Src::A)),
                    Cmd::Open(FileId::Src(Src::B)),
                    Cmd::Edit(FileId::Src(Src::B), source(vec![])),
                    Cmd::Close(FileId::Src(Src::B)),
                ],
            },
        ),
        (
            // A watched content change to a closed source not invalidating:
            // `B`'s open folded `P` with `A`'s old disk text, and `A.x` stays
            // bound after `A` is rewritten without it.
            "an external write to a closed source reaches the files after it",
            Case {
                mode: Mode::Push,
                disk: vec![
                    (FileId::Proj(Proj::P), project(vec![], vec![Src::A, Src::B])),
                    (FileId::Src(Src::A), source(vec![x_is_zero()])),
                    (
                        FileId::Src(Src::B),
                        source(vec![Item::Let(Name::Y, Expr::Qual(Src::A, Name::X))]),
                    ),
                ],
                cmds: vec![
                    Cmd::Open(FileId::Src(Src::B)),
                    Cmd::Write(FileId::Src(Src::A), source(vec![])),
                ],
            },
        ),
        (
            // A pull `resultId` that omits the `#if` symbols: after `P` starts
            // defining `FOO`, the client's re-pull echoes its id, the server
            // answers `Unchanged`, and the parse error now active under `FOO`
            // is never shown.
            "a define change reaches a pull client's cached diagnostics",
            Case {
                mode: Mode::Pull,
                disk: vec![
                    (FileId::Proj(Proj::P), project(vec![], vec![Src::A])),
                    (
                        FileId::Src(Src::A),
                        source(vec![Item::IfDef(Sym::Foo, vec![Item::Garbage], vec![])]),
                    ),
                ],
                cmds: vec![
                    Cmd::Open(FileId::Src(Src::A)),
                    Cmd::Write(FileId::Proj(Proj::P), project(vec![Sym::Foo], vec![Src::A])),
                ],
            },
        ),
        (
            // An edit not owing a `semanticTokens/refresh`: the client re-pulls
            // only the buffer it edited (`A`), so `B`'s tokens for `A.x` keep
            // classifying a binding the edit removed.
            "an edit refreshes the tokens of the files after it",
            Case {
                mode: Mode::Push,
                disk: vec![
                    (FileId::Proj(Proj::P), project(vec![], vec![Src::A, Src::B])),
                    (FileId::Src(Src::A), source(vec![x_is_zero()])),
                    (
                        FileId::Src(Src::B),
                        source(vec![Item::Let(Name::Y, Expr::Qual(Src::A, Name::X))]),
                    ),
                ],
                cmds: vec![
                    Cmd::Open(FileId::Src(Src::A)),
                    Cmd::Open(FileId::Src(Src::B)),
                    Cmd::Edit(FileId::Src(Src::A), source(vec![])),
                ],
            },
        ),
        (
            // A watched `.dll` change not invalidating the assembly env: the
            // framework assembly is deleted, and `x.Length` stays a resolved
            // `System.String` member.
            "a referenced assembly's deletion reaches the assembly env",
            Case {
                mode: Mode::Push,
                disk: vec![
                    (FileId::Proj(Proj::P), project(vec![], vec![Src::A])),
                    (
                        FileId::Src(Src::A),
                        source(vec![Item::StrPair(Name::X, Name::Y)]),
                    ),
                    (FileId::Assets(Proj::P), Content::Assets),
                    (FileId::Dll, Content::Dll),
                ],
                cmds: vec![Cmd::Open(FileId::Src(Src::A)), Cmd::Delete(FileId::Dll)],
            },
        ),
        (
            // A structural change not republishing the open buffers: after `P`
            // starts defining `FOO`, a push client keeps showing no error for
            // the line now active under it.
            "a define change reaches a push client's diagnostics",
            Case {
                mode: Mode::Push,
                disk: vec![
                    (FileId::Proj(Proj::P), project(vec![], vec![Src::A])),
                    (
                        FileId::Src(Src::A),
                        source(vec![Item::IfDef(Sym::Foo, vec![Item::Garbage], vec![])]),
                    ),
                ],
                cmds: vec![
                    Cmd::Open(FileId::Src(Src::A)),
                    Cmd::Write(FileId::Proj(Proj::P), project(vec![Sym::Foo], vec![Src::A])),
                ],
            },
        ),
    ]
}

/// Every pinned script agrees with the fresh-session reference.
#[test]
fn pinned_scripts_agree_with_a_fresh_session() {
    let coverage = RefCell::new(Coverage::default());
    let failures: Vec<String> = pinned_scripts()
        .into_iter()
        .filter_map(|(name, case)| {
            run_case(&case, &coverage)
                .err()
                .map(|err| format!("pinned script `{name}` failed: {err}\n\nscript: {case:?}"))
        })
        .collect();
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}
