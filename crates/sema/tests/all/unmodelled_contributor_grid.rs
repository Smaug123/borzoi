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
    FileUses, NormalisedProjectUse, ensure_ce_extension_fixture_built,
    ensure_contributor_fixture_built, ensure_fsharp_core_dll, invoke_fcs_dump_project_with_refs,
    parse_fcs_uses_project, temp_fs_tree,
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
    /// `let probe = { new System.Object() with override _.ToString() = string
    /// (N 1) }`: inside an object-expression member.
    ObjectExpression,
}

impl Use {
    const ALL: [Use; 7] = [
        Use::Applied,
        Use::Head,
        Use::FunctionValue,
        Use::Local,
        Use::Nested,
        Use::Pattern,
        Use::ObjectExpression,
    ];

    fn applies_to(self, kind: Kind, route: Route) -> bool {
        if route.probes_itself() {
            return self == Use::Applied;
        }
        self != Use::Pattern || kind.is_case()
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
            Use::ObjectExpression => vec![
                "let probe = { new System.Object() with override _.ToString() = string (@N 1) }",
            ],
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
    /// A generic class of the name, then a non-generic union of it: both take
    /// part in the slot, the class's constructor among them.
    GenericClassThenUnion,
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
    /// An `[<AutoOpen>]` exception with a static of the name, in an
    /// `[<AutoOpen>]` module.
    AutoOpenExceptionInModule,
    /// The same exception declared directly in the probe's module.
    AutoOpenException,
}

impl Route {
    const ALL: [Route; 70] = [
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
        Route::GenericClassThenUnion,
        Route::EarlierNamespaceCtor,
        Route::EarlierNamespaceCtorOpened,
        Route::EarlierNamespaceCtorAbbrev,
        Route::RqaAliased,
        Route::EarlierNamespaceRqaAliased,
        Route::Extern,
        Route::Exception,
        Route::ActivePattern,
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
        Route::AutoOpenExceptionInModule,
        Route::AutoOpenException,
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
            Route::GenericClassThenUnion => (
                vec![],
                "module GridProbe\n@OWN\ntype @NAME<'T>(value: 'T) =\n    member _.Value = value\ntype @NAME = GridA | GridB\n@USE\n",
            ),
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
            Route::AutoOpenExceptionInModule => (
                vec![],
                "module GridProbe\n@OWN\n[<AutoOpen>]\nmodule GridAuto =\n    [<AutoOpen>]\n    exception GridEx of int with\n        @STATIC\n@USE\n",
            ),
            Route::AutoOpenException => (
                vec![],
                "module GridProbe\n@OWN\n[<AutoOpen>]\nexception GridEx of int with\n    @STATIC\n@USE\n",
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
    /// Nothing takes the probe from the unshadowed reading, and the walk can
    /// prove it: we must commit.
    control: bool,
    /// The referenced assemblies beside FSharp.Core.
    refs: Vec<&'static Path>,
    /// Whether FCS must check the cell cleanly, where the grid says.
    clean: Option<bool>,
    /// Something stands in front of the unshadowed reading — a contributor,
    /// or a builder offering an operation of the name — so a commit here is
    /// the walk deciding it does not take the probe.
    shadowed: bool,
}

fn cells(fixture: &'static Path) -> Vec<Cell> {
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
                    refs: vec![fixture],
                    clean: None,
                    shadowed: !route.adds_nothing(),
                });
            }
        }
    }
    cells
}

