//! The types written in **member signatures** against FCS: an `abstract`
//! slot's signature (`abstract M : x: R -> 'T`), a `val` field's type, and a
//! generic slot's own type parameters (`abstract N<'U> : 'U -> 'T`).
//!
//! These positions carry no body, so nothing in them is a value use: every
//! name FCS reports there is a type, a type parameter, or the label of a named
//! argument (which FCS reports as a *definition* of the parameter, never as a
//! use of anything in scope). The resolver must see them through the same type
//! resolver an ordinary annotation uses, with the type's type parameters and
//! the slot's own in scope.
//!
//! A generated grid crosses where the signature sits (an interface slot, an
//! `abstract member`, a `static abstract` slot, an abstract class's slot, a
//! `val` field of a class or a struct) with the shape of the signature (named,
//! optional, `byref`, `[<ParamArray>]`, curried and tupled argument groups,
//! property-shaped, an indexed property, an attribute argument spelling the
//! leaf again), the slot's own type-parameter list (none, fresh, constrained,
//! re-declaring the type's, declaring one name twice) and the type written at
//! each leaf (an in-file type, a generic in-file type, the type's own type
//! parameter, the slot's own, an undeclared one, an assembly type, an in-file
//! type named like a label or like the type parameter). Three properties hold
//! over every row:
//!
//! * **certain implies exact**: every in-file binder we commit is the one FCS
//!   names at that range; an assembly type we commit is one FCS also places
//!   outside the file; and, in a row FCS accepts, we commit nothing where FCS
//!   reports nothing;
//! * **must commit**: each leaf marked `«…»` — an in-file type, or a type
//!   parameter declared exactly once in the scopes open there — is committed,
//!   so the first property cannot hold by declining;
//! * **non-vacuity**: FCS accepts every row it is expected to, and the must-
//!   commit set is not empty.
//!
//! The rows FCS rejects are kept, and graded by the first property alone: an
//! editor sends recovered input between keystrokes, and a wrong answer there is
//! still a wrong answer.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::common::{
    CensusDecl, census_resolve_uses, full_bcl_env, invoke_fcs_dump_census, parse_census_jsonl,
    temp_fs_file,
};
use borzoi_cst::parser::parse;
use borzoi_cst::syntax::{AstNode, ImplFile};
use borzoi_sema::{ProjectItems, Resolution, ResolvedFile, SyntaxRecovery, resolve_file};
use rowan::TextRange;

/// The declarations every row's leaves may name.
const PRELUDE: &str = "\
open System.Collections.Generic
type K = { K1: int }
module N =
    type K = { K2: int }
type String = { S: int }
open N
open System
type R = { A: int }
type G<'a> = { B: 'a }
type x = { X: int }
type T = { C: int }
type IFace = interface end
type H<'a> = { E: 'a }
type H = { F: int }
type TyAttribute(t: System.Type) =
    inherit System.Attribute()
type List = { L: int }
type Action<'a> = { Act: 'a }
";

/// Where the signature sits. `{mtp}` is the slot's own type-parameter list and
/// `{sig}` the signature (a field's type, for the `val` positions).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Position {
    InterfaceSlot,
    InterfaceAbstractMember,
    StaticAbstractSlot,
    AbstractClassSlot,
    ClassValField,
    StructValField,
    /// A slot inside a `module rec`, which declares its own `R` after it.
    RecModuleSlot,
}

impl Position {
    const ALL: [Position; 7] = [
        Position::InterfaceSlot,
        Position::InterfaceAbstractMember,
        Position::StaticAbstractSlot,
        Position::AbstractClassSlot,
        Position::ClassValField,
        Position::StructValField,
        Position::RecModuleSlot,
    ];

    fn is_field(self) -> bool {
        matches!(self, Position::ClassValField | Position::StructValField)
    }

