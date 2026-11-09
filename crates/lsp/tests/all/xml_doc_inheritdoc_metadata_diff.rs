//! `<inheritdoc>` expansion against Roslyn's, over generated *metadata*.
//!
//! The C#-source generator (`xml_doc_inheritdoc_generated_diff`) reaches
//! only what a C# compiler writes. This one describes multi-assembly worlds
//! and has the oracle emit them directly (`tools/inheritdoc-oracle/Emit.cs`,
//! System.Reflection.Metadata), so it reaches the shapes the expansion must
//! bind exactly as Roslyn's PE importer does and C# never produces:
//!
//! - accessor methods named apart from their properties;
//! - assembly references by another version, case or culture than the
//!   loaded assembly's, through a type-forwarding facade, to an assembly
//!   that is loaded but lacks the type, or that is not loaded at all;
//! - the core library: the reference pack's, a hand-made one, one that
//!   references another assembly (so is none to Roslyn), or none;
//! - custom modifiers (`modopt`, `modreq`) on returns, agreeing or not
//!   between an override and its base;
//! - `MethodImpl` rows (explicit interface implementations, explicit
//!   overrides), and `newslot`/`final`/`abstract` flags;
//! - assemblies whose simple names collide by case, or by culture;
//! - documentation-file layout: compact `<member>` runs, CRLF and tabs, a
//!   namespaced `<member>`, a duplicated key.
//!
//! Each world is compared entry by entry, certain-implies-exact, against
//! Roslyn's expansion over the same reference set. A census per dimension
//! counts what each reached, and a liveness check over a fixed sample
//! requires every dimension to reach both an expansion and a decline.
//!
//! Some dimensions build metadata no C#, VB or F# compiler writes
//! ([`OUT_OF_DOMAIN`]). The expansion certifies only compiler-shaped
//! metadata, so an entry carrying one of them must decline, whatever Roslyn
//! makes of it: that property needs no oracle, and a disagreement Roslyn
//! would show there is not one the expansion can reach.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use proptest::prelude::*;
use proptest::test_runner::{Config, FileFailurePersistence, TestRunner};
use serde_json::{Value, json};
use tempfile::TempDir;

use crate::common::ensure_system_runtime_dll;
use crate::common::inheritdoc_diff::{Compared, Verdict};
use crate::common::inheritdoc_oracle::oracle;
use crate::xml_doc_inheritdoc_diff::{compare, fixture_from};

/// Cases per property in the normal suite.
const DEFAULT_CASES: u32 = 32;

/// Cases per property in the deep run (CI's `<inheritdoc>` step).
const DEEP_CASES: u32 = 384;

fn cases(default: u32) -> u32 {
    std::env::var("BORZOI_INHERITDOC_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// A type in a signature.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum STy {
    Int,
    Str,
    Obj,
    /// `N.T`, defined by `Lo`.
    T,
    /// `N.U`, defined by `Ex` (when it is loaded and has it).
    U,
}

impl STy {
    fn doc_id(self) -> &'static str {
        match self {
            STy::Int => "System.Int32",
            STy::Str => "System.String",
            STy::Obj => "System.Object",
            STy::T => "N.T",
            STy::U => "N.U",
        }
    }

    fn json(self, cx: &Cx) -> Value {
        match self {
            STy::Int => json!({"prim": "I4"}),
            STy::Str => json!({"prim": "String"}),
            STy::Obj => json!({"prim": "Object"}),
            STy::T => json!({"scope": cx.lo, "ns": "N", "name": "T"}),
            STy::U => json!({"scope": cx.ex, "ns": "N", "name": "U"}),
        }
    }
}

/// A signature type other than `ty`, for a declaration that binds nothing.
fn other_type(ty: STy) -> STy {
    if ty == STy::Int { STy::Str } else { STy::Int }
}

fn sty() -> impl Strategy<Value = STy> {
    prop_oneof![
        3 => Just(STy::Int),
        1 => Just(STy::Str),
        1 => Just(STy::Obj),
        2 => Just(STy::T),
        2 => Just(STy::U),
    ]
}

/// A custom modifier on a return type (`System.Runtime.CompilerServices.IsConst`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RetMod {
    None,
    Opt,
    Req,
}

fn ret_mod() -> impl Strategy<Value = RetMod> {
    prop_oneof![4 => Just(RetMod::None), 1 => Just(RetMod::Opt), 1 => Just(RetMod::Req)]
}

/// Where each assembly's references point, as `emit` scope indices (`-1`:
/// the assembly's own types).
struct Cx {
    core: i64,
    lo: i64,
    ex: i64,
}

fn sig(ty: STy, m: RetMod, cx: &Cx) -> Value {
    let mods = match m {
        RetMod::None => json!([]),
        RetMod::Opt | RetMod::Req => json!([{
            "optional": m == RetMod::Opt,
            "type": {"scope": cx.core, "ns": "System.Runtime.CompilerServices", "name": "IsConst"},
        }]),
    };
    json!({"type": ty.json(cx), "mods": mods})
}

fn object(cx: &Cx) -> Value {
    json!({"scope": cx.core, "ns": "System", "name": "Object"})
}

/// The core library of the reference set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Core {
    /// The reference pack's System.Runtime.
    Pack,
    /// A hand-made `Core` defining `System.Object` and the primitives.
    Own,
    /// The same, but referencing another assembly: no core library to Roslyn.
    OwnReferencing,
    /// System.Runtime, referenced but not in the reference set.
    Absent,
}

/// What `Hi` references `Lo`'s types through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Target {
    Lo,
    /// `Lo`, by a version the loaded `Lo` does not have.
    LoOtherVersion,
    /// `lo`: the simple name in another case.
    LoOtherCase,
    /// `Fa`, a facade forwarding `Lo`'s types.
    Facade,
}

/// The `Ex` assembly `Lo` and `Hi` both reference for `N.U`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ex {
    Loaded,
    /// Loaded, without `N.U`.
    LoadedWithoutU,
    /// Loaded, with `Hi` referencing a newer version than `Lo` does.
    HiNewerRef,
    Missing,
}

/// Another loaded assembly whose simple name collides with `Lo`'s.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Twin {
    None,
    /// `LO`.
    Case,
    /// `Lo` again, in another culture.
    Culture,
}

/// A documentation file's layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Layout {
    Indented,
    /// No whitespace between elements.
    Compact,
    /// CRLF line ends and tab indents.
    Crlf,
    /// An extra `<member>` in a default namespace.
    Namespaced,
    /// The first key repeated with other content.
    Duplicated,
}

fn layout() -> impl Strategy<Value = Layout> {
    prop_oneof![
        4 => Just(Layout::Indented),
        1 => Just(Layout::Compact),
        1 => Just(Layout::Crlf),
        1 => Just(Layout::Namespaced),
        1 => Just(Layout::Duplicated),
    ]
}

/// How an interface property's getter is named.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Getter {
    /// `get_P{i}`.
    Conventional,
    /// `get_P{i+1}` (mod the count): another property's conventional name.
    Rotated,
    /// `fetch_P{i}`.
    Odd,
}

#[derive(Debug, Clone)]
struct IMethod {
    ret: STy,
    param: STy,
}

#[derive(Debug, Clone)]
struct IProp {
    ty: STy,
    getter: Getter,
}

#[derive(Debug, Clone)]
struct BMethod {
    ret: STy,
    param: STy,
    ret_mod: RetMod,
    /// On the parameter, and on every override's parameter alike.
    param_mod: RetMod,
    is_abstract: bool,
    is_final: bool,
}

/// What `D` does with a base method.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Over {
    Skip,
    /// `virtual` reusing the slot, with this return modifier.
    Override(RetMod),
    /// `virtual newslot`.
    NewSlot,
    /// `virtual newslot` plus a `MethodImpl` row naming the base method.
    Explicit,
    /// The same, returning `string` whatever the base returns: a covariant
    /// return's shape.
    Covariant,
}

/// How `D` implements an interface member.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Impl {
    /// A public member of the same name.
    Implicit,
    /// A private member named `N.I.X` with a `MethodImpl` row.
    Explicit,
}

#[derive(Debug, Clone)]
struct World {
    core: Core,
    lo_culture: bool,
    ref_culture: bool,
    target: Target,
    ex: Ex,
    twin: Twin,
    imethods: Vec<IMethod>,
    iprops: Vec<IProp>,
    bmethods: Vec<BMethod>,
    overs: Vec<Over>,
    iimpls: Vec<Impl>,
    pimpls: Vec<Impl>,
    /// `D2 : D` overriding `D`'s methods.
    d2: bool,
    /// `D`'s overrides inherit through a `cref` to the base member.
    cref: bool,
    lo_layout: Layout,
    hi_layout: Layout,
    /// The first explicit method and property implementation's `MethodImpl`
    /// declaration returns another type than the member of its name (and
    /// the body) does: it binds nothing, so Roslyn finds no candidate,
    /// whatever the body's signature. Drawn last, so that adding it left
    /// every other dimension's draws as they were.
    misdeclare: bool,
}

