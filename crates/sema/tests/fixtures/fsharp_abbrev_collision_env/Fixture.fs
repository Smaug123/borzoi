namespace Lib

// A REAL class at the same FQN the main fixture exports as an abbreviation
// (`Lib.WidgetAlias`). Its `Make` static is what FCS would bind for a
// later-referenced assembly — the reason sema must decline resolve-through when
// this FQN collides across DLLs rather than commit the main fixture's target.
type WidgetAlias() =
    static member Make () = 99
