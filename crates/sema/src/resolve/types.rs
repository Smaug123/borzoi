//! Type definitions (`define_*`) and type-position name resolution.

use borzoi_cst::syntax::{
    ActivePatName, AstNode, LongIdentPat, MemberDefn, MemberLeading, Pat, SyntaxKind, SyntaxToken,
    TupleSegment, Type, TypeDefn, TypeDefnRepr,
};

use rowan::TextRange;

use crate::assembly_env::EntityHandle;
use crate::binders::{BinderRole, binders};
use crate::def::{Def, DefId, DefKind};

use super::id_text;
use super::model::{
    CaseKind, DeferredReason, ExportDeclKind, ExportedItem, ItemId, Resolution, SlotClass,
};
use super::state::{
    ActivePatternShape, MemberEntry, Resolver, ScopeEntry, ShadowVeto, TieredResolution,
};

/// The token-free decision of [`Resolver::decide_type_path`] — how a
/// type-syntactic dotted path resolves, computed without touching the
/// resolution map. The recording shell ([`Resolver::resolve_type_path`]) turns
/// it into [`Resolution`] records at the source tokens; a token-free caller
/// (the synthesised `…Attribute` attribute candidate,
/// `docs/extension-scope-enumeration-plan.md` §2(d)) can consume the decision
/// directly.
pub(super) enum TypePathResolution {
    /// A single segment names an in-file `type` def, which shadows any
    /// assembly type of that name (the decision returns this only for a
    /// single segment — in-file lookup is arity-agnostic and never dotted).
    InFileType(DefId),
    /// The path resolved through the assembly precedence tiers — the
    /// per-segment records to apply, keyed by segment index (a
    /// namespace-qualifier or unresolvable-tail segment is
    /// [`Resolution::Deferred`]; a rooting/nested type is
    /// [`Resolution::Entity`]), and the **leaf** type the whole path names
    /// (`Some` iff the winning reading owned the path; `None` for a partial
    /// reading whose tail deferred). The attribute resolution keys its verdict
    /// on the leaf; the recording shell ignores it.
    Assembly {
        idx_recs: Vec<(usize, Resolution)>,
        leaf: Option<EntityHandle>,
    },
    /// Resolution deferred — a shadow is possible but unpinnable (an opaque /
    /// unmodelled `open`, a project shadow, an in-scope auto-open type, a
    /// forward `rec` declaration, an abbreviation marker, a nested-module
    /// descent). The recording shell defers as shadowable.
    Deferred,
    /// Nothing in the model resolves *or shadows* this path — the recording
    /// shell records nothing, the signal a consumer reads as "no shadow
    /// possible" (see [`Resolver::defer_shadowable_type`]).
    NoMatch,
}

/// One attribute candidate's verdict (EX-3 §2(d); see
/// [`Resolver::resolve_attribute_type`]): [`TypePathResolution`] refined by
/// the attribute-specific guards (the project-type name guard, the
/// partial-reading decline) into the three outcomes the suffix-first candidate
/// walk distinguishes — a commitment, a no-claim, and a clean miss that lets
/// the next candidate be tried.
enum AttrCandidate {
    /// The candidate names exactly this type ([`Resolution::Local`] for an
    /// in-file definition, [`Resolution::Entity`] for an assembly type).
    Resolved(Resolution),
    /// The candidate cannot be pinned — no claim, and no further candidate is
    /// consulted (a shadow that could redirect this candidate would win in
    /// FCS over anything a later candidate resolves to).
    Deferred,
    /// The candidate misses everywhere we model, with no shadow possible: FCS
    /// would fail it too, so the next candidate may be tried.
    NoMatch,
}

impl<'a> Resolver<'a> {
    /// Intern a genuine type definition's name as a first-class
    /// [`DefKind::Type`] binder: record its self-resolution (so the defining
    /// occurrence answers go-to-definition) and enter it in [`Self::type_defs`]
    /// for same-file type-name use resolution. The binder is a
    /// [`Resolution::Local`] — reachable within this file's def arena; exporting
    /// it for cross-file qualified references is a later slice (see
    /// [`Self::type_defs`]).
    pub(super) fn define_type(
        &mut self,
        name: &SyntaxToken,
        slot: SlotClass,
        access_root_len: Option<usize>,
    ) {
        let def = Def::from_token(name, DefKind::Type);
        let key = id_text(&def.name).to_string();
        let range = def.range;
        let id = self.intern(def);
        self.record(range, Resolution::Local(id));
        // Filed under the current container path (see [`Self::type_defs`]). Last
        // definition of a name wins — a redefinition shadows the earlier one for
        // later uses, mirroring the value frames' last-wins.
        self.type_defs
            .entry(self.container_path.clone())
            .or_default()
            .insert(key.clone(), id);
        // The parallel slot-class entry (same key, same last-wins) — whether
        // this type's name enters FCS's unqualified slot and can evict a
        // same-named value ([`head_value_slot`](Self::head_value_slot)).
        self.type_slot_classes
            .entry(self.container_path.clone())
            .or_default()
            .insert(key.clone(), slot);
        // The parallel access-root entry (same key, same last-wins): a `private`
        // type is accessible only within its container's subtree, so a same-file
        // module-qualified `A.Foo.Red` from an inaccessible site (a sibling) does
        // not resolve its case/member ([`Self::type_access_roots`]).
        self.type_access_roots
            .entry(self.container_path.clone())
            .or_default()
            .insert(key.clone(), access_root_len);
        self.mark_decl(&key).ty = true;
    }

    /// Intern the cases of a union `type T = A | B of …` as [`DefKind::UnionCase`]
    /// binders: each case's defining occurrence resolves to itself, and — unless
    /// the union is `[<RequireQualifiedAccess>]` (`require_qualified`) — the case
    /// is added to the current container's value frame ([`module_frame`](Self::module_frame))
    /// at its source position, so an unqualified use resolves to it. A non-union
    /// definition contributes no cases.
    ///
    /// Cases are value-namespace entries living in the same position-ordered,
    /// container-scoped frame as values (see [`Self::top_level`]); that gives
    /// source-order shadowing against a same-named value, isolation from sibling
    /// `namespace` blocks, and visibility in nested modules, all for free via
    /// [`lookup`](Self::lookup) / [`case_reference`](Self::case_reference). A
    /// require-qualified union's cases are simply not added — reachable only as
    /// `T.Case` (a later slice). The binder is a [`Resolution::Local`]; exporting
    /// cases for cross-file qualified references is a later slice, mirroring
    /// [`define_type`](Self::define_type).
    pub(super) fn define_union_cases(
        &mut self,
        defn: &TypeDefn,
        type_name: &SyntaxToken,
        require_qualified: bool,
    ) {
        let Some(TypeDefnRepr::Union(u)) = defn.repr() else {
            return;
        };
        // A case of a `private` union type is scoped to the type's container
        // (oracle-pinned D3), so its export inherits that privacy.
        let type_is_private = super::decls::header_is_private(defn.syntax());
        let type_key = id_text(type_name.text()).to_string();
        for case in u.cases() {
            // An operator-named case (`([])` / `( :: )`, FSharp.Core's `list`) has
            // no identifier token; FCS records its compiled `op_Nil` /
            // `op_ColonColon` name, so define it under that.
            let def = if let Some(ident) = case.ident() {
                Def::from_token(&ident, DefKind::UnionCase)
            } else if let Some((name, range)) = case.operator_name() {
                Def::from_op_name(name, range, DefKind::UnionCase)
            } else {
                continue;
            };
            let name = id_text(&def.name).to_string();
            let range = def.range;
            let id = self.intern(def);
            // A non-qualified union case is *also* a value-namespace member,
            // reachable unqualified (`open M; Red`) and via the shortcut (`M.Red`,
            // type elided). Export it ([`export_case`]) and use the resulting
            // [`Resolution::Item`] for the declaration *and* the frame entry — one
            // identity everywhere, so find-references / rename span the case's
            // cross-file uses too. A require-qualified case is not value-exported or
            // framed (reachable only as `T.Case`); an anonymous-root case cannot be
            // exported, so it stays a `Local` and its module is marked hidden.
            let res = if require_qualified {
                // Reachable only as `T.Case`: give it a cross-file handle via the
                // type-qualified index (no value-namespace export) and use that
                // `Item` everywhere — one identity, like a non-RQA case.
                match self.export_require_qualified_case(
                    &name,
                    id,
                    &type_key,
                    type_is_private,
                    CaseKind::Union {
                        require_qualified: true,
                    },
                ) {
                    Some(item_id) => Resolution::Item(item_id),
                    None => Resolution::Local(id),
                }
            } else {
                match self.export_case(
                    &name,
                    id,
                    type_is_private,
                    CaseKind::Union {
                        require_qualified: false,
                    },
                ) {
                    Some(item_id) => {
                        // Reuse the value-namespace handle for the type-qualified
                        // path too — one identity for both `Mod.Case` and `T.Case`.
                        self.export_type_qualified_case(&type_key, &name);
                        Resolution::Item(item_id)
                    }
                    None => {
                        self.note_hidden_value_module(self.container_path.clone());
                        Resolution::Local(id)
                    }
                }
            };
            // Every union case is reachable type-qualified (`Color.Red`, RQA or
            // not), so file its *resolution* in the container-scoped
            // [`Self::type_cases`] index keyed by the type name (the same index enum
            // cases use). Storing the resolution — not the bare `DefId` — keeps one
            // identity: a non-RQA union case's `Color.Red` resolves to the same
            // `Item` as its declaration and bare / `Mod.Case` uses.
            self.type_cases
                .entry(self.container_path.clone())
                .or_default()
                .entry(type_key.clone())
                .or_default()
                .insert(name.clone(), res);
            // The defining occurrence resolves to itself.
            self.record(range, res);
            if !require_qualified {
                // A non-RQA union case is a bare-reachable constructor (value
                // namespace) → record it in the container's declared-name view, so a
                // same-named type elsewhere in the container is seen as contention.
                // (RQA cases are reachable only as `Type.Case`, so they never collide
                // with a module-qualifier segment — not recorded.)
                self.mark_decl(&name).union_case = true;
                let generation = self.open_generation;
                self.module_frame()
                    .entries
                    .push(ScopeEntry::binding(name, res, generation));
            }
        }
    }

    /// Intern the cases of an `enum` definition (`type Color = Red = 0 | …`,
    /// [`TypeDefnRepr::Enum`]) as [`DefKind::EnumCase`] binders keyed by the enum
    /// `type_name`. Each case's defining occurrence resolves to itself, and the
    /// case enters the require-qualified [`Self::type_cases`] index — **not** the
    /// value frame, since an enum case is reachable only as `Color.Red`, never
    /// bare `Red` (FCS reports bare `Red` as `FS0039`). A qualified use is
    /// resolved by [`resolve_long_ident`](Self::resolve_long_ident) via that
    /// index. A non-enum definition contributes no cases. The binder is a
    /// [`Resolution::Local`]; cross-file `A.Color.Red` is resolved through the
    /// [`ProjectItems`] type-qualified-case index. The caller clears any prior
    /// cases at `type_name` (last-wins on redefinition) before this runs.
    pub(super) fn define_enum_cases(&mut self, defn: &TypeDefn, type_name: &SyntaxToken) {
        let type_key = id_text(type_name.text()).to_string();
        let Some(TypeDefnRepr::Enum(e)) = defn.repr() else {
            return;
        };
        // A case of a `private` enum inherits the type's container scope.
        let type_is_private = super::decls::header_is_private(defn.syntax());
        for case in e.cases() {
            // An operator-named enum case (`| ([]) = 0`, FCS's bar-led
            // `unionCaseName EQUALS`) carries the compiled `op_Nil` /
            // `op_ColonColon` name rather than an identifier token.
            let def = if let Some(ident) = case.ident() {
                Def::from_token(&ident, DefKind::EnumCase)
            } else if let Some((name, range)) = case.operator_name() {
                Def::from_op_name(name, range, DefKind::EnumCase)
            } else {
                continue;
            };
            let case_name = id_text(&def.name).to_string();
            let range = def.range;
            let id = self.intern(def);
            // Reachable only as `Color.Red`: give it a cross-file handle so a later
            // file's `Lib.Color.Red` resolves it, and use that `Item` everywhere
            // (declaration and same-file `Color.Red`) — one identity. An
            // anonymous-root enum case has no handle and stays a `Local`.
            let res = match self.export_require_qualified_case(
                &case_name,
                id,
                &type_key,
                type_is_private,
                CaseKind::Enum,
            ) {
                Some(item_id) => Resolution::Item(item_id),
                None => Resolution::Local(id),
            };
            // The defining occurrence self-resolves (go-to-definition on the case).
            self.record(range, res);
            self.type_cases
                .entry(self.container_path.clone())
                .or_default()
                .entry(type_key.clone())
                .or_default()
                .insert(case_name.clone(), res);
        }
    }

