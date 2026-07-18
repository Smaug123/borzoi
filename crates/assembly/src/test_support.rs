//! Differential-test currency for the assembly reader.
//!
//! Both sides of the diff (our Rust importer and FCS's via `tools/fcs-dump`)
//! project to a shared [`NormalisedEntity`] tree. This normaliser elides
//! volatile bits by default — assembly versions, custom-attribute blobs,
//! member ordering.
//! Specific tests can call the lower-level projectors when they want to pin
//! one of those.
//!
//! Phase 1 has no real binary parsing on the Rust side and no entity
//! support in `fcs-dump` yet. The harness is exercised by hand-built
//! fixtures (both a Rust `Vec<Entity>` and the equivalent JSON) projecting
//! to the same [`NormalisedEntity`] tree, end-to-end through the same code
//! paths that phase 2 will plug a real backend into.
//!
//! The JSON shape this module accepts is the contract `fcs-dump` will be
//! taught in phase 2:
//!
//! ```json
//! {
//!   "Assembly": "mscorlib",
//!   "Entities": [
//!     { "Fqn": "System.Object", "Kind": "Class", "Access": "Public",
//!       "BaseType": null, "Interfaces": [], "Members": [
//!         { "Kind": "Method", "Name": "Equals",
//!           "Signature": "(System.Object) -> System.Boolean",
//!           "Access": "Public", "Flags": ["instance", "virtual"] }
//!       ], "NestedTypes": [] }
//!   ]
//! }
//! ```

use std::collections::{BTreeMap, BTreeSet};

use crate::{
    AbbreviationTarget, Access, CompilerFeatureRequired, DefaultMember, Entity, EntityKind, Event,
    Experimental, Member, MethodLike, MethodSignature, Nullability, NullableType, Obsolete,
    ParamDefault, Parameter, Primitive, Property, TypeParameter, TypeRef, Variance,
};
use serde::Deserialize;

/// One assembly's worth of normalised entities. Equal trees mean "the two
/// importers agree on the symbol surface of this DLL, modulo the volatile
/// bits the normaliser elides".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalisedAssembly {
    pub name: String,
    pub entities: Vec<NormalisedEntity>,
}

/// A type, module, or other top-level definition, projected to flat
/// strings so structural comparison is trivial.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalisedEntity {
    /// Fully-qualified name: `Namespace.Name` (no leading dot for the
    /// global namespace, no backtick arity suffix — the type-argument
    /// count is implied by `GenericParameters`).
    pub fqn: String,
    pub kind: String,
    pub access: String,
    /// Formal type parameters in declaration order. Empty for non-generic
    /// entities.
    pub generic_parameters: Vec<NormalisedGenericParameter>,
    pub base_type: Option<String>,
    pub interfaces: Vec<String>,
    pub members: Vec<NormalisedMember>,
    pub nested_types: Vec<NormalisedEntity>,
    /// `Some(rendering)` when the entity carries `[<Obsolete>]`. The
    /// rendering is shaped so the four legal payload combinations are
    /// visually distinct in failure output — see [`format_obsolete`].
    pub obsolete: Option<String>,
    /// `Some(rendering)` when the entity carries
    /// `[<Experimental>]` (the .NET 8+
    /// `System.Diagnostics.CodeAnalysis.ExperimentalAttribute`). The
    /// rendering is shaped so the eight legal payload combinations are
    /// visually distinct in failure output — see [`format_experimental`].
    pub experimental: Option<String>,
    /// `Some(rendering)` when the entity carries
    /// `[System.Reflection.DefaultMemberAttribute(name)]`. The rendering
    /// is `"[default-member: <name>]"` for a decoded name, or
    /// `"[default-member]"` for the degraded `Unknown` case (see
    /// [`format_default_member`]); the bracketed shape keeps it visually
    /// distinct from other diff strings and from `obsolete` /
    /// `experimental`.
    pub default_member: Option<String>,
    /// Sorted set of `[compiler-feature-required: <feature>]` renderings,
    /// one per `[CompilerFeatureRequiredAttribute]` on the entity (the
    /// attribute is `AllowMultiple = true`, so more than one can appear).
    /// Empty when the entity carries none. See
    /// [`format_compiler_feature_required`].
    pub compiler_feature_required: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalisedMember {
    pub kind: String,
    pub name: String,
    pub signature: String,
    pub access: String,
    /// Sorted set of flag strings so two importers emitting the same set
    /// in different orders compare equal.
    pub flags: BTreeSet<String>,
    /// Formal type parameters in declaration order, populated for generic
    /// methods. Empty for non-method members and for non-generic methods.
    pub generic_parameters: Vec<NormalisedGenericParameter>,
    /// Same shape as [`NormalisedEntity::obsolete`]; populated for
    /// methods only (the other [`Member`] kinds don't yet carry a
    /// projected `obsolete` field on the Rust side).
    pub obsolete: Option<String>,
    /// Same shape as [`NormalisedEntity::experimental`]; populated for
    /// methods only (the other [`Member`] kinds don't yet carry a
    /// projected `experimental` field on the Rust side).
    pub experimental: Option<String>,
}

/// One generic-parameter declaration, projected to strings so the diff is
/// stable across the two projectors. `declaration` carries the variance
/// prefix + typar name (`out T`, `in U`, or plain `T`); `constraints` is a
/// sorted set so a different on-the-wire order doesn't cause a spurious
/// diff. Order *within* a parent's parameter list is positional (typar
/// index), so the surrounding `Vec` preserves declaration order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalisedGenericParameter {
    pub declaration: String,
    pub constraints: BTreeSet<String>,
}

// ============================================================================
// Rust-side projection: Vec<Entity> -> NormalisedAssembly
// ============================================================================

pub fn normalise_entities(assembly_name: &str, entities: &[Entity]) -> NormalisedAssembly {
    let mut entities: Vec<_> = entities.iter().map(normalise_entity).collect();
    sort_entities(&mut entities);
    NormalisedAssembly {
        name: assembly_name.to_string(),
        entities,
    }
}

