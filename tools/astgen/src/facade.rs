//! The per-version typed facades (`docs/ast-versioning-plan.md` D4, D6, D8):
//! one module per `LanguageVersion`, each owning every node type and enum of
//! its surface.
//!
//! A facade is generated from its **lock** (`crates/cst/facades/<module>.lock`,
//! [`crate::lock`]) and the union's lock, and nothing else, so its code is a
//! function of the two. For an unpublished facade the lock is itself derived
//! from the union's ([`derived_lock`]): every type whose kinds the feature
//! table's live `Kind` rows admit at the version, and every accessor whose owner
//! and return type survive. A published facade ([`PUBLISHED`]) reads its lock
//! from disk and never writes it.
//!
//! A version whose derived lock equals the one before it is an **alias**: its
//! lock is `alias-of <module>` and its module is `pub use super::<module>::*`,
//! the same types. `preview` is never an alias and never aliased, so a stable
//! facade's `Facade::VERSIONS` never names the unstable preview version.
//!
//! In the code, a node is a newtype over the union's node, and an accessor is
//! one line: call the union accessor its lock line delegates to, and re-cast
//! the result into the facade's types. A re-cast that can fail (a dispatch enum
//! narrower than the union's, a value enum one of whose variants can fail) drops
//! the element: an `Option` becomes `None`, an iterator or `Vec` skips it,
//! which is how a node outside the surface reads as an absent child. An
//! accessor returning such a type bare cannot drop it, and the generator
//! refuses it, as it refuses a facade type or accessor with no union
//! counterpart: those need a hand-written compat shim.

use std::collections::{BTreeMap, BTreeSet};

use crate::features::{self, FeatureRow};
use crate::lock::{self, Line, Lock, Payload};
use crate::shape::{Elem, Names, Shape};
use crate::{CATEGORIES, reformat};

/// One `LanguageVersion` and its facade module.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Version {
    /// The table's spelling, `"4.6"` … `"11.0"`, `"preview"`.
    pub text: &'static str,
    /// The `LanguageVersion` variant, `"V4_6"`.
    pub variant: &'static str,
    /// FCS's decimal for it (`preview` is 9999).
    pub decimal: f64,
}

impl Version {
    /// The facade module: `v4_6`, `v5`, `v10`, `preview`.
    pub fn module(&self) -> String {
        if self.is_preview() {
            return "preview".to_string();
        }
        let text = self.text.strip_suffix(".0").unwrap_or(self.text);
        format!("v{}", text.replace('.', "_"))
    }

    /// The cargo feature that exposes the module: the module's name, or
    /// `unstable-preview` for preview.
    pub fn cargo_feature(&self) -> String {
        if self.is_preview() {
            "unstable-preview".to_string()
        } else {
            self.module()
        }
    }

    /// Whether this is FCS's `preview`.
    pub fn is_preview(&self) -> bool {
        self.text == "preview"
    }

    /// The facade's lock, repo-relative.
    pub fn lock_path(&self) -> String {
        format!("crates/cst/facades/{}.lock", self.module())
    }

    /// The facade's code, repo-relative.
    pub fn code_path(&self) -> String {
        format!(
            "crates/cst/src/syntax/generated/facades/{}.rs",
            self.module()
        )
    }
}

/// Every language version, ascending: one facade each.
pub fn versions() -> Vec<Version> {
    features::VERSIONS
        .iter()
        .map(|&(text, variant, decimal)| Version {
            text,
            variant,
            decimal,
        })
        .collect()
}

/// The published facades, by module: generated from their checked-in locks,
/// which the generator reads and never writes. Empty until the freeze (plan
/// PR 7).
pub const PUBLISHED: &[&str] = &[];

/// The facade module declarations, repo-relative.
pub const FACADES_MOD_PATH: &str = "crates/cst/src/syntax/generated/facades/mod.rs";

/// Whether `node`'s kinds are all legal at `lang` by the live `Kind` rows.
fn kinds_in_surface(rows: &[FeatureRow], kinds: &[&String], lang: f64) -> bool {
    kinds
        .iter()
        .all(|k| features::live_kind_introduced(rows, k).is_none_or(|intro| lang >= intro))
}

/// The lock of `union` restricted to what is legal at `lang`: every node all
/// of whose kinds are, the enum and value-enum variants over what survives,
/// and the accessors whose owner and return type survive, each delegating to
/// the union accessor of its own name.
pub fn derived_lock(union: &Lock, rows: &[FeatureRow], lang: f64) -> Lock {
    let mut node_kinds: BTreeMap<&String, Vec<&String>> = BTreeMap::new();
    for line in &union.lines {
        if let Line::Node { node, kind } = line {
            node_kinds.entry(node).or_default().push(kind);
        }
    }
    let nodes: BTreeSet<&String> = node_kinds
        .iter()
        .filter(|(_, kinds)| kinds_in_surface(rows, kinds, lang))
        .map(|(node, _)| *node)
        .collect();
    let mut lines: BTreeSet<Line> = union
        .lines
        .iter()
        .filter(|line| match line {
            Line::Node { node, .. } => nodes.contains(node),
            Line::Enum { node, .. } => nodes.contains(node),
            _ => false,
        })
        .cloned()
        .collect();
    // A value-enum variant survives when every type its payload names does,
    // and a value enum may carry another, so iterate to the fixpoint.
    loop {
        let names = Lock {
            lines: lines.clone(),
        }
        .names();
        let before = lines.len();
        for line in &union.lines {
            if let Line::Value { payload, .. } = line
                && payload_resolves(payload, &names)
            {
                lines.insert(line.clone());
            }
        }
        if lines.len() == before {
            break;
        }
    }
    let names = Lock {
        lines: lines.clone(),
    }
    .names();
    for line in &union.lines {
        match line {
            Line::Derive { name, .. } if names.values.contains(name) => {
                lines.insert(line.clone());
            }
            Line::Accessor {
                owner, name, shape, ..
            } if names.nodes.contains(owner) && names.resolves(shape) => {
                lines.insert(Line::Accessor {
                    owner: owner.clone(),
                    name: name.clone(),
                    shape: shape.clone(),
                    delegate: Some(name.clone()),
                });
            }
            _ => {}
        }
    }
    Lock { lines }
}

