//! The identity of assembly references and type forwarders.
//!
//! Roslyn binds an `AssemblyRef` to a loaded assembly by identity — simple
//! name, public-key token, version — and a forwarded type through its
//! forwarder's own reference by identity again. This draws the library a
//! fixture's base types come from (System.Runtime itself, or a library built
//! here, unsigned or strong-named with one of two keys), reaches it directly
//! or through a forwarder, rewrites one reference on the way (its version up
//! or down at each part, its token another key's, nobody's, or gone), and may
//! load a decoy beside the library: an assembly of the same simple name at
//! another version, declaring the same types with documentation of its own,
//! before or after it. Every expansion must still agree with Roslyn's,
//! whichever assembly Roslyn binds each reference to.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use borzoi::assembly_cache::AssemblyCache;
use borzoi::xml_doc::lookup::DocSources;
use proptest::prelude::*;
use tempfile::TempDir;

use super::{
    DEEP_CASES, DEFAULT_CASES, Reach, assert_every_dimension_expands, assert_sound, cases,
    check_each, oracle_guard, reach_by, sample,
};
use crate::common::ensure_system_runtime_dll;
use crate::common::inheritdoc_diff::{Compared, Verdict, build_env, compare_assembly};
use crate::common::inheritdoc_oracle::AssemblyRefPatch;

/// A key a library here may be public-signed with: the public key of one of
/// the reference pack's assemblies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Key {
    /// `netstandard.dll`'s, token `cc7b13ffcd2ddd51`.
    Netstandard,
    /// `System.Runtime.dll`'s, token `b03f5f7f11d50a3a`.
    Runtime,
}

impl Key {
    fn token(self) -> &'static str {
        match self {
            Key::Netstandard => "cc7b13ffcd2ddd51",
            Key::Runtime => "b03f5f7f11d50a3a",
        }
    }

    fn dll(self) -> PathBuf {
        let runtime = ensure_system_runtime_dll();
        match self {
            Key::Netstandard => runtime.with_file_name("netstandard.dll"),
            Key::Runtime => runtime,
        }
    }
}

/// Where the base types come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Lib {
    /// System.Runtime, version 10.0.0.0.
    Runtime,
    /// `Lib`, version 2.0.0.0, strong-named with this key or not.
    Built(Option<Key>),
}

/// The forwarder's own signing, when the base types are reached through one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Route {
    Direct,
    /// Through `Fwd`, version 5.0.0.0, strong-named with this key or not.
    Forwarded(Option<Key>),
}

/// Which reference is rewritten.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Target {
    /// The fixture's reference to the library, or (forwarded) to `Fwd`.
    Fixture,
    /// `Fwd`'s reference to the library.
    Forwarder,
}

/// The rewrite of one reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Patch {
    Unchanged,
    /// Its token, that of this key.
    Token(Key),
    /// Its token, one no key here has.
    TokenOfNobody,
    DropToken,
    HigherMajor,
    LowerMajor,
    HigherMinor,
    HigherRevision,
}

/// An assembly of the library's simple name at 1.0.0.0 (lower), 2.0.0.0
/// (the library's own), or 3.0.0.0 (what [`Patch::HigherMajor`]
/// references). The env unifies a lower duplicate of the same name and token
/// away, as MSBuild's conflict resolution does before any compiler runs, and
/// keeps an equal one beside the library.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Decoy {
    Lower,
    Equal,
    Higher,
}

#[derive(Debug, Clone)]
struct Case {
    lib: Lib,
    route: Route,
    target: Target,
    patch: Patch,
    /// A decoy (only beside a library built here), signed like it or not.
    decoy: Option<(Decoy, Option<Key>)>,
    /// Whether the decoy precedes the library in the reference set.
    decoy_first: bool,
}

fn key() -> impl Strategy<Value = Key> {
    prop_oneof![Just(Key::Netstandard), Just(Key::Runtime)]
}

fn signing() -> impl Strategy<Value = Option<Key>> {
    prop_oneof![1 => Just(None), 2 => key().prop_map(Some)]
}

