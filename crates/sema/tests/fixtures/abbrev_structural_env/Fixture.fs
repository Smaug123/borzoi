namespace AbbrevStruct

/// A structural (tuple) abbreviation that takes FSharp.Core's `array` name.
type array<'T> = 'T * 'T

/// A function-typed abbreviation.
type endo<'T> = 'T -> 'T

module Holder =
    /// A structural abbreviation nested in a module.
    type pair<'T> = 'T * 'T

namespace AbbrevNominal

/// A nominal abbreviation that takes FSharp.Core's `array` name.
type array<'T> = 'T list