fn payload_resolves(payload: &Payload, names: &Names) -> bool {
    match payload {
        Payload::Unit => true,
        Payload::Tuple(shapes) => shapes.iter().all(|s| names.resolves(s)),
        Payload::Named(fields) => fields.iter().all(|(_, s)| names.resolves(s)),
    }
}

/// The checked-in lock of the published facade `version`.
fn published_lock(version: &Version) -> Lock {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(version.lock_path());
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read the published lock {}: {e}", path.display()));
    lock::parse(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// Every facade's lock, ascending by version: the published ones as checked
/// in, the rest derived, with each numbered version whose lock equals the
/// current run's first an alias of it.
pub fn facade_locks(rows: &[FeatureRow], union: &Lock) -> Vec<(Version, Lock)> {
    facade_locks_with(rows, union, |v| {
        PUBLISHED
            .contains(&v.module().as_str())
            .then(|| published_lock(v))
    })
}

/// [`facade_locks`], with `published` giving a version's checked-in lock when
/// it has one.
pub fn facade_locks_with(
    rows: &[FeatureRow],
    union: &Lock,
    published: impl Fn(&Version) -> Option<Lock>,
) -> Vec<(Version, Lock)> {
    let mut out: Vec<(Version, Lock)> = Vec::new();
    // The module and lock of the current run of equal surfaces.
    let mut head: Option<(String, Lock)> = None;
    for version in versions() {
        let lock =
            published(&version).unwrap_or_else(|| derived_lock(union, rows, version.decimal));
        let errors = lock.check();
        assert!(
            errors.is_empty(),
            "the `{}` facade's lock is incoherent:\n  {}",
            version.module(),
            errors.join("\n  ")
        );
        let alias = lock.alias_of().map(str::to_string).or_else(|| {
            head.as_ref()
                .filter(|(_, head_lock)| !version.is_preview() && *head_lock == lock)
                .map(|(module, _)| module.clone())
        });
        match alias {
            Some(target) => out.push((
                version,
                Lock {
                    lines: [Line::AliasOf { target }].into(),
                },
            )),
            None => {
                head = Some((version.module(), lock.clone()));
                out.push((version, lock));
            }
        }
    }
    out
}

/// The versions a facade module serves: itself and its aliases.
fn served_versions<'a>(module: &str, locks: &'a [(Version, Lock)]) -> Vec<&'a Version> {
    locks
        .iter()
        .filter(|(v, lock)| v.module() == module || lock.alias_of() == Some(module))
        .map(|(v, _)| v)
        .collect()
}

/// Every generated facade file as `(repo-relative path, contents)`: each
/// unpublished facade's lock, each facade's code, and the module declarations.
/// Panics, listing them, on any reason a facade cannot be generated from its
/// lock.
pub fn outputs(rows: &[FeatureRow], union: &Lock) -> Vec<(String, String)> {
    let locks = facade_locks(rows, union);
    let mut out = Vec::new();
    for (version, lock) in &locks {
        if !PUBLISHED.contains(&version.module().as_str()) {
            out.push((version.lock_path(), lock::render(&version.module(), lock)));
        }
        let code = match lock.alias_of() {
            Some(target) => generate_alias(version, target),
            None => generate_facade(
                version,
                lock,
                union,
                &served_versions(&version.module(), &locks),
            )
            .unwrap_or_else(|errors| {
                panic!(
                    "the `{}` facade cannot be generated from its lock:\n  {}",
                    version.module(),
                    errors.join("\n  ")
                )
            }),
        };
        out.push((version.code_path(), code));
    }
    out.push((FACADES_MOD_PATH.to_string(), generate_mod(&locks)));
    out.push((MATRIX_PATH.to_string(), generate_matrix(&locks, union)));
    out
}

/// The property suite's table of every facade, repo-relative.
pub const MATRIX_PATH: &str = "crates/cst/tests/all/facade_properties/matrix.rs";

