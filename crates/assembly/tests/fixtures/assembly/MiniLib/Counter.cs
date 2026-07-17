namespace MiniLib;

// Phase 3a's "methods only" surface. Each member exercises a different
// branch of the Rust and fcs-dump projectors that the diff test pins:
//
//   - explicit parameterless ctor and ctor-with-arg          → .ctor pair,
//                                                              ParameterMetadata
//                                                              name lookup
//   - void instance method                                   → return-type ≡ void
//   - non-void instance method                               → return-type
//                                                              projection
//   - static method                                          → !instance flag
//   - virtual method                                         → ECMA-335
//                                                              virtual+newslot
//   - override of System.Object.ToString                     → virtual+override
//                                                              (no newslot)
//   - method with an `out` parameter                         → byref + is_out
//                                                              metadata
//   - protected ctor                                         → ECMA-335 Family
//                                                              on a .ctor; FCS's
//                                                              public-API
//                                                              `Accessibility`
//                                                              collapses this to
//                                                              `Public` (see
//                                                              `IsProtectedAccessibility`
//                                                              in `infos.fs`,
//                                                              which excludes
//                                                              constructors).
//                                                              fcs-dump must
//                                                              read the raw
//                                                              MethodAttributes
//                                                              to agree with
//                                                              the Rust
//                                                              side.
//   - protected-internal method                              → ECMA-335
//                                                              FamORAssem; FCS's
//                                                              4-way `Accessibility`
//                                                              has no slot for
//                                                              this and collapses
//                                                              it to `Protected`.
//                                                              fcs-dump must
//                                                              read the raw
//                                                              MethodAttributes
//                                                              to surface
//                                                              `ProtectedOrInternal`.
//
// Phase 3b adds fields. Each one exercises a different branch:
//
//   - public instance field                                  → baseline,
//                                                              instance flag
//   - public static field                                    → static flag
//   - public readonly instance field                         → init_only flag
//   - public const field                                     → ECMA-335
//                                                              static + literal
//                                                              + init_only. Both
//                                                              projectors must
//                                                              surface it as a
//                                                              regular static
//                                                              field; the diff
//                                                              normaliser
//                                                              doesn't carry
//                                                              literal-ness yet.
//   - protected field                                        → Family access.
//                                                              FCS's
//                                                              `FSharpField.Accessibility`
//                                                              routes through
//                                                              `RecdField.Accessibility`
//                                                              for IL-imported
//                                                              fields, so the
//                                                              4-way collapse
//                                                              that bites
//                                                              methods doesn't
//                                                              bite fields the
//                                                              same way — but
//                                                              this pins it.
//   - protected internal field                               → FamORAssem.
//                                                              Same as the
//                                                              method case: if
//                                                              FCS collapses
//                                                              to Protected,
//                                                              the diff will
//                                                              surface it and
//                                                              we'll know.
//   - private field                                          → dropped by
//                                                              the
//                                                              `AccessibleFromSomeFSharpCode`
//                                                              filter on both
//                                                              sides; verifies
//                                                              the filter is
//                                                              symmetric for
//                                                              fields too.
//
// Phase 3c adds properties. Each one exercises a different branch:
//
//   - auto-property with public get + public set         → baseline
//                                                          property +
//                                                          synthesised
//                                                          backing field.
//                                                          Both projectors
//                                                          must skip the
//                                                          `<X>k__BackingField`
//                                                          (private, so the
//                                                          accessibility
//                                                          filter drops it
//                                                          symmetrically)
//                                                          and only emit
//                                                          the property +
//                                                          two accessor
//                                                          methods. The Rust side
//                                                          actually pulls
//                                                          the accessor
//                                                          MethodDefs *into*
//                                                          the Property
//                                                          struct via
//                                                          MethodSemantics,
//                                                          so the
//                                                          method-enumeration
//                                                          path will NOT see
//                                                          them. fcs-dump
//                                                          filters them out
//                                                          of
//                                                          MembersFunctionsAndValues
//                                                          via
//                                                          IsPropertyGetterMethod /
//                                                          IsPropertySetterMethod.
//   - get-only auto-property                             → has_getter, no
//                                                          has_setter
//   - expression-bodied property (get-only)              → another get-only
//                                                          shape, no backing
//                                                          field at all
//   - static property                                    → CallingConv
//                                                          difference; both
//                                                          projectors derive
//                                                          static-ness from
//                                                          the property's
//                                                          own bit (Rust:
//                                                          PropertySig.has_this;
//                                                          fcs-dump:
//                                                          ILThisConvention)
//                                                          rather than from
//                                                          accessor flags.
//   - protected property                                 → ECMA-335
//                                                          per-accessor
//                                                          access; the
//                                                          property visibility
//                                                          is the union of
//                                                          getter+setter. FCS
//                                                          hard-codes
//                                                          ILProp accessor
//                                                          accessibility to
//                                                          Public, so the
//                                                          public API would
//                                                          mis-render this —
//                                                          fcs-dump uses the
//                                                          raw IL walk for
//                                                          properties too.
//   - protected internal property                        → FamORAssem on the
//                                                          accessor MethodDefs.
//                                                          Same FCS gap as
//                                                          methods/fields.
//   - private property                                   → filtered by
//                                                          AccessibleFromSomeFSharpCode
//                                                          on both sides.
//   - property with public get + protected internal set  → asymmetric
//                                                          accessors. The
//                                                          property surface
//                                                          collapses to the
//                                                          least-restrictive
//                                                          (Public);
//                                                          the Rust side's
//                                                          `max_access`
//                                                          documents the
//                                                          same convention.
//
// Phase 3d adds events. Each one exercises a different branch:
//
//   - field-like event                                   → C# synthesises a
//                                                          private backing
//                                                          field of the
//                                                          delegate type plus
//                                                          a public add/remove
//                                                          pair. Both
//                                                          projectors must
//                                                          emit only the Event
//                                                          (the backing field
//                                                          is private and the
//                                                          AccessibleFromSomeFSharpCode
//                                                          filter drops it
//                                                          symmetrically). The
//                                                          add/remove
//                                                          MethodDefs likewise
//                                                          live inside the
//                                                          Event struct on
//                                                          the Rust side
//                                                          via MethodSemantics
//                                                          and are filtered
//                                                          from
//                                                          MembersFunctionsAndValues
//                                                          on the fcs-dump
//                                                          side via
//                                                          IsEventAddMethod /
//                                                          IsEventRemoveMethod.
//   - custom-accessor event with generic delegate        → no backing field;
//                                                          delegate type is
//                                                          a closed generic
//                                                          instantiation
//                                                          (System.EventHandler<int>),
//                                                          which pins the
//                                                          existing TypeRef
//                                                          generic-args path
//                                                          through the event
//                                                          surface too.
//   - static event                                       → ECMA-335
//                                                          MethodAttributes.Static
//                                                          on both accessors;
//                                                          both projectors
//                                                          derive the event's
//                                                          static-ness from
//                                                          the add accessor's
//                                                          bit (there is no
//                                                          top-level event
//                                                          flag, unlike
//                                                          PropertySig's
//                                                          has_this).
//   - protected event                                    → ECMA-335 Family on
//                                                          both accessors;
//                                                          FCS hard-codes
//                                                          ILEvent accessor
//                                                          accessibility to
//                                                          Public, so the
//                                                          public API would
//                                                          mis-render this —
//                                                          fcs-dump uses the
//                                                          raw IL walk for
//                                                          events too.
//   - protected internal event                           → FamORAssem on the
//                                                          accessors. Same
//                                                          FCS gap as
//                                                          methods/fields/
//                                                          properties.
//   - private event                                      → filtered by
//                                                          AccessibleFromSomeFSharpCode
//                                                          on both sides.
public class Counter
{
    public Counter()
    {
    }