fn world() -> impl Strategy<Value = World> {
    let core = prop_oneof![
        5 => Just(Core::Pack),
        2 => Just(Core::Own),
        1 => Just(Core::OwnReferencing),
        1 => Just(Core::Absent),
    ];
    let target = prop_oneof![
        5 => Just(Target::Lo),
        1 => Just(Target::LoOtherVersion),
        1 => Just(Target::LoOtherCase),
        2 => Just(Target::Facade),
    ];
    let ex = prop_oneof![
        5 => Just(Ex::Loaded),
        1 => Just(Ex::LoadedWithoutU),
        1 => Just(Ex::HiNewerRef),
        1 => Just(Ex::Missing),
    ];
    let twin = prop_oneof![6 => Just(Twin::None), 1 => Just(Twin::Case), 1 => Just(Twin::Culture)];
    let getter = prop_oneof![
        3 => Just(Getter::Conventional),
        2 => Just(Getter::Rotated),
        1 => Just(Getter::Odd),
    ];
    let over = prop_oneof![
        1 => Just(Over::Skip),
        4 => ret_mod().prop_map(Over::Override),
        1 => Just(Over::NewSlot),
        1 => Just(Over::Explicit),
        1 => Just(Over::Covariant),
    ];
    let imp = prop_oneof![Just(Impl::Implicit), Just(Impl::Explicit)];
    (
        (core, prop::bool::weighted(0.2), prop::bool::weighted(0.2), target, ex, twin),
        prop::collection::vec((sty(), sty()).prop_map(|(ret, param)| IMethod { ret, param }), 1..3),
        prop::collection::vec((sty(), getter).prop_map(|(ty, getter)| IProp { ty, getter }), 0..3),
        prop::collection::vec(
            (
                sty(),
                sty(),
                ret_mod(),
                prop_oneof![6 => Just(RetMod::None), 1 => Just(RetMod::Opt), 1 => Just(RetMod::Req)],
                prop::bool::weighted(0.2),
                prop::bool::weighted(0.15),
            )
                .prop_map(|(ret, param, ret_mod, param_mod, is_abstract, is_final)| BMethod {
                    ret,
                    param,
                    ret_mod,
                    param_mod,
                    is_abstract,
                    is_final: is_final && !is_abstract,
                }),
            1..4,
        ),
        prop::collection::vec(over, 3),
        prop::collection::vec(imp.clone(), 2),
        prop::collection::vec(imp, 2),
        (any::<bool>(), prop::bool::weighted(0.25), layout(), layout()),
        prop::bool::weighted(0.3),
    )
        .prop_map(
            |(
                (core, lo_culture, ref_culture, target, ex, twin),
                imethods,
                iprops,
                bmethods,
                overs,
                iimpls,
                pimpls,
                (d2, cref, lo_layout, hi_layout),
                misdeclare,
            )| World {
                core,
                lo_culture,
                ref_culture,
                target,
                ex,
                twin,
                imethods,
                iprops,
                bmethods,
                overs,
                iimpls,
                pimpls,
                d2,
                cref,
                lo_layout,
                hi_layout,
                misdeclare,
            },
        )
}

/// A dimension of the world, for the census and the liveness check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum Dim {
    CorePack,
    CoreOwn,
    CoreOwnReferencing,
    CoreAbsent,
    LoCulture,
    RefCulture,
    TargetOtherVersion,
    TargetOtherCase,
    Facade,
    ExLoadedWithoutU,
    ExHiNewerRef,
    ExMissing,
    CaseTwin,
    CultureTwin,
    LoCompact,
    LoCrlf,
    LoNamespaced,
    LoDuplicated,
    HiCompact,
    HiCrlf,
    HiNamespaced,
    HiDuplicated,
    // Per member.
    ModOpt,
    ModReq,
    ModMismatch,
    GetterRotated,
    GetterOdd,
    Override,
    NewSlot,
    ExplicitOverride,
    Covariant,
    AbstractBase,
    FinalBase,
    ImplicitIface,
    ExplicitIface,
    ImplicitProp,
    ExplicitProp,
    /// An explicit implementation whose declaration binds nothing.
    Misdeclared,
    UsesT,
    UsesU,
    Cref,
    Derived2,
}

const ALL_DIMS: [Dim; 42] = [
    Dim::CorePack,
    Dim::CoreOwn,
    Dim::CoreOwnReferencing,
    Dim::CoreAbsent,
    Dim::LoCulture,
    Dim::RefCulture,
    Dim::TargetOtherVersion,
    Dim::TargetOtherCase,
    Dim::Facade,
    Dim::ExLoadedWithoutU,
    Dim::ExHiNewerRef,
    Dim::ExMissing,
    Dim::CaseTwin,
    Dim::CultureTwin,
    Dim::LoCompact,
    Dim::LoCrlf,
    Dim::LoNamespaced,
    Dim::LoDuplicated,
    Dim::HiCompact,
    Dim::HiCrlf,
    Dim::HiNamespaced,
    Dim::HiDuplicated,
    Dim::ModOpt,
    Dim::ModReq,
    Dim::ModMismatch,
    Dim::GetterRotated,
    Dim::GetterOdd,
    Dim::Override,
    Dim::NewSlot,
    Dim::ExplicitOverride,
    Dim::Covariant,
    Dim::AbstractBase,
    Dim::FinalBase,
    Dim::ImplicitIface,
    Dim::ExplicitIface,
    Dim::ImplicitProp,
    Dim::ExplicitProp,
    Dim::Misdeclared,
    Dim::UsesT,
    Dim::UsesU,
    Dim::Cref,
    Dim::Derived2,
];

impl World {
    fn dims(&self) -> BTreeSet<Dim> {
        let mut d = BTreeSet::new();
        d.insert(match self.core {
            Core::Pack => Dim::CorePack,
            Core::Own => Dim::CoreOwn,
            Core::OwnReferencing => Dim::CoreOwnReferencing,
            Core::Absent => Dim::CoreAbsent,
        });
        if self.lo_culture {
            d.insert(Dim::LoCulture);
        }
        if self.ref_culture {
            d.insert(Dim::RefCulture);
        }
        d.extend(match self.target {
            Target::Lo => None,
            Target::LoOtherVersion => Some(Dim::TargetOtherVersion),
            Target::LoOtherCase => Some(Dim::TargetOtherCase),
            Target::Facade => Some(Dim::Facade),
        });
        d.extend(match self.ex {
            Ex::Loaded => None,
            Ex::LoadedWithoutU => Some(Dim::ExLoadedWithoutU),
            Ex::HiNewerRef => Some(Dim::ExHiNewerRef),
            Ex::Missing => Some(Dim::ExMissing),
        });
        d.extend(match self.twin {
            Twin::None => None,
            Twin::Case => Some(Dim::CaseTwin),
            Twin::Culture => Some(Dim::CultureTwin),
        });
        let lay = |l: Layout, c, r, n, du| match l {
            Layout::Indented => None,
            Layout::Compact => Some(c),
            Layout::Crlf => Some(r),
            Layout::Namespaced => Some(n),
            Layout::Duplicated => Some(du),
        };
        d.extend(lay(
            self.lo_layout,
            Dim::LoCompact,
            Dim::LoCrlf,
            Dim::LoNamespaced,
            Dim::LoDuplicated,
        ));
        d.extend(lay(
            self.hi_layout,
            Dim::HiCompact,
            Dim::HiCrlf,
            Dim::HiNamespaced,
            Dim::HiDuplicated,
        ));
        d
    }
}

fn ty_dims(t: STy, d: &mut BTreeSet<Dim>) {
    match t {
        STy::T => {
            d.insert(Dim::UsesT);
        }
        STy::U => {
            d.insert(Dim::UsesU);
        }
        _ => {}
    }
}

fn mod_dims(m: RetMod, d: &mut BTreeSet<Dim>) {
    match m {
        RetMod::None => {}
        RetMod::Opt => {
            d.insert(Dim::ModOpt);
        }
        RetMod::Req => {
            d.insert(Dim::ModReq);
        }
    }
}

/// An assembly's identity, as `emit` takes it.
#[derive(Debug, Clone)]
struct Ident {
    name: String,
    version: String,
    culture: String,
    token: Option<String>,
}

impl Ident {
    fn new(name: &str, version: &str, culture: &str) -> Self {
        Ident {
            name: name.into(),
            version: version.into(),
            culture: culture.into(),
            token: None,
        }
    }

    fn json(&self) -> Value {
        json!({
            "name": self.name,
            "version": self.version,
            "culture": self.culture,
            "publicKeyToken": self.token,
        })
    }
}

/// The reference pack's System.Runtime: its path and identity.
fn system_runtime() -> &'static (PathBuf, Ident) {
    static RUNTIME: std::sync::OnceLock<(PathBuf, Ident)> = std::sync::OnceLock::new();
    RUNTIME.get_or_init(|| {
        use borzoi_assembly::EcmaView;
        let path = ensure_system_runtime_dll();
        let bytes = std::fs::read(&path).unwrap();
        let view = borzoi_assembly::Ecma335Assembly::parse(&bytes).unwrap();
        let id = view.identity();
        let v = &id.version;
        let ident = Ident {
            name: id.name.clone(),
            version: format!("{}.{}.{}.{}", v.major, v.minor, v.build, v.revision),
            culture: String::new(),
            token: id
                .public_key_token
                .map(|t| t.iter().map(|b| format!("{b:02x}")).collect()),
        };
        (path, ident)
    })
}

/// A method description for `emit`.
#[allow(clippy::too_many_arguments)]
fn method(
    name: &str,
    access: &str,
    flags: (bool, bool, bool, bool),
    special: bool,
    ret: Value,
    params: Vec<Value>,
    impls: Vec<Value>,
) -> Value {
    let (virtual_, newslot, abstract_, final_) = flags;
    json!({
        "name": name, "access": access, "virtual": virtual_, "newslot": newslot,
        "abstract": abstract_, "final": final_, "specialName": special,
        "ret": ret, "params": params, "impls": impls,
    })
}

/// One documentation file's entries: key and the element children.
type Entries = Vec<(String, Vec<String>)>;

fn full(key: &str) -> Vec<String> {
    vec![
        format!("<summary>Summary of {key}.</summary>"),
        format!("<remarks>Remarks of {key}.</remarks>"),
    ]
}

fn bare() -> Vec<String> {
    vec!["<inheritdoc/>".to_string()]
}

