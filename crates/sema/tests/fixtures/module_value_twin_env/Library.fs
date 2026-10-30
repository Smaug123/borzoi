namespace Collision

/// The module-value fixture declares `Collision.Thing` too, with `P: string`.
type Thing = { P: int }

/// The module-value fixture declares `Collision.GThing<'T>` too, with
/// `P: string`.
type GThing<'T> = { P: int; T: 'T }

module B =
    let item: Thing = { P = 1 }
    let take (t: Thing) = t.P
    let gitem: GThing<int> = { P = 1; T = 1 }
    let gtake (t: GThing<int>) = t.P

namespace TwinOnly

/// A generic record and union only this assembly declares, so a use of the
/// functions over them reads them from the twin.
type TBox<'T> = { T: 'T }

type TU<'T> =
    | TA of 'T
    | TB

module TwinFns =
    let tboxVal = { T = 1 }
    let tbox (b: TBox<int>) = b.T
    let tboxG (x: 'a) = { T = x }
    let tuVal: TU<string> = TA "s"
    let tu (u: TU<string>) = 1