/// FCS's record at exactly `span`, ignoring definitions.
pub(crate) fn fcs_at(uses: &FileUses, span: TextRange) -> Option<&NormalisedProjectUse> {
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
pub(crate) fn render_fcs(u: Option<&NormalisedProjectUse>) -> String {
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
pub(crate) fn agrees(
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

pub(crate) fn env_of(dlls: &[&Path]) -> AssemblyEnv {
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
    let fixture = ensure_contributor_fixture_built();
    grade(
        &cells(fixture),
        "unmodelled_contributor_grid",
        "every_contributor_route",
    );
}

/// Every way a computation expression's builder can offer a custom operation,
/// crossed with every kind of name it can take and every position the name
/// can sit in, graded against FCS.
#[test]
fn every_computation_expression_cell_agrees_with_fcs() {
    let fixture = ensure_contributor_fixture_built();
    let extension = ensure_ce_extension_fixture_built();
    grade(
        &ce_cells(fixture, extension),
        "unmodelled_contributor_grid_ce",
        "every_computation_expression_cell",
    );
}

/// Grade `all` against FCS, and pin each cell's two answers in the manifest
/// `manifest`.
fn grade(all: &[Cell], manifest: &str, test: &str) {
    let fsharp_core = ensure_fsharp_core_dll();
    let mut envs: Vec<(Vec<&Path>, AssemblyEnv)> = Vec::new();
    for cell in all {
        if !envs.iter().any(|(refs, _)| *refs == cell.refs) {
            let dlls: Vec<&Path> = std::iter::once(fsharp_core.as_path())
                .chain(cell.refs.iter().copied())
                .collect();
            envs.push((cell.refs.clone(), env_of(&dlls)));
        }
    }

    let mut failures = Vec::new();
    let mut entries = Vec::new();
    let mut shadowed_commits = 0;
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
                    let json = invoke_fcs_dump_project_with_refs(&paths, &cell.refs);
                    let fcs = parse_fcs_uses_project(&json, &written);
                    let _ = std::fs::remove_dir_all(&root);
                    *answers[i].lock().unwrap() = Some((written, fcs));
                }
            });
        }
    });
    for (cell, answer) in all.iter().zip(answers) {
        let (written, fcs) = answer.into_inner().unwrap().expect("every cell was asked");
        let env = &envs
            .iter()
            .find(|(refs, _)| *refs == cell.refs)
            .expect("an env per reference set")
            .1;
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
        if let Some(clean) = cell.clean
            && clean != errors.is_empty()
        {
            failures.push(format!(
                "  {} [{}]\n    FCS:  {rendered}\n    errors: {errors:?}\n    files: {:?}",
                cell.label,
                if clean {
                    "EXPECTED CLEAN"
                } else {
                    "EXPECTED RECOVERED"
                },
                cell.files
            ));
        }

        let proj = resolve_project_files(&project(&rel), env);
        let ours = proj
            .file(probe_idx)
            .resolution_at(cell.probe)
            .filter(|r| !matches!(r, Resolution::Deferred(_) | Resolution::Unresolved));
        let file_paths: Vec<PathBuf> = written.iter().map(|(p, _)| p.clone()).collect();
        let verdict = match ours {
            Some(res) => {
                if cell.shadowed {
                    shadowed_commits += 1;
                }
                if !theirs.is_some_and(|u| agrees(env, &proj, &file_paths, probe_idx, res, u)) {
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
        "{} of {} cells disagree with FCS:\n{}",
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
    let manifest_entries = Manifest::from_entries(entries).expect("manifest entries");
    check_manifest(
        manifest,
        &manifest_entries,
        &regenerate(&format!("unmodelled_contributor_grid::{test}")),
    );
}

// ---------------------------------------------------------------------------
// The computation-expression axis.
//
// Inside `b { … }`, FCS adds a name-environment entry for each of the
// builder's custom operations (`[<CustomOperation>]`) ahead of the
// expression's body (`TcComputationExpression`, `AddFakeNameToNameEnv`), so a
// bare use of an operation's name finds the operation rather than any binding
// made outside the expression. Bindings made inside it come later and win —
// except where the translation reads a clause by its name alone
// (`CustomOperationClause`), whatever is bound. The operations are those of
// the builder's type, its base types, and every extension of it in scope
// (`getCustomOperationMethods`), named by the attribute's argument or, with
// none, by the method. The cells cross every way of supplying an operation
// with every kind of name it can take and every position the name can sit in.
// ---------------------------------------------------------------------------

/// What a bare name inside a computation expression binds when no custom
/// operation takes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CeKind {
    /// FSharp.Core's implicitly opened `id`.
    ImplicitValue,
    /// FSharp.Core's implicitly opened union case `Some`.
    ImplicitCase,
    /// A module-level `let` of the probe file's own.
    OwnValue,
    /// A union case of the probe file's own.
    OwnCase,
    /// A parameter of the function the expression is in.
    Parameter,
    /// A local `let` before the expression.
    OuterLocal,
    /// A `let` inside the expression, before the use.
    InnerLocal,
}

impl CeKind {
    const ALL: [CeKind; 7] = [
        CeKind::ImplicitValue,
        CeKind::ImplicitCase,
        CeKind::OwnValue,
        CeKind::OwnCase,
        CeKind::Parameter,
        CeKind::OuterLocal,
        CeKind::InnerLocal,
    ];

    fn is_case(self) -> bool {
        matches!(self, CeKind::ImplicitCase | CeKind::OwnCase)
    }

    /// The name probed, absent a builder that needs its own.
    fn name(self) -> &'static str {
        match self {
            CeKind::ImplicitValue => "id",
            CeKind::ImplicitCase => "Some",
            CeKind::OwnCase => "GridC",
            CeKind::OwnValue | CeKind::Parameter | CeKind::OuterLocal | CeKind::InnerLocal => {
                "gridV"
            }
        }
    }
}

/// The builder the expression runs on, and so the custom operations it
/// offers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CeBuilder {
    /// FSharp.Core's `async`: no custom operations.
    Async,
    /// FSharp.Core's `task`: no custom operations, and extension members of
    /// its base type in FSharp.Core.
    Task,
    /// FSharp.Core's `seq`: a function, so the braces are a sequence
    /// expression, which has no custom operations at all.
    Seq,
    /// FSharp.Core's `query`, with a name none of its operations has.
    Query,
    /// `query` with a name one of its operations has (`skip`).
    QueryOp,
    /// A project builder with no custom operations.
    Plain,
    /// A project builder whose one custom operation is named otherwise.
    OtherOp,
    /// `[<CustomOperation("N")>]` on a differently named method.
    Op,
    /// `[<CustomOperation>]` on a method named `N`.
    OpMethodName,
    /// `[<CustomOperation("")>]` on a method named `N`.
    OpEmptyName,
    /// `[<CustomOperation("", JoinConditionWord = "equals")>]` on a method
    /// named `N`: a named argument's string is no name.
    OpEmptyNameJoinWord,
    /// The attribute spelled through an abbreviation.
    OpAliased,
    /// The attribute through an abbreviation declared with the `Attribute`
    /// suffix and written without it, which F# also looks up.
    OpAliasedSuffixed,
    /// The same abbreviation declared in an earlier file.
    OpAliasedSuffixedEarlier,
    /// The same abbreviation declared in an earlier file's signature.
    OpAliasedSuffixedSignature,
    /// The attribute spelled fully qualified, with its `Attribute` suffix.
    OpQualified,
    /// The operation declared on the builder's base class.
    OpInherited,
    /// The builder constructed through an abbreviation of its type.
    OpAbbreviated,
    /// The builder constructed `new GridB<GridArg>()`, its type argument a
    /// type of the file's own.
    OpNewGeneric,
    /// The operation added by an intrinsic augmentation.
    OpAugmented,
    /// The operation added by an extension in a module the file opens.
    OpExtension,
    /// The same extension, its module not opened: out of scope.
    OpExtensionUnopened,
    /// The operation added by a C#-style `[<Extension>]` static method.
    OpCSharpExtension,
    /// The operation added to FSharp.Core's `AsyncBuilder` by an extension of
    /// the probe file's own, on `async`.
    AsyncExtension,
    /// The same extension in an earlier file, `[<AutoOpen>]`, whose signature
    /// alone carries the attribute: the signature's is the one later files
    /// see.
    SignatureExtension,
    /// A generic builder with the operation.
    Generic,
    /// A generic builder without it.
    GenericNoOp,
    /// A builder value whose type the walk does not know, with the operation.
    Unknown,
    /// The same, without it.
    UnknownNoOp,
    /// A builder declared in an earlier file, with the operation.
    CrossFile,
    /// The same, without it.
    CrossFileNoOp,
    /// A referenced assembly's builder with the operation.
    Fixture,
    /// A referenced assembly's builder without it.
    FixtureNoOp,
    /// A referenced assembly's builder inheriting the operation.
    FixtureInherited,
    /// A referenced assembly's builder whose operation takes its method's
    /// name.
    FixtureMethodName,
    /// A referenced assembly's extension adding the operation to `async`, in
    /// a namespace the file opens.
    FixtureExtension,
    /// The same, the namespace not opened: out of scope.
    FixtureExtensionUnopened,
}

