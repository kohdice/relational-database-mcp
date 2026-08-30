//! Decoding of database row cells into their string representation.
//!
//! Each SQL type is mapped to exactly one Rust decode target, selected by the type
//! name sqlx reports for the value. The mapping is per driver because the drivers
//! disagree about which Rust types a SQL type accepts:
//!
//! - PostgreSQL compares type OIDs for equality, so `i64` decodes `INT8` and
//!   nothing else — `INT4` needs `i32`.
//! - MySQL rejects `UNSIGNED` columns for the signed integer types but accepts
//!   them for `bool`, so an unsigned column must name an unsigned Rust type.
//!
//! Trying types in a fixed order until one succeeds cannot honour those rules: it
//! reports whichever type happens to match first (an `INT UNSIGNED` of 42 decoded
//! as `true`), and it pays a failed decode — two string allocations inside sqlx —
//! for every column of every row.

mod pg_numeric;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use sqlx::{
    Column, Row, TypeInfo, ValueRef,
    mysql::types::MySqlTime,
    postgres::PgTypeInfo,
    types::{BigDecimal, JsonValue, Uuid},
};

use pg_numeric::PgNumeric;

/// Prefix marking a value that was base64-encoded because it is not valid UTF-8.
pub(crate) const BINARY_PREFIX: &str = "base64:";

/// Renders raw bytes as text: valid UTF-8 verbatim, anything else as base64.
///
/// The base64 form is prefixed with [`BINARY_PREFIX`] and is reversible, unlike a
/// summary such as `<binary data: N bytes>`, which discards the value outright and
/// is indistinguishable from a text column that happens to hold that same string.
pub(crate) fn bytes_to_string(bytes: Vec<u8>) -> String {
    String::from_utf8(bytes)
        .unwrap_or_else(|e| format!("{BINARY_PREFIX}{}", STANDARD.encode(e.into_bytes())))
}

/// Renders a decimal with every digit of the scale it carries.
///
/// `BigDecimal`'s own `Display` takes two shortcuts that rewrite the stored value:
/// it switches to exponential notation past five leading zeros (`0.0000001` prints
/// as `1E-7`) and it drops the scale of zero (`0.00` prints as `0`). Naming an
/// explicit precision disables both, because `bigdecimal` only reaches either
/// shortcut when the formatter carries no precision, and pads out to the precision
/// it is given otherwise.
///
/// A negative scale means the digits end above the decimal point (`1E+2`), so there
/// is no fractional part to print and precision `0` is the faithful rendering.
fn decimal_to_string(value: &BigDecimal) -> String {
    let scale = usize::try_from(value.fractional_digit_count()).unwrap_or(0);
    format!("{value:.scale$}")
}

/// Renders a MySQL `TIME` in the text form the server itself prints.
///
/// `MySqlTime`'s `Display` is not that form. It leaves the hours unpadded, and — called
/// without a precision, as `to_string` does — it strips the trailing zeros of the
/// fractional part and omits the fraction altogether when it is zero. A `TIME(6)`
/// holding `00:00:00.500000` prints as `0:00:00.5`, and one holding `00:00:00.000000`
/// as `0:00:00`.
///
/// The hours pad to a *minimum* of two digits, never a fixed two: the range reaches
/// `838:59:59`. A nonzero fraction is padded to six digits, which is exact for `TIME(6)`.
/// The column's declared fractional precision is not part of the row metadata, so it
/// cannot be honoured in general: a `TIME(3)` holding `.500` still renders `.500000`, and
/// a stored zero fraction — indistinguishable here from a `TIME(0)` value — is dropped
/// rather than guessed, leaving `00:00:00` where a `TIME(6)` column prints
/// `00:00:00.000000`. Those two are the only remaining departures.
///
/// The sign is read from `is_positive`, not `is_negative`: in sqlx 0.9.0 the latter
/// returns `self.sign.is_positive()` and so answers the opposite of its name.
fn mysql_time_to_string(value: &MySqlTime) -> String {
    let sign = if value.is_positive() { "" } else { "-" };
    let (hours, minutes, seconds) = (value.hours(), value.minutes(), value.seconds());
    match value.microseconds() {
        0 => format!("{sign}{hours:02}:{minutes:02}:{seconds:02}"),
        micros => format!("{sign}{hours:02}:{minutes:02}:{seconds:02}.{micros:06}"),
    }
}

/// Decodes `$row`'s cell at `$index` as `$ty` and renders it with [`Display`](std::fmt::Display).
macro_rules! render {
    ($row:expr, $index:expr, $ty:ty) => {
        $row.try_get::<$ty, _>($index)?.to_string()
    };
}

