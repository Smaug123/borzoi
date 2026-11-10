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

    // A module the manifest auto-open brings into bare scope: as a dotted
    // expression head it is searched before any type of the name
    // (`resolve_duplicate_case_names_diff`).
    module Shade =
        let A = 1

// A ROOT-namespace union of the same name. The root is the *lowest* reading
// tier, so this is the one the manifest auto-open can plausibly out-rank — the
// precedence the review's report turns on, decided by the oracle rather than
// asserted.
namespace global

type Target =
    | Carrier of int
    | Nullary
