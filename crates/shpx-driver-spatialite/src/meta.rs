//! SpatiaLite メタテーブルの SQL 定数。
//!
//! `spatial_ref_sys` / `geometry_columns` は `InitSpatialMetadata(1)` で生成される。
//! shpx 側は CREATE TABLE 文を持たず、初期化と SELECT のみ行う前提。
//! `1` は SpatiaLite 4.0+ の FastInit (WGS84 系のみシード)。

/// `geometry_columns` から指定テーブルの geometry 列メタを取得する。
///
/// SpatiaLite 4.x の geometry_columns: (f_table_name, f_geometry_column,
/// geometry_type, coord_dimension, srid, spatial_index_enabled)。
/// - geometry_type は integer (1=Point, 2=LineString, ...)
/// - coord_dimension は integer (2=XY, 3=XYZ, 4=XYZM)
pub const SQL_SELECT_GEOM_COLUMN: &str = r"
SELECT f_geometry_column, geometry_type, coord_dimension, srid
FROM geometry_columns
WHERE lower(f_table_name) = lower(?1)
";

/// geometry_columns に登録済みの全テーブル一覧。
pub const SQL_LIST_FEATURE_TABLES: &str = r"
SELECT f_table_name FROM geometry_columns
ORDER BY f_table_name
";

/// `spatial_ref_sys` の 1 行を引く SQL。
pub const SQL_SELECT_SRS: &str = r"
SELECT auth_name, auth_srid, ref_sys_name, proj4text, srtext
FROM spatial_ref_sys
WHERE srid = ?1
";

/// `spatial_ref_sys` に新規 SRS 行を best-effort で追加する。
/// 既存 srid と衝突したら何もしない。
pub const SQL_INSERT_SRS: &str = r"
INSERT OR IGNORE INTO spatial_ref_sys
  (srid, auth_name, auth_srid, ref_sys_name, proj4text, srtext)
VALUES (?1, ?2, ?3, ?4, ?5, ?6)
";

/// `geometry_columns` テーブルが存在するかを検出する。
/// SpatiaLite 化されていない素の SQLite ファイルでは存在しない。
pub const SQL_GEOMETRY_COLUMNS_EXISTS: &str = r"
SELECT COUNT(*) FROM sqlite_master
WHERE type='table' AND name='geometry_columns'
";

/// `geometry_type` integer 値（SpatiaLite 4.x）。
pub mod geometry_type {
    pub const POINT: i32 = 1;
    pub const LINESTRING: i32 = 2;
    pub const POLYGON: i32 = 3;
    pub const MULTIPOINT: i32 = 4;
    pub const MULTILINESTRING: i32 = 5;
    pub const MULTIPOLYGON: i32 = 6;
    pub const GEOMETRYCOLLECTION: i32 = 7;
}

/// SpatiaLite の `geometry_type` integer 値を `GeometryType` enum に変換。
pub fn geom_type_from_int(t: i32) -> shpx_core::schema::GeometryType {
    use shpx_core::schema::GeometryType;
    match t {
        geometry_type::POINT => GeometryType::Point,
        geometry_type::LINESTRING => GeometryType::LineString,
        geometry_type::POLYGON => GeometryType::Polygon,
        geometry_type::MULTIPOINT => GeometryType::MultiPoint,
        geometry_type::MULTILINESTRING => GeometryType::MultiLineString,
        geometry_type::MULTIPOLYGON => GeometryType::MultiPolygon,
        geometry_type::GEOMETRYCOLLECTION => GeometryType::GeometryCollection,
        // 1001-1007 は XYZ、2001-2007 は XYM、3001-3007 は XYZM。v0.5 では XY のみ
        // サポートだが、reader 側は寛容に読む（型名は base type で記録）。
        _ => GeometryType::Geometry,
    }
}

/// `GeometryType` を SpatiaLite の `AddGeometryColumn` で受ける文字列名に変換。
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

    #[test]
    fn geom_type_int_roundtrip() {
        use shpx_core::schema::GeometryType;
        for (t, name) in [
            (GeometryType::Point, "POINT"),
            (GeometryType::LineString, "LINESTRING"),
            (GeometryType::Polygon, "POLYGON"),
            (GeometryType::MultiPoint, "MULTIPOINT"),
            (GeometryType::MultiLineString, "MULTILINESTRING"),
            (GeometryType::MultiPolygon, "MULTIPOLYGON"),
        ] {
            assert_eq!(geom_type_to_name(t), name);
        }
        // int → enum
        assert_eq!(geom_type_from_int(1), GeometryType::Point);
        assert_eq!(geom_type_from_int(6), GeometryType::MultiPolygon);
        assert_eq!(geom_type_from_int(999), GeometryType::Geometry);
    }
}