/// Builds the error returned for a SQL type this server has no mapping for.
///
/// Reported rather than substituted with a placeholder so that a value the server
/// cannot represent is never mistaken for data. The message names an escape hatch
/// the caller can apply without server changes.
fn unsupported_type<R: Row>(row: &R, index: usize, type_name: &str) -> sqlx::Error {
    let column =
        row.columns().get(index).map_or_else(|| index.to_string(), |c| c.name().to_string());
    sqlx::Error::ColumnDecode {
        index: format!("{index} ({column})"),
        source: format!(
            "unsupported SQL type `{type_name}`; cast the column to text in the query \
             (e.g. CAST({column} AS CHAR) on MySQL, {column}::text on PostgreSQL)"
        )
        .into(),
    }
}

/// Decodes one cell of a database row into its string representation.
pub(crate) trait CellDecode: Row {
    /// Returns `Ok(None)` for SQL NULL and `Ok(Some(_))` for a decoded value.
    ///
    /// # Errors
    /// Returns [`sqlx::Error::ColumnDecode`] when the column's SQL type has no
    /// mapping, or when the mapped type fails to decode the value.
    fn cell_to_string(&self, index: usize) -> Result<Option<String>, sqlx::Error>;
}

impl CellDecode for sqlx::mysql::MySqlRow {
    fn cell_to_string(&self, index: usize) -> Result<Option<String>, sqlx::Error> {
        let raw = self.try_get_raw(index)?;
        if raw.is_null() {
            return Ok(None);
        }
        let type_info = raw.type_info();
        let name = type_info.name();

        // Names come from `ColumnType::name`, which folds the UNSIGNED flag into
        // the name and reports TINYINT(1) as BOOLEAN.
        let rendered = match name {
            "BOOLEAN" => render!(self, index, bool),
            "TINYINT" => render!(self, index, i8),
            "SMALLINT" => render!(self, index, i16),
            "INT" | "MEDIUMINT" => render!(self, index, i32),
            "BIGINT" => render!(self, index, i64),
            "TINYINT UNSIGNED" => render!(self, index, u8),
            "SMALLINT UNSIGNED" | "YEAR" => render!(self, index, u16),
            "INT UNSIGNED" | "MEDIUMINT UNSIGNED" => render!(self, index, u32),
            "BIGINT UNSIGNED" | "BIT" => render!(self, index, u64),
            "FLOAT" => render!(self, index, f32),
            "DOUBLE" => render!(self, index, f64),
            // A MySQL DECIMAL carries up to 65 significant digits, more than a 96-bit
            // mantissa holds. `rust_decimal` fails or silently rounds such values; a
            // `BigDecimal` is unbounded and reproduces the text form exactly.
            "DECIMAL" => decimal_to_string(&self.try_get::<BigDecimal, _>(index)?),
            "CHAR" | "VARCHAR" | "TEXT" | "TINYTEXT" | "MEDIUMTEXT" | "LONGTEXT" | "ENUM" => {
                render!(self, index, String)
            }
            // A SET is length-prefixed text on the wire, exactly like ENUM, but
            // `<str as Type<MySql>>::compatible` lists `ColumnType::Enum` and not
            // `ColumnType::Set`, so the checked path rejects it. Read it unchecked
            // rather than failing every `SELECT *` over a table holding one.
            "SET" => self.try_get_unchecked::<String, _>(index)?,
            "DATETIME" => render!(self, index, chrono::NaiveDateTime),
            "TIMESTAMP" => render!(self, index, chrono::DateTime<chrono::Utc>),
            "DATE" => render!(self, index, chrono::NaiveDate),
            // A MySQL TIME is a signed elapsed time over -838:59:59..=838:59:59, not a
            // time of day, so `chrono::NaiveTime` rejects most of the range and fails the
            // whole query.
            "TIME" => mysql_time_to_string(&self.try_get::<MySqlTime, _>(index)?),
            "JSON" => render!(self, index, JsonValue),
            "BINARY" | "VARBINARY" | "BLOB" | "TINYBLOB" | "MEDIUMBLOB" | "LONGBLOB" => {
                bytes_to_string(self.try_get::<Vec<u8>, _>(index)?)
            }
            other => return Err(unsupported_type(self, index, other)),
        };
        Ok(Some(rendered))
    }
}

/// Resolves a PostgreSQL type through any chain of domains to its base type.
///
/// A domain reports its own name (`information_schema` is built on `sql_identifier`,
/// `character_data`, and friends), but sqlx type-checks it by the base type's OID.
/// Matching on the domain's name would therefore reject values that decode fine.
fn pg_base_type(info: &PgTypeInfo) -> &PgTypeInfo {
    let mut current = info;
    while let sqlx::postgres::PgTypeKind::Domain(base) = current.kind() {
        current = base;
    }
    current
}

