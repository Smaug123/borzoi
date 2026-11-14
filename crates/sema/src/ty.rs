//! The type representation for semantic analysis (Phase 3 inference).
//!
//! A [`Ty`] is a closed, structured description of an F# type — a data value,
//! not a string (per "data descriptions over behavioural abstractions") —
//! held as an id into a [`TyTable`] that interns each distinct type once
//! (`docs/sema-interning-plan.md`, D4 and Stage 8). Its node is a [`TyKind`]:
//! a named type applied to arguments, a project type, an array, a tuple, a
//! function, a quantified parameter, or an inference variable. Two types are
//! equal exactly when their ids are, and a type is `Copy`.
//!
//! [`TyKind::Var`], the unknown the unification substrate ([`crate::unify`])
//! solves for, is an *internal* shape: it appears in the terms the solver
//! manipulates, never in [`crate::infer_file`]'s output, which emits only
//! **ground** types — the D5 "say nothing when unsure" contract, enforced at
//! read-off by code rather than convention. So the terms one file's inference
//! builds live in a table of its own, and what it publishes is copied into
//! the env's table ([`AssemblyEnv::types`]), which holds ground types only.
//!
//! A named type's path is a [`TyPath`]: an id in the env's name table, so a
//! `Ty` means something only next to the env it was built against, as an
//! [`EntityHandle`](crate::EntityHandle) does, and reading or rendering one
//! takes that env.
//!
//! The canonical rendering ([`Ty::render`]) is the **differential currency**: it
//! emits the abbreviation-resolved BCL FQN convention (`System.Int32`,
//! `System.Byte[]`) that the FCS oracle's `TypeCanon` field also emits
//! (`tools/fcs-dump` `renderTypeCanonical`), so the two sides compare by string
//! equality — the same projection-to-a-shared-form discipline the parser
//! differential uses.

use crate::AssemblyEnv;
use crate::names::{NameTable, PathId};

#[cfg(test)]
pub(crate) mod reference;
mod table;

pub(crate) use table::TyTable;

/// The most nodes a type inference builds or walks may have, counting every
/// [`TyKind`] node of its tree as one, with its variables resolved.
///
/// Inferred types are not bounded by the tree they are inferred from: each
/// line of `let g0 x = (x, 0)`, `let g1 x = g0 (g0 x)`, …, doubles the type,
/// and the unification table shares structure through variables that
/// resolving a type expands (`let a = (x, x) in let b = (a, a) in …`). So
/// the unification table's walks count what they visit and stop here, and a file
/// whose inference reaches this bound publishes nothing
/// ([`InferredFile::type_budget_exceeded`](crate::InferredFile::type_budget_exceeded)).
///
/// A type is a tree even where its table holds a subterm once: the bound
/// counts the tree, since the walks that render, resolve and unify a type
/// visit each place a subterm occurs.
///
/// The bound is 75 times the largest type inference walks in the pinned
/// corpora: 54 nodes over every `.fs` file of the F# compiler corpus, each
/// inferred alone against the reference pack and FSharp.Core, and 42 over
/// the whole-project corpus (measured with this budget in place, which
/// none of them reached). `infer_corpus_diff` holds its sample to an eighth
/// of it.
pub const MAX_TYPE_NODES: usize = 1 << 12;

/// The deepest a type inference builds or walks may be, the root counting as
/// depth 1: the depth bound beside [`MAX_TYPE_NODES`].
///
/// Every walk of a [`Ty`] — resolution, the occurs check, unification,
/// instantiation, rendering, and copying one between tables — recurses once
/// per level, so this is what bounds the stack they use on an inferred type
/// (`tests/all/infer_type_budget.rs` walks one at this depth under the
/// tallest expression, on half of [`WALK_STACK_BYTES`](crate::WALK_STACK_BYTES)).
///
/// It is the tree's own height bound,
/// [`MAX_TREE_HEIGHT`](borzoi_cst::parser::MAX_TREE_HEIGHT): a type built a
/// level per level of the tree — an annotation, the chain of results a long
/// curried application unifies its callee with — never passes it, so the
/// budget bites only on types that outgrow the tree. The one construct that
/// builds depth from the tree's width, a function's curried parameters, is
/// checked where its type is built. The deepest type inference walks in the
/// pinned corpora is 20 levels deep (a long curried function in the F#
/// compiler corpus; 10 in the whole-project corpus).
pub const MAX_TYPE_DEPTH: usize = borzoi_cst::parser::MAX_TREE_HEIGHT;

/// Which of the type budget's bounds a file's inference reached:
/// [`MAX_TYPE_NODES`] or [`MAX_TYPE_DEPTH`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TypeBudgetExceeded {
    /// A type with more than [`MAX_TYPE_NODES`] nodes.
    Nodes,
    /// A type deeper than [`MAX_TYPE_DEPTH`].
    Depth,
}

/// The node count and depth of a type, as [`MAX_TYPE_NODES`] and
/// [`MAX_TYPE_DEPTH`] count them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TySize {
    pub nodes: usize,
    pub depth: usize,
}

impl TySize {
    /// The larger of two sizes in each dimension.
    pub fn max_each(self, other: TySize) -> TySize {
        TySize {
            nodes: self.nodes.max(other.nodes),
            depth: self.depth.max(other.depth),
        }
    }
}

/// An inference variable: a placeholder the unification substrate (the
/// crate-internal `unify` module) solves for. A newtype over the `ena`
/// [`UnifyKey`] index, not a bare `u32`, per "no primitive obsession".
///
/// [`UnifyKey`]: ena::unify::UnifyKey
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TyVid(pub(crate) u32);

impl TyVid {
    /// The underlying table index.
    pub fn index(self) -> u32 {
        self.0
    }
}

/// The path of a named type ([`TyKind::Named`], [`TyKind::Project`]): its
/// segments, held by their id in the env's name table, so a clone copies an
/// integer and equality compares one.
///
/// A path names a type only next to the env whose table issued it; read it
/// back with [`Self::segments`]. Like the table's ids it has no `Ord`, since
/// ids are handed out in interning order: ordered output sorts by text.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct TyPath {
    id: PathId,
}

impl std::fmt::Debug for TyPath {
    /// The id: with no table at hand, it is all a path can show.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("TyPath").field(&self.id).finish()
    }
}

impl TyPath {
    /// `path`, interned in `names`.
    pub(crate) fn new<S: AsRef<str>>(names: &NameTable, path: &[S]) -> TyPath {
        TyPath {
            id: names.intern_path(path),
        }
    }