    fn template(self) -> &'static str {
        match self {
            Position::InterfaceSlot => "type I<'T> =\n    abstract M{mtp} : {sig}\n",
            Position::InterfaceAbstractMember => {
                "type I<'T> =\n    abstract member M{mtp} : {sig}\n"
            }
            Position::StaticAbstractSlot => "type I<'T> =\n    static abstract M{mtp} : {sig}\n",
            Position::AbstractClassSlot => {
                "[<AbstractClass>]\ntype C<'T>() =\n    abstract M{mtp} : {sig}\n"
            }
            Position::ClassValField => {
                "type C<'T>() =\n    [<DefaultValue(false)>] val mutable F : {sig}\n"
            }
            Position::StructValField => "[<Struct>]\ntype S<'T> =\n    val F : {sig}\n",
            Position::RecModuleSlot => {
                "module rec Inner =\n    type I<'T> =\n        abstract M{mtp} : {sig}\n    type R = { B: int }\n"
            }
        }
    }
}

/// The slot's own type-parameter list.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum SlotTypars {
    None,
    /// `<'U>`: a fresh name.
    Fresh,
    /// `<'T>`: the type's own name again.
    Colliding,
    /// `<'U when …>`: a fresh name under constraints naming it and a type.
    Constrained,
    /// `<'U, 'U>`: a fresh name declared twice, which FCS accepts, binding
    /// the uses to the first.
    Duplicated,
}

impl SlotTypars {
    const ALL: [SlotTypars; 5] = [
        SlotTypars::None,
        SlotTypars::Fresh,
        SlotTypars::Colliding,
        SlotTypars::Constrained,
        SlotTypars::Duplicated,
    ];

    fn text(self) -> &'static str {
        match self {
            SlotTypars::None => "",
            SlotTypars::Fresh => "<«'U»>",
            SlotTypars::Colliding => "<'T>",
            SlotTypars::Constrained => "<«'U» when «'U» : equality and «'U» :> «IFace»>",
            SlotTypars::Duplicated => "<'U, 'U>",
        }
    }

    /// Whether the slot declares `'U`.
    fn declares_u(self) -> bool {
        matches!(
            self,
            SlotTypars::Fresh | SlotTypars::Constrained | SlotTypars::Duplicated
        )
    }

    /// Whether `'U` has exactly one binder in scope.
    fn declares_u_once(self) -> bool {
        matches!(self, SlotTypars::Fresh | SlotTypars::Constrained)
    }
}

/// The type written at a leaf of the signature.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Leaf {
    /// An in-file record.
    InFile,
    /// An in-file generic record applied to an in-file record.
    InFileGeneric,
    /// The enclosing type's `'T`.
    TypeTypar,
    /// `'U`: the slot's own when it declares one, else undeclared.
    SlotTypar,
    /// `'V`: never declared.
    Implicit,
    /// `string`, from FSharp.Core.
    Assembly,
    /// `System.String`, written qualified.
    QualifiedAssembly,
    /// The in-file type `x`, spelled like the argument labels.
    LabelNamed,
    /// The in-file type `T`, spelled like the type parameter.
    TyparNamed,
    /// `H<R>`, where an in-file `H` of another arity is declared later.
    ArityClashApplied,
    /// `H`, where an in-file `H<'a>` is declared earlier.
    ArityClashBare,
    /// `Missing<R>`: recovered input, an application of nothing in scope.
    Unresolved,
    /// `G<R, R>`: recovered input, an application of the wrong arity.
    WrongArity,
    /// `List<R>`: an in-file non-generic `List` beside the opened
    /// `System.Collections.Generic.List<'T>`, which FCS picks by arity.
    ImportedArity,
    /// `K`: an in-file `K`, and a later `open` of a module declaring another.
    OpenedLater,
    /// `String`: an in-file `String`, and a later `open System`.
    OpenedOver,
    /// `Action`: an in-file `Action<'a>`, bare, beside the opened
    /// non-generic `System.Action`.
    BareGenericShadow,
}