    /// Intern an `exception E of …` definition's constructor name as a
    /// [`DefKind::ExceptionCase`] value binder: record its self-resolution (so
    /// the defining occurrence answers go-to-definition) and add it to the
    /// current container's value frame ([`module_frame`](Self::module_frame)) at
    /// its source position, so an unqualified use resolves.
    ///
    /// The single-constructor analogue of [`define_union_cases`](Self::define_union_cases):
    /// the constructor is a value-namespace entry in the same position-ordered,
    /// container-scoped frame as values and union cases, giving source-order
    /// shadowing against a same-named value, isolation from sibling `namespace`
    /// blocks, and visibility in nested modules for free via
    /// [`lookup`](Self::lookup) / [`case_reference`](Self::case_reference). An
    /// exception is never `[<RequireQualifiedAccess>]`, so it is always added.
    /// The binder is a [`Resolution::Local`]; exporting it for cross-file
    /// qualified references is a later slice, mirroring the union-case path.
    pub(super) fn define_exception_case(&mut self, name: &SyntaxToken, is_private: bool) {
        let def = Def::from_token(name, DefKind::ExceptionCase);
        let entry_name = id_text(&def.name).to_string();
        let range = def.range;
        let id = self.intern(def);
        // An exception constructor is a value-namespace member, like a non-qualified
        // union case: export it and use the resulting `Item` as the one identity for
        // declaration, frame entry, and cross-file open (so find-references spans
        // its cross-file uses); an anonymous-root constructor stays `Local` and its
        // module is marked hidden.
        let res = match self.export_case(&entry_name, id, is_private, CaseKind::Exception) {
            Some(item_id) => Resolution::Item(item_id),
            None => {
                self.note_hidden_value_module(self.container_path.clone());
                Resolution::Local(id)
            }
        };
        self.record(range, res);
        // The constructor is a dottable value at a *segment* (`E.x` is member
        // access on it) — but distinct from a `let` value, because at a dotted
        // *head* it does not commit member access (see [`DeclKinds::exception_ctor`]).
        self.mark_decl(&entry_name).exception_ctor = true;
        let generation = self.open_generation;
        self.module_frame()
            .entries
            .push(ScopeEntry::binding(entry_name, res, generation));
    }

    /// Intern an active-pattern definition's recognizer and cases
    /// (`let (|Even|Odd|) … = …`), returning the case value-frame *entries* the
    /// caller makes visible (each a distinct per-case identity ranged at the
    /// recognizer span). Mirrors FCS:
    ///
    /// - The **recognizer** is interned as a [`DefKind::ActivePattern`] binder at
    ///   the `|Even|Odd|` name span ([`ActivePatName::name_range`], parens
    ///   excluded), and its occurrence self-resolves — its own symbol (`Even`/`Odd`
    ///   uses are separate, below), so find-references on the recognizer name does
    ///   not pull in case uses.
    /// - Each **case token** (`Even`, `Odd`; the trailing `_` of a partial
    ///   `(|Parse|_|)` is skipped) is interned as a [`DefKind::ActivePatternCase`]
    ///   binder at its own range and self-resolves, so the *defining occurrence*
    ///   in the `(|…|)` name answers go-to-definition — a distinct symbol from the
    ///   recognizer and from the case *uses*, as FCS reports it (full name `Even`
    ///   vs `…(|Even|Odd|).Even`).
    /// - Each case **entry** maps the case name to a per-case
    ///   [`DefKind::ActivePattern`] def *also* ranged at the recognizer span:
    ///   go-to-definition on a use jumps to the recognizer (FCS reports the use's
    ///   declaration as that span, not the case token), but each case keeps a
    ///   **distinct** [`Resolution`] so find-references / rename on `Even` does not
    ///   pull in `Odd`. The entry lives in the same position-ordered,
    ///   container-scoped value frame as union / exception cases (added by the
    ///   caller), and is recognised in pattern position by
    ///   [`case_reference`](Self::case_reference). Unlike those, however, an
    ///   active-pattern case is **not** a value in *expression* position
    ///   (`let v = Even` is FCS `FS0039`), so [`lookup`](Self::lookup) skips it —
    ///   which also leaves a total recognizer's own body constructing a case
    ///   (`… then Even`) *declined*: a bare case name there is ambiguous between a
    ///   result-case construction (FCS `ActivePatternCase`) and a fresh uppercase
    ///   pattern rebinding (FCS a fresh local), which this pass cannot tell apart,
    ///   so the caller pushes a decline *barrier* around the RHS
    ///   ([`ScopeEntry::ap_case_barrier`], see [`resolve_rhss`](Self::resolve_rhss))
    ///   that only stops the body use committing an outer same-named value. A
    ///   sound coverage gap, never a wrong answer.
    ///
    /// The binders are [`Resolution::Local`]; exporting the recognizer for
    /// cross-file qualified references is a later slice, mirroring the union- and
    /// exception-case paths.
    ///
    /// **Parameterized patterns (same-file done; cross-file pending):** a
    /// *parameterized* active pattern's arguments in a use (`match n with DivBy
    /// divisor`) are a mix of expression *parameters* (FCS resolves `divisor` to an
    /// outer value) and a result *sub-pattern* (`Parse v`, where `v` binds). The
    /// resolution-independent [`binders`](crate::binders) walk cannot tell them
    /// apart — it has no recognizer shape — so it fabricates a binder for every
    /// applied-head *name* argument (a literal like `DivBy 3` binds nothing and is
    /// already correct). For a **same-file** recognizer this is now resolved: this
    /// method stores the recognizer's [`ActivePatternShape`] (the `arity` parameter
    /// plus the totality / case-count read off `apn`), and
    /// [`resolve_pat_types`](Self::resolve_pat_types)'
    /// [`split_active_pattern_args`](Self::split_active_pattern_args) keys FCS's
    /// parameter/result split on it, excluding the parameter binders and resolving
    /// them as expressions (Stage 2 of
    /// `docs/parameterized-active-pattern-args-plan.md`). A **cross-file** or
    /// referenced-assembly recognizer carries no shape (the boundary records only
    /// `is_case`), so its parameter arguments still fabricate binders — the
    /// remaining gap, tightened in Stage 3.
    pub(super) fn define_active_pattern(
        &mut self,
        apn: &ActivePatName,
        at_module_level: bool,
        arity: Option<usize>,
        is_private: bool,
    ) -> Vec<ScopeEntry> {
        let Some(range) = apn.name_range() else {
            // A malformed name with no `|` (recovery): nothing to intern.
            return Vec::new();
        };
        // The recognizer's *shape*, read off the name it already carries (the
        // caller supplies `arity`, the only part the name does not hold): `total`
        // = no trailing `|_|` (the trailing `_` is the sole non-IDENT case token
        // the loop below skips), `single_case` = exactly one IDENT case token.
        // Stored per case use-def below, keyed by each `use_id`; no consumer yet
        // (Stage 1 of `docs/parameterized-active-pattern-args-plan.md`).
        let ident_case_count = apn
            .case_tokens()
            .filter(|t| t.kind() == SyntaxKind::IDENT_TOK)
            .count();
        let has_wildcard_case = apn.case_tokens().any(|t| t.kind() != SyntaxKind::IDENT_TOK);
        let shape = ActivePatternShape {
            total: !has_wildcard_case,
            single_case: ident_case_count == 1,
            arity,
        };
        // The recognizer value, ranged over the `|Even|Odd|` span. Its `name` is
        // the literal source (`(|Even|Odd|)`) — only used for hover display; case
        // uses look up by case name, not by this.
        let recognizer = Def {
            name: apn.syntax().text().to_string(),
            range,
            kind: DefKind::ActivePattern,
            provisional: false,
        };
        let recognizer_id = self.intern(recognizer);
        self.record(range, Resolution::Local(recognizer_id));

        let mut entries = Vec::new();
        for tok in apn.case_tokens() {
            // The trailing `_` of a partial active pattern is not a case.
            if tok.kind() != SyntaxKind::IDENT_TOK {
                continue;
            }
            // The case token's defining occurrence self-resolves to itself, as a
            // distinct symbol from the recognizer (FCS reports it with full name
            // `Even`, not `…(|Even|Odd|)`).
            let token_def = Def::from_token(&tok, DefKind::ActivePatternCase);
            let case_name = id_text(&token_def.name).to_string();
            let token_range = token_def.range;
            let token_id = self.intern(token_def);
            self.record(token_range, Resolution::Local(token_id));
            // A *use* of the case resolves to a per-case def ranged at the
            // recognizer span: go-to-definition jumps to the recognizer (FCS
            // reports the use's declaration as that span), but each case keeps a
            // **distinct** [`Resolution`] identity so find-references / rename on
            // one case does not pull in its siblings (`Even` vs `Odd`). The def
            // carries the case name for hover. (The token occurrence above and
            // these uses are separate symbols in FCS, so they are not grouped —
            // matching FCS, which gives them distinct full names.)
            let use_def = Def {
                name: case_name.clone(),
                range,
                kind: DefKind::ActivePattern,
                provisional: false,
            };
            let use_id = self.intern(use_def);
            // Every case of one recognizer shares its shape, keyed by the *use*
            // identity — the case's `use_id` def, which is also the `ExportedItem`'s
            // `def` below, so a same-file `Item` maps back to it (Stage 3a).
            self.active_pattern_shape.insert(use_id, shape);
            // A *module-level* case name occupies the value/pattern namespace of its
            // container; record it so a same-named type is seen as contention for the
            // module-qualifier (in pattern position). A *local* active pattern (inside a
            // function body) is **not** a member of the enclosing module — `Pal.Color`
            // cannot reach it — so it must not touch `container_decls`, or a later
            // `match … with Pal.Color.Red` would wrongly defer instead of resolving the
            // union case.
            //
            // Stage 3a: a non-anonymous-root module-level case gets ONE
            // project-global identity — a `Resolution::Item` shared by same-file AND
            // cross-file uses (find-references/rename span both — the union-case
            // precedent) — via an [`ExportedItem`] with `qualified: None` (so
            // value-namespace queries never see it; it rides `value_exports` as a
            // pattern-only case through the decl derivation) whose `def` is the
            // recognizer-span `use_id`, pointing go-to-def at the recognizer. A
            // distinct `ItemId` per case keeps find-references per-case. An
            // anonymous-root or *local* case has no cross-file handle: it stays a
            // file-local `Resolution::Local(use_id)`.
            let case_res = if at_module_level {
                self.mark_decl(&case_name).active_pattern = true;
                let item = if self.anonymous_root {
                    None
                } else {
                    let item_idx = self.items.len();
                    let item_id = ItemId::new(self.item_base as usize + item_idx);
                    self.items.push(ExportedItem {
                        name: case_name.clone(),
                        qualified: None,
                        id: item_id,
                        def: use_id,
                        case: None,
                        access_root_len: self.export_access_root_len(is_private),
                    });
                    Some((item_idx, item_id))
                };
                let mut path = self.container_path.clone();
                path.push(case_name.clone());
                self.push_export_decl(
                    path,
                    token_range.start(),
                    ExportDeclKind::ActivePatternCase {
                        item: item.map(|(idx, _)| idx),
                        shape,
                    },
                );
                match item {
                    Some((_, item_id)) => Resolution::Item(item_id),
                    None => Resolution::Local(use_id),
                }
            } else {
                Resolution::Local(use_id)
            };
            // The case entry is **pattern-namespace-only** — an active-pattern case
            // is not a value in expression position (`let v = Even` is FS0039). Mark
            // it so `latest_entry` (expression lookup) skips it regardless of whether
            // it is `Item`- (module-level, Stage 3a) or `Local`-backed (anonymous
            // root / a local recognizer); `case_reference` (pattern position) still
            // finds it. Without this an `Item`-backed case would leak into
            // expression lookup and shadow a same-named ordinary value.
            let mut entry = ScopeEntry::binding(case_name, case_res, self.open_generation);
            entry.pattern_only = true;
            entries.push(entry);
        }
        entries
    }