fn normalise_entity(e: &Entity) -> NormalisedEntity {
    let mut members: Vec<_> = e
        .members
        .iter()
        .filter(|m| accessible_from_some_fsharp_code(member_access(m)))
        .filter(|m| !is_unmirrorable_generic_module_method(e.kind, m))
        .filter(|m| !is_module_literal(e.kind, m))
        .map(normalise_member)
        .collect();
    sort_members(&mut members);
    // F# cannot declare user types inside a union, so every nested
    // TypeDef under a Union-kind entity is compiler machinery: per-case
    // subclasses (`C1`), nullary-case singletons (`_C0`), debugger
    // proxies (`…@DebugTypeProxy`), `Tags`. FCS hides all of these from
    // `NestedEntities` (compiler-generated provenance); the owned model
    // keeps them faithfully, so the differential elides them here — the
    // same one-sided-view pattern as `is_unmirrorable_generic_module_method`.
    let mut nested: Vec<_> = if e.kind == EntityKind::Union {
        vec![]
    } else {
        e.nested_types.iter().map(normalise_entity).collect()
    };
    sort_entities(&mut nested);
    NormalisedEntity {
        // fcs-dump renders an entity by its F# `DisplayName` (the source name),
        // so a module-suffix entity (`ListModule`) must be compared as `List`.
        // For every non-suffixed entity `source_name` is `None` and this is the
        // plain IL name, matching `DisplayName` there too.
        fqn: fqn(&e.namespace, e.source_name.as_deref().unwrap_or(&e.name)),
        kind: format_entity_kind(e),
        access: access_str(e.access).into(),
        generic_parameters: e.generic_parameters.iter().map(normalise_typar).collect(),
        base_type: e.base_type.as_ref().map(render_type),
        interfaces: e.interfaces.iter().map(render_type).collect(),
        members,
        nested_types: nested,
        obsolete: e.obsolete.as_ref().map(format_obsolete),
        experimental: e.experimental.as_ref().map(format_experimental),
        default_member: e.default_member.as_ref().map(format_default_member),
        compiler_feature_required: e
            .compiler_feature_required
            .iter()
            .map(format_compiler_feature_required)
            .collect(),
    }
}

fn normalise_typar(p: &TypeParameter) -> NormalisedGenericParameter {
    let declaration = match p.variance {
        Variance::Invariant => p.name.clone(),
        Variance::Covariant => format!("out {}", p.name),
        Variance::Contravariant => format!("in {}", p.name),
    };
    let mut constraints = BTreeSet::new();
    if p.reference_type_constraint {
        constraints.insert("class".into());
    }
    if p.value_type_constraint {
        constraints.insert("struct".into());
    }
    if p.default_constructor_constraint {
        constraints.insert("new()".into());
    }
    if p.is_unmanaged {
        // `unmanaged` is additive alongside `struct` — in IL the
        // unmanaged constraint sets both the value-type bit AND the
        // `IsUnmanagedAttribute` CA; preserve both in the diff so neither
        // half can silently drop the bit.
        constraints.insert("unmanaged".into());
    }
    if p.allows_ref_struct {
        // The C# 13 / F# 9 `allows ref struct` anti-constraint — the
        // `AllowByRefLike` (`0x0020`) bit on the typar. Independent of the
        // other special constraints; `fcs-dump` reads the same
        // `ILGenericParameterDef.HasAllowsRefStruct` bit and emits this
        // exact token.
        constraints.insert("allows ref struct".into());
    }
    // Nullable state is emitted as an additive token (phase 4m.1). Roslyn
    // emits `notnull` and `class?` as the C# user-facing constraint
    // names; the diff borrows them so a reader recognises the source
    // intent. `Oblivious` is the pre-C#8 default and contributes no
    // token — matching FCS's `ILGenericParameter` surface on assemblies
    // that were compiled without the nullable feature.
    match p.nullability {
        Nullability::Oblivious => {}
        Nullability::NotAnnotated => {
            constraints.insert("notnull".into());
        }
        Nullability::Annotated => {
            constraints.insert("nullable".into());
        }
    }
    for c in &p.type_constraints {
        // Nullability-elided: FCS cannot see a constraint row's `[Nullable]`
        // (see [`render_constraint_type`]), so the shared currency cannot carry
        // it. The Rust-side decode is pinned directly instead.
        constraints.insert(render_constraint_type(c));
    }
    NormalisedGenericParameter {
        declaration,
        constraints,
    }
}

/// A module's `[<Literal>]` constant, elided from the diff: the projection surfaces it
/// (as the static literal field fsc emits) because FCS brings it into *scope* — `open M`
/// then bare `MaxValue` compiles — but `fcs-dump` renders a module's public surface as
/// its members-and-functions list, in which a literal does not appear. The elision
/// mirrors fcs-dump's rendering limit, exactly as the generic-extension one below does;
/// the owned model the LSP/sema consume carries the literal (and
/// `resolve_autoopen.rs::a_literal_in_an_opened_assembly_module_resolves` pins that it
/// resolves).
fn is_module_literal(kind: EntityKind, m: &Member) -> bool {
    // Every field the projector keeps on a module is a literal — a CLI-`Literal` one, or
    // a `decimal` carrying `[DecimalConstantAttribute]` (which the CLI cannot express as
    // a literal). Both are elided: fcs-dump renders neither.
    matches!((kind, m), (EntityKind::Module, Member::Field(_)))
}

/// A generic module method `fcs-dump` cannot mirror, elided from the diff on
/// both sides (its `isProjectableMethod` drops the identical set). Two
/// shapes:
///
/// - a *generic F#-native extension member* — `type T with member …` where
///   the method carries generic parameters (a generic method extending a
///   builder, or a generic *target* whose typars are lifted onto the
///   method): the fcs-dump extension-receiver rendering cannot thread the
///   augmented type's typars;
/// - a generic binding whose typar carries an **IL-visible constraint**
///   (`array2D`'s flexible `#seq` parameter → a coercion constraint row):
///   the fcs-dump FCS-surface typar rendering is name-only, so the Rust
///   side's IL-derived constraint tokens would one-sidedly diverge.
///
/// The owned model *keeps* both shapes, extension-flagged from the pickle
/// where applicable (name resolution and the overload extension gate need
/// them — see `projector_fsharp_core.rs`); the differential elides them so
/// both sides compare the mirrorable subset. Plain unconstrained generic
/// module `let`s are NOT elided: both projectors surface them (fcs-dump
/// renders their typars from the FCS public surface, and the IL-*erased*
/// constraint kinds — SRTP, comparison/equality — are invisible on both
/// sides).
///
/// Known asymmetry (unexercised): an F# `[<Extension>]`-attributed generic
/// `let` sets the CLR-attribute extension flag on the Rust side (elided
/// here) but has `IsExtensionMember = false` on the FCS side (kept there);
/// no fixture declares one.
fn is_unmirrorable_generic_module_method(kind: EntityKind, m: &Member) -> bool {
    let Member::Method(method) = m else {
        return false;
    };
    if !matches!(kind, EntityKind::Module) || method.generic_parameters.is_empty() {
        return false;
    }
    let il_visible_constraint = method.generic_parameters.iter().any(|p| {
        p.reference_type_constraint
            || p.value_type_constraint
            || p.default_constructor_constraint
            || p.is_unmanaged
            || p.allows_ref_struct
            || !p.type_constraints.is_empty()
    });
    method.is_extension_method || il_visible_constraint
}

fn member_access(m: &Member) -> Access {
    match m {
        Member::Method(meth) => meth.access,
        Member::Field(f) => f.access,
        Member::Property(p) => p.access,
        Member::Event(e) => e.access,
    }
}

