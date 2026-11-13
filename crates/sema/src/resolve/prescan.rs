//! What is decided before the walk starts ([`Prescan`]): facts about the
//! whole file, and about the inputs it is resolved against. The walk borrows
//! it and cannot write it, so a fact computed here holds at every position of
//! the file, whatever the walk has reached. Each is order-independent by
//! design: a name declared after a use counts as much as one declared before.

use std::collections::BTreeSet;

use crate::hash::{HashMap, HashSet};

use borzoi_cst::syntax::{
    AstNode, ExceptionDefnDecl, ExternDecl, ImplFile, JoinInExpr, ModuleDecl, NestedModuleDecl,
    SyntaxKind, SyntaxNode, TextRange, TypeDefn, TypeDefnRepr, UnionCase,
};

use super::contributors;
use super::custom_operations::{self, file_custom_operations, may_be_custom_operation};
use super::model::ProjectItems;
use super::state::implicit_open_groups;
use super::{
    abbrev_target_single_ident, attrs_auto_open, dropped_in_namespace,
    exception_named_like_core_attribute, id_text, is_type_augmentation,
    type_defn_renames_attribute, type_defn_renames_core_attribute,
};
use crate::assembly_env::AssemblyEnv;
use crate::assembly_env::CustomOperationNames;
use crate::binders::every_let_pattern_binder_name;

