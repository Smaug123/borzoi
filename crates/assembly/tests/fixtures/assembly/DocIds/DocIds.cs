// Fixture for the documentation-comment-ID differential test. Every public
// member carries a `///` comment so Roslyn emits a `<member name="…">` entry
// for it in `DocIds.xml`; that key set is the oracle the Rust doc-id generator
// must reproduce. The members are chosen to exercise each mangling rule:
// primitives, vector/multidimensional arrays, ref/out (`@`), pointers (`*`),
// generic instantiations (`{…}`), type/method typars (`` `i ``/`` ``i ``),
// constructors (`#ctor`), operators, conversion operators (`~ret`), nested
// generic types (per-level arity), fields, properties, indexers, and events.
using System;
using System.Collections.Generic;

namespace DocIds
{
    /// <summary>A documented top-level class.</summary>
    public class Shapes
    {
        /// <summary>Parameterless constructor.</summary>
        public Shapes() { }

        /// <summary>Constructor with a parameter.</summary>
        public Shapes(int x) { }

        /// <summary>A field.</summary>
        public int Value;

        /// <summary>A property.</summary>
        public string Name { get; set; }

        /// <summary>An indexer.</summary>
        public int this[int i] => i;

        /// <summary>An event.</summary>
        public event EventHandler Changed;

        /// <summary>Primitive parameters.</summary>
        public void Write(string s, int n, bool b, double d, char c) { }

        /// <summary>A vector array parameter.</summary>
        public void Vec(int[] xs) { }

        /// <summary>A rank-2 array parameter.</summary>
        public void Grid(int[,] g) { }

        /// <summary>An out parameter.</summary>
        public bool TryGet(string key, out int value) { value = 0; return false; }

        /// <summary>A ref parameter.</summary>
        public void Bump(ref int n) { }

        /// <summary>Generic instantiation parameters.</summary>
        public void Take(List<string> xs, Dictionary<string, int> m) { }

        /// <summary>A constructed nested generic: outer generic, inner non-generic
        /// (args distribute onto the encloser — `Dictionary{K,V}.Enumerator`).</summary>
        public void Enum(Dictionary<string, int>.Enumerator e) { }

        /// <summary>A constructed nested generic with both levels generic
        /// (one arg per segment — `Box{System.String}.Pair{System.Int32}`).</summary>
        public void Nest(Box<string>.Pair<int> p) { }

        /// <summary>A generic method.</summary>
        public T Id<T>(T x) => x;

        /// <summary>A generic method over an array and an instantiation.</summary>
        public void Many<T>(T[] xs, IEnumerable<T> ys) { }

        /// <summary>A pointer parameter.</summary>
        public unsafe void Ptr(int* p) { }

        /// <summary>An ordinary operator.</summary>
        public static Shapes operator +(Shapes a, Shapes b) => a;

        /// <summary>An explicit conversion operator.</summary>
        public static explicit operator int(Shapes s) => 0;

        /// <summary>An implicit conversion operator.</summary>
        public static implicit operator Shapes(int n) => new Shapes();
    }

    /// <summary>Has checked and unchecked conversion operators.</summary>
    public readonly struct Celsius
    {
        /// <summary>An unchecked conversion operator.</summary>
        public static explicit operator int(Celsius c) => 0;

        /// <summary>A checked conversion operator.</summary>
        public static explicit operator checked int(Celsius c) => 0;
    }

    /// <summary>A generic interface implemented explicitly below.</summary>
    /// <typeparam name="T">The item type.</typeparam>
    public interface IStore<T>
    {
        /// <summary>Stores an item.</summary>
        void Store(T item);

        /// <summary>The stored count.</summary>
        int Count { get; }
    }

    /// <summary>Implements <see cref="IStore{T}"/> explicitly.</summary>
    public class IntStore : IStore<int>
    {
        /// <summary>An explicit interface method implementation.</summary>
        void IStore<int>.Store(int item) { }

        /// <summary>An explicit interface property implementation.</summary>
        int IStore<int>.Count => 0;
    }

    /// <summary>A two-parameter generic interface implemented explicitly below.</summary>
    /// <typeparam name="TKey">The key type.</typeparam>
    /// <typeparam name="TValue">The value type.</typeparam>
    public interface ILookup<TKey, TValue>
    {
        /// <summary>Looks a value up by key.</summary>
        TValue Get(TKey key);
    }

