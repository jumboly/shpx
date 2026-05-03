//! Arrow ↔ SQL Server (T-SQL) 型マッピング。
//!
//! - 書き出し: Arrow `DataType` → T-SQL の宣言型（`CREATE TABLE` 用文字列）
//! - 読み出し: `INFORMATION_SCHEMA.COLUMNS.DATA_TYPE` 文字列 → Arrow `DataType`
//!
//! 対応型: Boolean / Int16-64 / Float32-64 / Utf8 / Binary / Date32 /
//! Timestamp(_, None|UTC) / Decimal128 / geometry / geography。
//! Int8 / UInt 系 / Date64 / 他 TZ / xml / hierarchyid 等は未対応。

use arrow_schema::{DataType, TimeUnit};
use shpx_core::{schema::GeometryType, Error, Result};

/// Arrow `DataType` を SQL Server の宣言型へ。
///
/// `Decimal128(p, s)` は `decimal(p, s)` 形式で返す。geometry 列は別経路で
/// `geometry` / `geography` を直接組み立てるため本関数の管轄外。
pub fn arrow_to_decl(dt: &DataType) -> Result<String> {
    Ok(match dt {
        DataType::Boolean => "bit".to_string(),
        DataType::Int16 => "smallint".to_string(),
        DataType::Int32 => "int".to_string(),
        DataType::Int64 => "bigint".to_string(),
        DataType::Float32 => "real".to_string(),
        // T-SQL の `float` は既定 53bit (= IEEE 754 double precision)。`float(53)` と等価。
        DataType::Float64 => "float".to_string(),
        // SQL Server の `nvarchar(max)` は UTF-16 で最大 2GB。Utf8/LargeUtf8 を
        // 区別せず `nvarchar(max)` に集約する（cycle 1 範囲内では十分）。
        DataType::Utf8 | DataType::LargeUtf8 => "nvarchar(max)".to_string(),
        DataType::Binary | DataType::LargeBinary => "varbinary(max)".to_string(),
        DataType::Date32 => "date".to_string(),
        // 既定 precision 7 (100ns 解像度) の `datetime2`。Arrow の microsecond には十分。
        DataType::Timestamp(_, None) => "datetime2".to_string(),
        DataType::Timestamp(_, Some(_)) => "datetimeoffset".to_string(),
        DataType::Decimal128(p, s) => {
            // T-SQL `decimal(p, s)` の precision は 1..=38、scale は 0..=p。
            // Arrow Decimal128 の precision: u8、scale: i8。負スケールは未対応。
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
            format!("decimal({p}, {s_u})")
        }
        other => {
            return Err(Error::Schema(format!(
                "unsupported Arrow type for SQL Server writer: {other:?}"
            )));
        }
    })
}

/// `INFORMATION_SCHEMA.COLUMNS.DATA_TYPE` の小文字文字列と、numeric の場合の
/// `(precision, scale)` 取得結果を Arrow `DataType` に変換する。
///
/// `precision` / `scale` は `decimal` / `numeric` のみで使う（他型は無視）。
pub fn sqlserver_type_to_arrow(
    data_type: &str,
    numeric_precision: Option<i32>,
    numeric_scale: Option<i32>,
) -> Result<DataType> {
    Ok(match data_type {
        "bit" => DataType::Boolean,
        "smallint" => DataType::Int16,
        "int" => DataType::Int32,
        "bigint" => DataType::Int64,
        "real" => DataType::Float32,
        "float" => DataType::Float64,
        // text 系は全て UTF-8 表現 (Arrow Utf8) に集約。SQL Server の `text` は deprecated だが
        // 既存テーブル互換のため受け取る。
        "nvarchar" | "varchar" | "nchar" | "char" | "text" | "ntext" => DataType::Utf8,
        "varbinary" | "binary" | "image" => DataType::Binary,
        "date" => DataType::Date32,
        "datetime2" | "datetime" | "smalldatetime" => {
            DataType::Timestamp(TimeUnit::Microsecond, None)
        }
        "datetimeoffset" => DataType::Timestamp(TimeUnit::Microsecond, Some("UTC".into())),
        "decimal" | "numeric" => {
            let p_i = numeric_precision.unwrap_or(38);
            let s_i = numeric_scale.unwrap_or(0);
            // INFORMATION_SCHEMA は precision/scale を `int` で返す（SQL Server 仕様）。
            // 範囲外は (38, 0) フォールバック。
            if !(1..=38).contains(&p_i) || !(0..=38).contains(&s_i) || s_i > p_i {
                DataType::Decimal128(38, 0)
            } else {
                let p = u8::try_from(p_i).expect("precision 1..=38 fits u8");
                let s = i8::try_from(s_i).expect("scale 0..=38 fits i8");
                DataType::Decimal128(p, s)
            }
        }
        // 未対応型は `Error::Schema` で reader 側のメッセージに乗せる。
        other => {
            return Err(Error::Schema(format!(
                "unsupported SQL Server type for sqlserver reader: {other}"
            )));
        }
    })
}

/// `INFORMATION_SCHEMA.COLUMNS.DATA_TYPE` が SQL Server の geometry / geography UDT か。
#[must_use]
pub fn is_geometry_type_name(name: &str) -> bool {
    matches!(name, "geometry" | "geography")
}

