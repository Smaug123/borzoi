namespace GridFix

/// A static of every probed name, declared directly.
type Holder =
    static member id(x: int) = x + 1
    static member gridV(x: int) = x + 1
    static member Some(x: int) = x + 1
    static member GridC(x: int) = x + 1

/// The same statics, one level up a class hierarchy.
type Base() =
    static member id(x: int) = x + 1
    static member gridV(x: int) = x + 1
    static member Some(x: int) = x + 1
    static member GridC(x: int) = x + 1

/// Inherits every static it carries.
type Derived() =
    inherit Base()

/// Statics the assembly projection drops: an indexed property whose index is
/// a byref has no model in `borzoi-assembly`.
type Dropped =
    static member id
        with get (x: inref<int>) = x + 1

    static member gridV
        with get (x: inref<int>) = x + 1

    static member Some
        with get (x: inref<int>) = x + 1

    static member GridC
        with get (x: inref<int>) = x + 1