fn strategy() -> impl Strategy<Value = Case> {
    (
        prop_oneof![
            1 => Just(Lib::Runtime),
            3 => signing().prop_map(Lib::Built),
        ],
        prop_oneof![1 => Just(Route::Direct), 1 => signing().prop_map(Route::Forwarded)],
        prop_oneof![Just(Target::Fixture), Just(Target::Forwarder)],
        prop_oneof![
            2 => Just(Patch::Unchanged),
            1 => key().prop_map(Patch::Token),
            1 => Just(Patch::TokenOfNobody),
            1 => Just(Patch::DropToken),
            1 => Just(Patch::HigherMajor),
            1 => Just(Patch::LowerMajor),
            1 => Just(Patch::HigherMinor),
            1 => Just(Patch::HigherRevision),
        ],
        prop::option::weighted(
            0.5,
            (
                prop_oneof![Just(Decoy::Lower), Just(Decoy::Equal), Just(Decoy::Higher)],
                signing(),
            ),
        ),
        any::<bool>(),
    )
        .prop_map(|(lib, route, target, patch, decoy, decoy_first)| {
            // System.Runtime is not forwarded to here; a decoy is built
            // beside a built library only; and a forwarder has a reference
            // of its own to rewrite only when there is one.
            let route = if lib == Lib::Runtime {
                Route::Direct
            } else {
                route
            };
            let target = if route == Route::Direct {
                Target::Fixture
            } else {
                target
            };
            let decoy = if lib == Lib::Runtime { None } else { decoy };
            Case {
                lib,
                route,
                target,
                patch,
                decoy,
                decoy_first,
            }
        })
}

impl Case {
    /// The simple name, version and signing of the assembly the rewritten
    /// reference names.
    fn referenced(&self) -> (&'static str, [u16; 4], Option<Key>) {
        match (self.target, self.route, self.lib) {
            (Target::Fixture, Route::Forwarded(key), _) => ("Fwd", [5, 0, 0, 0], key),
            (_, _, Lib::Runtime) => ("System.Runtime", [10, 0, 0, 0], Some(Key::Runtime)),
            (_, _, Lib::Built(key)) => ("Lib", [2, 0, 0, 0], key),
        }
    }

    /// The rewrite as the oracle takes it, or `None` where it would change
    /// nothing or is impossible: a token rewrite needs a token to rewrite,
    /// and one the referencing assembly's reference to System.Runtime does
    /// not share (the oracle rewrites the blob in place).
    fn patch(&self) -> Option<AssemblyRefPatch> {
        let (name, [a, b, c, d], key) = self.referenced();
        let rewritable = key.is_some() && (key != Some(Key::Runtime) || name == "System.Runtime");
        let version = |v: [u16; 4]| -> &'static str {
            Box::leak(format!("{}.{}.{}.{}", v[0], v[1], v[2], v[3]).into_boxed_str())
        };
        let other_token = |k: Key| -> Option<&'static str> {
            (rewritable && Some(k) != key).then_some(k.token())
        };
        match self.patch {
            Patch::Unchanged => None,
            Patch::Token(k) => other_token(k).map(|token| AssemblyRefPatch {
                token: Some(token),
                ..Default::default()
            }),
            Patch::TokenOfNobody => rewritable.then_some(AssemblyRefPatch {
                token: Some("0123456789abcdef"),
                ..Default::default()
            }),
            Patch::DropToken => key.map(|_| AssemblyRefPatch {
                drop_token: true,
                ..Default::default()
            }),
            Patch::HigherMajor => Some(AssemblyRefPatch {
                version: Some(version([a + 1, b, c, d])),
                ..Default::default()
            }),
            Patch::LowerMajor => Some(AssemblyRefPatch {
                version: Some(version([a - 1, b, c, d])),
                ..Default::default()
            }),
            Patch::HigherMinor => Some(AssemblyRefPatch {
                version: Some(version([a, b + 1, c, d])),
                ..Default::default()
            }),
            Patch::HigherRevision => Some(AssemblyRefPatch {
                version: Some(version([a, b, c, d + 1])),
                ..Default::default()
            }),
        }
    }

    fn dims(&self) -> BTreeSet<Dim> {
        let mut out = BTreeSet::from([
            Dim::Lib(self.lib),
            Dim::Route(self.route),
            Dim::Target(self.target),
        ]);
        let patch = if self.patch().is_some() {
            self.patch
        } else {
            Patch::Unchanged
        };
        out.insert(Dim::Patch(patch));
        match self.decoy {
            None => {}
            Some((d, _)) => {
                out.insert(Dim::Decoy(d));
                out.insert(if self.decoy_first {
                    Dim::DecoyFirst
                } else {
                    Dim::DecoyAfter
                });
            }
        }
        out
    }
}

