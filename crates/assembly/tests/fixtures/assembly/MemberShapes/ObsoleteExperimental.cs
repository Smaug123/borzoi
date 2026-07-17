// `[Obsolete]` / `[Experimental]` payload decoding: the ctor-arg and
// named-arg shapes a C# compiler actually emits, projected onto
// `Entity::obsolete` / `Entity::experimental` (type position) and
// `MethodLike::obsolete` / `MethodLike::experimental` (method position).
// Each public member maps 1:1 to a happy-path test in
// `tests/all/projector_obsolete_experimental.rs`.
//
// Roslyn can only emit `ObsoleteAttribute` via its ctor overloads — `()`,
// `(string)`, `(string, bool)` — because `Message` / `IsError` are get-only
// properties, so the named-arg precedence paths have no compiler-producible
// fixture and stay pinned by the hand-built-blob unit tests in
// `crates/assembly/src/reader/attributes_tests.rs`. `ExperimentalAttribute`'s
// `UrlFormat` / `Message` are settable named properties, so its full
// payload matrix is reachable here; only the get-only `DiagnosticId`
// override is not.

using System;
using System.Diagnostics.CodeAnalysis;

namespace MemberShapes.ObsoleteExperimental;

// projects_obsolete_payload_shapes_bare: `[Obsolete]` → no ctor args →
// Obsolete { message: None, is_error: false }.
[Obsolete]
public class ObsoleteBare { }

// projects_obsolete_payload_shapes_warned: `[Obsolete("…")]` → (string) ctor
// → Obsolete { message: Some, is_error: false }.
[Obsolete("use V2 instead")]
public class ObsoleteWarned { }

// projects_obsolete_payload_shapes_errored: `[Obsolete("…", true)]` →
// (string, bool) ctor → Obsolete { message: Some, is_error: true }.
[Obsolete("gone", true)]
public class ObsoleteErrored { }

// projects_obsolete_on_method: obsolete lands on MethodLike::obsolete, the
// second `detect_obsolete_*` call site, not just Entity::obsolete.
public class ObsoleteOnMethod
{
    [Obsolete("use New")]
    public void Old() { }
}

// projects_experimental_payload_shapes_bare: `[Experimental("…")]` →
// (string) ctor only → diagnostic_id Some, url/message None.
[Experimental("DIAG001")]
public class ExperimentalBare { }

// projects_experimental_payload_shapes_with_url: ctor + `UrlFormat` named
// property → diagnostic_id + url_format, message None.
[Experimental("DIAG002", UrlFormat = "https://aka.ms/{0}")]
public class ExperimentalWithUrl { }

// projects_experimental_payload_shapes_with_message: ctor + `Message` named
// property → diagnostic_id + message, url_format None.
[Experimental("DIAG003", Message = "subject to change")]
public class ExperimentalWithMessage { }

// projects_experimental_payload_shapes_both: ctor + both named properties →
// all three fields Some.
[Experimental("DIAG004", UrlFormat = "u", Message = "m")]
public class ExperimentalBoth { }

// projects_experimental_on_method: experimental lands on
// MethodLike::experimental, the second `detect_experimental_*` call site.
public class ExperimentalOnMethod
{
    [Experimental("DIAG_M001")]
    public void Preview() { }
}