    public Counter(int start)
    {
        // Force-use the parameter so the compiler doesn't warn under
        // TreatWarningsAsErrors when this fixture is folded into a larger
        // build context.
        _ = start;
    }

    // The ECMA-335 row for this ctor has Access=Family. FCS's
    // `getApproxFSharpAccessibilityOfMember` maps Family to taccessPublic
    // and then `IsProtectedAccessibility` excludes constructors, so the
    // `FSharpAccessibility` surface reports `IsPublic = true`. Without the
    // attribute-aware path on the fcs-dump side, the diff would record
    // `Public` here while the Rust side correctly emits `Protected`.
    protected Counter(string label)
    {
        _ = label;
    }

    public void Increment()
    {
    }

    public int Get() => 0;

    public static Counter Zero() => new Counter();

    public virtual int Combine(int other) => other;

    public override string ToString() => "Counter";

    public bool TryGet(out int value)
    {
        value = 0;
        return true;
    }

    // Phase 4c pins `[System.ParamArrayAttribute]`. The C# compiler emits
    // it on the final `params T[]` parameter; the parameter's IL type is
    // unchanged (still `int[]`), so without an attribute-aware projection
    // the Rust side would emit a plain `int[] values` while fcs-dump
    // — which surfaces `FSharpParameter.IsParamArrayArg` — would render
    // `params int[] values`, and the diff would fail. Pins both sides on
    // the attribute path.
    public int Sum(params int[] values) => values.Length;

    // The ECMA-335 row has Access=FamORAssem (`protected internal` in C#).
    // FCS's 4-way `FSharpAccessibility` has no slot for FamORAssem —
    // `getApproxFSharpAccessibilityOfMember` maps it to taccessPublic and
    // `IsProtectedAccessibility` returns true, collapsing the surface to
    // `IsProtected`. The Rust side reads the raw bit and emits
    // `ProtectedOrInternal`; matching that on the fcs-dump side requires
    // an attribute-aware projection.
    protected internal int InternalCombine(int x) => x;

    // Regression: FCS's `MembersFunctionsAndValues` applies
    // `AccessibleFromSomeFSharpCode` and drops private/internal members,
    // while the Rust-side importer surfaces every MethodDef. The
    // normaliser (`src/test_support.rs`, behind the `test-support`
    // feature) mirrors FCS's
    // filter so the diff stays aligned. Without that filter this method
    // would be a Rust-only ghost and the diff would fail loudly.
    private int Hidden() => 42;

    // Field shapes. The naming is deliberately mundane (`tag`, `Count`,
    // …) so the diff failure output reads naturally; the *interesting*
    // bits are the access/static/init_only/literal flag combinations.
    public int Count;
    public static int Total;
    public readonly string Tag = "tag";
    public const int MaxValue = 42;
    protected int ProtectedField;
    protected internal int ProtectedInternalField;

    // Verifies that `AccessibleFromSomeFSharpCode` drops private fields
    // symmetrically with private methods. Without the filter, this would
    // appear on the Rust side and not on the fcs-dump side.
    // `#pragma 169` keeps the "field never used" warning quiet under a
    // future TreatWarningsAsErrors build.
#pragma warning disable 169
    private int hiddenField;
#pragma warning restore 169

    // Property shapes. See the header block above for what each one pins.
    // The names are mundane on purpose so the diff output reads naturally.
    public int Value { get; set; }
    public string Name { get; } = "default";
    public int Doubled => Value * 2;
    public static int Created { get; set; }
    protected int ProtectedProp { get; set; }
    protected internal int InternalProp { get; set; }
    private int HiddenProp { get; set; }
    public int Asymmetric { get; protected internal set; }