    /// The path `dotted` spells, segments split at each `.`, interned in
    /// `names`.
    pub(crate) fn dotted(names: &NameTable, dotted: &str) -> TyPath {
        TyPath::new(names, &dotted.split('.').collect::<Vec<_>>())
    }

    /// `path`, interned in `env`'s table.
    pub fn of<S: AsRef<str>>(env: &AssemblyEnv, path: &[S]) -> TyPath {
        TyPath::new(env.names(), path)
    }

    /// The path's segments, outermost first, as `env` spells them; `env` is
    /// the one the path was built against.
    pub fn segments<'e>(&self, env: &'e AssemblyEnv) -> Vec<&'e str> {
        self.segments_in(env.names())
    }

    /// [`Self::segments`], by the table.
    pub(crate) fn segments_in<'n>(&self, names: &'n NameTable) -> Vec<&'n str> {
        names.segments(self.id)
    }

    /// The path's segments as owned strings, for a query that takes them so.
    pub(crate) fn spelled(&self, names: &NameTable) -> Vec<String> {
        self.segments_in(names)
            .into_iter()
            .map(str::to_owned)
            .collect()
    }

    /// Whether the path's segments are exactly `path`.
    pub(crate) fn is(&self, names: &NameTable, path: &[&str]) -> bool {
        names.lookup_path(path) == Some(self.id)
    }

    /// The same path, as an id of `to`'s; `self` is `from`'s.
    #[cfg(feature = "test-support")]
    pub(crate) fn rekey(&self, from: &NameTable, to: &NameTable) -> TyPath {
        TyPath {
            id: crate::names::rekey_path(from, to, self.id),
        }
    }
}

/// A best-effort F# type, as far as the current inference phase models it:
/// an id into the type table that interned it, whose node is a [`TyKind`].
///
/// A type the env publishes ([`crate::InferredFile::types`] and the rest)
/// is an id into the env's type table: read it with
/// [`Self::kind`] and render it with [`Self::render`], against that env.
/// Like the name table's ids it implements `Eq` and `Hash` and not `Ord`, and
/// in a debug build it carries its table's tag, so comparing ids of two
/// tables panics.
#[derive(Clone, Copy)]
pub struct Ty {
    index: u32,
    #[cfg(debug_assertions)]
    table: u32,
}

impl PartialEq for Ty {
    fn eq(&self, other: &Self) -> bool {
        #[cfg(debug_assertions)]
        assert_eq!(
            self.table, other.table,
            "two Tys from different type tables compared"
        );
        self.index == other.index
    }
}

impl Eq for Ty {}

impl std::hash::Hash for Ty {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.index.hash(state);
    }
}

impl std::fmt::Debug for Ty {
    /// The id: with no table at hand, the id is all a type can show.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("Ty").field(&self.index).finish()
    }
}

/// A [`Ty`]'s node: closed, so every variant corresponds to an actual type
/// shape and illegal states (an array of nothing) are unrepresentable. Its
/// children are ids of the same table.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TyKind {
    /// A named type by its canonical, abbreviation-resolved path (a
    /// [`TyPath`]) — the segments `System`, `Int32` for `int`, `System`,
    /// `String` for `string` — applied to `args`.
    ///
    /// `args` is empty for a nullary type, and a *generic application* carries
    /// its arguments in source order: `int option` is
    /// `{ path: ["Microsoft", "FSharp", "Core", "FSharpOption"], args: [int] }`.
    /// The two surface spellings — postfix `int option` and prefix
    /// `Option<int>` — are already normalised by the CST
    /// ([`borzoi_cst::syntax::AppType`]), so only one shape reaches here.
    ///
    /// A nullary type is `args: []` and never a separate variant, so a type has
    /// exactly one representation and structural equality is the type equality
    /// the unifier needs. Build one with [`Ty::named`].
    Named { path: TyPath, args: Box<[Ty]> },
    /// A type the **project** declares, by the path FCS names it with —
    /// namespace, each enclosing module's compiled name, the type's own
    /// (`["N", "Inner", "G"]`) — applied to `args`.
    ///
    /// A type is its path and its arity (`args.len()`): F# rejects two project
    /// types at one path and arity in a single assembly (FS0249, private ones
    /// included), whichever files declare them, so the path names one type
    /// project-wide, from its own file or any other, and a signature and its
    /// implementation agree on it. Never a referenced assembly's type, even
    /// one at the same path: that is a [`TyKind::Named`], and the two never
    /// unify.
    Project { path: TyPath, args: Box<[Ty]> },
    /// An array type. `elem` is the element type, `rank` the dimensionality
    /// (1 for `T[]`, 2 for `T[,]`, …).
    Array { elem: Ty, rank: u32 },
    /// A reference tuple type (`int * string`), the element types in source
    /// order. Always arity ≥ 2 (a 1-element "tuple" is just the element; F# has
    /// no 0-tuple — `unit` is its own type).
    Tuple(Box<[Ty]>),
    /// A function type `arg -> ret`. Currying is nested to the right, matching
    /// FCS: `a -> b -> c` is `Fun(a, Fun(b, c))`. Emitted on a monomorphic
    /// function binder (`let f c = if c then 1 else 2` ⇒ `bool -> int`); a
    /// polymorphic function is generalised over [`TyKind::Param`]s.
    Fun { arg: Ty, ret: Ty },
    /// A **bound** (quantified) type parameter of a `let`-generalised scheme,
    /// numbered by first appearance (`Param(0)` renders `'a`, `Param(1)` `'b`,
    /// …). Distinct from [`TyKind::Var`]: a `Var` is an *unsolved* unknown (not
    /// ground), whereas a `Param` is a *resolved* quantifier standing for "any
    /// type" — so it **is** ground and reaches read-off.
    /// Introduced at generalisation: a walk-complete, unpoisoned function
    /// binding whose resolved type still has open variables created in that
    /// binding has those variables replaced by `Param`s to form its scheme;
    /// each use instantiates the scheme with fresh [`TyKind::Var`]s (see
    /// [`crate::infer_file`]).
    /// A `Param` is never bound *into* the unification table — schemes are
    /// instantiated before their bodies touch the solver — so the unify arms that
    /// treat it as a rigid constant (equal only to the same index) are, in
    /// practice, unreachable; they are kept total and property-tested regardless.
    Param(u32),
    /// An inference variable, solved by the unification substrate. Internal to
    /// inference: it appears in the terms the solver manipulates but never in
    /// [`crate::infer_file`]'s output (which emits only ground types), and the
    /// env's table cannot hold one.
    Var(TyVid),
}