impl Leaf {
    const ALL: [Leaf; 17] = [
        Leaf::InFile,
        Leaf::InFileGeneric,
        Leaf::TypeTypar,
        Leaf::SlotTypar,
        Leaf::Implicit,
        Leaf::Assembly,
        Leaf::QualifiedAssembly,
        Leaf::LabelNamed,
        Leaf::TyparNamed,
        Leaf::ArityClashApplied,
        Leaf::ArityClashBare,
        Leaf::Unresolved,
        Leaf::WrongArity,
        Leaf::ImportedArity,
        Leaf::OpenedLater,
        Leaf::OpenedOver,
        Leaf::BareGenericShadow,
    ];

    /// Whether the leaf probes which of several same-named declarations a name
    /// reaches, rather than the signature's own structure.
    fn probes_scope(self) -> bool {
        matches!(
            self,
            Leaf::ArityClashApplied
                | Leaf::ArityClashBare
                | Leaf::Unresolved
                | Leaf::WrongArity
                | Leaf::ImportedArity
                | Leaf::OpenedLater
                | Leaf::OpenedOver
                | Leaf::BareGenericShadow
        )
    }

    /// The leaf's text with its must-commit markers, given the slot's typars.
    /// An undeclared type parameter is never marked: where FCS generalises
    /// one depends on what it managed to check, which the resolver does not
    /// guess.
    fn text(self, slot: SlotTypars) -> &'static str {
        match self {
            Leaf::InFile => "«R»",
            Leaf::InFileGeneric => "«G»<«R»>",
            // A slot re-declaring `'T` puts two binders of the name in scope.
            Leaf::TypeTypar if slot == SlotTypars::Colliding => "'T",
            Leaf::TypeTypar => "«'T»",
            Leaf::SlotTypar if slot.declares_u_once() => "«'U»",
            Leaf::SlotTypar => "'U",
            Leaf::Implicit => "'V",
            Leaf::Assembly => "string",
            Leaf::QualifiedAssembly => "System.String",
            Leaf::LabelNamed => "«x»",
            Leaf::TyparNamed => "«T»",
            // FCS picks the `H` of the written arity; the resolver declines a
            // name whose in-file types differ in arity.
            Leaf::ArityClashApplied => "H<«R»>",
            Leaf::ArityClashBare => "H",
            Leaf::Unresolved => "Missing<R>",
            Leaf::WrongArity => "G<R, R>",
            Leaf::ImportedArity => "List<«R»>",
            Leaf::OpenedLater => "K",
            Leaf::OpenedOver => "String",
            Leaf::BareGenericShadow => "Action",
        }
    }
}

/// The signature's shape, over one leaf `L`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Shape {
    Property,
    Arrow,
    Named,
    Optional,
    TupledNamed,
    Tupled,
    Curried,
    CurriedNamed,
    ParenTuple,
    PostfixByref,
    PrefixByref,
    ParamArray,
    PostfixList,
    UnitArrow,
    /// An indexed property: arrow-shaped, but a property all the same.
    IndexedProperty,
    /// A parameter attribute whose argument names the leaf type again, in an
    /// expression (`typeof<L>`) rather than a type position.
    AttributeTypeof,
}

impl Shape {
    const ALL: [Shape; 16] = [
        Shape::Property,
        Shape::Arrow,
        Shape::Named,
        Shape::Optional,
        Shape::TupledNamed,
        Shape::Tupled,
        Shape::Curried,
        Shape::CurriedNamed,
        Shape::ParenTuple,
        Shape::PostfixByref,
        Shape::PrefixByref,
        Shape::ParamArray,
        Shape::PostfixList,
        Shape::UnitArrow,
        Shape::IndexedProperty,
        Shape::AttributeTypeof,
    ];

