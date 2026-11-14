//! The types the project declares, as inference reads them: their paths, and
//! the schemes of their cases and record fields.

use super::*;

/// Which case of which union a [`PatShape::Case`] names: its position in the
/// union's case list, of `siblings` cases.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CaseKey {
    pub(super) union: UnionId,
    pub(super) index: usize,
    pub(super) siblings: usize,
}

/// A union, as reachability tells two apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum UnionId {
    /// A referenced assembly's union.
    Assembly(EntityHandle),
    /// A union the project declares, named by its first case, which no other
    /// union shares.
    Project(OwnCase),
}

/// How the resolver names a case of a union the project declares: by its
/// item, or, in a file with no header (whose cases have no cross-file
/// handle, so the resolver reads each use as the case's definition), by its
/// definition. Only an item's case is ever published to a later file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum OwnCase {
    Item(ItemId),
    Def(DefId),
}

/// A case of a union the project declares, as inference types its uses
/// ([`FileFacts::project_type_schemes`]): its scheme, over the union's type
/// parameters, and its place among the union's cases.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProjectCase {
    /// The case's name, as a use spells it.
    pub(super) name: String,
    pub(super) scheme: UnionCaseScheme,
    pub(super) key: CaseKey,
    /// Where a use of the case is read, for a union whose representation is
    /// `private` (the enclosing module's or namespace's range in the
    /// declaring file) or that a headerless file declares (the file). Such a
    /// case is never published to a later file.
    pub(super) scope: Option<TextRange>,
}

/// The records of the project inference reads field accesses on, by the
/// path and arity of their [`TyKind::Project`] (one type per assembly, FS0249).
pub(crate) type ProjectRecords = HashMap<(TyPath, usize), ProjectRecord>;

/// A record the project declares, as inference reads a field access on it
/// ([`FileFacts::project_type_schemes`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProjectRecord {
    /// The record applied to its own parameters ([`TyKind::Param`]s).
    pub(super) record: Ty,
    /// Each field inference reads, by name, over those parameters.
    pub(super) fields: HashMap<String, Ty>,
    /// Where a field is read, for a `private` representation or a headerless
    /// file's record (as [`ProjectCase::scope`]); such a record is never
    /// published.
    pub(super) scope: Option<TextRange>,
}

/// `records`, their types' paths translated from `from`'s table to `to`'s.
#[cfg(feature = "test-support")]
pub(crate) fn rekey_records(
    records: &ProjectRecords,
    from: &AssemblyEnv,
    to: &AssemblyEnv,
) -> ProjectRecords {
    records
        .iter()
        .map(|((path, arity), record)| {
            let ProjectRecord {
                record: ty,
                fields,
                scope,
            } = record;
            let record = ProjectRecord {
                record: ty.rekey(from, to),
                fields: fields
                    .iter()
                    .map(|(name, ty)| (name.clone(), ty.rekey(from, to)))
                    .collect(),
                scope: *scope,
            };
            ((path.rekey(from.names(), to.names()), *arity), record)
        })
        .collect()
}

impl ProjectRecord {
    /// Whether a later file may read the record's fields.
    pub(crate) fn publishable(&self) -> bool {
        self.scope.is_none()
    }

    /// The record, applied to its own parameters: a type of the env's table.
    pub(crate) fn record(&self) -> Ty {
        self.record
    }
}

/// A type definition whose declared fields inference reads.
enum DeclaredFields {
    Union(UnionRepr),
    Record(RecordRepr),
}

impl ProjectCase {
    /// The case with its scheme's paths translated from `from`'s table to
    /// `to`'s.
    #[cfg(feature = "test-support")]
    pub(crate) fn rekey(&self, from: &AssemblyEnv, to: &AssemblyEnv) -> ProjectCase {
        let ProjectCase {
            name,
            scheme,
            key,
            scope,
        } = self;
        ProjectCase {
            name: name.clone(),
            scheme: UnionCaseScheme {
                fields: scheme.fields.iter().map(|f| f.rekey(from, to)).collect(),
                union: scheme.union.rekey(from, to),
            },
            key: *key,
            scope: *scope,
        }
    }

    /// Whether a later file may read the case.
    pub(crate) fn publishable(&self) -> bool {
        self.scope.is_none()
    }

