namespace RqaAsm

[<RequireQualifiedAccess>]
module Strict =
    let strictValue = 1

module Loose =
    let looseValue = 2

module Outer =
    [<RequireQualifiedAccess>]
    module InnerStrict =
        let innerStrictValue = 3

    module InnerLoose =
        let innerLooseValue = 4

// The `List` shape: a type and a same-named module, which the compiler
// suffixes (`BagModule`) and FCS still opens as `Bag`.
type Bag = { Items: int list }

[<RequireQualifiedAccess>]
module Bag =
    let empty = { Items = [] }

// A child of an `[<AutoOpen>]` module answers to its short name wherever
// `RqaAsm` is opened.
[<AutoOpen>]
module AutoHost =
    [<RequireQualifiedAccess>]
    module ViaAuto =
        let viaAutoValue = 5

[<RequireQualifiedAccess>]
module internal HiddenStrict =
    let hiddenValue = 6

namespace RqaAsm.Deep

[<RequireQualifiedAccess>]
module DeepStrict =
    let deepValue = 7

// A module in a namespace FSharp.Core auto-opens, from an assembly that does
// not: FCS opens FSharp.Core's fragment of the namespace alone, so the short
// name does not reach it.
namespace Microsoft.FSharp.Control

[<RequireQualifiedAccess>]
module RqaForeignControl =
    let foreignValue = 13
