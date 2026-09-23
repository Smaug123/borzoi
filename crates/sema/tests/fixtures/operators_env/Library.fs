namespace OpsFixture

/// `(+)` over ints that returns a string: opened, `1 + 2` is a `string`.
module StringOps =
    let (+) (a: int) (b: int) : string = "sum"
