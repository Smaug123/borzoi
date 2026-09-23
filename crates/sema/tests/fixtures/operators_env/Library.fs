namespace OpsFixture

/// `(+)` over ints that returns a string: opened, `1 + 2` is a `string`.
module StringOps =
    let (+) (a: int) (b: int) : string = "sum"

/// `(-)` over ints that returns a string, auto-opened by the assembly-level
/// attribute below: with no `open` at all, `3 - 1` is a `string`.
module ManifestOps =
    let (-) (a: int) (b: int) : string = "diff"

[<assembly: AutoOpen("OpsFixture.ManifestOps")>]
do ()