/// How a builder's expression yields a value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CeForm {
    Return,
    Yield,
    Query,
}

impl CeBuilder {
    const ALL: [CeBuilder; 37] = [
        CeBuilder::Async,
        CeBuilder::Task,
        CeBuilder::Seq,
        CeBuilder::Query,
        CeBuilder::QueryOp,
        CeBuilder::Plain,
        CeBuilder::OtherOp,
        CeBuilder::Op,
        CeBuilder::OpMethodName,
        CeBuilder::OpEmptyName,
        CeBuilder::OpEmptyNameJoinWord,
        CeBuilder::OpAliased,
        CeBuilder::OpAliasedSuffixed,
        CeBuilder::OpAliasedSuffixedEarlier,
        CeBuilder::OpAliasedSuffixedSignature,
        CeBuilder::OpQualified,
        CeBuilder::OpInherited,
        CeBuilder::OpAbbreviated,
        CeBuilder::OpNewGeneric,
        CeBuilder::OpAugmented,
        CeBuilder::OpExtension,
        CeBuilder::OpExtensionUnopened,
        CeBuilder::OpCSharpExtension,
        CeBuilder::AsyncExtension,
        CeBuilder::SignatureExtension,
        CeBuilder::Generic,
        CeBuilder::GenericNoOp,
        CeBuilder::Unknown,
        CeBuilder::UnknownNoOp,
        CeBuilder::CrossFile,
        CeBuilder::CrossFileNoOp,
        CeBuilder::Fixture,
        CeBuilder::FixtureNoOp,
        CeBuilder::FixtureInherited,
        CeBuilder::FixtureMethodName,
        CeBuilder::FixtureExtension,
        CeBuilder::FixtureExtensionUnopened,
    ];

