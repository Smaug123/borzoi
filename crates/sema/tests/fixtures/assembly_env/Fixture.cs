// Test data for crates/sema's AssemblyEnv index (tests/all/assembly_env.rs) and the
// fully-qualified assembly-reference resolution differential
// (tests/all/resolve_assembly_diff.rs), where it is the *referenced assembly*.
//
// Deliberately small but exercises every lookup the index supports:
//   - top-level types across two namespaces (`Demo`, `Demo.Sub`)  → lookup_type
//   - a nested type (`Demo.Thing.Inner`)                          → nested(...)
//   - named members on a couple of types                         → member(...)
//   - static members reachable from a qualified value path        → Stage D
//
// Read through Ecma335Assembly::enumerate_type_defs, so only the .dll's logical
// shape matters; the source is never compiled by Rust.

namespace Demo
{
    public class Thing
    {
        public void Go() { }

        public int Value;

        public class Inner
        {
            public void Tick() { }
        }
    }

    public class Other { }

    // Instance-data-member shapes for the Stage-3.3a member-access filter
    // (`AssemblyEnv::instance_data_member_ty`, tests/all/assembly_env.rs). Each pins
    // one arm of "a *single unambiguous public instance readable data member*
    // resolves; everything else defers":
    //   - `Count`      public instance field                → resolves
    //   - `Name`       public instance get/set property     → resolves
    //   - `ReadOnly`   public instance get-only property    → resolves
    //   - `WriteOnly`  public instance set-only property    → defers (not readable)
    //   - `PrivGet`    public set, *private* get            → defers (read inaccessible)
    //   - `this[int]`  indexer (index parameter)            → defers
    //   - `StaticCount`/`StaticProp` static members         → defer
    //   - `Secret`/`Hidden` internal/private members        → defer
    //   - `Go`         instance method                      → defers
    public class Widget
    {
        public int Count;
        public string Name { get; set; }
        public int ReadOnly => Count;
        public int WriteOnly { set { Count = value; } }

        // A public *setter* with a *private* getter: the property-level
        // accessibility is `Public` (least-restrictive of the two accessors), but a
        // read `w.PrivGet` is inaccessible cross-assembly. The filter must gate on
        // the getter's own accessibility, so this defers.
        public int PrivGet { private get; set; }

        public int this[int i] => Count + i;

        public static int StaticCount;
        public static int StaticProp { get; set; }

        internal int Secret;
        private int Hidden { get; set; }

        public void Go() { }
    }

    // Instance-*method* shapes for the Stage-3.3d method-call filter
    // (`AssemblyEnv::instance_method`, tests/all/assembly_env.rs). Each pins one arm of
    // "a *single non-overloaded, non-generic public instance method* resolves to
    // its return type; everything else defers":
    //   - `Ping`   single-candidate, returns `int`       → resolves (ret Int32)
    //   - `Label`  single-candidate, returns `string`    → resolves (ret String)
    //   - `Over`   two overloads                         → defers (overloaded)
    //   - `Echo`   generic method (`<T>`)                → defers (generic)
    //   - `Act`    single-candidate, returns `void`      → found (the *wake* defers
    //                                                       the unit type, but the
    //                                                       selection still returns it
    //                                                       so its identity records)
    //   - `Stat`   static method                         → defers (not instance)
    public class Gizmo
    {
        public int Ping() => 0;
        public string Label() => "";

        public int Over(int x) => x;
        public int Over(string s) => s.Length;

        public T Echo<T>(T x) => x;

        public void Act() { }

        public static int Stat() => 0;
    }

