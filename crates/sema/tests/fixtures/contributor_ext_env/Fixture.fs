namespace GridFixExt

/// No probed statics of its own: `PlainExt` adds them as extension methods,
/// in scope wherever `GridFixExt` is opened.
type Plain2 =
    static member Marker = 0

[<AutoOpen>]
module PlainExt =
    type Plain2 with
        static member id(x: int) = x + 1
        static member gridV(x: int) = x + 1
        static member Some(x: int) = x + 1
        static member GridC(x: int) = x + 1

/// No probed statics of its own: `Plain3Ext` adds them as extension
/// *properties*, which compile to `Plain3.get_X.Static`.
type Plain3 =
    static member Marker = 0

[<AutoOpen>]
module Plain3Ext =
    type Plain3 with
        static member id = 42
        static member gridV = 42
        static member Some = 42
        static member GridC = 42

/// No probed statics of its own: `Plain4Ext` adds them under compiled names
/// that name another type.
type Plain4 =
    static member Marker = 0

[<AutoOpen>]
module Plain4Ext =
    type Plain4 with
        [<CompiledName("Renamed.id")>]
        static member id(x: int) = x + 1

        [<CompiledName("Renamed.gridV")>]
        static member gridV(x: int) = x + 1

        [<CompiledName("Renamed.Some")>]
        static member Some(x: int) = x + 1

        [<CompiledName("Renamed.GridC")>]
        static member GridC(x: int) = x + 1

/// No probed statics of its own: `Plain5Ext` adds them as extensions the
/// projection drops (an indexed property whose index is a byref).
type Plain5 =
    static member Marker = 0

[<AutoOpen>]
module Plain5Ext =
    type Plain5 with
        static member id
            with get (x: inref<int>) = x + 1

        static member gridV
            with get (x: inref<int>) = x + 1

        static member Some
            with get (x: inref<int>) = x + 1

        static member GridC
            with get (x: inref<int>) = x + 1