    /// The case's union, applied to its own parameters: a type of the env's
    /// table.
    pub(crate) fn union(&self) -> Ty {
        self.scheme.union
    }
}

impl<'a> FileFacts<'a> {
    /// The path FCS names each in-file type definition by — its namespace,
    /// each enclosing module's compiled name, its own — with its arity, for
    /// the definitions where that is provably the written path
    /// ([`TyKind::Project`]). Probed against FCS's rendering: a type's
    /// `[<CompiledName>]` renames it, a module takes a `Module` suffix beside a
    /// same-named type or under `CompilationRepresentation(ModuleSuffix)`, and
    /// a file without a header names its implicit module after the file. So a
    /// definition is left out when
    ///
    /// - it is not a nominal type F# names by its own path: an abbreviation
    ///   (FCS names its target), an augmentation, inline IL, or a bodyless
    ///   type (a measure);
    /// - a type parameter carries a constraint or an attribute (a measure):
    ///   no argument is checked against either, and F# rejects an
    ///   application violating one, recovering the binder to a variable;
    /// - it may carry `[<CompiledName>]` or an attribute that may make F#
    ///   reject a use ([`Self::rejects_use`]), or an enclosing module may carry
    ///   `[<CompilationRepresentation>]` or such an attribute — by the
    ///   resolver's verdict on each attribute ([`Self::attribute_may_be`]);
    /// - an enclosing module shares its name with any type the file declares
    ///   (a headerless file's implicit module included);
    /// - the file has no `module` or `namespace` header and its implicit
    ///   module's path is not known ([`Self::anonymous_module`]), or is
    ///   `namespace global`;
    /// - the type is declared twice: another definition in its container has
    ///   its name and arity (FS0037), or an earlier file published its path
    ///   (FS0249). FCS rejects the second and keeps one, so neither is read.
    pub(super) fn project_type_paths(&self, file: &ImplFile) -> HashMap<DefId, (TyPath, usize)> {
        let env = self.env;
        let type_names: HashSet<String> = file
            .syntax()
            .descendants_of::<TypeDefn>()
            .filter_map(|d| d.long_id())
            .flat_map(|li| li.idents().map(|t| ident_text(&t)).collect::<Vec<_>>())
            .collect();
        // A module whose attribute renames it, or may make F# reject a use
        // reaching the type through it. A module's attributes and its header
        // keyword are found among its children, which are also its
        // declarations, so both are read once per module rather than once per
        // type in it, which would cost a module of `n` types `n²`.
        let mut renames_cache: HashMap<SyntaxNode, bool> = HashMap::default();
        let mut renames_module = |node: &SyntaxNode| {
            *renames_cache.entry(node.clone()).or_insert_with(|| {
                self.attribute_may_be(node, |h| {
                    env.is_compilation_representation_attribute(h) || self.rejects_use(h)
                })
            })
        };
        let mut module_headers: HashMap<SyntaxNode, Option<Vec<String>>> = HashMap::default();
        let mut out = HashMap::default();
        for defn in file.syntax().descendants_of::<TypeDefn>() {
            let Some(name) = defn.long_id().and_then(|li| {
                let mut idents = li.idents();
                let first = idents.next()?;
                idents.next().is_none().then_some(first)
            }) else {
                continue;
            };
            // The defining occurrence of an in-file type, not an augmentation
            // naming one.
            let Some(def) = self.def_at(name.text_range()) else {
                continue;
            };
            let d = self.resolved.def(def);
            if d.kind != DefKind::Type || d.range != name.text_range() {
                continue;
            }
            let nominal = match defn.repr() {
                Some(
                    TypeDefnRepr::Record(_)
                    | TypeDefnRepr::Union(_)
                    | TypeDefnRepr::Enum(_)
                    | TypeDefnRepr::Delegate(_),
                ) => true,
                Some(TypeDefnRepr::ObjectModel(om)) => !om.is_augmentation(),
                Some(TypeDefnRepr::Abbrev(_) | TypeDefnRepr::InlineIl(_)) | None => false,
            };
            // A type parameter with a constraint or an attribute (a measure):
            // nothing here checks an argument against either, and F# rejects
            // an application that violates one, recovering the binder.
            // `TypeDefn::constraints` reads both places a `when` constraint
            // can be written: inside the parameter list and after the type's
            // name; an intersection (`'a & #IDisposable`) sits on the
            // parameter itself.
            let unchecked_parameters = defn.constraints().next().is_some()
                || defn.typar_decls().is_some_and(|decls| {
                    decls.typars().any(|t| {
                        t.is_head_type()
                            || t.attributes().next().is_some()
                            || t.intersection_constraints().next().is_some()
                    })
                });
            if !nominal
                || unchecked_parameters
                || self.attribute_may_be(defn.syntax(), |h| {
                    env.is_compiled_name_attribute(h) || self.rejects_use(h)
                })
            {
                continue;
            }
            let mut path = vec![ident_text(&name)];
            let mut provable = true;
            for anc in defn.syntax().ancestors().skip(1) {
                if let Some(m) = NestedModuleDecl::cast(anc.clone()) {
                    let Some(li) = m.long_id() else {
                        provable = false;
                        break;
                    };
                    let segs: Vec<String> = li.idents().map(|t| ident_text(&t)).collect();
                    if renames_module(&anc) || segs.iter().any(|s| type_names.contains(s)) {
                        provable = false;
                        break;
                    }
                    path.splice(0..0, segs);
                } else if let Some(m) = ModuleOrNamespace::cast(anc.clone()) {
                    // The path the module contributes, read once per module:
                    // its kind and header are found among its children.
                    let header = module_headers
                        .entry(anc.clone())
                        .or_insert_with(|| {
                            let kind = m.kind();
                            let segs: Option<Vec<String>> = match kind {
                                ModuleOrNamespaceKind::DeclaredNamespace => m
                                    .long_id()
                                    .map(|li| li.idents().map(|t| ident_text(&t)).collect()),
                                ModuleOrNamespaceKind::NamedModule if !renames_module(&anc) => m
                                    .long_id()
                                    .map(|li| li.idents().map(|t| ident_text(&t)).collect()),
                                // A headerless file's implicit module carries
                                // no attribute to rename it.
                                ModuleOrNamespaceKind::Anon => self.anonymous_module.clone(),
                                _ => None,
                            };
                            // A module named like a type the file declares
                            // may compile under another name.
                            segs.filter(|segs| {
                                kind == ModuleOrNamespaceKind::DeclaredNamespace
                                    || segs.last().is_some_and(|last| !type_names.contains(last))
                            })
                        })
                        .clone();
                    let Some(segs) = header else {
                        provable = false;
                        break;
                    };
                    path.splice(0..0, segs);
                    break;
                }
            }
            if !provable {
                continue;
            }
            let arity = defn.typar_decls().map_or(0, |t| t.typars().count());
            let path = TyPath::new(env.names(), &path);
            if self.preceding.declares_type(&path, arity) {
                continue;
            }
            out.insert(def, (path, arity));
        }
        // Every definition of a container's name and arity, augmentations
        // aside (they extend, not redeclare).
        let mut declared: HashMap<(TextRange, String, usize), usize> = HashMap::default();
        for defn in file.syntax().descendants_of::<TypeDefn>() {
            if matches!(defn.repr(), Some(TypeDefnRepr::ObjectModel(om)) if om.is_augmentation()) {
                continue;
            }
            let Some(container) = defn.syntax().ancestors().skip(1).find(|a| {
                matches!(
                    a.kind(),
                    SyntaxKind::NESTED_MODULE_DECL | SyntaxKind::MODULE_OR_NAMESPACE
                )
            }) else {
                continue;
            };
            let arity = defn.typar_decls().map_or(0, |t| t.typars().count());
            for name in defn
                .long_id()
                .into_iter()
                .flat_map(|li| li.idents().collect::<Vec<_>>())
            {
                *declared
                    .entry((container.text_range(), ident_text(&name), arity))
                    .or_default() += 1;
            }
        }
        // Two blocks of one namespace are one container: a path and arity
        // computed twice is a type declared twice.
        let mut paths: HashMap<(TyPath, usize), usize> = HashMap::default();
        for key in out.values() {
            *paths.entry(key.clone()).or_default() += 1;
        }
        out.retain(|_, key| paths.get(key).is_some_and(|&n| n == 1));
        out.retain(|def, (_, arity)| {
            let d = self.resolved.def(*def);
            let Some(container) = file
                .syntax()
                .covering_element(d.range)
                .ancestors()
                .find(|a| {
                    matches!(
                        a.kind(),
                        SyntaxKind::NESTED_MODULE_DECL | SyntaxKind::MODULE_OR_NAMESPACE
                    )
                })
            else {
                return false;
            };
            declared
                .get(&(
                    container.text_range(),
                    d.spelling(self.env).into_owned(),
                    *arity,
                ))
                .is_none_or(|&n| n == 1)
        });
        out
    }

