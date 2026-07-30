namespace PreVisibleUnion

/// Compiled under `<LangVersion>8.0</LangVersion>`: fsc emits public `IsHeads` /
/// `IsTails` property rows carrying `[CompilerGenerated]`,
/// `[DebuggerNonUserCode]` and `[DebuggerBrowsable]` — the same attributes a
/// current compiler gives the *nameable* testers — but does not publish them as
/// members, so `coin.IsHeads` is `FS0039` for a consumer. The host pickle's
/// `tcaug.adhoc` is the only record of the difference.
type Coin =
    | Heads
    | Tails