    // Event shapes. The names are mundane on purpose; the interesting bits
    // are the field-like-vs-custom-accessor split, the static flag, and
    // the accessibility lattice. C# warns "event never used" on a field-like
    // event with no raises in this assembly; suppress so a future
    // TreatWarningsAsErrors build doesn't break the fixture.
#pragma warning disable 67
    public event System.EventHandler Tick;
    public event System.EventHandler<int> CustomTick
    {
        add { _ = value; }
        remove { _ = value; }
    }
    public static event System.EventHandler Reset;
    protected event System.EventHandler ProtectedTick;
    protected internal event System.EventHandler InternalTick;
    private event System.EventHandler HiddenTick;
#pragma warning restore 67
}

// Phase 3e adds generic parameters + constraints. Each entity below pins
// one or more axes of the new model surface:
//
//   - Box<T>                            → bare generic class. Pins:
//                                          - typar declaration with no
//                                            constraints
//                                          - field of typar type (`!T0`)
//                                          - method returning typar type
//                                          - constructor taking typar
//                                          - method-signature renderer
//                                            resolving typar references
//                                            by name on both sides
//   - Container<T> where T : class, new() → special constraints. Pins:
//                                          - `class` + `new()` constraint
//                                            string normalisation
//                                          - constraint set sorted as a
//                                            BTreeSet on the Rust side
//                                            so order-agnostic
//   - Sorted<T> where T : IComparable<T>  → typar referencing itself
//                                            inside a constraint. Pins:
//                                            - constraint rendering
//                                              recursing through a typar
//                                              reference
//                                            - the `!T0` index space
//                                              extending into constraint
//                                              types
//   - IProducer<out T>                   → covariant typar. Pins
//                                          `out T` declaration string.
//   - IConsumer<in T>                    → contravariant typar. Pins
//                                          `in T` declaration string.
//   - Picker                              → non-generic class with a
//                                            generic method `T Pick<T>(T)`.
//                                            Pins:
//                                            - method-typar declaration
//                                              with no constraints
//                                            - `!!M0` (vs `!T0`)
//                                              discrimination in the
//                                              IL renderer's TypeVar
//                                              index space
//                                            - CallingConvention::Generic
//                                              byte on the method
//   - PairMap<TKey, TValue> where TKey : System.IComparable<TKey>
//                                          → multi-typar declaration plus
//                                            a per-typar type constraint
//                                            that references the typar
//                                            itself. Pins:
//                                            - declaration order
//                                              preserved positionally
//                                            - constraints applied to
//                                              the correct typar by index
//   - Shadow<T>.Outer(T) + Shadow<T>.Inner<T>(T)
//                                          → legal IL where a method typar
//                                            shadows a same-named outer
//                                            type typar. The signature of
//                                            `Outer` references the type
//                                            typar (must render `!T0`),
//                                            and the signature of `Inner`
//                                            references the method typar
//                                            (must render `!!M0`). Pins:
//                                            - the FCS renderer resolves
//                                              typar references by IDENTITY
//                                              (`typarRefEq`) rather than
//                                              by name — otherwise the
//                                              method scope would mask the
//                                              outer reference.

public class Box<T>
{
    public T Item;

    public Box(T item)
    {
        Item = item;
    }

    public T Get() => Item;
}

public class Container<T> where T : class, new()
{
    public T Make() => new T();
}

public class Sorted<T> where T : System.IComparable<T>
{
    public T Pick(T a, T b) => a.CompareTo(b) < 0 ? a : b;
}

public interface IProducer<out T>
{
    T Produce();
}

public interface IConsumer<in T>
{
    void Consume(T value);
}

public class Picker
{
    public T Pick<T>(T x) => x;
}

public class PairMap<TKey, TValue> where TKey : System.IComparable<TKey>
{
    public TKey Key;
    public TValue Value;

    public PairMap(TKey key, TValue value)
    {
        Key = key;
        Value = value;
    }
}

// CS0693: "Type parameter 'T' has the same name as the type parameter from
// outer type 'Shadow<T>'". Legal IL — pins the renderer's identity-based
// typar resolution.
#pragma warning disable CS0693
public class Shadow<T>
{
    public T Outer(T x) => x;
    public T Inner<T>(T x) => x;
}
#pragma warning restore CS0693

// Phase 4b adds C# extension methods. The C# compiler emits
// `[System.Runtime.CompilerServices.ExtensionAttribute]` on every static
// method whose first parameter is `this T`, plus the enclosing static
// class. On the FCS side these surface as plain static methods (FCS
// only sets `IsExtensionMember` for F#-native `type ... with member`
// augmentations), so without the attribute-aware path the Rust
// side would emit `extension` as a method flag and fcs-dump would not —
// the diff would fail. Both projectors must read the attribute and
// agree.
public static class CounterExtensions
{
    public static int DoubledValue(this Counter c) => c.Value * 2;
}

// Phase 4d pins the modern-struct flavour markers. C# emits
// `[System.Runtime.CompilerServices.IsReadOnlyAttribute]` on a
// `readonly struct` and `[...IsByRefLikeAttribute]` on a `ref struct`,
// referenced via TypeRef on net10.0. The absolute Rust-side `true` values
// for this cross-assembly `Reference` arm are pinned in
// `tests/all/projector_markers.rs`; the same-assembly `Definition` arm has no
// net10.0 fixture that reaches it and is currently uncovered. FCS's `FSharpEntity.IsByRefLike`
// surfaces the ref-struct bit directly; the readonly bit is read off
// `FSharpEntity.Attributes` because FCS has no typed property for it.
public readonly struct ReadOnlyPoint
{
    public readonly int X;
    public readonly int Y;
    public ReadOnlyPoint(int x, int y) { X = x; Y = y; }
}

