//! The **unmodelled-contributor grid**: every route by which F# can put a bare
//! name in front of another without the resolver's walk pushing a scope entry
//! for it at that point, crossed with the kind of name it shadows and where the
//! name is used, each cell graded against FCS.
//!
//! The walk models a binding by pushing an entry where F# adds it to the name
//! environment, and the environment is latest-write-wins
//! (`NameResolution.fs`). A route that adds names somewhere the walk pushes
//! nothing is a *contributor the walk does not model*, and a name it adds
//! takes the slot from whatever the walk would have committed: FSharp.Core's
//! implicitly opened `id` or `Some`, or an earlier same-named binder of the
//! file's own. The routes are enumerated from the places FCS extends the
//! environment (`CheckDeclarations.fs`: `AddLocalSubModule`, `AddLocalTycons`,
//! `AddLocalRootModuleOrNamespace`, `ImplicitlyOpenOwnNamespace`,
//! `OpenModuleOrNamespaceRefs`, `OpenTypeContent`; `NameResolution.fs`:
//! `AddModuleOrNamespaceRefToNameEnv`'s recursive `[<AutoOpen>]` fold,
//! `CanAutoOpenTyconRef`, `AddStaticContentOfTypeToNameEnv`), not from the
//! shapes that review happened to find; [`Route`] lists them, and
//! `docs/unmodelled-contributors.md` is the closed list with each route's
//! source.
//!
//! Each cell is one small project — the route's earlier files, then the probe
//! file — checked by FCS on its own, because a root-level `[<AutoOpen>]` module
//! reaches every later file of a project. Every cell asserts
//! certain-implies-exact against FCS's record at the probe; the unshadowed
//! [`Route::Nothing`] cells must commit; and FCS's answer and ours are pinned
//! per cell in an exact manifest (`tests/manifests/unmodelled_contributor_grid.txt`),
//! so a cell that starts or stops committing — or a route whose premise FCS
//! stops honouring — moves a line.

use std::path::{Path, PathBuf};

use crate::common::corpus_manifest::{check_manifest, regenerate};
use crate::common::{
    FileUses, NormalisedProjectUse, ensure_contributor_fixture_built, ensure_fsharp_core_dll,
    invoke_fcs_dump_project_with_refs, parse_fcs_uses_project, temp_fs_tree,
};
use crate::resolve_signatures::project;
use borzoi_assembly::Ecma335Assembly;
use borzoi_oracle_harness::manifest::Manifest;
use borzoi_sema::{AssemblyEnv, Resolution, ResolvedProject, resolve_project_files};
use rowan::TextRange;

/// What the probed name would bind if no contributor stood in front of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    /// FSharp.Core's implicitly opened `id`.
    ImplicitValue,
    /// A `let` binder of the probe file's own, declared before the contributor.
    OwnValue,
    /// FSharp.Core's implicitly opened union case `Some`.
    ImplicitCase,
    /// A union case of the probe file's own, declared before the contributor.
    OwnCase,
}

impl Kind {
    const ALL: [Kind; 4] = [
        Kind::ImplicitValue,
        Kind::OwnValue,
        Kind::ImplicitCase,
        Kind::OwnCase,
    ];

    fn name(self) -> &'static str {
        match self {
            Kind::ImplicitValue => "id",
            Kind::OwnValue => "gridV",
            Kind::ImplicitCase => "Some",
            Kind::OwnCase => "GridC",
        }
    }

    fn is_case(self) -> bool {
        matches!(self, Kind::ImplicitCase | Kind::OwnCase)
    }

    /// The probe file's own binder of the name, if the kind has one.
    fn own(self) -> Vec<String> {
        match self {
            Kind::OwnValue => vec!["let gridV (x: int) = x * 2".to_string()],
            Kind::OwnCase => vec!["type GridE = GridC of int | GridEOther".to_string()],
            Kind::ImplicitValue | Kind::ImplicitCase => vec![],
        }
    }

    /// A module-level declaration of the name, of the kind's own sort.
    fn member(self) -> Vec<String> {
        let name = self.name();
        if self.is_case() {
            vec![format!("type GridU = {name} of int | GridUOther")]
        } else {
            vec![format!("let {name} (x: int) = x + 1")]
        }
    }

    /// A static member of the name, for a type's surface.
    fn static_member(self) -> Vec<String> {
        vec![format!("static member {}(x: int) = x + 1", self.name())]
    }
}

/// How the probe uses the name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Use {
    /// `let probe = N 1`, in the block the contributor reaches.
    Applied,
    /// `let probe = List.map N [ 1 ]`: the name as a first-class value.
    FunctionValue,
    /// `let probe () = N 1`: under a local scope.
    Local,
    /// `module GridUse = let probe = N 1`: in a nested module.
    Nested,
    /// `let probe x = match x with N _ -> 1 | _ -> 0`: a pattern head.
    Pattern,
    /// `let probe = N.GetType ()`: the head of a dotted path.
    Head,
    /// `let probe = gridB { N 1 }`: a computation expression's statement.
    CeStatement,
    /// `let probe = gridB { return N 1 }`: inside a computation expression.
    CeReturn,
}

impl Use {
    const ALL: [Use; 8] = [
        Use::Applied,
        Use::Head,
        Use::FunctionValue,
        Use::Local,
        Use::Nested,
        Use::Pattern,
        Use::CeStatement,
        Use::CeReturn,
    ];

    fn applies_to(self, kind: Kind, route: Route) -> bool {
        let in_ce = matches!(self, Use::CeStatement | Use::CeReturn);
        if route.probes_itself() {
            return self == Use::Applied;
        }
        (self != Use::Pattern || kind.is_case()) && in_ce == route.is_builder()
    }

    /// The use's lines, unindented, with `@N` where the probed name goes.
    fn lines(self) -> Vec<&'static str> {
        match self {
            Use::Applied => vec!["let probe = @N 1"],
            Use::FunctionValue => vec!["let probe = List.map @N [ 1 ]"],
            Use::Local => vec!["let probe () = @N 1"],
            Use::Nested => vec!["module GridUse =", "    let probe = @N 1"],
            Use::Pattern => vec!["let probe x = match x with @N _ -> 1 | _ -> 0"],
            Use::Head => vec!["let probe = @N.GetType ()"],
            Use::CeStatement => vec!["let probe = gridB { @N 1 }"],
            Use::CeReturn => vec!["let probe = gridB { return @N 1 }"],
        }
    }
}