/// The property suite's table (`crates/cst/tests/all/facade_properties`):
/// for each version, its `Parse::view`; for each distinct facade, every node
/// type and dispatch enum (its casts, beside the union's) and every accessor
/// (its result, beside the union accessor's); and every union node type. The
/// macros it invokes are the suite's.
fn generate_matrix(locks: &[(Version, Lock)], union: &Lock) -> String {
    let mut out = format!(
        "{HEADER}//!\n\
         //! The facade property suite's table: every facade, node type, dispatch enum\n\
         //! and accessor, from the locks, so the properties range over all of them.\n\n\
         use borzoi_cst::language_version::LanguageVersion;\n\n\
         use super::{{Accessor, Cast, FacadeCase, UnionNode, View}};\n\n"
    );
    out.push_str("pub const VIEWS: &[View] = &[\n");
    for (version, lock) in locks {
        let module = version.module();
        let head = lock.alias_of().map_or(module.clone(), str::to_string);
        out.push_str(&format!(
            "view_case!({module}, {head}, LanguageVersion::{}),\n",
            version.variant
        ));
    }
    out.push_str("];\n\n");
    out.push_str("pub const UNION_NODES: &[UnionNode] = &[\n");
    for node in union.names().nodes {
        out.push_str(&format!("union_node!({node}),\n"));
    }
    out.push_str("];\n\n");
    out.push_str("pub const FACADES: &[FacadeCase] = &[\n");
    for (version, lock) in locks {
        if lock.alias_of().is_some() {
            continue;
        }
        let module = version.module();
        let served: Vec<String> = served_versions(&module, locks)
            .iter()
            .map(|v| format!("LanguageVersion::{}", v.variant))
            .collect();
        let names = lock.names();
        out.push_str(&format!(
            "FacadeCase {{\nmodule: \"{module}\",\nversions: &[{}],\ncasts: &[\n",
            served.join(", ")
        ));
        for name in names.nodes.iter().chain(&names.enums) {
            out.push_str(&format!("cast_case!({module}, {name}),\n"));
        }
        out.push_str("],\naccessors: &[\n");
        for line in &lock.lines {
            if let Line::Accessor {
                owner, name, shape, ..
            } = line
            {
                let form = if matches!(shape, Shape::Iter(_)) {
                    "iter"
                } else {
                    "one"
                };
                out.push_str(&format!(
                    "accessor_case!({module}, {owner}, {name}, {form}, \"{}\"),\n",
                    shape.render()
                ));
            }
        }
        out.push_str("],\n},\n");
    }
    out.push_str("];\n");
    reformat(&out)
}

const HEADER: &str = "//! GENERATED by `tools/astgen` — do not edit by hand.\n\
                      //! Regenerate with `cargo run -p borzoi-astgen --example generate`; kept honest\n\
                      //! by borzoi-astgen's `tests/up_to_date.rs` staleness test.\n";

/// `F# 8.0`, `F# preview`.
fn fsharp(version: &Version) -> String {
    format!("F# {}", version.text)
}

/// The facade module declarations: each distinct facade compiled when its own
/// feature or any alias's is on, each alias under its own.
fn generate_mod(locks: &[(Version, Lock)]) -> String {
    let mut out = format!(
        "{HEADER}//!\n\
         //! The per-version typed facades (`docs/ast-versioning-plan.md` D4, D8): one\n\
         //! module per `LanguageVersion`, each behind the cargo feature of its name\n\
         //! (`unstable-preview` for `preview`). An alias's feature also compiles, and\n\
         //! so exposes, the module it aliases.\n\n"
    );
    for (version, lock) in locks {
        let module = version.module();
        let features: Vec<String> = if lock.alias_of().is_some() {
            vec![version.cargo_feature()]
        } else {
            locks
                .iter()
                .filter(|(v, l)| v.module() == module || l.alias_of() == Some(module.as_str()))
                .map(|(v, _)| v.cargo_feature())
                .collect()
        };
        let cfg = if let [one] = &features[..] {
            format!("feature = \"{one}\"")
        } else {
            let all: Vec<String> = features
                .iter()
                .map(|f| format!("feature = \"{f}\""))
                .collect();
            format!("any({})", all.join(", "))
        };
        out.push_str(&format!("#[cfg({cfg})]\npub mod {module};\n"));
    }
    reformat(&out)
}

/// An alias facade: the earlier facade's types.
fn generate_alias(version: &Version, target: &str) -> String {
    reformat(&format!(
        "{HEADER}//!\n\
         //! The **{fsharp}** typed facade. Its surface equals [`{target}`](super::{target})'s,\n\
         //! so it is that module's types (`crates/cst/facades/{module}.lock` is\n\
         //! `alias-of {target}`).\n\n\
         pub use super::{target}::*;\n",
        fsharp = fsharp(version),
        module = version.module(),
    ))
}

/// What the union's lock says about each type and accessor, for the checks
/// and re-casts a facade's code needs.
struct Union<'a> {
    node_kinds: BTreeMap<&'a str, BTreeSet<&'a str>>,
    enum_variants: BTreeMap<&'a str, BTreeMap<&'a str, &'a str>>,
    value_variants: BTreeMap<&'a str, BTreeMap<&'a str, &'a Payload>>,
    accessors: BTreeMap<(&'a str, &'a str), &'a Shape>,
}

