namespace CSharpSidecar;

using System.Reflection;
using System.Runtime.CompilerServices;
using System.Text;
using System.Text.Json;
using System.Text.Json.Nodes;

using Microsoft.Build.Locator;

/// <summary>
/// Phase 4 sidecar: a JSON-RPC-over-stdio loop with <c>initialize</c>,
/// <c>buildMetadata</c>, and <c>shutdown</c>. <c>buildMetadata</c> loads the
/// requested csproj via <see cref="Microsoft.CodeAnalysis.MSBuild.MSBuildWorkspace"/>,
/// derives a SHA-256 content-addressed cache key over the load's inputs
/// (see <see cref="Cache"/>), and either returns a cached metadata DLL or
/// drives <see cref="Microsoft.CodeAnalysis.Compilation.Emit(System.IO.Stream, Microsoft.CodeAnalysis.Emit.EmitOptions)"/>
/// with <c>EmitMetadataOnly = true</c> and publishes the resulting DLL
/// atomically inside the workspace's <c>obj/borzoi/csharp-sidecar/</c>
/// directory under a <c>&lt;prefix&gt;/&lt;hash&gt;.dll</c> path. See
/// <c>docs/completed/csharp-sidecar-plan.md</c> for the design.
/// </summary>
internal static class Program
{
    /// <summary>
    /// Wire-protocol version. Bumped whenever the request/response shapes
    /// change in a way that is not strictly additive. The Rust client checks
    /// for an exact match — mismatches are fatal (see the protocol-versioning
    /// risk in <c>docs/completed/csharp-sidecar-plan.md</c>). 0.3.0 introduced the
    /// required <c>contentHash</c> field on the <c>buildMetadata</c>
    /// success response (phase 4); 0.4.0 introduces the required
    /// <c>projectTfms</c> field on <c>BuildMetadataParams</c>
    /// (phase 3 of <c>docs/completed/multi-tfm-resolution-plan.md</c>) — the sidecar
    /// deserialises it but does not yet consume it.
    /// </summary>
    private const string ProtocolVersion = "0.4.0";

    /// <summary>
    /// Cache root, relative to <see cref="_workspaceRoot"/>. The plan
    /// (<c>docs/completed/csharp-sidecar-plan.md</c> D6) pins this path so users'
    /// existing <c>.gitignore</c> (which excludes <c>obj/</c>) already covers
    /// the sidecar's output.
    /// </summary>
    private const string CacheRootSubdir = "obj/borzoi/csharp-sidecar";

    private static readonly JsonSerializerOptions JsonOptions = new()
    {
        PropertyNamingPolicy = JsonNamingPolicy.CamelCase,
        WriteIndented = false,
    };

    /// <summary>Tracks initialize state; <c>buildMetadata</c> rejects requests
    /// dispatched before <c>initialize</c> completes so the failure mode is
    /// "structured error" rather than "first MSBuild call blows up".</summary>
    private static bool _initialized;

    /// <summary>Workspace root captured at <c>initialize</c> time, used as the
    /// prefix for the cache directory the sidecar publishes DLLs into.</summary>
    private static string? _workspaceRoot;

    /// <summary>Outcome of the one-shot <see cref="MSBuildLocator"/> registration.</summary>
    private static SdkRegistration _sdk = SdkRegistration.NotAttempted;

    /// <summary>Lazily-created on first <c>buildMetadata</c>. The JIT only
    /// resolves <see cref="BuildService"/>'s MSBuild references when this
    /// field is first written, which must happen after
    /// <see cref="EnsureSdkRegistered"/> succeeds.</summary>
    private static BuildService? _buildService;

