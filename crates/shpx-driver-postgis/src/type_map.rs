//! Arrow ↔ PostgreSQL 型マッピング。
//!
//! - 書き出し: Arrow `DataType` → PostgreSQL の宣言型（CREATE TABLE 用文字列）
//! - 読み出し: `tokio_postgres::types::Type` → Arrow `DataType`
//!
//! 対応型: Boolean / Int16-64 / Float32-64 / Utf8 / Binary / Date32 /
//! Timestamp(_, None|UTC) / geometry。Decimal / Int8 / UInt 系 / Date64 / 他 TZ は未対応。

use arrow_schema::{DataType, TimeUnit};
use shpx_core::{Error, Result};
use tokio_postgres::types::Type as PgType;

/// Arrow DataType を PostgreSQL の宣言型へ。
///
/// `Decimal128(p, s)` は `numeric(p, s)` のように precision/scale 込みで返すため
/// 戻り値は `String`（cycle 1 までは `&'static str` で十分だったが cycle 2 で拡張）。
pub fn arrow_to_decl(dt: &DataType) -> Result<String> {
    Ok(match dt {
        DataType::Boolean => "boolean".to_string(),
        DataType::Int16 => "smallint".to_string(),
        DataType::Int32 => "integer".to_string(),
        DataType::Int64 => "bigint".to_string(),
        DataType::Float32 => "real".to_string(),
        DataType::Float64 => "double precision".to_string(),
        DataType::Utf8 | DataType::LargeUtf8 => "text".to_string(),
        DataType::Binary | DataType::LargeBinary => "bytea".to_string(),
        DataType::Date32 => "date".to_string(),
        DataType::Timestamp(_, None) => "timestamp".to_string(),
        DataType::Timestamp(_, Some(_)) => "timestamptz".to_string(),
        DataType::Decimal128(p, s) => {
            // PG numeric の precision は 1..=1000、scale は 0..=p。Arrow Decimal128 は precision: u8、
            // scale: i8。Decimal128 として有効な p は 1..=38、s は 0..=p（負スケールは未対応）。
            if *p == 0 || *p > 38 {
                return Err(Error::Schema(format!(
                    "Decimal128 precision must be 1..=38, got {p}"
                )));
            }
            let s_u = u8::try_from(*s).map_err(|_| {
                Error::Schema(format!(
                    "Decimal128 scale out of range (must be 0..=p): {s}"
                ))
            })?;
            format!("numeric({p}, {s_u})")
        }
        other => {
            return Err(Error::Schema(format!(
                "unsupported Arrow type for PostGIS writer: {other:?}"
            )));
        }
    })
}

/// PostgreSQL Type → Arrow DataType（geometry/geography は別判定）。
pub fn pg_to_arrow(ty: &PgType) -> Result<DataType> {
    Ok(match *ty {
        PgType::BOOL => DataType::Boolean,
        PgType::INT2 => DataType::Int16,
        PgType::INT4 => DataType::Int32,
        PgType::INT8 => DataType::Int64,
        PgType::FLOAT4 => DataType::Float32,
        PgType::FLOAT8 => DataType::Float64,
        PgType::TEXT | PgType::VARCHAR | PgType::BPCHAR | PgType::NAME => DataType::Utf8,
        PgType::BYTEA => DataType::Binary,
        PgType::DATE => DataType::Date32,
        PgType::TIMESTAMP => DataType::Timestamp(TimeUnit::Microsecond, None),
        PgType::TIMESTAMPTZ => DataType::Timestamp(TimeUnit::Microsecond, Some("UTC".into())),
        _ => {
            return Err(Error::Schema(format!(
                "unsupported PostgreSQL type for PostGIS reader: {} (OID {})",
                ty.name(),
                ty.oid()
            )));
        }
    })
}

/// PostgreSQL 型名が PostGIS の geometry/geography かどうか。OID は extension 由来で
/// 動的に割り当てられるため名前で判定する。
pub fn is_geometry_type(ty: &PgType) -> bool {
    let name = ty.name();
    name == "geometry" || name == "geography"
}