/// One route by which a bare name can reach the probe. The list is the closed
/// enumeration in `docs/unmodelled-contributors.md`; routes the walk does model
/// are here too, because "modelled" is exactly the claim each cell checks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Route {
    /// No contributor: the shadowed reading is the only one.
    Nothing,
    /// A same-file `[<AutoOpen>]` module before the use (`AddLocalSubModule`).
    AutoOpenModule,
    /// The attribute spelled through a same-file abbreviation.
    AutoOpenAliased,
    /// The attribute spelled fully qualified, with its `Attribute` suffix.
    AutoOpenQualified,
    /// The attribute sharing a list with another.
    AutoOpenInList,
    /// An `[<AutoOpen>]` module holding a single-case union written without a
    /// bar (`type GridU = N`), which the syntax reads as an abbreviation.
    AutoOpenBarlessUnion,
    /// An `[<AutoOpen>]` module inside an `[<AutoOpen>]` module (the recursive
    /// fold of `AddModuleOrNamespaceRefToNameEnv`).
    AutoOpenNested,
    /// A `private` `[<AutoOpen>]` module.
    AutoOpenPrivate,
    /// An `[<AutoOpen>]` module inside a plain one that the file then opens.
    AutoOpenUnderOpenedModule,
    /// An `[<AutoOpen>]` module declared after the use: it reaches nothing.
    AutoOpenAfterUse,
    /// The name only inside a plain module nested in an `[<AutoOpen>]` one: not
    /// folded.
    AutoOpenHidesNested,
    /// A same-file `[<AutoOpen>]` type with a static of the name
    /// (`CanAutoOpenTyconRef`, `AddStaticContentOfTypeToNameEnv`).
    AutoOpenType,
    /// A generic `[<AutoOpen>]` type: `CanAutoOpenTyconRef` refuses it.
    AutoOpenGenericType,
    /// An `[<AutoOpen>]` abbreviation of a referenced type, whose statics are
    /// spelled nowhere in the file.
    AutoOpenTypeAbbrev,
    /// An `[<AutoOpen>]` class inheriting a referenced class's statics.
    AutoOpenTypeInherits,
    /// An `[<AutoOpen>]` type inside an `[<AutoOpen>]` module.
    AutoOpenTypeInModule,
    /// An earlier file's root-level `[<AutoOpen>] module`
    /// (`AddLocalRootModuleOrNamespace`).
    EarlierRootModule,
    /// An earlier file's `[<AutoOpen>]` module in `namespace global`.
    EarlierGlobalModule,
    /// An earlier file's `[<AutoOpen>]` type in `namespace global`.
    EarlierGlobalType,
    /// An earlier file's `[<AutoOpen>]` abbreviation in `namespace global`.
    EarlierGlobalTypeAbbrev,
    /// An earlier file's root-level `[<AutoOpen>]` module behind a signature.
    EarlierRootModuleSigned,
    /// An earlier file's root-level module whose signature alone says
    /// `[<AutoOpen>]`: the signature's attributes are the ones later files see.
    EarlierRootModuleSigOnly,
    /// The same for a module in a namespace the probe file opens.
    EarlierNamespaceSigOnly,
    /// The same for a module nested in a module the probe file opens.
    EarlierTopSigOnly,
    /// An earlier file's root-level module whose implementation alone says
    /// `[<AutoOpen>]`.
    EarlierRootModuleImplOnly,
    /// An earlier file's `namespace global` declaring the name directly: a
    /// union case, or a constructible type.
    EarlierGlobalDecl,
    /// An earlier file's plain top-level module holding an `[<AutoOpen>]`
    /// module, not opened: it reaches nothing.
    EarlierTopUnopened,
    /// The same, opened by the probe file (`OpenModuleOrNamespaceRefs`).
    EarlierTopOpened,
    /// An earlier file's `[<AutoOpen>]` module in the probe file's own
    /// namespace (`ImplicitlyOpenOwnNamespace`).
    EarlierNamespaceOwn,
    /// The same namespace, opened from another.
    EarlierNamespaceOpened,
    /// The same, with the attribute spelled through an abbreviation.
    EarlierNamespaceAliased,
    /// An `[<AutoOpen>]` module nested in one, in an opened namespace.
    EarlierNamespaceNested,
    /// An `[<AutoOpen>]` type in an opened namespace.
    EarlierNamespaceType,
    /// An `[<AutoOpen>]` abbreviation in an opened namespace.
    EarlierNamespaceTypeAbbrev,
    /// A namespace's module holding an `[<AutoOpen>]` module, opened by path.
    EarlierNamespaceModuleOpened,
    /// An assembly-level `[<assembly: AutoOpen>]` of the project's own: it
    /// applies to referenced assemblies only (`AddCcuToTcEnv`).
    OwnAssemblyAutoOpen,
    /// An `[<AutoOpen>]` module in the same namespace block as the use.
    NamespaceSameBlock,
    /// An `[<AutoOpen>]` module in an earlier block of the same namespace in
    /// the same file.
    NamespaceEarlierBlock,
    /// A `module rec` whose binder of the name follows the use.
    RecForward,
    /// The same, with an `open type` supplying the name before the use: in a
    /// `rec` block, every binder outranks every `open`.
    RecForwardOverOpen,
    /// A `module rec` whose `[<AutoOpen>]` module follows the use.
    RecAutoOpenAfter,
    /// A `namespace rec` whose `[<AutoOpen>]` module follows the use.
    NamespaceRecAutoOpenAfter,
    /// `open type` of a referenced type declaring the static.
    OpenTypeDeclared,
    /// `open type` of a referenced type inheriting the static.
    OpenTypeInherited,
    /// `open type` of a referenced type whose static the projection drops.
    OpenTypeDropped,
    /// A constructible type of the name.
    ConstructibleType,
    /// A record of the name that a `StructAttribute` spelled through an
    /// abbreviation makes a struct, in an `[<AutoOpen>]` module: a struct
    /// takes the slot.
    AutoOpenAliasedStructRecord,
    /// The same record, declared directly in the probe's module.
    AliasedStructRecord,
    /// An abbreviation of a constructible type, named so.
    ConstructibleAbbrev,
    /// An earlier file's constructible type of the name in the probe file's
    /// own namespace.
    EarlierNamespaceCtor,
    /// The same, opened from another namespace.
    EarlierNamespaceCtorOpened,
    /// An earlier file's abbreviation of a constructible type, named so, in
    /// the probe file's own namespace.
    EarlierNamespaceCtorAbbrev,
    /// A union of the name's case whose `[<RequireQualifiedAccess>]` is
    /// spelled through an abbreviation: its cases are not bare.
    RqaAliased,
    /// The same in an earlier file's namespace, opened.
    EarlierNamespaceRqaAliased,
    /// An `extern` prototype of the name.
    Extern,
    /// An exception of the name.
    Exception,
    /// An active pattern whose case is the name.
    ActivePattern,
    /// A computation-expression builder with a custom operation of the name,
    /// used inside its expression.
    CeCustomOperation,
    /// A builder with no custom operation of the name.
    CeNoOperation,
    /// An intrinsic augmentation of an `[<AutoOpen>]` type, adding the
    /// static: FCS folds the completed type.
    AutoOpenTypeAugmented,
    /// The same, in an `[<AutoOpen>]` module.
    AutoOpenTypeAugmentedInModule,
    /// A member parameter of the name, used in its body, in a type group
    /// whose later `and` type is named so: the group's types enter scope
    /// before its members' bodies, so the parameter holds.
    TypeGroupParameter,
    /// The same with a local `let` in the member body.
    TypeGroupLocalLet,
    /// The same local heading a dotted path.
    TypeGroupLocalHead,
    /// A value heading a dotted path, after a same-named enum, and after an
    /// `[<AutoOpen>]` type that spells the name only as a parameter: the
    /// value keeps the qualifier, whatever the contributor's mark says.
    EnumQualifierAfterMark,
    /// A contributor's value heading a dotted path, after a later record of
    /// the name with a static of the tail: a record never takes the slot, so
    /// the value keeps the qualifier.
    RecordQualifierAfterMark,
    /// In a `module rec`, a type group's member body heading a dotted path
    /// with a name the group declares as a type and a later `[<AutoOpen>]`
    /// module declares as a value: modules enter after the group's types.
    RecTypeGroupQualifier,
    /// A static property of an `[<AutoOpen>]` type brings its accessor
    /// `get_GridP` into bare scope too, in front of an earlier binder of that
    /// name.
    AutoOpenTypeAccessor,
    /// An operator-named union case (`([])`, compiled `op_Nil`) of an
    /// `[<AutoOpen>]` module, in front of an earlier binder of that name.
    AutoOpenOperatorCase,
}