    public static int Main()
    {
        using var stdin = Console.OpenStandardInput();
        using var stdout = Console.OpenStandardOutput();

        while (true)
        {
            byte[]? messageBody;
            try
            {
                messageBody = ReadMessage(stdin);
            }
            catch (InvalidDataException ex)
            {
                WriteErrorResponse(stdout, id: null, ProtocolErrorCode.InvalidRequest, ex.Message);
                continue;
            }

            if (messageBody is null)
            {
                // Clean EOF on stdin. The peer dropped without a shutdown — exit
                // non-zero so the caller's wait() surfaces the abnormal close.
                return 1;
            }

            JsonObject request;
            try
            {
                request = JsonNode.Parse(messageBody) as JsonObject
                    ?? throw new JsonException("Request must be a JSON object");
            }
            catch (JsonException ex)
            {
                WriteErrorResponse(stdout, id: null, ProtocolErrorCode.ParseError, ex.Message);
                continue;
            }

            // The id may be a number, a string, or null. We pass it back
            // verbatim — JSON-RPC §4 lets the client choose the shape.
            JsonNode? idNode = request["id"];
            JsonNode? paramsNode = request["params"];

            // `method` must be a JSON string. A naive `GetValue<string>()`
            // would throw InvalidOperationException for non-string values
            // (numbers, arrays, etc.) and kill the loop; check the node
            // kind first and report InvalidRequest instead.
            string? method = null;
            if (request["method"] is JsonValue methodValue)
            {
                methodValue.TryGetValue<string>(out method);
            }

            if (method is null)
            {
                WriteErrorResponse(stdout, idNode, ProtocolErrorCode.InvalidRequest, "Missing or non-string 'method'");
                continue;
            }

            switch (method)
            {
                case "initialize":
                    HandleInitialize(stdout, idNode, paramsNode);
                    break;

                case "buildMetadata":
                    HandleBuildMetadata(stdout, idNode, paramsNode);
                    break;

                case "shutdown":
                    WriteResultResponse(stdout, idNode, result: null);
                    stdout.Flush();
                    return 0;

                default:
                    WriteErrorResponse(
                        stdout,
                        idNode,
                        ProtocolErrorCode.MethodNotFound,
                        $"Method not found: {method}");
                    break;
            }
        }
    }

    private static void HandleInitialize(Stream stdout, JsonNode? idNode, JsonNode? paramsNode)
    {
        InitializeParams? p;
        try
        {
            p = paramsNode?.Deserialize<InitializeParams>(JsonOptions);
        }
        catch (JsonException ex)
        {
            WriteErrorResponse(stdout, idNode, ProtocolErrorCode.InvalidParams, ex.Message);
            return;
        }

        if (p is null
            || string.IsNullOrEmpty(p.WorkspaceRoot)
            || string.IsNullOrEmpty(p.DotnetRoot))
        {
            WriteErrorResponse(
                stdout,
                idNode,
                ProtocolErrorCode.InvalidParams,
                "initialize requires non-empty workspaceRoot and dotnetRoot");
            return;
        }

        // Bind to Roslyn's internal deterministic-key API now. If reflection
        // can't find the method (Roslyn upgrade renamed/removed it), fail
        // loudly here rather than at first buildMetadata where the failure
        // would otherwise present as a wrong cache key. The handshake stays
        // un-completed so any subsequent buildMetadata returns NotInitialized.
        try
        {
            RoslynDeterministicKey.Probe();
        }
        catch (Exception ex)
        {
            WriteSidecarError(stdout, idNode, SidecarErrorKind.IncompatibleRoslyn,
                $"Sidecar cannot bind to Roslyn's deterministic-key API; rebuild against the active Roslyn version: {ex.Message}",
                data: null);
            return;
        }

        // We populate roslynVersion eagerly: the Microsoft.CodeAnalysis.CSharp
        // assembly does NOT depend on Microsoft.Build.*, so reading its
        // version here is safe even before MSBuildLocator runs (which we defer
        // until the first buildMetadata).
        var result = new InitializeResult(
            ProtocolVersion: ProtocolVersion,
            RuntimeVersion: Environment.Version.ToString(),
            RoslynVersion: ReadRoslynVersion());
        WriteResultResponse(stdout, idNode, result);
        _workspaceRoot = p.WorkspaceRoot;
        _initialized = true;
    }

