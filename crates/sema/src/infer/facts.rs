//! [`FileFacts`]: what one file's inference reads and never writes, built
//! before its walk.

use super::*;

/// The immutable facts of one file's inference: its resolution, the env, what
/// the earlier files published, and what the file declares. [`FileFacts::build`]
/// computes them before any [`Gen`] exists, and the walk only reads them.
pub(super) struct FileFacts<'a> {
    pub(super) resolved: &'a ResolvedFile,
    /// What the preceding Compile-order files published for their top-level
    /// values ([`infer_file`]'s `preceding`).
    pub(super) preceding: &'a PrecedingTypes,
    /// The flattened referenced-assembly view a member access resolves *into*
    /// (Stage 3.3a): the receiver's [`TyKind::Named`] head is looked up here for its
    /// instance data member. Read-only (pure — values in).
    pub(super) env: &'a AssemblyEnv,
    /// The file's in-scope instance-extension-member surface (OV-6): the
    /// extension-absence gate the method-call wake consults before committing an
    /// overload.
    pub(super) ext_scope: ExtensionScope,
    /// The path of the implicit module a headerless file's contents live
    /// under, when the file's name is known and FCS's spelling of it is
    /// modelled ([`infer_named_file`]); `None` for any other file.
    pub(super) anonymous_module: Option<Vec<String>>,
    /// The path FCS names each in-file type definition by, and its arity,
    /// where that path is provably the written one
    /// ([`Self::project_type_paths`]); a type absent here is not bridged.
    pub(super) project_types: HashMap<DefId, (TyPath, usize)>,
    /// The cases of the file's own unions whose fields are all bridged, by
    /// how the resolver names the case ([`Self::project_type_schemes`]).
    pub(super) project_cases: HashMap<OwnCase, ProjectCase>,
    /// How many constructor-like declarations of each name the file makes
    /// ([`constructor_names`]).
    constructor_names: HashMap<String, usize>,
    /// The file's own records whose fields inference reads
    /// ([`Self::project_type_schemes`]).
    pub(super) project_records: ProjectRecords,
    /// Every case of the file's own unions, typed or not, that has an item
    /// ([`Self::own_case_at`]; a headerless file's cases have none).
    pub(super) own_union_cases: HashSet<ItemId>,
    /// The project's unions and records inference knows of, by path, each
    /// with its arities: this file's ([`Self::project_cases`],
    /// [`Self::project_records`]) and those earlier files published. F#
    /// seals both, so no type coerces into one ([`Gen::no_subsumption_ty`]).
    pub(super) sealed_project_types: HashMap<TyPath, HashSet<usize>>,
    /// The defining ranges of the in-file binders a use of which F# may reject
    /// for their attributes ([`Self::rejected_use_defs`]).
    rejected_defs: HashSet<TextRange>,
    /// The module headers and nested modules whose attributes may make F#
    /// reject a use of their contents ([`Self::rejecting_modules`]).
    rejecting_modules: HashSet<SyntaxNode>,
    /// The trivia-trimmed span of each node a span was asked of
    /// ([`Self::node_span`]). A cache of a pure function of the tree, so
    /// shared access may fill it: borrowed only inside `node_span`.
    spans: RefCell<NodeSpans>,
}

