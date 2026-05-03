//! Arrow `DataType` ↔ SQLite 宣言型のマッピング。
//!
//! GPKG driver の `type_map.rs` と同等。SpatiaLite は SQLite 上に乗るので、
//! 属性列の型表現は GPKG と共通で問題ない。geometry 列だけ blob 表現が異なる
//! （GPKG binary header vs SpatiaLite blob）。

use arrow_schema::{DataType, TimeUnit};

use crate::util::driver_msg;

/// CREATE TABLE 列宣言として書く SQLite 型文字列を返す。
#[allow(clippy::match_same_arms)]
pub fn arrow_to_decl(dt: &DataType) -> shpx_core::Result<&'static str> {
    Ok(match dt {
        DataType::Boolean => "BOOLEAN",
        DataType::Int8 => "TINYINT",
        DataType::Int16 => "SMALLINT",
        DataType::Int32 => "MEDIUMINT",
        DataType::Int64 | DataType::UInt8 | DataType::UInt16 | DataType::UInt32 => "INTEGER",
        DataType::UInt64 => {
            return Err(shpx_core::Error::Schema(
                "UInt64 not supported in SpatiaLite (sqlite max integer is i64)".to_string(),
            ));
        }
        DataType::Float16 | DataType::Float32 => "FLOAT",
        DataType::Float64 => "DOUBLE",
        DataType::Utf8 | DataType::LargeUtf8 => "TEXT",
        DataType::Binary | DataType::LargeBinary => "BLOB",
        DataType::Date32 | DataType::Date64 => "DATE",
        DataType::Timestamp(_, _) => "DATETIME",
        DataType::Decimal128(_, _) | DataType::Decimal256(_, _) => "TEXT",
        other => {
            return Err(shpx_core::Error::Schema(format!(
                "unsupported Arrow type for SpatiaLite: {other:?}"
            )));
        }
    })
}

/// SQLite の宣言型文字列から Arrow `DataType` を推論する。
#[allow(clippy::match_same_arms)]
pub fn decl_to_arrow(decl: &str) -> shpx_core::Result<DataType> {
    let upper = decl.to_ascii_uppercase();
    let name = upper.split('(').next().unwrap_or("").trim();
    Ok(match name {
        "BOOLEAN" | "BOOL" => DataType::Boolean,
        "TINYINT" => DataType::Int8,
        "SMALLINT" | "INT2" => DataType::Int16,
        "MEDIUMINT" | "INT4" => DataType::Int32,
        "INTEGER" | "INT" | "BIGINT" | "INT8" => DataType::Int64,
        "FLOAT" | "REAL" => DataType::Float32,
        "DOUBLE" | "DOUBLE PRECISION" | "NUMERIC" => DataType::Float64,
        "TEXT" | "CLOB" | "STRING" | "VARCHAR" | "CHAR" | "CHARACTER" => DataType::Utf8,
        "BLOB" | "BINARY" | "VARBINARY" => DataType::Binary,
        "DATE" => DataType::Date32,
        "DATETIME" | "TIMESTAMP" => DataType::Timestamp(TimeUnit::Microsecond, None),
        "" => return Err(driver_msg("empty SQLite declared type")),
        _ => DataType::Utf8,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arrow_to_decl_basic_types() {
        assert_eq!(arrow_to_decl(&DataType::Int64).unwrap(), "INTEGER");
        assert_eq!(arrow_to_decl(&DataType::Boolean).unwrap(), "BOOLEAN");
        assert_eq!(arrow_to_decl(&DataType::Float64).unwrap(), "DOUBLE");
        assert_eq!(arrow_to_decl(&DataType::Utf8).unwrap(), "TEXT");
        assert_eq!(arrow_to_decl(&DataType::Binary).unwrap(), "BLOB");
        assert_eq!(arrow_to_decl(&DataType::Date32).unwrap(), "DATE");
    }

    #[test]
    fn arrow_to_decl_rejects_uint64() {
        let err = arrow_to_decl(&DataType::UInt64).unwrap_err();
        assert!(matches!(err, shpx_core::Error::Schema(_)));
    }

    #[test]
    fn decl_to_arrow_handles_modifiers() {
        assert_eq!(decl_to_arrow("VARCHAR(255)").unwrap(), DataType::Utf8);
        assert_eq!(decl_to_arrow("INTEGER").unwrap(), DataType::Int64);
        assert_eq!(decl_to_arrow("integer").unwrap(), DataType::Int64);
        assert_eq!(
            decl_to_arrow("DATETIME").unwrap(),
            DataType::Timestamp(TimeUnit::Microsecond, None)
        );
    }
}