/// Mirror FCS's `AccessibleFromSomeFSharpCode` predicate so the projected
/// visibility surface matches what `fcs-dump` emits.
///
/// FCS obtains members through `MembersFunctionsAndValues`, which calls
/// `GetImmediateIntrinsicMethInfosOfType` with `AccessibleFromSomeFSharpCode`.
/// That predicate keeps `Public`, `Family` (protected), and
/// `FamilyOrAssembly` (protected internal); it drops `Private`,
/// `Assembly` (internal), and `FamilyAndAssembly` (private protected).
/// See `AccessibilityLogic.fs:IsILMemberAccessible` in the F# compiler.
///
/// The Rust importer itself keeps emitting all members — an LSP needs to
/// see private declarations for go-to-definition within an assembly —
/// but the diff oracle compares only what both sides can observe.
fn accessible_from_some_fsharp_code(a: Access) -> bool {
    match a {
        Access::Public | Access::Protected | Access::ProtectedOrInternal => true,
        Access::Private | Access::Internal | Access::ProtectedAndInternal => false,
    }
}

fn normalise_member(m: &Member) -> NormalisedMember {
    match m {
        Member::Method(m) => normalise_method(m),
        Member::Field(f) => NormalisedMember {
            kind: "Field".into(),
            name: f.name.clone(),
            signature: format!(
                "{}{}",
                render_type(&f.ty),
                nullability_suffix(f.nullability)
            ),
            access: access_str(f.access).into(),
            flags: {
                let mut s = BTreeSet::new();
                s.insert(if f.is_static { "static" } else { "instance" }.into());
                if f.is_init_only {
                    s.insert("init_only".into());
                }
                if f.is_volatile {
                    s.insert("volatile".into());
                }
                if f.is_required {
                    s.insert("required".into());
                }
                for g in &f.compiler_feature_required {
                    s.insert(format_compiler_feature_required(g));
                }
                s
            },
            generic_parameters: vec![],
            obsolete: None,
            experimental: None,
        },
        Member::Property(p) => normalise_property(p),
        Member::Event(e) => normalise_event(e),
    }
}

fn normalise_event(e: &Event) -> NormalisedMember {
    let mut flags = BTreeSet::new();
    flags.insert(if e.is_static { "static" } else { "instance" }.into());
    flags.insert("add".into());
    flags.insert("remove".into());
    if e.has_fire {
        flags.insert("fire".into());
    }
    NormalisedMember {
        kind: "Event".into(),
        name: e.name.clone(),
        signature: format!(
            "{}{}",
            render_type(&e.delegate_type),
            nullability_suffix(e.nullability)
        ),
        access: access_str(e.access).into(),
        flags,
        generic_parameters: vec![],
        obsolete: None,
        experimental: None,
    }
}

fn normalise_property(p: &Property) -> NormalisedMember {
    let mut flags = BTreeSet::new();
    flags.insert(if p.is_static { "static" } else { "instance" }.into());
    if p.has_getter {
        flags.insert("get".into());
    }
    if p.has_setter {
        flags.insert("set".into());
    }
    if p.is_required {
        flags.insert("required".into());
    }
    for g in &p.compiler_feature_required {
        flags.insert(format_compiler_feature_required(g));
    }
    NormalisedMember {
        kind: "Property".into(),
        name: p.name.clone(),
        signature: render_property_signature(p),
        access: access_str(p.access).into(),
        flags,
        generic_parameters: vec![],
        obsolete: None,
        experimental: None,
    }
}

/// Render a property's signature for the diff string. An ordinary property
/// is just its type (with the nullability suffix); an indexer (non-empty
/// `parameters`) renders in a bracketed `[T1, T2] -> Ret` shape that reads
/// as an index dimension and is distinct from the method `(…) -> …` form.
/// Each index parameter carries its nullability suffix (plan B3), sourced
/// from the getter parameter on both projectors, so the bracketed shape
/// reads e.g. `[System.String?] -> System.String?`. Index parameter *names*
/// are deliberately not rendered — FCS and our reader source them
/// differently and the differential compares only the index *types*.
fn render_property_signature(p: &Property) -> String {
    let ret = format!(
        "{}{}",
        render_type(&p.ty),
        nullability_suffix(p.nullability)
    );
    if p.parameters.is_empty() {
        ret
    } else {
        let params = p
            .parameters
            .iter()
            .map(|ip| render_nullable_type(&ip.ty))
            .collect::<Vec<_>>()
            .join(", ");
        format!("[{params}] -> {ret}")
    }
}

fn normalise_method(m: &MethodLike) -> NormalisedMember {
    let mut flags = BTreeSet::new();
    flags.insert(if m.is_static { "static" } else { "instance" }.into());
    if m.is_virtual {
        flags.insert("virtual".into());
    }
    if m.is_abstract {
        flags.insert("abstract".into());
    }
    if m.is_constructor {
        flags.insert("constructor".into());
    }
    if m.is_extension_method {
        flags.insert("extension".into());
    }
    if m.sets_required_members {
        flags.insert("sets_required_members".into());
    }
    for g in &m.compiler_feature_required {
        flags.insert(format_compiler_feature_required(g));
    }
    NormalisedMember {
        kind: "Method".into(),
        name: m.name.clone(),
        signature: render_signature(&m.signature),
        access: access_str(m.access).into(),
        flags,
        generic_parameters: m.generic_parameters.iter().map(normalise_typar).collect(),
        obsolete: m.obsolete.as_ref().map(format_obsolete),
        experimental: m.experimental.as_ref().map(format_experimental),
    }
}

fn sort_entities(v: &mut [NormalisedEntity]) {
    v.sort_by(|a, b| {
        // The backtick arity suffix is stripped from `fqn` (the type-arg
        // count is implied by `generic_parameters`), so `Foo` and `Foo<T>`
        // — both projected to `MyLib.Foo` — would compare equal here and
        // sort in input order. Tie-break on arity + declarations so the
        // two normalised assemblies sort to the same place regardless of
        // which projector emitted what order.
        a.fqn
            .cmp(&b.fqn)
            .then_with(|| a.generic_parameters.len().cmp(&b.generic_parameters.len()))
            .then_with(|| {
                a.generic_parameters
                    .iter()
                    .map(|g| &g.declaration)
                    .cmp(b.generic_parameters.iter().map(|g| &g.declaration))
            })
    });
}

fn sort_members(v: &mut [NormalisedMember]) {
    v.sort_by(|a, b| {
        a.kind
            .cmp(&b.kind)
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.signature.cmp(&b.signature))
            // Tie-break on generic arity + declarations so legal IL
            // overloads like `void M()` vs `void M<T>()` — which share
            // `kind` / `name` / `signature` — sort deterministically;
            // otherwise the diff would become input-order-sensitive
            // again. Compare arity first so the cheap `len()` short-
            // circuits the common case, then declarations for the
            // (rare) same-arity overload pair like `M<T>()` and `M<U>()`.
            .then_with(|| a.generic_parameters.len().cmp(&b.generic_parameters.len()))
            .then_with(|| {
                a.generic_parameters
                    .iter()
                    .map(|g| &g.declaration)
                    .cmp(b.generic_parameters.iter().map(|g| &g.declaration))
            })
    });
}

