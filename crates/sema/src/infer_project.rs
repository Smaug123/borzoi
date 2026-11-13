//! Inference folded over Compile order: what each file publishes for its
//! top-level values, and the fold that threads it to later files.
//!
//! F# checks a project file by file, and a later file sees an earlier file's
//! value at the type the earlier file's check fixed for it — after
//! generalisation, and after FCS has applied its defaults at the end of that
//! file, so nothing a later file does can change it. Inference therefore types
//! a use of an earlier file's top-level value from the type that file's own
//! inference *published* for the binder ([`InferredFile::def_type`]): a ground
//! type, or a generalised scheme, which a use instantiates afresh exactly as it
//! instantiates an in-file scheme. Which binder a name means is the resolver's
//! answer ([`Resolution::Item`]), never re-derived here.
//!
//! What a file publishes is exactly what its own binder hover already claims,
//! so every published type is graded where it is defined by the `binder-types`
//! oracle; the cross-file differential
//! (`crates/sema/tests/all/infer_cross_file_diff.rs`) grades its uses.
//!
//! Everything else is a decline with its reason ([`PrecedingGap`]), and the
//! reasons keep "we looked and there is no certain type" apart from "we did
//! not look": an absent entry in [`PrecedingTypes`] is
//! [`PrecedingGap::NotInferred`], never a licence.
//!
//! [`Resolution::Item`]: crate::Resolution::Item

use std::borrow::Cow;
use std::sync::Arc;

use crate::assembly_env::AssemblyEnv;
use crate::def::{DefId, DefKind};
use crate::hash::{HashMap, HashSet};
use crate::infer::{InferredFile, ProjectCase, ProjectRecord, ProjectRecords, infer_named_file};
use crate::names::NameTable;
use crate::resolve::{
    ItemId, ProjectFile, ResolvedFile, ResolvedProject, same_tree, signature_partners,
};
use crate::ty::{Ty, TyPath};

/// Why a use of an earlier file's top-level value is not typed: the census's
/// split of [`Incomplete::PrecedingValue`](crate::Incomplete::PrecedingValue).
///
/// The variants fall into two kinds, and they must not be merged: the first
/// three say inference never looked at the binding, the rest that it looked
/// and declines for a reason it can name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PrecedingGap {
    /// The declaring file was not inferred: a signature slot, an
    /// implementation FCS rejects whole as a repeated identity, or a file this
    /// fold did not reach. Nothing is known about the binding.
    NotInferred,
    /// The declaring file was inferred, but the binding was not walked: a
    /// `let rec` group, a binding that may be `[<EntryPoint>]`, a
    /// `let mutable` function, a declaration at or after a syntax error. FCS
    /// may type it; inference did not look.
    NotWalked,
    /// The binding was walked, and its type is not certain: an incomplete or
    /// open binding, which publishes neither a ground type nor a scheme.
    Untyped,
    /// The declaring file's inference reached the type budget, so the file
    /// publishes no inferred type
    /// ([`InferredFile::type_budget_exceeded`]).
    TypeBudget,
    /// The declaring file has a signature, which is what a later file sees:
    /// an abstract type there can stand for the implementation's
    /// abbreviation (`type T` over `type T = int`), so the implementation's
    /// type is not the one a use gets.
    Signature,
    /// An `inline` binding.
    Inline,
    /// A binding whose attributes may make F# reject a use of it (an
    /// `[<Obsolete>]`, a `[<CompilerMessage>]`, a
    /// `[<RequiresExplicitTypeArguments>]`, on it or a module around it, the
    /// file's own top-level module included): FCS types such a use as its
    /// error type when the level is an error.
    RejectedUse,
    /// An export that is not a `let`-bound value: a union, enum or exception
    /// case, an active pattern.
    NotAValue,
}

/// What one file publishes for a top-level binder, before the export boundary
/// decides what a later file sees ([`FileContribution::of`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ModuleValue {
    /// The binder's certain type: ground, or a generalised scheme over
    /// [`TyKind::Param`](crate::TyKind::Param)s.
    Typed(Ty),
    /// No certain type, and why.
    Declined(PrecedingGap),
}