fn render_xml(assembly: &str, entries: &Entries, layout: Layout) -> String {
    let (nl, i1, i2, i3) = match layout {
        Layout::Compact => ("", "", "", ""),
        Layout::Crlf => ("\r\n", "\t", "\t\t", "\t\t\t"),
        _ => ("\n", "    ", "        ", "            "),
    };
    let member = |key: &str, body: &[String], xmlns: &str| {
        let mut m = format!("{i2}<member{xmlns} name=\"{key}\">{nl}");
        for line in body {
            m.push_str(&format!("{i3}{line}{nl}"));
        }
        m.push_str(&format!("{i2}</member>{nl}"));
        m
    };
    let mut out = format!(
        "<?xml version=\"1.0\"?>{nl}<doc>{nl}{i1}<assembly>{nl}{i2}<name>{assembly}</name>{nl}{i1}</assembly>{nl}{i1}<members>{nl}"
    );
    for (key, body) in entries {
        out.push_str(&member(key, body, ""));
    }
    if let Some((key, _)) = entries.first() {
        match layout {
            Layout::Namespaced => out.push_str(&member(
                key,
                &["<summary>A namespaced entry.</summary>".to_string()],
                " xmlns=\"urn:x\"",
            )),
            Layout::Duplicated => out.push_str(&member(
                key,
                &["<summary>The key again, differently.</summary>".to_string()],
                "",
            )),
            _ => {}
        }
    }
    out.push_str(&format!("{i1}</members>{nl}</doc>{nl}"));
    out
}

/// A world on disk: its reference set, `Hi`'s path, and each `Hi` entry's
/// dimensions by key.
struct Built {
    dir: TempDir,
    references: Vec<PathBuf>,
    hi: PathBuf,
    dims: BTreeMap<String, BTreeSet<Dim>>,
}

fn emit(
    dir: &Path,
    name: &str,
    assembly: &Ident,
    refs: &[&Ident],
    types: Vec<Value>,
    forwarders: Value,
) -> PathBuf {
    let path = dir.join(format!("{name}.dll"));
    oracle().lock().unwrap().emit(json!({
        "path": path,
        "assembly": assembly.json(),
        "refs": refs.iter().map(|r| r.json()).collect::<Vec<_>>(),
        "types": types,
        "forwarders": forwarders,
    }));
    path
}

fn class(ns: &str, name: &str, base: Option<Value>, methods: Vec<Value>) -> Value {
    json!({"ns": ns, "name": name, "kind": "class", "base": base, "interfaces": [], "methods": methods, "properties": []})
}

/// The hand-made core library's types.
fn core_types() -> Vec<Value> {
    let obj = json!({"scope": -1, "ns": "System", "name": "Object"});
    let vt = json!({"scope": -1, "ns": "System", "name": "ValueType"});
    let strukt = |name: &str| json!({"ns": "System", "name": name, "kind": "struct", "base": vt, "interfaces": [], "methods": [], "properties": []});
    vec![
        class("System", "Object", None, vec![]),
        json!({"ns": "System", "name": "ValueType", "kind": "class", "abstract": true, "base": obj, "interfaces": [], "methods": [], "properties": []}),
        strukt("Int32"),
        strukt("Boolean"),
        strukt("Void"),
        json!({"ns": "System", "name": "String", "kind": "class", "sealed": true, "base": obj, "interfaces": [], "methods": [], "properties": []}),
        class(
            "System.Runtime.CompilerServices",
            "IsConst",
            Some(obj.clone()),
            vec![],
        ),
    ]
}

