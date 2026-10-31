//! The **contributor name oracle**: a generated property that every bare
//! name FCS says a contributor introduces is one the walk marks.
//!
//! The contributor marks (`docs/unmodelled-contributors.md`) are name sets
//! read off declarations, and a name a declaration binds but the scan misses
//! is a wrong target: the walk commits the earlier binder the contributor hid.
//! The hand-written grid (`unmodelled_contributor_grid.rs`) finds those one
//! shape at a time. This generates the shapes instead: contributors of every
//! kind (`[<AutoOpen>]` modules, aliased, nested, `rec`, an earlier file's
//! root, a signature-only `[<AutoOpen>]`) holding random declarations of every
//! kind (values, functions, literals, patterns, active patterns, unions,
//! operator cases, records, classes and structs with every kind of static,
//! enums, exceptions, abbreviations, delegates, externs, nested modules,
//! augmentations).
//!
//! Each program binds every candidate name before the contributor, then uses
//! each bare after it. The candidates are the names the generator wrote, their
//! accessor forms, the compiled operator names, and every name FCS reports as
//! defined inside the contributor. Two properties follow:
//!
//! - **ours vs FCS**: at every probe where we commit, FCS names the same
//!   symbol. A missed contributor name commits the earlier binder where FCS
//!   names the contributor's, and fails here.
//! - **the generator vs FCS** (the absolute property): every declaration kind
//!   the generator claims introduces a bare name does, in FCS, at least once
//!   over the run. A kind the generator cannot build, or builds as something
//!   that introduces nothing, fails here rather than passing the first property
//!   vacuously.
//!
//! Default runtime is modest; `BORZOI_CONTRIB_ORACLE_SEEDS=<n>` widens it, and
//! `BORZOI_CONTRIB_ORACLE_DUMP=<seed>` prints that seed's project.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::common::{
    ensure_fsharp_core_dll, env_usize_or, invoke_fcs_dump_project_with_refs,
    parse_fcs_uses_project, temp_fs_tree,
};
use crate::resolve_signatures::project;
use crate::unmodelled_contributor_grid::{agrees, env_of, fcs_at, render_fcs};
use borzoi_sema::{Resolution, resolve_project_files};
use rowan::TextRange;

/// Seeds checked against FCS by default.
const DEFAULT_SEEDS: usize = 48;

/// A small deterministic generator (xorshift64*).
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        usize::try_from(self.next() % (n as u64)).unwrap()
    }
}

/// One kind of declaration the generator writes inside a contributor.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum DeclKind {
    LetValue,
    LetFunction,
    LetLiteral,
    LetTuple,
    PartialPattern,
    TotalPattern,
    Union,
    OperatorCases,
    RqaUnion,
    StructUnion,
    Record,
    StructRecord,
    Class,
    AutoOpenClass,
    StructType,
    Enum,
    Exception,
    AutoOpenException,
    Abbreviation,
    BarlessUnion,
    Delegate,
    Extern,
    NestedAutoOpen,
    NestedPlain,
    /// An `[<AutoOpen>]` class, then an intrinsic augmentation of it holding
    /// every member kind ([`Unit::AugmentationMember`]).
    Augmentation,
    /// A class that is not `[<AutoOpen>]`, then an intrinsic augmentation of
    /// it holding every member kind: nothing it declares is bare.
    PlainAugmentation,
    /// An optional augmentation (an extension) of a referenced type: its
    /// members are reachable only through the type.
    ExtensionAugmentation,
}

impl DeclKind {
    const ALL: [DeclKind; 27] = [
        DeclKind::LetValue,
        DeclKind::LetFunction,
        DeclKind::LetLiteral,
        DeclKind::LetTuple,
        DeclKind::PartialPattern,
        DeclKind::TotalPattern,
        DeclKind::Union,
        DeclKind::OperatorCases,
        DeclKind::RqaUnion,
        DeclKind::StructUnion,
        DeclKind::Record,
        DeclKind::StructRecord,
        DeclKind::Class,
        DeclKind::AutoOpenClass,
        DeclKind::StructType,
        DeclKind::Enum,
        DeclKind::Exception,
        DeclKind::AutoOpenException,
        DeclKind::Abbreviation,
        DeclKind::BarlessUnion,
        DeclKind::Delegate,
        DeclKind::Extern,
        DeclKind::NestedAutoOpen,
        DeclKind::NestedPlain,
        DeclKind::Augmentation,
        DeclKind::PlainAugmentation,
        DeclKind::ExtensionAugmentation,
    ];