/// The types the files before a file in Compile order published for their
/// top-level values, keyed by the resolver's project-global [`ItemId`] — the
/// handle a cross-file [`Resolution::Item`](crate::Resolution::Item) carries.
///
/// Built only by `PrecedingTypes::merge`, one file's `FileContribution` at
/// a time in Compile order ([`infer_project`] is the fold). [`Default`] is the
/// empty table: a file with nothing before it, or a single file inferred on
/// its own, where every cross-file use declines as
/// [`PrecedingGap::NotInferred`].
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PrecedingTypes {
    items: HashMap<ItemId, ModuleValue>,
    /// The types each earlier file's annotations bridge, keyed by the file's
    /// Compile index and the type's position among those it declares — the
    /// identity a later file's
    /// [`Resolution::EarlierFileType`](crate::Resolution::EarlierFileType)
    /// carries — with the path and arity of the [`TyKind::Project`](crate::TyKind::Project) it denotes.
    types: HashMap<(usize, u32), (TyPath, usize)>,
    /// The cases of each earlier file's unions its inference types, by the
    /// case's item — the handle a later file's use of one resolves to.
    cases: HashMap<ItemId, ProjectCase>,
    /// How many constructor-like declarations of each name the earlier files
    /// make, signatured ones included ([`InferredFile::constructor_names`]).
    constructor_names: HashMap<String, usize>,
    /// The records each earlier file declares whose fields its inference
    /// reads, by the path and arity of their `TyKind::Project`.
    records: ProjectRecords,
}

impl ModuleValue {
    /// The value with its type's paths translated from `from`'s table to
    /// `to`'s.
    #[cfg(feature = "test-support")]
    pub(crate) fn rekey(&self, from: &AssemblyEnv, to: &AssemblyEnv) -> ModuleValue {
        match self {
            ModuleValue::Typed(ty) => ModuleValue::Typed(ty.rekey(from, to)),
            ModuleValue::Declined(gap) => ModuleValue::Declined(*gap),
        }
    }
}

impl PrecedingTypes {
    /// The table with every type's paths translated from `from`'s table to
    /// `to`'s.
    #[cfg(feature = "test-support")]
    fn rekey_names(&self, from: &AssemblyEnv, to: &AssemblyEnv) -> PrecedingTypes {
        let PrecedingTypes {
            items,
            types,
            cases,
            constructor_names,
            records,
        } = self;
        PrecedingTypes {
            items: items
                .iter()
                .map(|(id, value)| (*id, value.rekey(from, to)))
                .collect(),
            types: types
                .iter()
                .map(|(key, (path, arity))| (*key, (path.rekey(from.names(), to.names()), *arity)))
                .collect(),
            cases: cases
                .iter()
                .map(|(id, case)| (*id, case.rekey(from, to)))
                .collect(),
            constructor_names: constructor_names.clone(),
            records: crate::infer::rekey_records(records, from, to),
        }
    }

    /// The type a use of the earlier file's item `id` instantiates, or why
    /// there is none. An item no file published is
    /// [`PrecedingGap::NotInferred`]: absence is "not looked at", never
    /// "nothing to find".
    pub fn get(&self, id: ItemId) -> Result<&Ty, PrecedingGap> {
        match self.items.get(&id) {
            Some(ModuleValue::Typed(ty)) => Ok(ty),
            Some(ModuleValue::Declined(gap)) => Err(*gap),
            None => Err(PrecedingGap::NotInferred),
        }
    }

    /// The path and arity of the [`TyKind::Project`](crate::TyKind::Project) an annotation naming the
    /// type at position `index` of the earlier file `file` denotes; `None` for
    /// a type that file's inference would not bridge either.
    pub(crate) fn earlier_type(&self, file: usize, index: u32) -> Option<&(TyPath, usize)> {
        self.types.get(&(file, index))
    }

