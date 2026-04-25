//! GeoPackage メタテーブルの SQL 定数と検証 helper。
//!
//! OGC GeoPackage 1.3 (Annex C) で定義される 3 つの必須テーブル:
//! - `gpkg_spatial_ref_sys`
//! - `gpkg_contents`
//! - `gpkg_geometry_columns`
//!
//! `IF NOT EXISTS` を付けて初期化を冪等に保つ（`overwrite=true` でも、ファイル削除→再作成
//! の経路を writer 側が選ぶため、ここでは新規ファイル前提で十分）。

/// `application_id` PRAGMA の値。`'GPKG'` (0x47, 0x50, 0x4B, 0x47) を big-endian で詰めた整数。
/// SQLite header の `application_id` フィールドに同値が書かれる。
pub const APPLICATION_ID: i32 = 0x4750_4b47;

/// `user_version` PRAGMA の値。GeoPackage 1.3 のバージョン番号 10300 を使う。
/// reader は本値を見て厳密に拒否しない（gpkg_spatial_ref_sys / gpkg_geometry_columns 存在で判定）。
pub const USER_VERSION: i32 = 10_300;

/// `gpkg_spatial_ref_sys.organization` の主要な値。EPSG 参照は `"EPSG"`、
/// shpx が WKT のみの CRS を受け入れて採番した行は `"shpx"`。
pub const ORG_EPSG: &str = "EPSG";
pub const ORG_SHPX: &str = "shpx";

pub const SQL_CREATE_GPKG_SPATIAL_REF_SYS: &str = r"
CREATE TABLE IF NOT EXISTS gpkg_spatial_ref_sys (
    srs_name                 TEXT    NOT NULL,
    srs_id                   INTEGER NOT NULL PRIMARY KEY,
    organization             TEXT    NOT NULL,
    organization_coordsys_id INTEGER NOT NULL,
    definition               TEXT    NOT NULL,
    description              TEXT
)
";

pub const SQL_CREATE_GPKG_CONTENTS: &str = r"
CREATE TABLE IF NOT EXISTS gpkg_contents (
    table_name  TEXT NOT NULL PRIMARY KEY,
    data_type   TEXT NOT NULL,
    identifier  TEXT UNIQUE,
    description TEXT DEFAULT '',
    last_change DATETIME NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    min_x DOUBLE,
    min_y DOUBLE,
    max_x DOUBLE,
    max_y DOUBLE,
    srs_id INTEGER,
    CONSTRAINT fk_gc_r_srs_id FOREIGN KEY (srs_id) REFERENCES gpkg_spatial_ref_sys(srs_id)
)
";

pub const SQL_CREATE_GPKG_GEOMETRY_COLUMNS: &str = r"
CREATE TABLE IF NOT EXISTS gpkg_geometry_columns (
    table_name TEXT NOT NULL,
    column_name TEXT NOT NULL,
    geometry_type_name TEXT NOT NULL,
    srs_id INTEGER NOT NULL,
    z TINYINT NOT NULL,
    m TINYINT NOT NULL,
    CONSTRAINT pk_geom_cols PRIMARY KEY (table_name, column_name),
    CONSTRAINT fk_gc_tn FOREIGN KEY (table_name) REFERENCES gpkg_contents(table_name),
    CONSTRAINT fk_gc_srs FOREIGN KEY (srs_id) REFERENCES gpkg_spatial_ref_sys(srs_id)
)
";

/// 必須 SRS 行 3 件: 仕様で `gpkg_spatial_ref_sys` に必ず存在しなければならない。
/// EPSG:4326 の definition は呼び出し側で `epsg_to_wkt1(4326)` を流し込む（バイナリサイズ削減のため定数化しない）。
pub const SQL_INSERT_REQUIRED_SRS: &str = r"
INSERT OR IGNORE INTO gpkg_spatial_ref_sys
  (srs_name, srs_id, organization, organization_coordsys_id, definition, description)
VALUES
  ('Undefined cartesian SRS',  -1, 'NONE', -1, 'undefined', 'undefined cartesian coordinate reference system'),
  ('Undefined geographic SRS',  0, 'NONE',  0, 'undefined', 'undefined geographic coordinate reference system'),
  ('WGS 84',                 4326, 'EPSG', 4326, ?1,        'longitude/latitude coordinates in decimal degrees on the WGS 84 spheroid')
";

/// `gpkg_contents` に登録された feature テーブルの一覧を返す SQL。
pub const SQL_LIST_FEATURE_TABLES: &str = r"
SELECT table_name FROM gpkg_contents
WHERE data_type = 'features'
ORDER BY table_name
";

/// gpkg_geometry_columns から column_name と geometry_type_name と srs_id を引く SQL。
pub const SQL_SELECT_GEOM_COLUMN: &str = r"
SELECT column_name, geometry_type_name, srs_id
FROM gpkg_geometry_columns
WHERE table_name = ?1
";

/// gpkg_spatial_ref_sys の 1 行を引く SQL。
pub const SQL_SELECT_SRS: &str = r"
SELECT srs_name, organization, organization_coordsys_id, definition
FROM gpkg_spatial_ref_sys
WHERE srs_id = ?1
";