impl Route {
    const ALL: [Route; 69] = [
        Route::Nothing,
        Route::AutoOpenModule,
        Route::AutoOpenAliased,
        Route::AutoOpenQualified,
        Route::AutoOpenInList,
        Route::AutoOpenBarlessUnion,
        Route::AutoOpenNested,
        Route::AutoOpenPrivate,
        Route::AutoOpenUnderOpenedModule,
        Route::AutoOpenAfterUse,
        Route::AutoOpenHidesNested,
        Route::AutoOpenType,
        Route::AutoOpenGenericType,
        Route::AutoOpenTypeAbbrev,
        Route::AutoOpenTypeInherits,
        Route::AutoOpenTypeInModule,
        Route::EarlierRootModule,
        Route::EarlierGlobalModule,
        Route::EarlierGlobalType,
        Route::EarlierGlobalTypeAbbrev,
        Route::EarlierRootModuleSigned,
        Route::EarlierRootModuleSigOnly,
        Route::EarlierNamespaceSigOnly,
        Route::EarlierTopSigOnly,
        Route::EarlierRootModuleImplOnly,
        Route::EarlierGlobalDecl,
        Route::EarlierTopUnopened,
        Route::EarlierTopOpened,
        Route::EarlierNamespaceOwn,
        Route::EarlierNamespaceOpened,
        Route::EarlierNamespaceAliased,
        Route::EarlierNamespaceNested,
        Route::EarlierNamespaceType,
        Route::EarlierNamespaceTypeAbbrev,
        Route::EarlierNamespaceModuleOpened,
        Route::OwnAssemblyAutoOpen,
        Route::NamespaceSameBlock,
        Route::NamespaceEarlierBlock,
        Route::RecForward,
        Route::RecForwardOverOpen,
        Route::RecAutoOpenAfter,
        Route::NamespaceRecAutoOpenAfter,
        Route::OpenTypeDeclared,
        Route::OpenTypeInherited,
        Route::OpenTypeDropped,
        Route::ConstructibleType,
        Route::AutoOpenAliasedStructRecord,
        Route::AliasedStructRecord,
        Route::ConstructibleAbbrev,
        Route::EarlierNamespaceCtor,
        Route::EarlierNamespaceCtorOpened,
        Route::EarlierNamespaceCtorAbbrev,
        Route::RqaAliased,
        Route::EarlierNamespaceRqaAliased,
        Route::Extern,
        Route::Exception,
        Route::ActivePattern,
        Route::CeCustomOperation,
        Route::CeNoOperation,
        Route::AutoOpenTypeAugmented,
        Route::AutoOpenTypeAugmentedInModule,
        Route::TypeGroupParameter,
        Route::TypeGroupLocalLet,
        Route::TypeGroupLocalHead,
        Route::EnumQualifierAfterMark,
        Route::RecordQualifierAfterMark,
        Route::RecTypeGroupQualifier,
        Route::AutoOpenTypeAccessor,
        Route::AutoOpenOperatorCase,
    ];

    /// Whether the route adds nothing the probe could bind — FCS's answer is
    /// the unshadowed one, and the walk must commit it.
    fn adds_nothing(self) -> bool {
        matches!(
            self,
            Route::Nothing
                | Route::AutoOpenAfterUse
                | Route::AutoOpenHidesNested
                | Route::AutoOpenGenericType
                | Route::AutoOpenTypeInherits
                | Route::OpenTypeInherited
                | Route::OwnAssemblyAutoOpen
                | Route::EarlierTopUnopened
                | Route::TypeGroupParameter
                | Route::TypeGroupLocalLet
                | Route::TypeGroupLocalHead
        )
    }

    /// Whether the route writes its probe itself (`@P`), so it has one use.
    fn probes_itself(self) -> bool {
        matches!(
            self,
            Route::TypeGroupParameter
                | Route::TypeGroupLocalLet
                | Route::TypeGroupLocalHead
                | Route::EnumQualifierAfterMark
                | Route::RecordQualifierAfterMark
                | Route::RecTypeGroupQualifier
                | Route::AutoOpenTypeAccessor
                | Route::AutoOpenOperatorCase
        )
    }