fn build(w: &World) -> Built {
    let dir = TempDir::new().unwrap();
    let d = dir.path();
    let mut references = Vec::new();
    // The core library.
    let core = match w.core {
        Core::Pack | Core::Absent => {
            let (path, ident) = system_runtime();
            if w.core == Core::Pack {
                references.push(path.clone());
            }
            ident.clone()
        }
        Core::Own | Core::OwnReferencing => {
            let ident = Ident::new("Core", "1.0.0.0", "");
            let other = Ident::new("Other", "1.0.0.0", "");
            let refs: Vec<&Ident> = if w.core == Core::OwnReferencing {
                vec![&other]
            } else {
                vec![]
            };
            references.push(emit(d, "Core", &ident, &refs, core_types(), json!([])));
            ident
        }
    };
    // `Ex`, defining `N.U`.
    let ex = Ident::new("Ex", "1.0.0.0", "");
    let ex_cx = Cx {
        core: 0,
        lo: -1,
        ex: -1,
    };
    let u = if w.ex == Ex::LoadedWithoutU { "V" } else { "U" };
    let ex_path = emit(
        d,
        "Ex",
        &ex,
        &[&core],
        vec![class("N", u, Some(object(&ex_cx)), vec![])],
        json!([]),
    );
    if w.ex != Ex::Missing {
        references.push(ex_path);
    }

    // `Lo`: `N.T`, `N.I`, `N.B`, fully documented.
    let lo = Ident::new("Lo", "1.0.0.0", if w.lo_culture { "ja" } else { "" });
    let cx = Cx {
        core: 0,
        lo: -1,
        ex: 1,
    };
    let mut lo_docs: Entries = Vec::new();
    let mut lo_types = vec![class("N", "T", Some(object(&cx)), vec![])];
    lo_docs.push(("T:N.T".into(), full("T:N.T")));
    let n = w.iprops.len();
    let getter_name = |i: usize| match w.iprops[i].getter {
        Getter::Conventional => format!("get_P{i}"),
        Getter::Rotated => format!("get_P{}", (i + 1) % n),
        Getter::Odd => format!("fetch_P{i}"),
    };
    let mut i_methods = Vec::new();
    for (i, m) in w.imethods.iter().enumerate() {
        i_methods.push(method(
            &format!("N{i}"),
            "public",
            (true, true, true, false),
            false,
            sig(m.ret, RetMod::None, &cx),
            vec![sig(m.param, RetMod::None, &cx)],
            vec![],
        ));
        let key = format!("M:N.I.N{i}({})", m.param.doc_id());
        lo_docs.push((key.clone(), full(&key)));
    }
    let mut i_props = Vec::new();
    for (i, p) in w.iprops.iter().enumerate() {
        i_props.push(json!({"name": format!("P{i}"), "type": sig(p.ty, RetMod::None, &cx), "getter": i_methods.len()}));
        i_methods.push(method(
            &getter_name(i),
            "public",
            (true, true, true, false),
            true,
            sig(p.ty, RetMod::None, &cx),
            vec![],
            vec![],
        ));
        let key = format!("P:N.I.P{i}");
        lo_docs.push((key.clone(), full(&key)));
    }
    lo_types.push(json!({"ns": "N", "name": "I", "kind": "interface", "base": null, "interfaces": [], "methods": i_methods, "properties": i_props}));
    lo_docs.push(("T:N.I".into(), full("T:N.I")));
    let mut b_methods = Vec::new();
    for (k, m) in w.bmethods.iter().enumerate() {
        b_methods.push(method(
            &format!("M{k}"),
            "public",
            (true, true, m.is_abstract, m.is_final),
            false,
            sig(m.ret, m.ret_mod, &cx),
            vec![sig(m.param, m.param_mod, &cx)],
            vec![],
        ));
        let key = format!("M:N.B.M{k}({})", m.param.doc_id());
        lo_docs.push((key.clone(), full(&key)));
    }
    let abstract_b = w.bmethods.iter().any(|m| m.is_abstract);
    lo_types.push(json!({"ns": "N", "name": "B", "kind": "class", "abstract": abstract_b, "base": object(&cx), "interfaces": [], "methods": b_methods, "properties": []}));
    lo_docs.push(("T:N.B".into(), full("T:N.B")));
    let lo_path = emit(d, "Lo", &lo, &[&core, &ex], lo_types.clone(), json!([]));
    std::fs::write(
        lo_path.with_extension("xml"),
        render_xml("Lo", &lo_docs, w.lo_layout),
    )
    .unwrap();
    references.push(lo_path);

    // The facade, and the twins.
    let target = match w.target {
        Target::Lo => Ident::new("Lo", "1.0.0.0", ""),
        Target::LoOtherVersion => Ident::new("Lo", "3.0.0.0", ""),
        Target::LoOtherCase => Ident::new("lo", "1.0.0.0", ""),
        Target::Facade => Ident::new("Fa", "1.0.0.0", ""),
    };
    let target = Ident {
        culture: if w.ref_culture && w.target != Target::Facade {
            "ja".into()
        } else {
            target.culture
        },
        ..target
    };
    if w.target == Target::Facade {
        let fa = Ident::new("Fa", "1.0.0.0", "");
        let forwarders: Vec<Value> = ["T", "I", "B"]
            .iter()
            .map(|t| json!({"ns": "N", "name": t, "ref": 0}))
            .collect();
        references.push(emit(d, "Fa", &fa, &[&lo], vec![], json!(forwarders)));
    }
    match w.twin {
        Twin::None => {}
        Twin::Case => {
            let twin_cx = Cx {
                core: 0,
                lo: -1,
                ex: -1,
            };
            references.push(emit(
                &d.join("twin"),
                "LO",
                &Ident::new("LO", "1.0.0.0", ""),
                &[&core],
                vec![class("N", "Z", Some(object(&twin_cx)), vec![])],
                json!([]),
            ));
        }
        Twin::Culture => {
            let twin = Ident {
                culture: if w.lo_culture { "fr" } else { "ja" }.into(),
                ..lo.clone()
            };
            references.push(emit(
                &d.join("twin"),
                "Lo",
                &twin,
                &[&core, &ex],
                lo_types,
                json!([]),
            ));
        }
    }

    // `Hi`: `D : B, I`, `D2 : D`, and `H2 : H`, all inheriting.
    let ex_ref = Ident {
        version: if w.ex == Ex::HiNewerRef {
            "2.0.0.0".into()
        } else {
            "1.0.0.0".into()
        },
        ..ex.clone()
    };
    let cx = Cx {
        core: 0,
        lo: 1,
        ex: 2,
    };
    let mut hi_docs: Entries = Vec::new();
    let mut dims: BTreeMap<String, BTreeSet<Dim>> = BTreeMap::new();
    let world_dims = w.dims();
    let mut note = |key: &str, extra: BTreeSet<Dim>| {
        let mut all = world_dims.clone();
        all.extend(extra);
        dims.insert(key.to_string(), all);
    };
    let b_ref = json!({"scope": cx.lo, "ns": "N", "name": "B"});
    let i_ref = json!({"scope": cx.lo, "ns": "N", "name": "I"});
    let mut d_methods = Vec::new();
    // `D`'s public virtual methods, for `D2`: name, return, parameter, modifier.
    let mut overridable: Vec<(String, STy, STy, RetMod, RetMod)> = Vec::new();
    for (k, m) in w.bmethods.iter().enumerate() {
        let over = w.overs.get(k).copied().unwrap_or(Over::Skip);
        let mut md = BTreeSet::new();
        ty_dims(m.ret, &mut md);
        ty_dims(m.param, &mut md);
        mod_dims(m.ret_mod, &mut md);
        mod_dims(m.param_mod, &mut md);
        if m.is_abstract {
            md.insert(Dim::AbstractBase);
        }
        if m.is_final {
            md.insert(Dim::FinalBase);
        }
        let name = format!("M{k}");
        let explicit_impl = || {
            vec![json!({
                "parent": b_ref, "name": name,
                "ret": sig(m.ret, m.ret_mod, &cx),
                "params": [sig(m.param, m.param_mod, &cx)],
            })]
        };
        let mut ret = m.ret;
        let (flags, ret_mod, impls) = match over {
            Over::Skip => continue,
            Over::Override(r) => {
                md.insert(Dim::Override);
                mod_dims(r, &mut md);
                if r != m.ret_mod {
                    md.insert(Dim::ModMismatch);
                }
                ((true, false, false, false), r, vec![])
            }
            Over::NewSlot => {
                md.insert(Dim::NewSlot);
                ((true, true, false, false), m.ret_mod, vec![])
            }
            Over::Explicit => {
                md.insert(Dim::ExplicitOverride);
                ((true, true, false, false), m.ret_mod, explicit_impl())
            }
            Over::Covariant => {
                md.insert(Dim::Covariant);
                ret = STy::Str;
                ((true, true, false, false), RetMod::None, explicit_impl())
            }
        };
        d_methods.push(method(
            &name,
            "public",
            flags,
            false,
            sig(ret, ret_mod, &cx),
            vec![sig(m.param, m.param_mod, &cx)],
            impls,
        ));
        overridable.push((name.clone(), ret, m.param, ret_mod, m.param_mod));
        let key = format!("M:N.D.{name}({})", m.param.doc_id());
        let body = if w.cref && k % 2 == 0 {
            md.insert(Dim::Cref);
            vec![format!(
                "<inheritdoc cref=\"M:N.B.{name}({})\"/>",
                m.param.doc_id()
            )]
        } else {
            bare()
        };
        hi_docs.push((key.clone(), body));
        note(&key, md);
    }
    for (i, m) in w.imethods.iter().enumerate() {
        let imp = w.iimpls.get(i).copied().unwrap_or(Impl::Implicit);
        let mut md = BTreeSet::new();
        ty_dims(m.ret, &mut md);
        ty_dims(m.param, &mut md);
        let decl = json!({
            "parent": i_ref, "name": format!("N{i}"),
            "ret": sig(m.ret, RetMod::None, &cx),
            "params": [sig(m.param, RetMod::None, &cx)],
        });
        let (name, access, impls, id_name) = match imp {
            Impl::Implicit => {
                md.insert(Dim::ImplicitIface);
                (format!("N{i}"), "public", vec![], format!("N{i}"))
            }
            Impl::Explicit => {
                let mut decl = decl;
                if w.misdeclare && i == 0 {
                    md.insert(Dim::Misdeclared);
                    decl["ret"] = sig(other_type(m.ret), RetMod::None, &cx);
                } else {
                    md.insert(Dim::ExplicitIface);
                }
                (
                    format!("N.I.N{i}"),
                    "private",
                    vec![decl],
                    format!("N#I#N{i}"),
                )
            }
        };
        d_methods.push(method(
            &name,
            access,
            (true, true, false, true),
            false,
            sig(m.ret, RetMod::None, &cx),
            vec![sig(m.param, RetMod::None, &cx)],
            impls,
        ));
        let key = format!("M:N.D.{id_name}({})", m.param.doc_id());
        hi_docs.push((key.clone(), bare()));
        note(&key, md);
    }
    let mut d_props = Vec::new();
    for (i, p) in w.iprops.iter().enumerate() {
        let imp = w.pimpls.get(i).copied().unwrap_or(Impl::Implicit);
        let mut md = BTreeSet::new();
        ty_dims(p.ty, &mut md);
        md.extend(match p.getter {
            Getter::Conventional => None,
            Getter::Rotated if n > 1 => Some(Dim::GetterRotated),
            Getter::Rotated => None,
            Getter::Odd => Some(Dim::GetterOdd),
        });
        let (prop, getter, access, impls, id_name) = match imp {
            Impl::Implicit => {
                md.insert(Dim::ImplicitProp);
                (
                    format!("P{i}"),
                    format!("get_P{i}"),
                    "public",
                    vec![],
                    format!("P{i}"),
                )
            }
            Impl::Explicit => {
                let ret = if w.misdeclare && i == 0 {
                    md.insert(Dim::Misdeclared);
                    other_type(p.ty)
                } else {
                    md.insert(Dim::ExplicitProp);
                    p.ty
                };
                (
                    format!("N.I.P{i}"),
                    format!("N.I.get_P{i}"),
                    "private",
                    vec![json!({
                        "parent": i_ref, "name": getter_name(i),
                        "ret": sig(ret, RetMod::None, &cx), "params": [],
                    })],
                    format!("N#I#P{i}"),
                )
            }
        };
        d_props.push(
            json!({"name": prop, "type": sig(p.ty, RetMod::None, &cx), "getter": d_methods.len()}),
        );
        d_methods.push(method(
            &getter,
            access,
            (true, true, false, true),
            true,
            sig(p.ty, RetMod::None, &cx),
            vec![],
            impls,
        ));
        let key = format!("P:N.D.{id_name}");
        hi_docs.push((key.clone(), bare()));
        note(&key, md);
    }
    let mut hi_types = vec![json!({
        "ns": "N", "name": "D", "kind": "class", "base": b_ref, "interfaces": [i_ref],
        "methods": d_methods, "properties": d_props,
    })];
    hi_docs.push(("T:N.D".into(), bare()));
    note("T:N.D", BTreeSet::new());
    if w.d2 {
        let d_ref = json!({"scope": -1, "ns": "N", "name": "D"});
        let mut methods = Vec::new();
        for (name, ret, param, ret_mod, param_mod) in &overridable {
            methods.push(method(
                name,
                "public",
                (true, false, false, false),
                false,
                sig(*ret, *ret_mod, &cx),
                vec![sig(*param, *param_mod, &cx)],
                vec![],
            ));
            let key = format!("M:N.D2.{name}({})", param.doc_id());
            hi_docs.push((key.clone(), bare()));
            let mut md = BTreeSet::from([Dim::Derived2]);
            ty_dims(*ret, &mut md);
            ty_dims(*param, &mut md);
            mod_dims(*ret_mod, &mut md);
            mod_dims(*param_mod, &mut md);
            note(&key, md);
        }
        hi_types.push(class("N", "D2", Some(d_ref), methods));
        hi_docs.push(("T:N.D2".into(), bare()));
        note("T:N.D2", BTreeSet::from([Dim::Derived2]));
    }
    // `H2 : H`, inside `Hi`: reaches an expansion whatever the references.
    let h_ref = json!({"scope": -1, "ns": "N", "name": "H"});
    let k0 = |flags: (bool, bool, bool, bool)| {
        method(
            "K0",
            "public",
            flags,
            false,
            sig(STy::Obj, RetMod::None, &cx),
            vec![],
            vec![],
        )
    };
    hi_types.push(class(
        "N",
        "H",
        Some(object(&cx)),
        vec![k0((true, true, false, false))],
    ));
    hi_types.push(class(
        "N",
        "H2",
        Some(h_ref),
        vec![k0((true, false, false, false))],
    ));
    hi_docs.push(("T:N.H".into(), full("T:N.H")));
    hi_docs.push(("M:N.H.K0".into(), full("M:N.H.K0")));
    hi_docs.push(("T:N.H2".into(), bare()));
    hi_docs.push(("M:N.H2.K0".into(), bare()));
    note("T:N.H2", BTreeSet::new());
    note("M:N.H2.K0", BTreeSet::new());
    let hi = emit(
        d,
        "Hi",
        &Ident::new("Hi", "1.0.0.0", ""),
        &[&core, &target, &ex_ref],
        hi_types,
        json!([]),
    );
    std::fs::write(
        hi.with_extension("xml"),
        render_xml("Hi", &hi_docs, w.hi_layout),
    )
    .unwrap();
    references.push(hi.clone());
    Built {
        dir,
        references,
        hi,
        dims,
    }
}

/// Dimensions whose metadata no C#, VB or F# compiler writes: why each is
/// outside the domain the expansion certifies. An entry carrying one never
/// expands.
const OUT_OF_DOMAIN: [(Dim, &str); 5] = [
    (
        Dim::GetterRotated,
        "a getter named as another property's (`get_P1` for `P0`)",
    ),
    (Dim::GetterOdd, "a getter outside the `get_` convention"),
    (
        Dim::ModOpt,
        "an optional `IsConst` modifier, which only C++/CLI writes",
    ),
    (
        Dim::ModMismatch,
        "an override whose return modifiers differ from its base's",
    ),
    (
        Dim::Misdeclared,
        "a `MethodImpl` declaration whose signature names no member",
    ),
];