    /// Whether the kind writes a signature for itself, so it can sit in a
    /// signature-only `[<AutoOpen>]` contributor.
    fn has_signature(self) -> bool {
        matches!(
            self,
            DeclKind::LetValue
                | DeclKind::LetFunction
                | DeclKind::Union
                | DeclKind::RqaUnion
                | DeclKind::Record
                | DeclKind::Enum
                | DeclKind::Exception
                | DeclKind::Abbreviation
                | DeclKind::BarlessUnion
                | DeclKind::Delegate
        )
    }

    /// Whether the kind checks cleanly inside a `module rec` whose probes come
    /// first. FCS refuses an exception's members and a module-level tuple
    /// pattern there, and a class's statics take their types from the probes'
    /// uses before their own definitions.
    fn rec_safe(self) -> bool {
        !matches!(
            self,
            DeclKind::LetTuple
                | DeclKind::AutoOpenException
                | DeclKind::Class
                | DeclKind::AutoOpenClass
                | DeclKind::StructType
                | DeclKind::Augmentation
                | DeclKind::PlainAugmentation
                | DeclKind::ExtensionAugmentation
        )
    }

    /// Whether the generator's model says the kind puts some bare name in
    /// scope where its contributor is folded. Checked against FCS: each such
    /// kind must introduce at least one probed name over the run.
    fn introduces(self) -> bool {
        // A struct record's only bare name is its type's, which the probes
        // leave out: bare, it is no value FCS accepts (the grid covers its
        // constructor slot).
        !matches!(
            self,
            DeclKind::RqaUnion
                | DeclKind::Record
                | DeclKind::StructRecord
                | DeclKind::Abbreviation
                | DeclKind::NestedPlain
                | DeclKind::ExtensionAugmentation
        )
    }
}

/// One kind of member an auto-open class's statics may hold.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum MemberKind {
    StaticProperty,
    StaticMethod,
    StaticAuto,
    StaticGetSet,
    Indexer,
    Operator,
    Event,
    Instance,
}

impl MemberKind {
    const ALL: [MemberKind; 8] = [
        MemberKind::StaticProperty,
        MemberKind::StaticMethod,
        MemberKind::StaticAuto,
        MemberKind::StaticGetSet,
        MemberKind::Indexer,
        MemberKind::Operator,
        MemberKind::Event,
        MemberKind::Instance,
    ];

    /// Whether an `[<AutoOpen>]` class's member of the kind puts a bare name
    /// in scope, by the generator's model (checked against FCS like
    /// [`DeclKind::introduces`]).
    /// FCS adds no static operator to bare scope: `(+.)` after the fold
    /// resolves to nothing.
    fn introduces(self) -> bool {
        !matches!(self, MemberKind::Instance | MemberKind::Operator)
    }
}

/// What a generated unit is, for the coverage census: a declaration, or a
/// member of a generated class.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum Unit {
    Decl(DeclKind),
    Member(MemberKind),
    /// A member of an intrinsic augmentation of an `[<AutoOpen>]` class.
    AugmentationMember(MemberKind),
}

/// Where the contributor sits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Context {
    /// `[<AutoOpen>] module` before the probes.
    Module,
    /// The same, its attribute spelled through an abbreviation.
    Aliased,
    /// An `[<AutoOpen>]` module inside an `[<AutoOpen>]` module.
    Nested,
    /// A `module rec` whose declarations follow its probes.
    Rec,
    /// An earlier file's root-level `[<AutoOpen>] module`.
    Root,
    /// The same, with only its signature saying `[<AutoOpen>]`.
    Signature,
}

impl Context {
    const ALL: [Context; 6] = [
        Context::Module,
        Context::Aliased,
        Context::Nested,
        Context::Rec,
        Context::Root,
        Context::Signature,
    ];
}

/// A generated declaration: its lines (unindented), its signature's lines if
/// it has one, the names the generator knows it may bind, and the span (into
/// its own lines' text, line by line) of each unit it holds.
struct Decl {
    lines: Vec<(String, Unit)>,
    sig: Vec<String>,
    names: Vec<String>,
}

