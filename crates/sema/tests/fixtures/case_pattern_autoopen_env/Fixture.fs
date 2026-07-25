// See the `.fsproj` comment. `Target` collides with the main fixture's
// `Cases.Union.Target`, so with both DLLs referenced a reading the prefix walk
// cannot see sits above one it can.
//
// The auto-open must be a **manifest** one — `[<assembly: AutoOpen(…)>]` in
// `AssemblyInfo.fs`. `[<AutoOpen>]` on the module itself only means "opening the
// enclosing namespace also opens this module", which brings nothing into a
// consumer's bare scope (FCS-pinned: the control probe in
// `resolve_case_pattern_gen_diff` reported no binding at all for that shape).
namespace Cases.Retained

module Auto =
    type Target =
        | Carrier of int
        | Nullary

// A ROOT-namespace union of the same name. The root is the *lowest* reading
// tier, so this is the one the manifest auto-open can plausibly out-rank — the
// precedence the review's report turns on, decided by the oracle rather than
// asserted.
namespace global

type Target =
    | Carrier of int
    | Nullary