    /// Resolve the type *uses* inside one type definition's right-hand side: an
    /// abbreviation's target, a record's field types, or a union's case field
    /// types. An enum repr holds only constant cases (no type uses), and an
    /// object-model body's member signatures / inheritance are a later slice.
    pub(super) fn resolve_type_defn(&mut self, defn: &TypeDefn) {
        match defn.repr() {
            Some(TypeDefnRepr::Abbrev(a)) => {
                if let Some(ty) = a.ty() {
                    // FCS's `TyconCoreAbbrevThatIsReallyAUnion` (`CheckDeclarations.fs`):
                    // `type X = X` is not an abbreviation but a single-case *union*
                    // whose case is `X` — the `id.idText = unionCaseName.idText`
                    // branch. Our parser models it as an abbreviation, so resolving
                    // the RHS as a type reference records it against the type being
                    // defined and commits `Type` where FCS reports a `UnionCase`.
                    // Decline the RHS — say nothing — to keep the classifier's
                    // certain-implies-agree contract. (The rule's other branch, an RHS
                    // naming no type in scope, already declines: `resolve_type` records
                    // nothing for an unresolved single-segment ident.)
                    //
                    // FCS's rule is gated on `not hasMeasureAttr`, so a genuine
                    // `[<Measure>] type X = X` is a measure abbreviation whose RHS
                    // *does* resolve as the type — but whether the `[<Measure>]`
                    // attribute is the real FSharp.Core one or a same-named user type
                    // shadowing it (which leaves the RHS a union case, FCS-verified)
                    // is an attribute-identity question this resolution-only pass
                    // cannot settle. So we decline unconditionally: sound either way,
                    // at the cost of a (rare) missed resolution on a genuine measure
                    // self-abbreviation. See `resolve_types.rs`'s regressions.
                    let self_union = super::abbrev_target_single_ident(&ty)
                        .zip(defn.long_id().and_then(super::single_ident))
                        .is_some_and(|(rhs, name)| id_text(rhs.text()) == id_text(name.text()));
                    if !self_union {
                        self.resolve_type(&ty);
                    }
                }
            }
            Some(TypeDefnRepr::Record(r)) => {
                for field in r.fields() {
                    if let Some(ty) = field.ty() {
                        self.resolve_type(&ty);
                    }
                }
            }
            Some(TypeDefnRepr::Union(u)) => {
                for case in u.cases() {
                    for field in case.fields() {
                        if let Some(ty) = field.ty() {
                            self.resolve_type(&ty);
                        }
                    }
                    // The `FullType` signature form (`| Some : Value:'T -> 'T
                    // option`) carries its type names in the signature type
                    // instead of `of`-fields; resolve it too.
                    if let Some(ty) = case.full_type() {
                        self.resolve_type(&ty);
                    }
                }
            }
            // A delegate's signature (`delegate of int -> MyType`) is a real
            // type use, like an abbreviation's target — resolve it.
            Some(TypeDefnRepr::Delegate(d)) => {
                if let Some(ty) = d.ty() {
                    self.resolve_type(&ty);
                }
            }
            // An enum's cases are literal constants (`A = 0`); an object model's
            // member signatures / inheritance carry type uses but are a later
            // slice; a bodyless type (`type Foo`) has no right-hand side; an
            // inline-IL body (`type byref<'T> = (# "!0&" #)`) is a bare IL
            // instruction string with no resolvable type names.
            Some(TypeDefnRepr::Enum(_))
            | Some(TypeDefnRepr::ObjectModel(_))
            | Some(TypeDefnRepr::InlineIl(_))
            | None => {}
        }
    }

    /// Resolve the type-name uses inside a [`Type`], recursing structurally
    /// through the compound forms (`A list`, `A -> B`, `A * B`, `A[]`, `(A)`,
    /// `#A`, `A | null`). A leaf [`Type::LongIdent`] is resolved against the
    /// in-file type table ([`Self::resolve_type_path`]); anything not an in-file
    /// type — a referenced-assembly type, a cross-file or multi-segment path, a
    /// type variable, or an exotic type form — is left unrecorded, a sound
    /// deferral rather than a wrong answer.
    pub(super) fn resolve_type(&mut self, ty: &Type) {
        match ty {
            Type::LongIdent(t) => {
                if let Some(li) = t.long_ident() {
                    let segs: Vec<SyntaxToken> = li.idents().collect();
                    self.resolve_type_path(&segs, 0);
                }
            }
            Type::App(t) => {
                // The application's generic arity belongs to its *head* (the type
                // constructor `Foo` in `Foo<int>` / `int Foo`), so a generic
                // referenced type is keyed correctly — `Pair<int>` is ``Pair`1``,
                // not `Pair`. The arguments resolve as their own (arity-0) types.
                let args = t.type_args();
                let arity = args.len();
                if let Some(head) = t.type_name() {
                    self.resolve_type_head(&head, arity);
                }
                for arg in &args {
                    self.resolve_type(arg);
                }
            }
            Type::LongIdentApp(t) => {
                // `root.Path<args>` — the applied dotted path is a qualified
                // (member / cross-file) type reference, deferred; the root and
                // the type arguments carry the resolvable uses.
                if let Some(root) = t.root() {
                    self.resolve_type(&root);
                }
                for arg in t.type_args() {
                    self.resolve_type(&arg);
                }
            }
            Type::Tuple(t) => {
                for seg in t.segments() {
                    if let TupleSegment::Type(inner) = seg {
                        self.resolve_type(&inner);
                    }
                }
            }
            Type::Fun(t) => {
                if let Some(arg) = t.arg() {
                    self.resolve_type(&arg);
                }
                if let Some(ret) = t.ret() {
                    self.resolve_type(&ret);
                }
            }
            Type::Array(t) => {
                if let Some(elem) = t.element_type() {
                    self.resolve_type(&elem);
                }
            }
            Type::Paren(t) => {
                if let Some(inner) = t.inner() {
                    self.resolve_type(&inner);
                }
            }
            Type::Hash(t) => {
                if let Some(inner) = t.inner() {
                    self.resolve_type(&inner);
                }
            }
            Type::WithNull(t) => {
                if let Some(inner) = t.inner() {
                    self.resolve_type(&inner);
                }
            }
            Type::Constrained(t) => {
                // `T when 'a :> A` — both the base type and any subtype-constraint
                // target (`A`) are genuine type uses, so resolve both for
                // go-to-def. (A type-definition *header*'s constraints are a
                // separate path that does not flow through `resolve_type`; its own
                // non-resolution of constraint targets is a pre-existing gap, not
                // a model this node copies.)
                if let Some(base) = t.base() {
                    self.resolve_type(&base);
                }
                if let Some(cs) = t.constraints() {
                    for c in cs.constraints() {
                        if let Some(ty) = c.ty() {
                            self.resolve_type(&ty);
                        }
                    }
                }
                // The `'a :> T` shorthand carries its constraint target as the
                // second `Type` child (no `when` group), so resolve it too — `T`
                // is a genuine type use for go-to-def.
                if let Some(sub) = t.subtype() {
                    self.resolve_type(&sub);
                }
            }
            Type::AnonRecd(t) => {
                // `{| F : A; … |}` — the field *labels* name no type, but each
                // field's *type* carries resolvable uses, exactly like a named
                // record's field types.
                for field in t.fields() {
                    if let Some(ty) = field.ty() {
                        self.resolve_type(&ty);
                    }
                }
            }
            Type::SignatureParameter(t) => {
                // `x: A` / `?x: A` in a signature type — the parameter *name* is a
                // label (binds nothing, names no type), but its value type carries
                // resolvable uses, like a record/anon-record field type.
                if let Some(ty) = t.value_type() {
                    self.resolve_type(&ty);
                }
            }
            // No in-file type-name use to resolve (or one whose accessor surface
            // is a later slice): a type variable (`'a`), the wildcard `_`, a
            // constraint intersection (`#A & #B`), a measure power, or a
            // type-provider static argument. Left unrecorded — a sound deferral.
            Type::Var(_)
            | Type::Anon(_)
            | Type::Intersection(_)
            | Type::MeasurePower(_)
            | Type::StaticConst(_)
            | Type::StaticConstExpr(_)
            | Type::StaticConstNamed(_)
            | Type::StaticConstNull(_) => {}
        }
    }

    /// Resolve the *head* of a type application (`Foo` in `Foo<int>` / `int Foo`)
    /// carrying the application's generic `arity`. A long-ident head resolves as
    /// a type path keyed by that arity; any other head shape (a nested
    /// application, a parenthesised type) recurses structurally — the arity
    /// belongs to that inner node, not this one.
    pub(super) fn resolve_type_head(&mut self, ty: &Type, arity: usize) {
        match ty {
            Type::LongIdent(t) => {
                if let Some(li) = t.long_ident() {
                    let segs: Vec<SyntaxToken> = li.idents().collect();
                    self.resolve_type_path(&segs, arity);
                }
            }
            other => self.resolve_type(other),
        }
    }

    /// Resolve a type-syntactic dotted path carrying its generic `arity` (the
    /// number of type arguments written after the name; `0` for a non-applied
    /// name).
    ///
    /// A single segment naming an in-file `type` definition records a
    /// [`Resolution::Local`] — the in-file def shadows any same-named assembly
    /// type. Otherwise the path may name a **referenced-assembly type**, either
    /// fully-qualified (`Demo.Thing`) or shortened by a namespace `open`
    /// (`open Demo; Thing`): resolved arity-aware against the [`AssemblyEnv`] —
    /// the type-position counterpart of how [`Self::resolve_long_ident`] resolves
    /// a value/member path — recording [`Resolution::Entity`] at the rooting (and
    /// each nested) type segment.
    ///
    /// This covers the **assembly-only envelope**, where the `AssemblyEnv` is the
    /// complete type scope. Resolution follows F#'s name-lookup precedence — the
    /// shared walk of [`Self::resolve_assembly_path_tiered`]:
    ///
    /// 1. **opens** (`imports`, explicit + implicit `Microsoft.FSharp.*`,
    ///    relative-`open`s' readings computed by [`Self::open_interpretations`]) —
    ///    an `open` shadows the enclosing namespace and the root, latest-open-wins.
    /// 2. **the current enclosing namespace** (`namespace A.B.C` sees
    ///    `A.B.C` only; ancestor namespaces are not searched).
    /// 3. **root / as-written** (the fully-qualified reading).
    ///
    /// It is sound but deliberately incomplete (records nothing, a sound deferral
    /// D5) when an `open` could bring an *unmodelled type* into scope that might
    /// shadow the name ([`Self::unmodelled_open_active`] — an opened assembly module
    /// / `open type`; [`Self::opaque_dotted_open`] — an opened project module), or
    /// the path is project-shadowed. The `resolve_assembly_diff.rs` completeness
    /// property measures the remaining gap.
    pub(super) fn resolve_type_path(&mut self, segs: &[SyntaxToken], arity: usize) {
        let names: Vec<String> = segs.iter().map(|t| id_text(t.text()).to_string()).collect();
        match self.decide_type_path(&names, arity) {
            // A single-segment in-file `type` shadows any assembly type of that
            // name — record the local, exactly as
            // [`Self::resolve_in_file_type_path`] does for an augmentation head.
            TypePathResolution::InFileType(id) => {
                self.record(segs[0].text_range(), Resolution::Local(id));
            }
            // The assembly tiers resolved — map each index-keyed record back to
            // its source token and apply (the leaf is the attribute
            // resolution's currency, not the shell's).
            TypePathResolution::Assembly { idx_recs, .. } => {
                for (idx, res) in idx_recs {
                    self.record(segs[idx].text_range(), res);
                }
            }
            // A shadow is possible but unpinnable. A single-segment name records
            // the shadowable marker so a primitive-alias consumer knows not to
            // type it; a multi-segment path records nothing (never a primitive
            // alias) — [`Self::defer_shadowable_type`] makes that distinction.
            TypePathResolution::Deferred => self.defer_shadowable_type(segs),
            // Genuine no-match: nothing in our model resolves *or shadows* this
            // name at any priority. Record nothing — the signal a consumer
            // reads as "no shadow possible" (see [`Self::defer_shadowable_type`]).
            TypePathResolution::NoMatch => {}
        }
    }