fn member(kind: MemberKind, i: usize, n: usize, d: &mut Decl, unit: Unit) {
    let mut push = |line: String| d.lines.push((format!("    {line}"), unit));
    match kind {
        MemberKind::StaticProperty => {
            push(format!("static member OraKp{i}x{n} = {i}"));
            d.names.push(format!("OraKp{i}x{n}"));
        }
        MemberKind::StaticMethod => {
            push(format!(
                "static member oraKm{i}x{n} (p{i}x{n}: int) = p{i}x{n}"
            ));
            d.names.push(format!("oraKm{i}x{n}"));
            d.names.push(format!("p{i}x{n}"));
        }
        MemberKind::StaticAuto => {
            push(format!(
                "static member val OraKa{i}x{n} = {i} with get, set"
            ));
            d.names.push(format!("OraKa{i}x{n}"));
        }
        MemberKind::StaticGetSet => {
            push(format!("static member OraKg{i}x{n}"));
            push(format!("    with get () = {i}"));
            push("    and set (_: int) = ()".to_string());
            d.names.push(format!("OraKg{i}x{n}"));
        }
        MemberKind::Indexer => {
            push("static member Item".to_string());
            push("    with get (q: int) = q".to_string());
            d.names.push("Item".to_string());
        }
        MemberKind::Operator => {
            push("static member (+.) (a: int, b: int) = a + b".to_string());
            d.names.push("op_PlusDot".to_string());
        }
        MemberKind::Event => {
            push("[<CLIEvent>]".to_string());
            push(format!("static member OraKe{i}x{n} = oraEv{i}.Publish"));
            d.names.push(format!("OraKe{i}x{n}"));
        }
        MemberKind::Instance => {
            // A named self identifier, which is no bare name of the type's.
            push(format!(
                "member oraSelf{i}x{n}.OraKi{i}x{n} (q{i}x{n}: int) = q{i}x{n}"
            ));
            d.names.extend([
                format!("OraKi{i}x{n}"),
                format!("oraSelf{i}x{n}"),
                format!("q{i}x{n}"),
            ]);
        }
    }
}

