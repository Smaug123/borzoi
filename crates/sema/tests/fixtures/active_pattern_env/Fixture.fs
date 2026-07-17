namespace Demo.ApShape

module Recognizers =
    // Total single-case WITH a parameter. FCS splits an applied use frontAndBack
    // (the last arg is always the result, everything before it a parameter),
    // arity-independently: `Scale factor v` makes `factor` a parameter (an outer
    // value) and `v` the result binder. This is the flagship Stage-3b behaviour —
    // totality + single-case come from the mangled `|Scale|` name; arity is
    // irrelevant to the total single-case split.
    let (|Scale|) (k: int) (n: int) = k * n

    // Total single-case, no parameter: `Wrapped v` binds `v` (the sole arg is the
    // result). Matches today's fabricate-a-binder behaviour — a regression guard.
    let (|Wrapped|) (n: int) = n

    // Partial single-case WITH a parameter. The compiled IL flattens tupled
    // parameter groups, so the metadata parameter count over-counts F#'s curried
    // arity (`params - 1` is unsound); no arity attaches, and the split declines
    // to today's behaviour.
    let (|DivBy|_|) (d: int) (n: int) = if n % d = 0 then Some () else None

    // Total multi-case: an applied parameterised use is FS0722-illegal, so the
    // split never fires.
    let (|Even|Odd|) (n: int) = if n % 2 = 0 then Even else Odd

namespace Demo.ApResidue

module Contested =
    // An `[<AutoOpen>]` TYPE folds name-unknown residue at the tycon tier
    // (`CanAutoOpenTyconRef` statics we cannot enumerate), so opening this module
    // demotes its cases — the `Scale` recognizer's tag then defers with the group,
    // and its shape must NOT drive the use-site split (which would wrongly treat a
    // leading argument as a parameter where a hidden name could shadow the case).
    [<AutoOpen>]
    type Marker() =
        static member M() = 1

    let (|Scale|) (k: int) (n: int) = k * n

namespace Demo.ApLiteral

// A total single-case recognizer `(|Scale|)` and a same-named `[<Literal>]` value
// in ONE module. In pattern position FCS's latest-wins puts the literal (a
// constant pattern) in charge of `Scale`, so the recognizer's shape must NOT
// drive an applied use's argument split — `case_reference` skips the literal as
// an ordinary value and would otherwise reach the recognizer. Guards codex
// round 4c: the fold drops the shape when a same-named constant-pattern value is
// present.
module Shadowed =
    let (|Scale|) (k: int) (n: int) = k * n

    [<Literal>]
    let Scale = 7

// A plain `[<Literal>]` with NO same-named recognizer: fsc emits it as a CLI
// `Literal`-flagged static field, so `value_may_be_constant_pattern` is
// *certainly* true. Folded in by an `open`, it contests an earlier project
// case of the same name in the pattern namespace (a constant pattern,
// latest-wins) — the case must defer, not commit.
module Consts =
    [<Literal>]
    let Marker = 3