fn fqn(namespace: &[String], name: &str) -> String {
    if namespace.is_empty() {
        name.to_string()
    } else {
        format!("{}.{name}", namespace.join("."))
    }
}

/// Render an entity's kind together with its struct-flavour markers
/// (`is_readonly` / `is_byref_like` / `is_struct`). Order matches C# 11's
/// surface syntax (`readonly ref struct`) so the rendering is
/// self-explanatory; `fcs-dump` mirrors this on its side. `readonly`
/// and `ref` apply to any entity kind — the renderer trusts the bits
/// and prepends regardless. The `struct` marker is special: it only
/// fires when `is_struct` is set AND the base kind would otherwise
/// hide the struct-ness ([`EntityKind::Record`] / [`EntityKind::Union`]
/// / etc.). For [`EntityKind::Struct`] the prefix would be redundant
/// ("struct Struct"), and [`EntityKind::Enum`] is already a value type
/// at the source level, so both are suppressed.
fn format_entity_kind(e: &Entity) -> String {
    let mut prefix = String::new();
    if e.is_readonly {
        prefix.push_str("readonly ");
    }
    if e.is_byref_like {
        prefix.push_str("ref ");
    }
    if e.is_struct && !matches!(e.kind, EntityKind::Struct | EntityKind::Enum) {
        prefix.push_str("struct ");
    }
    if e.is_auto_open {
        prefix.push_str("auto_open ");
    }
    if e.is_require_qualified_access {
        prefix.push_str("require_qualified_access ");
    }
    if e.is_no_equality {
        prefix.push_str("no_equality ");
    }
    if e.is_no_comparison {
        prefix.push_str("no_comparison ");
    }
    if e.is_structural_equality {
        prefix.push_str("structural_equality ");
    }
    if e.is_structural_comparison {
        prefix.push_str("structural_comparison ");
    }
    if e.is_allow_null_literal {
        prefix.push_str("allow_null_literal ");
    }
    prefix.push_str(entity_kind_str(e.kind));
    prefix
}

/// Render an [`Obsolete`] for the diff harness. Both projectors emit
/// this exact string, so a mismatch shows up as a one-line diff in
/// failure output rather than two structs to eyeball-compare.
///
/// - `Obsolete { message: None, is_error: false }`        → `"[obsolete]"`
/// - `Obsolete { message: Some(m), is_error: false }`     → `"[obsolete: m]"`
/// - `Obsolete { message: None, is_error: true }`         → `"[obsolete error]"`
/// - `Obsolete { message: Some(m), is_error: true }`      → `"[obsolete error: m]"`
pub fn format_obsolete(o: &Obsolete) -> String {
    match (o.is_error, o.message.as_deref()) {
        (false, None) => "[obsolete]".to_string(),
        (false, Some(m)) => format!("[obsolete: {m}]"),
        (true, None) => "[obsolete error]".to_string(),
        (true, Some(m)) => format!("[obsolete error: {m}]"),
    }
}

/// Render a [`DefaultMember`] for the diff harness. Mirrors
/// [`format_obsolete`]: both projectors emit this exact string so a
/// mismatch is a one-line diff in failure output. The bracketed prefix
/// keeps it visually distinct from `obsolete` / `experimental` and from
/// member-signature strings.
///
/// - [`DefaultMember::Named`] → `[default-member: <name>]`.
/// - [`DefaultMember::Unknown`] → `[default-member]` (the degraded
///   shape, mirroring `[obsolete]`). The importer doesn't yet produce
///   `Unknown` — it refuses loud on those payloads — but the rendering
///   is defined now so a future relaxation doesn't have to invent it
///   (and so `fcs-dump`'s `tryFormatDefaultMember` has a fixed target to
///   mirror when that lands).
pub fn format_default_member(d: &DefaultMember) -> String {
    match d {
        DefaultMember::Named(name) => format!("[default-member: {name}]"),
        DefaultMember::Unknown => "[default-member]".to_string(),
    }
}

/// Render an [`Experimental`] for the diff harness. Mirrors
/// [`format_obsolete`]: both projectors emit this exact string so a
/// mismatch is a one-line diff in failure output.
///
/// The shape is `[experimental]` for the all-`None` (degraded) case and
/// `[experimental k1=v1, k2=v2]` otherwise, where keys appear in fixed
/// order — `id`, `url`, `message` — so two different orderings can't
/// produce a spurious diff.
pub fn format_experimental(e: &Experimental) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(id) = &e.diagnostic_id {
        parts.push(format!("id={id}"));
    }
    if let Some(url) = &e.url_format {
        parts.push(format!("url={url}"));
    }
    if let Some(msg) = &e.message {
        parts.push(format!("message={msg}"));
    }
    if parts.is_empty() {
        "[experimental]".to_string()
    } else {
        format!("[experimental {}]", parts.join(", "))
    }
}

/// Render a [`CompilerFeatureRequired`] for the diff harness. Mirrors
/// [`format_obsolete`] / [`format_default_member`]: both projectors emit
/// this exact string so a mismatch is a one-line diff in failure output.
///
/// Shape: `[compiler-feature-required: <feature>]`, with ` (optional)`
/// appended when `IsOptional = true`. The bracketed prefix keeps it
/// visually distinct from the plain flag tokens (`static`, `required`, …)
/// it sits alongside in a member's `flags` set, and from the entity's
/// dedicated `compiler_feature_required` set. `AllowMultiple = true` on the
/// attribute means a position can carry several gates; each renders to its
/// own string and they fold into a set, where ordering is irrelevant.
pub fn format_compiler_feature_required(g: &CompilerFeatureRequired) -> String {
    if g.is_optional {
        format!("[compiler-feature-required: {} (optional)]", g.feature)
    } else {
        format!("[compiler-feature-required: {}]", g.feature)
    }
}

fn entity_kind_str(k: EntityKind) -> &'static str {
    match k {
        EntityKind::Class => "Class",
        EntityKind::Struct => "Struct",
        EntityKind::Interface => "Interface",
        EntityKind::Enum => "Enum",
        EntityKind::Delegate => "Delegate",
        EntityKind::Module => "Module",
        EntityKind::Union => "Union",
        EntityKind::Record => "Record",
        EntityKind::Abbreviation => "Abbreviation",
        EntityKind::Exception => "Exception",
        EntityKind::Measure => "Measure",
    }
}

fn access_str(a: Access) -> &'static str {
    match a {
        Access::Public => "Public",
        Access::Internal => "Internal",
        Access::Private => "Private",
        Access::Protected => "Protected",
        Access::ProtectedOrInternal => "ProtectedOrInternal",
        Access::ProtectedAndInternal => "ProtectedAndInternal",
    }
}