fn decl(kind: DeclKind, i: usize, rng: &mut Rng) -> Decl {
    let unit = Unit::Decl(kind);
    let mut d = Decl {
        lines: Vec::new(),
        sig: Vec::new(),
        names: Vec::new(),
    };
    let lines = |d: &mut Decl, ls: &[String]| {
        for l in ls {
            d.lines.push((l.clone(), unit));
        }
    };
    match kind {
        DeclKind::LetValue => {
            lines(&mut d, &[format!("let oraV{i} = {i}")]);
            d.sig = vec![format!("val oraV{i}: int")];
            d.names.push(format!("oraV{i}"));
        }
        DeclKind::LetFunction => {
            lines(&mut d, &[format!("let oraF{i} (p{i}: int) = p{i} + {i}")]);
            d.sig = vec![format!("val oraF{i}: p{i}: int -> int")];
            d.names.extend([format!("oraF{i}"), format!("p{i}")]);
        }
        DeclKind::LetLiteral => {
            lines(
                &mut d,
                &["[<Literal>]".to_string(), format!("let OraLit{i} = {i}")],
            );
            d.names.push(format!("OraLit{i}"));
        }
        DeclKind::LetTuple => {
            lines(&mut d, &[format!("let (oraTa{i}, oraTb{i}) = ({i}, {i})")]);
            d.names.extend([format!("oraTa{i}"), format!("oraTb{i}")]);
        }
        DeclKind::PartialPattern => {
            lines(
                &mut d,
                &[format!(
                    "let (|OraP{i}|_|) (p{i}: int) = if p{i} > 0 then Option.Some () else Option.None"
                )],
            );
            d.names.extend([format!("OraP{i}"), format!("p{i}")]);
        }
        DeclKind::TotalPattern => {
            lines(
                &mut d,
                &[format!(
                    "let (|OraA{i}|OraB{i}|) (p{i}: int) = if p{i} > 0 then OraA{i} else OraB{i}"
                )],
            );
            d.names
                .extend([format!("OraA{i}"), format!("OraB{i}"), format!("p{i}")]);
        }
        DeclKind::Union => {
            let l = format!("type OraU{i} = OraC{i} of int | OraD{i}");
            lines(&mut d, std::slice::from_ref(&l));
            d.sig = vec![l];
            d.names
                .extend([format!("OraU{i}"), format!("OraC{i}"), format!("OraD{i}")]);
        }
        DeclKind::OperatorCases => {
            lines(
                &mut d,
                &[
                    format!("type OraLs{i} ="),
                    "    | ([])".to_string(),
                    format!("    | (::) of int * OraLs{i}"),
                ],
            );
            d.names
                .extend(["op_Nil".to_string(), "op_ColonColon".to_string()]);
        }
        DeclKind::RqaUnion => {
            let ls = vec![
                "[<RequireQualifiedAccess>]".to_string(),
                format!("type OraQ{i} = OraQc{i} of int | OraQd{i}"),
            ];
            lines(&mut d, &ls);
            d.sig = ls;
            d.names.extend([format!("OraQc{i}"), format!("OraQd{i}")]);
        }
        DeclKind::StructUnion => {
            lines(
                &mut d,
                &[
                    "[<Struct>]".to_string(),
                    format!("type OraSu{i} = OraSc{i} of int"),
                ],
            );
            d.names.push(format!("OraSc{i}"));
        }
        DeclKind::Record => {
            let l = format!("type OraR{i} = {{ OraRf{i}: int }}");
            lines(&mut d, std::slice::from_ref(&l));
            d.sig = vec![l];
            d.names.extend([format!("OraR{i}"), format!("OraRf{i}")]);
        }
        DeclKind::StructRecord => {
            lines(
                &mut d,
                &[
                    "[<Struct>]".to_string(),
                    format!("type OraSr{i} = {{ OraSrf{i}: int }}"),
                ],
            );
            d.names.push(format!("OraSrf{i}"));
        }
        DeclKind::Class | DeclKind::AutoOpenClass => {
            if kind == DeclKind::AutoOpenClass {
                lines(&mut d, &["[<AutoOpen>]".to_string()]);
            }
            lines(
                &mut d,
                &[
                    format!("type OraK{i}() ="),
                    format!("    static let oraEv{i} = Event<int>()"),
                ],
            );
            d.names.push(format!("OraK{i}"));
            if kind == DeclKind::AutoOpenClass {
                // Every member kind, so each is graded where it is folded.
                for (n, m) in MemberKind::ALL.into_iter().enumerate() {
                    member(m, i, n, &mut d, Unit::Member(m));
                }
            } else {
                let count = 1 + rng.below(3);
                let mut operator = false;
                let mut indexer = false;
                for n in 0..count {
                    let mut m = MemberKind::ALL[rng.below(MemberKind::ALL.len())];
                    // One operator and one indexer per class: a second is a
                    // duplicate definition.
                    if (m == MemberKind::Operator && operator)
                        || (m == MemberKind::Indexer && indexer)
                    {
                        m = MemberKind::StaticMethod;
                    }
                    operator |= m == MemberKind::Operator;
                    indexer |= m == MemberKind::Indexer;
                    member(m, i, n, &mut d, unit);
                }
            }
        }
        DeclKind::StructType => {
            lines(
                &mut d,
                &[
                    "[<Struct>]".to_string(),
                    format!("type OraSt{i} ="),
                    format!("    val OraStv{i}: int"),
                    format!("    static member OraSts{i} = {i}"),
                ],
            );
            d.names.extend([
                format!("OraSt{i}"),
                format!("OraStv{i}"),
                format!("OraSts{i}"),
            ]);
        }
        DeclKind::Enum => {
            let l = format!("type OraE{i} = OraEa{i} = 0 | OraEb{i} = 1");
            lines(&mut d, std::slice::from_ref(&l));
            d.sig = vec![l];
            d.names
                .extend([format!("OraE{i}"), format!("OraEa{i}"), format!("OraEb{i}")]);
        }
        DeclKind::Exception => {
            let l = format!("exception OraX{i} of int");
            lines(&mut d, std::slice::from_ref(&l));
            d.sig = vec![l];
            d.names.push(format!("OraX{i}"));
        }
        DeclKind::AutoOpenException => {
            lines(
                &mut d,
                &[
                    "[<AutoOpen>]".to_string(),
                    format!("exception OraXa{i} of int with"),
                    format!("    static member OraXs{i} = {i}"),
                ],
            );
            d.names.extend([format!("OraXa{i}"), format!("OraXs{i}")]);
        }
        DeclKind::Abbreviation => {
            let l = format!("type OraAb{i} = int");
            lines(&mut d, std::slice::from_ref(&l));
            d.sig = vec![l];
            d.names.push(format!("OraAb{i}"));
        }
        DeclKind::BarlessUnion => {
            let l = format!("type OraBu{i} = OraBc{i}");
            lines(&mut d, std::slice::from_ref(&l));
            d.sig = vec![l];
            d.names.extend([format!("OraBu{i}"), format!("OraBc{i}")]);
        }
        DeclKind::Delegate => {
            let l = format!("type OraDl{i} = delegate of int -> int");
            lines(&mut d, std::slice::from_ref(&l));
            d.sig = vec![l];
            // A delegate's bare name is no value FCS accepts; the grid
            // covers its constructor slot.
        }
        DeclKind::Extern => {
            lines(
                &mut d,
                &[
                    "[<System.Runtime.InteropServices.DllImport(\"ora\")>]".to_string(),
                    format!("extern int oraExt{i}(int x)"),
                ],
            );
            d.names.push(format!("oraExt{i}"));
        }
        DeclKind::NestedAutoOpen => {
            lines(
                &mut d,
                &[
                    "[<AutoOpen>]".to_string(),
                    format!("module OraNa{i} ="),
                    format!("    let oraNav{i} = {i}"),
                ],
            );
            d.names.push(format!("oraNav{i}"));
        }
        DeclKind::NestedPlain => {
            lines(
                &mut d,
                &[
                    format!("module OraNp{i} ="),
                    format!("    let oraNpv{i} = {i}"),
                ],
            );
            d.names.push(format!("oraNpv{i}"));
        }
        DeclKind::Augmentation | DeclKind::PlainAugmentation => {
            // The event's source is a module value: a static initialiser in
            // the class as well as the augmentation's `static member val`
            // duplicates the type's `.cctor`.
            lines(&mut d, &[format!("let oraGev{i} = Event<int>()")]);
            if kind == DeclKind::Augmentation {
                lines(&mut d, &["[<AutoOpen>]".to_string()]);
            }
            lines(
                &mut d,
                &[
                    format!("type OraG{i}() ="),
                    format!("    static member OraGs{i} = {i}"),
                    format!("type OraG{i} with"),
                    format!("    static member OraGa{i} = {i}"),
                ],
            );
            d.names
                .extend([format!("OraG{i}"), format!("OraGs{i}"), format!("OraGa{i}")]);
            // Every member kind in the augmentation, so each is graded where
            // the class is folded, and where it is not.
            let member_unit = |m| {
                if kind == DeclKind::Augmentation {
                    Unit::AugmentationMember(m)
                } else {
                    unit
                }
            };
            let start = d.lines.len();
            for (n, m) in MemberKind::ALL.into_iter().enumerate() {
                member(m, i, n, &mut d, member_unit(m));
            }
            for (line, _) in &mut d.lines[start..] {
                *line = line.replace(&format!("oraEv{i}.Publish"), &format!("oraGev{i}.Publish"));
            }
        }
        DeclKind::ExtensionAugmentation => {
            lines(
                &mut d,
                &[
                    "type System.Int32 with".to_string(),
                    format!("    static member OraXs{i} (r{i}: int) = r{i}"),
                    format!("    member oraXself{i}.OraXi{i} = {i}"),
                ],
            );
            d.names.extend([
                format!("OraXs{i}"),
                format!("r{i}"),
                format!("oraXself{i}"),
                format!("OraXi{i}"),
            ]);
        }
    }
    d
}