    /// Whether the route declares the `gridB` builder its uses run inside.
    fn is_builder(self) -> bool {
        matches!(self, Route::CeCustomOperation | Route::CeNoOperation)
    }

    /// Whether the route can declare a name of `kind` at all: F# requires an
    /// exception or active-pattern case to be capitalised.
    fn applies_to(self, kind: Kind) -> bool {
        match self {
            Route::Exception
            | Route::ActivePattern
            | Route::RqaAliased
            | Route::EarlierNamespaceRqaAliased
            | Route::AutoOpenBarlessUnion => kind.is_case(),
            // A capitalised parameter is a pattern, not a binder.
            Route::TypeGroupParameter
            | Route::TypeGroupLocalLet
            | Route::TypeGroupLocalHead
            | Route::EnumQualifierAfterMark
            | Route::RecordQualifierAfterMark
            | Route::RecTypeGroupQualifier => !kind.is_case(),
            // The probe is a fixed name, not the kind's: one kind runs it.
            Route::AutoOpenTypeAccessor | Route::AutoOpenOperatorCase => kind == Kind::OwnValue,
            _ => true,
        }
    }

    /// The project: earlier files as `(relative path, template)`, then the
    /// probe file's template. Templates mark where the kind's lines go with a
    /// line holding only `@OWN` (the kind's own binder), `@USE` (the use),
    /// `@MEMBER` (a module-level declaration of the name) or `@STATIC` (a
    /// static member of it), indented as the lines should be.
    fn templates(self) -> (Vec<(&'static str, &'static str)>, &'static str) {
        const PLAIN: &str = "module GridProbe\n@OWN\n@USE\n";
        match self {
            Route::Nothing => (vec![], PLAIN),
            Route::AutoOpenModule => (
                vec![],
                "module GridProbe\n@OWN\n[<AutoOpen>]\nmodule GridAuto =\n    @MEMBER\n@USE\n",
            ),
            Route::AutoOpenAliased => (
                vec![],
                "module GridProbe\ntype GridAO = AutoOpenAttribute\n@OWN\n[<GridAO>]\nmodule GridAuto =\n    @MEMBER\n@USE\n",
            ),
            Route::AutoOpenQualified => (
                vec![],
                "module GridProbe\n@OWN\n[<Microsoft.FSharp.Core.AutoOpenAttribute>]\nmodule GridAuto =\n    @MEMBER\n@USE\n",
            ),
            Route::AutoOpenInList => (
                vec![],
                "module GridProbe\n@OWN\n[<CompilationRepresentation(CompilationRepresentationFlags.ModuleSuffix); AutoOpen>]\nmodule GridAuto =\n    @MEMBER\n@USE\n",
            ),
            Route::AutoOpenBarlessUnion => (
                vec![],
                "module GridProbe\n@OWN\n[<AutoOpen>]\nmodule GridAuto =\n    type GridU = @NAME\n@USE\n",
            ),
            Route::AutoOpenNested => (
                vec![],
                "module GridProbe\n@OWN\n[<AutoOpen>]\nmodule GridAuto =\n    [<AutoOpen>]\n    module GridDeep =\n        @MEMBER\n@USE\n",
            ),
            Route::AutoOpenPrivate => (
                vec![],
                "module GridProbe\n@OWN\n[<AutoOpen>]\nmodule private GridAuto =\n    @MEMBER\n@USE\n",
            ),
            Route::AutoOpenUnderOpenedModule => (
                vec![],
                "module GridProbe\n@OWN\nmodule GridPlain =\n    [<AutoOpen>]\n    module GridAuto =\n        @MEMBER\nopen GridPlain\n@USE\n",
            ),
            Route::AutoOpenAfterUse => (
                vec![],
                "module GridProbe\n@OWN\n@USE\n[<AutoOpen>]\nmodule GridAuto =\n    @MEMBER\n",
            ),
            Route::AutoOpenHidesNested => (
                vec![],
                "module GridProbe\n@OWN\n[<AutoOpen>]\nmodule GridAuto =\n    module GridHidden =\n        @MEMBER\n@USE\n",
            ),
            Route::AutoOpenType => (
                vec![],
                "module GridProbe\n@OWN\n[<AutoOpen>]\ntype GridT =\n    @STATIC\n@USE\n",
            ),
            Route::AutoOpenGenericType => (
                vec![],
                "module GridProbe\n@OWN\n[<AutoOpen>]\ntype GridT<'a> =\n    @STATIC\n@USE\n",
            ),
            Route::AutoOpenTypeAbbrev => (
                vec![],
                "module GridProbe\n@OWN\n[<AutoOpen>]\ntype GridT = GridFix.Holder\n@USE\n",
            ),
            Route::AutoOpenTypeInherits => (
                vec![],
                "module GridProbe\n@OWN\n[<AutoOpen>]\ntype GridT() =\n    inherit GridFix.Base()\n@USE\n",
            ),
            Route::AutoOpenTypeInModule => (
                vec![],
                "module GridProbe\n@OWN\n[<AutoOpen>]\nmodule GridAuto =\n    [<AutoOpen>]\n    type GridT =\n        @STATIC\n@USE\n",
            ),
            Route::EarlierRootModule => (
                vec![("GridRoot.fs", "[<AutoOpen>]\nmodule GridRoot\n@MEMBER\n")],
                PLAIN,
            ),
            Route::EarlierGlobalModule => (
                vec![(
                    "GridRoot.fs",
                    "namespace global\n[<AutoOpen>]\nmodule GridRoot =\n    @MEMBER\n",
                )],
                PLAIN,
            ),
            Route::EarlierGlobalType => (
                vec![(
                    "GridRoot.fs",
                    "namespace global\n[<AutoOpen>]\ntype GridRootT =\n    @STATIC\n",
                )],
                PLAIN,
            ),
            Route::EarlierGlobalTypeAbbrev => (
                vec![(
                    "GridRoot.fs",
                    "namespace global\n[<AutoOpen>]\ntype GridRootT = GridFix.Holder\n",
                )],
                PLAIN,
            ),
            Route::EarlierRootModuleSigned => (
                vec![
                    ("GridRoot.fsi", "[<AutoOpen>]\nmodule GridRoot\n@SIG\n"),
                    ("GridRoot.fs", "[<AutoOpen>]\nmodule GridRoot\n@MEMBER\n"),
                ],
                PLAIN,
            ),
            Route::EarlierRootModuleSigOnly => (
                vec![
                    ("GridRoot.fsi", "[<AutoOpen>]\nmodule GridRoot\n@SIG\n"),
                    ("GridRoot.fs", "module GridRoot\n@MEMBER\n"),
                ],
                PLAIN,
            ),
            Route::EarlierNamespaceSigOnly => (
                vec![
                    (
                        "GridNs.fsi",
                        "namespace GridNs\n[<AutoOpen>]\nmodule GridAuto =\n    @SIG\n",
                    ),
                    (
                        "GridNs.fs",
                        "namespace GridNs\nmodule GridAuto =\n    @MEMBER\n",
                    ),
                ],
                "module GridProbe\n@OWN\nopen GridNs\n@USE\n",
            ),
            Route::EarlierTopSigOnly => (
                vec![
                    (
                        "GridTop.fsi",
                        "module GridTop\n[<AutoOpen>]\nmodule GridAuto =\n    @SIG\n",
                    ),
                    (
                        "GridTop.fs",
                        "module GridTop\nmodule GridAuto =\n    @MEMBER\n",
                    ),
                ],
                "module GridProbe\n@OWN\nopen GridTop\n@USE\n",
            ),
            Route::EarlierRootModuleImplOnly => (
                vec![
                    ("GridRoot.fsi", "module GridRoot\n@SIG\n"),
                    ("GridRoot.fs", "[<AutoOpen>]\nmodule GridRoot\n@MEMBER\n"),
                ],
                PLAIN,
            ),
            Route::EarlierGlobalDecl => (vec![("GridRoot.fs", "namespace global\n@DECL\n")], PLAIN),
            Route::EarlierTopUnopened => (
                vec![(
                    "GridTop.fs",
                    "module GridTop\n[<AutoOpen>]\nmodule GridAuto =\n    @MEMBER\n",
                )],
                PLAIN,
            ),
            Route::EarlierTopOpened => (
                vec![(
                    "GridTop.fs",
                    "module GridTop\n[<AutoOpen>]\nmodule GridAuto =\n    @MEMBER\n",
                )],
                "module GridProbe\n@OWN\nopen GridTop\n@USE\n",
            ),
            Route::EarlierNamespaceOwn => (
                vec![(
                    "GridNs.fs",
                    "namespace GridNs\n[<AutoOpen>]\nmodule GridAuto =\n    @MEMBER\n",
                )],
                "namespace GridNs\nmodule GridProbe =\n    @OWN\n    @USE\n",
            ),
            Route::EarlierNamespaceOpened => (
                vec![(
                    "GridNs.fs",
                    "namespace GridNs\n[<AutoOpen>]\nmodule GridAuto =\n    @MEMBER\n",
                )],
                "module GridProbe\n@OWN\nopen GridNs\n@USE\n",
            ),
            Route::EarlierNamespaceAliased => (
                vec![(
                    "GridNs.fs",
                    "namespace GridNs\ntype GridAO = AutoOpenAttribute\n[<GridAO>]\nmodule GridAuto =\n    @MEMBER\n",
                )],
                "module GridProbe\n@OWN\nopen GridNs\n@USE\n",
            ),
            Route::EarlierNamespaceNested => (
                vec![(
                    "GridNs.fs",
                    "namespace GridNs\n[<AutoOpen>]\nmodule GridAuto =\n    [<AutoOpen>]\n    module GridDeep =\n        @MEMBER\n",
                )],
                "module GridProbe\n@OWN\nopen GridNs\n@USE\n",
            ),
            Route::EarlierNamespaceType => (
                vec![(
                    "GridNs.fs",
                    "namespace GridNs\n[<AutoOpen>]\ntype GridT =\n    @STATIC\n",
                )],
                "module GridProbe\n@OWN\nopen GridNs\n@USE\n",
            ),
            Route::EarlierNamespaceTypeAbbrev => (
                vec![(
                    "GridNs.fs",
                    "namespace GridNs\n[<AutoOpen>]\ntype GridT = GridFix.Holder\n",
                )],
                "module GridProbe\n@OWN\nopen GridNs\n@USE\n",
            ),
            Route::EarlierNamespaceModuleOpened => (
                vec![(
                    "GridNs.fs",
                    "namespace GridNs\nmodule GridTop =\n    [<AutoOpen>]\n    module GridAuto =\n        @MEMBER\n",
                )],
                "module GridProbe\n@OWN\nopen GridNs.GridTop\n@USE\n",
            ),
            Route::OwnAssemblyAutoOpen => (
                vec![
                    (
                        "GridAsmNs.fs",
                        "namespace GridAsmNs\nmodule GridM =\n    @MEMBER\n",
                    ),
                    (
                        "GridAsm.fs",
                        "module GridAsm\n[<assembly: AutoOpen(\"GridAsmNs.GridM\")>]\ndo ()\n",
                    ),
                ],
                PLAIN,
            ),
            Route::NamespaceSameBlock => (
                vec![],
                "namespace GridNs\n[<AutoOpen>]\nmodule GridAuto =\n    @MEMBER\nmodule GridProbe =\n    @OWN\n    @USE\n",
            ),
            Route::NamespaceEarlierBlock => (
                vec![],
                "namespace GridNs\n[<AutoOpen>]\nmodule GridAuto =\n    @MEMBER\nnamespace GridNs\nmodule GridProbe =\n    @OWN\n    @USE\n",
            ),
            Route::RecForward => (
                vec![],
                "module GridProbe\n@OWN\nmodule rec GridRec =\n    @USE\n    @MEMBER\n",
            ),
            Route::RecForwardOverOpen => (
                vec![],
                "module GridProbe\n@OWN\nmodule rec GridRec =\n    open type GridFix.Holder\n    @USE\n    @MEMBER\n",
            ),
            Route::RecAutoOpenAfter => (
                vec![],
                "module rec GridProbe\n@OWN\n@USE\n[<AutoOpen>]\nmodule GridAuto =\n    @MEMBER\n",
            ),
            Route::NamespaceRecAutoOpenAfter => (
                vec![],
                "namespace rec GridNs\nmodule GridProbe =\n    @OWN\n    @USE\n[<AutoOpen>]\nmodule GridAuto =\n    @MEMBER\n",
            ),
            Route::OpenTypeDeclared => (
                vec![],
                "module GridProbe\n@OWN\nopen type GridFix.Holder\n@USE\n",
            ),
            Route::OpenTypeInherited => (
                vec![],
                "module GridProbe\n@OWN\nopen type GridFix.Derived\n@USE\n",
            ),
            Route::OpenTypeDropped => (
                vec![],
                "module GridProbe\n@OWN\nopen type GridFix.Dropped\n@USE\n",
            ),
            Route::AutoOpenAliasedStructRecord => (
                vec![],
                "module GridProbe\n@OWN\ntype GridS = StructAttribute\n[<AutoOpen>]\nmodule GridAuto =\n    [<GridS>]\n    type @NAME = { GridF: int }\n@USE\n",
            ),
            Route::AliasedStructRecord => (
                vec![],
                "module GridProbe\n@OWN\ntype GridS = StructAttribute\n[<GridS>]\ntype @NAME = { GridF: int }\n@USE\n",
            ),
            Route::ConstructibleType => (vec![], "module GridProbe\n@OWN\n@CTOR\n@USE\n"),
            Route::ConstructibleAbbrev => (vec![], "module GridProbe\n@OWN\n@OBJ\n@USE\n"),
            Route::EarlierNamespaceCtor => (
                vec![("GridNs.fs", "namespace GridNs\n@CTOR\n")],
                "namespace GridNs\nmodule GridProbe =\n    @OWN\n    @USE\n",
            ),
            Route::EarlierNamespaceCtorOpened => (
                vec![("GridNs.fs", "namespace GridNs\n@CTOR\n")],
                "module GridProbe\n@OWN\nopen GridNs\n@USE\n",
            ),
            Route::EarlierNamespaceCtorAbbrev => (
                vec![("GridNs.fs", "namespace GridNs\n@OBJ\n")],
                "namespace GridNs\nmodule GridProbe =\n    @OWN\n    @USE\n",
            ),
            Route::RqaAliased => (
                vec![],
                "module GridProbe\ntype GridRQA = RequireQualifiedAccessAttribute\n@OWN\n[<GridRQA>]\n@MEMBER\n@USE\n",
            ),
            Route::EarlierNamespaceRqaAliased => (
                vec![(
                    "GridNs.fs",
                    "namespace GridNs\ntype GridRQA = RequireQualifiedAccessAttribute\n[<GridRQA>]\n@MEMBER\n",
                )],
                "module GridProbe\n@OWN\nopen GridNs\n@USE\n",
            ),
            Route::Extern => (
                vec![],
                "module GridProbe\n@OWN\n[<System.Runtime.InteropServices.DllImport(\"grid\")>]\n@EXTERN\n@USE\n",
            ),
            Route::Exception => (vec![], "module GridProbe\n@OWN\n@EXN\n@USE\n"),
            Route::CeCustomOperation => (
                vec![],
                "module GridProbe\n@OWN\ntype GridB() =\n    member _.Yield(()) = 0\n    member _.Return(_: 'a) = 0\n    [<CustomOperation(\"@NAME\")>]\n    member _.GridOp(state: int, v: int) = state + v\nlet gridB = GridB()\n@USE\n",
            ),
            Route::AutoOpenTypeAugmented => (
                vec![],
                "module GridProbe\n@OWN\n[<AutoOpen>]\ntype GridT = class end\ntype GridT with\n    @STATIC\n@USE\n",
            ),
            Route::AutoOpenTypeAugmentedInModule => (
                vec![],
                "module GridProbe\n@OWN\n[<AutoOpen>]\nmodule GridAuto =\n    [<AutoOpen>]\n    type GridT = class end\n    type GridT with\n        @STATIC\n@USE\n",
            ),
            Route::TypeGroupParameter => (
                vec![],
                "module GridProbe\n@OWN\ntype GridM() =\n    member _.M(@NAME: int) = @P + 1\nand @NAME() =\n    member _.X = 0\n",
            ),
            Route::TypeGroupLocalLet => (
                vec![],
                "module GridProbe\n@OWN\ntype GridM() =\n    member _.M() =\n        let @NAME (x: int) = x\n        @P 1\nand @NAME() =\n    member _.X = 0\n",
            ),
            Route::TypeGroupLocalHead => (
                vec![],
                "module GridProbe\n@OWN\ntype GridM() =\n    member _.M() =\n        let @NAME = \"local\"\n        @P.Length\nand @NAME() =\n    member _.X = 0\n",
            ),
            Route::EnumQualifierAfterMark => (
                vec![],
                "module GridProbe\ntype @NAME = GridRed = 0\nlet @NAME = {| GridRed = 42 |}\n[<AutoOpen>]\ntype GridT =\n    static member F(@NAME: int) = @NAME\nlet probe = @P.GridRed\n",
            ),
            Route::RecordQualifierAfterMark => (
                vec![],
                "module GridProbe\n[<AutoOpen>]\nmodule GridAuto =\n    let @NAME = \"contributed\"\ntype @NAME =\n    { GridF: int }\n    static member Length = 0\nlet probe = @P.Length\n",
            ),
            Route::RecTypeGroupQualifier => (
                vec![],
                "module rec GridProbe\ntype @NAME() =\n    static member Length = 0\nand GridX() =\n    static member Test() = @P.Length\n[<AutoOpen>]\nmodule GridAuto =\n    let @NAME = \"contributed\"\n",
            ),
            Route::AutoOpenTypeAccessor => (
                vec![],
                "module GridProbe\nlet get_GridP () = 1\n[<AutoOpen>]\ntype GridT =\n    static member GridP = 42\nlet probe = @P{get_GridP} ()\n",
            ),
            Route::AutoOpenOperatorCase => (
                vec![],
                "module GridProbe\nlet op_Nil = 0\n[<AutoOpen>]\nmodule GridAuto =\n    type GridL =\n        | ([])\n        | (::) of int * GridL\nlet probe = @P{op_Nil}\n",
            ),
            Route::CeNoOperation => (
                vec![],
                "module GridProbe\n@OWN\ntype GridB() =\n    member _.Yield(()) = 0\n    member _.Return(_: 'a) = 0\nlet gridB = GridB()\n@USE\n",
            ),
            Route::ActivePattern => (vec![], "module GridProbe\n@OWN\n@AP\n@USE\n"),
        }
    }
}