    /// The **token-free decision core** behind [`Self::resolve_type_path`]:
    /// resolve the type-syntactic dotted path `names` (`idText`-normalised
    /// segments) carrying its generic `arity`, computing *what* each segment
    /// resolves to without recording anything. The recording shell above turns
    /// the verdict into [`Resolution`] records at the source tokens; keeping the
    /// decision path-based is what lets a candidate with no source token — the
    /// synthesised `…Attribute` attribute candidate
    /// (`docs/extension-scope-enumeration-plan.md` §2(d)) — resolve through the
    /// *same* precedence walk and shadow guards as every written type path.
    ///
    /// The precedence is F#'s type-name lookup, in order:
    ///
    /// 1. a same-file `[<AutoOpen>]` module's opened type (positional contest);
    /// 2. a single-segment in-file `type` (shadows any assembly type);
    /// 3. a forward-declared `rec`-block project type / nested module (defer);
    /// 4. an opaque / unmodelled `open` (could supply a shadowing type — defer);
    /// 5. a descent into a project nested module (defer);
    /// 6. the shared assembly precedence walk
    ///    ([`Self::resolve_assembly_path_tiered`]: opens → enclosing namespace →
    ///    root) with the per-tier [`ShadowVeto`].
    ///
    /// Steps 1/3/4/5 are the pre-tier shadow guards; a
    /// [`TypePathResolution::Deferred`] verdict from any of them (or a
    /// [`TieredResolution::ShadowDeferred`] from the tiered walk) reaches the
    /// shell as exactly the old `defer_shadowable_type` behaviour.
    pub(super) fn decide_type_path(&self, names: &[String], arity: usize) -> TypePathResolution {
        // A same-file `[<AutoOpen>]` module has opened a type of this name
        // into the current scope ([`Self::auto_open_type_shadow_names`]).
        // F#'s in-scope introductions contest positionally (latest wins), and
        // we model the in-file half of that contest exactly: a same-container
        // `type` declared AFTER the import outranks it and resolves below as
        // usual; anything else — the import outranking an earlier in-file
        // type, or contesting the open/assembly tiers (where a later `open`'s
        // same-named type would also win in FCS, a rarer collision we
        // deliberately over-defer rather than thread positions through the
        // tiered walk) — defers as shadowable. Name-keyed: only the names the
        // module actually declares pay this.
        if let [only] = names
            && let Some(shadow) = self.auto_open_type_shadow_names.get(only.as_str())
        {
            let import_pos = shadow.import_pos;
            let later_in_file_type = self
                .lookup_type_def(only)
                .is_some_and(|id| u32::from(self.defs[id.index()].range.start()) > import_pos);
            if !later_in_file_type {
                return TypePathResolution::Deferred;
            }
        }

        // A single-segment in-file `type` shadows any assembly type of that name
        // (arity-agnostic, as F# in-file type lookup is).
        if let [only] = names
            && let Some(id) = self.lookup_type_def(only)
        {
            return TypePathResolution::InFileType(id);
        }

        // Inside a `rec` block a forward-declared project type may not be in
        // `type_defs` yet, but it still outranks any same-named assembly
        // type — unlike [`Self::unmodelled_type_shadow_at`] below, a tiered
        // match here could be the *wrong* target, not just a missed one, so
        // this must defer unconditionally rather than participate in the
        // tiered walk's priority ordering.
        if names.len() == 1 && self.recursive_module_active {
            return TypePathResolution::Deferred;
        }

        // The multi-segment counterpart: a path whose head names a module
        // declared ANYWHERE in the enclosing `rec` block may descend into a
        // forward-declared nested module — the same wrong-target class
        // [`Self::type_path_descends_into_nested_module`] vetoes, invisible
        // to it here because the source-ordered walk has not reached the
        // module yet.
        if names.len() > 1
            && self.recursive_module_active
            && self.rec_module_names.contains(&names[0])
        {
            return TypePathResolution::Deferred;
        }

        // An opaque / unmodelled open could supply a *type* we do not model that
        // shadows this name — defer rather than risk a wrong target (D5). All three
        // opaque-open flags matter for *types*: `unmodelled_open_active` (an opened
        // assembly module / `open type` whose nested types we don't enumerate),
        // `opaque_dotted_open` (an opened project module whose submodules/types we
        // don't model), and `opaque_value_open` — which is *not* always implied by
        // `opaque_dotted_open` (the `open_imports_project_values` fallback sets only
        // the former), yet an opened project module with unenumerable contents can
        // still supply a shadowing type.
        if self.unmodelled_open_active || self.opaque_dotted_open || self.opaque_value_open {
            return TypePathResolution::Deferred;
        }

        // A path that descends **into a project nested module** (`Sub.Calc` where
        // `Sub` is a nested `module`) must defer: F# binds the nested module's own
        // (unmodelled) `type Calc` when it has one, and falls through to a
        // same-path assembly type only when it does not — and we cannot tell which,
        // so binding the assembly `Demo.Sub.Calc` would be a wrong target. This is
        // the type-position counterpart of the value path's as-written shadow veto.
        // A *top-level* module is deliberately **not** vetoed: it merges with the
        // assembly namespace (F# falls through), so the type resolves there.
        if self.type_path_descends_into_nested_module(names) {
            return TypePathResolution::Deferred;
        }

        // Resolve through the shared precedence walk (opens → enclosing namespace
        // → root), with the arity-aware token-free type record-generator as the
        // leaf. A project *module* sharing the as-written name does not veto the
        // opens tier (a module is not a type), so `as_written_vetoes_opens` is
        // false here. For a single-segment name, the per-tier [`ShadowVeto`]
        // verdict also participates in that *same* walk (V1/V3), at the two
        // strengths its variants document: [`ShadowVeto::Preemptive`] for an
        // in-scope auto-open module's accessible nested type/module named
        // `name` (exact metadata — outranks even a same-tier real match,
        // FCS-probe-confirmed: review round 6 on
        // `docs/completed/r2-annotation-typing-plan.md`), and [`ShadowVeto::OnNoMatch`]
        // for the coarse, name-blind risks (project auto-open modules and
        // unknowable-abbreviation namespaces) that must not pre-emptively
        // defer every other real type under the same reading (rounds 2/3 of
        // the same review).
        let only_name = match names {
            [only] => Some(only.as_str()),
            _ => None,
        };
        match self.resolve_assembly_path_tiered(
            |prefix| self.assembly_type_path_core(prefix, names, arity),
            false,
            |prefix| match only_name {
                // The exact, name-keyed check first — it is the stronger
                // verdict, and where it fires the coarse one is subsumed.
                Some(name)
                    if self
                        .assemblies
                        .auto_open_modules_in_namespace_shadow_type_named(prefix, name) =>
                {
                    ShadowVeto::Preemptive
                }
                Some(_) if self.unmodelled_type_shadow_at(prefix) => ShadowVeto::OnNoMatch,
                _ => ShadowVeto::None,
            },
        ) {
            TieredResolution::Resolved(reading) => TypePathResolution::Assembly {
                idx_recs: reading.idx_recs,
                leaf: reading.leaf,
            },
            // A project entity shadows the name at winning priority, or an
            // unmodelled type shadow won the walk at a higher-or-equal
            // priority than any real match.
            TieredResolution::ShadowDeferred => TypePathResolution::Deferred,
            // Genuine no-match: nothing in our model resolves *or shadows* this
            // name at any priority.
            TieredResolution::NoMatch => TypePathResolution::NoMatch,
        }
    }

    /// EX-3 §2(d) (`docs/extension-scope-enumeration-plan.md`): resolve the
    /// *type* each attribute in `lists` names, at the scope currently in
    /// force, recording the verdict at the written name's range into
    /// [`Self::attribute_resolutions`]. FCS resolves an attribute by trying
    /// two candidates through the general type resolver
    /// (`ResolveAttributeType`): the written path with `Attribute` suffixed
    /// onto the last segment **first**, then the path as written — so `[<Literal>]`
    /// is `LiteralAttribute`, and a `type MyExt = ExtensionAttribute` alias is
    /// found wherever the written name resolves. Each candidate goes through
    /// [`Self::decide_type_path`], inheriting every precedence tier and shadow
    /// guard a written type path gets.
    pub(super) fn resolve_attribute_lists(
        &mut self,
        lists: impl Iterator<Item = borzoi_cst::syntax::AttributeList>,
    ) {
        for list in lists {
            for attr in list.attributes() {
                if let Some(type_name) = attr.type_name() {
                    self.resolve_attribute_type(&type_name);
                } else {
                    // No name node at all: the gate cannot key this attribute
                    // and must keep the presence defer for the file.
                    self.attribute_shape_unknowable = true;
                }
            }
        }
    }

    /// Every [`AttributeList`](borzoi_cst::syntax::AttributeList) under `node`,
    /// resolved at the current scope — the per-declaration entry point into
    /// [`Self::resolve_attribute_lists`], reaching member-, parameter-, and
    /// case-level attributes as well as the declaration's own. The caller must
    /// not pass a node containing a nested module (its body's attributes
    /// resolve at the *nested* scope, when the walk reaches them).
    pub(super) fn resolve_attributes_under(&mut self, node: &borzoi_cst::syntax::SyntaxNode) {
        let lists: Vec<borzoi_cst::syntax::AttributeList> = node
            .descendants()
            .filter_map(borzoi_cst::syntax::AttributeList::cast)
            .collect();
        self.resolve_attribute_lists(lists.into_iter());
    }

    /// Resolve one written attribute name (see
    /// [`Self::resolve_attribute_lists`]) and record the verdict:
    ///
    /// - a candidate resolving to an in-file `type` commits
    ///   [`Resolution::Local`] (FCS binds the project type; its abbreviation
    ///   target, if any, is a *later* question for the consumer);
    /// - a candidate resolving through the assembly tiers to a whole-path leaf
    ///   commits [`Resolution::Entity`];
    /// - a candidate the walk defers — or one a project type declared in this
    ///   file or a preceding one could satisfy invisibly
    ///   ([`Self::project_type_named`]), or a `global.`-rooted path (a
    ///   root-anchored walk this decision core does not model) — records
    ///   [`Resolution::Deferred`]: no claim;
    /// - both candidates missing everywhere records **nothing** (FCS errors
    ///   and sinks no resolution — absence agrees with absence).
    ///
    /// The suffixed candidate is tried first, as FCS does; its clean no-match
    /// falls through to the written candidate, but its *deferral* does not — a
    /// shadow that could redirect the suffixed candidate would win in FCS over
    /// anything the written one resolves to.
    fn resolve_attribute_type(&mut self, type_name: &borzoi_cst::syntax::LongIdent) {
        let toks: Vec<SyntaxToken> = type_name.idents().collect();
        let (Some(first), Some(last)) = (toks.first(), toks.last()) else {
            // A name node with no ident tokens: unkeyable, like the nameless
            // case — the gate keeps the presence defer.
            self.attribute_shape_unknowable = true;
            return;
        };
        // FCS records the attribute-type use at the written path's range
        // (`rangeOfLid`; the suffixed candidate reuses the written ident's
        // range), so key the verdict the same way.
        let range = TextRange::new(first.text_range().start(), last.text_range().end());

        // A **multi-segment** candidate defers wholesale, as does a
        // `global.`-rooted one (checked on the RAW token text — a quoted
        // ``global`` is an ordinary segment, not the root marker). Qualified
        // attribute paths are rare in real code, and committing them soundly
        // means every qualifier segment participating in every shadow guard
        // (a project abbreviation supplying the head redirects the whole
        // path; a `global.` root anchors a different walk) — the same
        // per-segment enumeration the abandoned classifiers drowned in.
        // Deferral deletes that class; the single-segment path below is where
        // the gate's value lives (review on stage 4).
        if toks.len() > 1 || first.text() == "global" {
            self.attribute_resolutions
                .insert(range, Resolution::Deferred(DeferredReason::ShadowableType));
            return;
        }

        let names: Vec<String> = toks.iter().map(|t| id_text(t.text()).to_string()).collect();
        let mut suffixed = names.clone();
        *suffixed.last_mut().expect("split checked") = format!("{}Attribute", id_text(last.text()));

        let verdict = match self.attribute_candidate(&suffixed) {
            AttrCandidate::Resolved(res) => Some(res),
            AttrCandidate::Deferred => Some(Resolution::Deferred(DeferredReason::ShadowableType)),
            AttrCandidate::NoMatch => match self.attribute_candidate(&names) {
                AttrCandidate::Resolved(res) => Some(res),
                AttrCandidate::Deferred => {
                    Some(Resolution::Deferred(DeferredReason::ShadowableType))
                }
                AttrCandidate::NoMatch => None,
            },
        };
        if let Some(res) = verdict {
            self.attribute_resolutions.insert(range, res);
        }
    }

