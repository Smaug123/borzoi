namespace CSharpSidecar.Tests;

using Microsoft.CodeAnalysis.Emit;

using CSharpSidecar.Tests.TestHelpers;

using FsCheck;
using FsCheck.Xunit;

/// <summary>
/// FsCheck properties over <see cref="Cache.ComputeKey"/>. The
/// example-based tests in <c>CacheKeyTests</c> pin specific invariants;
/// these generalise the same shape across thousands of generated inputs
/// (gospel principle 4 — "cache-key is a function of inputs" is exactly the
/// kind of claim property tests buy more cheaply than example tests).
/// </summary>
public sealed class CacheKeyPropertyTests
{
    private static readonly EmitOptions EmitOpts = new(
        metadataOnly: true,
        includePrivateMembers: true,
        tolerateErrors: true);

    private const string MinimalCsproj =
        "<Project Sdk=\"Microsoft.NET.Sdk\">\n"
        + "  <PropertyGroup>\n"
        + "    <TargetFramework>net10.0</TargetFramework>\n"
        + "  </PropertyGroup>\n"
        + "</Project>\n";

    /// <summary>
    /// Permuting <c>transitiveProjectKeys</c> must not change the cache key.
    /// <c>Cache.ComputeKey</c> sorts by path before hashing; for any list
    /// we generate, the forward and reverse orderings have to produce
    /// byte-identical hashes. Reversal is sufficient: it's a single
    /// non-trivial permutation, and the only way to satisfy it for every
    /// generated list is to be order-independent.
    /// </summary>
    [Property]
    public bool TransitiveKey_OrderInvariance(KeyValuePair<NonEmptyString, byte[]>[] rawKeys)
    {
        var pairs = NormaliseTransitivePairs(rawKeys);
        if (pairs.Count < 2)
        {
            // No meaningful permutation to test.
            return true;
        }

        using var csproj = new TempCsproj(MinimalCsproj);
        var built = AdhocCompilation.Build("order-inv", csproj.Path);

        var forward = Cache.ComputeKey(
            built.Project, built.Compilation, EmitOpts, csproj.Path, pairs);
        var reversed = Enumerable.Reverse(pairs).ToList();
        var backward = Cache.ComputeKey(
            built.Project, built.Compilation, EmitOpts, csproj.Path, reversed);

        return forward.SequenceEqual(backward);
    }

    /// <summary>
    /// Mutating any byte of any transitive project key must change the cache
    /// key. This is the "sensitivity" side of D6 — if it failed for any
    /// generated input, a leaf-project change could ride through unnoticed.
    /// </summary>
    [Property]
    public bool TransitiveKey_ByteMutationChangesKey(
        NonEmptyString depPath,
        NonEmptyArray<byte> depKey,
        byte mutateIndexRaw,
        byte newByte)
    {
        var path = depPath.Get;
        var key = depKey.Get;
        var mutateIndex = mutateIndexRaw % key.Length;
        if (key[mutateIndex] == newByte)
        {
            // The mutation would be a no-op; skip rather than weaken the
            // property to "==".
            return true;
        }

        var mutated = (byte[])key.Clone();
        mutated[mutateIndex] = newByte;

        using var csproj = new TempCsproj(MinimalCsproj);
        var built = AdhocCompilation.Build("mut", csproj.Path);

        var original = Cache.ComputeKey(
            built.Project, built.Compilation, EmitOpts, csproj.Path,
            new[] { (path, key) });
        var altered = Cache.ComputeKey(
            built.Project, built.Compilation, EmitOpts, csproj.Path,
            new[] { (path, mutated) });

        return !original.SequenceEqual(altered);
    }