/// Expand a template: each marker line becomes the kind's lines at the
/// marker's indentation. Returns the text and, for the probe file, the span of
/// the probed name.
fn expand(template: &str, kind: Kind, use_: Use) -> (String, Option<TextRange>) {
    let name = kind.name();
    let mut out = String::new();
    let mut probe = None;
    for line in template.lines() {
        let indent = &line[..line.len() - line.trim_start().len()];
        let lines: Vec<String> = match line.trim() {
            "@OWN" => kind.own(),
            "@MEMBER" => kind.member(),
            "@STATIC" => kind.static_member(),
            "@SIG" => {
                if kind.is_case() {
                    kind.member()
                } else {
                    vec![format!("val {name}: x: int -> int")]
                }
            }
            "@CTOR" => vec![format!("type {name}(x: int) = member _.X = x")],
            "@OBJ" => vec![format!("type {name} = System.Object")],
            "@DECL" => {
                if kind.is_case() {
                    kind.member()
                } else {
                    vec![format!("type {name}(x: int) = member _.X = x")]
                }
            }
            "@EXTERN" => vec![format!("extern int {name}(int x)")],
            "@EXN" => vec![format!("exception {name} of int")],
            "@AP" => vec![format!(
                "let (|{name}|_|) (x: int) = if x > 0 then Option.Some x else Option.None"
            )],
            "@USE" => {
                let mut lines = Vec::new();
                for l in use_.lines() {
                    if let Some(at) = l.find("@N") {
                        let start = out.len()
                            + lines
                                .iter()
                                .map(|l: &String| indent.len() + l.len() + 1)
                                .sum::<usize>()
                            + indent.len()
                            + at;
                        probe = Some(TextRange::new(
                            u32::try_from(start).unwrap().into(),
                            u32::try_from(start + name.len()).unwrap().into(),
                        ));
                    }
                    lines.push(l.replace("@N", name));
                }
                lines
            }
            _ if line.contains("@P") => {
                // A probe written into the route's own text.
                // `@P{text}` probes `text` rather than the kind's name.
                let text = line.trim_start().replace("@NAME", name);
                let at = text.find("@P").expect("contains @P");
                let (probed, rest) = match text[at + 2..].strip_prefix('{') {
                    Some(braced) => {
                        let end = braced.find('}').expect("closing brace");
                        (braced[..end].to_string(), at + 2 + end + 2)
                    }
                    None => (name.to_string(), at + 2),
                };
                let start = out.len() + indent.len() + at;
                probe = Some(TextRange::new(
                    u32::try_from(start).unwrap().into(),
                    u32::try_from(start + probed.len()).unwrap().into(),
                ));
                vec![format!("{}{probed}{}", &text[..at], &text[rest..])]
            }
            _ => vec![line.trim_start().replace("@NAME", name)],
        };
        for l in lines {
            out.push_str(indent);
            out.push_str(&l);
            out.push('\n');
        }
    }
    (out, probe)
}

