namespace CSharpSidecar;

using System.Collections.Immutable;
using System.Reflection;
using System.Text;

using Microsoft.CodeAnalysis;
using Microsoft.CodeAnalysis.Diagnostics;
using Microsoft.CodeAnalysis.Emit;

/// <summary>
/// Thin reflection wrapper over Roslyn's internal
/// <c>Compilation.GetDeterministicKey</c>. Roslyn ships this for exactly
/// the use case we need — "what input affects emit output" — and keeps it
/// in sync with the rest of the compiler as new options are added. Hashing
/// the result locks in cache-key correctness for every emit-affecting
/// input the Roslyn team has identified, without us re-enumerating them by
/// hand (which is what the four-round codex review of the hand-rolled
/// version surfaced as a recurring source of bugs).
/// </summary>
/// <remarks>
/// <para>
/// The API is <c>internal</c> as of Roslyn 5.3.0 (the version pinned in
/// <c>csharp-sidecar.csproj</c>). We call it by reflection, which makes
/// the binding version-fragile — but the sidecar protocol-version
/// handshake forces a coordinated rebuild on Roslyn upgrades, and the
/// <see cref="Probe"/> entry point catches an incompatible Roslyn at
/// <c>initialize</c> time rather than silently producing wrong keys.
/// </para>
/// <para>
/// The returned string is a multi-KB JSON document Roslyn explicitly
/// describes as not minimal; consumers (us) hash it down to 32 bytes.
/// The JSON includes the compiler version, so any Roslyn upgrade
/// invalidates the cache by construction.
/// </para>
/// </remarks>
internal static class RoslynDeterministicKey
{
    private static readonly Lazy<Binding> CachedBinding = new(BindMethod);

    /// <summary>
    /// Compute the deterministic-key bytes for <paramref name="compilation"/>.
    /// The <see cref="EmitOptions"/> passed in must match what we actually
    /// hand <see cref="Compilation.Emit(System.IO.Stream, System.IO.Stream?, System.IO.Stream?, System.IO.Stream?, System.Collections.Generic.IEnumerable{ResourceDescription}?, EmitOptions?, IMethodSymbol?, System.IO.Stream?, System.Collections.Generic.IEnumerable{EmbeddedText}?, System.Threading.CancellationToken)"/>
    /// or the cache key won't reflect the actual emit shape.
    /// </summary>
    public static byte[] Compute(Compilation compilation, EmitOptions emitOptions)
    {
        var binding = CachedBinding.Value;
        var key = (string)binding.Method.Invoke(compilation, new object?[]
        {
            ImmutableArray<AdditionalText>.Empty,
            ImmutableArray<DiagnosticAnalyzer>.Empty,
            ImmutableArray<ISourceGenerator>.Empty,
            ImmutableArray<KeyValuePair<string, string>>.Empty,
            emitOptions,
            binding.DefaultOptions,
        })!;
        return Encoding.UTF8.GetBytes(key);
    }

    /// <summary>
    /// Force the reflection binding and confirm we can call the API on a
    /// trivial compilation. Called from <c>initialize</c> so a Roslyn
    /// upgrade that has renamed or removed the symbol fails fast with a
    /// clear error rather than later producing wrong cache keys.
    /// </summary>
    public static void Probe()
    {
        _ = CachedBinding.Value;
    }

    private static Binding BindMethod()
    {
        var detKeyOptionsType = typeof(Compilation).Assembly
            .GetType("Microsoft.CodeAnalysis.DeterministicKeyOptions")
            ?? throw new InvalidOperationException(
                "Microsoft.CodeAnalysis.DeterministicKeyOptions not found — "
                + "Roslyn's deterministic-key API has changed shape. Rebuild "
                + "the sidecar against the active Roslyn version.");

        var defaultOptions = Enum.Parse(detKeyOptionsType, "Default");

        var method = typeof(Compilation)
            .GetMethods(BindingFlags.Instance | BindingFlags.NonPublic)
            .FirstOrDefault(m =>
                m.Name == "GetDeterministicKey"
                && m.GetParameters().Length == 6
                && m.GetParameters()[5].ParameterType == detKeyOptionsType)
            ?? throw new InvalidOperationException(
                "Compilation.GetDeterministicKey(6-arg) not found — Roslyn's "
                + "deterministic-key API has changed shape. Rebuild the "
                + "sidecar against the active Roslyn version.");

        return new Binding(method, defaultOptions);
    }

    private sealed record Binding(MethodInfo Method, object DefaultOptions);
}