    /// One attribute candidate's verdict (see [`Self::resolve_attribute_type`];
    /// the caller defers every multi-segment path, so `names` is a single
    /// segment here — the per-split scans below still walk `prefix ++ names`).
    fn attribute_candidate(&self, names: &[String]) -> AttrCandidate {
        // A **project `[<AutoOpen>]` module** in scope-history — any block of
        // this file (FCS persists a same-named later block's auto-open where
        // the block-scoped shadow set is cleared), or a preceding
        // Compile-order file — needs no presence defer of its own (AO-2):
        // everything it can do to this lookup is guarded name-keyed already.
        // *Supplying* the candidate: every type and exception it declares, at
        // any depth, any block, is in the file-global §2(d) pre-scan and
        // threads cross-file as `project_type_simple_names`, so
        // [`Self::project_type_named`] defers those candidates below.
        // *Contesting* an in-file hit: an own-file auto-open's declared names
        // defer through the file-global, position-blind
        // [`Resolver::own_auto_open_type_names`](super::state::Resolver) set
        // in the in-file arm (the block-scoped
        // [`Self::auto_open_type_shadow_names`] guard alone misses the
        // three-block straddle — codex on AO-2), an auto-open `exception`
        // through the file-global exception guard there, and a preceding
        // *file's* import position is always earlier than any current-file
        // definition, so an in-file hit is FCS's winner against those.
        // Probed and pinned by the `*auto_open*` cases in
        // `attr_resolution_diff`.
        match self.decide_type_path(names, 0) {
            // An in-file `type` wins over everything the tiers could offer
            // (and over the guards below — the in-file def *is* the project
            // type that would otherwise force a defer) — *unless* an `open`
            // sits later in the source than the definition: F# is latest-wins
            // across bindings and opens alike, so `type ObsoleteAttribute …;
            // open System; [<Obsolete>]` binds `System.ObsoleteAttribute` in
            // FCS (codex round 6). The contest is positional and name-blind
            // (any later open defers) — opens conventionally precede type
            // definitions, so the common in-file shapes still commit.
            TypePathResolution::InFileType(id) => {
                // Ways the in-file hit can be the WRONG local, each a
                // defer (never a re-resolution):
                // - a generic declaration of the name anywhere in the file:
                //   FCS's attribute lookup is arity-0 and skips a generic
                //   local where our in-file lookup is arity-agnostic
                //   (codex round 7);
                // - an `exception` of the name anywhere in the file: absent
                //   from `type_defs`, so the hit may reach past a closer
                //   exception FCS binds (codex on stage 4);
                // - a declaration of the name directly inside an
                //   `[<AutoOpen>]` module anywhere in the file: the import
                //   contests the hit positionally in FCS, and across blocks
                //   the walk's block-scoped shadow guard cannot see it — a
                //   block-1 direct type, block-2 auto-open redeclaration,
                //   block-3 attribute binds the auto-open's type while
                //   `lookup_type_def` retains block 1's (codex on AO-2);
                // - inside a `rec` block, a closer forward-declared type may
                //   not be in `type_defs` yet, so an outer hit may be the
                //   wrong scope (same review) — the walk's own rec guard sits
                //   *after* the in-file step and never fires for a hit;
                // - the positional contest: an `open` later than the def
                //   wins in FCS (codex round 6).
                let unreliable = names.last().is_some_and(|last| {
                    self.own_generic_type_simple_names.contains(last)
                        || self.own_exception_simple_names.contains(last)
                        || self.own_auto_open_type_names.contains(last)
                }) || self.recursive_module_active
                    || self.latest_open_pos > u32::from(self.defs[id.index()].range.start());
                if unreliable {
                    AttrCandidate::Deferred
                } else {
                    AttrCandidate::Resolved(Resolution::Local(id))
                }
            }
            _ if names
                .last()
                .is_some_and(|last| self.project_type_named(last)) =>
            {
                // A project type — declared later in this file, in a sibling
                // block, or in a preceding Compile-order file — could satisfy
                // this candidate invisibly: the tiered walk indexes none of
                // those, so both its match and its no-match are untrustworthy
                // here. No claim. (Checked on the last segment for *every*
                // candidate shape — a qualified project alias shadows the same
                // way a bare one does.)
                AttrCandidate::Deferred
            }
            _ if self.attribute_candidate_unrulable(names) => AttrCandidate::Deferred,
            // A module-shaped leaf is not an attribute type: FCS does not bind
            // a module in attribute position (probed — `[<M>]` with a module
            // `MAttribute` falls through to a written type `M`). We do not
            // model that fallthrough's interaction with the walk's precedence,
            // so a module leaf declines rather than committing a wrong entity
            // (codex round 4).
            TypePathResolution::Assembly { leaf: Some(h), .. } if self.assemblies.is_module(h) => {
                AttrCandidate::Deferred
            }
            TypePathResolution::Assembly { leaf: Some(h), .. } => {
                AttrCandidate::Resolved(Resolution::Entity(h))
            }
            // A partial reading: the candidate names a rooting type but not
            // the whole path. FCS would fail this candidate and try the next,
            // but our partial evidence is not that proof — the unmodelled tail
            // could resolve for FCS. No claim.
            TypePathResolution::Assembly { leaf: None, .. } => AttrCandidate::Deferred,
            TypePathResolution::Deferred => AttrCandidate::Deferred,
            TypePathResolution::NoMatch => AttrCandidate::NoMatch,
        }
    }

    /// The attribute path's uncertainty scan: whether something we cannot
    /// enumerate could supply — or shadow — this candidate under **any**
    /// searched reading, making both a tier's match and its no-match
    /// untrustworthy for a *commitment* (deferral-only: a hit here never
    /// resolves anything, it only withholds a claim).
    ///
    /// The tiered walk itself applies the name-keyed [`ShadowVeto`]s only to
    /// *single-segment* names, and consults dropped types not at all (the
    /// assembly env documents that as the caller's check — a dropped type is a
    /// property of a *path*, `any_split_of_a_module_path_has_a_dropped_type`).
    /// So per searched prefix, over the full path `prefix ++ names`:
    ///
    /// - a **dropped type at any split** could be the very type FCS binds
    ///   (or a same-FQN merge partner supplying it);
    /// - an **unknowable-abbreviation namespace at any split** could alias
    ///   the segment looked up there (the walk's `OnNoMatch` veto covers
    ///   exactly the single-segment shape; a qualified candidate's splits are
    ///   not consulted there);
    /// - an **assembly `[<AutoOpen>]` module at a split** could shadow the
    ///   segment supplied *at that split* into its namespace — the leaf, or a
    ///   head that re-roots the whole path (again, the walk's `Preemptive`
    ///   veto is single-segment-keyed);
    /// - an **assembly module merging into a split's namespace** could hold a
    ///   nested type of any candidate segment bare-visible there (FCS merges
    ///   `module N` with `namespace N`), invisible to the top-level type
    ///   index;
    /// - a **contested type key at any split**: `lookup_type`'s index is
    ///   first-wins per `(namespace, name)`, but FCS merges same-FQN types
    ///   across references latest-wins, so a key with two public entities —
    ///   or whose slot answer is not the single public one (an
    ///   internal-first/public-second collision) — can misreport both a match
    ///   and a miss (codex on this stage; doom-loop round 4). The complete
    ///   [`AssemblyEnv`](crate::AssemblyEnv)`::public_entities_named` scan is
    ///   the check the first-wins slot cannot be.
    fn attribute_candidate_unrulable(&self, names: &[String]) -> bool {
        if names.is_empty() {
            return true;
        }
        // A retained manifest auto-open surface (a contested or module-shaped
        // assembly-level `[<AutoOpen>]` target) never appears in the prefix
        // walk below at all, yet FCS opens it at higher priority than any
        // modeled tier — any candidate segment it could supply (the leaf
        // directly, or a head the path would root at) defers the candidate.
        if names.iter().any(|seg| {
            self.assemblies
                .retained_auto_open_could_supply_entity_named(seg)
        }) {
            return true;
        }
        self.assembly_prefixes_by_priority().any(|prefix| {
            let mut full = prefix.to_vec();
            full.extend(names.iter().cloned());
            self.assemblies
                .any_split_of_a_module_path_has_a_dropped_type(&full)
                || (prefix.len()..full.len()).any(|k| {
                    let ns = &full[..k];
                    // The auto-open shadow is asked about the segment supplied
                    // AT this split — the name the lookup would root or extend
                    // with there — not the candidate's leaf: an auto-open
                    // module supplying the *head* of `[<A.B>]` re-roots the
                    // whole path at higher priority (codex round 3).
                    self.assemblies.unknowable_abbreviations_in_namespace(ns)
                        || self
                            .assemblies
                            .auto_open_modules_in_namespace_shadow_type_named(ns, &full[k])
                        || !self.opened_assembly_modules(ns).is_empty()
                        || {
                            // Occupancy is accessibility-blind: FCS resolves an
                            // *inaccessible* suffixed candidate (then errors)
                            // rather than falling through to the written one,
                            // so an internal occupant is not a clean miss
                            // (codex round 4). Trustworthy only as exactly one
                            // public entity that is also what the first-wins
                            // slot answers.
                            let occupants = self.assemblies.entities_named(ns, &full[k]);
                            match occupants.as_slice() {
                                [] => false,
                                [only] => {
                                    !self.assemblies.is_public(*only)
                                        || self.assemblies.lookup_type(ns, &full[k], 0)
                                            != Some(*only)
                                }
                                _ => true,
                            }
                        }
                })
        })
    }

    /// Whether any project **type** — declared anywhere in this file
    /// ([`Resolver::own_type_simple_names`]) or in a preceding Compile-order
    /// file ([`ProjectItems::project_type_simple_names`](super::model::ProjectItems))
    /// — has the simple name `name`. The attribute resolution's project-type
    /// guard; see [`Self::attribute_candidate`].
    fn project_type_named(&self, name: &str) -> bool {
        self.own_type_simple_names.contains(name)
            || self.preceding.project_type_simple_names.contains(name)
    }

    /// Record that a *single-segment* type-position name is deferred because a
    /// shadow is **possible** but unpinnable — an opaque/unmodelled `open`, a
    /// project shadow, or an ambiguous multi-open match. This distinguishes
    /// "maybe shadowed" from a name that genuinely resolves to nothing (the
    /// caller's no-match fall-through, which records nothing); a consumer
    /// (inference, R2) reads the *absence* of a record as "no shadow possible"
    /// and only then types a primitive-alias annotation. Only single-segment
    /// names are marked — a dotted path's tail is never a primitive alias, and
    /// nothing reads a multi-segment marker. See [`DeferredReason::ShadowableType`].
    fn defer_shadowable_type(&mut self, segs: &[SyntaxToken]) {
        if let [only] = segs {
            self.record(
                only.text_range(),
                Resolution::Deferred(DeferredReason::ShadowableType),
            );
        }
    }

    /// Whether `prefix` — an opened, enclosing-namespace, or root reading, in
    /// [`Self::assembly_prefixes_by_priority`] order — carries a *coarse,
    /// name-blind* unmodelled type shadow risk: a **project** `[<AutoOpen>]`
    /// module (sema does not enumerate its nested types at all) or a
    /// namespace declared into by an assembly whose abbreviations are
    /// [unknowable](borzoi_sema::AbbreviationVisibility) — its signature
    /// pickle failed to decode, so its metadata-invisible abbreviations (V3)
    /// could hold *any* name. Neither can be checked pre-emptively without
    /// over-deferring every other real type under the same reading; only
    /// consulted once the tier's own lookup is a genuine
    /// [`TieredResolution::NoMatch`]. Two channels are deliberately excluded
    /// here because they have exact, name-keyed representations instead: the
    /// **assembly**-side auto-open channel (exact metadata, checked
    /// *precisely and pre-emptively* — the [`ShadowVeto::Preemptive`] verdict
    /// of [`Self::resolve_assembly_path_tiered`]'s caller here), and a
    /// *decodable* assembly's abbreviations (synthesised
    /// `EntityKind::Abbreviation` markers in the entity tree, matched by the
    /// tier's own lookup like any type and shadow-deferred there).
    ///
    /// A pure per-namespace query — no resolver state to keep in sync with
    /// `imports` — so [`Self::resolve_assembly_path_tiered`] can call it at
    /// the same priority position it tries a real match, letting a
    /// higher-priority shadow risk win over a lower-priority real type of the
    /// same name.
    ///
    /// `prefix` may be the empty ROOT reading: an assembly can declare
    /// `namespace global` content (empty `namespace` path), and FCS lets a
    /// bare, unopened name bind to it, so the root tier needs the same check
    /// as every opened/enclosing one — found by review against
    /// `docs/completed/r2-annotation-typing-plan.md`.
    fn unmodelled_type_shadow_at(&self, prefix: &[String]) -> bool {
        self.project_auto_open_module_in_namespace(prefix)
            || self
                .assemblies
                .unknowable_abbreviations_in_namespace(prefix)
    }

    /// Whether `names` **strictly descends into** a project *nested* module (a path
    /// *under* it, `Sub.Calc` where `Sub` is a nested `module` — not the module's
    /// own name) — whose unmodelled types could shadow a same-path assembly type,
    /// so a type reference through it must defer (D5). Covers this file's nested
    /// modules (relative [`Self::nested_module_locals`] and qualified
    /// [`Self::nested_module_exports`] forms) and earlier Compile-order ones
    /// ([`ProjectItems::is_rooted_at_nested_module`]).
    ///
    /// Two cases are deliberately **excluded** (both resolve to the assembly type):
    /// a **top-level** project module (it merges with the assembly namespace), and
    /// a nested module's **own name** used as a type (`(x: Calc)` where `Calc` is a
    /// nested module — a module is not a type, FCS resolves the opened assembly
    /// `Demo.Calc`). Hence the *strict* prefix (`names.len() > p.len()`); the
    /// conservative defer is for proper descents like `Calc.Inner`.
    fn type_path_descends_into_nested_module(&self, names: &[String]) -> bool {
        let strictly_under = |p: &Vec<String>| {
            !p.is_empty() && names.len() > p.len() && names.starts_with(p.as_slice())
        };
        self.nested_module_locals.iter().any(strictly_under)
            || self.nested_module_exports.iter().any(strictly_under)
            || (self.preceding.is_rooted_at_nested_module(names)
                && !self.preceding.is_exact_nested_module(names))
    }