/// One generated cell.
struct Cell {
    label: String,
    /// `(relative path, source)` in Compile order, the probe file last.
    files: Vec<(String, String)>,
    probe: TextRange,
    /// No contributor reaches the probe ([`Route::adds_nothing`]): we must
    /// commit.
    control: bool,
    /// A cell of the one wrong-answer class the walk leaves standing
    /// ([`is_known_wrong`]).
    known_wrong: bool,
}

/// The project's own binding of a name used inside a computation expression,
/// whose builder may have a custom operation of the name. The walk withholds
/// only the names an implicit open supplies there: withholding the project's
/// too costs every parameter used in an `async { … }` (320 of the 22,247
/// matches `resolve_corpus_diff` grades), so this class is pinned rather than
/// fixed — the manifest records each cell's verdict, and the WRONG check
/// skips exactly these (`docs/unmodelled-contributors.md`, "Not on the
/// list").
fn is_known_wrong(kind: Kind, route: Route) -> bool {
    matches!(kind, Kind::OwnValue | Kind::OwnCase) && route.is_builder()
}

fn cells() -> Vec<Cell> {
    let mut cells = Vec::new();
    for kind in Kind::ALL {
        for route in Route::ALL {
            if !route.applies_to(kind) {
                continue;
            }
            for use_ in Use::ALL {
                if !use_.applies_to(kind, route) {
                    continue;
                }
                let (earlier, probe_template) = route.templates();
                let mut files: Vec<(String, String)> = earlier
                    .iter()
                    .map(|(path, t)| ((*path).to_string(), expand(t, kind, use_).0))
                    .collect();
                let (src, probe) = expand(probe_template, kind, use_);
                files.push(("GridProbe.fs".to_string(), src));
                cells.push(Cell {
                    label: format!("{kind:?} / {route:?} / {use_:?}"),
                    files,
                    probe: probe.expect("every probe template has a use"),
                    // FSharp.Core's union cases are not folded into the
                    // implicit surface, so its unshadowed cells decline.
                    // A union case heading a dotted path is not committed
                    // either.
                    control: route.adds_nothing()
                        && kind != Kind::ImplicitCase
                        && !(use_ == Use::Head && kind.is_case()),
                    known_wrong: is_known_wrong(kind, route),
                });
            }
        }
    }
    cells
}

