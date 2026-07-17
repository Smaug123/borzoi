namespace CSharpSidecar;

using System.Buffers.Binary;
using System.Security.Cryptography;
using System.Text;

using Microsoft.CodeAnalysis;
using Microsoft.CodeAnalysis.Emit;

/// <summary>
/// SHA-256 content-addressed cache-key derivation, per
/// <c>docs/completed/csharp-sidecar-plan.md</c> D6. The key is a hash over: the csproj
/// bytes; the output of Roslyn's own
/// <see cref="RoslynDeterministicKey"/> — which itself captures every emit-
/// affecting input the compiler has identified (sources with SHA-1 content
/// checksums, every <see cref="CompilationOptions"/> field, parse options,
/// reference identity and MVID, tools versions, emit options); the project's
/// <see cref="Project.AdditionalDocuments"/> and
/// <see cref="Project.AnalyzerConfigDocuments"/> (which sit on the
/// <see cref="Project"/> but are not on the <see cref="Compilation"/>); and
/// the cache keys of any transitive project references.
/// </summary>
/// <remarks>
/// <para>
/// The encoding is a stream of tagged length-prefixed records. Each record is
/// <c>[u32 BE tag length][tag ASCII bytes][u64 BE data length][data bytes]</c>.
/// Including the tag in the hash means two adjacent sections cannot collide by
/// concatenation.
/// </para>
/// <para>
/// Determinism for any collection iteration we control is enforced by sorting
/// with <see cref="StringComparer.Ordinal"/> before hashing. The Roslyn
/// detkey JSON is itself stable for stable inputs (we probe this at startup).
/// </para>
/// </remarks>
internal static class Cache
{
    /// <summary>
    /// Compute the cache key for a loaded project. The <paramref name="compilation"/>
    /// must already have <see cref="CSharpCompilationOptions.Deterministic"/>
    /// set to <c>true</c> — without it, MVIDs are random per emit and Roslyn's
    /// deterministic-key output is unstable across reloads of the same project.
    /// </summary>
    public static byte[] ComputeKey(
        Project project,
        Compilation compilation,
        EmitOptions emitOptions,
        string csprojPath,
        IReadOnlyList<(string ProjectPath, byte[] Key)> transitiveProjectKeys)
    {
        using var hash = IncrementalHash.CreateHash(HashAlgorithmName.SHA256);

        // Defensive layer below Roslyn's view: csproj bytes that affect MSBuild
        // evaluation but not directly Compilation (e.g. SDK target reads from
        // disk) would still flip this. Roslyn's deterministic key sees only
        // the already-evaluated Compilation.
        AppendBytes(hash, "csproj", File.ReadAllBytes(csprojPath));

        // Roslyn's authoritative description of "what affects emit output."
        // Covers compilation options (all fields), parse options, syntax
        // trees with SHA-1 content checksums, metadata references (identity
        // + MVID — and for CompilationReferences, the referenced
        // compilation's own detkey, which is the Merkle property we need
        // for ProjectReferences), tools versions, and emit options.
        AppendBytes(hash, "roslyn-detkey", RoslynDeterministicKey.Compute(compilation, emitOptions));

        // AdditionalDocuments and AnalyzerConfigDocuments are project-level
        // concepts that don't reach the Compilation directly; hash them
        // ourselves. Sorted by path so iteration order doesn't bleed in.
        var additionalDocs = project.AdditionalDocuments
            .Select(d => new { Document = d, SortKey = d.FilePath ?? d.Name })
            .OrderBy(x => x.SortKey, StringComparer.Ordinal)
            .ToList();
        foreach (var entry in additionalDocs)
        {
            AppendString(hash, "add-path", entry.SortKey);
            var text = entry.Document.GetTextAsync().GetAwaiter().GetResult();
            AppendBytes(hash, "add-bytes", Encoding.UTF8.GetBytes(text.ToString()));
        }

        var configDocs = project.AnalyzerConfigDocuments
            .Select(d => new { Document = d, SortKey = d.FilePath ?? d.Name })
            .OrderBy(x => x.SortKey, StringComparer.Ordinal)
            .ToList();
        foreach (var entry in configDocs)
        {
            AppendString(hash, "cfg-path", entry.SortKey);
            var text = entry.Document.GetTextAsync().GetAwaiter().GetResult();
            AppendBytes(hash, "cfg-bytes", Encoding.UTF8.GetBytes(text.ToString()));
        }

        // Transitive ProjectReferences — phase 5 populates this from a
        // topo-sort of the closure. Phase 4 callers pass an empty list;
        // Roslyn's detkey already cascades through CompilationReference, so
        // the dependent project's key changes when the referenced project's
        // source changes. This explicit section is belt-and-braces (and the
        // wire-visible Merkle proof phase 5 will expose).
        var transitives = transitiveProjectKeys
            .OrderBy(x => x.ProjectPath, StringComparer.Ordinal)
            .ToList();
        foreach (var (path, key) in transitives)
        {
            AppendString(hash, "proj-ref-path", path);
            AppendBytes(hash, "proj-ref-key", key);
        }

        return hash.GetHashAndReset();
    }

    /// <summary>
    /// Lowercase hex rendering of a 32-byte SHA-256 digest, matching the
    /// convention git uses for loose objects. The Rust side parses hex back
    /// into <c>[u8; 32]</c>; using lowercase consistently means the round-trip
    /// is byte-identical and not just semantically equivalent.
    /// </summary>
    public static string ToHexLower(byte[] hash) =>
        Convert.ToHexString(hash).ToLowerInvariant();

    private static void AppendBytes(IncrementalHash hash, string tag, ReadOnlySpan<byte> data)
    {
        Span<byte> lenBuf = stackalloc byte[8];
        var tagBytes = Encoding.ASCII.GetBytes(tag);

        BinaryPrimitives.WriteUInt32BigEndian(lenBuf[..4], (uint)tagBytes.Length);
        hash.AppendData(lenBuf[..4]);
        hash.AppendData(tagBytes);

        BinaryPrimitives.WriteUInt64BigEndian(lenBuf, (ulong)data.Length);
        hash.AppendData(lenBuf);
        hash.AppendData(data);
    }

    private static void AppendString(IncrementalHash hash, string tag, string value) =>
        AppendBytes(hash, tag, Encoding.UTF8.GetBytes(value));
}