impl<'a> Union<'a> {
    fn new(lock: &'a Lock) -> Self {
        let mut u = Union {
            node_kinds: BTreeMap::new(),
            enum_variants: BTreeMap::new(),
            value_variants: BTreeMap::new(),
            accessors: BTreeMap::new(),
        };
        for line in &lock.lines {
            match line {
                Line::Node { node, kind } => {
                    u.node_kinds.entry(node).or_default().insert(kind);
                }
                Line::Enum {
                    name,
                    variant,
                    node,
                } => {
                    u.enum_variants
                        .entry(name)
                        .or_default()
                        .insert(variant, node);
                }
                Line::Value {
                    name,
                    variant,
                    payload,
                } => {
                    u.value_variants
                        .entry(name)
                        .or_default()
                        .insert(variant, payload);
                }
                Line::Accessor {
                    owner, name, shape, ..
                } => {
                    u.accessors.insert((owner, name), shape);
                }
                Line::AliasOf { .. } | Line::Derive { .. } => {}
            }
        }
        u
    }
}

/// A facade's own types, read from its lock.
struct Own<'a> {
    node_kinds: BTreeMap<&'a str, Vec<&'a str>>,
    enum_variants: BTreeMap<&'a str, Vec<(&'a str, &'a str)>>,
    value_variants: BTreeMap<&'a str, Vec<(&'a str, &'a Payload)>>,
    derives: BTreeMap<&'a str, Vec<&'a str>>,
    /// `(owner, [(method, shape, delegate)])`.
    accessors: BTreeMap<&'a str, Vec<(&'a str, &'a Shape, &'a str)>>,
}

impl<'a> Own<'a> {
    fn new(lock: &'a Lock) -> Self {
        let mut own = Own {
            node_kinds: BTreeMap::new(),
            enum_variants: BTreeMap::new(),
            value_variants: BTreeMap::new(),
            derives: BTreeMap::new(),
            accessors: BTreeMap::new(),
        };
        for line in &lock.lines {
            match line {
                Line::Node { node, kind } => own.node_kinds.entry(node).or_default().push(kind),
                Line::Enum {
                    name,
                    variant,
                    node,
                } => own
                    .enum_variants
                    .entry(name)
                    .or_default()
                    .push((variant, node)),
                Line::Value {
                    name,
                    variant,
                    payload,
                } => own
                    .value_variants
                    .entry(name)
                    .or_default()
                    .push((variant, payload)),
                Line::Derive { name, trait_ } => own.derives.entry(name).or_default().push(trait_),
                Line::Accessor {
                    owner,
                    name,
                    shape,
                    delegate,
                } => own.accessors.entry(owner).or_default().push((
                    name,
                    shape,
                    delegate.as_deref().unwrap_or(name),
                )),
                Line::AliasOf { .. } => {}
            }
        }
        own
    }
}

/// How the facade turns a union value into its own: `None` when the value is
/// the same type in both (a token or scalar), otherwise the function to apply,
/// and whether it can fail (returns an `Option`).
#[derive(Clone)]
struct Conv {
    func: Option<String>,
    partial: bool,
}

/// Re-cast planning for one facade: which conversions can fail, and which
/// types' `from_union` the code calls.
struct Plan<'a> {
    union: &'a Union<'a>,
    own: &'a Own<'a>,
    partial: BTreeMap<String, bool>,
    needed: BTreeSet<String>,
    errors: Vec<String>,
}