    /// Whether FCS finds a custom operation of the probed name: the premise
    /// each cell's expected cleanliness rests on.
    fn offers(self) -> bool {
        !matches!(
            self,
            CeBuilder::Async
                | CeBuilder::Task
                | CeBuilder::Seq
                | CeBuilder::Query
                | CeBuilder::Plain
                | CeBuilder::OtherOp
                | CeBuilder::OpExtensionUnopened
                | CeBuilder::GenericNoOp
                | CeBuilder::UnknownNoOp
                | CeBuilder::CrossFileNoOp
                | CeBuilder::FixtureNoOp
                | CeBuilder::FixtureExtensionUnopened
        )
    }

    /// Whether the walk can prove the builder offers no operation of the
    /// name: its type is known, and so is every way an operation could be
    /// added to it.
    fn provably_lacks(self) -> bool {
        matches!(
            self,
            CeBuilder::Async
                | CeBuilder::Task
                | CeBuilder::Seq
                | CeBuilder::Query
                | CeBuilder::Plain
                | CeBuilder::OtherOp
                | CeBuilder::GenericNoOp
                | CeBuilder::FixtureNoOp
        )
    }

    /// Whether the builder declares nothing but `Yield` and `Return`: a
    /// statement after a `let` cannot be typed against it.
    fn minimal(self) -> bool {
        self.expr() == "gridB" || self.expr().starts_with("GridFix.")
    }

    /// Whether the builder types a statement that is not a custom
    /// operation: FSharp.Core's discard it or yield it. Against the others it
    /// is an error unrelated to custom operations, for which FCS records no
    /// symbol, so those cells are not generated.
    fn types_plain_statements(self) -> bool {
        matches!(self.expr(), "async" | "task" | "seq")
    }

    /// Whether the operation is one of `async`'s, so a nested `async { … }`
    /// offers it too.
    fn offers_on_async(self) -> bool {
        matches!(
            self,
            CeBuilder::AsyncExtension | CeBuilder::SignatureExtension | CeBuilder::FixtureExtension
        )
    }

    /// Whether the walk takes the name from a nested `async { … }` too,
    /// which FCS does not: it counts an operation an extension supplies as
    /// one of every builder's, and an expression over a builder it cannot
    /// list marks every name in it, the nested builder's own included. The
    /// walk does not resolve a base class an `inherit` names, so it cannot
    /// list an inheriting builder's operations, nor an abbreviation's.
    fn walk_reaches_async(self) -> bool {
        matches!(
            self,
            CeBuilder::OpInherited
                | CeBuilder::OpAbbreviated
                | CeBuilder::OpAugmented
                | CeBuilder::SignatureExtension
                | CeBuilder::OpExtension
                | CeBuilder::OpExtensionUnopened
                | CeBuilder::OpCSharpExtension
                | CeBuilder::AsyncExtension
                | CeBuilder::FixtureExtension
                | CeBuilder::FixtureExtensionUnopened
                | CeBuilder::Unknown
                | CeBuilder::UnknownNoOp
                | CeBuilder::CrossFile
                | CeBuilder::CrossFileNoOp
        )
    }

    fn form(self) -> CeForm {
        match self {
            CeBuilder::Seq => CeForm::Yield,
            CeBuilder::Query | CeBuilder::QueryOp => CeForm::Query,
            _ => CeForm::Return,
        }
    }

