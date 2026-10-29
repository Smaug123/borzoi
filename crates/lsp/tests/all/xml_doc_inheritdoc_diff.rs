//! `<inheritdoc>` expansion against Roslyn's IDE expansion, over purpose-built
//! C# fixtures compiled by `tools/inheritdoc-oracle` (so the `.xml` is the C#
//! compiler's own, `<inheritdoc>` left verbatim, crefs resolved to IDs).
//!
//! Each fixture is a reference set of the reference pack's `System.Runtime`
//! plus the fixture itself — the same set for the env and for Roslyn's
//! compilation. Every inheritdoc-bearing entry is compared under
//! certain-implies-exact ([`crate::common::inheritdoc_diff`]); on top of that,
//! the named cases pin which entries must *expand* (so the comparison cannot
//! pass by declining everything) and which must decline.

use std::path::PathBuf;
use std::sync::Arc;

use borzoi::assembly_cache::AssemblyCache;
use borzoi::semantic::build_env_from_dll_paths;
use borzoi::xml_doc::lookup::DocSources;
use borzoi_sema::AssemblyEnv;
use tempfile::TempDir;

use crate::common::ensure_system_runtime_dll;
use crate::common::inheritdoc_diff::{Census, Compared, Verdict, compare_assembly};
use crate::common::inheritdoc_oracle::{AssemblyRefPatch, oracle};

/// A compiled fixture and the env over its reference set.
pub struct Fixture {
    _dir: TempDir,
    pub dll: PathBuf,
    pub references: Vec<PathBuf>,
    pub env: Arc<AssemblyEnv>,
}

/// Compile `source` (as `Fx`) and build the env over System.Runtime + it.
/// `Err` holds the compiler's errors when it rejects the source.
pub fn fixture(source: &str) -> Result<Fixture, Vec<String>> {
    fixture_missing(source, None)
}

/// [`fixture`], with `Fx` compiled against a `Dep` assembly built from
/// `dependency` that the reference set then leaves out — the shape of a
/// package whose dependency was not restored.
pub fn fixture_missing(source: &str, dependency: Option<&str>) -> Result<Fixture, Vec<String>> {
    let deps: Vec<Dep<'_>> = dependency
        .map(|source| Dep {
            name: "Dep",
            source,
            alias: None,
            referenced: false,
            against: &[],
            visible: true,
            patch: None,
        })
        .into_iter()
        .collect();
    fixture_with(source, &deps)
}

/// A dependency assembly of a fixture.
pub struct Dep<'a> {
    pub name: &'a str,
    pub source: &'a str,
    /// The `extern alias` `Fx` reaches it through, if any.
    pub alias: Option<&'a str>,
    /// Whether the reference set (env and Roslyn alike) includes it.
    pub referenced: bool,
    /// The earlier dependencies (by position) it is compiled against.
    pub against: &'a [usize],
    /// Whether `Fx` is compiled against it.
    pub visible: bool,
    /// A rewrite of the compiled DLL, for metadata no C# compiler writes.
    pub patch: Option<fn(&std::path::Path)>,
}