impl Plan<'_> {
    /// Whether converting a union value of the facade type `name` can fail.
    fn is_partial(&mut self, name: &str) -> bool {
        if let Some(&p) = self.partial.get(name) {
            return p;
        }
        // A type is never its own payload, but guard the recursion anyway.
        self.partial.insert(name.to_string(), true);
        let p = if let Some(kinds) = self.own.node_kinds.get(name) {
            let union_kinds = self.union.node_kinds.get(name);
            union_kinds.is_none_or(|u| u.len() != kinds.len())
        } else if let Some(variants) = self.own.enum_variants.get(name) {
            let union_count = self.union.enum_variants.get(name).map_or(0, BTreeMap::len);
            let variants = variants.clone();
            union_count != variants.len() || variants.iter().any(|(_, node)| self.is_partial(node))
        } else if let Some(variants) = self.own.value_variants.get(name) {
            let union_count = self.union.value_variants.get(name).map_or(0, BTreeMap::len);
            let variants = variants.clone();
            union_count != variants.len()
                || variants.iter().any(|(_, payload)| {
                    payload_shapes(payload)
                        .into_iter()
                        .any(|s| self.shape_drops(s))
                })
        } else {
            true
        };
        self.partial.insert(name.to_string(), p);
        p
    }

    /// Whether a payload field of shape `s` fails when its element does: only
    /// a bare element does (a container absorbs the failure).
    fn shape_drops(&mut self, s: &Shape) -> bool {
        match s {
            Shape::One(e) => self.elem(e).partial,
            Shape::Option(_) | Shape::Iter(_) | Shape::Vec(_) => false,
        }
    }

    /// The conversion of one element.
    fn elem(&mut self, e: &Elem) -> Conv {
        match e {
            Elem::Node(n) => {
                if self.is_partial(n) {
                    self.needed.insert(n.clone());
                    Conv {
                        func: Some(format!("{n}::from_union")),
                        partial: true,
                    }
                } else {
                    Conv {
                        func: Some(n.clone()),
                        partial: false,
                    }
                }
            }
            Elem::Enum(n) | Elem::Value(n) => {
                self.needed.insert(n.clone());
                Conv {
                    func: Some(format!("{n}::from_union")),
                    partial: self.is_partial(n),
                }
            }
            Elem::Tuple(items) => {
                let convs: Vec<Conv> = items.iter().map(|i| self.elem(i)).collect();
                if convs.iter().all(|c| c.func.is_none()) {
                    return Conv {
                        func: None,
                        partial: false,
                    };
                }
                let partial = convs.iter().any(|c| c.partial);
                let vars: Vec<String> = (0..convs.len()).map(|i| format!("x{i}")).collect();
                let parts: Vec<String> = convs
                    .iter()
                    .zip(&vars)
                    .map(|(c, v)| apply_one(c, v, partial))
                    .collect();
                let body = format!("({})", parts.join(", "));
                let body = if partial {
                    format!("Some({body})")
                } else {
                    body
                };
                Conv {
                    func: Some(format!("|({})| {body}", vars.join(", "))),
                    partial,
                }
            }
            Elem::Token
            | Elem::Bool
            | Elem::Usize
            | Elem::I32
            | Elem::TextRange
            | Elem::StaticStr => Conv {
                func: None,
                partial: false,
            },
        }
    }

    /// The expression re-casting `input`, a union value of shape `shape`, into
    /// the facade's; `inside_option` when the expression may use `?` (the
    /// enclosing function returns an `Option`).
    fn shape_expr(
        &mut self,
        shape: &Shape,
        input: &str,
        inside_option: bool,
        what: &str,
    ) -> String {
        match shape {
            Shape::One(e) => {
                let conv = self.elem(e);
                if conv.partial && !inside_option {
                    self.errors.push(format!(
                        "{what} returns `{}` bare, but the union's can hold a value \
                         outside this facade; it needs a compat shim",
                        shape.render()
                    ));
                }
                apply_one(&conv, input, inside_option)
            }
            Shape::Option(e) => {
                let conv = self.elem(e);
                match conv.func {
                    None => input.to_string(),
                    Some(f) if conv.partial => format!("{input}.and_then({f})"),
                    Some(f) => format!("{input}.map({f})"),
                }
            }
            Shape::Iter(e) => {
                let conv = self.elem(e);
                match conv.func {
                    None => input.to_string(),
                    Some(f) if conv.partial => format!("{input}.filter_map({f})"),
                    Some(f) => format!("{input}.map({f})"),
                }
            }
            Shape::Vec(e) => {
                let conv = self.elem(e);
                match conv.func {
                    None => input.to_string(),
                    Some(f) if conv.partial => {
                        format!("{input}.into_iter().filter_map({f}).collect()")
                    }
                    Some(f) => format!("{input}.into_iter().map({f}).collect()"),
                }
            }
        }
    }
}

/// `f(input)`, with `?` when `f` can fail and the caller propagates.
fn apply_one(conv: &Conv, input: &str, propagate: bool) -> String {
    match &conv.func {
        None => input.to_string(),
        Some(f) if f.starts_with('|') => {
            let call = format!("({f})({input})");
            if conv.partial && propagate {
                format!("{call}?")
            } else {
                call
            }
        }
        Some(f) if conv.partial && propagate => format!("{f}({input})?"),
        Some(f) => format!("{f}({input})"),
    }
}

fn payload_shapes(payload: &Payload) -> Vec<&Shape> {
    match payload {
        Payload::Unit => Vec::new(),
        Payload::Tuple(shapes) => shapes.iter().collect(),
        Payload::Named(fields) => fields.iter().map(|(_, s)| s).collect(),
    }
}

/// The Rust spelling of an element in the facade's namespace.
fn elem_type(e: &Elem) -> String {
    match e {
        Elem::Node(n) | Elem::Enum(n) | Elem::Value(n) => n.clone(),
        Elem::Token => "crate::syntax::SyntaxToken".to_string(),
        Elem::Bool => "bool".to_string(),
        Elem::Usize => "usize".to_string(),
        Elem::I32 => "i32".to_string(),
        Elem::TextRange => "crate::syntax::TextRange".to_string(),
        Elem::StaticStr => "&'static str".to_string(),
        Elem::Tuple(items) => {
            let items: Vec<String> = items.iter().map(elem_type).collect();
            format!("({})", items.join(", "))
        }
    }
}

/// The Rust spelling of a shape in the facade's namespace.
fn shape_type(s: &Shape) -> String {
    match s {
        Shape::One(e) => elem_type(e),
        Shape::Option(e) => format!("Option<{}>", elem_type(e)),
        Shape::Iter(e) => format!("impl Iterator<Item = {}> + '_", elem_type(e)),
        Shape::Vec(e) => format!("Vec<{}>", elem_type(e)),
    }
}

/// The value enum `name`'s doc and its variants' docs, from astgen's tables
/// (docs are not part of a lock).
fn value_docs(name: &str) -> Option<&'static crate::ValueEnum> {
    CATEGORIES
        .iter()
        .flat_map(|c| c.value_enums)
        .find(|v| v.name == name)
}

fn push_doc_lines(doc: &[&str], indent: &str, out: &mut String) {
    for line in doc {
        if line.is_empty() {
            out.push_str(&format!("{indent}///\n"));
        } else {
            out.push_str(&format!("{indent}/// {line}\n"));
        }
    }
}