// Phase 4o note: alongside the byref-like marker, Roslyn emits a
// type-level `[System.Runtime.CompilerServices.CompilerFeatureRequired("RefStructs")]`
// on every `ref struct` (the gate that makes a pre-C#-11 compiler refuse
// the type, paired with the synthetic `[Obsolete(error)]` decoded below).
// So `RefSpan` / `ReadOnlyRefSpan` double as the entity-level fixtures for
// the `CompilerFeatureRequired` decoder — both projectors surface
// `[compiler-feature-required: RefStructs]` on the type. The separate C# 13
// `where T : allows ref struct` typar anti-constraint is a distinct signal:
// its `HasAllowsRefStruct` (`AllowByRefLike` `0x0020`) bit is decoded and
// rendered as the `allows ref struct` token; see `RefStructBox` /
// `RefStructHost` below. (This toolchain emits no `RefStructs` gate for the
// anti-constraint itself, only the bit.)
public ref struct RefSpan
{
    public int Length;
}

// `readonly ref struct` carries both markers. Surface order in the
// rendered kind matches C# 11 syntax: `readonly ref Struct`.
public readonly ref struct ReadOnlyRefSpan
{
    public readonly int Length;
    public ReadOnlyRefSpan(int length) { Length = length; }
}

// Phase 4e pins `[System.ObsoleteAttribute]` payload decoding. Each
// fixture below picks exactly one constructor overload so the three C#-
// expressible payload combinations are all exercised end-to-end through
// the diff oracle:
//   - `[Obsolete]`                    → message=None,         is_error=false
//   - `[Obsolete("msg")]`             → message=Some("msg"),  is_error=false
//   - `[Obsolete("msg", true)]`       → message=Some("msg"),  is_error=true
// (The named-arg-only case `IsError = true` isn't expressible in C# —
// `IsError` is a get-only property — so it's pinned via a unit test
// against a hand-built CA blob in `crates/assembly/src/reader/attributes_tests.rs`.) Declarations
// don't *use* any IsError=true symbol, so this stays inside
// `[Obsolete(error: true)]`'s usage-site-only enforcement — the fixture
// compiles even with the escalations present.
[System.Obsolete]
public class ObsoleteBare
{
    public int Get() => 0;
}

[System.Obsolete("use Counter instead")]
public class ObsoleteWithMessage
{
    public int Get() => 0;
}

[System.Obsolete("removed in v3", true)]
public class ObsoleteHardError
{
    public int Get() => 0;
}

// Obsolete on a method, not the type — pins `MethodLike::obsolete` end-to-end.
public class ObsoleteHost
{
    [System.Obsolete("use NewWay")]
    public int OldWay() => 0;

    public int NewWay() => 1;
}

// Obsolete with a >127-byte message. ECMA-335 II.23.2 encodes the
// length prefix as a compressed integer (1 byte ≤ 0x7F, 2 bytes ≤
// 0x3FFF, 4 bytes otherwise). The owned `Ecma335Assembly` reader's
// `SerString` decoder reads the multi-byte prefix correctly, so the
// full message and the warning/error flag are surfaced; FCS decodes
// it verbatim too. See `tryFormatObsolete` in
// `tools/fcs-dump/Program.fs` for the matching F#-side rule and
// `decodes_long_string_attribute_in_corpus` in
// `crates/assembly/src/reader/attributes_tests.rs` for the Rust-side
// proof. This fixture is exactly 160 ASCII bytes, comfortably above
// the 0x7F single-byte boundary and well inside the 2-byte arm — it
// pins faithful end-to-end decoding of a 2-byte-prefixed string.
[System.Obsolete("xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx")]
public class ObsoleteLongMessage
{
    public int Get() => 0;
}

// Phase 4e codex follow-up: short `Message`, but a long ignored named
// property (`DiagnosticId`). A long string sitting in a named arg the
// model discards (Obsolete keeps only the message + `IsError`) must not
// derail the CA-blob parse: the owned reader decodes every string in
// `ConstructorArguments` / `NamedArguments` faithfully, then the model
// drops `DiagnosticId` by design, so the short message survives and both
// sides surface `[obsolete: short]`. fcs-dump's `tryFormatObsolete`
// likewise decodes the long DiagnosticId verbatim and ignores it. This
// fixture pins the agreement end-to-end. The DiagnosticId payload is 160
// ASCII bytes — same construction as the long-message fixture above.
[System.Obsolete("short", DiagnosticId = "yyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyy")]
public class ObsoleteShortMessageLongDiagnosticId
{
    public int Get() => 0;
}

// Phase 4g pins `[System.Diagnostics.CodeAnalysis.ExperimentalAttribute]`
// payload decoding (the .NET 8+ "this API may change without notice"
// marker). The fixtures mirror the Obsolete pattern above: one fixture
// per legal payload shape, plus a long-string degradation pin.
//
//   - `[Experimental("DIAG001")]`                                 → diagnostic_id=Some, url=None, message=None
//   - `[Experimental("DIAG001", UrlFormat = "...{0}")]`           → diagnostic_id=Some, url=Some
//   - `[Experimental("DIAG001", Message = "...")]`               → diagnostic_id=Some, message=Some
//   - `[Experimental("DIAG001", UrlFormat = "...", Message = "...")]`
//                                                                → all three populated
// (The named-arg-overrides-positional case is pinned in a unit test
// against a hand-built CA blob in `crates/assembly/src/reader/attributes_tests.rs`; C# rejects
// `DiagnosticId` as a duplicate write at the source level.)
[System.Diagnostics.CodeAnalysis.Experimental("DIAG001")]
public class ExperimentalBare
{
    public int Get() => 0;
}

[System.Diagnostics.CodeAnalysis.Experimental("DIAG002", UrlFormat = "https://example.com/{0}")]
public class ExperimentalWithUrl
{
    public int Get() => 0;
}

[System.Diagnostics.CodeAnalysis.Experimental("DIAG003", Message = "subject to change")]
public class ExperimentalWithMessage
{
    public int Get() => 0;
}

[System.Diagnostics.CodeAnalysis.Experimental(
    "DIAG004",
    UrlFormat = "https://example.com/{0}",
    Message = "subject to change")]
public class ExperimentalWithUrlAndMessage
{
    public int Get() => 0;
}