    /// The scheme of each case of the file's own unions whose type is read
    /// exactly, by how the resolver names the case ([`OwnCase`]): its fields'
    /// written types, read in the union's own type-parameter scope
    /// ([`AnnotationCx::DeclTypars`]), and the union ([`TyKind::Project`])
    /// applied to its parameters; and the same for each record's fields. Reads
    /// [`Self::own_union_cases`], [`Self::project_types`] and
    /// [`Self::constructor_names`], which [`Self::build`] has filled.
    ///
    /// A union contributes only when [`Self::project_type_paths`] holds its
    /// path (which already leaves out a constrained or attributed parameter,
    /// and an attribute that may make F# reject a use) and its declaration
    /// did not recover. A `private` representation confines its cases to
    /// the enclosing module or namespace ([`ProjectCase::scope`]), and a
    /// headerless file's are confined to the file. A case is left out
    /// when it carries an attribute (an error-level `[<Obsolete>]` rejects
    /// its use), is written in the `Case : type` signature form, or has a field whose type the annotation bridge declines (a
    /// measure among them); a use of one declines.
    ///
    /// So is a case whose name another export of the file also declares (a
    /// case of another union, a value, an exception, an active pattern's
    /// case): FCS resolves such a
    /// name by orders the resolver's latest-wins export history does not
    /// follow. An `open` adds a module's types, and so their cases, before
    /// its values (`let X = 5` then `type UV = X | W`: the opened `X` is the
    /// value), and a module-qualified name has its own: values, then
    /// exceptions, then the first type declaring the case, in an expression;
    /// that type, then exceptions, active patterns and values, in a pattern
    /// (`ResolveExprLongIdentInModuleOrNamespace`,
    /// `ResolvePatternLongIdentInModuleOrNamespace`; `type U1 = X | Y` and
    /// `type U2 = X | Z`: FCS's `Lib.X` is `U1`'s). A name the module
    /// declares once means one thing in every order.
    pub(super) fn project_type_schemes(
        &self,
        file: &ImplFile,
        tys: &TyTable,
    ) -> (HashMap<OwnCase, ProjectCase>, ProjectRecords) {
        let mut out = HashMap::default();
        let mut records: ProjectRecords = HashMap::default();
        let recovery = self.resolved.recovery();
        let headerless = is_headerless(file);
        // Built at the first record, so a file without one pays nothing.
        let mut member_names: Option<IntrinsicMemberNames> = None;
        for defn in file.syntax().descendants_of::<TypeDefn>() {
            let repr = match defn.repr() {
                Some(TypeDefnRepr::Union(repr)) => DeclaredFields::Union(repr),
                Some(TypeDefnRepr::Record(repr)) => DeclaredFields::Record(repr),
                _ => continue,
            };
            let repr_node = match &repr {
                DeclaredFields::Union(r) => r.syntax().clone(),
                DeclaredFields::Record(r) => r.syntax().clone(),
            };
            // Recovery drops what it cannot parse, so a recovered type's
            // surviving cases or fields look well-formed (`C : | D` reads as
            // two nullary cases) where FCS rejects them.
            if !recovery.declaration_is_intact(defn.syntax())
                || !recovery.clean_through(defn.syntax())
            {
                continue;
            }
            // `type U = private A | B`: the cases (or fields) are accessible
            // only inside the type's enclosing module or namespace, outside
            // which FCS rejects a use the resolver still reads. (An
            // `internal` representation is the whole assembly's.)
            let private_repr = repr_node
                .children_with_tokens()
                .any(|el| el.kind() == SyntaxKind::ACCESS_TOK && el.to_string() == "private");
            let scope = if private_repr {
                let Some(container) = defn.syntax().ancestors().skip(1).find(|a| {
                    matches!(
                        a.kind(),
                        SyntaxKind::NESTED_MODULE_DECL | SyntaxKind::MODULE_OR_NAMESPACE
                    )
                }) else {
                    continue;
                };
                Some(container.text_range())
            } else if headerless {
                // A later file reaches nothing of a headerless file by name
                // (the resolver gives its contents no cross-file handle), so
                // its schemes stay in the file: another file's type at the
                // same path (`one/Program.fs` and `two/Program.fs` share the
                // implicit module `Program`) is a different type.
                Some(file.syntax().text_range())
            } else {
                None
            };
            let Some(name) = defn.long_id().and_then(|li| {
                let mut idents = li.idents();
                let first = idents.next()?;
                idents.next().is_none().then_some(first)
            }) else {
                continue;
            };
            let Some((path, arity)) = self
                .def_at(name.text_range())
                .and_then(|def| self.project_types.get(&def))
                .cloned()
            else {
                continue;
            };
            // `arity` counts these same declarations.
            let names: Option<Vec<String>> = defn.typar_decls().map_or(Some(Vec::new()), |d| {
                d.typars()
                    .map(|t| t.ident().map(|tok| ident_text(&tok)))
                    .collect()
            });
            // A name declared twice is an error FCS recovers from by binding
            // its uses to the first declaration, where a map keeps the last.
            let Some(names) = names.filter(|n| n.iter().collect::<HashSet<_>>().len() == n.len())
            else {
                continue;
            };
            let decl_typars: HashMap<String, u32> = names
                .into_iter()
                .enumerate()
                .map(|(i, n)| {
                    (
                        n,
                        u32::try_from(i).expect("a type of under u32::MAX parameters"),
                    )
                })
                .collect();
            let cx = AnnotationCx::DeclTypars(&decl_typars);
            let params: Vec<Ty> = (0..arity)
                .map(|i| tys.param(u32::try_from(i).expect("checked above")))
                .collect();
            let declared = tys.project(path.clone(), params);
            match repr {
                DeclaredFields::Union(repr) => {
                    self.union_case_schemes(tys, cx, &repr, declared, scope, &mut out);
                }
                DeclaredFields::Record(repr) => {
                    let members = member_names
                        .get_or_insert_with(|| IntrinsicMemberNames::of(file))
                        .of_type(&defn);
                    let fields = self.record_field_schemes(tys, cx, &repr, members);
                    let mut memo = HashMap::default();
                    let record = ProjectRecord {
                        record: self.publish(tys, declared, &mut memo),
                        fields: fields
                            .into_iter()
                            .map(|(name, ty)| (name, self.publish(tys, ty, &mut memo)))
                            .collect(),
                        scope,
                    };
                    records.insert((path, arity), record);
                }
            }
        }
        (out, records)
    }