impl Ty {
    /// This type's node; `env` is the one the type was published against.
    pub fn kind(self, env: &AssemblyEnv) -> &TyKind {
        env.types().kind(self)
    }

    /// A nullary [`TyKind::Named`] from a dotted canonical path
    /// (`"System.Int32"`), interned in `env`'s tables.
    pub fn named(env: &AssemblyEnv, path: &str) -> Ty {
        env.types().named(env.names(), path)
    }

    /// The named type `path` applied to `args`, in `env`'s table.
    pub fn app(env: &AssemblyEnv, path: TyPath, args: Vec<Ty>) -> Ty {
        env.types().app(path, args)
    }

    /// The project type `path` applied to `args`, in `env`'s table.
    pub fn project(env: &AssemblyEnv, path: TyPath, args: Vec<Ty>) -> Ty {
        env.types().project(path, args)
    }

    /// The array of `elem` of rank `rank`, in `env`'s table.
    pub fn array(env: &AssemblyEnv, elem: Ty, rank: u32) -> Ty {
        env.types().array(elem, rank)
    }

    /// The tuple of `elems`, in `env`'s table.
    pub fn tuple(env: &AssemblyEnv, elems: Vec<Ty>) -> Ty {
        env.types().tuple(elems)
    }

    /// The function `arg -> ret`, in `env`'s table.
    pub fn fun(env: &AssemblyEnv, arg: Ty, ret: Ty) -> Ty {
        env.types().fun(arg, ret)
    }

    /// The quantified parameter `i`, in `env`'s table.
    pub fn param(env: &AssemblyEnv, i: u32) -> Ty {
        env.types().param(i)
    }

    /// This type's node count and depth, as the type budget counts them
    /// ([`MAX_TYPE_NODES`], [`MAX_TYPE_DEPTH`]).
    pub fn size(self, env: &AssemblyEnv) -> TySize {
        env.types().size(self)
    }

    /// Render to the canonical string the FCS oracle's `TypeCanon` emits:
    /// `.`-joined segments for a named type and `Elem[,…]` for an array (rank
    /// commas, as ECMA-335 / FCS render them). The currency the inference
    /// differential (`crates/sema/tests/all/infer_literals_diff.rs`) compares.
    /// `env` is the one the type was published against.
    pub fn render(self, env: &AssemblyEnv) -> String {
        env.types().render(env.names(), self)
    }

    /// Render to the **F# display form** an editor shows — `int`, `float`,
    /// `byte[]` — rather than the canonical BCL FQN [`Self::render`] emits. The
    /// hover/completion currency; the canonical form stays the oracle currency.
    /// `env` is the one the type was published against.
    ///
    /// The BCL-primitive ↔ F#-alias map is **not** restated here: it defers to
    /// [`borzoi_assembly::fsharp_alias`], the single source of truth this
    /// crate already uses to render referenced-assembly members — so a hovered
    /// literal and a hovered member agree (`uint`, not `uint32`). A named type
    /// with no alias falls back to its canonical dotted string, so this is total.
    pub fn render_fsharp(self, env: &AssemblyEnv) -> String {
        env.types().render_fsharp(env.names(), self)
    }
}

impl Ty {
    /// The same type as one of `to`'s, its paths in `to`'s name table; `self`
    /// is `from`'s.
    #[cfg(feature = "test-support")]
    pub(crate) fn rekey(self, from: &AssemblyEnv, to: &AssemblyEnv) -> Ty {
        to.types()
            .rekey(from.types(), from.names(), to.names(), self)
    }
}

impl TyTable {
    /// [`Ty::render`], by the tables.
    pub(crate) fn render(&self, names: &NameTable, ty: Ty) -> String {
        let render = |t: Ty| self.render(names, t);
        match self.kind(ty) {
            TyKind::Named { path, args } | TyKind::Project { path, args } => {
                render_named(&path.segments_in(names).join("."), args, render)
            }
            TyKind::Array { elem, rank } => render_array(self, *elem, *rank, render),
            // `a * b * …` with FQN elements, matching the oracle's canonical
            // tuple rendering (`fcs-dump`'s `renderTypeInScope`). A nested tuple
            // element is parenthesised so the flat ` * ` join stays unambiguous.
            TyKind::Tuple(elems) => render_tuple(self, elems, render),
            // `arg -> ret` with FQN operands, matching the oracle's canonical
            // function rendering (`fcs-dump`'s `renderTypeCanonical`). Right-
            // associative, so only a *function* domain is parenthesised.
            TyKind::Fun { arg, ret } => render_fun(self, *arg, *ret, render),
            // A quantified type parameter renders `'a`, `'b`, … by index — the
            // same convention the oracle's canonicaliser
            // (`tools/fcs-dump`'s `renderTypeCanonical`) renames FCS's arbitrary
            // typar names to, so a generalised scheme compares by string equality.
            TyKind::Param(i) => typar_name(*i),
            // An unsolved variable has no canonical type; `infer_file` never
            // emits one (it emits only ground types), so this is reachable only
            // from debugging. Render an anonymous typar to stay total rather
            // than panicking.
            TyKind::Var(_) => "'_".to_owned(),
        }
    }