/// One world's comparisons, with each entry's dimensions. Panics on a
/// disagreement, and on an expansion of an entry carrying an
/// [`OUT_OF_DOMAIN`] dimension.
fn run(w: &World) -> Vec<(Compared, BTreeSet<Dim>)> {
    let built = build(w);
    let fx = fixture_from(built.dir, built.hi.clone(), built.references.clone());
    let compared = compare(&fx);
    if let Some(c) = compared
        .iter()
        .find(|c| matches!(c.verdict, Verdict::Disagrees { .. }))
        && let Verdict::Disagrees { ours, roslyn } = &c.verdict
    {
        panic!(
            "{} disagrees with Roslyn:\nours: {ours}\nroslyn: {roslyn}\nworld: {w:#?}",
            c.key
        );
    }
    let compared: Vec<(Compared, BTreeSet<Dim>)> = compared
        .into_iter()
        .map(|c| {
            let d = built.dims.get(&c.key).cloned().unwrap_or_default();
            (c, d)
        })
        .collect();
    for (c, dims) in &compared {
        if let Some((d, why)) = OUT_OF_DOMAIN.iter().find(|(d, _)| dims.contains(d))
            && matches!(c.verdict, Verdict::Agrees)
        {
            panic!(
                "{} expanded over metadata no compiler writes ({d:?}: {why}); \
                 it must decline\nworld: {w:#?}",
                c.key
            );
        }
    }
    compared
}

fn check(cases: u32) {
    let config = Config {
        cases,
        source_file: Some(file!()),
        failure_persistence: Some(Box::new(FileFailurePersistence::SourceParallel(
            "proptest-regressions",
        ))),
        ..Config::default()
    };
    let mut runner = TestRunner::new(config);
    if let Err(e) = runner.run(&world(), |w| {
        run(&w);
        Ok(())
    }) {
        panic!("{e}");
    }
}

#[test]
fn generated_metadata_expands_exactly_as_roslyn() {
    check(cases(DEFAULT_CASES));
}

#[test]
#[ignore = "deep run: CI's <inheritdoc> sweep step"]
fn deep_generated_metadata_expands_exactly_as_roslyn() {
    check(cases(DEEP_CASES));
}

/// Worlds in the liveness sample.
const LIVENESS_WORLDS: usize = 256;

/// Dimensions the expansion declines as a class, so they reach declines
/// only: why each never expands.
const DECLINE_ONLY: [(Dim, &str); 5] = [
    (
        Dim::Misdeclared,
        "a MethodImpl declaration that binds no member gives Roslyn no candidate",
    ),
    (
        Dim::HiNamespaced,
        "a namespaced <member> makes Roslyn's qualified-name match uncertain for every key of the file",
    ),
    (
        Dim::ModReq,
        "the projection drops a member whose signature carries an unrecognised modreq",
    ),
    (
        Dim::ExplicitOverride,
        "a MethodImpl row naming a class method (an explicit override) is not modelled",
    ),
    (
        Dim::Covariant,
        "a covariant return is an explicit override's MethodImpl row, not modelled",
    ),
];

/// Per dimension: entries compared, expanded, declined, and without a start.
#[derive(Debug, Default)]
struct Reach {
    compared: usize,
    expanded: usize,
    declined: usize,
    no_start: usize,
    causes: BTreeMap<String, usize>,
}

/// Over a fixed sample: the census per dimension, and every dimension
/// reaching both an expansion and a decline.
#[test]
fn the_metadata_generator_reaches_every_dimension() {
    use proptest::strategy::ValueTree;
    let mut runner = TestRunner::deterministic();
    let mut reach: BTreeMap<Dim, Reach> = BTreeMap::new();
    for _ in 0..LIVENESS_WORLDS {
        let w = world().new_tree(&mut runner).unwrap().current();
        for (c, dims) in run(&w) {
            for d in dims {
                let r = reach.entry(d).or_default();
                r.compared += 1;
                match &c.verdict {
                    Verdict::Agrees => r.expanded += 1,
                    Verdict::Declined { cause, .. } => {
                        r.declined += 1;
                        *r.causes.entry(cause.clone()).or_default() += 1;
                    }
                    Verdict::NoStart(_) => r.no_start += 1,
                    Verdict::Disagrees { .. } => unreachable!("run panics on a disagreement"),
                }
            }
        }
    }
    eprintln!(
        "== metadata generator census (dimension: compared / expanded / declined / no start)"
    );
    for d in ALL_DIMS {
        let r = reach.get(&d);
        match r {
            Some(r) => eprintln!(
                "  {d:?}: {} / {} / {} / {}  {:?}",
                r.compared, r.expanded, r.declined, r.no_start, r.causes
            ),
            None => eprintln!("  {d:?}: never compared"),
        }
    }
    let dead: Vec<String> = ALL_DIMS
        .iter()
        .filter(|d| {
            !reach.get(d).is_some_and(|r| {
                r.declined > 0
                    && (r.expanded > 0
                        || DECLINE_ONLY.iter().any(|(x, _)| x == *d)
                        || OUT_OF_DOMAIN.iter().any(|(x, _)| x == *d))
            })
        })
        .map(|d| format!("{d:?}"))
        .collect();
    assert!(
        dead.is_empty(),
        "dimensions not reaching both an expansion and a decline: {}",
        dead.join(", ")
    );
}

/// A world with defaults everywhere but where a case says otherwise.
fn plain_world() -> World {
    World {
        core: Core::Pack,
        lo_culture: false,
        ref_culture: false,
        target: Target::Lo,
        ex: Ex::Loaded,
        twin: Twin::None,
        imethods: vec![IMethod {
            ret: STy::Int,
            param: STy::Int,
        }],
        iprops: vec![],
        bmethods: vec![BMethod {
            ret: STy::Int,
            param: STy::Int,
            ret_mod: RetMod::None,
            param_mod: RetMod::None,
            is_abstract: false,
            is_final: false,
        }],
        overs: vec![Over::Skip; 3],
        iimpls: vec![Impl::Implicit; 2],
        pimpls: vec![Impl::Implicit; 2],
        d2: false,
        cref: false,
        lo_layout: Layout::Indented,
        hi_layout: Layout::Indented,
        misdeclare: false,
    }
}

/// The worlds the generator found disagreeing with Roslyn, minimised, each
/// with the entry that disagreed: pinned so a change of strategy, which
/// reshuffles every seed, cannot lose them.
fn found_worlds() -> Vec<(&'static str, World, &'static str)> {
    let int_prop = |getter| IProp {
        ty: STy::Int,
        getter,
    };
    vec![
        (
            "a core library with references is none: `int` is an error type",
            World {
                core: Core::OwnReferencing,
                iprops: vec![int_prop(Getter::Conventional)],
                ..plain_world()
            },
            "P:N.D.P0",
        ),
        (
            "an override whose return lacks the base's modopt overrides nothing (#339)",
            World {
                bmethods: vec![BMethod {
                    ret_mod: RetMod::Opt,
                    ..plain_world().bmethods[0].clone()
                }],
                overs: vec![Over::Override(RetMod::None), Over::Skip, Over::Skip],
                ..plain_world()
            },
            "M:N.D.M0(System.Int32)",
        ),
        (
            "an implicit property whose getter is named like another property's",
            World {
                iprops: vec![
                    IProp {
                        ty: STy::T,
                        getter: Getter::Rotated,
                    },
                    IProp {
                        ty: STy::T,
                        getter: Getter::Conventional,
                    },
                ],
                ..plain_world()
            },
            "P:N.D.P0",
        ),
        (
            "a `virtual final newslot` base method is not virtual to C#",
            World {
                bmethods: vec![
                    plain_world().bmethods[0].clone(),
                    BMethod {
                        is_final: true,
                        ..plain_world().bmethods[0].clone()
                    },
                ],
                overs: vec![Over::Skip, Over::Override(RetMod::None), Over::Skip],
                ..plain_world()
            },
            "M:N.D.M1(System.Int32)",
        ),
    ]
}

/// Each found world now agrees with Roslyn, the entry that disagreed
/// declining.
#[test]
fn the_found_worlds_agree_with_roslyn() {
    for (why, w, key) in found_worlds() {
        let compared = run(&w);
        let verdict = compared
            .iter()
            .find(|(c, _)| c.key == key)
            .map(|(c, _)| &c.verdict);
        assert!(
            matches!(verdict, Some(Verdict::Declined { .. })),
            "{why}: {key}: {verdict:?}"
        );
    }
}

