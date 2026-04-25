//! Arrow `DataType` ↔ GeoPackage / SQLite 宣言型のマッピング。
//!
//! 双方向のマッピングは「宣言型を最優先」で運用する。SQLite は dynamic typing で
//! 値型と宣言型が一致しない可能性があるが、shpx は CREATE TABLE 時に shpx-core の
//! Arrow schema をそのまま GPKG 仕様で許される宣言型へ落とし込む。

use arrow_schema::{DataType, TimeUnit};

use crate::util::driver_msg;

/// 出力時に GPKG / SQLite の CREATE TABLE 列宣言として書く文字列を返す。
///
/// 失敗:
/// - `UInt64` などのサポート外型 → `Error::Schema`
///
/// `clippy::match_same_arms` を許容: Decimal は意味的に「文字列降格」であって TEXT との
/// セマンティクスは違う（writer 側で `apply_on_loss` を発火する）。同じ "TEXT" を返す
/// だけでも、宣言として並べたいので分けて書いている。
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
                "UInt64 not supported in GPKG (sqlite max integer is i64)".to_string(),
            ));
        }
        DataType::Float16 | DataType::Float32 => "FLOAT",
        DataType::Float64 => "DOUBLE",
        DataType::Utf8 | DataType::LargeUtf8 => "TEXT",
        DataType::Binary | DataType::LargeBinary => "BLOB",
        DataType::Date32 | DataType::Date64 => "DATE",
        DataType::Timestamp(_, _) => "DATETIME",
        // Decimal は GPKG 仕様で直接表現できない。文字列降格は writer 側の
        // `apply_on_loss` 経路で処理するため、宣言型は TEXT を返すだけにとどめる。
        DataType::Decimal128(_, _) | DataType::Decimal256(_, _) => "TEXT",
        other => {
            return Err(shpx_core::Error::Schema(format!(
                "unsupported Arrow type for GPKG: {other:?}"
            )));
        }
    })
}

/// SQLite の宣言型文字列から Arrow `DataType` を推論する。
///
/// `PRAGMA table_info` が返す型名はユーザーが CREATE TABLE で書いた文字列そのまま
/// （大文字小文字や `(...)` 修飾もそのまま）。GPKG 仕様で定義された宣言型と GDAL/QGIS が
/// 出す揺れ（`INT2`, `INT8`, `INT`, `BIGINT` 等）も寛容に解釈する。
#[allow(clippy::match_same_arms)]
pub fn decl_to_arrow(decl: &str) -> shpx_core::Result<DataType> {
    // `VARCHAR(255)` のような型修飾子を切り落とし、大文字に揃える。
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
        // 不明な宣言型は TEXT 扱いにフォールバック（dynamic typing で値が文字列以外で
        // 来る可能性は reader 側で `to_string` 降格して救う）。
        // 未知型を Schema エラーにするより、データを読めるほうを優先する。
        "" => return Err(driver_msg("empty SQLite declared type")),
        _ => DataType::Utf8,
    })
}

/// `gpkg_geometry_columns.geometry_type_name` を `GeometryType` enum に変換する。
/// GPKG 仕様（OGC SFA 1.2.1）の型名をすべて受け付け、未知名は `Geometry` 扱い。
pub fn geom_type_from_name(name: &str) -> shpx_core::schema::GeometryType {
    use shpx_core::schema::GeometryType;
    match name.to_ascii_uppercase().as_str() {
        "POINT" => GeometryType::Point,
        "LINESTRING" => GeometryType::LineString,
        "POLYGON" => GeometryType::Polygon,
        "MULTIPOINT" => GeometryType::MultiPoint,
        "MULTILINESTRING" => GeometryType::MultiLineString,
        "MULTIPOLYGON" => GeometryType::MultiPolygon,
        "GEOMETRYCOLLECTION" => GeometryType::GeometryCollection,
        // GEOMETRY / 不明 → 混在許容の Geometry。
        _ => GeometryType::Geometry,
    }
}

/// `GeometryType` を `gpkg_geometry_columns.geometry_type_name` に書く文字列に変換。
#[must_use]
pub fn geom_type_to_name(t: shpx_core::schema::GeometryType) -> &'static str {
    use shpx_core::schema::GeometryType;
    match t {
        GeometryType::Geometry => "GEOMETRY",
        GeometryType::Point => "POINT",
        GeometryType::LineString => "LINESTRING",
        GeometryType::Polygon => "POLYGON",
        GeometryType::MultiPoint => "MULTIPOINT",
        GeometryType::MultiLineString => "MULTILINESTRING",
        GeometryType::MultiPolygon => "MULTIPOLYGON",
        GeometryType::GeometryCollection => "GEOMETRYCOLLECTION",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shpx_core::schema::GeometryType;

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

    #[test]
    fn decl_to_arrow_unknown_falls_back_to_utf8() {
        assert_eq!(decl_to_arrow("WIDGET").unwrap(), DataType::Utf8);
    }

    #[test]
    fn geom_type_roundtrip_via_name() {
        for t in [
            GeometryType::Geometry,
            GeometryType::Point,
            GeometryType::LineString,
            GeometryType::Polygon,
            GeometryType::MultiPoint,
            GeometryType::MultiLineString,
            GeometryType::MultiPolygon,
            GeometryType::GeometryCollection,
        ] {
            let name = geom_type_to_name(t);
            assert_eq!(geom_type_from_name(name), t, "{name}");
        }
    }
}
