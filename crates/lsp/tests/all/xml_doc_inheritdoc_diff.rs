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
use crate::common::inheritdoc_oracle::oracle;

/// `dynamic` needs `DynamicAttribute`, which the reference pack keeps in
/// `System.Linq.Expressions`; a fixture over `System.Runtime` alone declares
/// its own, as the compiler accepts.
pub const DYNAMIC_ATTRIBUTE: &str = r#"
namespace System.Runtime.CompilerServices
{
    /// <summary>The compiler's marker for `dynamic`.</summary>
    public sealed class DynamicAttribute : System.Attribute
    {
        /// <summary>A whole-type `dynamic`.</summary>
        public DynamicAttribute() { }

        /// <summary>A `dynamic` at the flagged positions.</summary>
        public DynamicAttribute(bool[] transformFlags) { }
    }
}
"#;

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
}

/// [`fixture`], with `Fx` compiled against dependency assemblies built from
/// `deps` (each against System.Runtime alone).
pub fn fixture_with(source: &str, deps: &[Dep<'_>]) -> Result<Fixture, Vec<String>> {
    let dir = TempDir::new().unwrap();
    let runtime = ensure_system_runtime_dll();
    let mut compile_against: Vec<(PathBuf, Option<&str>)> = vec![(runtime.clone(), None)];
    let mut references = vec![runtime.clone()];
    let mut built: Vec<PathBuf> = Vec::new();
    for (i, dep) in deps.iter().enumerate() {
        let dep_dir = dir.path().join(format!("dep{i}"));
        let mut against = vec![runtime.clone()];
        against.extend(dep.against.iter().map(|&j| built[j].clone()));
        let dll = oracle()
            .lock()
            .unwrap()
            .compile(dep.source, dep.name, &dep_dir, &against)?;
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
    references.push(dll.clone());
    // The runtime's on-disk projection cache, in a directory of this test
    // binary's own: the reference pack is projected once, not per fixture.
    static CACHE: std::sync::OnceLock<(TempDir, AssemblyCache)> = std::sync::OnceLock::new();
    let (_, cache) = CACHE.get_or_init(|| {
        let dir = TempDir::new().unwrap();
        let cache = AssemblyCache::at(dir.path().to_path_buf());
        (dir, cache)
    });
    let (env, _) = build_env_from_dll_paths(references.iter().map(PathBuf::as_path), cache);
    Ok(Fixture {
        _dir: dir,
        dll,
        references,
        env: Arc::new(env),
    })
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

/// <summary>Spellings: C# compares element names ignoring case, and a
/// namespaced element is another name.</summary>
public class Spell : B
{
    /// <InheritDoc/>
    public Spell(string s) : base(s) { }

    /// <INHERITDOC/>
    public override int M(int x) => x;

    /// <inheritdoc xmlns="urn:x"/>
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
    let must_expand = [
        "T:F.D",
        "M:F.D.#ctor(System.String)",
        "M:F.D.M(System.Int32)",
        "P:F.D.P",
        "E:F.D.Changed",
        "M:F.D.OwnPlusCref(System.Int32)",
        "M:F.D.PathNodes(System.Int32)",
        "M:F.D.PathRemarks",
        "M:F.D.NestedSummary",
        "M:F.D.CrossAssembly",
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
        "P:F.Spell.P",
        "E:F.Spell.Changed",
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

/// Two assemblies defining the same full type name: a signature's types are
/// compared by the definition they bind to, as Roslyn compares them, not by
/// name — `Mid.M(B::N.T)` is an overload, not what `Derived.M(A::N.T)`
/// overrides.
#[test]
fn same_named_types_from_two_assemblies_are_different_types() {
    let t = "namespace N; public class T { }";
    let fx = fixture_with(
        r#"
extern alias A;
extern alias B;

namespace F;

/// <summary>The base.</summary>
public class Base
{
    /// <summary>Base.M, taking A's T.</summary>
    public virtual void M(A::N.T t) { }
}

/// <summary>The middle.</summary>
public class Mid : Base
{
    /// <summary>Mid.M, taking B's T: an overload.</summary>
    public virtual void M(B::N.T t) { }
}

/// <summary>The derived.</summary>
public class Derived : Mid
{
    /// <inheritdoc/>
    public override void M(A::N.T t) { }
}
"#,
        &[
            Dep {
                name: "DepA",
                source: t,
                alias: Some("A"),
                referenced: true,
                against: &[],
                visible: true,
            },
            Dep {
                name: "DepB",
                source: t,
                alias: Some("B"),
                referenced: true,
                against: &[],
                visible: true,
            },
        ],
    )
    .unwrap_or_else(|e| panic!("fixture does not compile: {e:#?}"));
    let compared = compare(&fx);
    let mut census = Census::default();
    census.add(&compared);
    census.print("same-named types");
    census.assert_sound();
    let v = verdicts(&compared);
    assert!(
        matches!(v.get("M:F.Derived.M(N.T)"), Some(Verdict::Agrees)),
        "{:?}",
        v.get("M:F.Derived.M(N.T)")
    );
}

/// Roslyn's base-constructor rule compares parameter types with the default
/// symbol comparer, which tells `dynamic` from `object` and one tuple's
/// element names from another's — distinctions metadata carries only in
/// attributes. `D(dynamic)` has no base constructor to Roslyn, so the
/// `B(object)` beside it must not be inherited from.
#[test]
fn a_base_constructor_differing_by_an_attribute_carried_distinction_is_not_inherited() {
    let fx = fixture(
        &[
            DYNAMIC_ATTRIBUTE,
            r#"
namespace F {

/// <summary>The base.</summary>
public class B
{
    /// <summary>B from an object.</summary>
    public B(object x) { }

    /// <summary>B from an unnamed tuple.</summary>
    public B((int, string) t, int y) { }

    /// <summary>B from an IntPtr.</summary>
    public B(System.IntPtr p, string s) { }

    /// <summary>B from objects.</summary>
    public B(object[] xs, int y) { }

    /// <summary>B from an int.</summary>
    public B(int i) { }
}

/// <summary>The derived.</summary>
public class D : B
{
    /// <inheritdoc/>
    public D(dynamic x) : base((object)x) { }

    /// <inheritdoc/>
    public D((int a, string b) t, int y) : base(t, y) { }

    /// <inheritdoc/>
    public D(nint p, string s) : base(p, s) { }

    /// <inheritdoc/>
    public D(dynamic[] xs, int y) : base((object[])xs, y) { }

    /// <inheritdoc/>
    public D(int i) : base(i) { }
}
}
"#,
        ]
        .concat(),
    )
    .unwrap_or_else(|e| panic!("fixture does not compile: {e:#?}"));
    let compared = compare(&fx);
    let mut census = Census::default();
    census.add(&compared);
    census.print("attribute-carried distinctions");
    census.assert_sound();
    let v = verdicts(&compared);
    for key in [
        "M:F.D.#ctor(System.Object)",
        "M:F.D.#ctor(System.ValueTuple{System.Int32,System.String},System.Int32)",
        "M:F.D.#ctor(System.Object[],System.Int32)",
    ] {
        assert!(
            matches!(v.get(key), Some(Verdict::Declined { cause, .. }) if cause == "Undecidable(AttributeCarriedDistinction)"),
            "{key}: {:?}",
            v.get(key)
        );
    }
    assert!(
        matches!(v.get("M:F.D.#ctor(System.Int32)"), Some(Verdict::Agrees)),
        "{:?}",
        v.get("M:F.D.#ctor(System.Int32)")
    );
}

/// Roslyn finds a `<typeparamref>`'s parameter innermost first — a method's
/// own, then its type's, then the enclosing types' — and takes the first of
/// the name. A method `M<T>` of `G<T>` reached as `G<int>.M` refers to its
/// own `T`, which stays a reference; so does a nested `In<T>` of `O<T>`.
#[test]
fn a_shadowing_type_parameter_is_the_innermost() {
    let fx = fixture(
        r#"
namespace F;

/// <summary>A generic base.</summary>
public class G<T>
{
    /// <summary>M of <typeparamref name="T"/>.</summary>
    public virtual void M<T>(T x) { }

    /// <summary>Q of <typeparamref name="T"/> and <typeparamref name="U"/>.</summary>
    public virtual void Q<U>(T x, U u) { }
}

/// <summary>A closed derivation.</summary>
public class GI : G<int>
{
    /// <inheritdoc/>
    public override void M<T>(T x) { }

    /// <inheritdoc/>
    public override void Q<U>(int x, U u) { }
}

/// <summary>An outer generic.</summary>
public class O<T>
{
    /// <summary>An inner generic, shadowing <typeparamref name="T"/>.</summary>
    public class In<T>
    {
        /// <summary>M of <typeparamref name="T"/>.</summary>
        public virtual void M() { }
    }
}

/// <summary>A closed nested derivation.</summary>
public class OI : O<int>.In<string>
{
    /// <inheritdoc/>
    public override void M() { }
}
"#,
    )
    .unwrap_or_else(|e| panic!("fixture does not compile: {e:#?}"));
    let compared = compare(&fx);
    let mut census = Census::default();
    census.add(&compared);
    census.print("shadowing type parameters");
    census.assert_sound();
    let v = verdicts(&compared);
    for key in [
        "M:F.GI.M``1(``0)",
        "M:F.GI.Q``1(System.Int32,``0)",
        "M:F.OI.M",
    ] {
        assert!(
            matches!(v.get(key), Some(Verdict::Agrees)),
            "{key}: {:?}",
            v.get(key)
        );
    }
}

/// Two references to a type that the loaded assembly of their name lacks,
/// made against different versions of it: Roslyn binds both into the loaded
/// assembly, where they name one missing type, so `Derived.M` overrides
/// `Mid.M`. Comparing the references' versions instead would step past `Mid`
/// to `Base0`.
#[test]
fn references_to_a_missing_type_of_a_loaded_assembly_are_one_type() {
    let dep = |version: &str, body: &str| {
        format!(
            "[assembly: System.Reflection.AssemblyVersion(\"{version}\")]\nnamespace N {{ {body} }}"
        )
    };
    let dep_1 = dep("1.0.0.0", "public class T { }");
    let dep_2 = dep("2.0.0.0", "public class T { }");
    let dep_3 = dep("3.0.0.0", "public class Other { }");
    let fx = fixture_with(
        r#"
namespace F;

/// <summary>The derived.</summary>
public class Derived : Hi.Mid
{
    /// <inheritdoc/>
    public override N.T M() => null!;
}
"#,
        &[
            Dep {
                name: "Dep",
                source: &dep_1,
                alias: None,
                referenced: false,
                against: &[],
                visible: true,
            },
            Dep {
                name: "Dep",
                source: &dep_2,
                alias: None,
                referenced: false,
                against: &[],
                visible: false,
            },
            Dep {
                name: "Dep",
                source: &dep_3,
                alias: None,
                referenced: true,
                against: &[],
                visible: false,
            },
            Dep {
                name: "Lo",
                source: "namespace Lo {\n/// <summary>The root.</summary>\npublic class Base0 {\n/// <summary>Base0.M.</summary>\npublic virtual N.T M() => null!; } }",
                alias: None,
                referenced: true,
                against: &[0],
                visible: true,
            },
            Dep {
                name: "Hi",
                source: "namespace Hi {\n/// <summary>The middle.</summary>\npublic class Mid : Lo.Base0 {\n/// <summary>Mid.M.</summary>\npublic new virtual N.T M() => null!; } }",
                alias: None,
                referenced: true,
                against: &[1, 3],
                visible: true,
            },
        ],
    )
    .unwrap_or_else(|e| panic!("fixture does not compile: {e:#?}"));
    let compared = compare(&fx);
    let mut census = Census::default();
    census.add(&compared);
    census.print("missing type of a loaded assembly");
    census.assert_sound();
    let v = verdicts(&compared);
    assert!(
        matches!(v.get("M:F.Derived.M"), Some(Verdict::Declined { cause, .. }) if cause == "Undecidable(TypeIdentity)"),
        "{:?}",
        v.get("M:F.Derived.M")
    );
}