/// One generated project: its files, the probe file last, each probe's span
/// and name, and the source span of each unit in the contributor's file.
struct Program {
    context: Context,
    files: Vec<(String, String)>,
    probes: Vec<(String, TextRange)>,
    /// `(file index, start, end, unit)` of every generated unit.
    units: Vec<(usize, usize, usize, Unit)>,
}

/// Every candidate's accessor and event forms, beside the name itself.
fn forms(name: &str) -> impl Iterator<Item = String> + '_ {
    ["", "get_", "set_", "add_", "remove_"]
        .into_iter()
        .map(move |p| format!("{p}{name}"))
}

/// Write `decls` into `out` at `indent`, recording each unit's span against
/// file `file`.
fn write_decls(
    out: &mut String,
    decls: &[Decl],
    indent: &str,
    file: usize,
    units: &mut Vec<(usize, usize, usize, Unit)>,
) {
    for d in decls {
        for (line, unit) in &d.lines {
            let start = out.len();
            out.push_str(indent);
            out.push_str(line);
            units.push((file, start, out.len(), *unit));
            out.push('\n');
        }
    }
}

fn generate(seed: u64, extra_names: &BTreeSet<String>) -> Program {
    let mut rng = Rng::new(seed);
    let context = Context::ALL[usize::try_from(seed).unwrap() % Context::ALL.len()];
    let count = 3 + rng.below(6);
    let mut decls = Vec::new();
    let mut operator_cases = false;
    for i in 0..count {
        let mut kind = DeclKind::ALL[rng.below(DeclKind::ALL.len())];
        if context == Context::Signature && !kind.has_signature() {
            kind = DeclKind::LetValue;
        }
        if context == Context::Rec && !kind.rec_safe() {
            kind = DeclKind::LetFunction;
        }
        if kind == DeclKind::OperatorCases {
            if operator_cases {
                kind = DeclKind::Union;
            }
            operator_cases = true;
        }
        decls.push(decl(kind, i, &mut rng));
    }
    let mut candidates: BTreeSet<String> = BTreeSet::new();
    for d in &decls {
        for name in &d.names {
            candidates.extend(forms(name));
        }
    }
    for name in extra_names {
        candidates.extend(forms(name));
    }
    let candidates: Vec<String> = candidates.into_iter().collect();

    let mut files: Vec<(String, String)> = Vec::new();
    let mut units = Vec::new();
    let mut probe = String::from("module OraProbe\n");
    if context == Context::Aliased {
        probe.push_str("type OraAO = AutoOpenAttribute\n");
    }
    // Each candidate's earlier binder is a function: a module value compiles
    // to a property whose getter `get_X` would clash with the binder of the
    // candidate `get_X`.
    // `op_Nil` and `op_ColonColon` cannot be bound: in a binding's head they
    // are FSharp.Core's list cases, read as patterns.
    for name in candidates.iter().filter(|n| !n.starts_with("op_")) {
        probe.push_str(&format!("let {name} () = 0\n"));
    }
    let probe_idx = match context {
        Context::Root | Context::Signature => 1 + usize::from(context == Context::Signature),
        _ => 0,
    };
    let mut probes = Vec::new();
    // Each candidate is used bare in expression position, and — capitalised,
    // where a bare pattern is a constructor reference rather than a binder —
    // in pattern position too.
    let mut write_probes = |probe: &mut String, indent: &str| {
        for (k, name) in candidates.iter().enumerate() {
            let mut forms = vec![(format!("{indent}let oraProbe{k} = "), String::new())];
            if name.chars().next().is_some_and(char::is_uppercase) {
                forms.push((
                    format!("{indent}let oraPat{k} (n: int) = match n with "),
                    " -> 1 | _ -> 0".to_string(),
                ));
            }
            // A static operator is reached through its symbol.
            if name == "op_PlusDot" {
                forms = vec![(format!("{indent}let oraProbe{k} = ("), ") 1 2".to_string())];
            }
            let spelled = if name == "op_PlusDot" {
                "+."
            } else {
                name.as_str()
            };
            for (prefix, suffix) in forms {
                let start = probe.len() + prefix.len();
                probe.push_str(&prefix);
                probe.push_str(spelled);
                probe.push_str(&suffix);
                probe.push('\n');
                probes.push((
                    name.clone(),
                    TextRange::new(
                        u32::try_from(start).unwrap().into(),
                        u32::try_from(start + spelled.len()).unwrap().into(),
                    ),
                ));
            }
        }
    };
    match context {
        Context::Module | Context::Aliased => {
            let attr = if context == Context::Aliased {
                "OraAO"
            } else {
                "AutoOpen"
            };
            probe.push_str(&format!("[<{attr}>]\nmodule OraAuto =\n"));
            write_decls(&mut probe, &decls, "    ", probe_idx, &mut units);
            write_probes(&mut probe, "");
        }
        Context::Nested => {
            probe.push_str(
                "[<AutoOpen>]\nmodule OraOuter =\n    [<AutoOpen>]\n    module OraInner =\n",
            );
            write_decls(&mut probe, &decls, "        ", probe_idx, &mut units);
            write_probes(&mut probe, "");
        }
        Context::Rec => {
            probe.push_str("module rec OraRec =\n");
            write_probes(&mut probe, "    ");
            write_decls(&mut probe, &decls, "    ", probe_idx, &mut units);
        }
        Context::Root => {
            let mut root = String::from("[<AutoOpen>]\nmodule OraRoot\n");
            write_decls(&mut root, &decls, "", 0, &mut units);
            files.push(("OraRoot.fs".to_string(), root));
            write_probes(&mut probe, "");
        }
        Context::Signature => {
            let mut sig = String::from("[<AutoOpen>]\nmodule OraRoot\n");
            for d in &decls {
                for l in &d.sig {
                    sig.push_str(l);
                    sig.push('\n');
                }
            }
            let mut root = String::from("module OraRoot\n");
            write_decls(&mut root, &decls, "", 1, &mut units);
            files.push(("OraRoot.fsi".to_string(), sig));
            files.push(("OraRoot.fs".to_string(), root));
            write_probes(&mut probe, "");
        }
    }
    files.push(("OraProbe.fs".to_string(), probe));
    assert_eq!(files.len() - 1, probe_idx);
    Program {
        context,
        files,
        probes,
        units,
    }
}