    /// Whether an earlier file published a type at `path` with `arity`
    /// parameters.
    pub(crate) fn declares_type(&self, path: &TyPath, arity: usize) -> bool {
        self.types.values().any(|(p, a)| *a == arity && p == path)
    }

    /// The case of an earlier file's union that the item `id` is, as that
    /// file's inference typed it; `None` for anything else, and for a case
    /// that file would not type either.
    pub(crate) fn case(&self, id: ItemId) -> Option<&ProjectCase> {
        self.cases.get(&id)
    }

    /// The union of each case and the record of each record the earlier
    /// files published: project types F# seals.
    pub(crate) fn sealed_types(&self) -> impl Iterator<Item = Ty> + '_ {
        self.cases
            .values()
            .map(ProjectCase::union)
            .chain(self.records.values().map(ProjectRecord::record))
    }

    /// How many constructor-like declarations of `name` the earlier files
    /// make.
    pub(crate) fn constructor_name_count(&self, name: &str) -> usize {
        self.constructor_names.get(name).copied().unwrap_or(0)
    }

    /// The record an earlier file declares at `path` with `arity` type
    /// parameters, as that file's inference reads its fields; `None` for
    /// anything else.
    pub(crate) fn record(&self, path: &TyPath, arity: usize) -> Option<&ProjectRecord> {
        self.records.get(&(path.clone(), arity))
    }

    /// Add one file's contribution, in Compile order, after the file is
    /// inferred.
    pub(crate) fn merge(&mut self, contribution: FileContribution<'_>) {
        let FileContribution {
            file,
            constructor_names,
            types,
            cases,
            records,
            items,
        } = contribution;
        for (name, n) in constructor_names {
            *self.constructor_names.entry(name.to_owned()).or_default() += n;
        }
        for (index, ty) in types {
            self.types.insert((file, index), ty.clone());
        }
        self.cases
            .extend(cases.into_iter().map(|(id, case)| (id, case.clone())));
        self.records.extend(
            records
                .into_iter()
                .map(|(key, record)| (key.clone(), record.clone())),
        );
        for (id, verdict) in items {
            let previous = self.items.insert(id, verdict.into_owned());
            debug_assert!(
                previous.is_none(),
                "item {id:?} published twice: the fold must publish each file once"
            );
        }
    }
}