/// Render a [`TypeRef`] to a stable string. Format is hand-tuned to read
/// naturally in test failure output:
///
/// - primitives → the ECMA-335 primitive name (`System.Int32`, `System.Boolean`)
/// - named types → `Namespace.Name` with `<T, U>` for generic args
/// - generic vars → `!T0` (type) / `!!M0` (method), matching ildasm
/// - arrays → `T[]` for rank 1, `T[,]` for higher
/// - byref → `T&`, and a read-only byref (`in`/`ref readonly`) → `readonly T&`
///
/// Phase 4m.3: generic args and array elements are [`NullableType`]
/// wrappers, so each inner position renders as `<element-type><suffix>`
/// — `System.Collections.Generic.List<System.String?>` for
/// `List<string?>`. The outer position's suffix is still applied by the
/// per-position renderers (`render_parameter`, `render_field`, ...) from
/// the structural `nullability` field.
fn render_type(t: &TypeRef) -> String {
    render_type_inner(t, Nullable::Shown)
}

/// [`render_type`] for a *generic-parameter constraint*, which renders with
/// nullability **elided** at every depth.
///
/// Not a modelling gap on our side — the projector decodes a constraint row's
/// `[Nullable]` and carries the result (`tests/projector_generic_nullability.rs`
/// pins it against real Roslyn output). It is the *shared currency* that cannot
/// express it: FCS's IL surface hands `ILGenericParameterDef.Constraints` over
/// as bare `ILTypes` with no per-constraint custom attributes attached, so
/// `fcs-dump` has nothing to read a suffix from and always renders the bare
/// type. Emitting our suffix here would fail every diff over an assembly with an
/// annotated constraint for a reason that is not a divergence, so the token
/// elides it — like every other detail one side cannot see.
fn render_constraint_type(t: &TypeRef) -> String {
    render_type_inner(t, Nullable::Elided)
}

/// Whether a rendering shows inner nullability suffixes ([`render_type`]) or
/// elides them ([`render_constraint_type`]).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Nullable {
    Shown,
    Elided,
}

fn render_type_inner(t: &TypeRef, nullable: Nullable) -> String {
    match t {
        TypeRef::Primitive(p) => primitive_str(*p).into(),
        TypeRef::Named {
            namespace,
            name,
            type_args,
            ..
        } => {
            let mut s = fqn(namespace, name);
            if !type_args.is_empty() {
                s.push('<');
                for (i, a) in type_args.iter().enumerate() {
                    if i > 0 {
                        s.push_str(", ");
                    }
                    s.push_str(&render_nullable_type_inner(a, nullable));
                }
                s.push('>');
            }
            s
        }
        TypeRef::Var { index, is_method } => {
            if *is_method {
                format!("!!M{index}")
            } else {
                format!("!T{index}")
            }
        }
        // Per-dimension sizes / lower bounds are deliberately elided from the
        // differential string (like ranges and other trivia) — the comparison
        // currency keys on element type and rank. The owned model still carries
        // them faithfully.
        TypeRef::Array { element, rank, .. } => {
            let mut s = render_nullable_type_inner(element, nullable);
            s.push('[');
            for _ in 1..*rank {
                s.push(',');
            }
            s.push(']');
            s
        }
        TypeRef::Ptr(Some(inner)) => format!("{}*", render_type_inner(inner, nullable)),
        TypeRef::Ptr(None) => "void*".to_string(),
        // A read-only byref (`modreq(InAttribute)`) is distinguished: fcs-dump
        // reads the same modifier off the IL type and renders the same prefix,
        // so the diff *pins* the bit rather than eliding it.
        TypeRef::ByRef { inner, readonly } => {
            let prefix = if *readonly { "readonly " } else { "" };
            format!("{prefix}{}&", render_type_inner(inner, nullable))
        }
    }
}

fn render_nullable_type(nt: &NullableType) -> String {
    render_nullable_type_inner(nt, Nullable::Shown)
}

fn render_nullable_type_inner(nt: &NullableType, nullable: Nullable) -> String {
    let mut s = render_type_inner(&nt.ty, nullable);
    if nullable == Nullable::Shown {
        s.push_str(nullability_suffix(nt.nullability));
    }
    s
}

fn primitive_str(p: Primitive) -> &'static str {
    match p {
        Primitive::Void => "System.Void",
        Primitive::Bool => "System.Boolean",
        Primitive::Char => "System.Char",
        Primitive::I1 => "System.SByte",
        Primitive::U1 => "System.Byte",
        Primitive::I2 => "System.Int16",
        Primitive::U2 => "System.UInt16",
        Primitive::I4 => "System.Int32",
        Primitive::U4 => "System.UInt32",
        Primitive::I8 => "System.Int64",
        Primitive::U8 => "System.UInt64",
        Primitive::R4 => "System.Single",
        Primitive::R8 => "System.Double",
        Primitive::IntPtr => "System.IntPtr",
        Primitive::UIntPtr => "System.UIntPtr",
        Primitive::Object => "System.Object",
        Primitive::String => "System.String",
    }
}

fn render_signature(sig: &MethodSignature) -> String {
    let mut s = String::from("(");
    for (i, p) in sig.parameters.iter().enumerate() {
        if i > 0 {
            s.push_str(", ");
        }
        s.push_str(&render_parameter(p));
    }
    s.push_str(") -> ");
    s.push_str(&render_type(&sig.return_type));
    s.push_str(nullability_suffix(sig.return_nullability));
    s
}

fn render_parameter(p: &Parameter) -> String {
    let mut s = String::new();
    if p.is_out {
        s.push_str("out ");
    } else if p.is_byref {
        // `in`/`ref readonly` (a `modreq(InAttribute)` byref) against a plain
        // `ref`: fcs-dump reads the same modifier off the IL parameter type.
        s.push_str(if p.is_readonly_ref {
            "inref "
        } else {
            "byref "
        });
    } else if p.is_param_array {
        // `params T[]` from `[System.ParamArrayAttribute]`. Mutually
        // exclusive with `out`/`byref` — the attribute only sits on the
        // trailing value parameter, never on a byref one — so the
        // branches don't overlap.
        s.push_str("params ");
    }
    s.push_str(&render_type(&p.ty));
    s.push_str(nullability_suffix(p.nullability));
    // Any optional/default parameter (F# `?x` or a .NET optional) renders as
    // `T = ?` — the same shape FCS's `IsOptionalArg` produces in fcs-dump, so
    // the diff stays byte-identical across the `has_default` → `ParamDefault`
    // model change (which the human formatter, not this normaliser, consumes).
    if p.default != ParamDefault::None {
        s.push_str(" = ?");
    }
    s
}

/// Embed [`Nullability`] state inline with a type rendering (phase 4m.2):
/// `!` for `NotAnnotated`, `?` for `Annotated`, empty for `Oblivious`. The
/// suffix sits directly after the type — `System.String?`, `System.Int32`
/// (no suffix — value types stay oblivious), `System.String!` — so a
/// reader recognises the C# `T?` / `notnull` shape. Both projectors emit
/// the same suffix so the diff oracle compares position nullability as a
/// suffix on the rendered type string.
fn nullability_suffix(n: Nullability) -> &'static str {
    match n {
        Nullability::Oblivious => "",
        Nullability::NotAnnotated => "!",
        Nullability::Annotated => "?",
    }
}