/// A `MethodImpl` row's declaration names the interface member it
/// implements, whatever the body's own signature: `C.X(int)` mapped to
/// `I.M(string)` inherits the string overload's documentation in Roslyn.
/// The declaration selects `M(string)`; a body disagreeing with the member
/// it implements is not modelled, and declines.
#[test]
fn an_explicit_implementation_whose_body_disagrees_declines() {
    let dir = TempDir::new().unwrap();
    let d = dir.path();
    let (runtime, core) = system_runtime();
    let lo = Ident::new("Lo", "1.0.0.0", "");
    let cx = Cx {
        core: 0,
        lo: -1,
        ex: -1,
    };
    let int = sig(STy::Int, RetMod::None, &cx);
    let string = sig(STy::Str, RetMod::None, &cx);
    let i_ref = json!({"scope": -1, "ns": "N", "name": "I"});
    let abstract_ = (true, true, true, false);
    let types = vec![
        json!({"ns": "N", "name": "I", "kind": "interface", "base": null, "interfaces": [], "properties": [],
        "methods": [
            method("M", "public", abstract_, false, int.clone(), vec![int.clone()], vec![]),
            method("M", "public", abstract_, false, int.clone(), vec![string.clone()], vec![]),
        ]}),
        json!({"ns": "N", "name": "C", "kind": "class", "base": object(&cx), "interfaces": [i_ref.clone()], "properties": [],
        "methods": [method(
            "X",
            "private",
            (true, true, false, true),
            false,
            int.clone(),
            vec![int.clone()],
            vec![json!({"parent": i_ref, "name": "M", "ret": int, "params": [string]})],
        )]}),
    ];
    let path = emit(d, "Lo", &lo, &[core], types, json!([]));
    let docs: Entries = vec![
        ("T:N.I".into(), full("T:N.I")),
        (
            "M:N.I.M(System.Int32)".into(),
            full("M:N.I.M(System.Int32)"),
        ),
        (
            "M:N.I.M(System.String)".into(),
            full("M:N.I.M(System.String)"),
        ),
        ("T:N.C".into(), full("T:N.C")),
        ("M:N.C.X(System.Int32)".into(), bare()),
    ];
    std::fs::write(
        path.with_extension("xml"),
        render_xml("Lo", &docs, Layout::Indented),
    )
    .unwrap();
    let fx = fixture_from(dir, path.clone(), vec![runtime.clone(), path]);
    let compared = compare(&fx);
    let mut census = crate::common::inheritdoc_diff::Census::default();
    census.add(&compared);
    census.print("an explicit implementation of an overloaded name");
    census.assert_sound();
    let verdict = compared
        .iter()
        .find(|c| c.key == "M:N.C.X(System.Int32)")
        .map(|c| &c.verdict);
    assert!(
        matches!(verdict, Some(Verdict::Declined { .. })),
        "{verdict:?}"
    );
}

/// `A.M()` returns `int[]`, `B : A` adds a new slot `M()` returning `int[*]`,
/// and `C : B` overrides the vector one: Roslyn inherits `A.M`'s
/// documentation. The projection refuses `int[*]` (it would spell it as
/// `int[]`), so `B.M` is a dropped member, and `C.M` declines rather than
/// take it for the method it overrides.
#[test]
fn an_override_beside_a_rank_one_array_declines() {
    let dir = TempDir::new().unwrap();
    let d = dir.path();
    let (runtime, core) = system_runtime();
    let lo = Ident::new("Lo", "1.0.0.0", "");
    let cx = Cx {
        core: 0,
        lo: -1,
        ex: -1,
    };
    let vector = json!({"type": {"szarray": {"prim": "I4"}}, "mods": []});
    let rank_one = json!({"type": {"array": {"prim": "I4"}}, "mods": []});
    let local = |name: &str| json!({"scope": -1, "ns": "N", "name": name});
    let m = |ret: &Value, newslot: bool| {
        method(
            "M",
            "public",
            (true, newslot, false, false),
            false,
            ret.clone(),
            vec![],
            vec![],
        )
    };
    let types = vec![
        class("N", "A", Some(object(&cx)), vec![m(&vector, true)]),
        class("N", "B", Some(local("A")), vec![m(&rank_one, true)]),
        class("N", "C", Some(local("B")), vec![m(&vector, false)]),
    ];
    let path = emit(d, "Lo", &lo, &[core], types, json!([]));
    let docs: Entries = vec![
        ("T:N.A".into(), full("T:N.A")),
        ("M:N.A.M".into(), full("M:N.A.M")),
        ("T:N.B".into(), full("T:N.B")),
        ("M:N.B.M".into(), full("M:N.B.M")),
        ("T:N.C".into(), full("T:N.C")),
        ("M:N.C.M".into(), bare()),
    ];
    std::fs::write(
        path.with_extension("xml"),
        render_xml("Lo", &docs, Layout::Indented),
    )
    .unwrap();
    let fx = fixture_from(dir, path.clone(), vec![runtime.clone(), path]);
    let compared = compare(&fx);
    let mut census = crate::common::inheritdoc_diff::Census::default();
    census.add(&compared);
    census.print("an override beside a rank-1 array");
    census.assert_sound();
    let verdict = compared
        .iter()
        .find(|c| c.key == "M:N.C.M")
        .map(|c| &c.verdict);
    assert!(
        matches!(verdict, Some(Verdict::Declined { .. })),
        "{verdict:?}"
    );
}

/// Signatures differing only by an optional modifier, C++/CLI's
/// `M(const int)` beside `M(int)` (#339). The projection drops the `modopt`,
/// but Roslyn's override comparer keeps it, so to Roslyn `C.M(int)` overrides
/// `B.M(int)` and not its `modopt(IsConst)` twin, and `C2.M(int)` steps past
/// `B2`, whose only `M` carries the modifier, to `A.M`. Read without the
/// modifier, `C2.M` would take `B2.M`'s documentation: the two are an unproven
/// equality, and each override declines (`B` and `B2` are not
/// compiler-shaped).
#[test]
fn an_override_beside_a_modopt_only_twin_declines() {
    let dir = TempDir::new().unwrap();
    let d = dir.path();
    let (runtime, core) = system_runtime();
    let lo = Ident::new("Lo", "1.0.0.0", "");
    let cx = Cx {
        core: 0,
        lo: -1,
        ex: -1,
    };
    let int = sig(STy::Int, RetMod::None, &cx);
    let const_int = sig(STy::Int, RetMod::Opt, &cx);
    let local = |name: &str| json!({"scope": -1, "ns": "N", "name": name});
    let m = |param: &Value, newslot: bool| {
        method(
            "M",
            "public",
            (true, newslot, false, false),
            false,
            int.clone(),
            vec![param.clone()],
            vec![],
        )
    };
    let types = vec![
        class("N", "A", Some(object(&cx)), vec![m(&int, true)]),
        class(
            "N",
            "B",
            Some(local("A")),
            vec![m(&const_int, true), m(&int, true)],
        ),
        class("N", "C", Some(local("B")), vec![m(&int, false)]),
        class("N", "B2", Some(local("A")), vec![m(&const_int, true)]),
        class("N", "C2", Some(local("B2")), vec![m(&int, false)]),
    ];
    let path = emit(d, "Lo", &lo, &[core], types, json!([]));
    let documented = |key: &str| (key.to_string(), full(key));
    let docs: Entries = vec![
        documented("T:N.A"),
        documented("M:N.A.M(System.Int32)"),
        documented("T:N.B"),
        documented("M:N.B.M(System.Int32)"),
        documented("T:N.C"),
        ("M:N.C.M(System.Int32)".into(), bare()),
        documented("T:N.B2"),
        documented("M:N.B2.M(System.Int32)"),
        documented("T:N.C2"),
        ("M:N.C2.M(System.Int32)".into(), bare()),
    ];
    std::fs::write(
        path.with_extension("xml"),
        render_xml("Lo", &docs, Layout::Indented),
    )
    .unwrap();
    let fx = fixture_from(dir, path.clone(), vec![runtime.clone(), path]);
    let compared = compare(&fx);
    let mut census = crate::common::inheritdoc_diff::Census::default();
    census.add(&compared);
    census.print("an override beside a modopt-only twin");
    census.assert_sound();
    // Roslyn expands both, so each decline is a question it answers.
    for key in ["M:N.C.M(System.Int32)", "M:N.C2.M(System.Int32)"] {
        let verdict = compared.iter().find(|c| c.key == key).map(|c| &c.verdict);
        assert!(
            matches!(
                verdict,
                Some(Verdict::Declined { cause, roslyn_left_it: false })
                    if cause.contains("OptionalModifier")
            ),
            "{key}: {verdict:?}"
        );
    }
}

/// `Lo` with interface `N.I` declaring `i_methods` (each documented under
/// its ID) and class `N.C : N.I` declaring `x`, whose entry `x_id` is a bare
/// `<inheritdoc/>`: compared against Roslyn, held sound, and `x_id`'s
/// verdict returned.
fn explicit_world(label: &str, i_methods: Vec<(Value, &str)>, x: Value, x_id: &str) -> Verdict {
    let dir = TempDir::new().unwrap();
    let d = dir.path();
    let (runtime, core) = system_runtime();
    let lo = Ident::new("Lo", "1.0.0.0", "");
    let cx = Cx {
        core: 0,
        lo: -1,
        ex: -1,
    };
    let i_ref = json!({"scope": -1, "ns": "N", "name": "I"});
    let mut docs: Entries = vec![("T:N.I".into(), full("T:N.I"))];
    docs.extend(i_methods.iter().map(|(_, id)| (id.to_string(), full(id))));
    docs.push(("T:N.C".into(), full("T:N.C")));
    docs.push((x_id.into(), bare()));
    let types = vec![
        json!({"ns": "N", "name": "I", "kind": "interface", "base": null, "interfaces": [], "properties": [],
            "methods": i_methods.into_iter().map(|(m, _)| m).collect::<Vec<_>>()}),
        json!({"ns": "N", "name": "C", "kind": "class", "base": object(&cx), "interfaces": [i_ref], "properties": [],
            "methods": [x]}),
    ];
    let path = emit(d, "Lo", &lo, &[core], types, json!([]));
    std::fs::write(
        path.with_extension("xml"),
        render_xml("Lo", &docs, Layout::Indented),
    )
    .unwrap();
    let fx = fixture_from(dir, path.clone(), vec![runtime.clone(), path]);
    let compared = compare(&fx);
    let mut census = crate::common::inheritdoc_diff::Census::default();
    census.add(&compared);
    census.print(label);
    census.assert_sound();
    compared
        .into_iter()
        .find(|c| c.key == x_id)
        .map(|c| c.verdict)
        .unwrap_or_else(|| panic!("{label}: no entry {x_id}"))
}