/// `STGeometryType()` 戻り値（例: `Point`, `LineString`, `MULTIPOLYGON`）から
/// `GeometryType` を取り出す。SQL Server は PostGIS の `ST_Point` のような prefix を付けず、
/// 大文字小文字も統一していないため、両方を受け入れる。
#[must_use]
pub fn geom_type_from_sqlserver_name(name: &str) -> GeometryType {
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
        assert_eq!(arrow_to_decl(&DataType::Boolean).unwrap(), "bit");
        assert_eq!(arrow_to_decl(&DataType::Int16).unwrap(), "smallint");
        assert_eq!(arrow_to_decl(&DataType::Int32).unwrap(), "int");
        assert_eq!(arrow_to_decl(&DataType::Int64).unwrap(), "bigint");
        assert_eq!(arrow_to_decl(&DataType::Float32).unwrap(), "real");
        assert_eq!(arrow_to_decl(&DataType::Float64).unwrap(), "float");
        assert_eq!(arrow_to_decl(&DataType::Utf8).unwrap(), "nvarchar(max)");
        assert_eq!(arrow_to_decl(&DataType::Binary).unwrap(), "varbinary(max)");
        assert_eq!(arrow_to_decl(&DataType::Date32).unwrap(), "date");
        assert_eq!(
            arrow_to_decl(&DataType::Timestamp(TimeUnit::Microsecond, None)).unwrap(),
            "datetime2"
        );
        assert_eq!(
            arrow_to_decl(&DataType::Timestamp(
                TimeUnit::Microsecond,
                Some("UTC".into())
            ))
            .unwrap(),
            "datetimeoffset"
        );
    }

    #[test]
    fn arrow_to_decl_decimal128() {
        assert_eq!(
            arrow_to_decl(&DataType::Decimal128(10, 2)).unwrap(),
            "decimal(10, 2)"
        );
        assert_eq!(
            arrow_to_decl(&DataType::Decimal128(38, 10)).unwrap(),
            "decimal(38, 10)"
        );
    }

    #[test]
    fn arrow_to_decl_rejects_decimal128_out_of_range() {
        assert!(arrow_to_decl(&DataType::Decimal128(39, 0)).is_err());
        assert!(arrow_to_decl(&DataType::Decimal128(0, 0)).is_err());
    }

    #[test]
    fn sqlserver_type_to_arrow_basics() {
        assert_eq!(
            sqlserver_type_to_arrow("bit", None, None).unwrap(),
            DataType::Boolean
        );
        assert_eq!(
            sqlserver_type_to_arrow("int", None, None).unwrap(),
            DataType::Int32
        );
        assert_eq!(
            sqlserver_type_to_arrow("nvarchar", None, None).unwrap(),
            DataType::Utf8
        );
        assert_eq!(
            sqlserver_type_to_arrow("varbinary", None, None).unwrap(),
            DataType::Binary
        );
        assert_eq!(
            sqlserver_type_to_arrow("datetime2", None, None).unwrap(),
            DataType::Timestamp(TimeUnit::Microsecond, None)
        );
        assert_eq!(
            sqlserver_type_to_arrow("datetimeoffset", None, None).unwrap(),
            DataType::Timestamp(TimeUnit::Microsecond, Some("UTC".into()))
        );
    }

    #[test]
    fn sqlserver_type_to_arrow_decimal_with_precision() {
        assert_eq!(
            sqlserver_type_to_arrow("decimal", Some(10), Some(2)).unwrap(),
            DataType::Decimal128(10, 2)
        );
        assert_eq!(
            sqlserver_type_to_arrow("numeric", Some(38), Some(10)).unwrap(),
            DataType::Decimal128(38, 10)
        );
    }

    #[test]
    fn sqlserver_type_to_arrow_decimal_fallback_when_invalid() {
        // p > 38 → fallback (38, 0)
        assert_eq!(
            sqlserver_type_to_arrow("decimal", Some(39), Some(0)).unwrap(),
            DataType::Decimal128(38, 0)
        );
        // s > p → fallback
        assert_eq!(
            sqlserver_type_to_arrow("decimal", Some(5), Some(10)).unwrap(),
            DataType::Decimal128(38, 0)
        );
        // None precision → fallback to (38, 0)
        assert_eq!(
            sqlserver_type_to_arrow("decimal", None, None).unwrap(),
            DataType::Decimal128(38, 0)
        );
    }

    #[test]
    fn sqlserver_type_to_arrow_rejects_unknown() {
        assert!(sqlserver_type_to_arrow("xml", None, None).is_err());
    }

    #[test]
    fn is_geometry_type_name_matches_both() {
        assert!(is_geometry_type_name("geometry"));
        assert!(is_geometry_type_name("geography"));
        assert!(!is_geometry_type_name("geom"));
        assert!(!is_geometry_type_name("varbinary"));
    }

    #[test]
    fn geom_type_from_sqlserver_name_round() {
        assert_eq!(geom_type_from_sqlserver_name("Point"), GeometryType::Point);
        assert_eq!(geom_type_from_sqlserver_name("POINT"), GeometryType::Point);
        // PostGIS の ST_Point prefix もたまたま受け入れる（型変換の互換性のため）。
        assert_eq!(
            geom_type_from_sqlserver_name("ST_Point"),
            GeometryType::Point
        );
        assert_eq!(
            geom_type_from_sqlserver_name("MULTIPOLYGON"),
            GeometryType::MultiPolygon
        );
        assert_eq!(
            geom_type_from_sqlserver_name("unknown"),
            GeometryType::Geometry
        );
    }
}