    /// The builder expression.
    fn expr(self) -> &'static str {
        match self {
            CeBuilder::Async
            | CeBuilder::AsyncExtension
            | CeBuilder::SignatureExtension
            | CeBuilder::FixtureExtension
            | CeBuilder::FixtureExtensionUnopened => "async",
            CeBuilder::Task => "task",
            CeBuilder::Seq => "seq",
            CeBuilder::Query | CeBuilder::QueryOp => "query",
            CeBuilder::Fixture => "GridFix.FixBuilders.fixB",
            CeBuilder::FixtureNoOp => "GridFix.FixBuilders.fixBNoOp",
            CeBuilder::FixtureInherited => "GridFix.FixBuilders.fixBDerived",
            CeBuilder::FixtureMethodName => "GridFix.FixBuilders.fixBMethodName",
            _ => "gridB",
        }
    }

    /// The probed name for `kind`, or `None` where the builder cannot probe
    /// it: `query`'s operations are named like no implicit value or case.
    fn name(self, kind: CeKind) -> Option<&'static str> {
        match self {
            CeBuilder::QueryOp if kind.is_case() || kind == CeKind::ImplicitValue => None,
            CeBuilder::QueryOp => Some("skip"),
            _ => Some(kind.name()),
        }
    }

    /// Whether the cell needs the extension fixture rather than the shared
    /// one: its extension reaches every `async` in the project.
    fn needs_extension_fixture(self) -> bool {
        matches!(
            self,
            CeBuilder::FixtureExtension | CeBuilder::FixtureExtensionUnopened
        )
    }

    /// Earlier files, as `(relative path, source)`, with `@N` for the name.
    fn earlier(self) -> Vec<(&'static str, String)> {
        match self {
            CeBuilder::CrossFile => vec![(
                "GridBuilders.fs",
                lines_of(
                    ["module GridBuilders".to_string()]
                        .into_iter()
                        .chain(grid_builder("type GridB() =", op_lines("@N", "")))
                        .chain(["let gridB = GridB()".to_string()]),
                ),
            )],
            CeBuilder::SignatureExtension => vec![
                (
                    "GridSigExt.fsi",
                    lines_of([
                        "[<AutoOpen>]".to_string(),
                        "module GridSigExt".to_string(),
                        "type Microsoft.FSharp.Control.AsyncBuilder with".to_string(),
                        "    [<CustomOperation(\"@N\")>]".to_string(),
                        "    member GridOp: state: int * v: int -> int".to_string(),
                    ]),
                ),
                (
                    "GridSigExt.fs",
                    lines_of([
                        "[<AutoOpen>]".to_string(),
                        "module GridSigExt".to_string(),
                        "type Microsoft.FSharp.Control.AsyncBuilder with".to_string(),
                        "    member _.GridOp(state: int, v: int) = state + v".to_string(),
                    ]),
                ),
            ],
            CeBuilder::OpAliasedSuffixedEarlier => vec![(
                "GridAlias.fs",
                lines_of([
                    "module GridAlias".to_string(),
                    "type GridCOAttribute = CustomOperationAttribute".to_string(),
                ]),
            )],
            CeBuilder::OpAliasedSuffixedSignature => vec![
                (
                    "GridAlias.fsi",
                    lines_of([
                        "module GridAlias".to_string(),
                        "type GridCOAttribute = CustomOperationAttribute".to_string(),
                    ]),
                ),
                (
                    "GridAlias.fs",
                    lines_of([
                        "module GridAlias".to_string(),
                        "type GridCOAttribute = CustomOperationAttribute".to_string(),
                    ]),
                ),
            ],
            CeBuilder::CrossFileNoOp => vec![(
                "GridBuilders.fs",
                lines_of(
                    ["module GridBuilders".to_string()]
                        .into_iter()
                        .chain(grid_builder("type GridB() =", vec![]))
                        .chain(["let gridB = GridB()".to_string()]),
                ),
            )],
            _ => vec![],
        }
    }

    /// The probe file's declarations of the builder, with `@N` for the name.
    fn decls(self) -> Vec<String> {
        let let_b = || "let gridB = GridB()".to_string();
        let plain = || grid_builder("type GridB() =", vec![]);
        let mut out: Vec<String> = match self {
            CeBuilder::Async
            | CeBuilder::Task
            | CeBuilder::Seq
            | CeBuilder::Query
            | CeBuilder::QueryOp
            | CeBuilder::Fixture
            | CeBuilder::FixtureNoOp
            | CeBuilder::FixtureInherited
            | CeBuilder::FixtureMethodName
            | CeBuilder::FixtureExtensionUnopened => vec![],
            CeBuilder::Plain => plain().into_iter().chain([let_b()]).collect(),
            CeBuilder::OtherOp => grid_builder("type GridB() =", op_lines("gridOther", ""))
                .into_iter()
                .chain([let_b()])
                .collect(),
            CeBuilder::Op => grid_builder("type GridB() =", op_lines("@N", ""))
                .into_iter()
                .chain([let_b()])
                .collect(),
            CeBuilder::OpMethodName => grid_builder(
                "type GridB() =",
                vec![
                    "[<CustomOperation>]".to_string(),
                    "member _.@N(state: int, v: int) = state + v".to_string(),
                ],
            )
            .into_iter()
            .chain([let_b()])
            .collect(),
            CeBuilder::OpEmptyName => grid_builder(
                "type GridB() =",
                vec![
                    "[<CustomOperation(\"\")>]".to_string(),
                    "member _.@N(state: int, v: int) = state + v".to_string(),
                ],
            )
            .into_iter()
            .chain([let_b()])
            .collect(),
            CeBuilder::OpEmptyNameJoinWord => grid_builder(
                "type GridB() =",
                vec![
                    "[<CustomOperation(\"\", JoinConditionWord = \"equals\")>]".to_string(),
                    "member _.@N(state: int, v: int) = state + v".to_string(),
                ],
            )
            .into_iter()
            .chain([let_b()])
            .collect(),
            CeBuilder::OpAbbreviated => grid_builder("type GridB0() =", op_lines("@N", ""))
                .into_iter()
                .chain(["type GridB = GridB0".to_string(), let_b()])
                .collect(),
            CeBuilder::OpNewGeneric => ["type GridArg() = class end".to_string()]
                .into_iter()
                .chain(grid_builder("type GridB<'T>() =", op_lines("@N", "")))
                .chain(["let gridB = new GridB<GridArg>()".to_string()])
                .collect(),
            CeBuilder::OpCSharpExtension => plain()
                .into_iter()
                .chain([
                    let_b(),
                    "[<System.Runtime.CompilerServices.Extension>]".to_string(),
                    "type GridExtensions =".to_string(),
                    "    [<System.Runtime.CompilerServices.Extension; CustomOperation(\"@N\")>]"
                        .to_string(),
                    "    static member GridOp(b: GridB, state: int, v: int) = state + v"
                        .to_string(),
                ])
                .collect(),
            CeBuilder::OpAliasedSuffixed => {
                ["type GridCOAttribute = CustomOperationAttribute".to_string()]
                    .into_iter()
                    .chain(suffixed_alias_builder("GridCO"))
                    .collect()
            }
            // Qualified, not opened: an `open` of the module is its own
            // contributor, which the grid's other routes cover.
            CeBuilder::OpAliasedSuffixedEarlier | CeBuilder::OpAliasedSuffixedSignature => {
                suffixed_alias_builder("GridAlias.GridCO")
            }
            CeBuilder::OpAliased => ["type GridCO = CustomOperationAttribute".to_string()]
                .into_iter()
                .chain(grid_builder(
                    "type GridB() =",
                    vec![
                        "[<GridCO(\"@N\")>]".to_string(),
                        "member _.GridOp(state: int, v: int) = state + v".to_string(),
                    ],
                ))
                .chain([let_b()])
                .collect(),
            CeBuilder::OpQualified => grid_builder(
                "type GridB() =",
                vec![
                    "[<Microsoft.FSharp.Core.CustomOperationAttribute(\"@N\")>]".to_string(),
                    "member _.GridOp(state: int, v: int) = state + v".to_string(),
                ],
            )
            .into_iter()
            .chain([let_b()])
            .collect(),
            CeBuilder::OpInherited => ["type GridBase() =".to_string()]
                .into_iter()
                .chain(op_lines("@N", "    "))
                .chain([
                    "type GridB() =".to_string(),
                    "    inherit GridBase()".to_string(),
                    "    member _.Yield(()) = 0".to_string(),
                    "    member _.Return(_: 'a) = 0".to_string(),
                    let_b(),
                ])
                .collect(),
            CeBuilder::OpAugmented => plain()
                .into_iter()
                .chain(["type GridB with".to_string()])
                .chain(op_lines("@N", "    "))
                .chain([let_b()])
                .collect(),
            CeBuilder::OpExtension | CeBuilder::OpExtensionUnopened => plain()
                .into_iter()
                .chain([
                    let_b(),
                    "module GridExt =".to_string(),
                    "    type GridB with".to_string(),
                ])
                .chain(op_lines("@N", "        "))
                .collect(),
            CeBuilder::SignatureExtension => vec![],
            CeBuilder::AsyncExtension => {
                ["type Microsoft.FSharp.Control.AsyncBuilder with".to_string()]
                    .into_iter()
                    .chain(op_lines("@N", "    "))
                    .collect()
            }
            CeBuilder::Generic => grid_builder("type GridB<'T>() =", op_lines("@N", ""))
                .into_iter()
                .chain(["let gridB = GridB<int>()".to_string()])
                .collect(),
            CeBuilder::GenericNoOp => grid_builder("type GridB<'T>() =", vec![])
                .into_iter()
                .chain(["let gridB = GridB<int>()".to_string()])
                .collect(),
            CeBuilder::Unknown | CeBuilder::UnknownNoOp => {
                let ops = if self == CeBuilder::Unknown {
                    op_lines("@N", "")
                } else {
                    vec![]
                };
                grid_builder("type GridB() =", ops)
                    .into_iter()
                    .chain([
                        "let makeGridB () = GridB()".to_string(),
                        "let gridB = makeGridB ()".to_string(),
                    ])
                    .collect()
            }
            CeBuilder::CrossFile | CeBuilder::CrossFileNoOp => {
                vec!["open GridBuilders".to_string()]
            }
            CeBuilder::FixtureExtension => vec!["open GridCeExt".to_string()],
        };
        if self == CeBuilder::OpExtension {
            out.push("open GridExt".to_string());
        }
        out
    }
}