/// gpkg_spatial_ref_sys に新規 SRS を追加する SQL（INSERT OR IGNORE）。
pub const SQL_INSERT_SRS: &str = r"
INSERT OR IGNORE INTO gpkg_spatial_ref_sys
  (srs_name, srs_id, organization, organization_coordsys_id, definition, description)
VALUES (?1, ?2, ?3, ?4, ?5, ?6)
";

/// gpkg_contents に feature レイヤを追加する SQL。
pub const SQL_INSERT_CONTENTS: &str = r"
INSERT INTO gpkg_contents
  (table_name, data_type, identifier, description, srs_id)
VALUES (?1, 'features', ?1, ?2, ?3)
";

/// gpkg_geometry_columns に geometry 列を追加する SQL。
pub const SQL_INSERT_GEOMETRY_COLUMN: &str = r"
INSERT INTO gpkg_geometry_columns
  (table_name, column_name, geometry_type_name, srs_id, z, m)
VALUES (?1, ?2, ?3, ?4, 0, 0)
";

/// gpkg_contents の bbox を更新する SQL。`finish()` で累積した bbox を反映する。
pub const SQL_UPDATE_CONTENTS_BBOX: &str = r"
UPDATE gpkg_contents
SET min_x = ?2, min_y = ?3, max_x = ?4, max_y = ?5,
    last_change = strftime('%Y-%m-%dT%H:%M:%fZ','now')
WHERE table_name = ?1
";

/// 累積 BBOX。NULL geometry 行は更新せず、有効値が来た時点で初期化する。
#[derive(Debug, Clone, Copy)]
pub struct BBox {
    pub min_x: f64,
    pub min_y: f64,
    pub max_x: f64,
    pub max_y: f64,
}

impl Default for BBox {
    fn default() -> Self {
        // 「未初期化」を表す番兵値。`update` が来るまで `is_initialized=false`。
        Self {
            min_x: f64::INFINITY,
            min_y: f64::INFINITY,
            max_x: f64::NEG_INFINITY,
            max_y: f64::NEG_INFINITY,
        }
    }
}

impl BBox {
    pub fn is_initialized(&self) -> bool {
        self.min_x.is_finite() && self.max_x.is_finite()
    }

    pub fn update_xy(&mut self, x: f64, y: f64) {
        if !x.is_finite() || !y.is_finite() {
            // NaN / 無限大は BBOX に含めない（GPKG 仕様で min/max は finite を要求）。
            return;
        }
        if x < self.min_x {
            self.min_x = x;
        }
        if y < self.min_y {
            self.min_y = y;
        }
        if x > self.max_x {
            self.max_x = x;
        }
        if y > self.max_y {
            self.max_y = y;
        }
    }

    pub fn update_geom(&mut self, g: &shpx_geom::wkb::Geom) {
        use shpx_geom::wkb::Geom;
        match g {
            Geom::Point(x, y) => self.update_xy(*x, *y),
            Geom::LineString(pts) | Geom::MultiPoint(pts) => {
                for (x, y) in pts {
                    self.update_xy(*x, *y);
                }
            }
            Geom::Polygon(rings) | Geom::MultiLineString(rings) => {
                for ring in rings {
                    for (x, y) in ring {
                        self.update_xy(*x, *y);
                    }
                }
            }
            Geom::MultiPolygon(polys) => {
                for poly in polys {
                    for ring in poly {
                        for (x, y) in ring {
                            self.update_xy(*x, *y);
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn application_id_matches_ascii_gpkg() {
        // ASCII 'G'(0x47), 'P'(0x50), 'K'(0x4B), 'G'(0x47) の big-endian。
        assert_eq!(APPLICATION_ID.to_be_bytes(), [0x47, 0x50, 0x4B, 0x47]);
    }

    #[test]
    fn bbox_default_is_uninitialized() {
        let b = BBox::default();
        assert!(!b.is_initialized());
    }

    #[test]
    fn bbox_update_with_point() {
        let mut b = BBox::default();
        b.update_geom(&shpx_geom::wkb::Geom::Point(1.0, 2.0));
        assert!(b.is_initialized());
        assert!((b.min_x - 1.0).abs() < f64::EPSILON);
        assert!((b.max_x - 1.0).abs() < f64::EPSILON);
        assert!((b.min_y - 2.0).abs() < f64::EPSILON);
        assert!((b.max_y - 2.0).abs() < f64::EPSILON);
    }

    #[test]
    fn bbox_update_with_polygon_with_hole() {
        let mut b = BBox::default();
        b.update_geom(&shpx_geom::wkb::Geom::Polygon(vec![
            vec![
                (0.0, 0.0),
                (10.0, 0.0),
                (10.0, 10.0),
                (0.0, 10.0),
                (0.0, 0.0),
            ],
            vec![(2.0, 2.0), (3.0, 2.0), (3.0, 3.0), (2.0, 2.0)],
        ]));
        assert!((b.min_x - 0.0).abs() < f64::EPSILON);
        assert!((b.max_x - 10.0).abs() < f64::EPSILON);
        assert!((b.min_y - 0.0).abs() < f64::EPSILON);
        assert!((b.max_y - 10.0).abs() < f64::EPSILON);
    }

    #[test]
    fn bbox_ignores_non_finite() {
        let mut b = BBox::default();
        b.update_xy(f64::NAN, 1.0);
        b.update_xy(1.0, f64::INFINITY);
        assert!(!b.is_initialized());
    }
}