    private static void HandleBuildMetadata(Stream stdout, JsonNode? idNode, JsonNode? paramsNode)
    {
        if (!_initialized)
        {
            WriteSidecarError(stdout, idNode, SidecarErrorKind.NotInitialized,
                "initialize must complete before buildMetadata", data: null);
            return;
        }

        BuildMetadataParams? p;
        try
        {
            p = paramsNode?.Deserialize<BuildMetadataParams>(JsonOptions);
        }
        catch (JsonException ex)
        {
            WriteErrorResponse(stdout, idNode, ProtocolErrorCode.InvalidParams, ex.Message);
            return;
        }

        if (p is null || string.IsNullOrEmpty(p.CsprojPath))
        {
            WriteErrorResponse(stdout, idNode, ProtocolErrorCode.InvalidParams,
                "buildMetadata requires a non-empty csprojPath");
            return;
        }
        if (string.IsNullOrEmpty(p.Configuration) || string.IsNullOrEmpty(p.TargetFramework))
        {
            // Configuration and TargetFramework become MSBuild global properties
            // during workspace creation; without them, MSBuild would silently
            // fall back to its defaults, which is not what the caller asked for.
            WriteErrorResponse(stdout, idNode, ProtocolErrorCode.InvalidParams,
                "buildMetadata requires non-empty configuration and targetFramework");
            return;
        }

        if (p.ProjectTfms is null)
        {
            // Protocol 0.4.0 makes `projectTfms` a required-on-the-wire field
            // (see docs/completed/multi-tfm-resolution-plan.md, phase 3). An absent
            // dictionary deserialises to null and is treated as a wire
            // protocol violation rather than silently substituting an empty
            // map — the version handshake should have stopped a 0.3.0 caller
            // before this point. We surface InvalidParams to make the cause
            // obvious in the response.
            WriteErrorResponse(stdout, idNode, ProtocolErrorCode.InvalidParams,
                "buildMetadata requires a projectTfms map (protocol 0.4.0+)");
            return;
        }

        if (!EnsureSdkRegistered(stdout, idNode))
        {
            return;
        }

        DispatchBuildMetadataLoad(stdout, idNode, p.CsprojPath, p.Configuration, p.TargetFramework, p.ProjectTfms);
    }

    /// <summary>
    /// Run <see cref="MSBuildLocator.RegisterDefaults"/> exactly once, latched
    /// in <see cref="_sdk"/>. If registration fails (no SDK on PATH or some
    /// other Locator failure) we report a structured <c>SdkUnavailable</c>
    /// error and never retry — Locator's contract is that registration is a
    /// one-shot process-global action.
    /// </summary>
    private static bool EnsureSdkRegistered(Stream stdout, JsonNode? idNode)
    {
        switch (_sdk)
        {
            case SdkRegistration.Succeeded:
                return true;
            case SdkRegistration.Failed:
                WriteSidecarError(stdout, idNode, SidecarErrorKind.SdkUnavailable,
                    "MSBuildLocator could not locate a .NET SDK; install the SDK referenced by the workspace",
                    data: null);
                return false;
        }

        try
        {
            MSBuildLocator.RegisterDefaults();
            _sdk = SdkRegistration.Succeeded;
            return true;
        }
        catch (Exception ex)
        {
            _sdk = SdkRegistration.Failed;
            WriteSidecarError(stdout, idNode, SidecarErrorKind.SdkUnavailable,
                $"MSBuildLocator.RegisterDefaults failed: {ex.Message}",
                data: null);
            return false;
        }
    }

