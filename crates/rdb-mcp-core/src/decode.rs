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

use base64::{Engine as _, engine::general_purpose::STANDARD};
use sqlx::{
    Column, Row, TypeInfo, ValueRef,
    types::{Decimal, JsonValue, Uuid},
};

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
            "DECIMAL" => render!(self, index, Decimal),
            "CHAR" | "VARCHAR" | "TEXT" | "TINYTEXT" | "MEDIUMTEXT" | "LONGTEXT" | "ENUM" => {
                render!(self, index, String)
            }
            "DATETIME" => render!(self, index, chrono::NaiveDateTime),
            "TIMESTAMP" => render!(self, index, chrono::DateTime<chrono::Utc>),
            "DATE" => render!(self, index, chrono::NaiveDate),
            "TIME" => render!(self, index, chrono::NaiveTime),
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
fn pg_base_type(info: &sqlx::postgres::PgTypeInfo) -> &sqlx::postgres::PgTypeInfo {
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
            "NUMERIC" => render!(self, index, Decimal),
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
    fn bytes_to_string_round_trips_non_utf8_bytes() {
        let original = vec![0x00, 0x80, 0xFF, 0xC0, 0x41];
        let rendered = bytes_to_string(original.clone());

        let encoded = rendered.strip_prefix(BINARY_PREFIX).expect("non-UTF-8 bytes are prefixed");
        assert_eq!(STANDARD.decode(encoded).expect("valid base64"), original);
    }
}