    /// <summary>Implements a *two-argument* generic interface explicitly with
    /// concrete type arguments. Current Roslyn keeps the `,` separator between
    /// them in the member-name portion (`…ILookup{System#Int32,System#String}#Get`)
    /// — it does *not* rewrite the comma to `@` for concrete args.</summary>
    public class IntStringLookup : ILookup<int, string>
    {
        /// <summary>An explicit two-parameter interface method implementation.</summary>
        string ILookup<int, string>.Get(int key) => "";
    }

    /// <summary>Implements a two-argument generic interface explicitly with its
    /// own *type parameters* as the interface arguments. Fresh Roslyn keys this
    /// with `,` (`…ILookup{A,B}#Get`); the shipped BCL reference-pack XML uses
    /// `@` (`{A@B}`) for the same shape — see docs/xmldoc-explicit-interface-plan.md.
    /// Here it pins the structured `explicit_interface` read (interface =
    /// ILookup{A,B} with both args type parameters).</summary>
    public class Wrapper<A, B> : ILookup<A, B>
    {
        /// <summary>Explicit impl using the encloser's type parameters.</summary>
        B ILookup<A, B>.Get(A key) => default!;
    }

    /// <summary>A generic interface explicitly implemented with a native-int
    /// (`nint`) argument. Fresh Roslyn keys the member-name portion with the C#
    /// *alias* from source (`…IAdder{nint}#Add`) while the parameter list uses the
    /// canonical `System.IntPtr` — and the IL `MethodDef` name likewise carries
    /// `nint`, so our generator (which escapes that raw name) matches fresh
    /// Roslyn. The *shipped* BCL ref-pack XML is inconsistent for this shape:
    /// some keys use canonical `{System#IntPtr}` (and `@` separators), some emit a
    /// literal `&lt;nint&gt;` (a Roslyn doc-ID bug). Reconciling against shipped XML
    /// is deferred lookup-layer work — see docs/xmldoc-explicit-interface-plan.md.</summary>
    public interface IAdder<T>
    {
        /// <summary>Adds a value.</summary>
        T Add(T x);
    }

    /// <summary>Explicit impl of a native-int-instantiated interface.</summary>
    public class NIntAdder : IAdder<nint>
    {
        /// <summary>Explicit native-int interface method.</summary>
        nint IAdder<nint>.Add(nint x) => x;
    }

    /// <summary>Base type for a covariant-return override (a *non-interface*
    /// `MethodImpl` use that must NOT be read as an explicit interface impl).</summary>
    public class CloneBase
    {
        /// <summary>Virtual clone returning the base type.</summary>
        public virtual CloneBase Clone() => this;
    }

    /// <summary>Covariant-return override: the compiler emits a `MethodImpl`
    /// mapping this `Clone` to CloneBase.Clone, with the base *class* (not an
    /// interface) as the declaration parent. The CLR does not name-mangle it, so
    /// `explicit_interface` must stay `None`.</summary>
    public class CloneDerived : CloneBase
    {
        /// <summary>Covariant override returning the derived type.</summary>
        public override CloneDerived Clone() => this;
    }

    /// <summary>A generic class.</summary>
    /// <typeparam name="T">The element type.</typeparam>
    public class Box<T>
    {
        /// <summary>Returns the boxed value.</summary>
        public T Get() => default!;

        /// <summary>Stores a value.</summary>
        public void Put(T item) { }

        /// <summary>A non-generic nested type (inherits the encloser's arity).</summary>
        public class Inner
        {
            /// <summary>References the enclosing type parameter.</summary>
            public T Outer() => default!;
        }

        /// <summary>A generic nested type (own arity on top of the encloser's).</summary>
        /// <typeparam name="U">The second element type.</typeparam>
        public class Pair<U>
        {
            /// <summary>References both type parameters.</summary>
            public void Set(T t, U u) { }
        }
    }

    /// <summary>A two-parameter generic class.</summary>
    /// <typeparam name="TKey">The key type.</typeparam>
    /// <typeparam name="TValue">The value type.</typeparam>
    public class Map<TKey, TValue>
    {
        /// <summary>Looks a value up by key.</summary>
        public TValue Lookup(TKey key) => default!;
    }
}
