//! Phase-1 measure-expression decoder.
//!
//! Mirrors `u_measure_expr` (`TypedTreePickle.fs:2257-2278`) and the
//! `u_rational` helper (`:2253-2255`). Both `range0` arguments at each
//! `Measure` constructor are dropped — F# source locations are not part
//! of the cross-CCU view.
//!
//! ### Tag-value pin (re-verified against FCS source)
//!
//! | Tag | FCS variant            | Reads in order                             |
//! |-----|------------------------|--------------------------------------------|
//! | 0   | `Measure.Const`        | `u_tcref`                                  |
//! | 1   | `Measure.Inv`          | `u_measure_expr`                           |
//! | 2   | `Measure.Prod`         | `u_measure_expr`, `u_measure_expr`         |
//! | 3   | `Measure.Var`          | `u_tpref`                                  |
//! | 4   | `Measure.One`          | —                                          |
//! | 5   | `Measure.RationalPower`| `u_measure_expr`, `u_rational`             |

use crate::error::ImportError;
use crate::fsharp_pickle::model::Measure;
use crate::fsharp_pickle::osgn::PhaseOneState;
use crate::fsharp_pickle::reader::PickleReader;
use crate::fsharp_pickle::types::{read_tcref, read_tpref};

/// `u_rational = u_tup2 u_int32 u_int32` (`:2253-2255`). The first
/// component is the numerator, the second is the denominator. Both are
/// compressed signed integers via `u_int32`.
pub(crate) fn read_rational(reader: &mut PickleReader<'_>) -> Result<(i32, i32), ImportError> {
    let num = reader.read_int32("u_rational numerator")?;
    let den = reader.read_int32("u_rational denominator")?;
    Ok((num, den))
}

/// `u_measure_expr` — `:2257-2278`. See module doc for the per-tag
/// breakdown. Threads `PhaseOneState` because `Const`/`Var` recurse
/// through `u_tcref` / `u_tpref`, both of which validate their stamps
/// against the entity / typar OSGN tables (`:596-602`).
///
/// Depth-guarded: tag 1 (`Inv`) self-recurses after consuming a single
/// byte, so a malformed run of tag bytes drives one stack frame per
/// byte without the guard.
pub(crate) fn read_measure_expr(state: &mut PhaseOneState<'_>) -> Result<Measure, ImportError> {
    state.reader.enter_recursion("u_measure_expr")?;
    let result = read_measure_expr_body(state);
    state.reader.exit_recursion();
    result
}

fn read_measure_expr_body(state: &mut PhaseOneState<'_>) -> Result<Measure, ImportError> {
    let tag = state.reader.read_byte("u_measure_expr tag")?;
    match tag {
        0 => {
            let tcref = read_tcref(state)?;
            Ok(Measure::Const { tcref })
        }
        1 => {
            let inner = read_measure_expr(state)?;
            Ok(Measure::Inv(Box::new(inner)))
        }
        2 => {
            let a = read_measure_expr(state)?;
            let b = read_measure_expr(state)?;
            Ok(Measure::Prod(Box::new(a), Box::new(b)))
        }
        3 => {
            let typar_index = read_tpref(state)?;
            Ok(Measure::Var { typar_index })
        }
        4 => Ok(Measure::One),
        5 => {
            let base = read_measure_expr(state)?;
            let (num, den) = read_rational(&mut state.reader)?;
            Ok(Measure::RationalPower {
                base: Box::new(base),
                num,
                den,
            })
        }
        other => Err(ImportError::UnsupportedPickleTag {
            context: "u_measure_expr tag",
            tag: u32::from(other),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fsharp_pickle::model::PickledTcRef;

    /// Build a phase-1 state with capacities large enough that every
    /// fixture's `u_tcref Local` and `u_tpref` indices land inside the
    /// preallocated typar / entity OSGN tables.
    fn make_state(bytes: &[u8]) -> PhaseOneState<'_> {
        PhaseOneState::with_capacities(PickleReader::new(bytes), 16, 16, 0)
    }

    fn read_one(bytes: &[u8]) -> Result<Measure, ImportError> {
        let mut s = make_state(bytes);
        let m = read_measure_expr(&mut s)?;
        assert!(
            s.reader.is_eof(),
            "decoder should consume the whole fixture; {} bytes left",
            s.reader.remaining()
        );
        Ok(m)
    }

    #[test]
    fn tag_0_const() {
        // tag 0, then non-local tcref index 3 (tcref tag 1 + idx 3).
        let bytes = [0, 1, 3];
        let m = read_one(&bytes).unwrap();
        assert_eq!(
            m,
            Measure::Const {
                tcref: PickledTcRef::NonLocal(3),
            }
        );
    }

    #[test]
    fn tag_1_inv_recurses() {
        // tag 1, then Measure.One (tag 4).
        let bytes = [1, 4];
        let m = read_one(&bytes).unwrap();
        assert_eq!(m, Measure::Inv(Box::new(Measure::One)));
    }

    #[test]
    fn tag_2_prod_two_children() {
        // tag 2, then Var{0} (tag 3 + tpref idx 0), then One (tag 4)
        let bytes = [2, 3, 0, 4];
        let m = read_one(&bytes).unwrap();
        assert_eq!(
            m,
            Measure::Prod(
                Box::new(Measure::Var { typar_index: 0 }),
                Box::new(Measure::One),
            ),
        );
    }

    #[test]
    fn tag_3_var() {
        // tag 3, then tpref idx 7.
        let bytes = [3, 7];
        let m = read_one(&bytes).unwrap();
        assert_eq!(m, Measure::Var { typar_index: 7 });
    }

    #[test]
    fn tag_4_one() {
        let bytes = [4];
        let m = read_one(&bytes).unwrap();
        assert_eq!(m, Measure::One);
    }

    #[test]
    fn tag_5_rational_power_positive() {
        // tag 5, base = One, num = 3 (literal), den = 4 (literal)
        let bytes = [5, 4, 3, 4];
        let m = read_one(&bytes).unwrap();
        assert_eq!(
            m,
            Measure::RationalPower {
                base: Box::new(Measure::One),
                num: 3,
                den: 4,
            },
        );
    }

    #[test]
    fn tag_5_rational_power_negative_numerator() {
        // -1 as a compressed i32 takes the marker form: 0xFF followed by
        // 4 little-endian bytes for 0xFFFFFFFF.
        let mut bytes = vec![5, 4, 0xFF];
        bytes.extend_from_slice(&(-1i32).to_le_bytes());
        bytes.push(1); // den = 1
        let m = read_one(&bytes).unwrap();
        assert_eq!(
            m,
            Measure::RationalPower {
                base: Box::new(Measure::One),
                num: -1,
                den: 1,
            },
        );
    }

    #[test]
    fn unknown_tag_errors() {
        let bytes = [6];
        let mut s = make_state(&bytes);
        match read_measure_expr(&mut s) {
            Err(ImportError::UnsupportedPickleTag {
                context: "u_measure_expr tag",
                tag: 6,
            }) => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn tag_3_var_validates_against_typar_table() {
        // tag 3, then tpref idx 200 — exceeds the 16-slot typar table.
        let bytes = [3, 0x80, 200];
        let mut s = make_state(&bytes);
        match read_measure_expr(&mut s) {
            Err(ImportError::OsgnIndexOutOfRange {
                kind: "typars",
                index: 200,
                max: 16,
            }) => {}
            other => panic!("expected OsgnIndexOutOfRange for typars, got {other:?}"),
        }
    }
}