    /// <summary>
    /// Drive the actual load + emit. Factored into its own method so the JIT
    /// only resolves <see cref="BuildService"/> (and transitively, the MSBuild
    /// assemblies) when control reaches this body — which is strictly after
    /// <see cref="EnsureSdkRegistered"/> has run.
    /// </summary>
    [MethodImpl(MethodImplOptions.NoInlining)]
    private static void DispatchBuildMetadataLoad(
        Stream stdout,
        JsonNode? idNode,
        string csprojPath,
        string configuration,
        string targetFramework,
        IReadOnlyDictionary<string, string> projectTfms)
    {
        // _workspaceRoot is non-null on this path: HandleBuildMetadata gates on
        // _initialized, and HandleInitialize only flips that after validating
        // and storing _workspaceRoot.
        var cacheRoot = Path.Combine(_workspaceRoot!, CacheRootSubdir);

        _buildService ??= BuildService.Create();
        var outcome = _buildService.BuildMetadata(csprojPath, configuration, targetFramework, projectTfms, cacheRoot);
        switch (outcome)
        {
            case BuildMetadataOutcome.CsprojNotFound nf:
                WriteSidecarError(stdout, idNode, SidecarErrorKind.CsprojNotFound,
                    $"csproj not found: {nf.Path}",
                    data: new JsonObject { ["csprojPath"] = nf.Path });
                break;
            case BuildMetadataOutcome.LoadFailedOutcome failed:
                WriteSidecarError(stdout, idNode, SidecarErrorKind.LoadFailed,
                    "MSBuildWorkspace reported a load failure",
                    data: new JsonObject
                    {
                        ["diagnostics"] = JsonSerializer.SerializeToNode(failed.Diagnostics, JsonOptions),
                    });
                break;
            case BuildMetadataOutcome.EmitFailed emit:
                // D8: no stale-cache fallback. Surface diagnostics, no path.
                WriteSidecarError(stdout, idNode, SidecarErrorKind.BuildFailed,
                    "Roslyn emit failed; see diagnostics",
                    data: new JsonObject
                    {
                        ["diagnostics"] = JsonSerializer.SerializeToNode(emit.CompilerDiagnostics, JsonOptions),
                        ["workspaceDiagnostics"] = JsonSerializer.SerializeToNode(emit.WorkspaceDiagnostics, JsonOptions),
                    });
                break;
            case BuildMetadataOutcome.CacheUnwritableOutcome cache:
                WriteSidecarError(stdout, idNode, SidecarErrorKind.CacheUnwritable,
                    $"cache directory unwritable: {cache.Detail}",
                    data: new JsonObject
                    {
                        ["cachePath"] = cache.Path,
                        ["detail"] = cache.Detail,
                    });
                break;
            case BuildMetadataOutcome.MissingProjectTfmOutcome missing:
                WriteSidecarError(stdout, idNode, SidecarErrorKind.MissingProjectTfm,
                    $"projectTfms has no entry for {missing.CsprojPath}",
                    data: new JsonObject { ["csprojPath"] = missing.CsprojPath });
                break;
            case BuildMetadataOutcome.Built built:
                var result = new BuildMetadataResult(
                    MetadataDllPath: built.MetadataDllPath,
                    ContentHash: Cache.ToHexLower(built.ContentHash),
                    FromCache: built.FromCache,
                    Diagnostics: built.CompilerDiagnostics,
                    TransitiveProjectRefs: built.TransitiveProjectRefs);
                WriteResultResponse(stdout, idNode, result);
                break;
        }
    }

    /// <summary>
    /// Reads the version of the Roslyn C# compiler assembly currently loaded.
    /// Bound by name to avoid <c>typeof()</c> against a Roslyn type from the
    /// JIT's perspective — we already reference Roslyn elsewhere, but this
    /// keeps the dependency explicit.
    /// </summary>
    private static string ReadRoslynVersion()
    {
        var asm = typeof(Microsoft.CodeAnalysis.CSharp.CSharpCompilationOptions).Assembly;
        // AssemblyInformationalVersionAttribute usually carries the public
        // marketing version (e.g. "5.3.0-1.24463.5"); fall back to the
        // assembly version if absent.
        var info = asm.GetCustomAttribute<AssemblyInformationalVersionAttribute>()?.InformationalVersion;
        return info ?? asm.GetName().Version?.ToString() ?? "unknown";
    }

