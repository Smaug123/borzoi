using System.Text.RegularExpressions;

namespace SourceGenFixture;

/// <summary>
/// Triggers the in-box <c>System.Text.RegularExpressions</c> source
/// generator. The compiler/SG transforms the <c>partial</c> declaration
/// into a real method whose body returns a pre-compiled <see cref="Regex"/>
/// instance; for metadata-only emit the body is elided but the
/// declaration (signature, name, accessibility) must survive — that's
/// what the differential test pins.
///
/// We deliberately use <c>GeneratedRegex</c> rather than
/// <c>System.Text.Json</c>'s SG because the latter synthesises generic
/// methods (e.g. <c>TryGetTypeInfoForRuntimeCustomConverter&lt;T&gt;</c>),
/// which the phase-3a assembly reader doesn't yet support. Once method
/// generics land in the reader we can grow this fixture; for now the
/// regex SG exercises the property we care about (Roslyn runs generators
/// during <c>Project.GetCompilationAsync()</c>) without dragging in
/// reader features that aren't ready.
/// </summary>
public static partial class Validator
{
    [GeneratedRegex(@"^\d+$")]
    public static partial Regex DigitsOnly();
}
