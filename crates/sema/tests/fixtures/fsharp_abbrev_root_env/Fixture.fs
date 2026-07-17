namespace global

// A real type at true global scope, plus the codex-round-4 motivating case:
// `namespace global` content has the empty namespace path — the ROOT ("as
// written, no open") reading. FCS lets a bare, unopened name bind to a
// global-namespace abbreviation with no `open` at all, so the resolver's
// ROOT tier needs the same shadow check as every opened/enclosing one.
type GlobalMarker = { Value: int }

type uint64 = string