    fn template(self) -> &'static str {
        match self {
            Shape::Property => "L",
            Shape::Arrow => "L -> unit",
            Shape::Named => "x: L -> L",
            Shape::Optional => "?x: L -> unit",
            Shape::TupledNamed => "x: L * y: L -> L",
            Shape::Tupled => "L * L -> L",
            Shape::Curried => "L -> L -> L",
            Shape::CurriedNamed => "x: L -> y: L -> L",
            Shape::ParenTuple => "(L * L) -> L",
            Shape::PostfixByref => "L byref -> unit",
            Shape::PrefixByref => "byref<L> -> unit",
            Shape::ParamArray => "[<System.ParamArray>] xs: L[] -> unit",
            Shape::PostfixList => "L list -> L option",
            Shape::UnitArrow => "unit -> L",
            Shape::IndexedProperty => "L -> L with get",
            // `@` is the leaf without must-commit markers: an attribute
            // argument is an expression, not a type position.
            Shape::AttributeTypeof => "[<Ty(typeof<@>)>] x: L -> L",
        }
    }

    /// Whether the shape is a property, which cannot be generic.
    fn is_property(self) -> bool {
        matches!(self, Shape::Property | Shape::IndexedProperty)
    }

    /// Whether the scope-probing leaves ([`Leaf::probes_scope`]) are crossed
    /// with this shape: their question is which declaration a name reaches,
    /// which the shape around it does not change, so a few shapes suffice.
    fn carries_scope_probes(self) -> bool {
        matches!(
            self,
            Shape::Property | Shape::Arrow | Shape::CurriedNamed | Shape::PrefixByref
        )
    }

    /// A shape a field's type can take.
    fn fits_field(self) -> bool {
        matches!(
            self,
            Shape::Property | Shape::Arrow | Shape::Tupled | Shape::Curried | Shape::PostfixList
        )
    }
}

/// One generated row.
#[derive(Clone, Copy, Debug)]
struct Row {
    position: Position,
    slot: SlotTypars,
    shape: Shape,
    leaf: Leaf,
}

impl Row {
    /// Whether FCS is expected to accept this row (each rule probed against
    /// FCS):
    ///
    /// * a type parameter no scope declares is generalised implicitly only by
    ///   a slot that declares none explicitly and is not property-shaped — a
    ///   field, a slot with `<…>`, and a property all reject it;
    /// * a property cannot be generic, so a slot with `<…>` must take
    ///   arguments;
    /// * a slot may re-declare its type's parameter (`abstract M<'T>` in
    ///   `I<'T>`).
    fn expect_clean(self) -> bool {
        if matches!(self.leaf, Leaf::Unresolved | Leaf::WrongArity) {
            return false;
        }
        let undeclared = match self.leaf {
            Leaf::Implicit => true,
            Leaf::SlotTypar => !self.slot.declares_u(),
            _ => false,
        };
        let property = self.shape.is_property();
        if undeclared && (self.position.is_field() || self.slot != SlotTypars::None || property) {
            return false;
        }
        !(property && self.slot != SlotTypars::None)
    }
}

fn rows() -> Vec<Row> {
    let mut out = Vec::new();
    for position in Position::ALL {
        let slots: &[SlotTypars] = if position.is_field() {
            &[SlotTypars::None]
        } else {
            &SlotTypars::ALL
        };
        for &slot in slots {
            for shape in Shape::ALL {
                if position.is_field() && !shape.fits_field() {
                    continue;
                }
                for leaf in Leaf::ALL {
                    if leaf.probes_scope() && !shape.carries_scope_probes() {
                        continue;
                    }
                    out.push(Row {
                        position,
                        slot,
                        shape,
                        leaf,
                    });
                }
            }
        }
    }
    out
}

/// A program with its `«…»` markers stripped, and the ranges they marked.
struct Program {
    src: String,
    must_commit: Vec<TextRange>,
}

fn render(module: usize, marked: &str) -> Program {
    let mut src = format!("module M{module}\n{PRELUDE}");
    let mut must_commit = Vec::new();
    let mut open: Option<usize> = None;
    for c in marked.chars() {
        match c {
            '«' => {
                assert!(open.is_none(), "nested markers in {marked:?}");
                open = Some(src.len());
            }
            '»' => {
                let start = open.take().expect("balanced markers");
                must_commit.push(range_of(start, src.len()));
            }
            c => src.push(c),
        }
    }
    assert!(open.is_none(), "unclosed marker in {marked:?}");
    Program { src, must_commit }
}