    /// The scheme of each case of the union `repr`, the in-file union
    /// `union` over its parameters, whose fields are all read, into `out`
    /// ([`Self::project_type_schemes`]), each field read in `cx`, which holds
    /// the union's type parameters.
    fn union_case_schemes(
        &self,
        tys: &TyTable,
        cx: AnnotationCx<'_>,
        repr: &UnionRepr,
        union: Ty,
        scope: Option<TextRange>,
        out: &mut HashMap<OwnCase, ProjectCase>,
    ) {
        {
            let cases: Vec<_> = repr.cases().collect();
            let items: Vec<Option<OwnCase>> = cases
                .iter()
                .map(|c| {
                    let tok = c.ident()?;
                    self.own_case_at(tok.text_range())
                        .filter(|case| match case {
                            OwnCase::Item(_) => true,
                            OwnCase::Def(def) => self.resolved.def(*def).range == tok.text_range(),
                        })
                })
                .collect();
            // The union's identity is its first case: without one, two of its
            // patterns could not be told to be one union's.
            let Some(Some(first)) = items.first().copied() else {
                return;
            };
            for (index, (case, item)) in cases.iter().zip(items).enumerate() {
                let Some(item) = item else {
                    continue;
                };
                if case.attributes().next().is_some() || case.full_type().is_some() {
                    continue;
                }
                let fields: Option<Vec<Ty>> = case
                    .fields()
                    .map(|f| f.ty().and_then(|t| self.annotation_ty(tys, &t, cx)))
                    .collect();
                let Some(fields) = fields else {
                    continue;
                };
                let key = CaseKey {
                    union: UnionId::Project(first),
                    index,
                    siblings: cases.len(),
                };
                let mut memo = HashMap::default();
                let scheme = UnionCaseScheme {
                    fields: fields
                        .into_iter()
                        .map(|f| self.publish(tys, f, &mut memo))
                        .collect(),
                    union: self.publish(tys, union, &mut memo),
                };
                let Some(name) = case.ident().map(|t| ident_text(&t)) else {
                    continue;
                };
                out.insert(
                    item,
                    ProjectCase {
                        name,
                        scheme,
                        key,
                        scope,
                    },
                );
            }
        }
    }