/// A builder whose operation's attribute is written `attribute`, which names
/// an abbreviation spelled with `Attribute` after it through F#'s suffix
/// lookup.
fn suffixed_alias_builder(attribute: &str) -> Vec<String> {
    grid_builder(
        "type GridB() =",
        vec![
            format!("[<{attribute}(\"@N\")>]"),
            "member _.GridOp(state: int, v: int) = state + v".to_string(),
        ],
    )
    .into_iter()
    .chain(["let gridB = GridB()".to_string()])
    .collect()
}

/// `[<CustomOperation(name)>]` on a method, indented by `indent`.
fn op_lines(name: &str, indent: &str) -> Vec<String> {
    vec![
        format!("{indent}[<CustomOperation(\"{name}\")>]"),
        format!("{indent}member _.GridOp(state: int, v: int) = state + v"),
    ]
}

/// A builder type: `header`, the members every builder here has, then
/// `extra` members.
fn grid_builder(header: &str, extra: Vec<String>) -> Vec<String> {
    [
        header.to_string(),
        "    member _.Yield(()) = 0".to_string(),
        "    member _.Return(_: 'a) = 0".to_string(),
    ]
    .into_iter()
    .chain(extra.into_iter().map(|l| format!("    {l}")))
    .collect()
}

fn lines_of(lines: impl IntoIterator<Item = String>) -> String {
    let mut out = String::new();
    for l in lines {
        out.push_str(&l);
        out.push('\n');
    }
    out
}