    /// [`Ty::render_fsharp`], by the tables.
    pub(crate) fn render_fsharp(&self, names: &NameTable, ty: Ty) -> String {
        let render = |t: Ty| self.render_fsharp(names, t);
        match self.kind(ty) {
            TyKind::Named { path, args } => {
                let path = path.segments_in(names);
                let (namespace, name) = match path.split_last() {
                    Some((name, ns)) => (ns.join("."), *name),
                    None => return String::new(),
                };
                // No alias: the path is raw metadata, so each segment is spelled
                // as an F# identifier ([`borzoi_assembly::join_quoted`]) — an
                // fsc-generated or source-quoted name cannot be written bare, and
                // under F#'s postfix generics an unquoted `Odd Type` is not
                // malformed but a *different* type (`Type<Odd>`). An alias is our
                // own literal (`int`, `string list`) and needs no such treatment.
                // `unit` is F#'s own type, not a BCL one, so it is not in
                // `fsharp_alias`'s table.
                // A renamed FSharp.Core generic shows the name F# gives it,
                // since its compiled name cannot be written in F# source.
                let unit =
                    (namespace == "Microsoft.FSharp.Core" && name == "Unit").then_some("unit");
                let renamed = (!args.is_empty())
                    .then(|| fsharp_core_renamed_display(&namespace, name))
                    .flatten();
                let head = unit
                    .or(renamed)
                    .or_else(|| borzoi_assembly::fsharp_alias(&namespace, name))
                    .map(str::to_owned)
                    .unwrap_or_else(|| borzoi_assembly::join_quoted(path.iter().copied()));
                render_named(&head, args, render)
            }
            // Spelled as F# identifiers, qualified, as the canonical form is.
            TyKind::Project { path, args } => render_named(
                &borzoi_assembly::join_quoted(path.segments_in(names)),
                args,
                render,
            ),
            TyKind::Array { elem, rank } => render_array(self, *elem, *rank, render),
            // The F# display form is the same `a * b` shape, with aliased
            // elements (`int * string`).
            TyKind::Tuple(elems) => render_tuple(self, elems, render),
            // The F# display form is the same `a -> b` shape, with aliased
            // operands (`bool -> int`).
            TyKind::Fun { arg, ret } => render_fun(self, *arg, *ret, render),
            // A quantified parameter renders identically in the display form —
            // `'a`, `'b`, … — so hover shows `f : 'a -> 'a` for a generalised
            // binder.
            TyKind::Param(i) => typar_name(*i),
            // Not emitted from `infer_file` (only ground types are); see
            // [`Ty::render`].
            TyKind::Var(_) => "'_".to_owned(),
        }
    }
}

/// FSharp.Core's generic types that F# **renames** with `[<CompiledName>]`
/// and that a [`Ty`] may carry: `(namespace, compiled name, source name,
/// display name)`.
///
/// [`TyKind::Named`] names a type by its compiled path, which is FCS's
/// canonical currency (`Microsoft.FSharp.Collections.FSharpList<System.Int32>`),
/// but no F# source can name `FSharpList`: [`Ty::render_fsharp`] shows the name
/// F# gives it instead, its FSharp.Core abbreviation where it has one (`list`,
/// `option`) and its source name otherwise (`Result`). A `Ty` holding one of
/// these paths is built only from FSharp.Core's own type, whose source name
/// the bridge checks against this table (`function_ty`), so the table cannot
/// name a type F# spells differently.
pub(crate) const FSHARP_CORE_RENAMED: &[(&str, &str, &str, &str)] = &[
    ("Microsoft.FSharp.Collections", "FSharpList", "List", "list"),
    ("Microsoft.FSharp.Core", "FSharpOption", "Option", "option"),
    (
        "Microsoft.FSharp.Core",
        "FSharpValueOption",
        "ValueOption",
        "voption",
    ),
    ("Microsoft.FSharp.Core", "FSharpRef", "Ref", "ref"),
    ("Microsoft.FSharp.Core", "FSharpResult", "Result", "Result"),
    ("Microsoft.FSharp.Core", "FSharpChoice", "Choice", "Choice"),
    ("Microsoft.FSharp.Control", "FSharpAsync", "Async", "Async"),
    ("Microsoft.FSharp.Collections", "FSharpMap", "Map", "Map"),
    ("Microsoft.FSharp.Collections", "FSharpSet", "Set", "Set"),
];

/// The F# display name of a renamed FSharp.Core generic type
/// ([`FSHARP_CORE_RENAMED`]) at `namespace.name`, if it is one.
fn fsharp_core_renamed_display(namespace: &str, name: &str) -> Option<&'static str> {
    FSHARP_CORE_RENAMED
        .iter()
        .find(|(ns, compiled, _, _)| *ns == namespace && *compiled == name)
        .map(|&(_, _, _, display)| display)
}

/// The canonical name of the type parameter at position `i`: `'a`, `'b`, …,
/// `'z` for the first 26, then a fixed overflow scheme `'t26`, `'t27`, … past
/// `'z`. This is the **shared convention** both sides of the generalisation
/// oracle emit — [`Ty::render`]/[`Ty::render_fsharp`] here, and the
/// `renderTypeCanonical` renamer in `tools/fcs-dump` (which we control, so the
/// scheme is chosen once and mirrored) — so a generalised scheme compares by
/// string equality regardless of FCS's arbitrary internal typar names. The `'t`
/// prefix on the overflow tail keeps it unambiguous against the letter run (no
/// single-letter name is `t26`).
pub(crate) fn typar_name(i: u32) -> String {
    if i < 26 {
        format!("'{}", (b'a' + i as u8) as char)
    } else {
        format!("'t{i}")
    }
}

/// Render a named type applied to `args` — the head alone when `args` is empty,
/// else `Head<a, b>`. The `", "` separator and angle brackets are the oracle's
/// (`fcs-dump`'s `renderTypeInScope`), so [`Ty::render`] compares by string
/// equality; [`Ty::render_fsharp`] reuses the shape with aliased operands.
///
/// `Head<a>` is legal F# for every generic type, but it is not always the
/// spelling FCS's *display* context picks: that prefers the postfix idiom for a
/// single-argument F# type constructor (`int option`, not `option<int>`), which
/// is what `borzoi_assembly::display` already does for assembly members.
/// Aligning the two display forms is separate work — the canonical form, which
/// is the differential currency, is unaffected either way.
fn render_named(head: &str, args: &[Ty], render_arg: impl Fn(Ty) -> String) -> String {
    if args.is_empty() {
        return head.to_owned();
    }
    let args: Vec<String> = args.iter().copied().map(render_arg).collect();
    format!("{head}<{}>", args.join(", "))
}

/// `ty` rendered by `render`, parenthesised when it is a **tuple** or a
/// **function**: what an array element and a tuple element need.
fn bracketed(tys: &TyTable, ty: Ty, render: impl Fn(Ty) -> String) -> String {
    match tys.kind(ty) {
        TyKind::Tuple(_) | TyKind::Fun { .. } => format!("({})", render(ty)),
        _ => render(ty),
    }
}