/// The union-lock checks a facade type or accessor must pass to be re-cast
/// mechanically.
fn check_against_union(own: &Own<'_>, union: &Union<'_>, errors: &mut Vec<String>) {
    for (node, kinds) in &own.node_kinds {
        match union.node_kinds.get(node) {
            None => errors.push(format!("node `{node}` is not in the union")),
            Some(u) => {
                for kind in kinds {
                    if !u.contains(kind) {
                        errors.push(format!(
                            "node `{node}` casts `{kind}`, which the union's `{node}` does not"
                        ));
                    }
                }
            }
        }
    }
    for (name, variants) in &own.enum_variants {
        for (variant, node) in variants {
            if union.enum_variants.get(name).and_then(|v| v.get(variant)) != Some(node) {
                errors.push(format!(
                    "`{name}::{variant}({node})` is not a variant of the union's `{name}`"
                ));
            }
        }
    }
    for (name, variants) in &own.value_variants {
        for (variant, payload) in variants {
            if union.value_variants.get(name).and_then(|v| v.get(variant)) != Some(payload) {
                errors.push(format!(
                    "value `{name}::{variant}` is not the union's (same name and payload)"
                ));
            }
        }
    }
    for (owner, accessors) in &own.accessors {
        for (name, shape, delegate) in accessors {
            if union.accessors.get(&(*owner, *delegate)) != Some(shape) {
                errors.push(format!(
                    "accessor `{owner}::{name}` delegates to `union.{delegate}`, which the \
                     union's `{owner}` lacks or declares with another shape"
                ));
            }
        }
    }
}