    // Inheritance shapes for the Stage-3.x-inh base-class walk
    // (`AssemblyEnv::instance_method` / `instance_data_member`, tests/all/assembly_env.rs).
    // `Demo.Derived : Demo.Base` — both live in this assembly, so the walk resolves
    // the base and completes the chain (up to the *absent* `System.Object`):
    //   - `Inherited`  declared on Base only          → resolves through Derived to Base
    //   - `BaseField`  data member on Base only        → inherited data member resolves
    //   - `Named`      Base `virtual`, Derived override → same partial sig at 2
    //                                                     levels ⇒ dedups to the
    //                                                     nearest (Derived) ⇒ resolves (OV-3)
    //   - `Clash`      Base(string) + Derived(int)      → distinct sigs ⇒ overload ⇒ defers
    //   - `RefClash`   Base(int) + Derived(ref int)     → `int` vs `int&` are distinct
    //                                                     signatures ⇒ overload ⇒ defers
    //                                                     (the OV-3 byref-vs-by-value key)
    //   - `Own`        Derived only                     → resolves on Derived
    public class Base
    {
        public int Inherited() => 0;
        public int BaseField;
        public virtual string Named() => "b";
        public int Clash(string s) => s.Length;
        public int RefClash(int x) => x;
    }

    public class Derived : Base
    {
        public override string Named() => "d";
        public int Clash(int x) => x;
        public string RefClash(ref int x) => x.ToString();
        public int Own() => 0;
    }

    // `Demo.Clashy` declares an *overload* of an `System.Object` method —
    // `Equals(int)`. Its base `System.Object` is NOT in this single-assembly
    // fixture, so the method group for `Equals` is *incomplete*: the inherited
    // `object.Equals(object)` is invisible. The walk must therefore DEFER an
    // `Equals` call rather than pick the visible `Equals(int)` — the
    // Object-capped-chain soundness guard (FCS binds the inherited `Equals(object)`,
    // not this overload). A non-`Object` name (`Ping`) on the same type resolves.
    public class Clashy
    {
        public string Equals(int x) => x.ToString();
        public int Ping() => 0;
    }

    // Name-hiding across static/instance for the base-class walk. `HideDerived`
    // declares a public **static** member whose name collides with an inherited
    // public **instance** member on `HideBase`. The static hides the inherited
    // instance member (C# CS0108) but cannot be reached through a value receiver, so
    // a value-receiver access/call must DEFER (FCS leaves `HideDerived().Prop` /
    // `.Meth()` as `obj`) rather than fall through to the hidden base instance member.
    public class HideBase
    {
        public int Prop => 0;
        public int Meth() => 0;
    }

    public class HideDerived : HideBase
    {
        public static int Prop = 1;
        public static int Meth() => 1;
    }

    // A public INSTANCE method and a public STATIC *overload* of the same name on one
    // type. The static does NOT hide the instance method — they coexist as an overload
    // set, and F# resolves an instance call to the instance method. So the base walk
    // must still resolve `Pick` (a same-name static at the *owning* level is ignored,
    // not a blocker) — the counterpart to `HideDerived`, whose static is the *only*
    // member of the name at its level and therefore hides.
    public class StaticOverload
    {
        public int Pick(int x) => x;
        public static int Pick(int a, int b) => a + b;
    }

    // A closed *generic* base makes the base chain Incomplete (the walk can't
    // substitute `T`). `DerivesGeneric`'s OWN data member must still resolve — it is
    // declared on the receiver, hides any inherited member, and needs no base walk —
    // while its own method call defers (an inherited same-arity overload from the
    // unwalkable base can't be ruled out) and an inherited member defers.
    public class GenericBase<T>
    {
        public T Stored;
    }

    public class DerivesGeneric : GenericBase<int>
    {
        public int OwnField;
        public int OwnMethod() => 0;
    }

    // Static members, reachable from F# by a fully-qualified *value* path
    // (`Demo.Calc.Zero`, `Demo.Calc.Answer`) — what Stage D resolves into the
    // assembly. The enclosing `Demo.Calc` is the type the path roots at.
    // `Hush` is an internal static member: it must NOT resolve cross-assembly.
    public static class Calc
    {
        public static int Zero() => 0;

        public static int Answer => 42;

        internal static int Hush() => 0;

        // An *overloaded* public static — two public statics share the name, so it
        // is not uniquely selectable. An `open type Demo.Calc` must defer a bare
        // `Twice` rather than pick one (we don't model overload resolution).
        public static int Twice(int x) => x * 2;
        public static string Twice(string s) => s + s;
    }

