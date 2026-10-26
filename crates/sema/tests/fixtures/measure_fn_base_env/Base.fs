namespace FSharpMeasureFnBase

#nowarn "42"

[<Measure>]
type bm

/// An abbreviation whose name says nothing about the measure it hides.
type BaseDist = float<bm>

/// The measure-free twin of `BaseDist`.
type BasePlain = float

/// The `FSharp.UMX` pattern: a measure on a type FSharp.Core never annotates.
[<MeasureAnnotatedAbbreviation>]
type BaseTagged<[<Measure>] 'u> = string

/// An abbreviation over the measure-annotated one, hiding its measure.
type BaseName = BaseTagged<bm>

/// A function abbreviation hiding a measure in its domain.
type BaseDistFn = float<bm> -> int

/// A generic abbreviation hiding a measure in its range.
type BaseToDist<'a> = 'a -> BaseDist