/// A `MethodImpl` declaration binds by name *and signature*: `C.X(): int`
/// mapped to `I.M(): bool`, where `I` declares only `M(): int`, binds
/// nothing in Roslyn, which then finds no candidate. An unoverloaded name
/// does not prove the declaration names the member.
#[test]
fn an_explicit_implementation_whose_declaration_matches_nothing_declines() {
    let cx = Cx {
        core: 0,
        lo: -1,
        ex: -1,
    };
    let int = sig(STy::Int, RetMod::None, &cx);
    let boolean = json!({"type": {"prim": "Bool"}, "mods": []});
    let i_ref = json!({"scope": -1, "ns": "N", "name": "I"});
    let verdict = explicit_world(
        "a declaration matching no member",
        vec![(
            method(
                "M",
                "public",
                (true, true, true, false),
                false,
                int.clone(),
                vec![],
                vec![],
            ),
            "M:N.I.M",
        )],
        method(
            "X",
            "private",
            (true, true, false, true),
            false,
            int,
            vec![],
            vec![json!({"parent": i_ref, "name": "M", "ret": boolean, "params": []})],
        ),
        "M:N.C.X",
    );
    assert!(matches!(verdict, Verdict::Declined { .. }), "{verdict:?}");
}

/// Roslyn imports a referenced interface's private non-virtual method not
/// at all, so a `MethodImpl` naming one binds nothing it can see.
#[test]
fn an_explicit_implementation_of_an_unimported_member_declines() {
    let cx = Cx {
        core: 0,
        lo: -1,
        ex: -1,
    };
    let int = sig(STy::Int, RetMod::None, &cx);
    let i_ref = json!({"scope": -1, "ns": "N", "name": "I"});
    let verdict = explicit_world(
        "a declaration naming an unimported member",
        vec![(
            method(
                "M",
                "private",
                (false, false, false, false),
                false,
                int.clone(),
                vec![],
                vec![],
            ),
            "M:N.I.M",
        )],
        method(
            "X",
            "public",
            (true, true, false, true),
            false,
            int.clone(),
            vec![],
            vec![json!({"parent": i_ref, "name": "M", "ret": int, "params": []})],
        ),
        "M:N.C.X",
    );
    assert!(matches!(verdict, Verdict::Declined { .. }), "{verdict:?}");
}

/// A signature naming a special type by reference (`class System.String`
/// rather than `ELEMENT_TYPE_STRING`) is one Roslyn treats as unsupported:
/// an implicit implementation returning one inherits nothing there.
#[test]
fn a_special_type_named_by_reference_declines() {
    let named_string = json!({"type": {"scope": 0, "ns": "System", "name": "String"}, "mods": []});
    let verdict = explicit_world(
        "a special type named by reference",
        vec![(
            method(
                "M",
                "public",
                (true, true, true, false),
                false,
                named_string.clone(),
                vec![],
                vec![],
            ),
            "M:N.I.M",
        )],
        method(
            "M",
            "public",
            (true, true, false, false),
            false,
            named_string,
            vec![],
            vec![],
        ),
        "M:N.C.M",
    );
    assert!(matches!(verdict, Verdict::Declined { .. }), "{verdict:?}");
}

/// `Lo` declaring `types` with `docs`, compared against Roslyn and held
/// sound: `key`'s verdict.
fn lo_world(label: &str, types: Vec<Value>, docs: &Entries, key: &str) -> Verdict {
    let dir = TempDir::new().unwrap();
    let (runtime, core) = system_runtime();
    let lo = Ident::new("Lo", "1.0.0.0", "");
    let path = emit(dir.path(), "Lo", &lo, &[core], types, json!([]));
    std::fs::write(
        path.with_extension("xml"),
        render_xml("Lo", docs, Layout::Indented),
    )
    .unwrap();
    let fx = fixture_from(dir, path.clone(), vec![runtime.clone(), path]);
    let compared = compare(&fx);
    let mut census = crate::common::inheritdoc_diff::Census::default();
    census.add(&compared);
    census.print(label);
    census.assert_sound();
    compared
        .into_iter()
        .find(|c| c.key == key)
        .map(|c| c.verdict)
        .unwrap_or_else(|| panic!("{label}: no entry {key}"))
}

/// `B` and `C : B` each declaring event `E` (`System.EventHandler`) through
/// accessors `add_E`, `remove_E` and `raise_E` with the given flags:
/// `(virtual, newslot)` for the adder and remover, then for the raiser.
/// `accessor_param` is the adder's and remover's parameter.
fn event_types(
    c_add: (bool, bool),
    c_fire: (bool, bool),
    fire: bool,
    accessor_param: &Value,
) -> Vec<Value> {
    let cx = Cx {
        core: 0,
        lo: -1,
        ex: -1,
    };
    let handler = json!({"scope": 0, "ns": "System", "name": "EventHandler"});
    let void = Value::Null;
    let accessors = |add: (bool, bool), fire_flags: (bool, bool)| {
        let m = |name: &str, (virtual_, newslot): (bool, bool), params: Vec<Value>| {
            method(
                name,
                "public",
                (virtual_, newslot, false, false),
                true,
                void.clone(),
                params,
                vec![],
            )
        };
        vec![
            m("add_E", add, vec![accessor_param.clone()]),
            m("remove_E", add, vec![accessor_param.clone()]),
            m("raise_E", fire_flags, vec![]),
        ]
    };
    let event = json!([{"name": "E", "type": handler, "add": 0, "remove": 1, "fire": if fire { json!(2) } else { Value::Null }}]);
    let local = |name: &str| json!({"scope": -1, "ns": "N", "name": name});
    vec![
        json!({"ns": "N", "name": "B", "kind": "class", "base": object(&cx), "interfaces": [], "properties": [],
            "methods": accessors((true, true), (true, true)), "events": event}),
        json!({"ns": "N", "name": "C", "kind": "class", "base": local("B"), "interfaces": [], "properties": [],
            "methods": accessors(c_add, c_fire), "events": event}),
    ]
}

fn event_docs() -> Entries {
    vec![
        ("T:N.B".into(), full("T:N.B")),
        ("E:N.B.E".into(), full("E:N.B.E")),
        ("T:N.C".into(), full("T:N.C")),
        ("E:N.C.E".into(), bare()),
    ]
}

/// No C#, VB or F# compiler writes an event's `fire` accessor (ILAsm and
/// C++/CLI do), so an event that has one declines. Here only `C`'s raiser
/// overrides `B`'s, its adder and remover are new slots.
#[test]
fn an_event_with_a_fire_accessor_declines() {
    let sig = json!({"type": {"scope": 0, "ns": "System", "name": "EventHandler"}, "mods": []});
    let verdict = lo_world(
        "an event with a fire accessor",
        event_types((true, true), (true, false), true, &sig),
        &event_docs(),
        "E:N.C.E",
    );
    assert!(matches!(verdict, Verdict::Declined { .. }), "{verdict:?}");
}

/// Compilers write an event's adder and remover to take the event's own
/// delegate type; accessors taking `object` for a `System.EventHandler`
/// event decline.
#[test]
fn an_event_whose_accessors_disagree_with_its_type_declines() {
    let sig = json!({"type": {"prim": "Object"}, "mods": []});
    let verdict = lo_world(
        "an event whose accessors disagree with its type",
        event_types((true, false), (true, true), false, &sig),
        &event_docs(),
        "E:N.C.E",
    );
    assert!(matches!(verdict, Verdict::Declined { .. }), "{verdict:?}");
}

/// A signature naming a method type parameter (`!!0`) its method does not
/// declare is no compiler's: `C.M(): !!0` overriding `B.M(): !!0`, neither
/// generic, declines.
#[test]
fn a_method_type_parameter_its_method_lacks_declines() {
    let cx = Cx {
        core: 0,
        lo: -1,
        ex: -1,
    };
    let mvar = json!({"type": {"mvar": 0}, "mods": []});
    let m = |newslot| {
        method(
            "M",
            "public",
            (true, newslot, false, false),
            false,
            mvar.clone(),
            vec![],
            vec![],
        )
    };
    let types = vec![
        class("N", "B", Some(object(&cx)), vec![m(true)]),
        class(
            "N",
            "C",
            Some(json!({"scope": -1, "ns": "N", "name": "B"})),
            vec![m(false)],
        ),
    ];
    let docs: Entries = vec![
        ("T:N.B".into(), full("T:N.B")),
        ("M:N.B.M".into(), full("M:N.B.M")),
        ("T:N.C".into(), full("T:N.C")),
        ("M:N.C.M".into(), bare()),
    ];
    let verdict = lo_world(
        "a method type parameter its method lacks",
        types,
        &docs,
        "M:N.C.M",
    );
    assert!(matches!(verdict, Verdict::Declined { .. }), "{verdict:?}");
}