    // A C#-style extension class: `Doubled` is an extension method (a static
    // carrying `[Extension]`, first parameter `this`), `Origin` an ordinary
    // static alongside it.
    //
    // FCS admits an extension method to *unqualified* scope from **no** open:
    // `open type Demo.Exts` then bare `Doubled 1` is FS0039 (fsi-verified with
    // `open type System.Linq.Enumerable` + bare `Select`, and with an F#
    // `[<Extension>]` type) — `ChooseMethInfosForNameEnv` filters it via
    // `IsMethInfoPlainCSharpStyleExtensionMember`. Its plain sibling `Origin`
    // does resolve, so the filter must be extension-keyed, not type-keyed.
    // Unlike an F#-native augmentation, it stays reachable *qualified*
    // (`Demo.Exts.Doubled 1`; fsi: `System.Linq.Enumerable.Select(xs, f)` compiles).
    public static class Exts
    {
        public static int Doubled(this int x) => x * 2;

        public static int Origin() => 0;
    }

    // An internal type — inaccessible cross-assembly, so a qualified path
    // through it must not resolve.
    internal static class Hidden
    {
        public static int Secret() => 0;
    }

    // Same simple name, different generic arity — distinct CLR type defs
    // (`Pair`, ``Pair`1``, ``Pair`2``). `Entity.name` strips the arity suffix,
    // so the index must key on arity too or these collapse onto one handle.
    public class Pair { }
    public class Pair<T> { }
    public class Pair<T, U> { }
}

namespace Demo.Sub
{
    public class Deep { }

    // Shares the simple name `Calc` with `Demo.Calc`. With both `open Demo` and
    // `open Demo.Sub` in scope, `Calc.Zero` is ambiguous — resolution must defer
    // rather than pick one (Stage E open-ambiguity test).
    public static class Calc
    {
        public static int Zero() => 0;
    }

    // A type whose simple name collides with the *root* `Sub.Thing` below, so a
    // relative `open Sub` from `namespace Demo` (→ `Demo.Sub`) resolving `Thing`
    // can be distinguished from the root `Sub.Thing` (relative-open
    // canonicalisation test). `Demo.Sub.Thing` ≠ `Sub.Thing`.
    public class Thing { }
}

// `Demo.Sub.Extra` is the *relative* chaining target of `open Sub; open Extra`
// from `namespace Demo` (the earlier `open Sub`'s relative reading `Demo.Sub`,
// chained by `open Extra`). It shares the simple name `Shared` with the root
// `Sub.Extra` below; latest-open-wins keeps the *relative* reading higher, so a
// colliding `Shared` is `Demo.Sub.Extra.Shared`, its own-only `RelThing` resolves
// here, and the root-only `ExtraThing` falls to `Sub.Extra` (FCS).
namespace Demo.Sub.Extra
{
    public class RelThing { }

    public class Shared { }
}

// A *root* namespace `Sub`, colliding with the relative `Demo.Sub`. From
// `namespace Demo`, F# resolves a relative `open Sub` to `Demo.Sub` (the nearer
// enclosing-namespace child), so `Calc` / `Thing` after it are `Demo.Sub.*`, not
// the root `Sub.*`. Exercises that open canonicalisation picks the relative
// namespace over the as-written root (and never the wrong root entity).
namespace Sub
{
    public static class Calc
    {
        public static int Zero() => 0;
    }

    public class Thing { }

    // A type that exists ONLY in the root `Sub` (no `Demo.Sub.RootOnly`). With a
    // project `namespace Sub` merged with this root assembly `Sub`, `open Sub`
    // from `namespace Demo` must still reach it (`RootOnly` → `Sub.RootOnly`),
    // while a colliding name (`Calc`) resolves the relative `Demo.Sub` — the
    // relative-over-root precedence of a single open's merge readings.
    public class RootOnly { }