// Experimental on a method, not the type — pins `MethodLike::experimental`
// end-to-end (sibling of `ObsoleteHost.OldWay` above).
public class ExperimentalHost
{
    [System.Diagnostics.CodeAnalysis.Experimental("DIAG005")]
    public int Try() => 0;

    public int Stable() => 1;
}

// Experimental with a >127-byte `UrlFormat`. Unlike Obsolete, the
// `Experimental` model keeps `UrlFormat`, so this pins that a >127-byte
// (2-byte-length-prefix) string decodes verbatim end-to-end: the owned
// `Ecma335Assembly` reader's `SerString` decoder reads it in full and the
// projection surfaces the complete URL, matching `tryFormatExperimental`
// in `tools/fcs-dump/Program.fs`, which decodes long strings verbatim
// too. The UrlFormat payload is exactly 160 ASCII bytes — same
// construction as the obsolete long-string fixtures.
[System.Diagnostics.CodeAnalysis.Experimental("DIAG006", UrlFormat = "zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz")]
public class ExperimentalLongUrlFormat
{
    public int Get() => 0;
}

// Phase 4h pins the C# 11 `required` contract: the field/property half
// (`[System.Runtime.CompilerServices.RequiredMemberAttribute]`, emitted
// by the C# compiler on every member declared with the `required`
// keyword) and the constructor half
// (`[System.Diagnostics.CodeAnalysis.SetsRequiredMembersAttribute]`,
// which lets a constructor opt out of the object-initialiser
// obligation). Both are presence-only — parameterless ctors, no payload
// — so the decoder shape is the simplest possible. F# has no `required`
// keyword so this never fires on F#-emitted members; the diff oracle
// runs the C# fixture through both sides.
//
//   - `Tag` field         → `RequiredMemberAttribute` on a field
//                           (`is_required = true` → `"required"` flag)
//   - `Name` property     → `RequiredMemberAttribute` on a property
//                           (`is_required = true` → `"required"` flag)
//   - parameterless ctor  → `SetsRequiredMembersAttribute` on `.ctor`
//                           (`sets_required_members = true` →
//                           `"sets_required_members"` flag); also pins
//                           that the bit DOES NOT bleed onto the
//                           other ctor below
//   - `(int)` ctor        → no `SetsRequiredMembers`. CS9035 (the call
//                           site warning that callers must initialise
//                           required members) is suppressed below: this
//                           constructor compiles fine as a *definition*,
//                           but consumers that invoke it from this
//                           project's compile context can't satisfy the
//                           required-members contract through it.
//
// The C# compiler also emits a type-level `[RequiredMember]` marker on
// `RequiredHolder` itself (a "this type contains required members"
// signal). Both projectors silently ignore it: it's redundant with the
// per-member flag set on the diff side, and projecting it would just
// duplicate state.
#pragma warning disable CS9035
public class RequiredHolder
{
    public required int Tag;
    public required string Name { get; set; } = "";

    [System.Diagnostics.CodeAnalysis.SetsRequiredMembers]
    public RequiredHolder() {}

    public RequiredHolder(int tag)
    {
        Tag = tag;
    }
}
#pragma warning restore CS9035

// Phase 4l pins the `unmanaged` typar constraint (`where T : unmanaged`).
// The C# compiler encodes this as TWO bits on the GenericParam row: the
// `value_type` special-constraint bit (same as `where T : struct`) plus
// a `[System.Runtime.CompilerServices.IsUnmanagedAttribute]` custom
// attribute on the typar itself. The diff oracle must agree that both
// projectors carry both bits — projecting only `struct` would silently
// lose the unmanaged-ness, and projecting only `unmanaged` would drop
// the struct-ness that callers also rely on. The normaliser renders
// `unmanaged` as an additive constraint token alongside `struct` rather
// than replacing it.
//
//   - `Blittable<T>`                → type-level typar with `unmanaged`.
//                                     Pins the `IsUnmanagedAttribute`
//                                     decode on a GenericParam owned by a
//                                     TypeDef row.
//   - `BlittableHost.MakeDefault<T>` → method-level typar with `unmanaged`.
//                                     Pins the same decode on a GenericParam
//                                     owned by a MethodDef row, ensuring
//                                     both arms route through the same
//                                     projector. (Avoids `sizeof` so the
//                                     fixture stays inside the default
//                                     no-unsafe build context.)
public class Blittable<T> where T : unmanaged
{
    public T Item;

    public Blittable(T item)
    {
        Item = item;
    }
}

public class BlittableHost
{
    public T MakeDefault<T>() where T : unmanaged => default(T);
}

// Pins the C# 13 `where T : allows ref struct` anti-constraint. Roslyn
// encodes it as a single bit on the GenericParam row —
// `GenericParameterAttributes.AllowByRefLike` (`0x0020`), which FCS reads as
// `ILGenericParameterDef.HasAllowsRefStruct`. Unlike `unmanaged` it is an
// *anti*-constraint (the argument is *permitted* to be a `ref struct`) and is
// an independent bit, so the normaliser renders it as the standalone
// `allows ref struct` token with no companion. Empirically this toolchain
// emits NO `[CompilerFeatureRequired("RefStructs")]` gate for the
// anti-constraint (only `ref struct` *types* carry that), so the fixture is
// just the bare bit on both arms:
//
//   - `RefStructBox<T>`              → type-level typar with the bit, on a
//                                     GenericParam owned by a TypeDef row.
//   - `RefStructHost.Accept<T>`     → method-level typar with the bit, on a
//                                     GenericParam owned by a MethodDef row,
//                                     so both arms route through the same
//                                     projector.
public class RefStructBox<T> where T : allows ref struct
{
}

public class RefStructHost
{
    public void Accept<T>(T value) where T : allows ref struct { }
}