    /// <summary>
    /// Mutating any byte of any transitive project <em>path</em> must change
    /// the cache key. Same shape as the byte-key test; paths are folded into
    /// the hash under their own tag.
    /// </summary>
    [Property]
    public bool TransitiveKey_PathMutationChangesKey(
        NonEmptyString depPath,
        NonEmptyArray<byte> depKey,
        char suffix)
    {
        var path = depPath.Get;
        var altered = path + suffix;
        if (altered == path)
        {
            return true;
        }

        using var csproj = new TempCsproj(MinimalCsproj);
        var built = AdhocCompilation.Build("pathmut", csproj.Path);

        var originalKey = Cache.ComputeKey(
            built.Project, built.Compilation, EmitOpts, csproj.Path,
            new[] { (path, depKey.Get) });
        var alteredKey = Cache.ComputeKey(
            built.Project, built.Compilation, EmitOpts, csproj.Path,
            new[] { (altered, depKey.Get) });

        return !originalKey.SequenceEqual(alteredKey);
    }

    /// <summary>
    /// Two semantically distinct input slots must not collide even when
    /// fed identical bytes: feeding bytes <c>B</c> as the csproj content
    /// must produce a different cache key than feeding a minimal csproj and
    /// the same <c>B</c> as a transitive project key. This is the
    /// surface-level manifestation of the tagged-length-prefix encoding
    /// in <c>Cache.AppendBytes</c> — without distinct tags, two record
    /// streams that happen to concatenate to the same bytes could collide.
    /// </summary>
    [Property]
    public bool TaggedRecord_CsprojBytesVsTransitiveKey_DoNotCollide(NonEmptyArray<byte> payload)
    {
        var bytes = payload.Get;

        // Case A: bytes live in the csproj slot, no transitive ref.
        using var csprojA = new TempCsproj(bytes);
        var builtA = AdhocCompilation.Build("collide-a", csprojA.Path);
        var keyA = Cache.ComputeKey(
            builtA.Project, builtA.Compilation, EmitOpts, csprojA.Path,
            Array.Empty<(string, byte[])>());

        // Case B: csproj is minimal; identical bytes live in the transitive
        // ref slot at a fixed path.
        using var csprojB = new TempCsproj(MinimalCsproj);
        var builtB = AdhocCompilation.Build("collide-b", csprojB.Path);
        var keyB = Cache.ComputeKey(
            builtB.Project, builtB.Compilation, EmitOpts, csprojB.Path,
            new[] { ("/abs/leaf.csproj", bytes) });

        return !keyA.SequenceEqual(keyB);
    }

    /// <summary>
    /// <c>EmitOrCache</c> derives the cached DLL filename as
    /// <c>Cache.ToHexLower(key) + ".dll"</c>. The escape-attempt Rust
    /// integration test (a hostile <c>&lt;AssemblyName&gt;</c>) asserts the
    /// filename cannot contain a path separator or <c>..</c>; structurally
    /// that is true only because <c>ToHexLower</c> emits exclusively
    /// lowercase-hex characters. Pin the structural invariant here so
    /// future changes to the rendering catch a regression at the unit
    /// level rather than at the integration boundary.
    /// </summary>
    [Property]
    public bool ToHexLower_OutputsLowercaseHexOnly(byte[] bytes)
    {
        var hex = Cache.ToHexLower(bytes);
        return hex.Length == bytes.Length * 2
            && hex.All(c => (c >= '0' && c <= '9') || (c >= 'a' && c <= 'f'));
    }

    /// <summary>
    /// Convert FsCheck's raw input shape into the
    /// <c>(string, byte[])</c> tuple list <c>Cache.ComputeKey</c> expects,
    /// deduplicating by path so the forward/reverse comparison isn't
    /// confused by repeated keys colliding under the sort.
    /// </summary>
    private static List<(string ProjectPath, byte[] Key)> NormaliseTransitivePairs(
        KeyValuePair<NonEmptyString, byte[]>[] rawKeys)
    {
        var seen = new HashSet<string>(StringComparer.Ordinal);
        var pairs = new List<(string, byte[])>();
        foreach (var kvp in rawKeys)
        {
            var path = kvp.Key.Get;
            if (seen.Add(path))
            {
                pairs.Add((path, kvp.Value ?? Array.Empty<byte>()));
            }
        }
        return pairs;
    }
}