    /// The type of each field of the record `repr` that is read, by name,
    /// read in `cx`, which holds the record's parameters. A field with an
    /// attribute (an error-level `[<Obsolete>]` rejects its use), a label
    /// declared twice, or a type the annotation bridge declines is left out:
    /// an access to it declines, and the record's other fields do not depend
    /// on it.
    ///
    /// A field is read only when no intrinsic member of the record shares its
    /// name: FCS's lookup prefers the member (`{ Equals: int }`'s `r.Equals`
    /// is the generated method). Those are the members a record generates
    /// ([`RECORD_GENERATED_MEMBERS`]) and the ones its definition, or a
    /// same-file augmentation of its name, declares (`members`, the record's
    /// [`IntrinsicMemberNames`]); a member whose name is not read off its head
    /// (`members` is `None`) leaves no field read.
    fn record_field_schemes(
        &self,
        tys: &TyTable,
        cx: AnnotationCx<'_>,
        repr: &RecordRepr,
        members: Option<&HashSet<String>>,
    ) -> HashMap<String, Ty> {
        let Some(members) = members else {
            return HashMap::default();
        };
        // A label declared twice is an error FCS recovers from by keeping
        // the first, where a map keeps the last: neither is read.
        let mut labels: HashMap<String, usize> = HashMap::default();
        for f in repr.fields() {
            if let Some(tok) = f.ident() {
                *labels.entry(ident_text(&tok)).or_default() += 1;
            }
        }
        repr.fields()
            .filter(|f| f.attributes().next().is_none())
            .filter(|f| {
                f.ident().is_some_and(|tok| {
                    let name = ident_text(&tok);
                    labels.get(&name) == Some(&1)
                        && !members.contains(&name)
                        && !RECORD_GENERATED_MEMBERS.contains(&name.as_str())
                })
            })
            .filter_map(|f| {
                let name = ident_text(&f.ident()?);
                let ty = self.annotation_ty(tys, &f.ty()?, cx)?;
                Some((name, ty))
            })
            .collect()
    }