// Phase 4m.1 pins the typar-only nullable decode. Under C#'s nullable
// reference types (#nullable enable), the compiler attaches a
// `[System.Runtime.CompilerServices.NullableAttribute(byte)]` directly to
// a type parameter to convey its nullable annotation state — 1 for
// `notnull` / non-nullable and 2 for `class?` / nullable. Both projectors
// must agree on the resulting per-typar nullability token (`notnull` or
// `nullable`); silently dropping either annotation would leave a gap that
// follow-on phases (per-position decode on parameters, fields, properties,
// return types — phase 4m.2 — and recursive decode through generic
// arguments — phase 4m.3) would inherit.
//
//   - `NotNullBox<T>`            → type-level typar with `where T : notnull`.
//                                  Pins the direct `NullableAttribute(1)`
//                                  decode on a GenericParam owned by a
//                                  TypeDef row. Kept member-free so no
//                                  per-position nullable metadata (a 4m.2
//                                  concern) leaks into the diff.
//   - `ClassQuestionBox<T>`      → type-level typar with `where T : class?`.
//                                  Pins the `NullableAttribute(2)` decode
//                                  combined with the reference-type bit.
//   - `NullableHost.PickNotNull` → method-level typar with `where T : notnull`,
//                                  pinning the same decode on a
//                                  GenericParam owned by a MethodDef row.
//                                  Roslyn condenses the method body to a
//                                  `[NullableContextAttribute(1)]` plus a
//                                  per-typar `NullableAttribute(1)`, so
//                                  this single shape exercises both arms.
//
// We deliberately use a local `#nullable enable` directive rather than
// the csproj-wide `<Nullable>enable</Nullable>` so the rest of the
// fixture keeps its oblivious encoding (and the diff for unrelated
// shapes stays unchanged).
#nullable enable
public class NotNullBox<T> where T : notnull
{
}

public class ClassQuestionBox<T> where T : class?
{
}

public class NullableHost
{
    public T PickNotNull<T>(T item) where T : notnull => item;
}

// `NullableContextHost` exercises the type-level
// `[NullableContextAttribute]` inheritance path: when a type has multiple
// generic methods all sharing the same nullable-default byte, Roslyn picks
// the lowest-cost emission shape — a single `[NullableContextAttribute(1)]`
// on the TypeDef and *no* attribute on the individual methods. A typar
// projector that consulted only the method's own attribute list would
// silently lose the `notnull` annotation; the type-level fallback is what
// keeps the diff honest. Whether Roslyn actually condenses for any given
// shape is a heuristic — the fixture is still valid if it doesn't (both
// sides project the same direct-attribute shape), but the multiple-method
// arrangement is the standard trigger.
public class NullableContextHost
{
    public T First<T>(T item) where T : notnull => item;
    public T Second<T>(T item) where T : notnull => item;
    public T Third<T>(T item) where T : notnull => item;
}
#nullable restore

// Phase 4m.2 pins per-position nullable decode on parameters, fields,
// properties, events, and method return types. Under `#nullable enable`
// the compiler attaches `[System.Runtime.CompilerServices.NullableAttribute(byte)]`
// to each annotable position (1 = NotAnnotated, 2 = Annotated) or
// condenses the common byte to a scope-wide
// `[System.Runtime.CompilerServices.NullableContextAttribute(byte)]` on
// the enclosing method or type. Both projectors must agree on the
// position-level nullability suffix (`!` for `NotAnnotated`, `?` for
// `Annotated`, none for `Oblivious`).
//
// The byte[] (composite) form of `NullableAttribute` — emitted when a
// position is a *generic* type like `List<string?>` or `string?[]` —
// is exercised by `CompositeNullableShapes` below (phase 4m.3).
//
//   - `NullableShapes`        → mixed positions in a single type: named
//                               and optional properties; named and
//                               optional fields; a value-type field
//                               (stays Oblivious — the value-type gate
//                               keeps the scope default from
//                               misclassifying it); methods that take and
//                               return both annotated and not-annotated
//                               references; an annotated delegate event.
//                               Pins direct-attribute decode at every
//                               position kind.
//   - `NullableContextHost2`  → exclusively NotAnnotated reference-type
//                               positions, so Roslyn condenses to a
//                               single `[NullableContextAttribute(1)]` on
//                               the TypeDef and emits *no* per-position
//                               `NullableAttribute`. A value-type field
//                               is left in to confirm the value-type gate
//                               doesn't promote `int` to `NotAnnotated`
//                               under the scope default.
#nullable enable
public class NullableShapes
{
    public string Named { get; set; } = "";
    public string? Optional { get; set; }
    public string PlainField = "";
    public string? OptField;
    public int Number;
    public string Trim(string input) => input;
    public string? Find(string? needle) => needle;
    public event System.EventHandler? Bang;
}

public class NullableContextHost2
{
    public string A = "";
    public string B = "";
    public string C = "";
    public int X;
}
#nullable restore