/// A dimension of the vocabulary; every compared entry of a case carries
/// all of the case's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Dim {
    Lib(Lib),
    Route(Route),
    Target(Target),
    Patch(Patch),
    Decoy(Decoy),
    DecoyFirst,
    DecoyAfter,
}

/// The dimensions some sampled entry must expand under: all but those of
/// [`DECLINING`].
fn expanding() -> Vec<Dim> {
    let mut out = vec![
        Dim::Lib(Lib::Runtime),
        Dim::Lib(Lib::Built(None)),
        Dim::Lib(Lib::Built(Some(Key::Netstandard))),
        Dim::Lib(Lib::Built(Some(Key::Runtime))),
        Dim::Route(Route::Direct),
        Dim::Route(Route::Forwarded(None)),
        Dim::Route(Route::Forwarded(Some(Key::Netstandard))),
        Dim::Route(Route::Forwarded(Some(Key::Runtime))),
        Dim::Target(Target::Fixture),
        Dim::Target(Target::Forwarder),
        Dim::Patch(Patch::Unchanged),
        Dim::Decoy(Decoy::Lower),
        Dim::Decoy(Decoy::Higher),
        Dim::DecoyFirst,
        Dim::DecoyAfter,
    ];
    out.extend(
        [
            Patch::Token(Key::Netstandard),
            Patch::Token(Key::Runtime),
            Patch::DropToken,
            Patch::HigherMajor,
            Patch::LowerMajor,
            Patch::HigherMinor,
            Patch::HigherRevision,
        ]
        .map(Dim::Patch),
    );
    out
}

/// The dimensions under which the expansion must decline, and some sampled
/// entry does: a token no loaded assembly carries, so Roslyn's reference
/// binds nothing; and a decoy of the library's own identity, which the env
/// keeps beside it, so every type of the name binds twice (Roslyn keeps one
/// of the two).
const DECLINING: [Dim; 2] = [Dim::Patch(Patch::TokenOfNobody), Dim::Decoy(Decoy::Equal)];

fn types_source(version: &str, tag: &str) -> String {
    format!(
        r#"[assembly: System.Reflection.AssemblyVersion("{version}")]
namespace L
{{
    /// <summary>Base, from {tag}.</summary>
    public class Base
    {{
        /// <summary>Base.M, from {tag}.</summary>
        public virtual void M() {{ }}
    }}

    /// <summary>IThing, from {tag}.</summary>
    public interface IThing
    {{
        /// <summary>IThing.Do, from {tag}.</summary>
        void Do();
    }}
}}
"#
    )
}

const FX_OVER_LIB: &str = r#"
namespace G;

/// <inheritdoc/>
public class D : L.Base, L.IThing
{
    /// <inheritdoc/>
    public override void M() { }

    /// <inheritdoc/>
    public void Do() { }

    /// <inheritdoc cref="L.Base.M"/>
    public void Cref() { }
}
"#;

const FX_OVER_RUNTIME: &str = r#"
namespace G;

/// <inheritdoc/>
public class D : System.Exception, System.IDisposable
{
    /// <inheritdoc/>
    public override string ToString() => "";

    /// <inheritdoc/>
    public void Dispose() { }

    /// <inheritdoc cref="System.IDisposable.Dispose"/>
    public void Cref() { }
}
"#;

const FORWARDER: &str = r#"[assembly: System.Reflection.AssemblyVersion("5.0.0.0")]
[assembly: System.Runtime.CompilerServices.TypeForwardedTo(typeof(L.Base))]
[assembly: System.Runtime.CompilerServices.TypeForwardedTo(typeof(L.IThing))]
"#;

/// Compile `source` as `name` into its own directory under `dir`, signed
/// with `key` if any.
fn compile(
    dir: &Path,
    sub: &str,
    source: &str,
    name: &str,
    references: &[PathBuf],
    key: Option<Key>,
) -> PathBuf {
    let out = dir.join(sub);
    let built = {
        let mut oracle = oracle_guard();
        match key {
            Some(k) => oracle.compile_signed(source, name, &out, references, &k.dll()),
            None => oracle.compile(source, name, &out, references),
        }
    };
    built.unwrap_or_else(|e| panic!("{sub} does not compile: {e:#?}\n{source}"))
}