/// One FCS error: its file's name and 1-based start line.
struct FcsError {
    file: String,
    line: u64,
    message: String,
}

/// Ask FCS about one program's project: the written files, the uses, and the
/// errors with where they start.
fn ask_fcs(
    program: &Program,
) -> (
    Vec<(PathBuf, String)>,
    Vec<crate::common::FileUses>,
    Vec<FcsError>,
) {
    let rel: Vec<(&str, &str)> = program
        .files
        .iter()
        .map(|(p, s)| (p.as_str(), s.as_str()))
        .collect();
    let (root, written) = temp_fs_tree("contributor_oracle", &rel);
    let paths: Vec<&Path> = written.iter().map(|(p, _)| p.as_path()).collect();
    let json = invoke_fcs_dump_project_with_refs(&paths, &[]);
    let fcs = parse_fcs_uses_project(&json, &written);
    let _ = std::fs::remove_dir_all(&root);
    let raw: serde_json::Value = serde_json::from_str(&json).expect("uses-project JSON");
    let mut errors = Vec::new();
    for file in raw["Files"].as_array().into_iter().flatten() {
        let name = Path::new(file["Path"].as_str().unwrap_or_default())
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        for d in file["Diagnostics"].as_array().into_iter().flatten() {
            if d["Severity"].as_str() == Some("Error") {
                errors.push(FcsError {
                    file: name.clone(),
                    line: d["Range"]["Start"]["Line"].as_u64().unwrap_or(0),
                    message: d["Message"].as_str().unwrap_or_default().to_string(),
                });
            }
        }
    }
    (written, fcs, errors)
}