    // `Widget` lives in root `Sub` and in root `Zap` (below), but NOT in the
    // relative `Demo.Sub`. From `namespace Demo`, `open Zap; open Sub; (x: Widget)`
    // is `Sub.Widget`: the *later* `open Sub`'s **root** reading shadows the earlier
    // `open Zap` (latest-open-wins), even though `Sub`'s relative reading
    // `Demo.Sub` has no `Widget`. Exercises that a relative open's root reading is
    // ordered at *its* open's source position, not globally last.
    public class Widget { }
}

// A root `Sub.Extra`, reachable only by chaining a later `open Extra` through the
// root reading of an earlier `open Sub` (`namespace Demo; open Sub; open Extra;
// (x: ExtraThing)` → `Sub.Extra.ExtraThing`). Exercises that an open's root
// reading also feeds the shortening prefixes.
namespace Sub.Extra
{
    public class ExtraThing { }

    // Collides with `Demo.Sub.Extra.Shared`; the relative reading out-ranks this
    // root one under latest-open-wins chaining.
    public class Shared { }
}

// A root `Zap` sharing the simple name `Widget` with root `Sub` (above), for the
// latest-open-wins root-reading-shadow test.
namespace Zap
{
    public class Widget { }
}

// A *global-namespace* type sharing the simple name `Calc` with `Demo.Calc`
// (and `Demo.Sub.Calc` / `Sub.Calc`), with a unique static (`Nope`) that
// `Demo.Calc` lacks. Pins the open-partial vs complete-root direction of the
// tier walk: under `open Demo`, `Calc.Nope` matches the opened `Demo.Calc` only
// *partially* (no `Nope` there) while this root `Calc` completes the whole path
// — the sweep records what FCS does with that collision so the walker's
// complete-beats-partial rule is exercised against the root tier too.
public static class Calc
{
    public static int Nope() => 1;
}

// A relative namespace `Demo.Hush` whose *only* type is internal — so from
// another assembly it is empty (cross-assembly resolution sees only public
// types). It collides with a *public* root `Hush`. From `namespace Demo`,
// `open Hush` must NOT canonicalise to the inaccessible `Demo.Hush`; F# falls
// back to the public root `Hush`. Exercises that `has_namespace` (hence open
// canonicalisation) ignores non-public-only namespaces.
namespace Demo.Hush
{
    internal class Secret { }
}

namespace Hush
{
    public class Visible { }
}

// A NAMESPACE whose path is a MODULE in the sibling F# fixture
// (`Demo.ModuleOpen.Plain`). FCS opens and merges both halves of such a path — a
// module and a namespace can share a path only *across* assemblies (FS0247 forbids
// the same-assembly clash), so this pins the merge that Q9 of
// `docs/assembly-module-open-plan.md` verified with two probe libraries.
namespace Demo.ModuleOpen.Merged
{
    public static class FromNamespaceHalf
    {
        public static int NsStatic() => 5;
    }
}

// The EX-2 overload-gate namespace (infer_member_access_diff.rs). A C#-style
// extension class on `string` in its own namespace, so `open ExtColl` brings its
// extension methods into scope — and the overload-absence gate
// (`docs/extension-scope-enumeration-plan.md` EX-2) must defer a call whose name
// one of them declares while still committing others.
//
// `Substring` COLLIDES with String.Substring's intrinsic overload set; `IndexOf`
// is left untouched. So after `open ExtColl`:
//   - `s.IndexOf('h')`   commits (no extension of that name) — the coverage EX-2
//                        recovers from the pre-EX-2 "any open ⇒ defer" gate;
//   - `s.Substring(1.5)` is FCS-resolved to THIS extension (`double` arg, the
//                        intrinsics inapplicable) → `System.Int64`, whereas our
//                        single-candidate arity shortcut would name the
//                        inapplicable intrinsic `Substring(int)` → `System.String`
//                        (the P15 landmine). The gate must defer.
namespace ExtColl
{
    public static class StringExts
    {
        public static long Substring(this string s, double factor) => (long)(s.Length * factor);
    }
}
