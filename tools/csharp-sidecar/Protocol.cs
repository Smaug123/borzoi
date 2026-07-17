namespace CSharpSidecar;

/// <summary>
/// JSON-RPC error codes used by the sidecar wire protocol. Values match the
/// reserved range defined by JSON-RPC 2.0 (§5.1) so generic clients can render
/// them sensibly. Sidecar-specific application errors live in the
/// "Server error" range (-32000 to -32099) and carry a typed
/// <see cref="ErrorData"/> payload so the Rust side can dispatch on the
/// <c>kind</c> rather than parsing free-form messages.
/// </summary>
internal static class ProtocolErrorCode
{
    public const int ParseError = -32700;
    public const int InvalidRequest = -32600;
    public const int MethodNotFound = -32601;
    public const int InvalidParams = -32602;
    /// <summary>
    /// All sidecar-defined application errors share this code. The
    /// <c>kind</c> field in <see cref="ErrorData"/> discriminates them.
    /// </summary>
    public const int SidecarError = -32000;
}

/// <summary>
/// String tags identifying sidecar-defined error kinds. Values are documented
/// in <c>docs/completed/csharp-sidecar-plan.md</c> D10 and kept in sync with the Rust
/// <c>SidecarErrorKind</c> enum.
/// </summary>
internal static class SidecarErrorKind
{
    /// <summary><c>buildMetadata</c> dispatched before <c>initialize</c>.</summary>
    public const string NotInitialized = "NotInitialized";
    /// <summary>Reserved for methods whose implementation is not in this phase yet.</summary>
    public const string NotImplemented = "NotImplemented";
    /// <summary>MSBuildLocator could not find a .NET SDK to bind to.</summary>
    public const string SdkUnavailable = "SdkUnavailable";
    /// <summary>The supplied csproj path does not exist.</summary>
    public const string CsprojNotFound = "CsprojNotFound";
    /// <summary>MSBuildWorkspace surfaced a load-time failure.</summary>
    public const string LoadFailed = "LoadFailed";
    /// <summary>
    /// Roslyn produced compiler diagnostics that prevent a successful emit.
    /// Per D8, we do not fall back to a stale cached DLL — the caller sees
    /// the diagnostics and no path.
    /// </summary>
    public const string BuildFailed = "BuildFailed";
    /// <summary>The sidecar could not create or write into the cache directory.</summary>
    public const string CacheUnwritable = "CacheUnwritable";
    /// <summary>
    /// The sidecar binary cannot bind to the Roslyn deterministic-key API it
    /// needs for cache-key derivation. Raised at <c>initialize</c> time via
    /// <see cref="RoslynDeterministicKey.Probe"/> so a Roslyn upgrade that
    /// renamed or removed the symbol fails fast — without it the sidecar
    /// would silently produce wrong cache keys.
    /// </summary>
    public const string IncompatibleRoslyn = "IncompatibleRoslyn";
    /// <summary>
    /// <c>projectTfms</c> did not contain an entry for a csproj the sidecar
    /// needs to load. Phase 4 of <c>docs/completed/multi-tfm-resolution-plan.md</c>
    /// hard-errors here (D5) rather than silently falling back to the
    /// consumer TFM: a missing entry is a Rust-side bug and we want it loud.
    /// The error payload carries <c>csprojPath</c> identifying which member
    /// of the closure was not found in the map.
    /// </summary>
    public const string MissingProjectTfm = "MissingProjectTfm";
}

internal sealed record InitializeParams(string? WorkspaceRoot, string? DotnetRoot);

internal sealed record InitializeResult(
    string ProtocolVersion,
    string RuntimeVersion,
    string? RoslynVersion);

/// <summary>
/// Parameters for the <c>buildMetadata</c> request. As of protocol
/// <c>0.4.0</c>, <c>ProjectTfms</c> is the closure-wide TFM map produced by
/// the Rust side (every csproj in the requested project's
/// <c>&lt;ProjectReference&gt;</c> closure, keyed to the short-form TFM
/// NuGet's restore selected for it). Phase 3 ships the field as required on
/// the wire but does not consume it yet — the sidecar still loads under
/// <c>TargetFramework</c>; per-project workspace construction lands in a
/// later phase. A <c>null</c> dictionary deserialises when the field is
/// absent, which is a protocol-version mismatch; an empty dictionary is the
/// degenerate-but-valid form a fresh client uses when it cannot resolve the
/// closure (e.g. one-off integration tests).
/// </summary>
internal sealed record BuildMetadataParams(
    string? CsprojPath,
    string? Configuration,
    string? TargetFramework,
    System.Collections.Generic.Dictionary<string, string>? ProjectTfms);

/// <summary>
/// Success-path response for <c>buildMetadata</c>. Phase 4 populates
/// <c>FromCache</c> from the cache lookup (true if the keyed DLL was already on
/// disk, false if we re-emitted), and <c>ContentHash</c> with the lowercase
/// hex rendering of the 32-byte SHA-256 cache key. Phase 5 populates
/// <c>TransitiveProjectRefs</c> with one entry per project in the requested
/// project's <c>&lt;ProjectReference&gt;</c> closure (sorted by csproj path
/// for wire stability).
/// </summary>
internal sealed record BuildMetadataResult(
    string MetadataDllPath,
    string ContentHash,
    bool FromCache,
    CompilerDiagnosticDto[] Diagnostics,
    TransitiveProjectRefDto[] TransitiveProjectRefs);

/// <summary>
/// One row of <c>BuildMetadataResult.TransitiveProjectRefs</c>. The csproj
/// path is one of the requested project's transitive
/// <c>&lt;ProjectReference&gt;</c> targets; the dll path is the metadata DLL
/// the sidecar emitted for it (lives inside the same cache root as the top
/// project's DLL). Phase 5 onward emits one of these per closure member;
/// earlier phases always emitted an empty list.
/// </summary>
internal sealed record TransitiveProjectRefDto(string CsprojPath, string MetadataDllPath);