// ============================================================================
// FCS-side projection: JSON -> NormalisedAssembly
// ============================================================================
//
// The JSON shape is contracted here, ahead of `fcs-dump`'s entity emitter.
// Phase 2 teaches the tool to produce this format; for now, tests
// hand-build small JSON literals.

pub fn parse_fcs_dump(json: &str) -> NormalisedAssembly {
    let dump: FcsDump = serde_json::from_str(json).expect("fcs-dump JSON shape");
    let mut entities: Vec<_> = dump.entities.into_iter().map(json_to_entity).collect();
    sort_entities(&mut entities);
    NormalisedAssembly {
        name: dump.assembly,
        entities,
    }
}

/// Key for [`fcs_abbreviation_targets`]: an abbreviation's fully-qualified name
/// paired with its generic arity. Both halves are load-bearing:
///
/// - The FQN has the *container path threaded back in* (see below), so a
///   module-nested abbreviation keys by its whole path and two same-named
///   aliases in different containers do not collide.
/// - The arity distinguishes the legal pair `type Alias = int` /
///   `type Alias<'T> = 'T list`, which `fcs-dump` emits under the **same** `Fqn`
///   but with different `GenericParameters`. A name-only key would let one
///   overwrite the other's target — and since a generic alias' target is often a
///   declined `None`, that would replace a renderable `Some` with a `None` the
///   differential then asserts nothing about.
pub type AbbreviationKey = (String, usize);

/// Extract every abbreviation entity's rendered immediate-logical target from a
/// `fcs-dump entities` JSON dump, keyed by [`AbbreviationKey`]. The value is
/// `Some(target)` when `fcs-dump` rendered one and `None` when it *declined* (a
/// structural/generic-instantiation shape the oracle does not yet model — see
/// `renderAbbreviationTargetLogical`), which mirrors the Rust decoder's own
/// fail-closed `None`. Nested entities are walked too, since a module-nested
/// abbreviation is a descendant entity.
///
/// This is a *separate extraction* from [`parse_fcs_dump`] on purpose: the
/// whole-tree [`NormalisedEntity`] comparison elides the target (an FCS-`Some` /
/// our-`None` asymmetry would otherwise break every diff before the decoder
/// lands), so the abbreviation-target differential reads the target through this
/// dedicated path instead.
pub fn fcs_abbreviation_targets(json: &str) -> BTreeMap<AbbreviationKey, Option<String>> {
    fn walk(prefix: &str, e: &FcsEntity, out: &mut BTreeMap<AbbreviationKey, Option<String>>) {
        // `fcs-dump`'s `Fqn` is namespace-qualified for a top-level entity but
        // only the `DisplayName` for a nested one (its parent is carried by the
        // JSON tree, not repeated into the child's `Fqn`). Rebuild the full path
        // by threading the container prefix.
        let fqn = if prefix.is_empty() {
            e.fqn.clone()
        } else {
            format!("{prefix}.{}", e.fqn)
        };
        // `entityKindString` renders flag prefixes into the kind string
        // (`[<AutoOpen>] type A = …` ⇒ `"auto_open Abbreviation"`), so match the
        // base kind by its final space-separated token rather than by equality —
        // otherwise every *attributed* abbreviation (the `TalliedAlias` fixture
        // among them) is silently skipped. Exception abbreviations
        // (`"Exception"`) carry no decoded target and are not collected.
        if e.kind.rsplit(' ').next() == Some("Abbreviation") {
            out.insert(
                (fqn.clone(), e.generic_parameters.len()),
                e.abbreviated_target.clone(),
            );
        }
        for n in &e.nested_types {
            walk(&fqn, n, out);
        }
    }
    let dump: FcsDump = serde_json::from_str(json).expect("fcs-dump JSON shape");
    let mut out = BTreeMap::new();
    for e in &dump.entities {
        walk("", e, &mut out);
    }
    out
}

/// Strip a trailing mangled-arity suffix (`` `N ``) from a tycon name segment,
/// so a head that already carries its arity (`` list`1 ``) does not double it
/// when the canonical arity is reapplied. Leaves a name without one untouched.
fn strip_backtick_arity(s: &str) -> &str {
    match s.rfind('`') {
        Some(tick) if tick + 1 < s.len() && s[tick + 1..].bytes().all(|b| b.is_ascii_digit()) => {
            &s[..tick]
        }
        _ => s,
    }
}

/// Render an owned [`AbbreviationTarget`] into the canonical string the
/// differential compares against `fcs-dump`'s `renderAbbreviationTargetLogical`.
/// The ccu is stored for sema but is **not** rendered — a same-assembly target
/// renders path-only, exactly as FCS does. The structural forms are
/// precedence-explicit (parenthesised tuples, a parenthesised function domain) so
/// the string is unambiguous.
pub fn render_abbreviation_target(t: &AbbreviationTarget) -> String {
    match t {
        AbbreviationTarget::Named { path, args, .. } => {
            if args.is_empty() {
                path.join(".")
            } else {
                // `int list` ⇒ `Microsoft.FSharp.Collections.list``1<Microsoft.FSharp.Core.int>`:
                // the tycon's logical path, its arity as a backtick suffix, then
                // the args. The pickle's head segment already carries the mangled
                // arity (`list``1`), so strip it before reapplying the canonical
                // one — otherwise the arity doubles (`list``1``1`). Arrays render
                // the same way through the `[]` tycon.
                let mut segs = path.clone();
                if let Some(last) = segs.last_mut() {
                    *last = strip_backtick_arity(last).to_string();
                }
                let inner = args
                    .iter()
                    .map(render_abbreviation_target)
                    .collect::<Vec<_>>()
                    .join(",");
                format!("{}`{}<{inner}>", segs.join("."), args.len())
            }
        }
        AbbreviationTarget::Var(pos) => format!("!T{pos}"),
        AbbreviationTarget::Fun(domain, range) => {
            // Right-associative: parenthesise a function-typed domain so
            // `(a -> b) -> c` stays distinct from `a -> b -> c`.
            let d = render_abbreviation_target(domain);
            let d = if matches!(**domain, AbbreviationTarget::Fun(..)) {
                format!("({d})")
            } else {
                d
            };
            format!("{d} -> {}", render_abbreviation_target(range))
        }
        AbbreviationTarget::Tuple { struct_kind, elems } => {
            let inner = elems
                .iter()
                .map(render_abbreviation_target)
                .collect::<Vec<_>>()
                .join(" * ");
            if *struct_kind {
                format!("struct ({inner})")
            } else {
                format!("({inner})")
            }
        }
    }
}

