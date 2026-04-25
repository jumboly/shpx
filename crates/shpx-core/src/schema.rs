//! ジオメトリ列の Arrow field metadata 規約。
//!
//! 中間表現として、Arrow `Binary` 列の field metadata に [`GEOMETRY_META_KEY`] キーで
//! [`GeometryMeta`] を JSON 文字列として持たせる。各 driver はこの JSON を
//! 自身のフォーマット固有メタデータ（GeoParquet の `geo` キー、Shapefile の `.prj` 等）と
//! 相互変換する。

use serde::{Deserialize, Serialize};

use crate::Crs;

/// Arrow field metadata に書き込むキー名。
pub const GEOMETRY_META_KEY: &str = "shpx:geometry";

/// ジオメトリ列メタデータの中間表現。JSON 文字列として field metadata に格納する。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GeometryMeta {
    /// ジオメトリのバイト列エンコーディング。v0.1 は `WKB` 固定。
    pub encoding: GeometryEncoding,

    /// 列に格納される代表ジオメトリ型。混在を許す場合は [`GeometryType::Geometry`]。
    pub geometry_type: GeometryType,

    /// 既知 CRS。`None` は CRS 情報無し。
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub crs: Option<Crs>,

    /// 平面 (`planar`) 球面 (`spherical`) の区別。v0.1 は `planar` のみ。
    #[serde(default)]
    pub edges: Edges,
}

/// ジオメトリのバイト列エンコーディング。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum GeometryEncoding {
    /// Well-Known Binary（OGC/ISO 標準、ISO/IEC 13249-3）。
    #[serde(rename = "WKB")]
    Wkb,
}

/// ジオメトリ型のタグ。GeoParquet 仕様のジオメトリ型名と揃える。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum GeometryType {
    /// 任意のジオメトリ型を許容する（混在）。
    Geometry,
    Point,
    LineString,
    Polygon,
    MultiPoint,
    MultiLineString,
    MultiPolygon,
    GeometryCollection,
}

/// 座標エッジの解釈。
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Edges {
    /// 平面（直交座標）。一般的な投影座標系で用いる。
    #[default]
    Planar,
    /// 球面（大圏弧）。地理座標系の地球表面 geometry に用いる。
    Spherical,
}

impl GeometryMeta {
    /// 列に WKB Polygon を保存する場合の典型的な [`GeometryMeta`] を作る。
    pub fn wkb(geometry_type: GeometryType, crs: Option<Crs>) -> Self {
        Self {
            encoding: GeometryEncoding::Wkb,
            geometry_type,
            crs,
            edges: Edges::Planar,
        }
    }

    /// JSON 文字列にシリアライズする（Arrow field metadata 用）。
    pub fn to_json(&self) -> crate::Result<String> {
        serde_json::to_string(self).map_err(|e| crate::Error::Schema(e.to_string()))
    }

    /// JSON 文字列からデシリアライズする。
    pub fn from_json(s: &str) -> crate::Result<Self> {
        serde_json::from_str(s).map_err(|e| crate::Error::Schema(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_minimal_polygon_with_epsg() {
        let meta = GeometryMeta::wkb(GeometryType::Polygon, Some(Crs::from_epsg(4326)));
        let json = meta.to_json().unwrap();
        // authority のみが入った最小 JSON になっているはず（wkt/projjson は省略）。
        assert!(json.contains("\"encoding\":\"WKB\""));
        assert!(json.contains("\"geometry_type\":\"Polygon\""));
        assert!(json.contains("\"authority\":[\"EPSG\",4326]"));
        assert!(json.contains("\"edges\":\"planar\""));
        assert!(!json.contains("\"wkt\""));
        assert!(!json.contains("\"projjson\""));

        let back = GeometryMeta::from_json(&json).unwrap();
        assert_eq!(back, meta);
    }

    #[test]
    fn roundtrip_without_crs() {
        let meta = GeometryMeta::wkb(GeometryType::Point, None);
        let json = meta.to_json().unwrap();
        assert!(!json.contains("\"crs\""));
        let back = GeometryMeta::from_json(&json).unwrap();
        assert_eq!(back, meta);
    }
}