// Phase 4m.3 — composite nullable positions. When a position is a
// generic type like `List<string?>`, Roslyn emits
// `[NullableAttribute(byte[])]` whose bytes correspond to a pre-order
// DFS walk over the type tree (one byte per annotable node). The
// projector must walk the type tree in lockstep with the bytes,
// mirroring `Nullness.ImportILTypeWithNullness` in
// `dotnet/fsharp/src/Compiler/Checking/import.fs:276-360`.
//
// Shapes covered:
//   - `ListOfMaybeStrings`   → outer NotAnnotated, inner `string?`.
//   - `StringToMaybe`        → outer NotAnnotated, K `string!`, V `string?`.
//   - `Nested`               → three annotable visits in pre-order.
//   - `ArrayOfMaybes`        → annotated *element* of a not-annotated array.
//   - `MaybeArrayOfStrings`  → annotated *array* of not-annotated strings.
//   - `GenericStruct`        → generic value type: byte consumed but
//                              discarded, outer forced to Oblivious;
//                              args walked normally.
//   - `NullableInt`          → `System.Nullable<int>` special case: no
//                              byte consumed, no descent into `T`.
//   - `ListOfValueType`      → outer byte only; inner `int` is
//                              non-annotable so the walk consumes
//                              exactly one byte.
#nullable enable
public class CompositeNullableShapes
{
    public System.Collections.Generic.List<string?> ListOfMaybeStrings = new();
    public System.Collections.Generic.Dictionary<string, string?> StringToMaybe = new();
    public System.Collections.Generic.List<System.Collections.Generic.List<string?>> Nested = new();
    public string?[] ArrayOfMaybes = System.Array.Empty<string?>();
    public string[]? MaybeArrayOfStrings;
    public System.Collections.Generic.KeyValuePair<string?, int> GenericStruct;
    // Generic value type whose reference-typed arg has no direct
    // `?` annotation — Roslyn emits no `NullableAttribute` on the field
    // because the args match the class-level `NullableContext(1)`. The
    // walker must still descend through the value-type outer (one byte
    // consumed and discarded) and broadcast the context byte to the
    // `string` arg so it lands as `NotAnnotated`. Without that the
    // inner `string` would silently project as `Oblivious` and the
    // diff would diverge.
    public System.Collections.Generic.KeyValuePair<string, int> GenericStructInContext;
    public int? NullableInt;
    // `Nullable<KeyValuePair<string?, int>>` — the outer `System.Nullable`
    // wrapper consumes no byte (matches `isSystemNullable` at
    // `import.fs:281`), but the walker still recurses into the generic
    // arg `KeyValuePair<string?, int>` per `import.fs:334`: the inner
    // KVP consumes-and-discards one byte, `string` consumes one byte
    // (annotated), and `int` consumes zero. Without that descent the
    // inner `string?` would silently project as `Oblivious`.
    public System.Collections.Generic.KeyValuePair<string?, int>? NullableOfGenericStruct;
    public System.Collections.Generic.List<int> ListOfValueType = new();

    public System.Collections.Generic.List<string?> Echo(
        System.Collections.Generic.List<string?> input,
        string?[] more)
        => input;
}
#nullable restore

// Phase 4n — `[System.Reflection.DefaultMemberAttribute(string)]` decoded
// to `Entity::default_member: Option<DefaultMember>`. Roslyn emits the
// attribute automatically on any class that declares an indexer
// (`this[...]`), using `"Item"` as the member name; user code can also
// write the attribute explicitly with an arbitrary string. The
// hand-applied form below exercises a non-`"Item"` name through the
// decode path.
[System.Reflection.DefaultMember("CustomThing")]
public class ExplicitDefaultMember
{
    public string CustomThing => "x";
}

// Phase B1 — real C# indexer. Roslyn auto-emits `[DefaultMember("Item")]`
// on the type and an `Item` property whose signature carries the index
// parameter(s). This pins the indexer-projection path end-to-end on both
// projectors: `project_property` must render the index dimension rather
// than refuse, and fcs-dump must render `ILPropertyDef.Args` in the same
// bracketed shape. The auto-emitted `"Item"` default-member name is the
// real-world counterpart to the synthetic-blob unit test
// `indexer_host_decodes_default_member_item`.
public class IndexerHost
{
    public int this[int i] => i;
}

// Overloaded indexer. Both accessors compile to a method named `get_Item`,
// differing only in their parameter signature — so fcs-dump's name-only
// accessor lookup is ambiguous and must disambiguate by signature, matching
// the Rust side which binds each property to its accessor via ECMA-335
// MethodSemantics. Pins that legal overloaded indexers project both index
// dimensions in lockstep on both projectors.
public class OverloadedIndexerHost
{
    public int this[int i] => i;
    public string this[string s] => s;
}

// Phase B3 — nullable index parameters. The index dimension's nullness
// lives on the getter `get_Item`'s parameter (here via the getter's
// `[NullableContextAttribute(2)]` scope default), not on the property
// signature, which carries types only. The importer and fcs-dump both
// source the index dimension from the getter parameter so the `?` lands.
// `Scalar` pins the outer reference annotation; `Composite` pins inner
// composite nullness (`List<string?>`), which the property-signature type
// alone projects as Oblivious and so cannot carry.
#nullable enable
public class NullableIndexerHost
{
    public string? this[string? key] => key;
    public string this[System.Collections.Generic.List<string?> xs] => xs[0]!;
}
#nullable restore

// Byref-like intrinsics. FCS classifies `System.TypedReference`,
// `System.ArgIterator`, and `System.RuntimeArgumentHandle` as byref-*like*
// (`isByrefTyconRef`, `TypedTreeOps.ExprConstruction.fs`), so
// `FSharpEntity.IsByRef` is `true` for all three even though none is a real
// `byref<T>` (they carry zero generic arguments). Each exercises a distinct
// reader path:
//
//   - `TypedReference` sits in a signature as `ELEMENT_TYPE_TYPEDBYREF`
//     (0x16) — no `TypeDefOrRef` token — which the signature decoder now
//     projects to a `System.TypedReference` value type.
//   - `ArgIterator` / `RuntimeArgumentHandle` sit as ordinary
//     `ELEMENT_TYPE_VALUETYPE <token>` and always decoded cleanly on the Rust
//     side; they pin the fcs-dump oracle's byref-like handling (a 0-arg
//     `IsByRef` entity must render as its named type, not crash the
//     one-generic-arg byref path).
public class ByRefLikeIntrinsics
{
    public static void TakeTypedRef(System.TypedReference tr) { }
    public static void TakeArgIterator(System.ArgIterator it) { }
    public static void TakeArgHandle(System.RuntimeArgumentHandle h) { }
}