/// FCS's record at exactly `span`, ignoring definitions.
fn fcs_at(uses: &FileUses, span: TextRange) -> Option<&NormalisedProjectUse> {
    let (start, end) = (usize::from(span.start()), usize::from(span.end()));
    let at: Vec<&NormalisedProjectUse> = uses
        .uses
        .iter()
        .filter(|u| !u.is_from_definition && u.start == start && u.end == end)
        .collect();
    if let Some(first) = at.first() {
        assert!(
            at.iter()
                .all(|u| u.full_name == first.full_name && u.kind == first.kind),
            "FCS reports more than one symbol at {span:?}: {at:?}"
        );
    }
    at.first().copied()
}

/// FCS's record, rendered for the manifest.
fn render_fcs(u: Option<&NormalisedProjectUse>) -> String {
    let Some(u) = u else {
        return "-".to_string();
    };
    let kind = if u.is_constructor {
        "ctor"
    } else {
        u.kind.as_deref().unwrap_or("?")
    };
    let origin = match (&u.decl, &u.assembly) {
        (Some(decl), _) => format!(
            "{}:{}",
            decl.file.file_name().unwrap_or_default().to_string_lossy(),
            decl.start
        ),
        (None, Some(assembly)) => assembly.clone(),
        (None, None) => "?".to_string(),
    };
    format!("{origin}:{}:{kind}", u.full_name.as_deref().unwrap_or("?"))
}