/// Where in the expression the name is used.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CeUse {
    /// `b { N 1 }`: a statement, which the translation reads as a custom
    /// operation clause by the name alone.
    Statement,
    /// `b { return N 1 }`.
    Return,
    /// `b { return (fun () -> N 1) () }`: under a lambda.
    Lambda,
    /// `b { return List.map N [ 1 ] }`: as a first-class value.
    FunctionValue,
    /// `b { return N.GetType () }`: heading a dotted path.
    Head,
    /// `b { return async { return N 1 } }`: in a nested expression over
    /// another builder.
    InnerCe,
    /// `async { let r = b { return N 1 } … }`: the expression nested in one
    /// over another builder.
    OuterCe,
    /// `b { let r = N 1 … }`: a `let` right-hand side.
    LetBound,
    /// `b { return (match … with N _ -> 1 | _ -> 0) }`: a pattern, which
    /// custom operations do not reach.
    Pattern,
    /// `query { for … do select N }`: the argument of another operation's
    /// clause, which the translation does not read by name.
    ClauseArgument,
    /// `b { return { new System.Object() with override _.ToString() = …N 1… } … }`:
    /// inside an object-expression member the expression contains.
    ObjectExpression,
}

impl CeUse {
    const ALL: [CeUse; 11] = [
        CeUse::Statement,
        CeUse::Return,
        CeUse::Lambda,
        CeUse::FunctionValue,
        CeUse::Head,
        CeUse::InnerCe,
        CeUse::OuterCe,
        CeUse::LetBound,
        CeUse::Pattern,
        CeUse::ClauseArgument,
        CeUse::ObjectExpression,
    ];

    fn applies_to(self, kind: CeKind, builder: CeBuilder) -> bool {
        match self {
            CeUse::Statement => builder.offers() || builder.types_plain_statements(),
            CeUse::Head => !kind.is_case(),
            CeUse::Pattern => kind.is_case(),
            CeUse::ClauseArgument => builder.form() == CeForm::Query,
            // A query is quoted, and a quotation cannot hold an object
            // expression.
            CeUse::ObjectExpression => builder.form() != CeForm::Query,
            _ => true,
        }
    }
}

/// The probe file's text for one cell, with `@N` for the name and `@P` for
/// the probed occurrence of it.
fn ce_probe_template(kind: CeKind, builder: CeBuilder, use_: CeUse) -> String {
    let mut lines: Vec<String> = vec!["module GridProbe".to_string()];
    match kind {
        CeKind::OwnValue => lines.push("let @N (x: int) = x * 2".to_string()),
        CeKind::OwnCase => lines.push("type GridE = GridC of int | GridEOther".to_string()),
        _ => {}
    }
    lines.extend(builder.decls());

    // The value the expression yields.
    let value = match use_ {
        CeUse::Statement | CeUse::Return | CeUse::OuterCe => "@P 1".to_string(),
        CeUse::Lambda => "(fun () -> @P 1) ()".to_string(),
        CeUse::FunctionValue => "List.map @P [ 1 ]".to_string(),
        CeUse::Head => "@P.GetType ()".to_string(),
        CeUse::InnerCe => "async { return @P 1 }".to_string(),
        CeUse::LetBound => "gridR".to_string(),
        CeUse::ClauseArgument => "@P".to_string(),
        CeUse::ObjectExpression => {
            "(let gridO = { new System.Object() with override _.ToString() = string (@P 1) } in gridO.GetHashCode())"
                .to_string()
        }
        CeUse::Pattern => {
            let subject = if kind == CeKind::OwnCase {
                "GridEOther"
            } else {
                "Option.Some 1"
            };
            format!("(match {subject} with @P _ -> 1 | _ -> 0)")
        }
    };
    let mut body: Vec<String> = Vec::new();
    if kind == CeKind::InnerLocal {
        body.push("let @N (x: int) = x * 2".to_string());
    }
    if use_ == CeUse::LetBound {
        body.push("let gridR = @P 1".to_string());
    }
    body.push(match (use_, builder.form()) {
        (CeUse::Statement, _) => value,
        (CeUse::ClauseArgument, _) => format!("select {value}"),
        (_, CeForm::Return) => format!("return {value}"),
        (_, CeForm::Yield) => format!("yield {value}"),
        (_, CeForm::Query) => format!("select ({value})"),
    });
    let mut ce: Vec<String> = vec![format!("{} {{", builder.expr())];
    let indent = match builder.form() {
        CeForm::Query => {
            ce.push("    for gridX in [ 1 ] do".to_string());
            "        "
        }
        _ => "    ",
    };
    ce.extend(body.into_iter().map(|l| format!("{indent}{l}")));
    ce.push("}".to_string());
    if use_ == CeUse::OuterCe {
        let inner = ce;
        ce = vec!["async {".to_string(), "    let gridO =".to_string()];
        ce.extend(inner.into_iter().map(|l| format!("        {l}")));
        ce.push("    return gridO".to_string());
        ce.push("}".to_string());
    }

    lines.push(match kind {
        CeKind::Parameter => "let probe (@N: int -> int) =".to_string(),
        // A generic value selected is generic: a function, not a value, can be.
        _ if use_ == CeUse::ClauseArgument => "let probe () =".to_string(),
        _ => "let probe =".to_string(),
    });
    if kind == CeKind::OuterLocal {
        lines.push("    let @N (x: int) = x * 2".to_string());
    }
    lines.extend(ce.into_iter().map(|l| format!("    {l}")));
    lines_of(lines)
}