    /// In-file-only type-path resolution: record a [`Resolution::Local`] when a
    /// single segment names an in-file `type` def (arity-agnostic, as F# in-file
    /// type lookup is). Returns whether it recorded. Used both as the first step
    /// of [`Self::resolve_type_path`] and on its own for an augmentation head,
    /// where an assembly lookup would be unsound — the augmented type's generic
    /// arity is not on the `long_id` there, so a keyed assembly lookup could pick
    /// the wrong arity (`type Demo.Pair<'T> with …` is ``Pair`1``, not `Pair`).
    pub(super) fn resolve_in_file_type_path(&mut self, segs: &[SyntaxToken]) -> bool {
        if let [only] = segs
            && let Some(id) = self.lookup_type_def(only.text())
        {
            self.record(only.text_range(), Resolution::Local(id));
            return true;
        }
        false
    }

    /// The in-file `type` def named `name` visible from the current container:
    /// the innermost match found by walking the container path outward, since a
    /// nested module sees its enclosing module/namespace's types and a nested
    /// type of the same name shadows the enclosing one. `None` if no in-file type
    /// of that name is in scope. (Cross-file / assembly types are a later slice.)
    pub(super) fn lookup_type_def(&self, name: &str) -> Option<DefId> {
        let name = id_text(name);
        (0..=self.container_path.len()).rev().find_map(|k| {
            self.type_defs
                .get(&self.container_path[..k])
                .and_then(|m| m.get(name))
                .copied()
        })
    }

    /// Resolve the non-binder *references* a pattern mentions — the ones the
    /// [`binders`] walk (correctly) drops — recursively through every structural
    /// sub-pattern: type names in annotations (`x : T`, `:? T`), and the head of
    /// an *applied* constructor pattern (`B n`, `Some x`), which names a value (a
    /// reference, not a binder). A *nullary* head (`Red`) is a provisional binder
    /// instead, resolved in the binders loop via
    /// [`case_reference`](Self::case_reference), so only applied heads are
    /// resolved here. Binders themselves are interned by [`binders`]. A
    /// quotation pattern (`<@ … @>`) additionally carries an *expression* body,
    /// resolved via [`Self::resolve_expr`] against the enclosing scope.
    pub(super) fn resolve_pat_types(&mut self, pat: &Pat) {
        match pat {
            Pat::Typed(p) => {
                if let Some(ty) = p.ty() {
                    self.resolve_type(&ty);
                }
                if let Some(inner) = p.pat() {
                    self.resolve_pat_types(&inner);
                }
            }
            Pat::IsInst(p) => {
                // `:? T` — the tested type is a type use.
                if let Some(ty) = p.ty() {
                    self.resolve_type(&ty);
                }
            }
            Pat::Paren(p) => {
                if let Some(inner) = p.inner() {
                    self.resolve_pat_types(&inner);
                }
            }
            Pat::Attrib(p) => {
                if let Some(inner) = p.pat() {
                    self.resolve_pat_types(&inner);
                }
            }
            Pat::Tuple(p) => {
                for el in p.elements() {
                    self.resolve_pat_types(&el);
                }
            }
            Pat::ArrayOrList(p) => {
                for el in p.elements() {
                    self.resolve_pat_types(&el);
                }
            }
            Pat::Record(p) => {
                for field in p.fields() {
                    if let Some(value) = field.pat() {
                        self.resolve_pat_types(&value);
                    }
                }
            }
            Pat::As(p) => {
                if let Some(lhs) = p.lhs() {
                    self.resolve_pat_types(&lhs);
                }
                if let Some(rhs) = p.rhs() {
                    self.resolve_pat_types(&rhs);
                }
            }
            Pat::ListCons(p) => {
                if let Some(lhs) = p.lhs() {
                    self.resolve_pat_types(&lhs);
                }
                if let Some(rhs) = p.rhs() {
                    self.resolve_pat_types(&rhs);
                }
            }
            Pat::Ands(p) => {
                for operand in p.operands() {
                    self.resolve_pat_types(&operand);
                }
            }
            Pat::Or(p) => {
                if let Some(lhs) = p.lhs() {
                    self.resolve_pat_types(&lhs);
                }
                if let Some(rhs) = p.rhs() {
                    self.resolve_pat_types(&rhs);
                }
            }
            Pat::LongIdent(p) => {
                // A path whose final segment is an *active-pattern name*
                // (`Color.Red.(|Foo|_|)`, FCS's `pathOp` ending in an `opName`)
                // references the active pattern, not a union case — and its name is
                // a sibling `ACTIVE_PAT_NAME`, so the head `LONG_IDENT` holds only
                // the *prefix* idents. The case machinery below reads the last
                // ident segment as the case name, so it would resolve the head span
                // to `Color.Red` — a wrong target. Leave such a head unresolved
                // (qualified active-pattern resolution is a follow-up); the
                // argument patterns below are still walked. An *operator*-terminated
                // path (`Color.Red.(+)`) needs no such guard: its `( op )` tokens
                // stay inside the `LONG_IDENT`, so the final segment is the operator
                // and the case lookup simply finds nothing.
                let head_names_active_pat = p.active_pat_name().is_some();
                // Set when the shape-keyed active-pattern split below has already
                // handled the curried args (parameters as expressions, result as a
                // sub-pattern), so the default "recurse every arg" fallthrough must
                // not run and re-process them through the constructor namespace.
                let mut split_applied = false;
                if let Some(head) = p.head().filter(|_| !head_names_active_pat) {
                    let segs: Vec<SyntaxToken> = head.idents().collect();
                    let applied = p.args().next().is_some() || p.name_pat_pairs().is_some();
                    if segs.len() >= 2 {
                        // A *qualified* head (`Color.Red`, `Lib.Color.Red`,
                        // `Shape.Circle r`) that names a union/enum case: FCS resolves
                        // it like the expression form (the whole head span → the
                        // case), nullary or applied alike. `binders` drops a
                        // multi-segment head, and `case_reference` (single-segment)
                        // does not see it.
                        self.record_qualified_case_pattern(&segs);
                    } else if applied
                        && let [single] = segs.as_slice()
                        && let Some(res) = self.case_reference(single.text())
                    {
                        // An *applied* single-segment head (`B n`, `Some x`) is a
                        // constructor / active-pattern reference that `binders` drops;
                        // resolve it to an in-scope case. A *nullary* single-segment
                        // head (`Red`) is a provisional binder, resolved in the
                        // binders loop, so it is skipped here.
                        self.record(single.text_range(), res);
                        // When the head is an active pattern with a stored shape —
                        // same-file (`Local`) or cross-file opened (`Item`, Stage
                        // 3a) — its curried arguments split into parameters
                        // (expressions) and the result sub-pattern (a binder) — FCS's
                        // `TcPatLongIdentActivePatternCase`. A named-field group
                        // (`Case (field = pat)`) is never an active-pattern applied
                        // head, so restrict to the curried form and leave the group to
                        // the default recursion below.
                        if let Some(shape) = self.resolution_active_pattern_shape(res)
                            && p.name_pat_pairs().is_none()
                        {
                            let args: Vec<Pat> = p.args().collect();
                            split_applied = self.split_active_pattern_args(shape, &args);
                        }
                    }
                }
                if !split_applied {
                    // A constructor / function-head application (`Some (x : T)`,
                    // `f (x : T)`): the head names a value, but the argument patterns
                    // — curried and named-field-group alike — may carry annotations.
                    self.resolve_applied_arg_patterns(p);
                }
            }
            Pat::Quote(q) => {
                // `<@ … @>` in pattern position — a parameterised active-pattern
                // argument (`SynPat.QuoteExpr`). Its body is an *expression*
                // captured from the enclosing scope, exactly like an
                // expression-position quote; resolve its value and type uses
                // there. This runs before the clause/binding binders are pushed
                // (`pattern_locals` → `resolve_pat_types`, then binders), so `q`'s
                // frame is the enclosing one — the clause's own binders are not
                // visible to the parameter expression, matching F# evaluation.
                // Reuses the same path as [`Self::resolve_expr`]'s `Expr::Quote`
                // arm (which descends into the quotation body).
                if let Some(inner) = q.inner() {
                    self.resolve_expr(&inner);
                }
            }
            // Leaves with no nested pattern and no type annotation. `OptionalVal`
            // (`?x`) is a bare binder; a typed optional arg `?x : T` is
            // `Typed(OptionalVal, T)`, so the annotation is resolved by the
            // `Typed` arm, never here.
            Pat::Named(_)
            | Pat::Wildcard(_)
            | Pat::Const(_)
            | Pat::Null(_)
            | Pat::OptionalVal(_) => {}
        }
    }

    /// Recurse into an applied [`Pat::LongIdent`]'s argument patterns — the
    /// curried args (`p.args()`) and the named-field group (`p.name_pat_pairs()`)
    /// — for their type annotations and nested constructor / active-pattern heads.
    /// The default when no shape-keyed split applies (see
    /// [`Self::resolve_pat_types`]) and for a direct `let` function-binding head
    /// (see [`Self::resolve_let_head_pat_types`]).
    fn resolve_applied_arg_patterns(&mut self, p: &LongIdentPat) {
        for arg in p.args() {
            self.resolve_pat_types(&arg);
        }
        if let Some(group) = p.name_pat_pairs() {
            for pair in group.pairs() {
                if let Some(value) = pair.pat() {
                    self.resolve_pat_types(&value);
                }
            }
        }
    }

    /// Resolve the type / reference uses of a **direct `let`-binding head**.
    /// Differs from [`Self::resolve_pat_types`] in exactly one case: an applied
    /// *single-segment* `LongIdent` head here is the **function being defined**
    /// (`let DivBy x = x` defines `DivBy`, a binder), never an active-pattern /
    /// constructor *use* — even when an in-scope active-pattern case shares the
    /// name. So the head must not be resolved as a case reference, and its
    /// arguments must not be split as active-pattern parameters (which would
    /// wrongly exclude the genuine parameter `x`, dropping its binder). This
    /// mirrors [`binders`](crate::binders)' `Ctx::LetHead`: the head binds, and
    /// each argument is an ordinary param pattern, so it recurses through
    /// [`Self::resolve_applied_arg_patterns`] — where a *nested* applied AP use in
    /// a parameter (`let f (Scale x) = …`) still splits correctly, having
    /// descended out of the let-head position.
    ///
    /// Any other head shape — a parenthesised deconstruction (`let (Some x) =
    /// …`), a tuple, a nullary maybe-var, a multi-segment path — is not a
    /// single-segment function-binding head (a `let` deconstruction never resolves
    /// its head as an applied case: the split only fires on a single-segment head
    /// via [`case_reference`](Self::case_reference)), so it resolves exactly as a
    /// pattern through [`Self::resolve_pat_types`].
    pub(super) fn resolve_let_head_pat_types(&mut self, head: &Pat) {
        // A let head is a binding-head position: an active-pattern parameter in it
        // (`let f (DivBy divisor) = …`, or a later curried param referencing an
        // earlier one) must not commit its expression against the enclosing scope
        // before the earlier curried params are visible (see
        // [`Self::decline_binding_head_param_exprs`]). Decline such argument
        // resolutions for the duration; the binder exclusion still runs.
        let saved = std::mem::replace(&mut self.decline_binding_head_param_exprs, true);
        if let Pat::LongIdent(p) = head
            && p.active_pat_name().is_none()
            && (p.args().next().is_some() || p.name_pat_pairs().is_some())
            && p.head().is_some_and(|h| h.idents().take(2).count() == 1)
        {
            // Function-binding form (`let DivBy x = x`, `let f a b = …`): the head
            // is the defined function's name (a binder, interned by the caller's
            // `binders` loop), so record no case reference and apply no split. The
            // arguments are ordinary param patterns.
            self.resolve_applied_arg_patterns(p);
        } else {
            self.resolve_pat_types(head);
        }
        self.decline_binding_head_param_exprs = saved;
    }