/// The names FCS reports as defined in the contributor's units: what the
/// generator's own candidate list may have missed.
fn defined_names(
    program: &Program,
    fcs: &[crate::common::FileUses],
    written: &[(PathBuf, String)],
) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for &(file, start, end, _) in &program.units {
        let Some(fu) = fcs
            .iter()
            .find(|f| f.path.file_name() == written[file].0.file_name())
        else {
            continue;
        };
        for u in &fu.uses {
            if u.is_from_definition
                && u.start >= start
                && u.end <= end
                && u.name.chars().all(|c| c.is_alphanumeric() || c == '_')
                && u.name.chars().next().is_some_and(char::is_alphabetic)
            {
                out.insert(u.name.clone());
            }
        }
    }
    out
}

#[test]
fn every_name_fcs_says_a_contributor_introduces_is_marked() {
    let fsharp_core = ensure_fsharp_core_dll();
    let env = env_of(&[fsharp_core.as_path()]);
    let seeds = env_usize_or("BORZOI_CONTRIB_ORACLE_SEEDS", DEFAULT_SEEDS);
    let dump = std::env::var("BORZOI_CONTRIB_ORACLE_DUMP")
        .ok()
        .and_then(|s| s.parse::<u64>().ok());

    let mut failures = Vec::new();
    // Per unit kind: (generated, probes FCS bound to it).
    let mut census: BTreeMap<Unit, (usize, usize)> = BTreeMap::new();
    let mut probes_graded = 0;
    // Programs FCS rejects: their bindings are error recovery, which grades
    // nothing, so the generator must not emit them.
    let mut rejected = Vec::new();
    for seed in 0..u64::try_from(seeds).unwrap() {
        // A first pass learns what FCS says the contributor defines; the
        // second probes those names too.
        let first = generate(seed, &BTreeSet::new());
        let (written, fcs, _) = ask_fcs(&first);
        let extra = defined_names(&first, &fcs, &written);
        let program = generate(seed, &extra);
        if dump == Some(seed) {
            for (p, s) in &program.files {
                eprintln!("--- {p}\n{s}");
            }
        }
        let (written, fcs, errors) = ask_fcs(&program);
        // A probe may name something that is no first-class value (a delegate
        // type, an indexer, a case expecting arguments): FCS still reports
        // what the name resolved to, and only that line errs. An error
        // anywhere else means the program is not what the generator meant.
        let probe_lines: BTreeSet<u64> = program
            .probes
            .iter()
            .map(|(_, span)| {
                let src = &program.files.last().unwrap().1;
                1 + src[..usize::from(span.start())].matches('\n').count() as u64
            })
            .collect();
        let errors: Vec<String> = errors
            .iter()
            .filter(|e| e.file != "OraProbe.fs" || !probe_lines.contains(&e.line))
            .map(|e| format!("{}:{}: {}", e.file, e.line, e.message))
            .collect();
        if !errors.is_empty() {
            rejected.push(format!("  seed {seed} ({:?}): {errors:?}", program.context));
            continue;
        }
        let probe_idx = written.len() - 1;
        let probe_file = &written[probe_idx].0;
        let fu = fcs
            .iter()
            .find(|f| f.path.file_name() == probe_file.file_name())
            .unwrap_or_else(|| panic!("seed {seed}: no FCS uses for the probe file"));

        for &(_, _, _, unit) in &program.units {
            census.entry(unit).or_default();
        }
        let mut seen_units: BTreeSet<Unit> = BTreeSet::new();
        for &(_, _, _, unit) in &program.units {
            if seen_units.insert(unit) {
                census.get_mut(&unit).unwrap().0 += 1;
            }
        }

        let rel: Vec<(&str, &str)> = program
            .files
            .iter()
            .map(|(p, s)| (p.as_str(), s.as_str()))
            .collect();
        let proj = resolve_project_files(&project(&rel), &env);
        let file_paths: Vec<PathBuf> = written.iter().map(|(p, _)| p.clone()).collect();
        let mut bound_units: BTreeSet<Unit> = BTreeSet::new();
        for (name, span) in &program.probes {
            probes_graded += 1;
            let theirs = fcs_at(fu, *span);
            // Which generated unit FCS bound the probe to, if any.
            if let Some(decl) = theirs.and_then(|u| u.decl.as_ref()) {
                for &(file, start, end, unit) in &program.units {
                    if decl.file.file_name() == written[file].0.file_name()
                        && decl.start >= start
                        && decl.end <= end
                    {
                        bound_units.insert(unit);
                    }
                }
            }
            let ours = proj
                .file(probe_idx)
                .resolution_at(*span)
                .filter(|r| !matches!(r, Resolution::Deferred(_) | Resolution::Unresolved));
            if let Some(res) = ours
                && !theirs.is_some_and(|u| agrees(&env, &proj, &file_paths, probe_idx, res, u))
            {
                failures.push(format!(
                    "  seed {seed} ({:?}): `{name}` committed {res:?}, FCS: {}",
                    program.context,
                    render_fcs(theirs)
                ));
            }
        }
        for unit in bound_units {
            census.get_mut(&unit).unwrap().1 += 1;
        }
    }

    for (unit, (generated, bound)) in &census {
        eprintln!(
            "  {unit:?}: generated in {generated} programs, FCS bound a probe to it in {bound}"
        );
    }
    assert!(
        rejected.is_empty(),
        "FCS rejected {} generated programs: the generator must emit only programs \
         it accepts:\n{}",
        rejected.len(),
        rejected.join("\n")
    );
    assert!(
        failures.is_empty(),
        "{} probes committed where FCS names something else (rerun with \
         BORZOI_CONTRIB_ORACLE_DUMP=<seed>):\n{}",
        failures.len(),
        failures.join("\n")
    );
    // The absolute property: every kind the generator says introduces a bare
    // name does, in FCS, somewhere in the run — and every kind is generated.
    let mut model_wrong = Vec::new();
    for kind in DeclKind::ALL {
        let (generated, bound) = census.get(&Unit::Decl(kind)).copied().unwrap_or_default();
        if generated == 0 {
            model_wrong.push(format!("  {kind:?} was never generated"));
        } else if kind.introduces() && bound == 0 {
            model_wrong.push(format!("  {kind:?} introduced no probed name in FCS"));
        }
    }
    for kind in MemberKind::ALL {
        for (unit, label) in [
            (Unit::Member(kind), "member"),
            (Unit::AugmentationMember(kind), "augmentation member"),
        ] {
            let (generated, bound) = census.get(&unit).copied().unwrap_or_default();
            if generated == 0 {
                model_wrong.push(format!("  {label} {kind:?} was never generated"));
            } else if kind.introduces() && bound == 0 {
                model_wrong.push(format!(
                    "  {label} {kind:?} introduced no probed name in FCS"
                ));
            }
        }
    }
    assert!(
        model_wrong.is_empty(),
        "the generator's model disagrees with FCS, or a shape went unbuilt:\n{}",
        model_wrong.join("\n")
    );
    eprintln!("contributor oracle: {seeds} programs, {probes_graded} probes graded");
}