/// The Rust-side twin of [`fcs_abbreviation_targets`]: every
/// [`EntityKind::Abbreviation`] marker's decoded target, keyed by the same
/// [`AbbreviationKey`] `(fully-qualified name, generic arity)` so the two maps
/// compare directly. The value is `Some(rendered)` when the decoder produced a
/// target and `None` when it declined — mirroring the FCS side's `null`.
///
/// Keyed identically to the FCS walk: a top-level marker by its
/// namespace-qualified name, a nested marker by its container prefix threaded
/// back in (a nested [`Entity`] carries an empty namespace of its own).
pub fn our_abbreviation_targets(entities: &[Entity]) -> BTreeMap<AbbreviationKey, Option<String>> {
    fn walk(prefix: &str, e: &Entity, out: &mut BTreeMap<AbbreviationKey, Option<String>>) {
        let name = e.source_name.as_deref().unwrap_or(&e.name);
        let self_fqn = if prefix.is_empty() {
            fqn(&e.namespace, name)
        } else {
            format!("{prefix}.{name}")
        };
        if e.kind == EntityKind::Abbreviation {
            out.insert(
                (self_fqn.clone(), e.generic_parameters.len()),
                e.abbreviation_target
                    .as_ref()
                    .map(render_abbreviation_target),
            );
        }
        for n in &e.nested_types {
            walk(&self_fqn, n, out);
        }
    }
    let mut out = BTreeMap::new();
    for e in entities {
        walk("", e, &mut out);
    }
    out
}

fn json_to_entity(j: FcsEntity) -> NormalisedEntity {
    let mut members: Vec<_> = j.members.into_iter().map(json_to_member).collect();
    sort_members(&mut members);
    let mut nested: Vec<_> = j.nested_types.into_iter().map(json_to_entity).collect();
    sort_entities(&mut nested);
    NormalisedEntity {
        fqn: j.fqn,
        kind: j.kind,
        access: j.access,
        generic_parameters: j
            .generic_parameters
            .into_iter()
            .map(json_to_typar)
            .collect(),
        base_type: j.base_type,
        interfaces: j.interfaces,
        members,
        nested_types: nested,
        obsolete: j.obsolete,
        experimental: j.experimental,
        default_member: j.default_member,
        compiler_feature_required: j.compiler_feature_required.into_iter().collect(),
    }
}

fn json_to_member(j: FcsMember) -> NormalisedMember {
    NormalisedMember {
        kind: j.kind,
        name: j.name,
        signature: j.signature,
        access: j.access,
        flags: j.flags.into_iter().collect(),
        generic_parameters: j
            .generic_parameters
            .into_iter()
            .map(json_to_typar)
            .collect(),
        obsolete: j.obsolete,
        experimental: j.experimental,
    }
}

fn json_to_typar(j: FcsGenericParameter) -> NormalisedGenericParameter {
    NormalisedGenericParameter {
        declaration: j.declaration,
        constraints: j.constraints.into_iter().collect(),
    }
}

#[derive(Deserialize)]
struct FcsDump {
    #[serde(rename = "Assembly")]
    assembly: String,
    #[serde(rename = "Entities")]
    entities: Vec<FcsEntity>,
}

#[derive(Deserialize)]
struct FcsEntity {
    #[serde(rename = "Fqn")]
    fqn: String,
    #[serde(rename = "Kind")]
    kind: String,
    #[serde(rename = "Access")]
    access: String,
    #[serde(rename = "GenericParameters", default)]
    generic_parameters: Vec<FcsGenericParameter>,
    #[serde(rename = "BaseType")]
    base_type: Option<String>,
    #[serde(rename = "Interfaces")]
    interfaces: Vec<String>,
    #[serde(rename = "Members")]
    members: Vec<FcsMember>,
    #[serde(rename = "NestedTypes", default)]
    nested_types: Vec<FcsEntity>,
    /// Pre-rendered `[obsolete ...]` string emitted by `fcs-dump` when
    /// the entity carries `[<Obsolete>]`. See [`format_obsolete`].
    #[serde(rename = "Obsolete", default)]
    obsolete: Option<String>,
    /// Pre-rendered `[experimental ...]` string emitted by `fcs-dump`
    /// when the entity carries `[<Experimental>]`. See
    /// [`format_experimental`].
    #[serde(rename = "Experimental", default)]
    experimental: Option<String>,
    /// Pre-rendered `[default-member: ...]` string emitted by `fcs-dump`
    /// when the entity carries `[<DefaultMember(name)>]`. See
    /// [`format_default_member`].
    #[serde(rename = "DefaultMember", default)]
    default_member: Option<String>,
    /// Pre-rendered `[compiler-feature-required: ...]` strings emitted by
    /// `fcs-dump`, one per `[CompilerFeatureRequiredAttribute]` on the
    /// entity. See [`format_compiler_feature_required`].
    #[serde(rename = "CompilerFeatureRequired", default)]
    compiler_feature_required: Vec<String>,
    /// The immediate, unchased, *logical* abbreviation target `fcs-dump`
    /// renders for an `IsFSharpAbbreviation` entity (`type IntId = int` ⇒
    /// `"Microsoft.FSharp.Core.int"`), or `null` when the target is a shape the
    /// oracle declines to render (a structural/generic-instantiation shape — see
    /// `renderAbbreviationTargetLogical` in `tools/fcs-dump/Program.fs`). Always
    /// `null` for non-abbreviation entities. Deliberately NOT surfaced on
    /// [`NormalisedEntity`]: the whole-tree diff elides it (an FCS-`Some` /
    /// our-`None` asymmetry would otherwise break every diff before the Rust
    /// decoder lands); the abbreviation-target differential reads it through
    /// [`fcs_abbreviation_targets`] instead.
    #[serde(rename = "AbbreviatedTarget", default)]
    abbreviated_target: Option<String>,
}

#[derive(Deserialize)]
struct FcsMember {
    #[serde(rename = "Kind")]
    kind: String,
    #[serde(rename = "Name")]
    name: String,
    #[serde(rename = "Signature")]
    signature: String,
    #[serde(rename = "Access")]
    access: String,
    #[serde(rename = "Flags")]
    flags: Vec<String>,
    #[serde(rename = "GenericParameters", default)]
    generic_parameters: Vec<FcsGenericParameter>,
    /// See [`FcsEntity::obsolete`]. Populated for methods; the other
    /// member kinds always emit `null`.
    #[serde(rename = "Obsolete", default)]
    obsolete: Option<String>,
    /// See [`FcsEntity::experimental`]. Populated for methods; the other
    /// member kinds always emit `null`.
    #[serde(rename = "Experimental", default)]
    experimental: Option<String>,
}