/// Substitute `name` for `@N` and `@P`, returning the text and the span of
/// the `@P` occurrence.
fn place_probe(template: &str, name: &str) -> (String, Option<TextRange>) {
    let text = template.replace("@N", name);
    let probe = text.find("@P").map(|at| {
        TextRange::new(
            u32::try_from(at).unwrap().into(),
            u32::try_from(at + name.len()).unwrap().into(),
        )
    });
    (text.replace("@P", name), probe)
}

fn ce_cells(fixture: &'static Path, extension: &'static Path) -> Vec<Cell> {
    let mut cells = Vec::new();
    for kind in CeKind::ALL {
        for builder in CeBuilder::ALL {
            let Some(name) = builder.name(kind) else {
                continue;
            };
            for use_ in CeUse::ALL {
                if !use_.applies_to(kind, builder) {
                    continue;
                }
                let mut files: Vec<(String, String)> = builder
                    .earlier()
                    .into_iter()
                    .map(|(path, t)| (path.to_string(), place_probe(&t, name).0))
                    .collect();
                let (src, probe) = place_probe(&ce_probe_template(kind, builder, use_), name);
                files.push(("GridProbe.fs".to_string(), src));
                let refs = if builder.needs_extension_fixture() {
                    vec![fixture, extension]
                } else {
                    vec![fixture]
                };
                cells.push(Cell {
                    label: format!("{kind:?} / {builder:?} / {use_:?}"),
                    files,
                    probe: probe.expect("every computation-expression cell has a probe"),
                    control: ce_control(kind, builder, use_),
                    refs,
                    clean: Some(ce_expect_clean(kind, builder, use_)),
                    shadowed: builder.offers(),
                });
            }
        }
    }
    cells
}

/// Whether FCS must check the cell cleanly. A custom operation used as a
/// value is an error, and so is one used as a statement after a `let` in a
/// builder with nothing but `Yield` and `Return`, though FCS still records
/// the operation there.
fn ce_expect_clean(kind: CeKind, builder: CeBuilder, use_: CeUse) -> bool {
    match use_ {
        CeUse::Statement => !(builder.offers() && kind == CeKind::InnerLocal && builder.minimal()),
        CeUse::Pattern => true,
        // The nested `async { … }` adds its own operations after the local.
        CeUse::InnerCe if kind == CeKind::InnerLocal => !builder.offers_on_async(),
        _ => !builder.offers() || kind == CeKind::InnerLocal,
    }
}

/// Whether the walk must commit: FCS binds the unshadowed reading, and the
/// walk can prove nothing takes it.
fn ce_control(kind: CeKind, builder: CeBuilder, use_: CeUse) -> bool {
    if !ce_expect_clean(kind, builder, use_) {
        return false;
    }
    match kind {
        // FSharp.Core's union cases are not folded into the implicit surface.
        CeKind::ImplicitCase => false,
        // A custom operation reaches no pattern.
        CeKind::OwnCase if use_ == CeUse::Pattern => true,
        // A binding inside the expression comes after its operations, except
        // where a statement is read as a clause by its name alone.
        CeKind::InnerLocal if use_ == CeUse::InnerCe => !builder.walk_reaches_async(),
        CeKind::InnerLocal if use_ != CeUse::Statement => true,
        _ => builder.provably_lacks(),
    }
}