/// What one file adds to [`PrecedingTypes`]: a function of its Compile index,
/// its resolution and inference, and whether a signature screens it, and of
/// nothing else.
///
/// Merging equal contributions into equal tables gives equal tables. That is
/// what lets the fold decide that the files after an edited one see what they
/// saw before by comparing one file's contribution with the one the same file
/// made before ([`infer_project_reusing`]), and never whole tables. So a
/// contribution holds everything [`PrecedingTypes::merge`] reads, and the
/// constructor names, cases and records are sorted by key, since the maps they
/// come from iterate in no fixed order.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct FileContribution<'a> {
    /// The Compile index, which keys [`PrecedingTypes::types`].
    file: usize,
    constructor_names: Vec<(&'a str, usize)>,
    /// Each declared type an annotation bridges, by its position among the
    /// file's types.
    types: Vec<(u32, &'a (TyPath, usize))>,
    cases: Vec<(ItemId, &'a ProjectCase)>,
    records: Vec<(&'a (TyPath, usize), &'a ProjectRecord)>,
    /// A verdict for each export the resolver threads forward, in export
    /// order, so every cross-file use reads a verdict this file decided.
    items: Vec<(ItemId, Cow<'a, ModuleValue>)>,
}

impl<'a> FileContribution<'a> {
    /// The contribution of the file at Compile index `file`. `inferred` is
    /// `None` for a file that was not inferred (a signature slot, or an
    /// implementation repeating an earlier one's identity); `screened` says
    /// the file has a paired signature ([`signature_partners`]), which is then
    /// what every later file sees, so its declared types, cases and records
    /// are not published: the signature may declare them differently.
    pub(crate) fn of(
        names: &NameTable,
        file: usize,
        resolved: &'a ResolvedFile,
        inferred: Option<&'a InferredFile>,
        screened: bool,
    ) -> Self {
        let mut constructor_names: Vec<(&str, usize)> = inferred
            .into_iter()
            .flat_map(|inferred| inferred.constructor_names())
            .map(|(name, &n)| (name.as_str(), n))
            .collect();
        constructor_names.sort_unstable();
        let declared = inferred.filter(|_| !screened);
        let types = declared
            .map(|inferred| {
                resolved
                    .placed_type_defs()
                    .enumerate()
                    .filter_map(|(index, def)| {
                        let ty = inferred.project_types().get(&def)?;
                        let index =
                            u32::try_from(index).expect("more than u32::MAX types in a file");
                        Some((index, ty))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let mut cases: Vec<(ItemId, &ProjectCase)> = declared
            .into_iter()
            .flat_map(|inferred| inferred.project_cases())
            .filter(|(_, case)| case.publishable())
            .map(|(&id, case)| (id, case))
            .collect();
        cases.sort_unstable_by_key(|(id, _)| *id);
        let mut records: Vec<(&(TyPath, usize), &ProjectRecord)> = declared
            .into_iter()
            .flat_map(|inferred| inferred.project_records())
            .filter(|(_, record)| record.publishable())
            .collect();
        // By the path's text: ids are handed out in interning order.
        records.sort_by_cached_key(|&((path, arity), _)| (path.segments_in(names), *arity));
        let items = resolved
            .exports()
            .iter()
            .map(|export| {
                let verdict = match (export.case_kind(), export.def()) {
                    (Some(_), _) => Cow::Owned(ModuleValue::Declined(PrecedingGap::NotAValue)),
                    // A signature-derived export: its def is the `.fsi`'s ident.
                    (None, None) => Cow::Owned(ModuleValue::Declined(PrecedingGap::Signature)),
                    (None, Some(_)) if screened => {
                        Cow::Owned(ModuleValue::Declined(PrecedingGap::Signature))
                    }
                    (None, Some(def)) => Self::own_value(resolved, inferred, def),
                };
                (export.id(), verdict)
            })
            .collect();
        FileContribution {
            file,
            constructor_names,
            types,
            cases,
            records,
            items,
        }
    }

    /// The verdict for an unscreened export bound in the file's own arena.
    fn own_value(
        resolved: &ResolvedFile,
        inferred: Option<&'a InferredFile>,
        def: DefId,
    ) -> Cow<'a, ModuleValue> {
        if !matches!(resolved.def(def).kind, DefKind::Value { .. }) {
            return Cow::Owned(ModuleValue::Declined(PrecedingGap::NotAValue));
        }
        let Some(inferred) = inferred else {
            return Cow::Owned(ModuleValue::Declined(PrecedingGap::NotInferred));
        };
        // A `let`-bound export the walk never met as a module-level binder:
        // not looked at.
        inferred.module_value(def).map_or(
            Cow::Owned(ModuleValue::Declined(PrecedingGap::NotWalked)),
            Cow::Borrowed,
        )
    }
}

/// What a file's inference read, besides the env and what the files before it
/// published: the key under which [`infer_project_reusing`] reuses it.
#[derive(Debug, Clone)]
struct FileInputs {
    /// The resolution, by allocation. The resolver shares one only for a file
    /// whose tree and threaded prefix are unchanged.
    resolved: Arc<ResolvedFile>,
    /// The Compile item: its tree, by identity, and its QNOF, which names a
    /// headerless file's implicit module.
    file: ProjectFile,
    /// Whether a signature screens the file, which decides what it publishes.
    screened: bool,
    /// Whether the file is an implementation with the [`QualifiedNameOfFile`]
    /// of an earlier one: FCS rejects it (FS0239, an implementation of the
    /// file or module already given) and checks nothing in it, so it is not
    /// inferred, and what it exports declines as [`PrecedingGap::NotInferred`].
    ///
    /// [`QualifiedNameOfFile`]: crate::QualifiedNameOfFile
    repeats: bool,
}

impl FileInputs {
    fn same_as(&self, other: &FileInputs) -> bool {
        Arc::ptr_eq(&self.resolved, &other.resolved)
            && same_tree(&self.file, &other.file)
            && self.screened == other.screened
            && self.repeats == other.repeats
    }
}

/// The result of inferring a whole project, or a Compile-order prefix of it:
/// each file's [`InferredFile`] (`None` for a signature slot, and for an
/// implementation FCS rejects whole as a repeated identity), and what every
/// file published for its top-level values.
#[derive(Debug, Default)]
pub struct InferredProject {
    files: Vec<Option<Arc<InferredFile>>>,
    /// What each file's inference read ([`infer_project_reusing`],
    /// [`Self::shared_prefix`]).
    inputs: Vec<FileInputs>,
    published: PrecedingTypes,
}

/// Two folds are equal when they inferred the same thing: each file's result,
/// and what the files published. Which inputs a fold read, by allocation and
/// identity, is how it was computed, not what it computed.
impl PartialEq for InferredProject {
    fn eq(&self, other: &Self) -> bool {
        self.files == other.files && self.published == other.published
    }
}

impl Eq for InferredProject {}

impl InferredProject {
    /// This fold with every type's paths translated from `from`'s table to
    /// `to`'s, so it compares with a fold made against `to`. What the fold
    /// read is kept as it was: it is how the fold was computed, which
    /// equality does not compare.
    #[cfg(feature = "test-support")]
    pub(crate) fn rekey_names(&self, from: &AssemblyEnv, to: &AssemblyEnv) -> InferredProject {
        InferredProject {
            files: self
                .files
                .iter()
                .map(|f| f.as_ref().map(|f| Arc::new(f.rekey_names(from, to))))
                .collect(),
            inputs: self.inputs.clone(),
            published: self.published.rekey_names(from, to),
        }
    }

    /// The inferred file at Compile index `idx`, or `None` for a signature
    /// slot or an implementation that repeats an earlier one's identity.
    pub fn file(&self, idx: usize) -> Option<&InferredFile> {
        self.files.get(idx).and_then(Option::as_deref)
    }

    /// How many leading files of `resolved` this fold inferred from the very
    /// same [`ResolvedFile`]s — the same allocations, which the resolver's
    /// incremental fold shares for a file it did not re-resolve. Those files'
    /// results stand for `resolved` too, provided the env is the one this fold
    /// ran against: a file's inference reads only its tree and resolution, the
    /// env, and what the files before it published.
    pub fn shared_prefix(&self, resolved: &ResolvedProject) -> usize {
        self.inputs
            .iter()
            .zip(resolved.files())
            .take_while(|(a, b)| Arc::ptr_eq(&a.resolved, b))
            .count()
    }

    pub fn len(&self) -> usize {
        self.files.len()
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// What the files inferred so far published: the certain type of a
    /// top-level value `id`, as a later file's use instantiates it — for a
    /// hover on such a use, which shows the value's own type.
    pub fn item_type(&self, id: ItemId) -> Result<&Ty, PrecedingGap> {
        self.published.get(id)
    }
}

/// Infer every file of `resolved` (the whole project, or a prefix of it) in
/// Compile order, threading what each publishes to the files after it.
///
/// `files` is the **whole** Compile list `resolved` was folded from, even
/// when `resolved` is a prefix: whether a file is screened by a signature is
/// decided by pairing over the whole list, as the resolver decides it
/// ([`crate::resolve_project_files_prefix`]), so a file's answer does not
/// depend on how deep the prefix went.
///
/// # Panics
///
/// Panics when `resolved` holds more files than `files`.
pub fn infer_project(
    files: &[ProjectFile],
    resolved: &ResolvedProject,
    env: &AssemblyEnv,
) -> InferredProject {
    infer_project_reusing(files, resolved, env, &InferredProject::default())
}

/// [`infer_project`], reusing `prev`'s inference of each file that nothing it
/// reads has changed for: the incremental form an editor's fold takes after an
/// edit.
///
/// A file's inference reads its tree, its resolution, its pairing, the env,
/// and what the files before it published. So the fold carries a flag that
/// holds while the table entering the file equals the one `prev` had entering
/// it, as the resolver's incremental fold carries `in_sync`. While the flag
/// holds, a file whose resolution is the same allocation as the one `prev`
/// inferred it from, with the same tree and pairing, is reused. A file
/// inferred afresh while the flag holds keeps it set only if its
/// `FileContribution` equals the one `prev`'s inference of it made,
/// recomputed from `prev`'s own resolution and inference rather than stored.
/// So after a body edit that changes no export's name or type, only the
/// edited file is inferred afresh.
///
/// `prev` must have been inferred against `env`: the env is the one input a
/// file's result depends on that this cannot check by pointer.
///
/// # Panics
///
/// Panics when `resolved` holds more files than `files`.
pub fn infer_project_reusing(
    files: &[ProjectFile],
    resolved: &ResolvedProject,
    env: &AssemblyEnv,
    prev: &InferredProject,
) -> InferredProject {
    infer_project_reusing_with_reuse(files, resolved, env, prev).0
}

/// [`infer_project_reusing`], also returning, per file, whether `prev`'s
/// inference of it was reused (`true`) or the file was inferred afresh.
///
/// The reuse vector is what the cutoff exists to produce, and a result that
/// equals a cold fold's holds even when nothing is reused, so tests read it
/// to assert the reuse happens.
///
/// # Panics
///
/// Panics when `resolved` holds more files than `files`.
pub fn infer_project_reusing_with_reuse(
    files: &[ProjectFile],
    resolved: &ResolvedProject,
    env: &AssemblyEnv,
    prev: &InferredProject,
) -> (InferredProject, Vec<bool>) {
    assert!(
        resolved.len() <= files.len(),
        "a resolved prefix of {} files cannot come from a Compile list of {}",
        resolved.len(),
        files.len()
    );
    let partners = signature_partners(files);
    let mut published = PrecedingTypes::default();
    let mut out = Vec::with_capacity(resolved.len());
    let mut inputs = Vec::with_capacity(resolved.len());
    let mut reused = Vec::with_capacity(resolved.len());
    // True while `published` equals the table `prev` had entering this file.
    // Monotone true → false: once a file's contribution differs, every later
    // file reads a different table.
    let mut in_sync = true;
    // The QNOFs of the implementations so far ([`FileInputs::repeats`]).
    let mut implementations: HashSet<&str> = HashSet::default();
    for (idx, rf) in resolved.files().iter().enumerate() {
        let is_impl = files[idx].file.as_impl().is_some();
        let input = FileInputs {
            resolved: Arc::clone(rf),
            file: files[idx].clone(),
            // An implementation's partner is the signature screening it; a
            // signature slot exports nothing of its own.
            screened: is_impl && partners[idx].is_some(),
            repeats: is_impl && !implementations.insert(files[idx].qnof.text()),
        };
        let before = prev
            .inputs
            .get(idx)
            .map(|p| (p, prev.files[idx].as_deref()));
        let reuse = in_sync && before.is_some_and(|(p, _)| p.same_as(&input));
        let inferred = if reuse {
            prev.files[idx].clone()
        } else if input.repeats {
            None
        } else {
            files[idx].file.as_impl().map(|impl_file| {
                Arc::new(infer_named_file(
                    impl_file,
                    rf,
                    env,
                    &published,
                    &files[idx].qnof,
                ))
            })
        };
        let contribution =
            FileContribution::of(env.names(), idx, rf, inferred.as_deref(), input.screened);
        // A reused file contributes what it contributed before, by
        // construction; a file inferred afresh is compared.
        if in_sync && !reuse {
            in_sync = before.is_some_and(|(p, inferred)| {
                contribution
                    == FileContribution::of(env.names(), idx, &p.resolved, inferred, p.screened)
            });
        }
        published.merge(contribution);
        out.push(inferred);
        inputs.push(input);
        reused.push(reuse);
    }
    (
        InferredProject {
            files: out,
            inputs,
            published,
        },
        reused,
    )
}