/// Render an array as `Elem[,…]` (rank commas, as ECMA-335 / FCS render them),
/// parenthesising an element that is itself a **tuple** or a **function**.
///
/// The parens are load-bearing, not cosmetic: `[]` binds tighter than both `*`
/// and `->`, so a bare `System.Int32 * System.String[]` denotes a *2-tuple of
/// `int` and `string[]`* — a different type from the array of pairs it was meant
/// to render. Shared by [`Ty::render`] and [`Ty::render_fsharp`].
fn render_array(tys: &TyTable, elem: Ty, rank: u32, render_elem: impl Fn(Ty) -> String) -> String {
    let commas = ",".repeat((rank as usize).saturating_sub(1));
    format!("{}[{commas}]", bracketed(tys, elem, render_elem))
}

/// Render a tuple's elements with `render_elem`, joined by ` * `, parenthesising
/// any element that is itself a **tuple** or a **function** so the flat join
/// stays unambiguous: a nested `(a * b) * c` does not collapse into the 3-tuple
/// `a * b * c`, and a function element `(a -> b) * c` does not read as
/// `a -> (b * c)` (since `*` binds tighter than `->`, an unparenthesised function
/// element would be mis-grouped). Shared by [`Ty::render`] and
/// [`Ty::render_fsharp`], and matched by the oracle's `renderTypeCanonical`.
fn render_tuple(tys: &TyTable, elems: &[Ty], render_elem: impl Fn(Ty) -> String) -> String {
    elems
        .iter()
        .map(|&e| bracketed(tys, e, &render_elem))
        .collect::<Vec<_>>()
        .join(" * ")
}

/// Render a function type `arg -> ret` with `render_elem`, matching FCS's
/// canonical/display form. `->` is right-associative and binds looser than `*`,
/// so the *range* is never parenthesised (a curried `a -> b -> c` reads flat)
/// and the *domain* is parenthesised only when it is itself a function
/// (`(a -> b) -> c`); a tuple domain (`a * b -> c`) needs none. Shared by
/// [`Ty::render`] and [`Ty::render_fsharp`].
fn render_fun(tys: &TyTable, arg: Ty, ret: Ty, render_elem: impl Fn(Ty) -> String) -> String {
    let arg_s = match tys.kind(arg) {
        TyKind::Fun { .. } => format!("({})", render_elem(arg)),
        _ => render_elem(arg),
    };
    format!("{arg_s} -> {}", render_elem(ret))
}

#[cfg(test)]
mod tests {
    use std::sync::LazyLock;

    use super::*;