/// One case's comparisons.
fn run(c: &Case) -> Vec<Compared> {
    let dir = TempDir::new().unwrap();
    let d = dir.path();
    let runtime = ensure_system_runtime_dll();
    let rt = std::slice::from_ref(&runtime);
    // The library, and the assembly the fixture is compiled against.
    let (lib, fx_source) = match c.lib {
        Lib::Runtime => (runtime.clone(), FX_OVER_RUNTIME),
        Lib::Built(key) => (
            compile(d, "lib", &types_source("2.0.0.0", "Lib"), "Lib", rt, key),
            FX_OVER_LIB,
        ),
    };
    let (fwd, fx_against) = match c.route {
        Route::Direct => (None, lib.clone()),
        Route::Forwarded(key) => {
            let defining = compile(
                d,
                "fwd-defining",
                &types_source("5.0.0.0", "the defining Fwd"),
                "Fwd",
                rt,
                key,
            );
            let fwd = compile(
                d,
                "fwd",
                FORWARDER,
                "Fwd",
                &[runtime.clone(), lib.clone()],
                key,
            );
            (Some(fwd), defining)
        }
    };
    let fx_refs = if fx_against == runtime {
        vec![runtime.clone()]
    } else {
        vec![runtime.clone(), fx_against]
    };
    let fx = compile(d, "fx", fx_source, "Fx", &fx_refs, None);
    let decoy = c.decoy.map(|(which, key)| {
        let version = match which {
            Decoy::Lower => "1.0.0.0",
            Decoy::Equal => "2.0.0.0",
            Decoy::Higher => "3.0.0.0",
        };
        compile(
            d,
            "decoy",
            &types_source(version, "the decoy"),
            "Lib",
            rt,
            key,
        )
    });
    if let Some(patch) = c.patch() {
        let (name, ..) = c.referenced();
        let patched = match c.target {
            Target::Fixture => &fx,
            Target::Forwarder => fwd.as_ref().expect("a forwarder to rewrite"),
        };
        oracle_guard().patch_assembly_ref(patched, name, &patch);
    }
    let mut references = vec![runtime.clone()];
    references.extend(fwd);
    let mut libs = Vec::new();
    if lib != runtime {
        libs.push(lib);
    }
    if let Some(decoy) = decoy {
        if c.decoy_first {
            libs.insert(0, decoy);
        } else {
            libs.push(decoy);
        }
    }
    references.extend(libs);
    references.push(fx.clone());
    // The reference pack is projected once, not per case.
    static CACHE: OnceLock<(TempDir, AssemblyCache)> = OnceLock::new();
    let (_, cache) = CACHE.get_or_init(|| {
        let dir = TempDir::new().unwrap();
        let cache = AssemblyCache::at(dir.path().to_path_buf());
        (dir, cache)
    });
    let (env, references) = build_env(&references, cache);
    static SOURCES: OnceLock<Mutex<DocSources>> = OnceLock::new();
    let mut sources = SOURCES
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let compared = compare_assembly(&env, &mut sources, &references, &fx);
    assert_sound(&compared, &format!("{c:#?}\npatch: {:?}", c.patch()));
    compared
}

#[test]
fn generated_reference_identities_expand_exactly_as_roslyn() {
    check_each(strategy(), cases(DEFAULT_CASES), |c| {
        run(&c);
    });
}

#[test]
#[ignore = "deep run: CI's <inheritdoc> sweep step"]
fn deep_generated_reference_identities_expand_exactly_as_roslyn() {
    check_each(strategy(), cases(DEEP_CASES), |c| {
        run(&c);
    });
}

/// Every dimension reaches an expanded entry, but those of [`DECLINING`],
/// which reach a decline.
#[test]
fn the_reference_identity_generator_reaches_every_dimension() {
    let mut reach: BTreeMap<Dim, Reach> = BTreeMap::new();
    let mut declined: BTreeSet<Dim> = BTreeSet::new();
    for case in sample(&strategy(), 64) {
        let dims = case.dims();
        for c in run(&case) {
            reach_by(&mut reach, dims.iter().copied(), &c.verdict);
            if matches!(c.verdict, Verdict::Declined { .. }) {
                declined.extend(dims.iter().copied());
            }
        }
    }
    assert_every_dimension_expands("reference identities", &expanding(), &reach);
    let unreached: Vec<Dim> = DECLINING
        .into_iter()
        .filter(|d| !declined.contains(d))
        .collect();
    assert!(
        unreached.is_empty(),
        "reference identities: no sampled entry declined under {unreached:?}"
    );
}