/// The code of the distinct facade `version` from its lock and the union's,
/// serving `served` (itself and its aliases) through `Parse::view`; or every
/// reason it cannot be generated mechanically.
pub fn generate_facade(
    version: &Version,
    lock: &Lock,
    union_lock: &Lock,
    served: &[&Version],
) -> Result<String, Vec<String>> {
    let union = Union::new(union_lock);
    let own = Own::new(lock);
    let mut errors = Vec::new();
    check_against_union(&own, &union, &mut errors);
    if !own.enum_variants.contains_key("ParsedInput") {
        errors.push("the facade has no `ParsedInput`, the root `Parse::view` returns".into());
    }
    let mut plan = Plan {
        union: &union,
        own: &own,
        partial: BTreeMap::new(),
        needed: BTreeSet::new(),
        errors: Vec::new(),
    };

    // Accessors first: they decide which types' `from_union` the code needs.
    let mut accessors = String::new();
    for (owner, methods) in &own.accessors {
        accessors.push_str(&format!("impl {owner} {{\n"));
        for (name, shape, delegate) in methods {
            let body = plan.shape_expr(
                shape,
                &format!("self.0.{delegate}()"),
                false,
                &format!("accessor `{owner}::{name}`"),
            );
            accessors.push_str(&format!(
                "pub fn {name}(&self) -> {} {{ {body} }}\n",
                shape_type(shape)
            ));
        }
        accessors.push_str("}\n\n");
    }

    // Each type's `from_union`, for every type the code converts, closing over
    // the types those convert in turn.
    let mut conversions = String::new();
    let mut done: BTreeSet<String> = BTreeSet::new();
    while let Some(name) = plan.needed.iter().find(|n| !done.contains(*n)).cloned() {
        done.insert(name.clone());
        let partial = plan.is_partial(&name);
        let ret = if partial { "Option<Self>" } else { "Self" };
        let wrap = |body: String| {
            if partial {
                format!("Some({body})")
            } else {
                body
            }
        };
        if own.node_kinds.contains_key(name.as_str()) {
            // Only a partial node is ever needed: a total one is its constructor.
            conversions.push_str(&format!(
                "impl {name} {{\n\
                 pub(crate) fn from_union(node: u::{name}) -> Option<Self> {{\n\
                 Self::can_cast(node.syntax().kind()).then_some(Self(node))\n}}\n}}\n\n"
            ));
        } else if let Some(variants) = own.enum_variants.get(name.as_str()) {
            let mut arms = String::new();
            for (variant, node) in variants {
                let conv = plan.elem(&Elem::Node(node.to_string()));
                arms.push_str(&format!(
                    "u::{name}::{variant}(n) => {name}::{variant}({}),\n",
                    apply_one(&conv, "n", partial)
                ));
            }
            if union
                .enum_variants
                .get(name.as_str())
                .map_or(0, BTreeMap::len)
                != variants.len()
            {
                arms.push_str("_ => return None,\n");
            }
            conversions.push_str(&format!(
                "impl {name} {{\n\
                 pub(crate) fn from_union(value: u::{name}) -> {ret} {{\n{}\n}}\n}}\n\n",
                wrap(format!("match value {{\n{arms}}}"))
            ));
        } else if let Some(variants) = own.value_variants.get(name.as_str()) {
            let mut arms = String::new();
            for (variant, payload) in variants {
                let what = format!("value `{name}::{variant}`");
                match payload {
                    Payload::Unit => {
                        arms.push_str(&format!("u::{name}::{variant} => {name}::{variant},\n"));
                    }
                    Payload::Tuple(shapes) => {
                        let vars: Vec<String> =
                            (0..shapes.len()).map(|i| format!("x{i}")).collect();
                        let exprs: Vec<String> = shapes
                            .iter()
                            .zip(&vars)
                            .map(|(s, v)| plan.shape_expr(s, v, partial, &what))
                            .collect();
                        arms.push_str(&format!(
                            "u::{name}::{variant}({}) => {name}::{variant}({}),\n",
                            vars.join(", "),
                            exprs.join(", ")
                        ));
                    }
                    Payload::Named(fields) => {
                        let names: Vec<&str> = fields.iter().map(|(f, _)| f.as_str()).collect();
                        let exprs: Vec<String> = fields
                            .iter()
                            .map(|(f, s)| {
                                let expr = plan.shape_expr(s, f, partial, &what);
                                if expr == *f {
                                    f.clone()
                                } else {
                                    format!("{f}: {expr}")
                                }
                            })
                            .collect();
                        arms.push_str(&format!(
                            "u::{name}::{variant} {{ {} }} => {name}::{variant} {{ {} }},\n",
                            names.join(", "),
                            exprs.join(", ")
                        ));
                    }
                }
            }
            if union
                .value_variants
                .get(name.as_str())
                .map_or(0, BTreeMap::len)
                != variants.len()
            {
                arms.push_str("_ => return None,\n");
            }
            conversions.push_str(&format!(
                "impl {name} {{\n\
                 pub(crate) fn from_union(value: u::{name}) -> {ret} {{\n{}\n}}\n}}\n\n",
                wrap(format!("match value {{\n{arms}}}"))
            ));
        }
    }
    errors.append(&mut plan.errors);
    if !errors.is_empty() {
        return Err(errors);
    }

    let module = version.module();
    let served_text: Vec<String> = served.iter().map(|v| fsharp(v)).collect();
    let aliases: Vec<String> = served[1..]
        .iter()
        .map(|v| format!("`{}`", v.module()))
        .collect();
    let alias_note = if aliases.is_empty() {
        String::new()
    } else {
        format!(
            "\n//! {} {} the same surface, so {} this module's types.",
            aliases.join(", "),
            if aliases.len() == 1 { "has" } else { "have" },
            if aliases.len() == 1 {
                "it re-exports"
            } else {
                "they re-export"
            },
        )
    };
    let mut out = format!(
        "{HEADER}//!\n//! Generated from `crates/cst/facades/{module}.lock` and the union's lock.\n\
         //!\n\
         //! The **{fsharp}** typed facade (`docs/ast-versioning-plan.md` D4): every node\n\
         //! type and enum of the {served} surface, each a newtype over the union's, with\n\
         //! accessors that re-cast the union's results. A node outside the surface\n\
         //! reads as an absent child. Read a parse through it with\n\
         //! [`Parse::view`](crate::parser::Parse::view).{alias_note}\n\n\
         use crate::language_version::LanguageVersion;\n\
         use crate::syntax::union as u;\n\
         use crate::syntax::{{AstNode, SyntaxKind, SyntaxNode}};\n\n",
        fsharp = fsharp(version),
        served = served_text.join(" / "),
    );

    for (node, kinds) in &own.node_kinds {
        out.push_str(&format!(
            "crate::syntax::facade_support::facade_node!({node}: {});\n",
            kinds.join(" | ")
        ));
    }
    out.push('\n');

    for (name, variants) in &own.enum_variants {
        let with_kinds: Vec<(&str, &str, Vec<String>)> = variants
            .iter()
            .map(|(variant, node)| {
                let kinds = own.node_kinds[node].iter().map(|k| k.to_string()).collect();
                (*variant, *node, kinds)
            })
            .collect();
        crate::gen_enum_with_kinds(name, &with_kinds, &mut out);
        let arms: Vec<String> = variants
            .iter()
            .map(|(variant, _)| {
                format!("{name}::{variant}(n) => u::{name}::{variant}(n.to_union()),")
            })
            .collect();
        out.push_str(&format!(
            "\nimpl {name} {{\n\
             /// This value as the union's [`{name}`](crate::syntax::{name}).\n\
             #[cfg(feature = \"unstable-union\")]\n\
             pub fn to_union(&self) -> u::{name} {{\nmatch self {{\n{}\n}}\n}}\n}}\n\n",
            arms.join("\n")
        ));
    }

    for (name, variants) in &own.value_variants {
        let docs = value_docs(name);
        if let Some(docs) = docs {
            push_doc_lines(docs.doc, "", &mut out);
        }
        out.push_str(&format!(
            "#[derive({})]\npub enum {name} {{\n",
            own.derives
                .get(name)
                .map(|d| d.join(", "))
                .unwrap_or_default()
        ));
        for (variant, payload) in variants {
            if let Some(v) = docs.and_then(|d| d.variants.iter().find(|v| v.name == *variant)) {
                push_doc_lines(v.doc, "    ", &mut out);
            }
            match payload {
                Payload::Unit => out.push_str(&format!("    {variant},\n")),
                Payload::Tuple(shapes) => {
                    let types: Vec<String> = shapes.iter().map(shape_type).collect();
                    out.push_str(&format!("    {variant}({}),\n", types.join(", ")));
                }
                Payload::Named(fields) => {
                    let fields: Vec<String> = fields
                        .iter()
                        .map(|(f, s)| format!("{f}: {}", shape_type(s)))
                        .collect();
                    out.push_str(&format!("    {variant} {{ {} }},\n", fields.join(", ")));
                }
            }
        }
        out.push_str("}\n\n");
    }

    out.push_str(&conversions);
    out.push_str(&accessors);

    let versions: Vec<String> = served
        .iter()
        .map(|v| format!("LanguageVersion::{}", v.variant))
        .collect();
    out.push_str(&format!(
        "impl crate::syntax::view::Sealed for ParsedInput {{}}\n\n\
         impl crate::syntax::Facade for ParsedInput {{\n\
         const VERSIONS: &'static [LanguageVersion] = &[{}];\n}}\n",
        versions.join(", ")
    ));
    Ok(reformat(&out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn union() -> Lock {
        crate::union_lock()
    }

    #[test]
    fn version_modules_and_features() {
        let names: Vec<(String, String)> = versions()
            .iter()
            .map(|v| (v.module(), v.cargo_feature()))
            .collect();
        let modules: Vec<&str> = names.iter().map(|(m, _)| m.as_str()).collect();
        assert_eq!(
            modules,
            [
                "v4_6", "v4_7", "v5", "v6", "v7", "v8", "v9", "v10", "v11", "preview"
            ]
        );
        assert_eq!(names.last().unwrap().1, "unstable-preview");
    }

    /// A published facade is generated from its lock: re-reading a derived
    /// lock from its text and publishing it gives the same code and the same
    /// aliasing, so `code == gen(lock)` holds whether the lock is derived or
    /// checked in.
    #[test]
    fn code_is_a_function_of_the_lock() {
        let rows = features::feature_table();
        let union = union();
        let derived = facade_locks_with(&rows, &union, |_| None);
        let reread = facade_locks_with(&rows, &union, |v| {
            let (_, lock) = derived.iter().find(|(d, _)| d == v).unwrap();
            Some(lock::parse(&lock::render(&v.module(), lock)).unwrap())
        });
        assert_eq!(derived, reread);
    }

    /// Every facade's lock is a subset of the union's, delegates aside, and a
    /// derived facade drops exactly the types a live `Kind` row introduced
    /// after its version, and whatever names them.
    #[test]
    fn derived_locks_restrict_the_union() {
        let rows = features::feature_table();
        let union = union();
        let undelegated = |line: &Line| match line {
            Line::Accessor {
                owner, name, shape, ..
            } => Line::Accessor {
                owner: owner.clone(),
                name: name.clone(),
                shape: shape.clone(),
                delegate: None,
            },
            other => other.clone(),
        };
        for (version, lock) in facade_locks_with(&rows, &union, |_| None) {
            for line in &lock.lines {
                if !matches!(line, Line::AliasOf { .. }) {
                    assert!(
                        union.lines.contains(&undelegated(line)),
                        "{}: {line:?} is not the union's",
                        version.module()
                    );
                }
            }
        }
        let v8 = derived_lock(&union, &rows, 8.0);
        assert!(!v8.names().nodes.contains("WithNullType"));
        assert!(v8.names().nodes.contains("IntersectionType"));
        let v7 = derived_lock(&union, &rows, 7.0);
        assert!(!v7.names().nodes.contains("WhileBangExpr"));
        assert_eq!(derived_lock(&union, &rows, 9999.0).lines.len(), {
            let v9 = derived_lock(&union, &rows, 9.0);
            v9.lines.len()
        });
    }

    /// The facade generator refuses, naming it, a facade type or accessor the
    /// union cannot back mechanically.
    #[test]
    fn a_lock_the_union_cannot_back_is_refused() {
        let rows = features::feature_table();
        let union = union();
        let version = versions()[5];
        let mut lock = derived_lock(&union, &rows, version.decimal);
        // A renamed delegate the union lacks.
        let accessor = lock
            .lines
            .iter()
            .find(|l| matches!(l, Line::Accessor { .. }))
            .cloned()
            .unwrap();
        lock.lines.remove(&accessor);
        let Line::Accessor {
            owner, name, shape, ..
        } = accessor
        else {
            unreachable!()
        };
        lock.lines.insert(Line::Accessor {
            owner: owner.clone(),
            name: name.clone(),
            shape,
            delegate: Some("no_such_accessor".to_string()),
        });
        let errors = generate_facade(&version, &lock, &union, &[&version]).unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| e.contains(&format!("{owner}::{name}"))),
            "{errors:#?}"
        );
    }

    /// An accessor that returns a narrowed dispatch enum bare cannot drop an
    /// out-of-surface value, so it needs a compat shim and the generator says so.
    #[test]
    fn a_bare_narrowed_return_needs_a_shim() {
        let rows = features::feature_table();
        let mut union = union();
        union.lines.insert(Line::Accessor {
            owner: "ParenType".to_string(),
            name: "bare_inner".to_string(),
            shape: Shape::One(Elem::Enum("Type".to_string())),
            delegate: None,
        });
        let version = versions()[5];
        let lock = derived_lock(&union, &rows, version.decimal);
        let errors = generate_facade(&version, &lock, &union, &[&version]).unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| e.contains("ParenType::bare_inner") && e.contains("compat shim")),
            "{errors:#?}"
        );
    }
}
