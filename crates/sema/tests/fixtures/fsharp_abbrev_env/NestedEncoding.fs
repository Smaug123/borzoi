// The *nested* encoding of a module FQN — it needs its own file, since a file that opens
// with `namespace` cannot also declare a root module (a `module` at column 0 after a
// namespace silently becomes nested INSIDE it, which is exactly how the first attempt at
// this fixture went wrong).
//
// `NestEnc.Inner` here is a root module `NestEnc` with a nested module `Inner` (no
// namespace at all), while the sibling autoopen fixture exposes the same FQN the other
// way: a top-level module `Inner` in namespace `NestEnc`. FCS merges both encodings; a
// walk that stops at the first metadata split silently drops one (review round 7).
module NestEnc

module Inner =
    let fromNestedEncoding () = 81