fn render_row(module: usize, row: Row) -> Program {
    let marked: String = if row.position == Position::RecModuleSlot {
        row.leaf
            .text(row.slot)
            .chars()
            .filter(|c| !matches!(c, '«' | '»'))
            .collect()
    } else {
        row.leaf.text(row.slot).to_string()
    };
    let unmarked: String = marked.chars().filter(|c| !matches!(c, '«' | '»')).collect();
    let sig = row
        .shape
        .template()
        .replace('L', &marked)
        .replace('@', &unmarked);
    let decl = row
        .position
        .template()
        .replace("{mtp}", row.slot.text())
        .replace("{sig}", &sig);
    render(module, &decl)
}

fn range_of(start: usize, end: usize) -> TextRange {
    TextRange::new(
        u32::try_from(start).unwrap().into(),
        u32::try_from(end).unwrap().into(),
    )
}

fn resolve(src: &str) -> ResolvedFile {
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{src}: {:?}", parsed.errors);
    let recovery = SyntaxRecovery::of(&parsed);
    let file = ImplFile::cast(parsed.root).expect("impl file");
    resolve_file(&file, &ProjectItems::default(), full_bcl_env(), &recovery)
}

/// What one program's check found.
#[derive(Default)]
struct Verdict {
    wrong: Vec<String>,
    undercommitted: Vec<String>,
    committed: usize,
}

/// Grade `program` against FCS's census of it. `clean` says FCS accepted it,
/// which gates the completeness property, and decides what FCS's silence at a
/// range means: evidence against a commitment there in an accepted program,
/// nothing in a rejected one — an erroring check sinks no uses for whatever it
/// abandoned, which implicates nobody.
fn grade(program: &Program, file: &crate::common::FileCensus, clean: bool) -> Verdict {
    let src = &program.src;
    let rf = resolve(src);
    let uses = census_resolve_uses(file, src);
    let mut v = Verdict::default();

    for (range, res) in rf.resolutions() {
        let range = *range;
        let at: Vec<_> = uses
            .iter()
            .filter(|u| range_of(u.start, u.end) == range)
            .collect();
        match res {
            Resolution::Local(_) | Resolution::Item(_) => {
                let ours = rf.resolved_def(*res).map(|d| d.range);
                // Our own defining occurrence: FCS may or may not report it.
                if at.is_empty() && (ours == Some(range) || !clean) {
                    continue;
                }
                let agrees = !at.is_empty()
                    && at.iter().all(|u| match u.decl {
                        CensusDecl::InFile(s, e) => ours == Some(range_of(s, e)),
                        CensusDecl::OtherFile => false,
                        CensusDecl::Absent => true,
                    });
                if agrees {
                    v.committed += 1;
                } else {
                    v.wrong.push(format!(
                        "{:?} at {range:?}: we bound {ours:?}, FCS {:?}\n{src}",
                        &src[range],
                        at.iter().map(|u| u.decl).collect::<Vec<_>>()
                    ));
                }
            }
            Resolution::Entity(_) | Resolution::Member { .. } => {
                // FCS spans a dotted type path from its first segment
                // (`System.String`); we key the type at its own segment. The
                // two share an end.
                let at: Vec<_> = uses
                    .iter()
                    .filter(|u| {
                        u.end == usize::from(range.end()) && u.start <= range.start().into()
                    })
                    .collect();
                if at.is_empty() && !clean {
                    continue;
                }
                let agrees = !at.is_empty()
                    && at
                        .iter()
                        .all(|u| matches!(u.decl, CensusDecl::OtherFile | CensusDecl::Absent));
                if agrees {
                    v.committed += 1;
                } else {
                    v.wrong.push(format!(
                        "{:?} at {range:?}: we gave {res:?}, FCS {:?}\n{src}",
                        &src[range],
                        at.iter().map(|u| u.decl).collect::<Vec<_>>()
                    ));
                }
            }
            Resolution::Unresolved => v.wrong.push(format!(
                "{:?} at {range:?}: we gave Unresolved\n{src}",
                &src[range]
            )),
            Resolution::Deferred(_) => {}
        }
    }

    if clean {
        for &range in &program.must_commit {
            let committed = rf
                .resolution_at(range)
                .is_some_and(|r| matches!(r, Resolution::Local(_) | Resolution::Item(_)));
            if !committed {
                v.undercommitted.push(format!(
                    "{:?} at {range:?} was not committed: {:?}\n{src}",
                    &src[range],
                    rf.resolution_at(range)
                ));
            }
        }
    }
    v
}