    /// Split the curried `args` of an applied *same-file active-pattern* head of
    /// the given `shape` into **parameters** (expressions evaluated in the
    /// enclosing scope) and the **result sub-pattern** (a binder), mirroring FCS's
    /// `TcPatLongIdentActivePatternCase`
    /// (`../fsharp/src/Compiler/Checking/Expressions/CheckExpressions.fs`). Each
    /// parameter argument has its would-be binder ranges excluded (so the
    /// [`binders`](crate::binders) walk does not fabricate a local for it) and is
    /// resolved as an expression ([`Self::resolve_pattern_arg_as_expr`]); the
    /// result sub-pattern is recursed through [`Self::resolve_pat_types`] as usual,
    /// so it binds and a nested applied head re-enters this same logic.
    ///
    /// Returns `true` when it *applied* the split (the caller must then skip its
    /// default arg recursion), `false` when the shape leaves today's behaviour
    /// unchanged — a **multi-case** recognizer (a parameterized multi-case use is
    /// FS0722-illegal, and the only legal applied use binds correctly by default)
    /// or a **partial point-free** one (`arity == None`, no parameter count to
    /// split on). See `docs/parameterized-active-pattern-args-plan.md`.
    /// The [`ActivePatternShape`] of the recognizer an applied pattern head
    /// resolved to, if it is an active pattern with a known shape (Stage 3a of
    /// `docs/export-decl-model-plan.md`):
    ///
    /// - a **same-file** case resolves to [`Resolution::Item`] (or, under an
    ///   anonymous root, [`Resolution::Local`]) — its shape is keyed by the case's
    ///   use-def in [`Self::active_pattern_shape`]; an `Item` is mapped to that def
    ///   through this file's `items`;
    /// - a **cross-file** case resolves to [`Resolution::Item`] whose handle is
    ///   out of this file's range — its shape comes from
    ///   [`ProjectItems::active_pattern_shape_of`].
    ///
    /// `None` for any other resolution — an ordinary value, a union/exception case,
    /// a referenced-assembly tag, a deferred/qualified head — which keeps today's
    /// fabricate-a-binder behaviour (the unknown-shape decline is Stage 3c).
    fn resolution_active_pattern_shape(&self, res: Resolution) -> Option<ActivePatternShape> {
        match res {
            Resolution::Local(id) => self.active_pattern_shape.get(&id).copied(),
            Resolution::Item(id) => match id.index().checked_sub(self.item_base as usize) {
                // Same-file: map the handle to its defining use-def, then the shape.
                Some(local) => self
                    .items
                    .get(local)
                    .and_then(|it| self.active_pattern_shape.get(&it.def).copied()),
                // Cross-file: the shape crosses the boundary in the side map.
                None => self.preceding.active_pattern_shape_of(id),
            },
            _ => None,
        }
    }

    fn split_active_pattern_args(&mut self, shape: ActivePatternShape, args: &[Pat]) -> bool {
        // An applied head always has ≥ 1 arg; guard the degenerate case anyway.
        if args.is_empty() {
            return false;
        }
        // Multi-case → no split (unchanged behaviour).
        if !shape.single_case {
            return false;
        }
        let k = args.len();
        // `param_count` leading args are parameters; when `has_result`, `args[param_count]`
        // is the result sub-pattern; `args[surplus_start..]` are recursed as today.
        let (param_count, has_result, surplus_start) = if shape.total {
            // Total single-case: `frontAndBack` — the last arg is ALWAYS the
            // result, everything before it a parameter. Arity is never consulted
            // (`Scale g` binds `g`; robust to eta-reduced definitions).
            (k - 1, true, k)
        } else {
            // Partial single-case: the split needs the parameter count. A
            // point-free recognizer (`arity == None`) has none visible, so decline
            // the split (unchanged behaviour).
            let Some(p) = shape.arity else {
                return false;
            };
            if k == p {
                // Exactly the parameters, no result (the payload type is unit, or
                // an unsolved typar that could be unit).
                (k, false, k)
            } else if k == p + 1 {
                // Parameters, then the result binder.
                (p, true, k)
            } else if k < p {
                // FS3868-illegal: treat every present arg as a parameter, never
                // fabricate a binder. Sound on clean code (which cannot reach here),
                // conservative on broken code.
                (k, false, k)
            } else {
                // `k > p + 1` — FS3868 unless the definition is eta-reduced (the
                // true `p` is larger). `args[0..p]` are parameters under any true
                // `p ≥ p_syn`; recurse the surplus as today (binding there is no
                // worse than the status quo).
                (p, false, p)
            }
        };
        for arg in &args[..param_count] {
            self.exclude_param_binders(arg);
            self.resolve_pattern_arg_as_expr(arg);
        }
        if has_result {
            self.resolve_pat_types(&args[param_count]);
        }
        for arg in &args[surplus_start..] {
            self.resolve_pat_types(arg);
        }
        true
    }

    /// Exclude the would-be binders of a *parameter* argument of an applied
    /// active-pattern head: run the [`binders`](crate::binders) walk and insert
    /// each returned def's **ident-token** range into
    /// [`Self::excluded_param_ranges`], so the three binder-interning loops drop
    /// them (see that field). The [`BinderRole`] only affects `DefKind`s, never
    /// ranges, so any role serves.
    fn exclude_param_binders(&mut self, arg: &Pat) {
        for def in binders(arg, BinderRole::Pattern) {
            self.excluded_param_ranges.insert(def.range);
        }
    }

    /// Resolve a *parameter* argument of an applied active-pattern head as an
    /// **expression** (evaluated in the enclosing scope), mirroring FCS's
    /// `ConvSynPatToSynExpr`. A **complete** structural walk over the pattern,
    /// deliberately *not* [`Self::resolve_pat_types`] (whose `LongIdent` arm would
    /// resolve applied heads through the *constructor* namespace and re-trigger the
    /// active-pattern split — both wrong in expression position). It splits the two
    /// kinds of use the way FCS's namespaces do:
    ///
    /// - **Type uses** — a `Typed` annotation (`: T`), an `IsInst` test type — are
    ///   resolved **unconditionally**: type names live in a separate namespace,
    ///   unaffected by the value-shadowing risk below.
    /// - **Value uses** — a `Named` leaf, or a single-segment `LongIdent` head
    ///   (nullary `Eq A`, or applied `Eq (Foo x)`; resolved through the *value*
    ///   namespace, not the constructor one) — are gated on
    ///   [`decline_binding_head_param_exprs`](Self::decline_binding_head_param_exprs):
    ///   in a binding-head position an earlier curried parameter that should shadow
    ///   the name is not yet in scope, so committing to an enclosing value would be
    ///   a wrong target — decline there (the binder exclusion still runs, so no
    ///   binder is fabricated). Elsewhere (a `match` clause) they resolve normally.
    ///
    /// Every compound form (`Paren`, `Tuple`, `ArrayOrList`, `Record`, `As`,
    /// `ListCons`, `Ands`, `Or`, an applied head's arguments) is traversed so nested
    /// type/value uses are reached. A multi-segment head (`A.B`, member access) and
    /// an active-pattern-name head are declined (records nothing). A `Quote` body is
    /// an expression captured from the enclosing scope, routed through
    /// [`Self::resolve_expr`] so its **type** uses resolve (the previous
    /// `Pat::Quote` recursion did); it is resolved even in a binding-head position —
    /// `resolve_expr` cannot separate its type uses from its value uses, and the only
    /// case that would mis-commit (a quotation active-pattern argument whose *value*
    /// name is shadowed by an earlier curried parameter) is essentially unreachable.
    fn resolve_pattern_arg_as_expr(&mut self, arg: &Pat) {
        // Value uses are declined in a binding-head position (sound — the binder
        // exclusion already ran) rather than risk committing to an enclosing value
        // an earlier, not-yet-in-scope curried parameter should shadow. Type uses are
        // unaffected and always resolve.
        let resolve_values = !self.decline_binding_head_param_exprs;
        match arg {
            Pat::Named(p) => {
                if resolve_values && let Some(tok) = p.ident() {
                    self.resolve_name_use(&tok);
                }
            }
            Pat::LongIdent(p) => {
                // As an expression, a single-segment head is a value / constructor /
                // function use — resolved through the *value* namespace (gated), not
                // the constructor namespace `resolve_pat_types` uses (which would be
                // wrong under value shadowing, and would re-split a same-file AP). A
                // multi-segment (qualified / member) head or an active-pattern name
                // is declined. The arguments are recursed for their own uses.
                if resolve_values
                    && p.active_pat_name().is_none()
                    && let Some(head) = p.head()
                {
                    let mut idents = head.idents();
                    if let Some(only) = idents.next()
                        && idents.next().is_none()
                    {
                        self.resolve_name_use(&only);
                    }
                }
                for arg in p.args() {
                    self.resolve_pattern_arg_as_expr(&arg);
                }
                if let Some(group) = p.name_pat_pairs() {
                    for pair in group.pairs() {
                        if let Some(value) = pair.pat() {
                            self.resolve_pattern_arg_as_expr(&value);
                        }
                    }
                }
            }
            Pat::Typed(p) => {
                // The type annotation resolves even in a binding-head position; the
                // inner pattern's value use self-gates on `resolve_values`.
                if let Some(ty) = p.ty() {
                    self.resolve_type(&ty);
                }
                if let Some(inner) = p.pat() {
                    self.resolve_pattern_arg_as_expr(&inner);
                }
            }
            Pat::IsInst(p) => {
                // `:? T` — the tested type is a type use (resolved unconditionally).
                if let Some(ty) = p.ty() {
                    self.resolve_type(&ty);
                }
            }
            Pat::Paren(p) => {
                if let Some(inner) = p.inner() {
                    self.resolve_pattern_arg_as_expr(&inner);
                }
            }
            Pat::Attrib(p) => {
                if let Some(inner) = p.pat() {
                    self.resolve_pattern_arg_as_expr(&inner);
                }
            }
            Pat::Tuple(p) => {
                for el in p.elements() {
                    self.resolve_pattern_arg_as_expr(&el);
                }
            }
            Pat::ArrayOrList(p) => {
                for el in p.elements() {
                    self.resolve_pattern_arg_as_expr(&el);
                }
            }
            Pat::Record(p) => {
                for field in p.fields() {
                    if let Some(value) = field.pat() {
                        self.resolve_pattern_arg_as_expr(&value);
                    }
                }
            }
            Pat::As(p) => {
                if let Some(lhs) = p.lhs() {
                    self.resolve_pattern_arg_as_expr(&lhs);
                }
                if let Some(rhs) = p.rhs() {
                    self.resolve_pattern_arg_as_expr(&rhs);
                }
            }
            Pat::ListCons(p) => {
                if let Some(lhs) = p.lhs() {
                    self.resolve_pattern_arg_as_expr(&lhs);
                }
                if let Some(rhs) = p.rhs() {
                    self.resolve_pattern_arg_as_expr(&rhs);
                }
            }
            Pat::Ands(p) => {
                for operand in p.operands() {
                    self.resolve_pattern_arg_as_expr(&operand);
                }
            }
            Pat::Or(p) => {
                if let Some(lhs) = p.lhs() {
                    self.resolve_pattern_arg_as_expr(&lhs);
                }
                if let Some(rhs) = p.rhs() {
                    self.resolve_pattern_arg_as_expr(&rhs);
                }
            }
            Pat::Quote(q) => {
                if let Some(inner) = q.inner() {
                    self.resolve_expr(&inner);
                }
            }
            // No use to resolve. `OptionalVal` (`?x`) is not a meaningful parameter
            // expression; `Const` / `Null` / `Wildcard` name nothing.
            Pat::OptionalVal(_) | Pat::Const(_) | Pat::Null(_) | Pat::Wildcard(_) => {}
        }
    }

    /// Index a genuine (non-augmentation) type definition's members into
    /// [`Self::type_members`] — the trailing-members slot (`type C = … member …`)
    /// plus, for an object model, the repr's own members. Entries are visible
    /// from the definition itself. Last definition of a name wins (mirroring
    /// [`Self::type_defs`] / [`Self::type_cases`]): a redefinition drops the
    /// earlier member set before repopulating.
    pub(super) fn define_type_members(&mut self, defn: &TypeDefn, type_name: &SyntaxToken) {
        let type_key = id_text(type_name.text()).to_string();
        if let Some(by_type) = self.type_members.get_mut(&self.container_path) {
            by_type.remove(&type_key);
        }
        let visible_from = defn.syntax().text_range().start();
        let mut members: Vec<MemberDefn> = defn.members().collect();
        if let Some(TypeDefnRepr::ObjectModel(om)) = defn.repr() {
            members.extend(om.members());
        }
        let container = self.container_path.clone();
        self.add_type_members(&container, &type_key, &members, visible_from);
    }

    /// Index a same-file augmentation's (`type T with …`) members. Only a
    /// **single-ident head naming a type of the current container** is filed —
    /// with the augmentation's own position as the entries' visibility start
    /// (its members do not exist before it: FCS `FS0039`, probe M4a). Any other
    /// head — dotted (`type A.B with …`), or a module-housed optional extension
    /// of an outer type (whose visibility is scope-dependent, probes M5a/M5b) —
    /// lands in [`Self::unindexed_augmented_names`] instead, suppressing member
    /// *emission* for that type name file-wide (the unfiled members could
    /// overload an indexed one).
    pub(super) fn index_augmentation_members(&mut self, defn: &TypeDefn) {
        let Some(li) = defn.long_id() else {
            return;
        };
        let segs: Vec<SyntaxToken> = li.idents().collect();
        let visible_from = defn.syntax().text_range().start();
        if let [seg] = segs.as_slice() {
            let type_key = id_text(seg.text()).to_string();
            if self
                .type_defs
                .get(&self.container_path)
                .is_some_and(|m| m.contains_key(&type_key))
            {
                let mut members: Vec<MemberDefn> = defn.members().collect();
                if let Some(TypeDefnRepr::ObjectModel(om)) = defn.repr() {
                    members.extend(om.members());
                }
                let container = self.container_path.clone();
                self.add_type_members(&container, &type_key, &members, visible_from);
                return;
            }
        }
        if let Some(last) = segs.last() {
            self.unindexed_augmented_names
                .insert(id_text(last.text()).to_string());
        }
    }

