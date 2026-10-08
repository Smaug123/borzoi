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
    let dir = TempDir::new().unwrap();
    let runtime = ensure_system_runtime_dll();
    let mut compile_against = vec![runtime.clone()];
    if let Some(dependency) = dependency {
        let dep_dir = dir.path().join("dep");
        compile_against.push(oracle().lock().unwrap().compile(
            dependency,
            "Dep",
            &dep_dir,
            std::slice::from_ref(&runtime),
        )?);
    }
    let dll = oracle()
        .lock()
        .unwrap()
        .compile(source, "Fx", dir.path(), &compile_against)?;
    let references = vec![runtime, dll.clone()];
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
    ];
    let mut wrong = Vec::new();
    for key in must_expand {
        match v.get(key) {
            Some(Verdict::Agrees) => {}
            other => wrong.push(format!("{key}: {other:?}")),
        }
    }
    for key in ["M:F.D.Cycle", "T:F.SNoCandidate"] {
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