/// A type named with a literal dot (`Odd.Type`, in namespace `F`) spells
/// the ID `F.Odd.Type`, which Roslyn's declaration-ID lookup reads as a
/// nested type: a `cref` naming a member whose signature takes it declines.
#[test]
fn a_cref_through_a_signature_type_named_with_a_dot_declines() {
    let cx = Cx {
        core: 0,
        lo: -1,
        ex: -1,
    };
    let odd = json!({"type": {"scope": -1, "ns": "F", "name": "Odd.Type"}, "mods": []});
    let m = method(
        "M",
        "public",
        (false, false, false, false),
        false,
        Value::Null,
        vec![odd],
        vec![],
    );
    let types = vec![
        class("F", "Odd.Type", Some(object(&cx)), vec![]),
        class("F", "B", Some(object(&cx)), vec![m]),
        class("F", "X", Some(object(&cx)), vec![]),
    ];
    let docs: Entries = vec![
        ("T:F.B".into(), full("T:F.B")),
        ("M:F.B.M(F.Odd.Type)".into(), full("M:F.B.M(F.Odd.Type)")),
        (
            "T:F.X".into(),
            vec![r#"<inheritdoc cref="M:F.B.M(F.Odd.Type)"/>"#.to_string()],
        ),
    ];
    let verdict = lo_world("a signature type named with a dot", types, &docs, "T:F.X");
    assert!(matches!(verdict, Verdict::Declined { .. }), "{verdict:?}");
}

/// A non-generic type named ``Z`1`` projects as `Z`; its ID is `T:F.Z` to
/// us but not to Roslyn, which reads the suffix as an arity. A `cref` to
/// `T:F.Z` declines.
#[test]
fn a_cref_to_a_type_whose_arity_suffix_disagrees_declines() {
    let cx = Cx {
        core: 0,
        lo: -1,
        ex: -1,
    };
    let types = vec![
        class("F", "Z`1", Some(object(&cx)), vec![]),
        class("F", "X", Some(object(&cx)), vec![]),
    ];
    let docs: Entries = vec![
        ("T:F.Z".into(), full("T:F.Z")),
        (
            "T:F.X".into(),
            vec![r#"<inheritdoc cref="T:F.Z"/>"#.to_string()],
        ),
    ];
    let verdict = lo_world(
        "an arity suffix disagreeing with the arity",
        types,
        &docs,
        "T:F.X",
    );
    assert!(matches!(verdict, Verdict::Declined { .. }), "{verdict:?}");
}

/// `D : B<Z>` inheriting `B<T>`'s summary, which names `T` by
/// `<typeparamref>`: Roslyn rewrites it to the argument's ID. With `Z` a
/// non-generic type named ``Z`1``, that ID is ``T:F.Z`1`` to Roslyn and
/// `T:F.Z` to the projection, so the expansion declines.
#[test]
fn a_type_argument_whose_arity_suffix_disagrees_declines() {
    let cx = Cx {
        core: 0,
        lo: -1,
        ex: -1,
    };
    let local = |name: &str| json!({"scope": -1, "ns": "F", "name": name});
    let types = vec![
        class("F", "Z`1", Some(object(&cx)), vec![]),
        json!({"ns": "F", "name": "B`1", "kind": "class", "base": object(&cx), "interfaces": [],
            "methods": [], "properties": [], "generic": ["T"]}),
        class(
            "F",
            "D",
            Some(json!({"inst": local("B`1"), "args": [local("Z`1")]})),
            vec![],
        ),
    ];
    let docs: Entries = vec![
        ("T:F.Z".into(), full("T:F.Z")),
        (
            "T:F.B`1".into(),
            vec![r#"<summary>Of <typeparamref name="T"/>.</summary>"#.to_string()],
        ),
        ("T:F.D".into(), bare()),
    ];
    let verdict = lo_world("a type argument's arity suffix", types, &docs, "T:F.D");
    assert!(matches!(verdict, Verdict::Declined { .. }), "{verdict:?}");
}

/// `D : B` inheriting from `B`, nested in a non-generic type named ``Z`1``:
/// `B`'s ID is ``T:F.Z`1.B`` to Roslyn and `T:F.Z.B` to the projection, so
/// the candidate's enclosing type is outside the domain and `D` declines.
#[test]
fn a_candidate_nested_in_a_type_whose_arity_suffix_disagrees_declines() {
    let cx = Cx {
        core: 0,
        lo: -1,
        ex: -1,
    };
    let types = vec![
        class("F", "Z`1", Some(object(&cx)), vec![]),
        json!({"ns": "", "name": "B", "kind": "class", "base": object(&cx), "interfaces": [],
            "methods": [], "properties": [], "nestedIn": 0}),
        class(
            "F",
            "D",
            Some(json!({"scope": -1, "ns": "", "name": "B"})),
            vec![],
        ),
    ];
    let docs: Entries = vec![
        ("T:F.Z".into(), full("T:F.Z")),
        ("T:F.Z.B".into(), full("T:F.Z.B")),
        ("T:F.D".into(), bare()),
    ];
    let verdict = lo_world(
        "a candidate nested in a type's arity suffix",
        types,
        &docs,
        "T:F.D",
    );
    assert!(matches!(verdict, Verdict::Declined { .. }), "{verdict:?}");
}

/// `X`'s entry inherits through `cref` from `C`'s member documented under
/// `member_id`, the member being `member` (and, for a property, `property`).
fn cref_world(label: &str, members: (Vec<Value>, Vec<Value>), member_id: &str) -> Verdict {
    let cx = Cx {
        core: 0,
        lo: -1,
        ex: -1,
    };
    let (methods, properties) = members;
    let types = vec![
        json!({"ns": "N", "name": "G`1", "kind": "class", "base": object(&cx), "interfaces": [],
            "methods": [], "properties": [], "generic": ["T"]}),
        json!({"ns": "N", "name": "C", "kind": "class", "base": object(&cx), "interfaces": [],
            "methods": methods, "properties": properties}),
        class("N", "X", Some(object(&cx)), vec![]),
    ];
    let docs: Entries = vec![
        ("T:N.C".into(), full("T:N.C")),
        (member_id.into(), full(member_id)),
        (
            "T:N.X".into(),
            vec![format!(r#"<inheritdoc cref="{member_id}"/>"#)],
        ),
    ];
    lo_world(label, types, &docs, "T:N.X")
}

/// C# spells `string` by its element type; a parameter encoded as
/// `class System.String` is outside the domain, so a `cref` to its method
/// declines.
#[test]
fn a_cref_to_a_method_spelling_string_by_reference_declines() {
    let by_reference = json!({"type": {"scope": 0, "ns": "System", "name": "String"}, "mods": []});
    let m = method(
        "M",
        "public",
        (false, false, false, false),
        false,
        Value::Null,
        vec![by_reference],
        vec![],
    );
    let verdict = cref_world(
        "string by reference",
        (vec![m], vec![]),
        "M:N.C.M(System.String)",
    );
    assert!(matches!(verdict, Verdict::Declined { .. }), "{verdict:?}");
}

/// `G<T>` constructed with two arguments in a signature is no compiler's.
#[test]
fn a_cref_to_a_method_constructing_with_too_many_arguments_declines() {
    let g = json!({"type": {"inst": {"scope": -1, "ns": "N", "name": "G`1"},
        "args": [{"prim": "I4"}, {"prim": "String"}]}, "mods": []});
    let m = method(
        "M",
        "public",
        (false, false, false, false),
        false,
        Value::Null,
        vec![g],
        vec![],
    );
    let verdict = cref_world(
        "too many type arguments",
        (vec![m], vec![]),
        "M:N.C.M(N.G{System.Int32,System.String})",
    );
    assert!(matches!(verdict, Verdict::Declined { .. }), "{verdict:?}");
}

/// An instance property over a static getter taking an `int` (F#'s shape,
/// read as IL): the projection reads the getter's parameter as an index,
/// Roslyn reads the property, so a `cref` to it declines.
#[test]
fn a_cref_to_an_instance_property_over_a_static_getter_declines() {
    let getter = json!({
        "name": "get_P", "access": "public", "static": true, "virtual": false, "newslot": false,
        "abstract": false, "final": false, "specialName": true,
        "ret": {"type": {"prim": "I4"}, "mods": []},
        "params": [{"type": {"prim": "I4"}, "mods": []}], "impls": [],
    });
    let property = json!({"name": "P", "type": {"type": {"prim": "I4"}, "mods": []}, "getter": 0});
    let verdict = cref_world(
        "an instance property over a static getter",
        (vec![getter], vec![property]),
        "P:N.C.P(System.Int32)",
    );
    assert!(matches!(verdict, Verdict::Declined { .. }), "{verdict:?}");
}

/// An array shape: rank, sizes, lower bounds.
type ArrayShape<'a> = (u32, &'a [u32], &'a [i32]);

/// A `cref` to a method `M(int[...])` whose parameter has the array `shape`,
/// spelled `id`.
fn array_cref_world(label: &str, shape: ArrayShape, id: &str) -> Verdict {
    let (rank, sizes, lobounds) = shape;
    let parameter = json!({"type": {"array": {"prim": "I4"}, "rank": rank, "sizes": sizes,
        "lobounds": lobounds}, "mods": []});
    let m = method(
        "M",
        "public",
        (false, false, false, false),
        false,
        Value::Null,
        vec![parameter],
        vec![],
    );
    cref_world(label, (vec![m], vec![]), id)
}

/// C# writes `int[,]` as a rank-2 shape with no sizes and both lower bounds
/// zero; a `cref` to it expands, as Roslyn's does.
#[test]
fn a_cref_to_a_method_taking_a_csharp_multidimensional_array_expands() {
    let verdict = array_cref_world(
        "C#'s int[,]",
        (2, &[], &[0, 0]),
        "M:N.C.M(System.Int32[0:,0:])",
    );
    assert!(matches!(verdict, Verdict::Agrees), "{verdict:?}");
}

/// Array shapes C# never writes — a non-zero lower bound, a declared size, a
/// rank-1 array that is not the vector — are outside the domain, so a `cref`
/// spelling the shape's own documentation ID declines.
#[test]
fn a_cref_to_a_method_taking_an_array_shape_csharp_does_not_write_declines() {
    let shapes: [(&str, ArrayShape, &str); 4] = [
        (
            "a non-zero lower bound",
            (2, &[], &[3, 0]),
            "M:N.C.M(System.Int32[3:,0:])",
        ),
        (
            "a declared size",
            (2, &[2], &[0, 0]),
            "M:N.C.M(System.Int32[0:2,0:])",
        ),
        (
            "a rank-1 array",
            (1, &[], &[0]),
            "M:N.C.M(System.Int32[0:])",
        ),
        (
            "a rank-2 array with no bounds",
            (2, &[], &[]),
            "M:N.C.M(System.Int32[0:,0:])",
        ),
    ];
    let undeclined: Vec<_> = shapes
        .into_iter()
        .map(|(label, shape, id)| (label, array_cref_world(label, shape, id)))
        .filter(|(_, verdict)| !matches!(verdict, Verdict::Declined { .. }))
        .collect();
    assert!(undeclined.is_empty(), "{undeclined:#?}");
}