/// [`fixture`], with `Fx` compiled against dependency assemblies built from
/// `deps` (each against System.Runtime alone).
pub fn fixture_with(source: &str, deps: &[Dep<'_>]) -> Result<Fixture, Vec<String>> {
    fixture_over(source, deps, true, None)
}

/// [`fixture`], with `Fx`'s compiled DLL rewritten by `patch` before the
/// env (and Roslyn) read it.
pub fn fixture_patched(
    source: &str,
    patch: &dyn Fn(&std::path::Path),
) -> Result<Fixture, Vec<String>> {
    fixture_over(source, &[], true, Some(patch))
}

/// [`fixture`], compiled against System.Runtime but with a reference set
/// (env and Roslyn alike) that leaves it out: no core library, so no
/// primitive type binds.
pub fn fixture_without_core_library(source: &str) -> Result<Fixture, Vec<String>> {
    fixture_over(source, &[], false, None)
}

fn fixture_over(
    source: &str,
    deps: &[Dep<'_>],
    core: bool,
    patch: Option<&dyn Fn(&std::path::Path)>,
) -> Result<Fixture, Vec<String>> {
    let dir = TempDir::new().unwrap();
    let runtime = ensure_system_runtime_dll();
    let mut compile_against: Vec<(PathBuf, Option<&str>)> = vec![(runtime.clone(), None)];
    let mut references = if core {
        vec![runtime.clone()]
    } else {
        Vec::new()
    };
    let mut built: Vec<PathBuf> = Vec::new();
    for (i, dep) in deps.iter().enumerate() {
        let dep_dir = dir.path().join(format!("dep{i}"));
        let mut against = vec![runtime.clone()];
        against.extend(dep.against.iter().map(|&j| built[j].clone()));
        let dll = oracle()
            .lock()
            .unwrap()
            .compile(dep.source, dep.name, &dep_dir, &against)?;
        if let Some(patch) = dep.patch {
            patch(&dll);
        }
        if dep.visible {
            compile_against.push((dll.clone(), dep.alias));
        }
        if dep.referenced {
            references.push(dll.clone());
        }
        built.push(dll);
    }
    let dll =
        oracle()
            .lock()
            .unwrap()
            .compile_aliased(source, "Fx", dir.path(), &compile_against)?;
    if let Some(patch) = patch {
        patch(&dll);
    }
    references.push(dll.clone());
    Ok(fixture_from(dir, dll, references))
}

/// The fixture over `references` (files under `dir`, which it keeps alive),
/// comparing `dll`'s entries.
pub fn fixture_from(dir: TempDir, dll: PathBuf, references: Vec<PathBuf>) -> Fixture {
    // The runtime's on-disk projection cache, in a directory of this test
    // binary's own: the reference pack is projected once, not per fixture.
    static CACHE: std::sync::OnceLock<(TempDir, AssemblyCache)> = std::sync::OnceLock::new();
    let (_, cache) = CACHE.get_or_init(|| {
        let dir = TempDir::new().unwrap();
        let cache = AssemblyCache::at(dir.path().to_path_buf());
        (dir, cache)
    });
    let (env, _) = build_env_from_dll_paths(references.iter().map(PathBuf::as_path), cache);
    Fixture {
        _dir: dir,
        dll,
        references,
        env: Arc::new(env),
    }
}

/// Compare `fx`'s entries. The doc caches are shared across the binary's
/// fixtures, as the server shares them across envs: each parsed file is
/// validated against its stamp, each census and index against env identity,
/// so sharing changes no answer — it spares re-parsing the reference pack's
/// 7.6 MB `System.Runtime.xml` per fixture.
pub fn compare(fx: &Fixture) -> Vec<Compared> {
    static SOURCES: std::sync::OnceLock<std::sync::Mutex<DocSources>> = std::sync::OnceLock::new();
    let mut sources = SOURCES
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    compare_assembly(&fx.env, &mut sources, &fx.references, &fx.dll)
}

const HANDWRITTEN: &str = r#"
namespace F;

/// <summary>The base.</summary>
public class B
{
    /// <summary>Makes a B from <paramref name="s"/>.</summary>
    /// <param name="s">The text.</param>
    public B(string s) { }

    /// <summary>B.M doubles.</summary>
    /// <param name="x">The number.</param>
    /// <returns>Twice <paramref name="x"/>.</returns>
    /// <remarks>Remarks of B.M.</remarks>
    public virtual int M(int x) => 2 * x;

    /// <summary>B.P.</summary>
    /// <value>The value.</value>
    public virtual string P { get; set; } = "";

    /// <summary>B.Changed.</summary>
    public virtual event System.EventHandler? Changed;

    /// <summary>Raises.</summary>
    protected void Raise() => Changed?.Invoke(this, System.EventArgs.Empty);
}

/// <inheritdoc/>
public class D : B
{
    /// <inheritdoc/>
    public D(string s) : base(s) { }

    /// <inheritdoc/>
    public override int M(int x) => x;

    /// <inheritdoc/>
    public override string P { get => ""; set { } }

    /// <inheritdoc/>
    public override event System.EventHandler? Changed;

    /// <summary>D's own summary.</summary>
    /// <inheritdoc cref="B.M(int)"/>
    public int OwnPlusCref(int x) => x;

    /// <summary>Only the param: <inheritdoc cref="B.M(int)" path="/param[@name='x']/node()"/></summary>
    public int PathNodes(int x) => x;

    /// <inheritdoc cref="B.M(int)" path="/remarks"/>
    public int PathRemarks() => 0;

    /// <summary>Own words.</summary>
    /// <inheritdoc cref="B.M(int)" path="/example"/>
    public int PathSelectsNothing() => 0;

    /// <summary><inheritdoc cref="B.M(int)"/></summary>
    /// <remarks>Own remarks.</remarks>
    public int NestedSummary() => 0;

    /// <inheritdoc cref="D.Cycle"/>
    public void Cycle() { }

    /// <inheritdoc cref="System.IDisposable.Dispose"/>
    public void CrossAssembly() { }

    /// <summary>D raises.</summary>
    protected void RaiseD() => Changed?.Invoke(this, System.EventArgs.Empty);
}

/// <inheritdoc/>
public class D2 : D
{
    /// <inheritdoc/>
    public D2(string s) : base(s) { }

    /// <inheritdoc/>
    public override int M(int x) => x;
}

/// <summary>The I.</summary>
public interface I
{
    /// <summary>I.N runs.</summary>
    void N();

    /// <summary>I.Q.</summary>
    int Q { get; }
}

/// <inheritdoc/>
public interface J : I
{
    /// <summary>J.O.</summary>
    void O();
}

/// <summary>Implicit.</summary>
public class Impl : J
{
    /// <inheritdoc/>
    public void N() { }

    /// <inheritdoc/>
    public int Q => 0;

    /// <inheritdoc/>
    public void O() { }
}

/// <summary>Explicit.</summary>
public class Expl : I
{
    /// <inheritdoc/>
    void I.N() { }

    /// <inheritdoc/>
    int I.Q => 0;
}

/// <summary>IA.</summary>
public interface IA
{
    /// <summary>IA.Z.</summary>
    void Z();
}

/// <summary>IB.</summary>
public interface IB
{
    /// <summary>IB.Z.</summary>
    void Z();
}

/// <summary>Both.</summary>
public class AB : IB, IA
{
    /// <inheritdoc/>
    public void Z() { }
}

/// <summary>A disposable.</summary>
public sealed class R : System.IDisposable
{
    /// <inheritdoc/>
    public void Dispose() { }

    /// <inheritdoc/>
    public override string ToString() => "";
}

/// <summary>A comparable.</summary>
public sealed class K : System.IComparable<K>
{
    /// <inheritdoc/>
    public int CompareTo(K? other) => 0;
}

/// <summary>Generic base.</summary>
/// <typeparam name="T">The T.</typeparam>
public class G<T>
{
    /// <summary>Gets a <typeparamref name="T"/>.</summary>
    /// <returns>The <typeparamref name="T"/>.</returns>
    public virtual T Get() => default!;

    /// <summary>Takes a <typeparamref name="T"/> and a <typeparamref name="U"/>.</summary>
    public virtual void Two<U>(T t, U u) { }
}

/// <inheritdoc/>
public class GInt : G<int>
{
    /// <inheritdoc/>
    public override int Get() => 0;

    /// <inheritdoc/>
    public override void Two<U>(int t, U u) { }
}

/// <inheritdoc/>
public class GSelf<V> : G<GSelf<V>>
{
    /// <inheritdoc/>
    public override GSelf<V> Get() => this;
}

/// <summary>Passes through.</summary>
public class GPass<W> : G<W>
{
    /// <inheritdoc/>
    public override W Get() => default!;
}

/// <summary>Overloads at an intermediate level.</summary>
public class O1 : B
{
    /// <inheritdoc/>
    public O1(string s) : base(s) { }

    /// <summary>O1.M's string overload, not what O2.M overrides.</summary>
    public virtual int M(string x) => 0;
}

/// <summary>Overrides past the overload.</summary>
public class O2 : O1
{
    /// <inheritdoc/>
    public O2(string s) : base(s) { }

    /// <inheritdoc/>
    public override int M(int x) => x;
}

/// <summary>Spellings: C# compares element names ignoring case.</summary>
public class Spell : B
{
    /// <InheritDoc/>
    public Spell(string s) : base(s) { }

    /// <INHERITDOC/>
    public override int M(int x) => x;

    /// <summary>Spell.P.</summary>
    public override string P { get => ""; set { } }

    /// <ınheritdoc/>
    public override event System.EventHandler? Changed;

    /// <summary>Spell raises.</summary>
    protected void RaiseSpell() => Changed?.Invoke(this, System.EventArgs.Empty);
}

/// <summary>Ref-kinds: an `in` overload between a `ref` method and its
/// override.</summary>
public class RB
{
    /// <summary>RB.M, by ref.</summary>
    public virtual void M(ref int x) { }
}

/// <summary>Adds the `in` overload.</summary>
public class RC : RB
{
    /// <summary>RC.M, by in: not what RD.M overrides.</summary>
    public virtual void M(in int x) { }
}

/// <summary>Overrides the `ref` one.</summary>
public class RD : RC
{
    /// <inheritdoc/>
    public override void M(ref int x) { }
}

/// <summary>A read-only ref return.</summary>
public interface IRet
{
    /// <summary>IRet.M, returning a read-only ref.</summary>
    ref readonly int M() => throw null!;
}

/// <summary>A writable ref return: not IRet.M's implementation.</summary>
public class Ret : IRet
{
    private int f;

    /// <inheritdoc/>
    public ref int M() => ref f;
}

/// <summary>Default interface members.</summary>
public interface IDim
{
    /// <summary>IDim.S, sealed: implemented by nothing.</summary>
    public sealed void S() { }

    /// <summary>IDim.P, private: implemented by nothing.</summary>
    private void P() { }

    /// <summary>IDim.V, a virtual default.</summary>
    void V() { }

    /// <summary>IDim.A, abstract.</summary>
    void A();
}

/// <summary>Members named like IDim's.</summary>
public class Dim : IDim
{
    /// <inheritdoc/>
    public void S() { }

    /// <inheritdoc/>
    public void P() { }

    /// <inheritdoc/>
    public void V() { }

    /// <inheritdoc/>
    public void A() { }
}

/// <summary>A struct.</summary>
public struct S
{
    /// <inheritdoc/>
    public override string ToString() => "";
}

/// <inheritdoc/>
public struct SNoCandidate { }
"#;

fn verdicts(compared: &[Compared]) -> std::collections::BTreeMap<&str, &Verdict> {
    compared
        .iter()
        .map(|c| (c.key.as_str(), &c.verdict))
        .collect()
}

#[test]
fn handwritten_cases_expand_exactly_as_roslyn() {
    let fx = fixture(HANDWRITTEN).unwrap_or_else(|e| panic!("fixture does not compile: {e:#?}"));
    let compared = compare(&fx);
    let mut census = Census::default();
    census.add(&compared);
    census.print("handwritten");
    census.assert_sound();
    let v = verdicts(&compared);
    // Every `cref` (and `path`) case.
    let must_expand = [
        "M:F.D.OwnPlusCref(System.Int32)",
        "M:F.D.PathNodes(System.Int32)",
        "M:F.D.PathRemarks",
        "M:F.D.NestedSummary",
        "M:F.D.CrossAssembly",
        // A spelling Roslyn does not take for `<inheritdoc>` (a dotless `ı`),
        // shown as it is.
        "E:F.Spell.Changed",
    ];
    // Every bare `<inheritdoc/>` Roslyn expands: implicit candidates, not
    // modelled, so each declines as one.
    let implicit = [
        "T:F.D",
        "M:F.D.#ctor(System.String)",
        "M:F.D.M(System.Int32)",
        "P:F.D.P",
        "E:F.D.Changed",
        "T:F.D2",
        "M:F.D2.#ctor(System.String)",
        "M:F.D2.M(System.Int32)",
        "T:F.J",
        "M:F.Impl.N",
        "P:F.Impl.Q",
        "M:F.Impl.O",
        "M:F.Expl.F#I#N",
        "P:F.Expl.F#I#Q",
        "M:F.AB.Z",
        "M:F.R.Dispose",
        "M:F.R.ToString",
        "M:F.K.CompareTo(F.K)",
        "T:F.GInt",
        "M:F.GInt.Get",
        "M:F.GInt.Two``1(System.Int32,``0)",
        "T:F.GSelf`1",
        "M:F.GSelf`1.Get",
        "M:F.GPass`1.Get",
        "M:F.S.ToString",
        "M:F.O2.M(System.Int32)",
        "M:F.Spell.#ctor(System.String)",
        "M:F.Spell.M(System.Int32)",
        "M:F.Dim.V",
        "M:F.Dim.A",
        "M:F.RD.M(System.Int32@)",
    ];
    let mut wrong = Vec::new();
    for key in must_expand {
        match v.get(key) {
            Some(Verdict::Agrees) => {}
            other => wrong.push(format!("{key}: {other:?}")),
        }
    }
    for key in implicit {
        match v.get(key) {
            Some(Verdict::Declined { cause, .. }) if cause == "ImplicitCandidate" => {}
            other => wrong.push(format!("{key} should decline as implicit: {other:?}")),
        }
    }
    // Roslyn removes an <inheritdoc> whose path selects nothing; hover keeps
    // it as the marker instead of dropping it silently.
    assert!(
        matches!(v.get("M:F.D.PathSelectsNothing"), Some(Verdict::Declined { cause, roslyn_left_it: false }) if cause == "NothingSelected"),
        "{:?}",
        v.get("M:F.D.PathSelectsNothing")
    );
    for key in [
        "M:F.D.Cycle",
        "T:F.SNoCandidate",
        "M:F.Dim.S",
        "M:F.Dim.P",
        "M:F.Ret.M",
    ] {
        if !matches!(v.get(key), Some(Verdict::Declined { .. })) {
            wrong.push(format!("{key} should decline: {:?}", v.get(key)));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// A `cref` to a member whose signature names a type from an assembly the
/// reference set lacks: Roslyn's ID binding cannot match an error type, so it
/// leaves the element; the string index here would still match, so the
/// expansion must decline.
#[test]
fn a_cref_whose_signature_needs_a_missing_assembly_declines() {
    let fx = fixture_missing(
        r#"
namespace F;

/// <summary>The base.</summary>
public class Base
{
    /// <summary>Makes a Base from an option.</summary>
    public Base(Dep.Opt o) { }

    /// <summary>Makes a plain Base.</summary>
    public Base() { }
}

/// <summary>A user.</summary>
public class User
{
    /// <inheritdoc cref="Base(Dep.Opt)"/>
    public void FromOption() { }

    /// <inheritdoc cref="Base()"/>
    public void Plain() { }
}
"#,
        Some("namespace Dep; public class Opt { }"),
    )
    .unwrap_or_else(|e| panic!("fixture does not compile: {e:#?}"));
    let compared = compare(&fx);
    let mut census = Census::default();
    census.add(&compared);
    census.print("missing dependency");
    census.assert_sound();
    let v = verdicts(&compared);
    assert!(
        matches!(v.get("M:F.User.FromOption"), Some(Verdict::Declined { cause, roslyn_left_it: true }) if cause.contains("UnboundSignature")),
        "{:?}",
        v.get("M:F.User.FromOption")
    );
    assert!(
        matches!(v.get("M:F.User.Plain"), Some(Verdict::Agrees)),
        "{:?}",
        v.get("M:F.User.Plain")
    );
}

/// A reference set without its core library: to Roslyn every primitive is an
/// error type, so a `cref` ID naming `System.Int32` binds nothing and a type
/// argument `int` has no documentation ID. Neither may be committed here.
#[test]
fn without_a_core_library_no_primitive_binds() {
    let fx = fixture_without_core_library(
        r#"
namespace F;

/// <summary>The base.</summary>
public class B
{
    /// <summary>B.M.</summary>
    public void M(int x) { }

    /// <summary>B.N.</summary>
    public void N() { }
}

/// <summary>A generic.</summary>
public class G<T>
{
    /// <summary>G.V of <typeparamref name="T"/>.</summary>
    public virtual void V() { }
}

/// <summary>A closed derivation.</summary>
public class GI : G<int>
{
    /// <inheritdoc/>
    public override void V() { }
}

/// <summary>A user.</summary>
public class U
{
    /// <inheritdoc cref="M:F.B.M(System.Int32)"/>
    public void A() { }

    /// <inheritdoc cref="M:F.B.N"/>
    public void C() { }
}
"#,
    )
    .unwrap_or_else(|e| panic!("fixture does not compile: {e:#?}"));
    let compared = compare(&fx);
    let mut census = Census::default();
    census.add(&compared);
    census.print("no core library");
    census.assert_sound();
    let v = verdicts(&compared);
    assert!(
        matches!(v.get("M:F.U.C"), Some(Verdict::Agrees)),
        "{:?}",
        v.get("M:F.U.C")
    );
    for key in ["M:F.U.A", "M:F.GI.V"] {
        assert!(
            matches!(v.get(key), Some(Verdict::Declined { .. })),
            "{key}: {:?}",
            v.get(key)
        );
    }
}

/// Roslyn's core library is the referenced assembly with no assembly
/// references of its own. An assembly that defines `System.Object` (and
/// `System.Int32`) while referencing another is not one: to Roslyn every
/// primitive is still an error type, so the `cref` naming `System.Int32` binds
/// nothing and must not be committed.
#[test]
fn a_system_object_definer_with_references_is_not_the_core_library() {
    let fx = fixture_without_core_library(
        r#"
namespace System
{
    /// <summary>A look-alike root type.</summary>
    public class Object { }

    /// <summary>A look-alike integer.</summary>
    public struct Int32 { }
}

namespace F
{
    /// <summary>The base.</summary>
    public class B
    {
        /// <summary>B.M.</summary>
        public void M(int x) { }

        /// <summary>B.N.</summary>
        public void N() { }
    }

    /// <summary>A user.</summary>
    public class U
    {
        /// <inheritdoc cref="M:F.B.M(System.Int32)"/>
        public void A() { }

        /// <inheritdoc cref="M:F.B.N"/>
        public void C() { }
    }
}
"#,
    )
    .unwrap_or_else(|e| panic!("fixture does not compile: {e:#?}"));
    let compared = compare(&fx);
    let mut census = Census::default();
    census.add(&compared);
    census.print("System.Object defined by an assembly with references");
    census.assert_sound();
    let v = verdicts(&compared);
    assert!(
        matches!(v.get("M:F.U.C"), Some(Verdict::Agrees)),
        "{:?}",
        v.get("M:F.U.C")
    );
    assert!(
        matches!(v.get("M:F.U.A"), Some(Verdict::Declined { .. })),
        "{:?}",
        v.get("M:F.U.A")
    );
}

/// Roslyn's documentation provider reads each `<member>` whole and then
/// advances one node, so a `<member>` immediately after another — no
/// whitespace between — is never indexed. Here `B`'s entry is invisible to
/// Roslyn, which expands `D`'s `<inheritdoc cref="B"/>` to nothing; inheriting `B`'s
/// summary would be a different answer.
#[test]
fn an_entry_adjacent_to_the_previous_one_is_read_differently() {
    let fx = fixture(
        r#"
namespace F;

/// <summary>A.</summary>
public class A { }

/// <summary>B.</summary>
public class B { }

/// <inheritdoc cref="B"/>
public class D : B { }
"#,
    )
    .unwrap_or_else(|e| panic!("fixture does not compile: {e:#?}"));
    let xml = fx.dll.with_extension("xml");
    let compact: String = std::fs::read_to_string(&xml)
        .unwrap()
        .lines()
        .map(str::trim)
        .collect();
    std::fs::write(&xml, compact).unwrap();
    let compared = compare(&fx);
    let mut census = Census::default();
    census.add(&compared);
    census.print("adjacent entries");
    census.assert_sound();
    let v = verdicts(&compared);
    assert!(
        matches!(v.get("T:F.D"), Some(Verdict::Declined { cause, .. }) if cause == "ReadDifferently"),
        "{:?}",
        v.get("T:F.D")
    );
}

/// The index reads the first `<doc>` of a `<span>`-wrapped file, but Roslyn's
/// documentation provider indexes every `<member>` in the document, last one
/// wins. With a second `<doc>` re-documenting `B`, Roslyn expands `D`'s
/// `<inheritdoc cref="B"/>` to the second summary; inheriting the first would be a
/// different answer.
#[test]
fn an_entry_in_a_second_doc_section_is_read_differently() {
    let fx = fixture(
        r#"
namespace F;

/// <summary>B.</summary>
public class B { }

/// <inheritdoc cref="B"/>
public class D : B { }
"#,
    )
    .unwrap_or_else(|e| panic!("fixture does not compile: {e:#?}"));
    let xml = fx.dll.with_extension("xml");
    let compiled = std::fs::read_to_string(&xml).unwrap();
    let body = compiled
        .trim_start()
        .strip_prefix("<?xml version=\"1.0\"?>")
        .expect("the compiler writes an XML declaration");
    let wrapped = format!(
        "<span>{body}<doc><assembly><name>Fx</name></assembly><members>\
         <member name=\"T:F.B\"><summary>The second B.</summary></member>\
         </members></doc></span>"
    );
    std::fs::write(&xml, wrapped).unwrap();
    let compared = compare(&fx);
    let mut census = Census::default();
    census.add(&compared);
    census.print("a second <doc> section");
    census.assert_sound();
    let v = verdicts(&compared);
    assert!(
        matches!(v.get("T:F.D"), Some(Verdict::Declined { cause, .. }) if cause == "ReadDifferently"),
        "{:?}",
        v.get("T:F.D")
    );
}

/// Roslyn binds an `AssemblyRef` to a referenced assembly by identity — name,
/// public-key token, version — not by simple name. Each row rewrites `Fx`'s
/// reference to `System.Runtime`, the one loaded DLL of that name, so that
/// some part of its identity no loaded DLL carries; every expansion must
/// still agree with Roslyn's, whichever way Roslyn binds the reference. The
/// `cref` to `Take` spells `System.Exception` through that reference: where
/// Roslyn leaves it unbound, the ID matches no symbol.
#[test]
fn a_reference_binds_by_identity_not_simple_name() {
    const SOURCE: &str = r#"
namespace F;

/// <summary>A user.</summary>
public class D
{
    /// <summary>Takes an exception.</summary>
    public void Take(System.Exception e) { }

    /// <inheritdoc cref="Take(System.Exception)"/>
    public void Cref() { }

    /// <inheritdoc cref="System.IDisposable.Dispose"/>
    public void Plain() { }
}
"#;
    let rows: &[(&str, AssemblyRefPatch)] = &[
        ("unchanged", AssemblyRefPatch::default()),
        (
            "another token",
            AssemblyRefPatch {
                token: Some("b03f5f7f11d50a3b"),
                ..Default::default()
            },
        ),
        (
            "no token",
            AssemblyRefPatch {
                drop_token: true,
                ..Default::default()
            },
        ),
        (
            "a higher version",
            AssemblyRefPatch {
                version: Some("11.0.0.0"),
                ..Default::default()
            },
        ),
        (
            "a lower version",
            AssemblyRefPatch {
                version: Some("4.0.0.0"),
                ..Default::default()
            },
        ),
        (
            "a higher revision",
            AssemblyRefPatch {
                version: Some("10.0.0.1"),
                ..Default::default()
            },
        ),
    ];
    // Every row's census is printed before any is asserted, so a failure
    // shows the whole table.
    let mut censuses = Vec::new();
    for (label, patch) in rows {
        let fx = fixture_patched(SOURCE, &|dll| {
            oracle()
                .lock()
                .unwrap()
                .patch_assembly_ref(dll, "System.Runtime", patch);
        })
        .unwrap_or_else(|e| panic!("fixture does not compile: {e:#?}"));
        let compared = compare(&fx);
        let mut census = Census::default();
        census.add(&compared);
        census.print(&format!("System.Runtime referenced with {label}"));
        censuses.push(census);
    }
    for census in &censuses {
        census.assert_sound();
    }
    assert_eq!(
        censuses[0].agrees, 2,
        "the unchanged reference must expand every entry"
    );
}

/// The identity check holds through a type forwarder too. `Real` is
/// strong-named and defines `F.T`; `Fwd` forwards `F.T` to it; `Fx` was
/// compiled against a `Fwd` that still defined `F.T`, so its reference goes
/// through the forwarder. Each row rewrites `Fwd`'s reference to `Real`, and
/// every expansion must still agree with Roslyn's.
#[test]
fn a_forwarder_binds_its_target_by_identity() {
    const T: &str = r#"
namespace F;

/// <summary>The real T.</summary>
public class T
{
    /// <summary>T.M.</summary>
    public virtual void M() { }
}
"#;
    let dir = TempDir::new().unwrap();
    let runtime = ensure_system_runtime_dll();
    let key = runtime.with_file_name("netstandard.dll");
    let mut oracle = oracle().lock().unwrap();
    let real = oracle
        .compile_signed(
            T,
            "Real",
            &dir.path().join("real"),
            std::slice::from_ref(&runtime),
            &key,
        )
        .unwrap_or_else(|e| panic!("Real does not compile: {e:#?}"));
    let fwd_defining = oracle
        .compile(
            T,
            "Fwd",
            &dir.path().join("fwd-defining"),
            std::slice::from_ref(&runtime),
        )
        .unwrap_or_else(|e| panic!("the defining Fwd does not compile: {e:#?}"));
    let fwd = oracle
        .compile(
            "[assembly: System.Runtime.CompilerServices.TypeForwardedTo(typeof(F.T))]",
            "Fwd",
            &dir.path().join("fwd"),
            &[runtime.clone(), real.clone()],
        )
        .unwrap_or_else(|e| panic!("the forwarding Fwd does not compile: {e:#?}"));
    let fx = oracle
        .compile(
            r#"
namespace G;

/// <summary>A user.</summary>
public class D
{
    /// <summary>Takes a T.</summary>
    public void Take(F.T t) { }

    /// <inheritdoc cref="Take(F.T)"/>
    public void Cref() { }
}
"#,
            "Fx",
            &dir.path().join("fx"),
            &[runtime.clone(), fwd_defining],
        )
        .unwrap_or_else(|e| panic!("Fx does not compile: {e:#?}"));
    let rows: &[(&str, AssemblyRefPatch)] = &[
        ("unchanged", AssemblyRefPatch::default()),
        (
            "another token",
            AssemblyRefPatch {
                token: Some("cc7b13ffcd2ddd52"),
                ..Default::default()
            },
        ),
        (
            "no token",
            AssemblyRefPatch {
                drop_token: true,
                ..Default::default()
            },
        ),
        (
            "a higher version",
            AssemblyRefPatch {
                version: Some("2.0.0.0"),
                ..Default::default()
            },
        ),
    ];
    let mut censuses = Vec::new();
    for (i, (label, patch)) in rows.iter().enumerate() {
        let patched = dir.path().join(format!("fwd{i}")).join("Fwd.dll");
        std::fs::create_dir_all(patched.parent().unwrap()).unwrap();
        std::fs::copy(&fwd, &patched).unwrap();
        oracle.patch_assembly_ref(&patched, "Real", patch);
        let references = vec![runtime.clone(), patched, real.clone(), fx.clone()];
        let cache = AssemblyCache::at(dir.path().join(format!("cache{i}")));
        let (env, _) = build_env_from_dll_paths(references.iter().map(PathBuf::as_path), &cache);
        // `compare` drives the oracle itself.
        drop(oracle);
        let compared =
            compare_assembly(&Arc::new(env), &mut DocSources::default(), &references, &fx);
        oracle = crate::common::inheritdoc_oracle::oracle().lock().unwrap();
        let mut census = Census::default();
        census.add(&compared);
        census.print(&format!("a forwarder to Real with {label}"));
        censuses.push(census);
    }
    drop(oracle);
    for census in &censuses {
        census.assert_sound();
    }
    assert_eq!(
        censuses[0].agrees, 1,
        "the unchanged forwarder must expand the entry"
    );
}

/// A property's or event's accessor is a method with a documentation ID of
/// its own (`M:F.Mod.get_x`), which the model folds into its owner. Roslyn
/// binds a `cref` naming that ID to the accessor like any other method, so
/// where another assembly's `F.Mod` declares an ordinary method of the
/// accessor's name, the `cref` has two targets: which one Roslyn's "first
/// symbol" picks depends on reference order, and inheriting the ordinary
/// method's documentation is not provably its answer. Each row is one way an
/// accessor's ID is spelled; in both orders the entry must agree with Roslyn,
/// and the accessor guard must fire — so that a row whose order happens to
/// make Roslyn pick the ordinary method still checks the ID was recognised.
#[test]
fn a_cref_naming_an_accessor_id_is_ambiguous_with_a_method_of_that_name() {
    // (shape, the accessor-bearing type, the ordinary-method type, the cref)
    let rows: &[(&str, &str, &str, &str)] = &[
        (
            "a getter",
            "public class Mod { /// <summary>A's x.</summary>\n public int x => 0; }",
            "public class Mod { /// <summary>B's get_x.</summary>\n public int get_x() => 0; }",
            "M:F.Mod.get_x",
        ),
        (
            "a getter of a generic type",
            "public class Mod<T> { /// <summary>A's x.</summary>\n public int x => 0; }",
            "public class Mod<T> { /// <summary>B's get_x.</summary>\n public int get_x() => 0; }",
            "M:F.Mod`1.get_x",
        ),
        (
            "a getter of a type nested in a generic one",
            "public class Outer<T> { /// <summary>A's Mod.</summary>\n public class Mod { /// <summary>A's x.</summary>\n public int x => 0; } }",
            "public class Outer<T> { /// <summary>B's Mod.</summary>\n public class Mod { /// <summary>B's get_x.</summary>\n public int get_x() => 0; } }",
            "M:F.Outer`1.Mod.get_x",
        ),
        (
            "an indexer's getter",
            "public class Mod { /// <summary>A's indexer.</summary>\n public int this[int i] => 0; }",
            "public class Mod { /// <summary>B's get_Item.</summary>\n public int get_Item(int i) => 0; }",
            "M:F.Mod.get_Item(System.Int32)",
        ),
        (
            "a setter",
            "public class Mod { /// <summary>A's x.</summary>\n public int x { set { } } }",
            "public class Mod { /// <summary>B's set_x.</summary>\n public void set_x(int value) { } }",
            "M:F.Mod.set_x(System.Int32)",
        ),
        (
            "an event's adder",
            "public class Mod { /// <summary>A's E.</summary>\n public event System.EventHandler? E; }",
            "public class Mod { /// <summary>B's add_E.</summary>\n public void add_E(System.EventHandler h) { } }",
            "M:F.Mod.add_E(System.EventHandler)",
        ),
    ];
    let mut censuses = Vec::new();
    let mut guarded = Vec::new();
    for (shape, accessor, method, cref) in rows {
        let accessor = format!("namespace F;\n/// <summary>A's.</summary>\n{accessor}");
        let method = format!("namespace F;\n/// <summary>B's.</summary>\n{method}");
        let fx_source = format!(
            "namespace G;\n/// <summary>C.</summary>\npublic class C\n{{\n    /// <inheritdoc cref=\"{cref}\"/>\n    public void Inherits() {{ }}\n}}\n"
        );
        let dep = |name, source| Dep {
            name,
            source,
            alias: None,
            referenced: true,
            against: &[],
            visible: false,
            patch: None,
        };
        let orders = [
            (
                "accessor first",
                [dep("DepA", &accessor), dep("DepB", &method)],
            ),
            (
                "method first",
                [dep("DepB", &method), dep("DepA", &accessor)],
            ),
        ];
        for (order, deps) in orders {
            let fx = fixture_with(&fx_source, &deps)
                .unwrap_or_else(|e| panic!("{shape}: fixture does not compile: {e:#?}"));
            let compared = compare(&fx);
            let mut census = Census::default();
            census.add(&compared);
            census.print(&format!("{shape}, {order}"));
            censuses.push(census);
            let verdict = verdicts(&compared)
                .get("M:G.C.Inherits")
                .map(|v| (*v).clone());
            guarded.push((
                format!("{shape}, {order}"),
                matches!(&verdict, Some(Verdict::Declined { cause, .. }) if cause == "Cref(MaybeAccessor)"),
                verdict,
            ));
        }
    }
    for census in &censuses {
        census.assert_sound();
    }
    for (row, fired, verdict) in &guarded {
        assert!(fired, "{row}: the accessor guard did not fire: {verdict:?}");
    }
}

/// [`DocIdIndex::may_name_an_accessor`] over System.Runtime's real
/// `System.Exception`: an accessor's ID is one, an ordinary method's is not —
/// until the projection loses the accessors' names, when no method ID of the
/// type can be told apart from an accessor's.
#[test]
fn accessor_ids_are_known_or_every_method_id_of_the_type_is_suspect() {
    use borzoi::xml_doc::key::DocIdIndex;
    use borzoi_assembly::{EcmaView, Member};

    let bytes = std::fs::read(ensure_system_runtime_dll()).unwrap();
    let view = borzoi_assembly::Ecma335Assembly::parse(&bytes).unwrap();
    let types = view.enumerate_type_defs().unwrap();
    let system = |name: &str, arity: usize| {
        types
            .iter()
            .find(|e| {
                e.namespace == ["System"] && e.name == name && e.generic_parameters.len() == arity
            })
            .unwrap_or_else(|| panic!("System.{name}`{arity}"))
            .clone()
    };
    let exception = system("Exception", 0);
    // A generic declaring type spells its arity inside the method ID's head.
    let nullable = DocIdIndex::of_env(&AssemblyEnv::from_entities(vec![system("Nullable", 1)]));
    assert!(nullable.may_name_an_accessor("M:System.Nullable`1.get_Value"));
    assert!(nullable.may_name_an_accessor("M:System.Nullable`1.get_HasValue"));
    assert!(!nullable.may_name_an_accessor("M:System.Nullable`1.GetValueOrDefault"));
    assert!(!nullable.may_name_an_accessor("M:System.Nullable`1.GetValueOrDefault(`0)"));
    let named = DocIdIndex::of_env(&AssemblyEnv::from_entities(vec![exception.clone()]));
    assert!(named.may_name_an_accessor("M:System.Exception.get_Message"));
    assert!(named.may_name_an_accessor("M:System.Exception.add_SerializeObjectState(System.EventHandler{System.Runtime.Serialization.SafeSerializationEventArgs})"));
    assert!(!named.may_name_an_accessor("M:System.Exception.GetBaseException"));
    assert!(!named.may_name_an_accessor("T:System.Exception"));
    assert!(!named.may_name_an_accessor("P:System.Exception.Message"));

    let mut blanked = exception;
    for member in &mut blanked.members {
        if let Member::Property(p) = member {
            for slot in &mut p.accessor_slots {
                slot.name.clear();
            }
        }
    }
    let unnamed = DocIdIndex::of_env(&AssemblyEnv::from_entities(vec![blanked]));
    assert!(unnamed.may_name_an_accessor("M:System.Exception.GetBaseException"));
    assert!(!unnamed.may_name_an_accessor("M:System.ArgumentException.GetBaseException"));
}

/// A referenced DLL the env could not read at all may declare a symbol of
/// the `cref`'s ID too — Roslyn then binds whichever its reference order
/// puts first — so over an incomplete assembly set no `cref` commits.
#[test]
fn a_cref_over_an_incomplete_assembly_set_declines() {
    let fx = fixture(
        r#"
namespace F;

/// <summary>The base.</summary>
public class B { }

/// <summary>A user.</summary>
public class U
{
    /// <inheritdoc cref="B"/>
    public void Cref() { }
}
"#,
    )
    .unwrap_or_else(|e| panic!("fixture does not compile: {e:#?}"));
    let v = compare(&fx);
    assert!(
        matches!(verdicts(&v).get("M:F.U.Cref"), Some(Verdict::Agrees)),
        "{:?}",
        verdicts(&v).get("M:F.U.Cref")
    );
    let mut env = (*fx.env).clone();
    env.mark_referenced_assemblies_incomplete();
    let incomplete = Fixture {
        env: Arc::new(env),
        ..fx
    };
    let v = compare(&incomplete);
    assert!(
        matches!(verdicts(&v).get("M:F.U.Cref"), Some(Verdict::Declined { cause, .. }) if cause == "Cref(IncompleteAssemblySet)"),
        "{:?}",
        verdicts(&v).get("M:F.U.Cref")
    );
}

/// The `cref`'s ID names a method of `F.B` in two assemblies; the one in
/// the first is vararg, which the projection drops. Roslyn binds the first,
/// so committing to the surviving one would inherit the wrong entry.
#[test]
fn a_dropped_member_of_a_same_named_type_elsewhere_declines_the_cref() {
    let fx = fixture_with(
        r#"
namespace G;

/// <summary>A user.</summary>
public class U
{
    /// <inheritdoc cref="M:F.B.M"/>
    public void Cref() { }
}
"#,
        &[
            Dep {
                name: "A",
                source: "namespace F {\n/// <summary>A's B.</summary>\npublic class B {\n/// <summary>A's vararg M.</summary>\npublic void M(__arglist) { } } }",
                alias: None,
                referenced: true,
                against: &[],
                visible: false,
                patch: None,
            },
            Dep {
                name: "Bee",
                source: "namespace F {\n/// <summary>Bee's B.</summary>\npublic class B {\n/// <summary>Bee's M.</summary>\npublic void M() { } } }",
                alias: None,
                referenced: true,
                against: &[],
                visible: false,
                patch: None,
            },
        ],
    )
    .unwrap_or_else(|e| panic!("fixture does not compile: {e:#?}"));
    let compared = compare(&fx);
    let mut census = Census::default();
    census.add(&compared);
    census.print("a dropped member of a same-named type");
    census.assert_sound();
    let v = verdicts(&compared);
    assert!(
        matches!(v.get("M:G.U.Cref"), Some(Verdict::Declined { cause, .. }) if cause == "Cref(DroppedNearby)"),
        "{:?}",
        v.get("M:G.U.Cref")
    );
}

/// Two assemblies define `F.C`, one implementing `static int I.P` explicitly
/// (private, non-virtual accessors, imported through their `MethodImpl`
/// rows) and one `int I.P`: both carry `P:F.C.F#I#P`, so the `cref` is
/// ambiguous to Roslyn, which binds the first.
#[test]
fn a_static_explicit_property_implementation_is_imported() {
    let iface = "namespace F {\n/// <summary>I.</summary>\npublic interface I {\n/// <summary>I.P.</summary>\nstatic abstract int P { get; } } }";
    let instance_iface = "namespace F {\n/// <summary>I.</summary>\npublic interface I {\n/// <summary>I.P.</summary>\nint P { get; } } }";
    let fx = fixture_with(
        r#"
namespace G;

/// <summary>A user.</summary>
public class U
{
    /// <inheritdoc cref="P:F.C.F#I#P"/>
    public void Cref() { }
}
"#,
        &[
            Dep {
                name: "IfcS",
                source: iface,
                alias: None,
                referenced: true,
                against: &[],
                visible: false,
                patch: None,
            },
            Dep {
                name: "StaticImpl",
                source: "namespace F {\n/// <summary>Static C.</summary>\npublic class C : I {\n/// <summary>The static P.</summary>\nstatic int I.P => 0; } }",
                alias: None,
                referenced: true,
                against: &[0],
                visible: false,
                patch: None,
            },
            Dep {
                name: "IfcI",
                source: instance_iface,
                alias: None,
                referenced: false,
                against: &[],
                visible: false,
                patch: None,
            },
            Dep {
                name: "InstanceImpl",
                source: "namespace F {\n/// <summary>Instance C.</summary>\npublic class C : I {\n/// <summary>The instance P.</summary>\nint I.P => 0; } }",
                alias: None,
                referenced: true,
                against: &[2],
                visible: false,
                patch: None,
            },
        ],
    )
    .unwrap_or_else(|e| panic!("fixture does not compile: {e:#?}"));
    let compared = compare(&fx);
    let mut census = Census::default();
    census.add(&compared);
    census.print("static explicit property implementation");
    census.assert_sound();
    assert!(
        matches!(
            verdicts(&compared).get("M:G.U.Cref"),
            Some(Verdict::Declined { .. })
        ),
        "{:?}",
        verdicts(&compared).get("M:G.U.Cref")
    );
}

/// An internal member Roslyn imports only through `InternalsVisibleTo`
/// (not modelled) shares the `cref`'s ID with a public one: which Roslyn
/// binds is not knowable here.
#[test]
fn an_internal_member_of_the_ids_name_declines_the_cref() {
    let fx = fixture_with(
        r#"
namespace G;

/// <summary>A user.</summary>
public class U
{
    /// <inheritdoc cref="M:F.V.X"/>
    public void Cref() { }
}
"#,
        &[
            Dep {
                name: "Friend",
                source: "[assembly: System.Runtime.CompilerServices.InternalsVisibleTo(\"InheritdocOracleConsumer\")]\nnamespace F {\n/// <summary>Friend V.</summary>\npublic class V {\n/// <summary>The internal X.</summary>\ninternal void X() { } } }",
                alias: None,
                referenced: true,
                against: &[],
                visible: false,
                patch: None,
            },
            Dep {
                name: "Public",
                source: "namespace F {\n/// <summary>Public V.</summary>\npublic class V {\n/// <summary>The public X.</summary>\npublic void X() { } } }",
                alias: None,
                referenced: true,
                against: &[],
                visible: false,
                patch: None,
            },
        ],
    )
    .unwrap_or_else(|e| panic!("fixture does not compile: {e:#?}"));
    let compared = compare(&fx);
    let mut census = Census::default();
    census.add(&compared);
    census.print("an internal member of the ID's name");
    census.assert_sound();
    assert!(
        matches!(verdicts(&compared).get("M:G.U.Cref"), Some(Verdict::Declined { cause, .. }) if cause == "Cref(MaybeImportedByFriendship)"),
        "{:?}",
        verdicts(&compared).get("M:G.U.Cref")
    );
}

/// A type projected in F#'s source view shares the `cref` target's parent ID:
/// its metadata members (a module value's getter, say) are not all in the
/// index, so the target found may not be the only one. The ID lies under a
/// namespace FSharp.Core occupies, which declines it before the parent is
/// looked at.
#[test]
fn a_source_view_type_of_the_parents_id_declines_the_cref() {
    let fsharp_core = crate::common::ensure_fsharp_core_dll();
    let dir = TempDir::new().unwrap();
    let runtime = ensure_system_runtime_dll();
    let dll = oracle()
        .lock()
        .unwrap()
        .compile(
            r#"
namespace Microsoft.FSharp.Core
{
    /// <summary>A C# Operators.</summary>
    public static class Operators
    {
        /// <summary>A C# method of the F# module's name.</summary>
        public static void CSharpOnly() { }
    }
}

namespace G
{
    /// <summary>A user.</summary>
    public class U
    {
        /// <inheritdoc cref="M:Microsoft.FSharp.Core.Operators.CSharpOnly"/>
        public void Cref() { }
    }
}
"#,
            "Fx",
            dir.path(),
            std::slice::from_ref(&runtime),
        )
        .unwrap_or_else(|e| panic!("fixture does not compile: {e:#?}"));
    let references = vec![runtime, fsharp_core, dll.clone()];
    let fx = fixture_from(dir, dll, references);
    let compared = compare(&fx);
    let mut census = Census::default();
    census.add(&compared);
    census.print("a source-view type of the parent's ID");
    census.assert_sound();
    assert!(
        matches!(verdicts(&compared).get("M:G.U.Cref"), Some(Verdict::Declined { cause, .. }) if cause == "Cref(FSharpNamespace)"),
        "{:?}",
        verdicts(&compared).get("M:G.U.Cref")
    );
}

/// Compare `fx`, assert it sound, and return the verdict for `key`.
fn sound_verdict(fx: &Fixture, label: &str, key: &str) -> Verdict {
    let compared = compare(fx);
    let mut census = Census::default();
    census.add(&compared);
    census.print(label);
    census.assert_sound();
    verdicts(&compared)
        .get(key)
        .map(|v| (*v).clone())
        .unwrap_or_else(|| panic!("{label}: no entry {key}"))
}

/// A dependency, referenced but not compiled against, built against the
/// earlier ones at `against`.
fn referenced_dep<'a>(name: &'a str, source: &'a str, against: &'a [usize]) -> Dep<'a> {
    Dep {
        name,
        source,
        alias: None,
        referenced: true,
        against,
        visible: false,
        patch: None,
    }
}

/// A nested `F.Outer.Inner` the projection drops is recorded under its
/// top-level encloser's namespace `F`, while a top-level `Inner` in namespace
/// `F.Outer` carries the same ID. Roslyn binds the first assembly's, the
/// dropped one, so no `cref` commits while any type was dropped.
#[test]
fn a_dropped_nested_type_of_the_crefs_id_declines_it() {
    let fx = fixture_with(
        r#"
namespace G;

/// <summary>A user.</summary>
public class U
{
    /// <inheritdoc cref="T:F.Outer.Inner"/>
    public void Cref() { }
}
"#,
        &[
            referenced_dep(
                "Nests",
                "namespace F {\n/// <summary>Outer.</summary>\npublic class Outer {\n/// <summary>The nested Inner.</summary>\n[System.Reflection.DefaultMember(null)]\npublic class Inner { } } }",
                &[],
            ),
            referenced_dep(
                "Tops",
                "namespace F.Outer {\n/// <summary>The top-level Inner.</summary>\npublic class Inner { } }",
                &[],
            ),
        ],
    )
    .unwrap_or_else(|e| panic!("fixture does not compile: {e:#?}"));
    let v = sound_verdict(&fx, "a dropped nested type", "M:G.U.Cref");
    assert!(
        matches!(&v, Verdict::Declined { cause, .. } if cause == "Cref(DroppedNearby)"),
        "{v:?}"
    );
}

/// Roslyn's declaration-ID lookup does not bind the ID of an explicit
/// implementation of a constructed generic interface member
/// (`M:F.A.F#I{System#Int32}#M(System.Int32)`), though it is the ID the
/// compiler writes for it: it leaves the `<inheritdoc>` as it is.
#[test]
fn a_generic_explicit_implementation_id_declines() {
    let fx = fixture(
        r#"
namespace F;

/// <summary>I.</summary>
public interface I<T>
{
    /// <summary>I.M.</summary>
    void M(T t);
    /// <summary>I.P.</summary>
    int P { get; }
}

/// <summary>A.</summary>
public class A : I<int>
{
    /// <summary>A's explicit M.</summary>
    void I<int>.M(int t) { }
    /// <summary>A's explicit P.</summary>
    int I<int>.P => 0;
}

/// <summary>U.</summary>
public class U
{
    /// <inheritdoc cref="M:F.A.F#I{System#Int32}#M(System.Int32)"/>
    public void CrefM() { }
    /// <inheritdoc cref="P:F.A.F#I{System#Int32}#P"/>
    public int CrefP => 0;
}
"#,
    )
    .unwrap_or_else(|e| panic!("fixture does not compile: {e:#?}"));
    for key in ["M:F.U.CrefM", "P:F.U.CrefP"] {
        let v = sound_verdict(&fx, "a generic explicit implementation", key);
        assert!(matches!(v, Verdict::Declined { .. }), "{key}: {v:?}");
    }
}

/// `Fake` and `Real` both define `F.X`, and `UserLib`'s `A.M` takes the one
/// in `Real`. Its metadata reference binds, but Roslyn's declaration-ID
/// lookup resolves `F.X` in `M:UserLib.A.M(F.X)` over the whole compilation,
/// where it is ambiguous, and binds nothing.
#[test]
fn a_signature_type_two_assemblies_define_declines_the_cref() {
    let fx = fixture_with(
        r#"
namespace G;

/// <summary>A user.</summary>
public class U
{
    /// <inheritdoc cref="M:UserLib.A.M(F.X)"/>
    public void Cref() { }
}
"#,
        &[
            referenced_dep("Fake", "namespace F { public class X { } }", &[]),
            referenced_dep("Real", "namespace F { public class X { } }", &[]),
            referenced_dep(
                "UserLib",
                "namespace UserLib {\n/// <summary>A.</summary>\npublic class A {\n/// <summary>A.M.</summary>\npublic void M(F.X x) { } } }",
                &[1],
            ),
        ],
    )
    .unwrap_or_else(|e| panic!("fixture does not compile: {e:#?}"));
    let v = sound_verdict(&fx, "an ambiguous signature type", "M:G.U.Cref");
    assert!(matches!(v, Verdict::Declined { .. }), "{v:?}");
}

/// A namespaced `<inheritdoc>`, or an `<inheritdoc>` with a namespaced
/// attribute, puts the compiled doc file outside the grammar
/// `xml_doc::file` certifies: every `<inheritdoc>` of it declines as read
/// differently, whatever Roslyn makes of it.
#[test]
fn namespaced_spellings_decline_the_whole_file() {
    let fx = fixture(
        r#"
namespace F;

/// <summary>B.</summary>
public class B
{
    /// <summary>B.M.</summary>
    public virtual int M(int x) => x;
    /// <summary>B.P.</summary>
    public virtual string P { get => ""; set { } }
}

/// <summary>D.</summary>
public class D : B
{
    /// <inheritdoc cref="B.M(int)" xml:lang="en"/>
    public int LangCref() => 0;

    /// <inheritdoc xmlns="urn:x"/>
    public override string P { get => ""; set { } }

    /// <inheritdoc/>
    public override int M(int x) => x;
}
"#,
    )
    .unwrap_or_else(|e| panic!("fixture does not compile: {e:#?}"));
    for key in ["M:F.D.LangCref", "M:F.D.M(System.Int32)"] {
        let v = sound_verdict(&fx, "namespaced spellings", key);
        assert!(
            matches!(&v, Verdict::Declined { cause, .. } if cause == "ReadDifferently"),
            "{key}: {v:?}"
        );
    }
}

/// Roslyn reads an entry by writing it out and re-reading it, which turns a
/// carriage-return reference in text into a line feed: an inherited entry
/// holding one is outside the certified grammar, and the expansion
/// declines rather than show the carriage return.
#[test]
fn an_inherited_carriage_return_reference_declines() {
    let fx = fixture(
        r#"
namespace F;

/// <summary>B.</summary>
public class B
{
    /// <summary><code>A&#xD;B</code></summary>
    public void M() { }
}

/// <summary>U.</summary>
public class U
{
    /// <inheritdoc cref="M:F.B.M"/>
    public void Cref() { }
}
"#,
    )
    .unwrap_or_else(|e| panic!("fixture does not compile: {e:#?}"));
    let v = sound_verdict(&fx, "a carriage-return reference", "M:F.U.Cref");
    assert!(
        matches!(&v, Verdict::Declined { cause, .. } if cause == "ReadDifferently"),
        "{v:?}"
    );
}

/// Two DLLs of one identity, `Dupe` 1.0.0.0, the first defining `F.C0` and
/// the second `F.C1`: the env keeps both, while Roslyn's compilation keeps
/// one (the second), so no `cref` commits while two loaded DLLs share a
/// simple name.
#[test]
fn a_cref_beside_two_dlls_of_one_name_declines() {
    let fx = fixture_with(
        r#"
namespace G;

/// <summary>A user.</summary>
public class U
{
    /// <inheritdoc cref="T:F.C0"/>
    public void Cref() { }
}
"#,
        &[
            referenced_dep(
                "Dupe",
                "namespace F {\n/// <summary>C0.</summary>\npublic class C0 { } }",
                &[],
            ),
            referenced_dep(
                "Dupe",
                "namespace F {\n/// <summary>C1.</summary>\npublic class C1 { } }",
                &[],
            ),
        ],
    )
    .unwrap_or_else(|e| panic!("fixture does not compile: {e:#?}"));
    let v = sound_verdict(&fx, "two DLLs of one name", "M:G.U.Cref");
    assert!(
        matches!(&v, Verdict::Declined { cause, .. } if cause == "Cref(DuplicateAssemblyName)"),
        "{v:?}"
    );
}

/// Two assemblies define `F.C`: the first with a function-pointer property
/// `P`, which the projector drops, the second with a documented method
/// `get_P`. Roslyn binds `M:F.C.get_P` to the first's accessor, which the
/// dropped member's name does not spell, so any dropped member of a type of
/// the parent's ID declines a member `cref`.
#[test]
fn a_dropped_property_declines_a_cref_to_its_accessors_id() {
    let fx = fixture_with(
        r#"
namespace G;

/// <summary>A user.</summary>
public class U
{
    /// <inheritdoc cref="M:F.C.get_P"/>
    public void Cref() { }
}
"#,
        &[
            referenced_dep(
                "Ptrs",
                "namespace F {\n/// <summary>Ptrs' C.</summary>\npublic unsafe class C {\n/// <summary>P.</summary>\npublic delegate*<void> P => null; } }",
                &[],
            ),
            referenced_dep(
                "Plain",
                "namespace F {\n/// <summary>Plain's C.</summary>\npublic class C {\n/// <summary>Plain's get_P.</summary>\npublic int get_P() => 0; } }",
                &[],
            ),
        ],
    )
    .unwrap_or_else(|e| panic!("fixture does not compile: {e:#?}"));
    let v = sound_verdict(&fx, "a dropped property's accessor", "M:G.U.Cref");
    assert!(
        matches!(&v, Verdict::Declined { cause, .. } if cause == "Cref(DroppedNearby)"),
        "{v:?}"
    );
}

/// FSharp.Core's union `FSharpOption<T>` has a nested `Tags` class in
/// metadata that its F# projection does not show, and a C# assembly defines
/// `Microsoft.FSharp.Core.FSharpOption<T>.Tags` too: Roslyn's lookup of the
/// ID meets both. Under a namespace an F#-projected assembly occupies, the
/// projection may hide any metadata type, so no `cref` there commits.
#[test]
fn a_cref_under_an_fsharp_assemblys_namespace_declines() {
    use crate::common::ensure_fsharp_core_dll;
    let dir = TempDir::new().unwrap();
    let runtime = ensure_system_runtime_dll();
    let fsharp_core = ensure_fsharp_core_dll();
    let mut oracle = oracle().lock().unwrap();
    let decoy = oracle
        .compile(
            "namespace Microsoft.FSharp.Core {\n/// <summary>The C# option.</summary>\npublic class FSharpOption<T> {\n/// <summary>The C# tags.</summary>\npublic class Tags { } } }",
            "Decoy",
            &dir.path().join("decoy"),
            std::slice::from_ref(&runtime),
        )
        .unwrap_or_else(|e| panic!("{e:#?}"));
    let dll = oracle
        .compile(
            "namespace G;\n/// <summary>A user.</summary>\npublic class U {\n/// <inheritdoc cref=\"T:Microsoft.FSharp.Core.FSharpOption`1.Tags\"/>\npublic void Cref() { } }",
            "Fx",
            dir.path(),
            std::slice::from_ref(&runtime),
        )
        .unwrap_or_else(|e| panic!("{e:#?}"));
    drop(oracle);
    let fx = fixture_from(dir, dll.clone(), vec![runtime, fsharp_core, decoy, dll]);
    let v = sound_verdict(&fx, "an F# assembly's namespace", "M:G.U.Cref");
    assert!(
        matches!(&v, Verdict::Declined { cause, .. } if cause == "Cref(FSharpNamespace)"),
        "{v:?}"
    );
}

/// Each of many `<inheritdoc>`s inheriting a large summary is small as a
/// node count, but not as text: the expansion bounds the bytes it produces
/// too, and declines past them.
#[test]
fn an_expansion_past_the_byte_budget_declines() {
    use borzoi::xml_doc::inherit::MAX_BYTES;
    let summary = "x".repeat(MAX_BYTES / 16);
    let crefs = "<inheritdoc cref=\"M:F.B.M\" path=\"/summary/node()\"/>".repeat(20);
    let fx = fixture(&format!(
        "namespace F;\n/// <summary>B.</summary>\npublic class B {{\n/// <summary>{summary}</summary>\npublic void M() {{ }} }}\n\
         /// <summary>U.</summary>\npublic class U {{\n/// <summary>{crefs}</summary>\npublic void Cref() {{ }} }}"
    ))
    .unwrap_or_else(|e| panic!("fixture does not compile: {e:#?}"));
    let v = sound_verdict(&fx, "the byte budget", "M:F.U.Cref");
    assert!(
        matches!(&v, Verdict::Declined { cause, .. } if cause == "TooLarge"),
        "{v:?}"
    );
}

/// An enum's `value__` backing field is no member Roslyn imports, even
/// where a doc file documents it: a `cref` to it binds nothing.
#[test]
fn a_cref_to_an_enum_backing_field_declines() {
    let fx = fixture(
        r#"
namespace F;

/// <summary>E.</summary>
public enum E
{
    /// <summary>A.</summary>
    A,
}

/// <summary>U.</summary>
public class U
{
    /// <inheritdoc cref="F:F.E.value__"/>
    public void Cref() { }
}
"#,
    )
    .unwrap_or_else(|e| panic!("fixture does not compile: {e:#?}"));
    let xml = fx.dll.with_extension("xml");
    let text = std::fs::read_to_string(&xml).unwrap();
    let text = text.replacen(
        "<members>",
        "<members>\n        <member name=\"F:F.E.value__\"><summary>The backing field.</summary></member>",
        1,
    );
    std::fs::write(&xml, text).unwrap();
    let v = sound_verdict(&fx, "an enum backing field", "M:F.U.Cref");
    assert!(matches!(v, Verdict::Declined { .. }), "{v:?}");
}