// Byref members — a `ref` field (in a `ref struct`) and `ref`-returning
// properties/indexers (`ELEMENT_TYPE_BYREF` at the outer position). Each keeps
// `T&` as its projected type, exactly like a `ref` method return. Two referent
// kinds pin the nullability-suffix placement, which the byref wrapper carries
// *after* the `&` (`T&{suffix}`, matching the byref-return convention):
//
//   - value referent (`ref int`)      → `System.Int32&`      (no suffix)
//   - reference referent (`ref string?`) → `System.String&?` (suffix after `&`)
#nullable enable
public ref struct RefFieldHost
{
    public ref int Slot;
    public ref string? Name;
}
public class RefAccessorHost
{
    private int[] _data = new int[4];
    private string?[] _names = new string?[1];
    public ref int this[int i] => ref _data[i];
    public ref int First => ref _data[0];
    public ref string? FirstName => ref _names[0];
}
#nullable restore

// `init`-only setters (C# 9+). An `init` accessor compiles to a `set_X` whose
// *void* return carries `modreq(System.Runtime.CompilerServices.IsExternalInit)`,
// so its MethodDefSig is `CMOD_REQD <IsExternalInit> VOID` — a modreq before
// `void`. The reader recognises that shape (rather than failing at the `VOID`)
// and projects the property with a setter, exactly as it would a plain `set`
// (neither the model nor FCS's IL-property view distinguishes `init` from
// `set`). `ReadWrite` keeps a plain `set` beside an `init` one so the two paths
// are pinned side by side.
public class InitHost
{
    public int Value { get; init; }
    public string Name { get; init; } = "";
    public int ReadWrite { get; set; }
    public int GetOnly { get; }
}

// A positional record — the ubiquitous real-world source of `init` setters: its
// `X`/`Y` positional properties compile to `get`/`init` pairs. It also drags in
// the record surface (`<Clone>$`, `EqualityContract`, `Deconstruct`,
// `PrintMembers`, `Equals`/`GetHashCode`/`op_Equality`), so it pins that the
// init-setter fix does not disturb the rest.
public record PointRecord(int X, int Y);

// Custom modifiers (ECMA-335 II.7.1.1: a `modopt` may be ignored, a `modreq`
// must be understood). Exactly two `modreq`s occur across the whole .NET 10
// runtime + ref pack, and both are pinned here.
//
// **Read-only references** — C#'s `in` / `ref readonly`, F#'s `inref<'T>`:
// readable through, not writable. Roslyn encodes this *two* ways, and which one
// you get depends on whether the CLI has to match on it:
//
//   - `modreq(System.Runtime.InteropServices.InAttribute)` **in the signature**,
//     for a byref *return* (`ModifierHost.Pick`, `ReadonlyRefAccessorHost`'s
//     accessors — and so the property/indexer type mirroring them) and for an
//     `in` parameter of a *virtual/abstract/interface* member
//     (`IModifierSink.Accept`, `ModifierSink.Accept`), where an override must
//     line up signature-for-signature;
//   - a `[IsReadOnly]` / `[RequiresLocation]` **attribute** on the position
//     otherwise — an `in` parameter of an ordinary method (`ModifierHost.Sum`,
//     `.Peek`), a `ref readonly` field (`ReadonlyRefFieldHost.Slot`), whose
//     signatures carry a *plain* byref.
//
// The model unions the two (`TypeRef::ByRef { readonly }`, and — a parameter's
// byref being a flag rather than part of its type — `Parameter::is_readonly_ref`),
// so `in int` reads the same on a virtual member and an ordinary one. Both
// encodings are exercised below; reading only the modifier would leave half of
// them looking writable.
//
// **`volatile`** — `modreq(System.Runtime.CompilerServices.IsVolatile)` on a
// *field* type is the sole encoding of C#'s `volatile` (there is no flag bit),
// so dropping it would silently mismodel the field's memory semantics. The
// projector peels it into `Field::is_volatile`.
//
// A nullable-reference referent (`in string?`, `volatile string?`) pins that a
// modifier consumes no `[Nullable]` byte: Roslyn's pre-order walk steps straight
// past it, so the suffix must still land on the referent.
#nullable enable
public class ModifierHost
{
    // `in` and (C# 12) `ref readonly` parameters. Both compile to the same
    // `modreq(InAttribute)` byref; they differ only in call-site attributes.
    public static int Sum(in int a, in int b) => a + b;
    public static int Peek(ref readonly int slot) => slot;
    public static int Mixed(in int a, ref int b, out int c) { c = a + b; return c; }
    public static int Length(in string? s) => s is null ? 0 : s.Length;

    private static readonly int[] Data = new int[4];

    // A `ref readonly` return: `modreq(InAttribute)` over the byref return.
    public static ref readonly int Pick(int i) => ref Data[i];
}

// The parameter-position `modreq`: an `in` parameter on an interface method and
// on the virtual method implementing it. Unlike `ModifierHost.Sum`'s (whose
// signature is a plain byref + `[IsReadOnly]`), these carry
// `modreq(InAttribute)` *in the signature*, because an implementation has to
// match the declaration byte for byte. This is the shape that accounts for the
// parameter-position custom modifiers throughout the BCL.
public interface IModifierSink
{
    int Accept(in int value);
    ref readonly int Latest { get; }
}

public class ModifierSink : IModifierSink
{
    private int _latest;
    public virtual int Accept(in int value) => value;
    public ref readonly int Latest => ref _latest;
}

public ref struct ReadonlyRefFieldHost
{
    // C# 11 `ref readonly` fields. Their signatures are *plain* byrefs — the
    // read-only-ness is the `[IsReadOnly]` attribute on the field row, not a
    // modifier — so they pin the attribute half of the union.
    public ref readonly int Slot;
    public ref readonly string? Name;
}

public class ReadonlyRefAccessorHost
{
    private int[] _data = new int[4];
    private string?[] _names = new string?[1];
    public ref readonly int this[int i] => ref _data[i];
    public ref readonly int First => ref _data[0];
    public ref readonly string? FirstName => ref _names[0];
}

public class VolatileHost
{
    public volatile int Counter;
    public volatile string? Label;
    public static volatile bool Ready;
}
#nullable restore