    /// The name table every type in these tests is built against.
    fn names() -> &'static NameTable {
        static NAMES: LazyLock<NameTable> = LazyLock::new(NameTable::default);
        &NAMES
    }

    /// The type table every type in these tests is interned in.
    fn tys() -> &'static TyTable {
        static TYS: LazyLock<TyTable> = LazyLock::new(TyTable::session);
        &TYS
    }

    fn named(path: &str) -> Ty {
        tys().named(names(), path)
    }

    fn app(path: &str, args: Vec<Ty>) -> Ty {
        tys().app(TyPath::dotted(names(), path), args)
    }

    fn arr(elem: Ty, rank: u32) -> Ty {
        tys().array(elem, rank)
    }

    fn tup(elems: Vec<Ty>) -> Ty {
        tys().tuple(elems)
    }

    fn fun(arg: Ty, ret: Ty) -> Ty {
        tys().fun(arg, ret)
    }

    fn render(ty: Ty) -> String {
        tys().render(names(), ty)
    }

    fn render_fsharp(ty: Ty) -> String {
        tys().render_fsharp(names(), ty)
    }

    /// The F# rendering spells each segment as an identifier. A named type's
    /// path is raw metadata, and metadata carries names F# can only write in
    /// double backticks — an fsc-generated type (`@`) or a source-quoted one —
    /// so a raw join emits a type that is not F#. Worse than malformed: F#'s
    /// postfix generic syntax makes `Odd Type` legal and *different*
    /// (`Type<Odd>`), so an unquoted awkward name reads as another type rather
    /// than as broken text.
    ///
    /// The `render` (CLR) form deliberately does not quote: it is the currency of
    /// the FCS differentials, which compare against raw metadata names.
    #[test]
    fn renders_fsharp_names_as_identifiers() {
        let ty = tys().named_path(names(), &["Ns", "Odd Type", "Inner@42"]);
        assert_eq!(render_fsharp(ty), "Ns.``Odd Type``.``Inner@42``");
        assert_eq!(render(ty), "Ns.Odd Type.Inner@42");
        // A generic argument is rendered through the same path, so it is quoted
        // wherever it appears.
        let outer = app("Ns.Holder", vec![ty]);
        assert_eq!(
            render_fsharp(outer),
            "Ns.Holder<Ns.``Odd Type``.``Inner@42``>"
        );
    }

    #[test]
    fn renders_named_and_array() {
        assert_eq!(render(named("System.Int32")), "System.Int32");
        assert_eq!(render(named("System.String")), "System.String");
        assert_eq!(render(arr(named("System.Byte"), 1)), "System.Byte[]");
        assert_eq!(render(arr(named("System.Int32"), 2)), "System.Int32[,]");
    }

    /// An array whose element is a **tuple** or a **function** parenthesises it.
    /// Without the parens `(int * string)[]` renders `int * string[]`, which
    /// denotes a different type — a 2-tuple of `int` and `string[]` — because
    /// `[]` binds tighter than `*` and `->`. That is a wrong hover, not a
    /// cosmetic one.
    #[test]
    fn renders_a_structural_array_element_parenthesised() {
        let pair = tup(vec![named("System.Int32"), named("System.String")]);
        assert_eq!(render(arr(pair, 1)), "(System.Int32 * System.String)[]");
        assert_eq!(render_fsharp(arr(pair, 1)), "(int * string)[]");

        let f = fun(named("System.Int32"), named("System.String"));
        assert_eq!(render(arr(f, 1)), "(System.Int32 -> System.String)[]");
        assert_eq!(render_fsharp(arr(f, 1)), "(int -> string)[]");

        // A named, array or parameter element needs none: `[]` already binds
        // tighter than anything those spell.
        assert_eq!(
            render_fsharp(arr(arr(named("System.Int32"), 1), 1)),
            "int[][]"
        );
        assert_eq!(render_fsharp(arr(tys().param(0), 1)), "'a[]");
    }

    /// The mirror of the array rule: a **generic argument** that is a tuple or a
    /// function is *not* parenthesised, because a comma-separated argument list
    /// has nothing to regroup — both `*` and `->` bind tighter than `,`. The F#
    /// compiler reads `Func<int -> string, bool>` and
    /// `Dictionary<int * string, bool>` as two-argument applications for exactly
    /// that reason.
    ///
    /// The rendering stays injective without the parens, which is the property
    /// that actually matters for a comparison currency: a single tuple argument
    /// and a two-argument application are distinct strings.
    #[test]
    fn renders_a_structural_generic_argument_bare() {
        let pair = tup(vec![named("System.Int32"), named("System.String")]);
        let f = fun(named("System.Int32"), named("System.String"));

        assert_eq!(
            render(app("N.H", vec![pair])),
            "N.H<System.Int32 * System.String>"
        );
        assert_eq!(render_fsharp(app("N.H", vec![pair])), "N.H<int * string>");
        assert_eq!(
            render(app("N.H", vec![f])),
            "N.H<System.Int32 -> System.String>"
        );
        assert_eq!(render_fsharp(app("N.H", vec![f])), "N.H<int -> string>");

        // Injectivity: one tuple argument never collides with two arguments.
        assert_ne!(
            render(app("N.H", vec![pair])),
            render(app(
                "N.H",
                vec![named("System.Int32"), named("System.String")]
            ))
        );
    }

    #[test]
    fn renders_tuples() {
        let pair = tup(vec![named("System.Int32"), named("System.String")]);
        assert_eq!(render(pair), "System.Int32 * System.String");
        assert_eq!(render_fsharp(pair), "int * string");

        // A nested tuple element is parenthesised so the join stays unambiguous.
        let nested = tup(vec![pair, named("System.Boolean")]);
        assert_eq!(
            render(nested),
            "(System.Int32 * System.String) * System.Boolean"
        );
        assert_eq!(render_fsharp(nested), "(int * string) * bool");
    }

    #[test]
    fn renders_functions() {
        // A monomorphic function: canonical FQNs and F# aliases.
        let simple = fun(named("System.Boolean"), named("System.Int32"));
        assert_eq!(render(simple), "System.Boolean -> System.Int32");
        assert_eq!(render_fsharp(simple), "bool -> int");

        // Currying is right-associative — a nested range reads flat, no parens.
        let curried = fun(
            named("System.Boolean"),
            fun(named("System.Int32"), named("System.String")),
        );
        assert_eq!(
            render(curried),
            "System.Boolean -> System.Int32 -> System.String"
        );
        assert_eq!(render_fsharp(curried), "bool -> int -> string");

        // A *function* domain is parenthesised; a tuple domain is not (`*`
        // binds tighter than `->`).
        let higher_order = fun(
            fun(named("System.Int32"), named("System.Int32")),
            named("System.Boolean"),
        );
        assert_eq!(
            render(higher_order),
            "(System.Int32 -> System.Int32) -> System.Boolean"
        );
        assert_eq!(render_fsharp(higher_order), "(int -> int) -> bool");

        let tuple_domain = fun(
            tup(vec![named("System.Int32"), named("System.Int32")]),
            named("System.Int32"),
        );
        assert_eq!(
            render(tuple_domain),
            "System.Int32 * System.Int32 -> System.Int32"
        );
        assert_eq!(render_fsharp(tuple_domain), "int * int -> int");
    }

    #[test]
    fn tuple_with_a_function_element_parenthesises_it() {
        // `(bool -> int) * int`: the function element must be parenthesised so it
        // does not read as `bool -> (int * int)` (`*` binds tighter than `->`).
        let ty = tup(vec![
            fun(named("System.Boolean"), named("System.Int32")),
            named("System.Int32"),
        ]);
        assert_eq!(
            render(ty),
            "(System.Boolean -> System.Int32) * System.Int32"
        );
        assert_eq!(render_fsharp(ty), "(bool -> int) * int");
    }

    #[test]
    fn function_is_ground_only_when_both_operands_are() {
        let ground = fun(named("System.Boolean"), named("System.Int32"));
        assert!(tys().is_ground(ground));

        let open = fun(tys().var(TyVid(0)), named("System.Int32"));
        assert!(!tys().is_ground(open));
    }

    #[test]
    fn renders_fsharp_primitives() {
        // Every BCL FQN literal typing produces maps to the same F# alias the
        // assembly crate uses (note `uint`, not `uint32`).
        let cases = [
            ("System.Int32", "int"),
            ("System.SByte", "sbyte"),
            ("System.Byte", "byte"),
            ("System.Int16", "int16"),
            ("System.UInt16", "uint16"),
            ("System.UInt32", "uint"),
            ("System.Int64", "int64"),
            ("System.UInt64", "uint64"),
            ("System.IntPtr", "nativeint"),
            ("System.UIntPtr", "unativeint"),
            ("System.Double", "float"),
            ("System.Single", "float32"),
            ("System.Decimal", "decimal"),
            ("System.String", "string"),
            ("System.Char", "char"),
            ("System.Boolean", "bool"),
        ];
        for (canonical, fsharp) in cases {
            assert_eq!(render_fsharp(named(canonical)), fsharp, "{canonical}");
        }
    }

    #[test]
    fn renders_fsharp_byte_array() {
        assert_eq!(render_fsharp(arr(named("System.Byte"), 1)), "byte[]");
    }

    #[test]
    fn renders_type_parameters() {
        let p = |i| tys().param(i);
        // The first 26 are `'a`..`'z`; index 26+ overflow to `'t26`, `'t27`, …
        assert_eq!(render(p(0)), "'a");
        assert_eq!(render(p(1)), "'b");
        assert_eq!(render(p(25)), "'z");
        assert_eq!(render(p(26)), "'t26");
        assert_eq!(render(p(27)), "'t27");
        // The display form is identical.
        assert_eq!(render_fsharp(p(0)), "'a");
        assert_eq!(render_fsharp(p(26)), "'t26");

        // A quantified parameter is ground (it stands for "any type"), so a
        // scheme body reaches read-off.
        assert!(tys().is_ground(p(0)));
        let scheme = fun(p(0), p(0));
        assert!(tys().is_ground(scheme));
        assert_eq!(render(scheme), "'a -> 'a");
        assert_eq!(render_fsharp(scheme), "'a -> 'a");
    }

    #[test]
    fn renders_scheme_with_params_in_tuples_and_functions() {
        let p = |i| tys().param(i);
        // `'a -> ('b -> 'b) * 'a` — the function tuple element parenthesised, the
        // curried range flat — matching FCS's canonical form for `let h x = (id, x)`.
        let scheme = fun(p(0), tup(vec![fun(p(1), p(1)), p(0)]));
        assert_eq!(render(scheme), "'a -> ('b -> 'b) * 'a");
        assert_eq!(render_fsharp(scheme), "'a -> ('b -> 'b) * 'a");
    }

    #[test]
    fn render_fsharp_spells_unit_as_fsharp_does() {
        let unit = named("Microsoft.FSharp.Core.Unit");
        assert_eq!(render(unit), "Microsoft.FSharp.Core.Unit");
        assert_eq!(render_fsharp(unit), "unit");
        let f = fun(unit, named("System.String"));
        assert_eq!(render_fsharp(f), "unit -> string");
    }

    /// A renamed FSharp.Core generic renders under its compiled name in the
    /// canonical currency, which is FCS's, and under the name F# gives it in
    /// the display form, since no F# source can name `FSharpList`. Only an
    /// application is renamed: the compiled path alone, nullary, is not a
    /// type any `Ty` carries, and keeps its canonical spelling.
    #[test]
    fn render_fsharp_names_a_renamed_fsharp_core_generic_as_fsharp_does() {
        let int = named("System.Int32");
        let list = app("Microsoft.FSharp.Collections.FSharpList", vec![int]);
        assert_eq!(
            render(list),
            "Microsoft.FSharp.Collections.FSharpList<System.Int32>"
        );
        assert_eq!(render_fsharp(list), "list<int>");
        let nested = app("Microsoft.FSharp.Core.FSharpOption", vec![list]);
        assert_eq!(render_fsharp(nested), "option<list<int>>");
        let result = app(
            "Microsoft.FSharp.Core.FSharpResult",
            vec![int, named("System.String")],
        );
        assert_eq!(render_fsharp(result), "Result<int, string>");
        for &(namespace, compiled, _, display) in FSHARP_CORE_RENAMED {
            let ty = app(&format!("{namespace}.{compiled}"), vec![int]);
            assert_eq!(render_fsharp(ty), format!("{display}<int>"));
        }
        assert_eq!(
            render_fsharp(named("Microsoft.FSharp.Collections.FSharpList")),
            "Microsoft.FSharp.Collections.FSharpList"
        );
    }

    #[test]
    fn render_fsharp_falls_back_to_canonical_for_unmapped() {
        // A named type with no F# alias renders by its dotted FQN.
        assert_eq!(
            render_fsharp(named("System.Collections.Generic.List")),
            "System.Collections.Generic.List"
        );
    }

    /// The env's table holds ground types only: an inference variable has no
    /// meaning outside the session that made it.
    #[test]
    #[should_panic(expected = "holds ground types only")]
    fn a_ground_table_refuses_an_inference_variable() {
        TyTable::ground().var(TyVid(0));
    }

    /// Ids of two tables never compare: the same node in two tables has
    /// unrelated ids.
    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "different type tables")]
    fn comparing_ids_of_two_tables_panics() {
        let other = TyTable::session();
        let _ = named("System.Int32") == other.named(names(), "System.Int32");
    }

    /// Reading an id against another table panics rather than reading
    /// whatever node that table holds at the index.
    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "another type table")]
    fn reading_an_id_against_another_table_panics() {
        let other = TyTable::session();
        other.kind(named("System.Int32"));
    }

    /// A type a sizeable tree shares is one node however often it occurs, and
    /// its size counts the tree: the type budget bounds what a walk visits.
    #[test]
    fn a_shared_subterm_is_one_node_but_counts_at_each_place() {
        let table = TyTable::session();
        let mut ty = table.named(names(), "System.Int32");
        for _ in 0..40 {
            ty = table.tuple(vec![ty, ty]);
        }
        assert_eq!(table.len(), 41, "each level adds one node");
        let size = table.size(ty);
        assert_eq!(size.depth, 41);
        assert_eq!(size.nodes, (1usize << 41) - 1, "the tree's count");
    }

    /// Interning from many threads at once hands out one id per type.
    #[test]
    fn interning_from_many_threads_gives_one_id_per_type() {
        let names = NameTable::default();
        let table = TyTable::ground();
        let build = |seed: usize| {
            (0..64)
                .map(|i| {
                    let leaf = table.named(&names, &format!("N.T{}", (i + seed) % 16));
                    table.tuple(vec![leaf, table.param(((i + seed) % 3) as u32)])
                })
                .collect::<Vec<_>>()
        };
        let results: Vec<Vec<Ty>> = std::thread::scope(|s| {
            let handles: Vec<_> = (0..8).map(|t| s.spawn(move || build(t))).collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        for (t, got) in results.iter().enumerate() {
            assert_eq!(got, &build(t), "thread {t}");
        }
        assert_eq!(table.len(), 16 + 3 + 16 * 3, "leaves, params and pairs");
    }
}

/// The interned form held to the string form it replaced ([`reference`]),
/// over generated types: a tree whose paths are spelled.
#[cfg(test)]
mod spelled_form {
    use proptest::prelude::*;

    use super::reference::{self, SpelledTy};
    use super::*;
    use crate::hash::HashMap;
    use crate::names::NameTable;

    /// Paths over few segments, so two paths often share a prefix or
    /// differ only in their last segment, plus the paths the renderers
    /// treat specially: an alias, `unit`, a renamed FSharp.Core generic, a
    /// name F# writes quoted, a segment holding a dot (which renders like
    /// two segments but is one), and the empty path.
    fn path() -> impl Strategy<Value = Vec<String>> {
        let special = prop::sample::select(vec![
            vec![],
            vec!["System", "Int32"],
            vec!["System", "Object"],
            vec!["System", "Void"],
            vec!["Microsoft", "FSharp", "Core", "Unit"],
            vec!["Microsoft", "FSharp", "Collections", "FSharpList"],
            vec!["Microsoft", "FSharp", "Core", "FSharpResult"],
            vec!["Ns", "Odd Type", "Inner@42"],
            vec!["Ns", "Odd", "Type"],
            vec!["Weird.Name"],
            vec!["Weird", "Name"],
        ])
        .prop_map(|p| p.into_iter().map(str::to_owned).collect());
        let small = prop::collection::vec(
            prop::sample::select(vec!["A", "B", "System", "Int32", "Unit"]).prop_map(str::to_owned),
            0..4,
        );
        prop_oneof![special, small]
    }

    /// Generated types, over a small pool of leaves so two often share a
    /// subterm, or differ in one place.
    fn spelled() -> impl Strategy<Value = SpelledTy> {
        spelled_with(true)
    }

    /// [`spelled`], without inference variables when `vars` is false.
    fn spelled_with(vars: bool) -> impl Strategy<Value = SpelledTy> {
        let var_weight = if vars { 1 } else { 0 };
        let leaf = prop_oneof![
            2 => (path(), Just(Vec::new())).prop_map(|(path, args)| SpelledTy::Named { path, args }),
            2 => (path(), Just(Vec::new())).prop_map(|(path, args)| SpelledTy::Project { path, args }),
            2 => (0u32..30).prop_map(SpelledTy::Param),
            var_weight => (0u32..4).prop_map(|v| SpelledTy::Var(TyVid(v))),
        ];
        leaf.prop_recursive(3, 24, 3, |inner| {
            prop_oneof![
                (path(), prop::collection::vec(inner.clone(), 1..3))
                    .prop_map(|(path, args)| SpelledTy::Named { path, args }),
                (path(), prop::collection::vec(inner.clone(), 1..3))
                    .prop_map(|(path, args)| SpelledTy::Project { path, args }),
                (inner.clone(), 1u32..3).prop_map(|(elem, rank)| SpelledTy::Array {
                    elem: Box::new(elem),
                    rank,
                }),
                prop::collection::vec(inner.clone(), 2..4).prop_map(SpelledTy::Tuple),
                (inner.clone(), inner.clone()).prop_map(|(arg, ret)| SpelledTy::Fun {
                    arg: Box::new(arg),
                    ret: Box::new(ret),
                }),
                // A node over two copies of one subterm: one node in the table.
                inner.prop_map(|t| SpelledTy::Tuple(vec![t.clone(), t])),
            ]
        })
    }

    /// A pair of types, often equal and often differing in one place.
    fn pair() -> impl Strategy<Value = (SpelledTy, SpelledTy)> {
        spelled().prop_flat_map(|a| {
            let same = Just(a.clone());
            let wrapped = path().prop_map({
                let a = a.clone();
                move |path| SpelledTy::Named {
                    path,
                    args: vec![a.clone()],
                }
            });
            (Just(a), prop_oneof![same, wrapped, spelled()])
        })
    }

    /// Tables already holding `decoys`, paths and types, so ids are not handed
    /// out in the order the type under test meets its nodes.
    fn tables(paths: &[Vec<String>], types: &[SpelledTy]) -> (NameTable, TyTable) {
        let names = NameTable::default();
        for decoy in paths {
            names.intern_path(decoy);
        }
        let tys = TyTable::session();
        for decoy in types {
            decoy.to_ty(&names, &tys);
        }
        (names, tys)
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 512, ..ProptestConfig::default() })]

        #[test]
        fn a_type_converts_to_and_from_its_spelling(
            s in spelled(),
            paths in prop::collection::vec(path(), 0..6),
            types in prop::collection::vec(spelled(), 0..4),
        ) {
            let (names, tys) = tables(&paths, &types);
            let ty = s.to_ty(&names, &tys);
            prop_assert_eq!(&SpelledTy::of(&names, &tys, ty), &s);
        }

        /// Hash-consing's law: two types are one id exactly when their trees
        /// are equal, whatever the table held first.
        #[test]
        fn types_are_equal_exactly_when_their_spellings_are(
            (a, b) in pair(),
            paths in prop::collection::vec(path(), 0..6),
            types in prop::collection::vec(spelled(), 0..4),
        ) {
            let (names, tys) = tables(&paths, &types);
            prop_assert_eq!(a.to_ty(&names, &tys) == b.to_ty(&names, &tys), a == b);
        }

        /// What a node records of its tree — its size, depth and whether a
        /// variable occurs in it — is what a walk of the tree finds.
        #[test]
        fn a_type_knows_its_trees_size_and_groundness(
            s in spelled(),
            types in prop::collection::vec(spelled(), 0..4),
        ) {
            let (names, tys) = tables(&[], &types);
            let ty = s.to_ty(&names, &tys);
            prop_assert_eq!(tys.size(ty), s.size());
            prop_assert_eq!(tys.is_ground(ty), s.is_ground());
        }

        /// Copying a type into another table keeps its tree, and keeps two
        /// types equal exactly when they were: what publishing a session's
        /// type into the env's table, and reading one back, rely on.
        #[test]
        fn importing_a_type_keeps_its_tree(
            (a, b) in pair(),
            types in prop::collection::vec(spelled(), 0..4),
            decoys in prop::collection::vec(spelled_with(false), 0..4),
        ) {
            let (names, session) = tables(&[], &types);
            let ground = TyTable::ground();
            for decoy in &decoys {
                decoy.to_ty(&names, &ground);
            }
            let other = TyTable::session();
            let mut memo = HashMap::default();
            let (ta, tb) = (a.to_ty(&names, &session), b.to_ty(&names, &session));
            let (ia, ib) = (other.import(&session, ta, &mut memo), other.import(&session, tb, &mut memo));
            prop_assert_eq!(&SpelledTy::of(&names, &other, ia), &a);
            prop_assert_eq!(ia == ib, a == b);
            if a.is_ground() {
                let mut memo = HashMap::default();
                let published = ground.import(&session, ta, &mut memo);
                prop_assert_eq!(&SpelledTy::of(&names, &ground, published), &a);
                let mut back = HashMap::default();
                prop_assert_eq!(session.import(&ground, published, &mut back), ta);
            }
        }

        #[test]
        fn a_path_is_exactly_the_segments_it_spells(
            a in path(),
            b in path(),
            decoys in prop::collection::vec(path(), 0..6),
        ) {
            let (names, _) = tables(&decoys, &[]);
            let ty_path = TyPath::new(&names, &a);
            let b: Vec<&str> = b.iter().map(String::as_str).collect();
            prop_assert_eq!(ty_path.is(&names, &b), a == b);
            let a: Vec<&str> = a.iter().map(String::as_str).collect();
            prop_assert!(ty_path.is(&names, &a));
        }

        #[test]
        fn a_type_renders_as_its_spelling_does(
            s in spelled(),
            paths in prop::collection::vec(path(), 0..6),
            types in prop::collection::vec(spelled(), 0..4),
        ) {
            let (names, tys) = tables(&paths, &types);
            let ty = s.to_ty(&names, &tys);
            prop_assert_eq!(tys.render(&names, ty), reference::render(&s));
            prop_assert_eq!(tys.render_fsharp(&names, ty), reference::render_fsharp(&s));
        }
    }
}