/// Run every program through FCS's census once.
fn census_of(programs: &[Program]) -> Vec<crate::common::FileCensus> {
    let paths: Vec<PathBuf> = programs
        .iter()
        .map(|p| temp_fs_file("member_sig", &p.src))
        .collect();
    let census = parse_census_jsonl(&invoke_fcs_dump_census(&paths));
    for p in &paths {
        let _ = std::fs::remove_file(p);
    }
    assert_eq!(census.len(), programs.len(), "one census line per program");
    census
}

#[test]
fn member_signature_grid_agrees_with_fcs() {
    let rows = rows();
    let programs: Vec<Program> = rows
        .iter()
        .enumerate()
        .map(|(i, row)| render_row(i, *row))
        .collect();
    let census = census_of(&programs);

    let mut wrong = Vec::new();
    let mut undercommitted = Vec::new();
    let mut unexpected_verdict = Vec::new();
    let mut must_commit_points = 0usize;
    let mut committed_by_leaf: BTreeMap<Leaf, usize> = BTreeMap::new();
    for ((row, program), file) in rows.iter().zip(&programs).zip(&census) {
        let clean = file.ok && !file.has_check_errors;
        if clean != row.expect_clean() {
            unexpected_verdict.push(format!(
                "{row:?}: FCS {} it\n{}",
                if clean { "accepts" } else { "rejects" },
                program.src
            ));
        }
        let v = grade(program, file, clean);
        if clean {
            must_commit_points += program.must_commit.len();
        }
        *committed_by_leaf.entry(row.leaf).or_default() += v.committed;
        wrong.extend(v.wrong.into_iter().map(|w| format!("{row:?}: {w}")));
        undercommitted.extend(
            v.undercommitted
                .into_iter()
                .map(|w| format!("{row:?}: {w}")),
        );
    }
    eprintln!(
        "member-signature grid: {} rows, {must_commit_points} must-commit points; \
         commits by leaf {committed_by_leaf:?}",
        rows.len()
    );
    assert!(
        wrong.is_empty(),
        "{} commitments FCS does not make:\n{}",
        wrong.len(),
        wrong.join("\n")
    );
    assert!(
        unexpected_verdict.is_empty(),
        "{} rows FCS judged other than the grid expects — fix the model before \
         trusting the must-commit set:\n{}",
        unexpected_verdict.len(),
        unexpected_verdict.join("\n")
    );
    assert!(
        undercommitted.is_empty(),
        "{} must-commit leaves were declined:\n{}",
        undercommitted.len(),
        undercommitted.join("\n")
    );
    assert!(
        must_commit_points >= 500,
        "the must-commit set shrank to {must_commit_points}: the grid no longer \
         exercises what it claims to"
    );
}

