//! Decoding of PostgreSQL's `NUMERIC` wire form at the scale the server stored.
//!
//! `numeric_send` in PostgreSQL's src/backend/utils/adt/numeric.c writes four
//! big-endian 2-byte header fields — ndigits, weight, sign, dscale — ahead of the
//! base-10000 digit groups. Only dscale, the count of digits after the point, is
//! read here: the digit groups carry four digits at a time and so cannot express a
//! display scale that is not a multiple of four.

use sqlx::{
    Decode, Type,
    error::BoxDynError,
    postgres::{PgTypeInfo, PgValueFormat, PgValueRef, Postgres},
    types::BigDecimal,
};

/// A PostgreSQL `NUMERIC` decoded at the precision and scale the server stored.
///
/// Neither stock sqlx decoder reproduces the value: `rust_decimal::Decimal` rounds away
/// everything past its 96-bit mantissa (~28 significant digits), and `BigDecimal`
/// recomputes the scale from the number of base-10000 digit groups, so its scale is
/// always a multiple of four — `NUMERIC(10,2)` holding `1234.56` renders `1234.5600`.
/// This type keeps `BigDecimal`'s unbounded digits and restores the display scale the
/// server transmitted alongside them.
pub(super) struct PgNumeric(pub(super) BigDecimal);

impl Type<Postgres> for PgNumeric {
    fn type_info() -> PgTypeInfo {
        <BigDecimal as Type<Postgres>>::type_info()
    }

    fn compatible(ty: &PgTypeInfo) -> bool {
        <BigDecimal as Type<Postgres>>::compatible(ty)
    }
}

impl<'r> Decode<'r, Postgres> for PgNumeric {
    fn decode(value: PgValueRef<'r>) -> Result<Self, BoxDynError> {
        let dscale = match value.format() {
            // dscale is the fourth of the four 2-byte header fields, so bytes 6..8.
            PgValueFormat::Binary => {
                let field: [u8; 2] = value
                    .as_bytes()?
                    .get(6..8)
                    .and_then(|bytes| bytes.try_into().ok())
                    .ok_or("PostgreSQL NUMERIC value is shorter than its 8-byte header")?;
                Some(i64::from(u16::from_be_bytes(field)))
            }
            // The text form spells the value out in ASCII, so the scale sqlx parses from
            // it is already the stored one.
            PgValueFormat::Text => None,
        };
        let decoded = <BigDecimal as Decode<'r, Postgres>>::decode(value)?;

        // Shrinking is safe because the server rounds a value to its dscale before storing
        // it, so the digits `with_scale` truncates off the final base-10000 group are zero
        // padding; growing restores trailing zeros the group encoding never carried. A
        // scale that already matches needs neither, and `with_scale` still clones the
        // digits, so the guard keeps that allocation out of the per-cell path.
        Ok(Self(match dscale {
            Some(scale) if decoded.fractional_digit_count() != scale => decoded.with_scale(scale),
            _ => decoded,
        }))
    }
}