    /// [`Self::sealed_project_types`]: every union a case inference reads
    /// belongs to, and every record whose fields it reads, in this file or an
    /// earlier one.
    pub(super) fn sealed_project_types(&self) -> HashMap<TyPath, HashSet<usize>> {
        let own_unions = self.project_cases.values().map(|pc| pc.scheme.union);
        let own_records = self.project_records.values().map(|r| r.record);
        let mut out: HashMap<TyPath, HashSet<usize>> = HashMap::default();
        for ty in own_unions
            .chain(own_records)
            .chain(self.preceding.sealed_types())
        {
            if let TyKind::Project { path, args } = self.env.types().kind(ty) {
                out.entry(path.clone()).or_default().insert(args.len());
            }
        }
        out
    }
}

/// How many declarations in `file` could be what a use of each name means
/// where a union case of that name is expected: every top-level export (a
/// value, a union or `exception` case, an active pattern's case) and every
/// type definition that can construct — anything but an unattributed union
/// or record, whose own name is no constructor (an enum and a struct have a
/// default one) — at any depth.
///
/// A file with no header exports only its top-level values: its cases, its
/// `exception` constructors, its active patterns' cases and its nested
/// modules' values have no cross-file handle. So there every definition of
/// those kinds counts, and every value's, local ones included, which can only
/// make a name contested.
pub(super) fn constructor_names(
    file: &ImplFile,
    resolved: &ResolvedFile,
    env: &AssemblyEnv,
) -> HashMap<String, usize> {
    let mut counts: HashMap<String, usize> = HashMap::default();
    for e in resolved.exports().iter() {
        *counts.entry(e.name(env).to_string()).or_default() += 1;
    }
    if is_headerless(file) {
        for def in resolved.defs() {
            if matches!(
                def.kind,
                DefKind::Value { .. }
                    | DefKind::UnionCase
                    | DefKind::ExceptionCase
                    | DefKind::ActivePatternCase
                    | DefKind::EnumCase
            ) {
                *counts.entry(def.spelling(env).into_owned()).or_default() += 1;
            }
        }
    }
    for defn in file.syntax().descendants_of::<TypeDefn>() {
        // A union's or record's own name constructs nothing, unless it is a
        // struct (a default constructor), which an attribute may make it.
        if matches!(
            defn.repr(),
            Some(TypeDefnRepr::Union(_) | TypeDefnRepr::Record(_))
        ) && defn.attributes().next().is_none()
        {
            continue;
        }
        for name in defn
            .long_id()
            .into_iter()
            .flat_map(|li| li.idents().collect::<Vec<_>>())
        {
            *counts.entry(ident_text(&name)).or_default() += 1;
        }
    }
    counts
}

