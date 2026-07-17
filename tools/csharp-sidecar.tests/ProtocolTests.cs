namespace CSharpSidecar.Tests;

using System.Collections.Generic;
using System.Text.Json;
using System.Text.Json.Nodes;

using Xunit;

/// <summary>
/// Wire-shape tests for the JSON-RPC DTOs. Phase 3 of
/// <c>docs/completed/multi-tfm-resolution-plan.md</c> introduced
/// <c>BuildMetadataParams.ProjectTfms</c> as a required-on-the-wire
/// dictionary; pin its deserialisation here so a typo in
/// <see cref="BuildMetadataParams"/> or a stray
/// <see cref="JsonSerializerOptions"/> setting breaks the test rather than
/// silently corrupting a real run.
/// </summary>
public sealed class ProtocolTests
{
    private static readonly JsonSerializerOptions JsonOpts = new()
    {
        PropertyNamingPolicy = JsonNamingPolicy.CamelCase,
        WriteIndented = false,
    };

    [Fact]
    public void BuildMetadataParamsDeserialisesProjectTfms()
    {
        var node = JsonNode.Parse(
            "{\"csprojPath\":\"/repo/Top.csproj\","
            + "\"configuration\":\"Debug\","
            + "\"targetFramework\":\"net10.0\","
            + "\"projectTfms\":{"
            + "\"/repo/Top.csproj\":\"net10.0\","
            + "\"/repo/Lib.csproj\":\"net8.0\""
            + "}}");
        Assert.NotNull(node);

        var p = node!.Deserialize<BuildMetadataParams>(JsonOpts);
        Assert.NotNull(p);
        Assert.Equal("/repo/Top.csproj", p!.CsprojPath);
        Assert.Equal("Debug", p.Configuration);
        Assert.Equal("net10.0", p.TargetFramework);
        Assert.NotNull(p.ProjectTfms);
        Assert.Equal(2, p.ProjectTfms!.Count);
        Assert.Equal("net10.0", p.ProjectTfms["/repo/Top.csproj"]);
        Assert.Equal("net8.0", p.ProjectTfms["/repo/Lib.csproj"]);
    }

    /// <summary>
    /// A wire payload that omits <c>projectTfms</c> still parses — the
    /// dictionary lands as <c>null</c>. A real client never sends this shape
    /// (the protocol-version handshake stops a 0.3.0 client from talking to
    /// a 0.4.0 sidecar), but the record type permits it so a deserialisation
    /// failure does not mask the version-mismatch diagnostic the handshake
    /// is supposed to produce.
    /// </summary>
    [Fact]
    public void BuildMetadataParamsAcceptsAbsentProjectTfms()
    {
        var node = JsonNode.Parse(
            "{\"csprojPath\":\"/repo/Top.csproj\","
            + "\"configuration\":\"Debug\","
            + "\"targetFramework\":\"net10.0\"}");
        Assert.NotNull(node);

        var p = node!.Deserialize<BuildMetadataParams>(JsonOpts);
        Assert.NotNull(p);
        Assert.Null(p!.ProjectTfms);
    }

    /// <summary>
    /// Empty closure (e.g. ad-hoc integration test that hasn't resolved
    /// <c>project.assets.json</c>) parses to an empty dictionary, not to
    /// <c>null</c>.
    /// </summary>
    [Fact]
    public void BuildMetadataParamsDeserialisesEmptyProjectTfms()
    {
        var node = JsonNode.Parse(
            "{\"csprojPath\":\"/repo/Top.csproj\","
            + "\"configuration\":\"Debug\","
            + "\"targetFramework\":\"net10.0\","
            + "\"projectTfms\":{}}");
        Assert.NotNull(node);

        var p = node!.Deserialize<BuildMetadataParams>(JsonOpts);
        Assert.NotNull(p);
        Assert.NotNull(p!.ProjectTfms);
        Assert.Empty(p.ProjectTfms!);
    }
}