/// Whether our committed `res` names exactly FCS's record `u`.
fn agrees(
    env: &AssemblyEnv,
    proj: &ResolvedProject,
    paths: &[PathBuf],
    probe_idx: usize,
    res: Resolution,
    u: &NormalisedProjectUse,
) -> bool {
    match res {
        Resolution::Local(_) | Resolution::Item(_) => {
            let Some(decl) = &u.decl else {
                return false;
            };
            let (file_idx, range) = match res {
                Resolution::Item(_) => match proj.item_def(res) {
                    Some((i, def)) => (i, def.range),
                    None => return false,
                },
                _ => match proj.file(probe_idx).resolved_def(res) {
                    Some(def) => (probe_idx, def.range),
                    None => return false,
                },
            };
            paths[file_idx].file_name() == decl.file.file_name()
                && (usize::from(range.start()), usize::from(range.end())) == (decl.start, decl.end)
        }
        Resolution::Entity(_) | Resolution::Member { .. } => {
            let Some(full_name) = &u.full_name else {
                return false;
            };
            let ours = match res {
                Resolution::Entity(h) => {
                    (env.entity(h).assembly.name.clone(), env.entity_full_name(h))
                }
                Resolution::Member { parent, idx } => (
                    env.entity(parent).assembly.name.clone(),
                    format!(
                        "{}.{}",
                        env.entity_full_name(parent),
                        env.member_display_name(parent, idx)
                    ),
                ),
                _ => unreachable!(),
            };
            u.decl.is_none()
                && u.assembly.as_deref() == Some(ours.0.as_str())
                && full_name.as_str() == ours.1
        }
        Resolution::Deferred(_) | Resolution::Unresolved => false,
    }
}

fn env_of(dlls: &[&Path]) -> AssemblyEnv {
    let bytes: Vec<Vec<u8>> = dlls
        .iter()
        .map(|p| std::fs::read(p).unwrap_or_else(|e| panic!("read {p:?}: {e}")))
        .collect();
    let views: Vec<Ecma335Assembly> = bytes
        .iter()
        .map(|b| Ecma335Assembly::parse(b).expect("parse assembly"))
        .collect();
    AssemblyEnv::from_views(&views).expect("build AssemblyEnv")
}

/// Every contributor route, crossed with every kind of name it can shadow and
/// every way of using the name, graded against FCS.
#[test]
fn every_contributor_route_agrees_with_fcs() {
    let fsharp_core = ensure_fsharp_core_dll();
    let fixture = ensure_contributor_fixture_built();
    let env = env_of(&[fsharp_core.as_path(), fixture]);

    let mut failures = Vec::new();
    let mut entries = Vec::new();
    let mut shadowed_commits = 0;
    let all = cells();
    // Each cell is its own FCS project; the resident pool serves several at
    // once, so ask in parallel and grade in order.
    let next = std::sync::atomic::AtomicUsize::new(0);
    let answers: Vec<std::sync::Mutex<Option<_>>> =
        all.iter().map(|_| std::sync::Mutex::new(None)).collect();
    std::thread::scope(|scope| {
        for _ in 0..4 {
            scope.spawn(|| {
                loop {
                    let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let Some(cell) = all.get(i) else { break };
                    let rel: Vec<(&str, &str)> = cell
                        .files
                        .iter()
                        .map(|(p, s)| (p.as_str(), s.as_str()))
                        .collect();
                    let (root, written) = temp_fs_tree("contributor_grid", &rel);
                    let paths: Vec<&Path> = written.iter().map(|(p, _)| p.as_path()).collect();
                    let json = invoke_fcs_dump_project_with_refs(&paths, &[fixture]);
                    let fcs = parse_fcs_uses_project(&json, &written);
                    let _ = std::fs::remove_dir_all(&root);
                    *answers[i].lock().unwrap() = Some((written, fcs));
                }
            });
        }
    });
    for (cell, answer) in all.iter().zip(answers) {
        let (written, fcs) = answer.into_inner().unwrap().expect("every cell was asked");
        let rel: Vec<(&str, &str)> = cell
            .files
            .iter()
            .map(|(p, s)| (p.as_str(), s.as_str()))
            .collect();

        let probe_idx = written.len() - 1;
        let probe_path = &written[probe_idx].0;
        let fu = fcs
            .iter()
            .find(|f| f.path.file_name() == probe_path.file_name())
            .unwrap_or_else(|| panic!("no FCS uses for {}", cell.label));
        let errors: Vec<&String> = fcs.iter().flat_map(|f| &f.errors).collect();
        let theirs = fcs_at(fu, cell.probe);
        let rendered = render_fcs(theirs);

        let proj = resolve_project_files(&project(&rel), &env);
        let ours = proj
            .file(probe_idx)
            .resolution_at(cell.probe)
            .filter(|r| !matches!(r, Resolution::Deferred(_) | Resolution::Unresolved));
        let file_paths: Vec<PathBuf> = written.iter().map(|(p, _)| p.clone()).collect();
        let verdict = match ours {
            Some(res) => {
                if !cell.control {
                    shadowed_commits += 1;
                }
                if !cell.known_wrong
                    && !theirs.is_some_and(|u| agrees(&env, &proj, &file_paths, probe_idx, res, u))
                {
                    failures.push(format!(
                        "  {} [WRONG]\n    FCS:  {rendered}\n    ours: {res:?}\n    files: {:?}",
                        cell.label, cell.files
                    ));
                }
                "commit"
            }
            None => {
                if cell.control {
                    failures.push(format!(
                        "  {} [MUST COMMIT]\n    FCS:  {rendered}\n    files: {:?}",
                        cell.label, cell.files
                    ));
                }
                "decline"
            }
        };
        let clean = if errors.is_empty() {
            "clean"
        } else {
            "erroring"
        };
        entries.push(format!(
            "{} | fcs {rendered} ({clean}) | ours {verdict}",
            cell.label
        ));
    }
    if !failures.is_empty() {
        for e in &entries {
            eprintln!("cell: {e}");
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} contributor cells disagree with FCS:\n{}",
        failures.len(),
        all.len(),
        failures.join("\n")
    );
    // Non-vacuity: a grid whose shadowed cells all declined would pass the
    // property above while exercising none of the decisions about when a
    // route does not, in fact, shadow.
    assert!(
        shadowed_commits > 0,
        "no shadowed cell commits: the grid has stopped exercising its routes"
    );
    let manifest = Manifest::from_entries(entries).expect("manifest entries");
    check_manifest(
        "unmodelled_contributor_grid",
        &manifest,
        &regenerate("unmodelled_contributor_grid::"),
    );
}