    /// EX-3 §2(a): collect the member **names** a `type … with` augmentation
    /// contributes, keyed by staticness, into
    /// [`Resolver::augmentation_instance_names`] /
    /// [`Resolver::augmentation_static_names`] — the extension gate defers
    /// exactly those names instead of every call in the file. Unlike the
    /// *emission* index ([`Self::index_augmentation_members`], which needs the
    /// head to resolve), the names are walkable for **every** augmentation
    /// shape; only a member whose name itself cannot be extracted (an
    /// operator/active-pattern head, an `inherit`) sets
    /// [`Resolver::augmentation_names_unknowable`], keeping the wholesale
    /// defer. Deliberately over-approximate where classification is uncertain:
    /// an unknown staticness lands in both sets, an `override` counts as an
    /// instance name — spurious entries only defer that one name.
    pub(super) fn collect_augmentation_extension_names(&mut self, defn: &TypeDefn) {
        let mut members: Vec<MemberDefn> = defn.members().collect();
        if let Some(TypeDefnRepr::ObjectModel(om)) = defn.repr() {
            members.extend(om.members());
        }
        for m in &members {
            match m {
                MemberDefn::Member(mm) => {
                    if mm.leading_keyword() == MemberLeading::New {
                        // A constructor never joins a named method group.
                        continue;
                    }
                    let name = mm.binding().and_then(|b| b.pat()).and_then(|p| match p {
                        Pat::LongIdent(l) => {
                            if l.syntax()
                                .children()
                                .any(|c| ActivePatName::can_cast(c.kind()))
                            {
                                return None;
                            }
                            let tok = l.head().and_then(|h| h.idents().last())?;
                            let raw = tok.text();
                            let name = id_text(raw).to_string();
                            // An operator member (`static member (+.) …`)
                            // joins resolution under its COMPILED `op_*`
                            // name, which we do not re-derive — unknowable.
                            // A quoted identifier (`` ``odd name`` ``) is a
                            // genuine group key and stays. The ident-shape
                            // test is on the stripped text; the backtick
                            // test on the raw.
                            let quoted = raw.starts_with("``");
                            if !quoted
                                && !name
                                    .chars()
                                    .all(|c| c.is_alphanumeric() || c == '_' || c == '\'')
                            {
                                return None;
                            }
                            Some(name)
                        }
                        _ => None,
                    });
                    match name {
                        Some(name) => {
                            if mm.leading_keyword() == MemberLeading::StaticMember {
                                self.augmentation_static_names.insert(name);
                            } else {
                                self.augmentation_instance_names.insert(name);
                            }
                        }
                        None => self.augmentation_names_unknowable = true,
                    }
                }
                MemberDefn::GetSetMember(g) => {
                    match g
                        .name()
                        .and_then(|n| n.idents().last())
                        .map(|t| id_text(t.text()).to_string())
                    {
                        Some(name) => {
                            if g.is_static() {
                                self.augmentation_static_names.insert(name);
                            } else {
                                self.augmentation_instance_names.insert(name);
                            }
                        }
                        None => self.augmentation_names_unknowable = true,
                    }
                }
                MemberDefn::AutoProperty(a) => match a.ident() {
                    Some(name) => {
                        let name = id_text(name.text()).to_string();
                        if a.is_static() {
                            self.augmentation_static_names.insert(name);
                        } else {
                            self.augmentation_instance_names.insert(name);
                        }
                    }
                    None => self.augmentation_names_unknowable = true,
                },
                // These shapes are illegal in an augmentation (FCS rejects
                // them), but a name we *can* read defers that name in both
                // groups rather than trusting the illegality.
                MemberDefn::AbstractSlot(s) => match s.val_sig().and_then(|v| v.ident()) {
                    Some(name) => {
                        let name = id_text(name.text()).to_string();
                        self.augmentation_instance_names.insert(name.clone());
                        self.augmentation_static_names.insert(name);
                    }
                    None => self.augmentation_names_unknowable = true,
                },
                MemberDefn::ValField(v) => match v.ident() {
                    Some(name) => {
                        let name = id_text(name.text()).to_string();
                        self.augmentation_instance_names.insert(name.clone());
                        self.augmentation_static_names.insert(name);
                    }
                    None => self.augmentation_names_unknowable = true,
                },
                MemberDefn::MemberSig(ms) => match ms.val_sig().and_then(|v| v.ident()) {
                    Some(name) => {
                        let name = id_text(name.text()).to_string();
                        self.augmentation_instance_names.insert(name.clone());
                        self.augmentation_static_names.insert(name);
                    }
                    None => self.augmentation_names_unknowable = true,
                },
                // An `inherit` pulls the base's members in under names we
                // cannot enumerate.
                MemberDefn::Inherit(_) => self.augmentation_names_unknowable = true,
                // Class-local bindings are lexically private; an interface
                // implementation's members are reachable only through the
                // interface, never the receiver's own name group.
                MemberDefn::Interface(_) | MemberDefn::LetBindings(_) | MemberDefn::Do(_) => {}
            }
        }
    }

    /// File `members` under `(container, type_key)`, classifying each shape
    /// per the M-series pins (`docs/project-type-member-plan.md`):
    ///
    /// - **Emit-eligible** — an unrestricted-access `static member` binding
    ///   (property or single method), `static member val`, or static get/set
    ///   property: interned as a [`DefKind::Member`] def (its defining
    ///   occurrence self-resolves) and emitted by the qualified paths.
    /// - **Owned, not emittable** — instance members (they *commit* the
    ///   qualifier: FCS errors FS0806 rather than backtracking, probe M9),
    ///   `override`/`default`, access-restricted members, abstract slots,
    ///   `val` fields (a static one is forcibly `private`, FS0881 — probe
    ///   M3a), member sigs: entered with no emit target.
    /// - **Suppressing** — `inherit` (base statics resolve through the derived
    ///   name, probe M6, and shadowing is unprobed) or any member whose name
    ///   the walker cannot extract (an operator/active-pattern head, a dotted
    ///   property path): sets [`TypeMemberSet::emit_suppressed`].
    /// - **Ignored** — class-local `let`/`do` (lexically private, never
    ///   `Type.x`-reachable) and interface implementations (explicit impls are
    ///   unreachable via the type's own name); constructors (`new`, implicit)
    ///   are not name-accessed members.
    ///
    /// A second same-name member (overloads) clears the entry's emit target
    /// and keeps the earliest visibility.
    fn add_type_members(
        &mut self,
        container: &[String],
        type_key: &str,
        members: &[MemberDefn],
        visible_from: rowan::TextSize,
    ) {
        for m in members {
            match m {
                MemberDefn::Member(mm) => {
                    let leading = mm.leading_keyword();
                    if leading == MemberLeading::New {
                        continue;
                    }
                    let Some(head) = mm.binding().and_then(|b| b.pat()).and_then(|p| match p {
                        Pat::LongIdent(l) => Some(l),
                        _ => None,
                    }) else {
                        self.suppress_member_emit(container, type_key);
                        continue;
                    };
                    let restricted = node_has_access_modifier(Some(mm.syntax()))
                        || mm
                            .binding()
                            .is_some_and(|b| node_has_access_modifier(Some(b.syntax())))
                        || node_has_access_modifier(Some(head.syntax()));
                    let idents: Vec<SyntaxToken> = match head.head() {
                        Some(h) => h.idents().collect(),
                        None => Vec::new(),
                    };
                    if head
                        .syntax()
                        .children()
                        .any(|c| ActivePatName::can_cast(c.kind()))
                        || idents.is_empty()
                        || idents.len() > 2
                    {
                        self.suppress_member_emit(container, type_key);
                        continue;
                    }
                    let name_tok = idents.last().expect("non-empty");
                    let emit_eligible = leading == MemberLeading::StaticMember && !restricted;
                    self.insert_member(container, type_key, name_tok, emit_eligible, visible_from);
                }
                MemberDefn::GetSetMember(g) => {
                    let idents: Vec<SyntaxToken> = match g.name() {
                        Some(n) => n.idents().collect(),
                        None => Vec::new(),
                    };
                    if g.head_pat().is_some_and(|p| {
                        p.syntax()
                            .children()
                            .any(|c| ActivePatName::can_cast(c.kind()))
                    }) || idents.is_empty()
                        || idents.len() > 2
                    {
                        self.suppress_member_emit(container, type_key);
                        continue;
                    }
                    let name_tok = idents.last().expect("non-empty");
                    // Accessor-level modifiers (`with private get () = …`) live
                    // inside the GET/SET accessor node, not the member node
                    // (codex round 2; `dotnet fsi` pins FS0491 on an outside
                    // read) — scan both accessors too.
                    let restricted = node_has_access_modifier(Some(g.syntax()))
                        || g.getter()
                            .is_some_and(|a| node_has_access_modifier(Some(a.syntax())))
                        || g.setter()
                            .is_some_and(|a| node_has_access_modifier(Some(a.syntax())));
                    let emit_eligible = g.is_static() && !restricted;
                    self.insert_member(container, type_key, name_tok, emit_eligible, visible_from);
                }
                MemberDefn::AutoProperty(a) => {
                    let Some(name_tok) = a.ident() else {
                        self.suppress_member_emit(container, type_key);
                        continue;
                    };
                    let emit_eligible =
                        a.is_static() && !node_has_access_modifier(Some(a.syntax()));
                    self.insert_member(container, type_key, &name_tok, emit_eligible, visible_from);
                }
                MemberDefn::AbstractSlot(s) => {
                    let Some(name_tok) = s.val_sig().and_then(|v| v.ident()) else {
                        self.suppress_member_emit(container, type_key);
                        continue;
                    };
                    self.insert_member(container, type_key, &name_tok, false, visible_from);
                }
                MemberDefn::ValField(v) => {
                    let Some(name_tok) = v.ident() else {
                        self.suppress_member_emit(container, type_key);
                        continue;
                    };
                    self.insert_member(container, type_key, &name_tok, false, visible_from);
                }
                MemberDefn::MemberSig(ms) => {
                    let Some(name_tok) = ms.val_sig().and_then(|v| v.ident()) else {
                        self.suppress_member_emit(container, type_key);
                        continue;
                    };
                    self.insert_member(container, type_key, &name_tok, false, visible_from);
                }
                MemberDefn::Inherit(_) => self.suppress_member_emit(container, type_key),
                MemberDefn::Interface(_) | MemberDefn::LetBindings(_) | MemberDefn::Do(_) => {}
            }
        }
    }

    /// Enter one member name under `(container, type_key)`. A fresh
    /// emit-eligible name is interned as a [`DefKind::Member`] def — the emit
    /// target qualified uses resolve to. Its defining occurrence is deliberately
    /// **not** self-recorded: FCS reports extra synthetic symbols at a member's
    /// name token (a `member val`'s compiler-generated backing field `X@`
    /// declares elsewhere), so recording there would disagree with the
    /// differential oracle — an unrecorded definition token is an honest defer.
    /// A repeated name (overloads) keeps a single entry with no emit target and
    /// the earliest visibility.
    fn insert_member(
        &mut self,
        container: &[String],
        type_key: &str,
        name_tok: &SyntaxToken,
        emit_eligible: bool,
        visible_from: rowan::TextSize,
    ) {
        let name = id_text(name_tok.text()).to_string();
        if let Some(entry) = self
            .type_members
            .get_mut(container)
            .and_then(|m| m.get_mut(type_key))
            .and_then(|s| s.entries.get_mut(&name))
        {
            entry.emit = None;
            entry.visible_from = entry.visible_from.min(visible_from);
            return;
        }
        let emit = if emit_eligible {
            Some(self.intern(Def::from_token(name_tok, DefKind::Member)))
        } else {
            None
        };
        self.type_members
            .entry(container.to_vec())
            .or_default()
            .entry(type_key.to_string())
            .or_default()
            .entries
            .insert(name, MemberEntry { emit, visible_from });
    }

    /// Disable member *emission* for `(container, type_key)` — see
    /// [`TypeMemberSet::emit_suppressed`](super::state::TypeMemberSet::emit_suppressed).
    fn suppress_member_emit(&mut self, container: &[String], type_key: &str) {
        self.type_members
            .entry(container.to_vec())
            .or_default()
            .entry(type_key.to_string())
            .or_default()
            .emit_suppressed = true;
    }
}

/// Whether `node` carries a direct access-modifier token
/// ([`SyntaxKind::ACCESS_TOK`] — `private` / `internal` / `public`). Access
/// rules are unmodeled, so any modifier (even `public`) keeps the member out
/// of the emit set — an availability-only conservatism.
fn node_has_access_modifier(node: Option<&borzoi_cst::syntax::SyntaxNode>) -> bool {
    node.is_some_and(|n| {
        n.children_with_tokens()
            .filter_map(|el| el.into_token())
            .any(|t| t.kind() == SyntaxKind::ACCESS_TOK)
    })
}