impl<'a> FileFacts<'a> {
    /// The facts of `file`, whose resolution is `resolved`. What the file
    /// declares is read in the session's type table `tys`, and published into
    /// the env's.
    ///
    /// Each step reads only the facts before it: the modules and binders whose
    /// attributes may make F# reject a use, then the paths of the file's types,
    /// then the schemes of their cases and fields (which name those types),
    /// and last the sealed types (which collect those schemes).
    pub(super) fn build(
        file: &ImplFile,
        resolved: &'a ResolvedFile,
        env: &'a AssemblyEnv,
        tys: &TyTable,
        preceding: &'a PrecedingTypes,
        anonymous_module: Option<Vec<String>>,
    ) -> FileFacts<'a> {
        let mut facts = FileFacts {
            resolved,
            preceding,
            env,
            ext_scope: ExtensionScope::of(resolved, env),
            anonymous_module,
            project_types: HashMap::default(),
            project_cases: HashMap::default(),
            constructor_names: HashMap::default(),
            project_records: HashMap::default(),
            own_union_cases: HashSet::default(),
            sealed_project_types: HashMap::default(),
            rejected_defs: HashSet::default(),
            rejecting_modules: HashSet::default(),
            spans: RefCell::new(NodeSpans::default()),
        };
        facts.rejecting_modules = facts.rejecting_modules(file);
        facts.rejected_defs = facts.rejected_use_defs(file);
        facts.project_types = facts.project_type_paths(file);
        facts.own_union_cases = resolved
            .exports()
            .iter()
            .filter(|e| matches!(e.case_kind(), Some(CaseKind::Union { .. })))
            .map(|e| e.id())
            .collect();
        facts.constructor_names = constructor_names(file, resolved, env);
        (facts.project_cases, facts.project_records) = facts.project_type_schemes(file, tys);
        facts.sealed_project_types = facts.sealed_project_types();
        facts
    }

    /// The [`InferredFile`] of a walk that read these facts: what the walk
    /// inferred, and what the file declares.
    pub(super) fn into_inferred(self, walked: Walked) -> InferredFile {
        let Walked {
            types,
            def_types,
            member_resolutions,
            incompleteness,
            receiver_types,
            module_values,
            deepest_expr_depth,
            type_budget_exceeded,
            type_walks,
        } = walked;
        InferredFile {
            types,
            def_types,
            member_resolutions,
            incompleteness,
            receiver_types,
            module_values,
            project_types: self.project_types,
            // A case named by its definition has no handle a later file could
            // use, so only the cases held by item are published.
            project_cases: self
                .project_cases
                .into_iter()
                .filter_map(|(case, pc)| match case {
                    OwnCase::Item(id) => Some((id, pc)),
                    OwnCase::Def(_) => None,
                })
                .collect(),
            constructor_names: self.constructor_names,
            project_records: self.project_records,
            deepest_expr_depth,
            type_budget_exceeded,
            type_walks,
        }
    }

    /// `ty`, a type of the session's table `tys` ([`Solver::tys`]), copied
    /// into the env's: what the file publishes, and the schemes it declares.
    /// `memo` carries the copies made so far. `ty` must be ground.
    pub(super) fn publish(&self, tys: &TyTable, ty: Ty, memo: &mut HashMap<Ty, Ty>) -> Ty {
        self.env.types().import(tys, ty, memo)
    }

    /// The in-file binder a source range resolves to, if any (a cross-file item,
    /// assembly entity/member, deferred, or unresolved use returns `None`, so it
    /// stays Deferred).
    pub(super) fn def_at(&self, range: TextRange) -> Option<DefId> {
        self.resolved
            .resolution_at(range)
            .and_then(|res| self.resolved.resolved_def_id(res))
    }

    /// Whether `node` — a `let` declaration or one of its bindings — carries an
    /// attribute of its own (`[<A>] let x = …` puts it on the declaration,
    /// `let [<A>] x = …` on the binding) that may be FSharp.Core's
    /// `EntryPointAttribute`. FCS unifies such a binding with `string[] -> int`
    /// before checking its body, so neither the binding's type nor its body's
    /// nodes can be read from the binding alone.
    ///
    /// Which type an attribute denotes is the resolver's verdict
    /// ([`ResolvedFile::attribute_may_be`]), since an abbreviation or an
    /// `open type` reaches `EntryPoint` under any name; an attribute with no
    /// name to key it by counts as a possible `EntryPoint`.
    pub(super) fn may_be_entry_point(&self, node: &SyntaxNode) -> bool {
        let env = self.env;
        self.attribute_may_be(node, |h| env.is_entry_point_attribute(h))
    }

    /// Whether `node` carries an attribute of its own that may denote a type
    /// `is_target` picks out, by the resolver's verdict on each attribute
    /// ([`ResolvedFile::attribute_may_be`]); an attribute with no name to key
    /// it by may be anything.
    pub(super) fn attribute_may_be(
        &self,
        node: &SyntaxNode,
        is_target: impl Fn(EntityHandle) -> bool,
    ) -> bool {
        node.children()
            .filter_map(AttributeList::cast)
            .flat_map(|list| list.attributes().collect::<Vec<_>>())
            .any(|attr| {
                let toks: Vec<SyntaxToken> = attr
                    .type_name()
                    .map(|n| n.idents().collect())
                    .unwrap_or_default();
                let (Some(first), Some(last)) = (toks.first(), toks.last()) else {
                    return true;
                };
                let range = TextRange::new(first.text_range().start(), last.text_range().end());
                self.resolved.attribute_may_be(range, self.env, &is_target)
            })
    }

    /// The defining ranges of every binder in `file` whose binding carries an
    /// attribute that may make F# **reject a use** of it: an `[<Obsolete>]` or
    /// `[<CompilerMessage>]` (F# checks both at every use, and fails an
    /// error-level one outright, typing the use as its error type rather than
    /// by the definition), or `[<RequiresExplicitTypeArguments>]`. Whether the
    /// level is an error is not read, so a warning-level one declines too.
    ///
    /// A binding inside a nested module carrying such an attribute counts too,
    /// at any depth: F# rejects an `open` of the module (FCS reports the
    /// attribute at the `open` and leaves the module unopened, so a name after
    /// it binds whatever it would have without the module — FSharp.Core's
    /// `&&` rather than the module's), and a qualified use reports the
    /// attribute at the use.
    ///
    /// Every name the binding's head binds is collected, its parameters
    /// included: a parameter is used only inside the binding, where declining
    /// costs nothing a use outside would not.
    fn rejected_use_defs(&self, file: &ImplFile) -> HashSet<TextRange> {
        let rejects = |h: EntityHandle| self.rejects_use(h);
        let in_rejected_module = |node: &SyntaxNode| {
            node.ancestors().any(|a| {
                a.kind() == SyntaxKind::NESTED_MODULE_DECL && self.rejecting_modules.contains(&a)
            })
        };
        let mut out = HashSet::default();
        for node in file.syntax().descendants() {
            let (decl_attributed, bindings): (bool, Vec<Binding>) =
                if let Some(decl) = LetDecl::cast(node.clone()) {
                    (
                        self.attribute_may_be(&node, rejects) || in_rejected_module(&node),
                        decl.bindings().collect(),
                    )
                } else if let Some(e) = LetOrUseExpr::cast(node.clone()) {
                    (
                        self.attribute_may_be(&node, rejects) || in_rejected_module(&node),
                        e.bindings().collect(),
                    )
                } else {
                    continue;
                };
            for binding in bindings {
                if !decl_attributed && !self.attribute_may_be(binding.syntax(), rejects) {
                    continue;
                }
                if let Some(pat) = binding.pat() {
                    out.extend(
                        pat.syntax()
                            .tokens_where(|k| k == SyntaxKind::IDENT_TOK)
                            .map(|t| t.text_range()),
                    );
                }
            }
        }
        out
    }

    /// Whether the attribute type `h` makes F# reject (or warn about) a use of
    /// what it is on: an `[<Obsolete>]`, a `[<CompilerMessage>]`, a
    /// `[<RequiresExplicitTypeArguments>]`.
    pub(super) fn rejects_use(&self, h: EntityHandle) -> bool {
        matches!(
            self.env.entity_full_name(h).as_str(),
            "System.ObsoleteAttribute"
                | "Microsoft.FSharp.Core.CompilerMessageAttribute"
                | "Microsoft.FSharp.Core.RequiresExplicitTypeArgumentsAttribute"
        )
    }

    /// Whether `node` sits in a top-level module whose header carries an
    /// attribute that may make F# reject a use of its contents
    /// ([`Self::rejects_use`]). A use from another file reaches the value
    /// through that module, so such a value is never published
    /// ([`Gen::finish`]); a use inside the module is the in-file rule's.
    pub(super) fn in_rejecting_header(&self, node: &SyntaxNode) -> bool {
        node.ancestors().any(|a| {
            a.kind() == SyntaxKind::MODULE_OR_NAMESPACE && self.rejecting_modules.contains(&a)
        })
    }

    /// Every module header and nested module in `file` whose attributes may
    /// make F# reject a use of its contents ([`Self::rejects_use`]).
    ///
    /// A module's attributes are found among its children, which are also
    /// its declarations, so each binding asking of its own containers would
    /// make a module of `n` bindings cost `n²`; asked once per module here,
    /// the file costs one pass.
    fn rejecting_modules(&self, file: &ImplFile) -> HashSet<SyntaxNode> {
        file.syntax()
            .descendants()
            .filter(|node| {
                matches!(
                    node.kind(),
                    SyntaxKind::MODULE_OR_NAMESPACE | SyntaxKind::NESTED_MODULE_DECL
                ) && self.attribute_may_be(node, |h| self.rejects_use(h))
            })
            .collect()
    }

    /// Whether a use of the in-file binder `def` may be one F# rejects for the
    /// binder's attributes ([`Self::rejected_use_defs`]).
    pub(super) fn use_may_be_rejected(&self, def: DefId) -> bool {
        self.rejected_defs.contains(&self.resolved.def(def).range)
    }

    /// The case the resolver reads at `at` — a referenced assembly's, or a
    /// case of a union the project declares — with its scheme and its place
    /// among its union's cases. Anything else declines, and so does a case
    /// whose type inference cannot read: one the maker cannot vouch for, or
    /// a project case [`Self::project_type_schemes`] left out.
    pub(super) fn case_at(&self, at: TextRange) -> Result<(CaseKey, UnionCaseScheme), Incomplete> {
        match self.resolved.resolution_at(at) {
            Some(Resolution::UnionCase { union, case }) => {
                let scheme =
                    union_case_scheme(self.env, union, case).map_err(Incomplete::AssemblyCase)?;
                let siblings = match &self.env.entity(union).union_cases {
                    borzoi_assembly::UnionCases::Known(cases) => cases.len(),
                    borzoi_assembly::UnionCases::Unknowable => {
                        unreachable!("a scheme read off a union whose cases are unknown")
                    }
                };
                let key = CaseKey {
                    union: UnionId::Assembly(union),
                    index: case.index(),
                    siblings,
                };
                Ok((key, scheme))
            }
            Some(Resolution::Item(id)) => self.read_project_case(at, self.project_case(id)),
            Some(Resolution::Local(_)) => {
                let case = self.own_case_at(at).ok_or(Incomplete::NotInFileValue)?;
                self.read_project_case(at, self.project_cases.get(&case))
            }
            _ => Err(Incomplete::NotInFileValue),
        }
    }

    /// The key and scheme of the project case `case` a use at `at` reads,
    /// when the use is in its scope and nothing else declares its name
    /// ([`Self::uncontested_case_name`]).
    fn read_project_case(
        &self,
        at: TextRange,
        case: Option<&ProjectCase>,
    ) -> Result<(CaseKey, UnionCaseScheme), Incomplete> {
        case.filter(|pc| pc.scope.is_none_or(|scope| scope.contains_range(at)))
            .filter(|pc| self.uncontested_case_name(&pc.name))
            .map(|pc| (pc.key, pc.scheme.clone()))
            .ok_or(Incomplete::NotInFileValue)
    }

    /// Whether the project declares nothing else a use of the case name
    /// `name` could mean, in this file or an earlier one: only the case itself
    /// is a constructor-like declaration of the name ([`constructor_names`],
    /// [`PrecedingTypes::constructor_name_count`]).
    ///
    /// FCS resolves such a name by orders the resolver's latest-wins export
    /// history does not follow. An `open` adds a module's types, and so their
    /// cases, before its values (`let X = 5` then `type UV = X | W`: the
    /// opened `X` is the value; an active pattern `(|C|_|)` before
    /// `type U = C of int` makes the opened `C` the active pattern), a later
    /// file's class takes the case's constructor name, and a qualified name
    /// is resolved against everything of the name its module or namespace
    /// holds, `[<RequireQualifiedAccess>]` cases and other files' fragments
    /// included. A name declared once means one thing in every order.
    fn uncontested_case_name(&self, name: &str) -> bool {
        let own = self.constructor_names.get(name).copied().unwrap_or(0);
        own + self.preceding.constructor_name_count(name) == 1
    }

    /// The case of a project union that the item `id` is, as inference types
    /// it: this file's own ([`Self::project_type_schemes`]), or what the
    /// earlier file declaring it published ([`PrecedingTypes::case`]).
    fn project_case(&self, id: ItemId) -> Option<&ProjectCase> {
        if self.resolved.owns_item(id) {
            self.project_cases.get(&OwnCase::Item(id))
        } else {
            self.preceding.case(id)
        }
    }

    /// Whether the resolver reads a case of one of this file's own unions at
    /// `at` ([`Self::own_case_at`]): it is a case, never a value binder's use.
    pub(super) fn own_union_case_at(&self, at: TextRange) -> bool {
        self.own_case_at(at).is_some()
    }

    /// The case of one of this file's own unions the resolver reads at `at`,
    /// typed or not: the case's item, or, in a headerless file, its
    /// definition ([`OwnCase`]). Either way the use's def is the case's
    /// declaration.
    pub(super) fn own_case_at(&self, at: TextRange) -> Option<OwnCase> {
        match self.resolved.resolution_at(at)? {
            Resolution::Item(id) if self.own_union_cases.contains(&id) => Some(OwnCase::Item(id)),
            Resolution::Local(def) if self.resolved.def(def).kind == DefKind::UnionCase => {
                Some(OwnCase::Def(def))
            }
            _ => None,
        }
    }

    /// Whether the resolver reads a case at `at` whose fields FCS eta-expands
    /// when it is used unapplied (`List.map Some`): FCS's node at the case's
    /// range is then the case built inside the expansion, the union, never
    /// the function, so nothing is emitted for such a use.
    pub(super) fn case_of_fields_at(&self, at: TextRange) -> bool {
        self.case_at(at)
            .is_ok_and(|(_, scheme)| !scheme.fields.is_empty())
    }

    /// The source span FCS reports for `node`: its range with leading and
    /// trailing trivia trimmed ([`NodeSpans::span`]).
    pub(super) fn node_span(&self, node: &SyntaxNode) -> TextRange {
        self.spans.borrow_mut().span(node)
    }
}
