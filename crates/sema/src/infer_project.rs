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

use std::collections::HashMap;
use std::sync::Arc;

use crate::assembly_env::AssemblyEnv;
use crate::def::{DefId, DefKind};
use crate::infer::{InferredFile, ProjectCase, infer_file};
use crate::resolve::{ItemId, ProjectFile, ResolvedFile, ResolvedProject, signature_partners};
use crate::ty::Ty;

/// Why a use of an earlier file's top-level value is not typed: the census's
/// split of [`Incomplete::PrecedingValue`](crate::Incomplete::PrecedingValue).
///
/// The variants fall into two kinds, and they must not be merged: the first
/// three say inference never looked at the binding, the rest that it looked
/// and declines for a reason it can name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PrecedingGap {
    /// The declaring file was not inferred: a signature slot, or a file this
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
/// decides what a later file sees ([`PrecedingTypes::publish`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ModuleValue {
    /// The binder's certain type: ground, or a generalised scheme over
    /// [`Ty::Param`]s.
    Typed(Ty),
    /// No certain type, and why.
    Declined(PrecedingGap),
}

/// The types the files before a file in Compile order published for their
/// top-level values, keyed by the resolver's project-global [`ItemId`] — the
/// handle a cross-file [`Resolution::Item`](crate::Resolution::Item) carries.
///
/// Built only by [`PrecedingTypes::publish`], one file at a time in Compile
/// order ([`infer_project`] is the fold). [`Default`] is the empty table: a
/// file with nothing before it, or a single file inferred on its own, where
/// every cross-file use declines as [`PrecedingGap::NotInferred`].
#[derive(Debug, Default, Clone)]
pub struct PrecedingTypes {
    items: HashMap<ItemId, ModuleValue>,
    /// The types each earlier file's annotations bridge, keyed by the file's
    /// Compile index and the type's position among those it declares — the
    /// identity a later file's
    /// [`Resolution::EarlierFileType`](crate::Resolution::EarlierFileType)
    /// carries — with the path and arity of the [`Ty::Project`] it denotes.
    types: HashMap<(usize, u32), (Vec<String>, usize)>,
    /// The cases of each earlier file's unions its inference types, by the
    /// case's item — the handle a later file's use of one resolves to.
    cases: HashMap<ItemId, ProjectCase>,
}

impl PrecedingTypes {
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

    /// The path and arity of the [`Ty::Project`] an annotation naming the
    /// type at position `index` of the earlier file `file` denotes; `None` for
    /// a type that file's inference would not bridge either.
    pub(crate) fn earlier_type(&self, file: usize, index: u32) -> Option<&(Vec<String>, usize)> {
        self.types.get(&(file, index))
    }

    /// The case of an earlier file's union that the item `id` is, as that
    /// file's inference typed it; `None` for anything else, and for a case
    /// that file would not type either.
    pub(crate) fn case(&self, id: ItemId) -> Option<&ProjectCase> {
        self.cases.get(&id)
    }

    /// Publish one file's exports, in Compile order, after the file is
    /// inferred. `file` is its Compile index; `inferred` is `None` for a file
    /// that was not inferred (a signature slot); `screened` says the file has
    /// a paired signature ([`signature_partners`]), which is then what every
    /// later file sees.
    ///
    /// Each export the resolver threads forward gets an entry, so every
    /// cross-file use reads a verdict this file decided. A screened file's
    /// types are not published: the signature may declare them differently.
    pub fn publish(
        &mut self,
        file: usize,
        resolved: &ResolvedFile,
        inferred: Option<&InferredFile>,
        screened: bool,
    ) {
        if let Some(inferred) = inferred.filter(|_| !screened) {
            for (index, def) in resolved.placed_type_defs().enumerate() {
                if let Some(ty) = inferred.project_types().get(&def) {
                    let index = u32::try_from(index).expect("more than u32::MAX types in a file");
                    self.types.insert((file, index), ty.clone());
                }
            }
            self.cases.extend(
                inferred
                    .project_cases()
                    .iter()
                    .map(|(&id, case)| (id, case.clone())),
            );
        }
        for export in resolved.exports().iter() {
            let verdict = match (export.case_kind(), export.def()) {
                (Some(_), _) => ModuleValue::Declined(PrecedingGap::NotAValue),
                // A signature-derived export: its def is the `.fsi`'s ident.
                (None, None) => ModuleValue::Declined(PrecedingGap::Signature),
                (None, Some(_)) if screened => ModuleValue::Declined(PrecedingGap::Signature),
                (None, Some(def)) => Self::own_value(resolved, inferred, def),
            };
            let previous = self.items.insert(export.id(), verdict);
            debug_assert!(
                previous.is_none(),
                "item {:?} published twice: the fold must publish each file once",
                export.id()
            );
        }
    }

    /// The verdict for an unscreened export bound in the file's own arena.
    fn own_value(
        resolved: &ResolvedFile,
        inferred: Option<&InferredFile>,
        def: DefId,
    ) -> ModuleValue {
        if !matches!(resolved.def(def).kind, DefKind::Value { .. }) {
            return ModuleValue::Declined(PrecedingGap::NotAValue);
        }
        let Some(inferred) = inferred else {
            return ModuleValue::Declined(PrecedingGap::NotInferred);
        };
        inferred
            .module_value(def)
            .cloned()
            // A `let`-bound export the walk never met as a module-level
            // binder: not looked at.
            .unwrap_or(ModuleValue::Declined(PrecedingGap::NotWalked))
    }
}

/// The result of inferring a whole project, or a Compile-order prefix of it:
/// each file's [`InferredFile`] (`None` for a signature slot), and what every
/// file published for its top-level values.
#[derive(Debug, Default)]
pub struct InferredProject {
    files: Vec<Option<Arc<InferredFile>>>,
    /// The resolved file each file was inferred from, by pointer
    /// ([`Self::shared_prefix`]).
    resolved: Vec<Arc<ResolvedFile>>,
    published: PrecedingTypes,
}

impl InferredProject {
    /// The inferred file at Compile index `idx`, or `None` for a signature
    /// slot.
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
        self.resolved
            .iter()
            .zip(resolved.files())
            .take_while(|(a, b)| Arc::ptr_eq(a, b))
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

/// [`infer_project`], reusing each leading file of `prev` inferred from the
/// same resolved file as `resolved`'s ([`InferredProject::shared_prefix`]) —
/// the incremental form an editor's fold takes after an edit, which leaves
/// every file before the edited one as it was.
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
    assert!(
        resolved.len() <= files.len(),
        "a resolved prefix of {} files cannot come from a Compile list of {}",
        resolved.len(),
        files.len()
    );
    let reuse = prev.shared_prefix(resolved);
    let partners = signature_partners(files);
    let mut published = PrecedingTypes::default();
    let mut out = Vec::with_capacity(resolved.len());
    for (idx, rf) in resolved.files().iter().enumerate() {
        let inferred = if idx < reuse {
            prev.files[idx].clone()
        } else {
            files[idx]
                .file
                .as_impl()
                .map(|impl_file| Arc::new(infer_file(impl_file, rf, env, &published)))
        };
        // An implementation's partner is the signature screening it; a
        // signature slot exports nothing of its own.
        let screened = files[idx].file.as_impl().is_some() && partners[idx].is_some();
        published.publish(idx, rf, inferred.as_deref(), screened);
        out.push(inferred);
    }
    InferredProject {
        files: out,
        resolved: resolved.files().to_vec(),
        published,
    }
}