/// Hand-written cases drawn from the corpus, each a shape the grid does not
/// build: several slots sharing a type's parameter under a constraint, a
/// generic slot of a generic type, slot parameters spanning lines.
const CASES: &[&str] = &[
    // WoofWare.Expect `Dot.fs`: a constrained type parameter used across
    // curried, named slots.
    "\
open System
type IProcess<«'Process» when 'Process :> IDisposable> =
    abstract Create : exe : string -> args : string -> «'Process»
    abstract Start : «'Process» -> bool
    abstract ExitCode : «'Process» -> int
",
    // WoofWare.PawPrint `TypeInfo.fs`: a generic slot of a generic type.
    "\
type TypeInfo<'a, 'field> = { Name: string }
type TypeInfoEval<«'ret»> =
    abstract Eval<«'a», «'field»> : «TypeInfo»<«'a», «'field»> -> «'ret»
type TypeInfoCrate =
    abstract Apply<«'ret»> : «TypeInfoEval»<«'ret»> -> «'ret»
    abstract ToString : unit -> string
",
    // WoofWare.PawPrint `TypeConcretisation.fs`: named parameters over lines.
    "\
type DumpedAssembly = { Name: string }
type IAssemblyLoad =
    abstract LoadAssembly :
        loadedAssemblies : Map<string, «DumpedAssembly»> ->
        referencedIn : string ->
            Map<string, «DumpedAssembly»> * «DumpedAssembly»
",
    // WoofWare.Incremental `Types.fs` / WoofWare.WeakHashTable: constraints
    // on a slot's own type parameters, naming them and the type's; and an
    // implicitly generalised one, which is left unrecorded.
    "\
type ArrayFoldEval<'a, 'ret> =
    abstract Eval<«'b»> : «'b» list -> «'a» -> «'ret»
type ArrayFoldCrate<«'a»> =
    abstract Apply<«'TEval», «'ret» when «'TEval» :> «ArrayFoldEval»<«'a», «'ret»>> : «'TEval» -> «'ret»
type WeakHashTableEval<«'ret»> =
    abstract Eval<«'a», «'b» when «'a» : equality and «'b» : not struct> : «'a» -> «'b» -> «'ret»
type ExpertEdgeCrate =
    abstract Apply : «ArrayFoldEval»<int, 'ret> -> 'ret
",
    // A slot's own type parameter shadows nothing outside the slot: the next
    // slot's `'U` is implicit, and a later type's `'U` is its own.
    "\
type I<«'T»> =
    abstract A<«'U»> : «'U» -> «'T»
    abstract B : «'T» -> «'T»
type J<«'U»> =
    abstract C : «'U» list
",
];

/// Whole files, each one shape where an in-file type is not the one FCS binds
/// and the resolver must decline: an `[<AutoOpen>]` module's type, declared
/// in one `namespace` block, shadows the earlier direct declaration in every
/// later block of that namespace.
const WHOLE_FILES: &[&str] = &["\
namespace N
type R = { A: int }
[<AutoOpen>]
module X =
    type R = { B: int }
namespace N
type I =
    abstract M : R -> R
"];

#[test]
fn whole_file_member_signatures_agree_with_fcs() {
    let programs: Vec<Program> = WHOLE_FILES
        .iter()
        .map(|src| Program {
            src: (*src).to_string(),
            must_commit: Vec::new(),
        })
        .collect();
    let census = census_of(&programs);
    let mut failures = Vec::new();
    for (program, file) in programs.iter().zip(&census) {
        assert!(
            file.ok && !file.has_check_errors,
            "the case must type-check cleanly, or FCS's answer is recovery:\n{}",
            program.src
        );
        failures.extend(grade(program, file, true).wrong);
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn corpus_member_signatures_agree_with_fcs() {
    let programs: Vec<Program> = CASES
        .iter()
        .enumerate()
        .map(|(i, c)| render(i, c))
        .collect();
    let census = census_of(&programs);
    let mut failures = Vec::new();
    for (program, file) in programs.iter().zip(&census) {
        assert!(
            file.ok && !file.has_check_errors,
            "the case must type-check cleanly, or FCS's answer is recovery:\n{}",
            program.src
        );
        assert!(!program.must_commit.is_empty(), "{}", program.src);
        let v = grade(program, file, true);
        failures.extend(v.wrong);
        failures.extend(v.undercommitted);
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