/// The instance members F# gives every record (from `System.Object`, and
/// the equality and comparison it generates), which a field of the same
/// name does not shadow.
const RECORD_GENERATED_MEMBERS: &[&str] = &[
    "Equals",
    "GetHashCode",
    "GetType",
    "ToString",
    "CompareTo",
    "MemberwiseClone",
    "Finalize",
];

/// The names of the instance-accessible members each type in a file
/// declares, in its definition or a same-file augmentation of its name (`type
/// R with member …`), keyed by the last segment of that name; `None` for a
/// name one of whose definitions declares a member whose name is not read off
/// its head. An interface implementation is reached only through the
/// interface, so it names nothing here.
///
/// Built in one pass over the file: a record asking of every type
/// definition for its augmentations would make a file of `n` records cost
/// `n²`.
pub(super) struct IntrinsicMemberNames(HashMap<String, Option<HashSet<String>>>);

impl IntrinsicMemberNames {
    pub(super) fn of(file: &ImplFile) -> IntrinsicMemberNames {
        let mut by_name: HashMap<String, Option<HashSet<String>>> = HashMap::default();
        for d in file.syntax().descendants_of::<TypeDefn>() {
            let Some(name) = type_last_name(&d) else {
                continue;
            };
            let declared = declared_member_names(&d);
            let entry = by_name
                .entry(name)
                .or_insert_with(|| Some(HashSet::default()));
            match (entry.as_mut(), declared) {
                (Some(names), Some(declared)) => names.extend(declared),
                _ => *entry = None,
            }
        }
        IntrinsicMemberNames(by_name)
    }

    /// The members of the type `defn` defines, or `None` when one is not
    /// read (or `defn` has no name).
    pub(super) fn of_type(&self, defn: &TypeDefn) -> Option<&HashSet<String>> {
        self.0.get(&type_last_name(defn)?)?.as_ref()
    }
}

/// The last segment of the name a type definition declares.
fn type_last_name(d: &TypeDefn) -> Option<String> {
    d.long_id()
        .and_then(|li| li.idents().last())
        .map(|t| ident_text(&t))
}

/// The names of the instance-accessible members the one definition `d`
/// declares ([`IntrinsicMemberNames`]), or `None` when one is a member whose
/// name is not read off its head.
fn declared_member_names(d: &TypeDefn) -> Option<Vec<String>> {
    let mut members: Vec<MemberDefn> = d.members().collect();
    if let Some(TypeDefnRepr::ObjectModel(om)) = d.repr() {
        members.extend(om.members());
    }
    let mut out = Vec::new();
    for m in members {
        match m {
            MemberDefn::Interface(_) => {}
            MemberDefn::Member(mm) => {
                let Some(Pat::LongIdent(l)) = mm.binding().and_then(|b| b.pat()) else {
                    return None;
                };
                let tok = l.head().and_then(|h| h.idents().last())?;
                out.push(ident_text(&tok));
            }
            _ => return None,
        }
    }
    Some(out)
}
