namespace Collision

/// The module-value fixture declares `Collision.Thing` too, with `P: string`.
type Thing = { P: int }

module B =
    let item: Thing = { P = 1 }
    let take (t: Thing) = t.P