#[derive(Deserialize)]
struct FcsGenericParameter {
    #[serde(rename = "Declaration")]
    declaration: String,
    #[serde(rename = "Constraints")]
    constraints: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `fcs_abbreviation_targets` must survive the three shapes a real dump
    /// throws at it: an abbreviation whose kind carries a rendered flag prefix
    /// (`auto_open Abbreviation`), a *nested* abbreviation whose `Fqn` is only its
    /// own `DisplayName`, and an arity-overloaded pair sharing one `Fqn`. A
    /// name-only or exact-kind extraction silently loses coverage on all three.
    #[test]
    fn abbreviation_targets_key_by_qualified_name_and_arity() {
        // `M` is a module (not collected); its three nested abbreviations plus
        // one top-level abbreviation are. `Dup`/`Dup<'T>` share an `Fqn` and must
        // not overwrite one another; `AutoAlias` carries the `auto_open` prefix
        // and a bare nested `Fqn` that must be re-qualified to `N.M.AutoAlias`.
        let json = r#"{
          "Assembly": "T",
          "Entities": [
            { "Fqn": "N.Top", "Kind": "Abbreviation", "Access": "public",
              "BaseType": null, "Interfaces": [], "Members": [],
              "AbbreviatedTarget": "Microsoft.FSharp.Core.int" },
            { "Fqn": "N.M", "Kind": "Module", "Access": "public",
              "BaseType": null, "Interfaces": [], "Members": [],
              "NestedTypes": [
                { "Fqn": "AutoAlias", "Kind": "auto_open Abbreviation",
                  "Access": "public", "BaseType": null, "Interfaces": [],
                  "Members": [], "AbbreviatedTarget": "N.M.Concrete" },
                { "Fqn": "Dup", "Kind": "Abbreviation", "Access": "public",
                  "BaseType": null, "Interfaces": [], "Members": [],
                  "AbbreviatedTarget": "Microsoft.FSharp.Core.int" },
                { "Fqn": "Dup", "Kind": "Abbreviation", "Access": "public",
                  "BaseType": null, "Interfaces": [], "Members": [],
                  "GenericParameters": [ { "Declaration": "T", "Constraints": [] } ],
                  "AbbreviatedTarget": null }
              ] }
          ]
        }"#;
        let targets = fcs_abbreviation_targets(json);

        // The module itself is not an abbreviation; exactly the four aliases are.
        assert_eq!(
            targets.len(),
            4,
            "expected exactly the four abbreviation entries, got {targets:#?}",
        );
        assert_eq!(
            targets.get(&("N.Top".to_string(), 0)),
            Some(&Some("Microsoft.FSharp.Core.int".to_string())),
        );
        // Prefix + nested container: `auto_open Abbreviation` is recognised and
        // the bare `AutoAlias` is re-qualified to `N.M.AutoAlias`.
        assert_eq!(
            targets.get(&("N.M.AutoAlias".to_string(), 0)),
            Some(&Some("N.M.Concrete".to_string())),
            "an attributed, nested abbreviation must still be collected under its \
             full path; got {targets:#?}",
        );
        // Arity keys the overloaded pair apart: the nullary `Some` is not
        // clobbered by the generic `None`.
        assert_eq!(
            targets.get(&("N.M.Dup".to_string(), 0)),
            Some(&Some("Microsoft.FSharp.Core.int".to_string())),
        );
        assert_eq!(targets.get(&("N.M.Dup".to_string(), 1)), Some(&None));
    }

    /// The canonical rendering of each `AbbreviationTarget` shape, pinned
    /// directly (the differential proves it matches fcs-dump; this proves the
    /// exact strings, and covers the struct-tuple form that has no `.fs` fixture
    /// because `type X = struct (…)` misparses as a struct-type definition).
    #[test]
    fn render_abbreviation_target_is_precedence_explicit() {
        fn named(path: &[&str], args: Vec<AbbreviationTarget>) -> AbbreviationTarget {
            AbbreviationTarget::Named {
                ccu: None,
                path: path.iter().map(|s| s.to_string()).collect(),
                args,
            }
        }
        let int = || named(&["Microsoft", "FSharp", "Core", "int"], vec![]);

        // Nullary named: path only.
        assert_eq!(
            render_abbreviation_target(&named(&["System", "String"], vec![])),
            "System.String",
        );
        // Generic app: path + backtick arity + `<args>`. The head segment carries
        // the mangled arity (`` list`1 ``, as the real pickle path does), which the
        // renderer strips before reapplying the canonical one — so the arity is
        // never doubled.
        assert_eq!(
            render_abbreviation_target(&named(
                &["Microsoft", "FSharp", "Collections", "list`1"],
                vec![int()],
            )),
            "Microsoft.FSharp.Collections.list`1<Microsoft.FSharp.Core.int>",
        );
        // Typar.
        assert_eq!(
            render_abbreviation_target(&AbbreviationTarget::Var(0)),
            "!T0"
        );
        // Function — no domain parens for a non-function domain.
        assert_eq!(
            render_abbreviation_target(&AbbreviationTarget::Fun(Box::new(int()), Box::new(int()),)),
            "Microsoft.FSharp.Core.int -> Microsoft.FSharp.Core.int",
        );
        // Nested function — the function-typed *domain* is parenthesised, so
        // `(a -> b) -> c` stays distinct from `a -> b -> c`.
        assert_eq!(
            render_abbreviation_target(&AbbreviationTarget::Fun(
                Box::new(AbbreviationTarget::Fun(Box::new(int()), Box::new(int()))),
                Box::new(int()),
            )),
            "(Microsoft.FSharp.Core.int -> Microsoft.FSharp.Core.int) -> Microsoft.FSharp.Core.int",
        );
        // Reference tuple and struct tuple — both parenthesised.
        assert_eq!(
            render_abbreviation_target(&AbbreviationTarget::Tuple {
                struct_kind: false,
                elems: vec![int(), int()],
            }),
            "(Microsoft.FSharp.Core.int * Microsoft.FSharp.Core.int)",
        );
        assert_eq!(
            render_abbreviation_target(&AbbreviationTarget::Tuple {
                struct_kind: true,
                elems: vec![int(), int()],
            }),
            "struct (Microsoft.FSharp.Core.int * Microsoft.FSharp.Core.int)",
        );
    }

    /// An unconstrained invariant `T` — the baseline the per-flag tests tweak.
    fn bare_typar() -> TypeParameter {
        TypeParameter {
            name: "T".to_string(),
            variance: Variance::Invariant,
            reference_type_constraint: false,
            value_type_constraint: false,
            default_constructor_constraint: false,
            is_unmanaged: false,
            allows_ref_struct: false,
            nullability: Nullability::Oblivious,
            type_constraints: vec![],
        }
    }

    #[test]
    fn allows_ref_struct_renders_as_a_standalone_token() {
        let p = TypeParameter {
            allows_ref_struct: true,
            ..bare_typar()
        };
        let n = normalise_typar(&p);
        assert!(
            n.constraints.contains("allows ref struct"),
            "the AllowByRefLike bit must surface as the `allows ref struct` token, got {:?}",
            n.constraints,
        );
        // It is an independent anti-constraint: nothing else is implied.
        assert_eq!(n.constraints.len(), 1);
    }

    #[test]
    fn no_allows_ref_struct_token_when_bit_clear() {
        let n = normalise_typar(&bare_typar());
        assert!(!n.constraints.contains("allows ref struct"));
        assert!(n.constraints.is_empty());
    }
}