/// The file's prescan facts; see the [module docs](self).
#[derive(Debug)]
pub(super) struct Prescan {
    /// Whether this file declares a type spelled like `[<AutoOpen>]` or
    /// `[<RequireQualifiedAccess>]`, or an abbreviation of one, or an
    /// exception so named — which makes every written attribute name
    /// untrustworthy for those two
    /// ([`Resolver::attribute_spellings_trusted`](super::state::Resolver::attribute_spellings_trusted)).
    /// Whole-file, so a `[<AutoOpen>]` written before the alias is declared
    /// is as untrustworthy as one written after it.
    pub(super) declares_core_attribute_alias: bool,
    /// Whether the file declares a type spelled like
    /// `CustomOperationAttribute` or an abbreviation of it, after which any
    /// attribute may be one
    /// ([`FileContributions::declares_custom_operation_alias`](super::contributors::FileContributions::declares_custom_operation_alias)).
    pub(super) declares_custom_operation_alias: bool,
    /// Whether the file declares a type spelled like `StructAttribute` or
    /// `MeasureAttribute` or an abbreviation of one
    /// ([`FileContributions::declares_slot_attribute_alias`](super::contributors::FileContributions::declares_slot_attribute_alias)).
    pub(super) declares_slot_attribute_alias: bool,
    /// The names the custom operations the file's type augmentations declare
    /// may take, which extend every builder in every later computation
    /// expression, this file's included
    /// ([`FileContributions::custom_operations`](super::contributors::FileContributions::custom_operations)).
    /// Whole-file: F# reads a builder's operations off the completed type.
    pub(super) custom_operations: CustomOperationNames,
    /// The ranges of the file's declarations FCS drops unchecked
    /// ([`super::dropped_in_namespace`]), which the walk does not enter.
    pub(super) dropped_decls: HashSet<TextRange>,
    /// How many *leading* [`OpenScope::imports`](crate::resolve::open_scope::OpenScope::imports) groups are the implicit
    /// auto-opens ([`implicit_open_groups`]). Constant for the file: every
    /// reset re-seeds `imports` with exactly that list, and explicit opens
    /// are only ever appended after it — so `imports[..implicit_import_count]`
    /// is always the implicit seed and the rest the explicit source opens
    /// ([`Resolver::explicit_open_reading_prefixes`](crate::resolve::Resolver::explicit_open_reading_prefixes) relies on this split).
    pub(super) implicit_import_count: usize,
    /// Every project **type**'s simple name declared *anywhere in this file*,
    /// pre-scanned before the walk —
    /// order-independent by design. The attribute resolution's own-file guard
    /// (`Resolver::project_type_named`): a candidate a project type could
    /// satisfy must defer, because the tiered walk indexes neither
    /// later-in-file types (not yet walked) nor abbreviation targets.
    /// Over-approximate (an augmentation target counts too) — sound, since a
    /// spurious match only defers.
    pub(super) own_type_simple_names: HashSet<String>,
    /// Every nested project **module definition**'s simple name declared
    /// *anywhere in this file*, pre-scanned beside
    /// [`Self::own_type_simple_names`] and order-independent for the same
    /// reason. The dotted half of the project `[<AutoOpen>]` shadow
    /// (`Resolver::project_module_named`): what owns a *dotted* head is a
    /// module, so that is the index the head is asked of.
    ///
    /// `module X = A.B` **abbreviations** are excluded — they publish nothing
    /// past their own container, and inside it are modelled by
    /// [`Indexes::module_aliases`](crate::resolve::indexes::Indexes::module_aliases) rather than hidden. Over-approximate in the
    /// remaining direction (a module in an unrelated container counts too) —
    /// sound, since a spurious match only defers.
    pub(super) own_module_simple_names: HashSet<String>,
    /// Every **value binder** simple name introduced anywhere in the file —
    /// pre-scanned file-globally (like [`Self::own_type_simple_names`]) from the
    /// resolution-independent [`binders`](crate::binders) walk over every pattern,
    /// *before* its provisional/interning drops, plus each `extern` prototype's
    /// name (the one value binder that has no pattern to walk, and which the decl
    /// arm deliberately never interns). The expression-constructor fallback
    /// ([`Resolver::opened_constructor_target`](crate::resolve::Resolver::opened_constructor_target)) consults it: the value-frame
    /// [`lookup`](crate::resolve::Resolver::lookup) is deliberately conservative and returns `None` for
    /// names a binder still owns — a provisional uppercase parameter (`let f
    /// (Thing: int) = Thing`), a would-be-shadowed local — so committing an opened
    /// assembly type for such a bare name would be a wrong go-to-definition. Any
    /// name a binder could own defers instead. File-global and order-independent:
    /// over-approximate (a binder in a sibling scope the use cannot see counts
    /// too), which is sound — a spurious match only defers.
    pub(super) own_binder_simple_names: HashSet<String>,
    /// The subset of [`Self::own_type_simple_names`] with a **generic**
    /// declaration anywhere in the file. FCS's attribute lookup is arity-0
    /// (`DefiniteEmpty`), so it skips a generic local type — an in-file
    /// attribute commit for such a name defers (codex round 7).
    pub(super) own_generic_type_simple_names: HashSet<String>,
    /// How many type or exception declarations this file makes under each
    /// simple name, anywhere — later ones, nested modules and `rec` blocks
    /// included. Inside a `rec` block a declaration the walk has not reached
    /// yet is already in scope, so more here than
    /// [`Indexes::type_decl_counts`](crate::resolve::indexes::Indexes::type_decl_counts) can see is a possible forward shadow.
    pub(super) own_type_decl_counts: HashMap<String, usize>,
    /// The simple names and arities of this file's genuine type definitions
    /// (any container, any position), each with whether some definition is
    /// generic or could declare union cases (a union, or an abbreviation whose
    /// target may be read as a case, `type T = T`). An augmentation can only
    /// be intrinsic to a type of its head's arity, so one whose name and arity
    /// no such definition makes *scoped* sees the module scope alone whether
    /// FCS reads it as intrinsic or optional, and is walked without deciding
    /// which ([`MemberScope`](super::types::MemberScope)).
    pub(super) own_type_scoped: HashMap<(String, usize), bool>,
    /// The subset of [`Self::own_type_simple_names`] declared by an
    /// `exception` anywhere in the file. Exceptions never enter `type_defs`,
    /// so an in-file *type* hit for such a name may be reaching past a closer
    /// exception FCS would bind — the in-file attribute commit defers
    /// (codex on stage 4).
    pub(super) own_exception_simple_names: HashSet<String>,
    /// The subset of [`Self::own_type_simple_names`] declared as an
    /// **abbreviation** anywhere in the file — a committed
    /// [`Resolution::Local`](crate::resolve::model::Resolution::Local) attribute resolution of such a name may alias
    /// `ExtensionAttribute` (the resolver does not chase in-file abbreviation
    /// targets), so the gate's derivation treats it as a possible extension
    /// marker (EX-3 §2(d) stage 5).
    pub(super) own_abbrev_type_simple_names: HashSet<String>,
    /// The type and exception simple names declared **directly inside any
    /// `[<AutoOpen>]` module of this file** — pre-scanned file-globally like
    /// [`Self::own_type_simple_names`], so position- and block-blind. An
    /// in-file attribute hit for such a name must defer (AO-2): the
    /// auto-open import contests it positionally in FCS, and the walk's
    /// block-scoped [`OpenScope::auto_open_type_shadow_names`](crate::resolve::open_scope::OpenScope::auto_open_type_shadow_names) guard cannot see a
    /// straddle — a block-1 direct type, a block-2 auto-open redeclaration,
    /// and a block-3 attribute bind the *auto-open's* type in FCS while
    /// the in-file lookup retains block 1's (codex on AO-2). Position-blind
    /// is over-approximate — an in-file def declared after the import would
    /// win and could commit — which only defers (sound).
    pub(super) own_auto_open_type_names: HashSet<String>,
    /// Every name this file's type augmentations declare: what an `open
    /// type` may bring as extension members, in this file and, published as
    /// [`FileContributions::extension_statics`](crate::resolve::contributors::FileContributions::extension_statics),
    /// in later ones.
    pub(super) extension_statics: BTreeSet<String>,
}