    /// <summary>
    /// Reads a single LSP-style framed message from <paramref name="stream"/>.
    /// Returns <c>null</c> on a clean EOF (no bytes consumed since the last
    /// message), or throws <see cref="InvalidDataException"/> if the header
    /// is malformed mid-message.
    /// </summary>
    private static byte[]? ReadMessage(Stream stream)
    {
        // Read header bytes until CRLF CRLF.
        var headerBuf = new MemoryStream();
        int crlfState = 0; // counts characters matched against "\r\n\r\n"
        while (crlfState < 4)
        {
            int b = stream.ReadByte();
            if (b == -1)
            {
                if (headerBuf.Length == 0)
                {
                    // Clean EOF between messages.
                    return null;
                }
                throw new InvalidDataException("EOF inside message header");
            }
            headerBuf.WriteByte((byte)b);
            crlfState = (crlfState, (char)b) switch
            {
                (0, '\r') => 1,
                (1, '\n') => 2,
                (2, '\r') => 3,
                (3, '\n') => 4,
                (_, '\r') => 1,
                _ => 0,
            };
        }

        // Headers are ASCII per LSP convention.
        var headerText = Encoding.ASCII.GetString(headerBuf.ToArray());
        int contentLength = -1;
        foreach (var rawLine in headerText.Split("\r\n"))
        {
            if (rawLine.Length == 0) continue;
            int colon = rawLine.IndexOf(':');
            if (colon < 0)
            {
                throw new InvalidDataException($"Malformed header line: {rawLine}");
            }
            var name = rawLine[..colon].Trim();
            var value = rawLine[(colon + 1)..].Trim();
            if (name.Equals("Content-Length", StringComparison.OrdinalIgnoreCase))
            {
                if (!int.TryParse(value, out contentLength) || contentLength < 0)
                {
                    throw new InvalidDataException($"Bad Content-Length: {value}");
                }
            }
            // Content-Type / other headers ignored.
        }

        if (contentLength < 0)
        {
            throw new InvalidDataException("Missing Content-Length header");
        }

        var body = new byte[contentLength];
        int read = 0;
        while (read < contentLength)
        {
            int n = stream.Read(body, read, contentLength - read);
            if (n == 0)
            {
                throw new InvalidDataException("EOF inside message body");
            }
            read += n;
        }
        return body;
    }

    private static void WriteMessage(Stream stream, byte[] body)
    {
        var header = Encoding.ASCII.GetBytes($"Content-Length: {body.Length}\r\n\r\n");
        stream.Write(header, 0, header.Length);
        stream.Write(body, 0, body.Length);
        stream.Flush();
    }

    private static void WriteResultResponse(Stream stream, JsonNode? id, object? result)
    {
        var response = new JsonObject
        {
            ["jsonrpc"] = "2.0",
            ["id"] = id?.DeepClone(),
            ["result"] = result is null
                ? null
                : JsonSerializer.SerializeToNode(result, JsonOptions),
        };
        WriteMessage(stream, JsonSerializer.SerializeToUtf8Bytes(response, JsonOptions));
    }

    private static void WriteErrorResponse(Stream stream, JsonNode? id, int code, string message)
    {
        var response = new JsonObject
        {
            ["jsonrpc"] = "2.0",
            ["id"] = id?.DeepClone(),
            ["error"] = new JsonObject
            {
                ["code"] = code,
                ["message"] = message,
            },
        };
        WriteMessage(stream, JsonSerializer.SerializeToUtf8Bytes(response, JsonOptions));
    }

    /// <summary>
    /// Emits a JSON-RPC error response carrying a typed sidecar-specific
    /// <c>kind</c> in <c>data</c>. The Rust client parses this into its own
    /// <c>SidecarErrorKind</c> enum, so the wire shape here is load-bearing.
    /// </summary>
    private static void WriteSidecarError(Stream stream, JsonNode? id, string kind, string message, JsonNode? data)
    {
        var dataNode = new JsonObject { ["kind"] = kind };
        if (data is JsonObject obj)
        {
            foreach (var kv in obj)
            {
                dataNode[kv.Key] = kv.Value?.DeepClone();
            }
        }

        var response = new JsonObject
        {
            ["jsonrpc"] = "2.0",
            ["id"] = id?.DeepClone(),
            ["error"] = new JsonObject
            {
                ["code"] = ProtocolErrorCode.SidecarError,
                ["message"] = message,
                ["data"] = dataNode,
            },
        };
        WriteMessage(stream, JsonSerializer.SerializeToUtf8Bytes(response, JsonOptions));
    }

    private enum SdkRegistration
    {
        NotAttempted,
        Succeeded,
        Failed,
    }
}