impl CellDecode for sqlx::postgres::PgRow {
    fn cell_to_string(&self, index: usize) -> Result<Option<String>, sqlx::Error> {
        let raw = self.try_get_raw(index)?;
        if raw.is_null() {
            return Ok(None);
        }
        let type_info = raw.type_info();
        let base = pg_base_type(&type_info);

        // An enum's wire form is its label, but sqlx only accepts it for a matching
        // user type, so read it unchecked rather than failing a common column type.
        if matches!(base.kind(), sqlx::postgres::PgTypeKind::Enum(_)) {
            return Ok(Some(self.try_get_unchecked::<String, _>(index)?));
        }

        // Names come from `PgType::display_name`, which reports the internal
        // catalog spelling: INT4 rather than "integer", BPCHAR as "CHAR".
        let rendered = match base.name() {
            "BOOL" => render!(self, index, bool),
            "INT2" => render!(self, index, i16),
            "INT4" => render!(self, index, i32),
            "INT8" => render!(self, index, i64),
            "FLOAT4" => render!(self, index, f32),
            "FLOAT8" => render!(self, index, f64),
            "NUMERIC" => decimal_to_string(&self.try_get::<PgNumeric, _>(index)?.0),
            "TEXT" | "VARCHAR" | "CHAR" | "NAME" | "UNKNOWN" => render!(self, index, String),
            "TIMESTAMP" => render!(self, index, chrono::NaiveDateTime),
            "TIMESTAMPTZ" => render!(self, index, chrono::DateTime<chrono::Utc>),
            "DATE" => render!(self, index, chrono::NaiveDate),
            "TIME" => render!(self, index, chrono::NaiveTime),
            "UUID" => render!(self, index, Uuid),
            "JSON" | "JSONB" => render!(self, index, JsonValue),
            "BYTEA" => bytes_to_string(self.try_get::<Vec<u8>, _>(index)?),
            other => return Err(unsupported_type(self, index, other)),
        };
        Ok(Some(rendered))
    }
}

impl CellDecode for sqlx::sqlite::SqliteRow {
    fn cell_to_string(&self, index: usize) -> Result<Option<String>, sqlx::Error> {
        let raw = self.try_get_raw(index)?;
        if raw.is_null() {
            return Ok(None);
        }
        let type_info = raw.type_info();
        let name = type_info.name();

        // SQLite is dynamically typed: the name here describes the stored value,
        // not the column's declared type, so only the four storage classes occur
        // once NULL has been handled above.
        let rendered = match name {
            "INTEGER" => render!(self, index, i64),
            "REAL" => render!(self, index, f64),
            "TEXT" => render!(self, index, String),
            "BLOB" => bytes_to_string(self.try_get::<Vec<u8>, _>(index)?),
            other => return Err(unsupported_type(self, index, other)),
        };
        Ok(Some(rendered))
    }
}

/// Returns the column names of `row`, in the order the database reported them.
pub(crate) fn column_names<R: Row>(row: &R) -> Vec<String> {
    row.columns().iter().map(|c| c.name().to_string()).collect()
}

/// Decodes every cell of `row` into its string representation.
///
/// # Errors
/// Propagates the first [`sqlx::Error`] raised by [`CellDecode::cell_to_string`].
pub(crate) fn decode_row<R: CellDecode>(row: &R) -> Result<Vec<Option<String>>, sqlx::Error> {
    row.columns().iter().map(|c| row.cell_to_string(c.ordinal())).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_to_string_returns_valid_utf8_unchanged() {
        assert_eq!(bytes_to_string(b"hello".to_vec()), "hello");
    }

    #[test]
    fn bytes_to_string_base64_encodes_non_utf8_bytes() {
        assert_eq!(bytes_to_string(vec![0xFF, 0xFE, 0xFD]), "base64://79");
    }

    #[test]
    fn decimal_to_string_spells_out_values_below_the_exponential_threshold() {
        let value: BigDecimal = "0.0000001".parse().expect("valid decimal literal");
        assert_eq!(decimal_to_string(&value), "0.0000001");
    }

    #[test]
    fn decimal_to_string_keeps_the_scale_of_zero() {
        let value: BigDecimal = "0.00".parse().expect("valid decimal literal");
        assert_eq!(decimal_to_string(&value), "0.00");
    }

    #[test]
    fn decimal_to_string_keeps_the_scale_of_an_ordinary_value() {
        let value: BigDecimal = "1234.56".parse().expect("valid decimal literal");
        assert_eq!(decimal_to_string(&value), "1234.56");
    }

    #[test]
    fn decimal_to_string_renders_a_negative_scale_as_a_whole_number() {
        let value: BigDecimal = "1E+2".parse().expect("valid decimal literal");
        assert_eq!(decimal_to_string(&value), "100");
    }

    #[test]
    fn bytes_to_string_round_trips_non_utf8_bytes() {
        let original = vec![0x00, 0x80, 0xFF, 0xC0, 0x41];
        let rendered = bytes_to_string(original.clone());

        let encoded = rendered.strip_prefix(BINARY_PREFIX).expect("non-UTF-8 bytes are prefixed");
        assert_eq!(STANDARD.decode(encoded).expect("valid base64"), original);
    }
}
