//! Type annotations, read as [`Ty`]s ([`FileFacts::annotation_ty`]), and the
//! checks of an assembly entity that reading one makes.

use super::*;

/// What a type variable in an annotation means where
/// [`FileFacts::annotation_ty`] reads it.
#[derive(Clone, Copy)]
pub(super) enum AnnotationCx<'c> {
    /// Nothing: the annotation declines.
    Plain,
    /// A module-level function binding's head (its parameters' and return
    /// annotations): each name is that binding's implicitly scoped variable
    /// ([`Gen::open_head_typars`]).
    Head(&'c HashMap<String, TyVid>),
    /// A type definition's case fields or record fields: each name is the
    /// definition's declared type parameter at that position, as a
    /// [`TyKind::Param`] ([`FileFacts::project_type_schemes`]).
    DeclTypars(&'c HashMap<String, u32>),
}

impl<'c> AnnotationCx<'c> {
    /// The context of a binding's head: [`Self::Head`] when
    /// [`Gen::open_head_typars`] opened its type variables, and otherwise
    /// [`Self::Plain`], where each declines.
    pub(super) fn head(typars: Option<&'c HashMap<String, TyVid>>) -> Self {
        typars.map_or(AnnotationCx::Plain, AnnotationCx::Head)
    }
}

impl<'a> FileFacts<'a> {
    /// The [`TyKind::Project`] an annotation naming the in-file type `def` applied
    /// to `args` denotes, when [`Self::project_type_paths`] holds its path and
    /// the arity matches.
    fn project_annotation_ty(&self, tys: &TyTable, def: DefId, args: Vec<Ty>) -> Option<Ty> {
        let (path, arity) = self.project_types.get(&def)?;
        (args.len() == *arity).then(|| tys.project(path.clone(), args))
    }

    /// [`Self::project_annotation_ty`] for a type an earlier file declares:
    /// the path and arity that file's inference published for it
    /// ([`PrecedingTypes::earlier_type`]), under the same arity check.
    fn earlier_annotation_ty(
        &self,
        tys: &TyTable,
        file: u32,
        index: u32,
        args: Vec<Ty>,
    ) -> Option<Ty> {
        let (path, arity) = self.preceding.earlier_type(file as usize, index)?;
        (args.len() == *arity).then(|| tys.project(path.clone(), args))
    }

    /// The [`Ty`] an annotation provably denotes, or `None` to defer (Stage
    /// R2-a). Accepts a **bare single-segment `Type::LongIdent`** whose head
    /// the resolver resolved to a concrete entity, peels transparent parens,
    /// and **structurally recurses** function, (non-struct, measure-free)
    /// tuple, and array shapes, whose renderings agree with FCS's canonical
    /// forms byte-for-byte (plan probe R11). Everything else defers:
    /// unresolved/deferred heads, generic/measure applications (`int64
    /// option`, `float<m>`), and every other `Type` shape. A type variable
    /// means what `cx` says it does.
    ///
    /// A concrete [`Resolution::Entity`] bridges via
    /// [`Self::entity_annotation_ty`] (Stage R2-d) — covering `String` under
    /// `open System` at a single-segment head, qualified `System.Int64` at a
    /// multi-segment head's tail, and the F# primitive aliases: `int` records
    /// its FSharp.Core abbreviation **marker**, which the bridge chases
    /// through the pickled target chain to `System.Int32`. There is no
    /// hard-coded alias table — the semantics come from FSharp.Core's own
    /// signature data, and the real-FSharp.Core sweep in
    /// `resolve_fsharp_core.rs` pins that the chase reproduces every
    /// primitive alias exactly. (With no assembly env at all — FSharp.Core
    /// unreferenced — a primitive annotation records nothing and defers,
    /// which is honest: the name genuinely has no binding.)
    ///
    /// Project-defined types (`Local`/`Item`) still defer: their canonical
    /// rendering against the oracle's `<Project>.M.T` form is unprobed (the
    /// plan's R2-d "probe first" rule). Widening any of these qualifiers needs
    /// its own soundness argument in `docs/completed/r2-annotation-typing-plan.md`
    /// first.
    pub(super) fn annotation_ty(
        &self,
        tys: &TyTable,
        ty: &Type,
        cx: AnnotationCx<'_>,
    ) -> Option<Ty> {
        // An annotation is only worth reading if the parser understood the
        // declaration it was written in. Recovery *drops* what it cannot parse,
        // so `let v : System.String. = …` and `let v : System.String[ = …`
        // leave behind trees whose surviving children are indistinguishable
        // from well-formed ones — and FCS rejects both, recovering the binder
        // to whatever it likes (`System.String` for the first, `System.Object`
        // for a rejected generic application). Which of those it picks is not a
        // rule we can reproduce, so the only sound answer on a declaration that
        // did not parse is silence.
        //
        // Asked here rather than at the callers so a new entry point into
        // annotation reading cannot miss it. The recursion re-asks it per node,
        // which is free on a clean parse (the empty-span case answers without
        // touching the tree) and a short ancestor climb per node on a file that
        // did error — where there is nothing to commit anyway.
        if !self.resolved.recovery().declaration_is_intact(ty.syntax()) {
            return None;
        }
        match ty {
            Type::Var(v) => {
                let name = ident_text(&v.ident()?);
                match cx {
                    AnnotationCx::Plain => None,
                    AnnotationCx::Head(typars) => typars.get(&name).map(|&tv| tys.var(tv)),
                    AnnotationCx::DeclTypars(params) => params.get(&name).map(|&i| tys.param(i)),
                }
            }
            Type::Paren(p) => self.annotation_ty(tys, &p.inner()?, cx),
            Type::LongIdent(li) => {
                let li = li.long_ident()?;
                // An active-pattern segment cannot be projected as an ident
                // token; treating the remaining tokens as the path would
                // mis-read it (cannot occur in a well-formed type, but recovery
                // trees can hold anything).
                if li.active_pat_names().next().is_some() {
                    return None;
                }
                let idents: Vec<SyntaxToken> = li.idents().collect();
                // Only a concrete `Entity` recorded at the head's **final**
                // segment — where the resolver roots both a bare and a
                // qualified type — bridges (R2-d); anything else defers.
                match idents.as_slice() {
                    [.., last] => match self.resolved.resolution_at(last.text_range()) {
                        Some(Resolution::Entity(handle)) => self.entity_annotation_ty(tys, handle),
                        Some(Resolution::Local(def)) => {
                            self.project_annotation_ty(tys, def, Vec::new())
                        }
                        Some(Resolution::EarlierFileType { file, index }) => {
                            self.earlier_annotation_ty(tys, file, index, Vec::new())
                        }
                        _ => None,
                    },
                    [] => None,
                }
            }
            Type::Fun(f) => {
                let arg = self.annotation_ty(tys, &f.arg()?, cx)?;
                let ret = self.annotation_ty(tys, &f.ret()?, cx)?;
                Some(tys.fun(arg, ret))
            }
            Type::Tuple(t) => {
                // A struct tuple is a different runtime type; a `/` segment is
                // a unit-of-measure tuple form — both defer.
                if t.is_struct() {
                    return None;
                }
                let mut elems = Vec::new();
                for seg in t.segments() {
                    match seg {
                        TupleSegment::Type(e) => elems.push(self.annotation_ty(tys, &e, cx)?),
                        TupleSegment::Star(_) => {}
                        TupleSegment::Slash(_) => return None,
                    }
                }
                // `TyKind::Tuple` is always arity ≥ 2; a degenerate segment
                // list (recovery) defers.
                if elems.len() < 2 {
                    return None;
                }
                Some(tys.tuple(elems))
            }
            Type::Array(a) => {
                let rank = u32::try_from(a.rank()).ok()?;
                if rank > MAX_ARRAY_RANK {
                    return None;
                }
                let elem = self.annotation_ty(tys, &a.element_type()?, cx)?;
                Some(tys.array(elem, rank))
            }
            // A generic application. The CST has already normalised the two
            // surface spellings — postfix `int list` and prefix
            // `Dictionary<int, string>` — into a head plus an ordered argument
            // list, so only one shape arrives here. A `Type::LongIdentApp`
            // (`Dictionary<int, string>.KeyCollection`) is a different shape
            // that `Ty` cannot spell and stays deferred.
            Type::App(app) => {
                if !application_syntax_is_complete(app) {
                    return None;
                }
                let Type::LongIdent(li) = &app.type_name()? else {
                    return None;
                };
                let li = li.long_ident()?;
                if li.active_pat_names().next().is_some() {
                    return None;
                }
                let last = li.idents().last()?;
                let head = self.resolved.resolution_at(last.text_range());
                if !matches!(
                    head,
                    Some(
                        Resolution::Entity(_)
                            | Resolution::Local(_)
                            | Resolution::EarlierFileType { .. }
                    )
                ) {
                    return None;
                }
                let mut args = Vec::new();
                for arg in app.type_args() {
                    args.push(self.annotation_ty(tys, &arg, cx)?);
                }
                match head? {
                    Resolution::Entity(handle) => self.applied_annotation_ty(tys, handle, args),
                    Resolution::Local(def) => self.project_annotation_ty(tys, def, args),
                    Resolution::EarlierFileType { file, index } => {
                        self.earlier_annotation_ty(tys, file, index, args)
                    }
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// Bridge a concrete annotation-head [`Resolution::Entity`] to a
    /// [`TyKind::Named`] (Stage R2-d), under the same conventions as the
    /// `member_ty.rs` `TypeRef` bridge: **non-generic** and **non-nested**
    /// only, with the same defer set.
    ///
    /// An **abbreviation marker** bridges through its chased terminal
    /// ([`AssemblyEnv::resolve_abbreviation_target`]): `let x : int = …` records the
    /// `int` marker, whose chain (`int` → `int32` → `System.Int32`) lands on
    /// the entity this then bridges exactly like a directly-named one. The
    /// binder-types oracle strips abbreviation layers (fcs-dump's
    /// `renderTypeInScope` renders `AbbreviatedType` through), so the
    /// terminal's FQN is precisely FCS's rendering currency for an annotation
    /// written through an alias. An unchaseable marker defers as before.
    ///
    /// Additionally deferred here:
    ///
    /// - an F# **module** (not a type an annotation can denote — a module
    ///   record at a type head is a resolver artefact to stay silent on);
    /// - a **measure** (`Ty` has no measure story — plan probe R5);
    /// - a **source-renamed** entity (its FCS rendering uses the F# source
    ///   name, which the flat `namespace + IL name` path would misrender);
    /// - `System.Void`, which F# admits only as `typeof<System.Void>`.
    ///
    /// The nested/renamed check is one comparison: the canonical dotted path
    /// must equal [`AssemblyEnv::entity_full_name`], which walks enclosing
    /// entities for nested types and prefers the source name.
    fn entity_annotation_ty(&self, tys: &TyTable, handle: EntityHandle) -> Option<Ty> {
        let entity = self.env.entity(handle);
        if !entity.generic_parameters.is_empty() {
            return None;
        }
        // The marker's *own* attributes, which is where F# actually looks:
        // `CheckEntityAttributes` runs on the entity the name resolved to and
        // does not chase an abbreviation. Measured — a use of `type Chain =
        // WarnObsoleteAbbrev` reports nothing, though declaring the chain
        // reports FS0044 at the link. An abbreviation emits no ECMA TypeDef, so
        // this reads the marker `fsharp_pickle_merge` synthesises from the
        // pickle; a marker that reported its attributes absent rather than
        // reading them publishes a type F# rejects.
        if rejected_by_attributes(entity) {
            return None;
        }
        let (handle, entity) = if entity.kind == EntityKind::Abbreviation {
            let terminal = self.env.resolve_abbreviation_target(handle)?;
            (terminal, self.env.entity(terminal))
        } else {
            (handle, entity)
        };
        if !entity.generic_parameters.is_empty() {
            return None;
        }
        // The **terminal**'s attributes, which F# does *not* consult at a use of
        // the alias — so this is deliberate over-rejection, not a second attempt
        // at reproducing `CheckEntityAttributes`. It costs a decline on `type
        // Chain = ErrorObsoleteAtom`, which F# accepts.
        //
        // Kept because the two questions differ in kind: whether F# *rejects the
        // name written* is decided at the marker above, while what we publish is
        // the terminal — and an alias is not a licence to publish a type whose
        // own declaration says it may not be used. The structural checks below
        // sit here for the sharper form of that reason: `is_byref_like` is a
        // property of the published type and a marker carries `false`.
        if rejected_by_attributes(entity) {
            return None;
        }
        if matches!(
            entity.kind,
            EntityKind::Module | EntityKind::Abbreviation | EntityKind::Measure
        ) {
            return None;
        }
        // A **byref-like** type (`[<IsByRefLike>]`, `Span<_>` and friends) is a
        // type F# admits in exactly one annotation position — a parameter —
        // and rejects everywhere else this bridge can reach: FS0431 for a
        // let-bound value, FS0412 for a return type or a type argument.
        // Measured, and checked HERE rather than at the resolved head because
        // an abbreviation marker carries `is_byref_like = false`: `type A = R`
        // over a byref-like `R` would otherwise commit `A`'s terminal.
        //
        // Nothing here distinguishes a parameter annotation from the rest, and
        // `Ty` cannot carry byref-likeness to a later consumer, so the sound
        // reading is to commit one nowhere. It costs `let f (s : Span<int>)`,
        // the single legal position.
        if entity.is_byref_like {
            return None;
        }
        let mut path: Vec<String> = entity.namespace.clone();
        path.push(entity.name.clone());
        // `System.Void` is refused: F# admits it only as `typeof<System.Void>`
        // (FS0411), so it is never a type an annotation denotes — and
        // `Ty::render_fsharp` would alias it to `unit`, making a rejected
        // annotation hover as a plausible one. (F#'s own `unit` is
        // `Microsoft.FSharp.Core.Unit`, which bridges like any other type.)
        if path == ["System", "Void"] {
            return None;
        }
        if self.env.entity_full_name(handle) != path.join(".") {
            return None;
        }
        Some(tys.named_path(self.env.names(), &path))
    }

    /// Bridge a **generic application** whose head resolved to `handle` and
    /// whose written arguments are `args`, under the nullary bridge's
    /// conventions plus three of its own.
    ///
    /// The head must declare **exactly as many generic parameters as the
    /// application writes**. A partial application is not a type, and a count
    /// mismatch means the head is not the tycon we think it is.
    ///
    /// An **abbreviation** head defers, unlike the nullary bridge which chases
    /// it. A generic alias may permute or fix its target's arguments
    /// (`type Swap<'a, 'b> = Map<'b, 'a>`, `type IntMap<'V> = Map<int, 'V>`), so
    /// applying the written arguments positionally to the chased tycon can
    /// silently produce a different type, and arity alone does not rule that out
    /// — `Swap` preserves it. Recovering the common aliases needs a chase that
    /// reports the target's argument *structure*, which
    /// [`AbbreviationTarget`](borzoi_assembly::AbbreviationTarget) models but
    /// [`AssemblyEnv::resolve_abbreviation_target`] does not return.
    ///
    /// A head **constraining any of its parameters** defers — see
    /// [`constrained_parameter`]. Nothing here checks a constraint against an
    /// argument, so the only sound reading of one is to decline, and that is not
    /// a coverage nicety: F# rejects both `System.Nullable<string[]>` (IL
    /// `struct`/`new()`) and `Constrained<int -> int>` (F# `comparison`) and
    /// recovers the annotated binder to `System.Object`, so committing the
    /// written type is a wrong answer rather than a commit on an erroring line.
    ///
    /// Four heads name a shape FCS canonicalises **structurally** rather than as
    /// an application, so committing the application form would be a wrong
    /// string for the right type. Measured off `infer_annotation_shape_gen_diff`,
    /// not reasoned out. The set is closed because what it enumerates is closed:
    /// three are [`Ty`]'s own structural variants, one head each, and the fourth
    /// is the struct tuple, which `Ty` cannot represent at all — which is why
    /// [`Self::annotation_ty`] already declines a written `struct (a * b)`.
    fn applied_annotation_ty(
        &self,
        tys: &TyTable,
        handle: EntityHandle,
        args: Vec<Ty>,
    ) -> Option<Ty> {
        let entity = self.env.entity(handle);
        if entity.is_byref_like {
            return None;
        }
        // An error-obsolete *argument* is caught by the recursion, which reaches
        // this bridge or the nullary one for every written argument; this covers
        // the head.
        if rejected_by_attributes(entity) {
            return None;
        }
        if entity.generic_parameters.len() != args.len() {
            return None;
        }
        // FSharp.Core's own generic abbreviation (`int option`), read through
        // to the target FCS names; any other abbreviation declines.
        if entity.kind == EntityKind::Abbreviation {
            return crate::function_ty::fsharp_core_abbreviation_ty(self.env, tys, handle, args);
        }
        if matches!(entity.kind, EntityKind::Module | EntityKind::Measure) {
            return None;
        }
        if entity.generic_parameters.iter().any(constrained_parameter) {
            return None;
        }
        // Neither NESTED nor source-RENAMED, the same combined test the nullary
        // bridge applies and for the same second reason: `Ty` carries one path
        // and a renamed entity needs two — FCS's canonical currency is the
        // COMPILED name (`Microsoft.FSharp.Core.FSharpResult`) while
        // [`Ty::render_fsharp`] must show the source name a user can write
        // (`Result`). The exception is FSharp.Core's own renamed generics
        // (`FSHARP_CORE_RENAMED`), which [`Ty::render_fsharp`] shows by their
        // F# name: a renamed head is read through the signature bridge's rule
        // for them ([`crate::function_ty::applied_type_path`]), so the two
        // bridges name one type one way.
        let mut path: Vec<String> = entity.namespace.clone();
        path.push(entity.name.clone());
        if self.env.entity_full_name(handle) != path.join(".") {
            let path = crate::function_ty::applied_type_path(self.env, handle)?;
            return Some(tys.app(path, args));
        }
        if matches!(
            path.iter()
                .map(String::as_str)
                .collect::<Vec<_>>()
                .as_slice(),
            ["System", "Tuple"]
                | ["System", "ValueTuple"]
                | ["Microsoft", "FSharp", "Core", "FSharpFunc"]
        ) {
            return None;
        }
        Some(tys.app(TyPath::new(self.env.names(), &path), args))
    }
}

/// Whether a written generic application's **punctuation accounts for every
/// argument it appears to have**.
///
/// Parser recovery drops what it could not parse rather than representing it, so
/// the surviving `Type` children of `KeyValuePair<int, string,>` are a
/// well-formed-looking pair — while FCS reports FS1241 and recovers the binder
/// to `System.Object`. Committing there is a wrong hover on exactly the
/// half-typed input an editor sees most.
///
/// The punctuation is what still records the loss: a well-formed prefix
/// application has one `,` between each pair of arguments and is closed by `>`,
/// so a trailing comma or a missing bracket does not add up. The postfix form
/// (`int list`) carries neither and has exactly one argument.
///
/// Not done by looking for `ERROR` in the tree: the parser emits **zero-width
/// `ERROR` tokens as ordinary structure** — a cleanly-parsed
/// `KeyValuePair<int, string>` contains one — so their presence says nothing
/// about whether anything was lost.
fn application_syntax_is_complete(app: &borzoi_cst::syntax::AppType) -> bool {
    let args = app.type_args().len();
    if app.is_postfix() {
        return args == 1;
    }
    let mut commas = 0usize;
    let (mut opened, mut closed) = (false, false);
    for token in app
        .syntax()
        .children_with_tokens()
        .filter_map(|el| el.into_token())
    {
        match token.kind() {
            SyntaxKind::COMMA_TOK => commas += 1,
            SyntaxKind::LESS_TOK => opened = true,
            SyntaxKind::GREATER_TOK => closed = true,
            _ => {}
        }
    }
    opened && closed && args > 0 && commas + 1 == args
}

/// Whether a generic parameter constrains what may be substituted for it, in
/// any way this crate can see.
///
/// Nothing in the annotation bridge *checks* a constraint against an argument,
/// so the only sound reading of one is to decline the whole application. That
/// makes presence the entire question, and it has to be asked of every channel
/// at once: the IL flags and named type constraints
/// (`System.Nullable<'T>`'s `struct` + `new()`), and the F#-only ones
/// ([`FSharpConstraints`]), which have no IL encoding at all. Reading one
/// channel and not the other is how `System.Nullable<string[]>` — which F#
/// rejects — gets published.
///
/// [`FSharpConstraints::Unknowable`] counts as constrained for the same reason:
/// an unread pickle means unread constraints, not absent ones.
///
/// `allows_ref_struct` is deliberately absent: it *widens* what may be
/// substituted rather than narrowing it, so it never makes an application
/// illegal. `nullability` is an annotation, not a constraint.
pub(crate) fn constrained_parameter(p: &borzoi_assembly::TypeParameter) -> bool {
    p.reference_type_constraint
        || p.value_type_constraint
        || p.default_constructor_constraint
        || p.is_unmanaged
        || !p.type_constraints.is_empty()
        || p.fsharp_constraints != FSharpConstraints::Free
}

/// The highest array rank F# accepts in an annotation.
///
/// Our parser reads any rank the brackets spell, so the limit has to be applied
/// here or a rank the language rejects reaches [`TyKind::Array`]. Measured against
/// the `binder-types` oracle rather than read off a spec: `int[,…]` at rank 32
/// types as itself, and at rank 33 FCS reports the annotation and recovers the
/// binder to `System.Object` — in argument position (`System.Func<int[…], bool>`)
/// exactly as in bare position, which is why the check sits on the shared array
/// arm rather than in either bridge.
const MAX_ARRAY_RANK: u32 = 32;

/// Whether F# **rejects a use** of this entity on account of its attributes.
///
/// This reproduces the F# compiler's `CheckEntityAttributes`
/// (`src/Compiler/Checking/AttributeChecking.fs`), which is the whole of what
/// it consults at a type use, and is why the set is closed rather than however
/// many channels a reviewer has thought of so far. It dispatches to
/// `CheckILAttributes` (obsolescence, experimentality) or `CheckFSharpAttributes`
/// (those plus compiler-message and unverifiability), and of the four only two
/// can produce an **error**:
///
/// - `[<Obsolete(_, true)>]`, on either path;
/// - `[<CompilerMessage(_, n, IsError = true)>]`, on the F#-tycon path.
///
/// Experimental and unverifiable warn and never reject, so neither is read.
///
/// Asking both of every entity — rather than gating the second on whether the
/// entity came through F# signature data — costs only a decline on an IL type
/// carrying an attribute F# would not consult there, which is not a shape a
/// C# compiler emits.
pub(crate) fn rejected_by_attributes(entity: &borzoi_assembly::Entity) -> bool {
    error_obsolete(entity) || error_compiler_message(entity)
}

/// Whether an entity is marked `[<Obsolete(_, true)>]` — obsolete **as an
/// error** rather than as a warning.
///
/// F# rejects an annotation naming one (FS0101) and recovers the binder to
/// `System.Object`, so committing the written type is a wrong answer rather than
/// a commit on an erroring line. The `is_error` flag is the whole discriminator,
/// measured: `[<Obsolete("…", false)>] Warn<int>` types as `Warn<System.Int32>`
/// while `[<Obsolete("…", true)>] Old<int>` types as `System.Object`, and an
/// error-obsolete type used as a generic *argument* sinks the whole application
/// the same way.
fn error_obsolete(entity: &borzoi_assembly::Entity) -> bool {
    entity.obsolete.as_ref().is_some_and(|o| o.is_error)
}

/// Whether an entity carries `[<CompilerMessage(_, n, IsError = true)>]` at a
/// message number F# does not suppress.
///
/// F#'s `CheckCompilerMessageAttribute` errors when `IsError` holds, with two
/// exemptions: number **3501** is suppressed outright (FSharp.Core's `nameof`
/// marker), and 1204 is suppressed only while compiling FSharp.Core itself,
/// which we never are. An *undecodable* marker is read as "not suppressed",
/// so a payload we could not parse declines rather than commits.
fn error_compiler_message(entity: &borzoi_assembly::Entity) -> bool {
    /// FSharp.Core's `nameof` marker, which F# suppresses whatever `IsError`
    /// says.
    const NAMEOF_MARKER: i32 = 3501;
    entity
        .compiler_message
        .as_ref()
        .is_some_and(|m| m.is_error && m.marker != Some(NAMEOF_MARKER))
}