impl Prescan {
    /// The prescan of `file`, resolved after `preceding` against
    /// `assemblies`.
    pub(super) fn of(
        file: &ImplFile,
        preceding: &ProjectItems,
        assemblies: &AssemblyEnv,
    ) -> Prescan {
        let mut p = Prescan {
            declares_core_attribute_alias: declares_core_attribute_alias(file.syntax()),
            declares_custom_operation_alias: false,
            declares_slot_attribute_alias: false,
            custom_operations: CustomOperationNames::default(),
            dropped_decls: dropped_in_namespace(file),
            implicit_import_count: implicit_open_groups(assemblies).len(),
            own_type_simple_names: HashSet::default(),
            own_module_simple_names: HashSet::default(),
            own_binder_simple_names: HashSet::default(),
            own_generic_type_simple_names: HashSet::default(),
            own_type_decl_counts: HashMap::default(),
            own_type_scoped: HashMap::default(),
            own_exception_simple_names: HashSet::default(),
            own_abbrev_type_simple_names: HashSet::default(),
            own_auto_open_type_names: HashSet::default(),
            extension_statics: BTreeSet::new(),
        };
        // EX-3 §2(d): pre-scan every type definition's simple name for the
        // attribute resolution's project-type guard. Whole-file and up front (not
        // accumulated during the walk) so it is order-independent — a `[<Foo>]`
        // candidate must defer whether the potentially-aliasing `type
        // FooAttribute` is declared before or after it, or in a sibling block.
        // Over-approximate (an augmentation target's last segment counts too),
        // which is sound: a spurious match only adds a defep.
        for defn in file.syntax().descendants_of::<TypeDefn>() {
            if custom_operations::type_defn_renames_custom_operation(&defn) {
                p.declares_custom_operation_alias = true;
            }
            if is_type_augmentation(&defn) {
                let names = contributors::declared_identifiers(defn.syntax());
                p.extension_statics.extend(names);
            }
            if type_defn_renames_attribute(&defn, crate::assembly_env::SLOT_ATTRIBUTE_NAMES) {
                p.declares_slot_attribute_alias = true;
            }
            if let Some(name) = defn.long_id().and_then(|li| li.idents().last()) {
                p.own_type_simple_names.insert(id_text(&name).to_string());
                if !is_type_augmentation(&defn) {
                    let arity = defn.typar_decls().map_or(0, |t| t.typars().count());
                    let scoped = arity > 0
                        || match defn.repr() {
                            Some(TypeDefnRepr::Union(_)) => true,
                            Some(TypeDefnRepr::Abbrev(a)) => a
                                .ty()
                                .as_ref()
                                .and_then(abbrev_target_single_ident)
                                .is_some(),
                            _ => false,
                        };
                    *p.own_type_scoped
                        .entry((id_text(&name).to_string(), arity))
                        .or_default() |= scoped;
                }
                // A generic declaration of the name disqualifies an in-file
                // attribute commit: FCS's attribute lookup is arity-0
                // (`DefiniteEmpty`), so it skips a generic local and binds an
                // assembly arity-0 type instead, and the in-file lookup only
                // pins a local of the written arity in the innermost container
                // declaring the name (codex round 7).
                if defn.typar_decls().is_some() {
                    p.own_generic_type_simple_names
                        .insert(id_text(&name).to_string());
                }
                if !is_type_augmentation(&defn) {
                    *p.own_type_decl_counts
                        .entry(id_text(&name).to_string())
                        .or_default() += 1;
                }
                // An in-file ABBREVIATION could alias `ExtensionAttribute` (the
                // resolver records the local without chasing its target), so the
                // gate's derivation must know which local names are abbreviations
                // — a committed `Local` of one is a possible extension marker,
                // where a concrete in-file class is its own tycon and provably
                // not (EX-3 §2(d) stage 5).
                if matches!(defn.repr(), Some(TypeDefnRepr::Abbrev(_))) {
                    p.own_abbrev_type_simple_names
                        .insert(id_text(&name).to_string());
                }
            }
        }
        // An `exception E` occupies F#'s *type* namespace too (FCS resolves
        // `[<E>]` to the exception, then errors on its constructor), so exception
        // names join the same guard (codex round 6) — and their own set: an
        // exception is never in `type_defs`, so an in-file *type* hit for a name
        // an exception also declares may be resolving past the closer exception
        // FCS would bind (codex on stage 4).
        for exn in file.syntax().descendants_of::<ExceptionDefnDecl>() {
            if let Some(name) = exn.union_case().and_then(|c| c.ident()) {
                p.own_type_simple_names.insert(id_text(&name).to_string());
                p.own_exception_simple_names
                    .insert(id_text(&name).to_string());
                *p.own_type_decl_counts
                    .entry(id_text(&name).to_string())
                    .or_default() += 1;
            }
        }
        // The custom operations the file's augmentations declare, which extend
        // every builder in every later computation expression, this file's
        // included. Whole-file: F# reads a builder's operations off the completed
        // type.
        let alias_declared =
            preceding.custom_operation_alias_declared || p.declares_custom_operation_alias;
        p.custom_operations = file_custom_operations(file.syntax(), &|s| {
            may_be_custom_operation(s, alias_declared, assemblies)
        });
        // Every *nested module definition*'s simple name in the file, for the
        // dotted half of the project `[<AutoOpen>]` shadow
        // (`Resolver::project_module_named`): what owns a dotted head is a module.
        //
        // Definitions only. A `module X = A.B` **abbreviation** binds a name inside
        // its own container and publishes it nowhere — fsc-verified, an abbreviation
        // in an `[<AutoOpen>] module Auto` leaves a *sibling* module's use of the
        // name at FS0039 — so it can never be the hidden declaration this index
        // stands fop. Within its container it is in scope, but there it is not
        // unmodelled either: `Resolver::module_aliases` resolves same-file
        // abbreviations directly. Everything collected here is a declaration
        // nothing else models, which is what makes a name in it evidence of a real
        // hazard rather than of a name merely occurring.
        //
        // A file's own top-level `module M` header is a `MODULE_OR_NAMESPACE` and
        // so is not collected, which is exactly right for the same reason: the risk
        // this bounds is a declaration nested inside an `[<AutoOpen>]` module, and a
        // top-level header can never be one.
        //
        // A `module private X` **is** collected, though F# does not publish it past
        // its parent either (fsc-verified: a private child of an auto-open module
        // loses `Demo.CasePat.Shape` to the referenced assembly). Inside that parent
        // it is both visible and unmodelled — sema does not model a nested module's
        // members — and this pre-scan is position-blind, so it cannot tell the two
        // sites apart. Keeping it over-defers uses outside the parent; dropping it
        // would wrong-target uses inside.
        //
        // Whole-file and order-independent like the type pre-scan above.
        for nm in file.syntax().descendants_of::<NestedModuleDecl>() {
            // A nested header is written `module X`; `long_id` is dotted only on a
            // malformed one, where the last segment is still the module the body
            // hangs off.
            if let Some(name) = nm.long_id().and_then(|li| li.idents().last()) {
                p.own_module_simple_names.insert(id_text(&name).to_string());
            }
        }
        // The type/exception names declared **directly inside an `[<AutoOpen>]`
        // module** (AO-2): an in-file attribute hit for one of these must defer —
        // the auto-open import contests it positionally in FCS, across blocks the
        // block-scoped shadow set cannot see. Direct children only: a nested
        // plain module's types are not bare-visible through the auto-open, and a
        // nested auto-open module is itself a `NestedModuleDecl` this descendants
        // walk visits. Like every pre-scan above, file-global and
        // order-independent by design.
        for nm in file.syntax().descendants_of::<NestedModuleDecl>() {
            if !attrs_auto_open(nm.attributes()) {
                continue;
            }
            for decl in nm.decls() {
                match decl {
                    ModuleDecl::Types(types) => {
                        for defn in types.defns() {
                            if let Some(name) = defn.long_id().and_then(|li| li.idents().last()) {
                                p.own_auto_open_type_names
                                    .insert(id_text(&name).to_string());
                            }
                        }
                    }
                    ModuleDecl::Exception(exn) => {
                        if let Some(name) = exn.union_case().and_then(|c| c.ident()) {
                            p.own_auto_open_type_names
                                .insert(id_text(&name).to_string());
                        }
                    }
                    _ => {}
                }
            }
        }
        // Every value-binder simple name in the file — the constructor fallback's
        // "not-a-value" oracle ([`Resolver::own_binder_simple_names`]). Every
        // pattern goes through the resolution-independent [`pattern_names`] walk,
        // which yields a binder *before* the resolution stage's provisional/interning
        // drops, so a would-be-shadowed local and a provisional uppercase parameter
        // are both captured. Every pattern node is entered (not just outermost) so no
        // construct `pattern_names` might not recurse can hide a name —
        // over-collection only defers. File-global and order-independent, like the
        // type-name pre-scans above.
        //
        // The empty `argument_expressions` set costs nothing — it makes a
        // parameterised active pattern's argument read as a binder, which is more
        // over-collection of the kind this oracle already accepts.
        p.own_binder_simple_names
            .extend(every_let_pattern_binder_name(
                file.syntax(),
                assemblies.names(),
            ));
        // An `extern` prototype is the one value-namespace binder with no pattern for
        // the walk above to reach: its name is a `LongIdent`, and the decl arm
        // deliberately does not intern it as a usable value. Both halves of the
        // fallback's "not a value" evidence therefore miss it, so it is added to the
        // same oracle directly — otherwise `open Demo; extern int Thing(int x);
        // Thing 1` would name the opened `Demo.Thing` where F# binds the prototype.
        // Union and exception **cases** are bare-visible values with no `Pat` for the
        // walk above to reach: `[<AutoOpen>] module A = exception Thing of int` puts
        // `Thing` in bare expression scope, and a namespace-level union does the same.
        // File-wide and kind-blind, like the binder scan — over-collection only
        // defers, and a case name that never becomes bare-visible costs one deferral.
        for case in file.syntax().descendants_of::<UnionCase>() {
            if let Some(name) = case.ident() {
                p.own_binder_simple_names.insert(id_text(&name).to_string());
            }
        }
        // A query `join`/`groupJoin` range variable (`join Thing in ys on …`) is a
        // value binder written as an *expression*, so the pattern walk cannot reach it
        // eithep. FCS binds the LHS occurrence as the definition and later ones as
        // uses of it. Only the binding position — the LHS — names a binder; the `rhs`
        // is the source collection, an ordinary expression.
        // A query `into` clause (`groupJoin … into Thing`, `group … into Thing`)
        // names a further range variable. `into` lexes as an ordinary ident, so the
        // binder is simply the ident that follows it. Scanned file-wide rather than
        // per node kind: `into` appears under several query operators, not only
        // `JoinInExpr`. A user value actually named `into` would over-collect the
        // following name, which costs one deferral.
        {
            let mut prev_was_into = false;
            for tok in file.syntax().tokens_where(|k| k == SyntaxKind::IDENT_TOK) {
                if prev_was_into {
                    p.own_binder_simple_names.insert(id_text(&tok).to_string());
                }
                prev_was_into = id_text(&tok) == "into";
            }
        }
        for join in file.syntax().descendants_of::<JoinInExpr>() {
            let Some(lhs) = join.lhs() else { continue };
            // Every ident in the binding position, not just a bare one: the `join`
            // keyword itself lexes as an ident, so the LHS is the application
            // `join Thing` rather than the binder alone. Over-collecting (the keyword,
            // a `groupJoin` alias) only ever defers, which is what this oracle is fop.
            for tok in lhs.syntax().tokens_where(|k| k == SyntaxKind::IDENT_TOK) {
                p.own_binder_simple_names.insert(id_text(&tok).to_string());
            }
        }
        for ext in file.syntax().descendants_of::<ExternDecl>() {
            if let Some(last) = ext.name().and_then(|n| n.idents().last()) {
                p.own_binder_simple_names.insert(id_text(&last).to_string());
            }
        }
        p
    }
}

/// Whether the file rooted at `root`, an implementation or a signature,
/// declares a type or an exception that may alias `[<AutoOpen>]` or
/// `[<RequireQualifiedAccess>]` ([`Prescan::declares_core_attribute_alias`]).
pub(super) fn declares_core_attribute_alias(root: &SyntaxNode) -> bool {
    root.descendants_of::<TypeDefn>()
        .any(|defn| type_defn_renames_core_attribute(&defn))
        || root
            .descendants_of::<ExceptionDefnDecl>()
            .any(|exn| exception_named_like_core_attribute(&exn))
}