/// `GeometryType` を PostGIS の DDL 文字列に変換する（CREATE TABLE で `geometry(<type>, <srid>)` に使う）。
pub fn geom_type_to_decl_name(gt: shpx_core::schema::GeometryType) -> &'static str {
    use shpx_core::schema::GeometryType;
    match gt {
        GeometryType::Point => "Point",
        GeometryType::LineString => "LineString",
        GeometryType::Polygon => "Polygon",
        GeometryType::MultiPoint => "MultiPoint",
        GeometryType::MultiLineString => "MultiLineString",
        GeometryType::MultiPolygon => "MultiPolygon",
        GeometryType::GeometryCollection => "GeometryCollection",
        GeometryType::Geometry => "Geometry",
    }
}

/// PostGIS の `ST_GeometryType()` 戻り値（例: `ST_Point`）から `GeometryType` を取り出す。
/// 不明な値は `Geometry` にフォールバック。
pub fn geom_type_from_st_name(name: &str) -> shpx_core::schema::GeometryType {
    use shpx_core::schema::GeometryType;
    match name.trim_start_matches("ST_").to_ascii_lowercase().as_str() {
        "point" => GeometryType::Point,
        "linestring" => GeometryType::LineString,
        "polygon" => GeometryType::Polygon,
        "multipoint" => GeometryType::MultiPoint,
        "multilinestring" => GeometryType::MultiLineString,
        "multipolygon" => GeometryType::MultiPolygon,
        "geometrycollection" => GeometryType::GeometryCollection,
        _ => GeometryType::Geometry,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arrow_to_decl_basics() {
        assert_eq!(arrow_to_decl(&DataType::Boolean).unwrap(), "boolean");
        assert_eq!(arrow_to_decl(&DataType::Int32).unwrap(), "integer");
        assert_eq!(arrow_to_decl(&DataType::Int64).unwrap(), "bigint");
        assert_eq!(
            arrow_to_decl(&DataType::Float64).unwrap(),
            "double precision"
        );
        assert_eq!(arrow_to_decl(&DataType::Utf8).unwrap(), "text");
        assert_eq!(arrow_to_decl(&DataType::Binary).unwrap(), "bytea");
        assert_eq!(arrow_to_decl(&DataType::Date32).unwrap(), "date");
        assert_eq!(
            arrow_to_decl(&DataType::Timestamp(TimeUnit::Microsecond, None)).unwrap(),
            "timestamp"
        );
        assert_eq!(
            arrow_to_decl(&DataType::Timestamp(
                TimeUnit::Microsecond,
                Some("UTC".into())
            ))
            .unwrap(),
            "timestamptz"
        );
    }

    #[test]
    fn arrow_to_decl_decimal128() {
        assert_eq!(
            arrow_to_decl(&DataType::Decimal128(10, 2)).unwrap(),
            "numeric(10, 2)"
        );
        assert_eq!(
            arrow_to_decl(&DataType::Decimal128(38, 10)).unwrap(),
            "numeric(38, 10)"
        );
    }

    #[test]
    fn arrow_to_decl_rejects_decimal128_out_of_range() {
        // p > 38 is not supported by Decimal128 in shpx-driver-postgis (would need Decimal256).
        assert!(arrow_to_decl(&DataType::Decimal128(39, 0)).is_err());
        assert!(arrow_to_decl(&DataType::Decimal128(0, 0)).is_err());
    }

    #[test]
    fn pg_to_arrow_basics() {
        assert_eq!(pg_to_arrow(&PgType::BOOL).unwrap(), DataType::Boolean);
        assert_eq!(pg_to_arrow(&PgType::INT4).unwrap(), DataType::Int32);
        assert_eq!(pg_to_arrow(&PgType::FLOAT8).unwrap(), DataType::Float64);
        assert_eq!(pg_to_arrow(&PgType::TEXT).unwrap(), DataType::Utf8);
        assert_eq!(pg_to_arrow(&PgType::BYTEA).unwrap(), DataType::Binary);
        assert_eq!(pg_to_arrow(&PgType::DATE).unwrap(), DataType::Date32);
    }

    #[test]
    fn geom_type_decl_round() {
        use shpx_core::schema::GeometryType;
        assert_eq!(geom_type_to_decl_name(GeometryType::Point), "Point");
        assert_eq!(geom_type_from_st_name("ST_Point"), GeometryType::Point);
        assert_eq!(
            geom_type_from_st_name("ST_MULTIPOLYGON"),
            GeometryType::MultiPolygon
        );
        assert_eq!(geom_type_from_st_name("unknown"), GeometryType::Geometry);
    }
}
