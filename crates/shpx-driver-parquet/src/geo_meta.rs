//! GeoParquet `geo` KeyValue メタデータ ↔ shpx-core 中間表現の相互変換。
//!
//! GeoParquet 1.0.0 仕様（最小準拠）:
//! ```json
//! {
//!   "version": "1.0.0",
//!   "primary_column": "geometry",
//!   "columns": {
//!     "geometry": {
//!       "encoding": "WKB",
//!       "geometry_types": [],
//!       "crs": {"id": {"authority": "EPSG", "code": 4326}},
//!       "edges": "planar"
//!     }
//!   }
//! }
//! ```
//!
//! v0.1 は covering / bbox / orientation などの拡張フィールドを書き出さないが、
//! reader は forward-compat のため未知フィールドを無視する。

use serde_json::{json, Map, Value};
use shpx_core::{
    schema::{Edges, GeometryEncoding, GeometryMeta, GeometryType},
    Crs, Error, Result, WktFlavor,
};
use shpx_geom::projjson;

/// Parquet ファイルレベル KeyValue のキー名（GeoParquet 仕様）。
pub const GEO_KV_KEY: &str = "geo";
/// 既定の primary geometry 列名。
pub const DEFAULT_PRIMARY: &str = "geometry";

/// shpx の geometry メタから GeoParquet `geo` JSON 文字列を組み立てる。
///
/// `primary_column` には geometry 列の Arrow フィールド名を渡す。
/// CRS は EPSG が分かれば最小 PROJJSON、無ければ `null` を出力する。
pub fn build_geo_metadata(primary_column: &str, meta: &GeometryMeta) -> Result<String> {
    let crs_value = match &meta.crs {
        Some(c) => crs_to_projjson(c),
        None => Value::Null,
    };

    let mut col = Map::new();
    col.insert("encoding".to_string(), json!(encoding_to_str(meta.encoding)));
    // GeoParquet 仕様で必須。v0.1 は実列値を走査せず空配列で「混在許可」を表す。
    col.insert("geometry_types".to_string(), json!([]));
    col.insert("crs".to_string(), crs_value);
    col.insert("edges".to_string(), json!(edges_to_str(meta.edges)));

    let mut columns = Map::new();
    columns.insert(primary_column.to_string(), Value::Object(col));

    let root = json!({
        "version": "1.0.0",
        "primary_column": primary_column,
        "columns": Value::Object(columns),
    });
    serde_json::to_string(&root).map_err(|e| Error::Format(e.to_string()))
}

/// Parquet ファイル KeyValue から GeoParquet 情報を取り出す。
///
/// `geo` キーが無ければ `Ok(None)`。`geo` キーがあるが内容が不正な場合は `Err`。
/// 戻り値の primary_column 名は `Field::name()` の照合に使う。
pub fn parse_geo_metadata(geo_json: &str) -> Result<(String, GeometryMeta)> {
    let root: Value =
        serde_json::from_str(geo_json).map_err(|e| Error::Format(format!("geo metadata: {e}")))?;

    let primary = root
        .get("primary_column")
        .and_then(Value::as_str)
        .unwrap_or(DEFAULT_PRIMARY)
        .to_string();

    let columns = root
        .get("columns")
        .and_then(Value::as_object)
        .ok_or_else(|| Error::Format("geo metadata: `columns` missing or not object".into()))?;

    let col = columns.get(&primary).ok_or_else(|| {
        Error::Format(format!(
            "geo metadata: primary column `{primary}` not in `columns`"
        ))
    })?;

    let encoding = match col.get("encoding").and_then(Value::as_str) {
        Some("WKB") | None => GeometryEncoding::Wkb,
        Some(other) => {
            return Err(Error::Format(format!(
                "geo metadata: unsupported encoding `{other}` (only WKB in v0.1)"
            )))
        }
    };

    let geometry_type = parse_geometry_types(col.get("geometry_types"));
    let edges = match col.get("edges").and_then(Value::as_str) {
        Some("spherical") => Edges::Spherical,
        _ => Edges::Planar,
    };
    let crs = parse_crs(col.get("crs"));

    Ok((
        primary,
        GeometryMeta {
            encoding,
            geometry_type,
            crs,
            edges,
        },
    ))
}

fn encoding_to_str(e: GeometryEncoding) -> &'static str {
    match e {
        GeometryEncoding::Wkb => "WKB",
    }
}

fn edges_to_str(e: Edges) -> &'static str {
    match e {
        Edges::Planar => "planar",
        Edges::Spherical => "spherical",
    }
}

fn crs_to_projjson(c: &Crs) -> Value {
    if let Some(code) = c.epsg_code() {
        return projjson::minimal_for_epsg(code);
    }
    if let Some(s) = c.projjson.as_deref() {
        if let Ok(v) = serde_json::from_str::<Value>(s) {
            return v;
        }
    }
    Value::Null
}

/// `geometry_types` 配列の唯一値から代表型を導く。空配列・複数型・未知値は混在扱い。
fn parse_geometry_types(v: Option<&Value>) -> GeometryType {
    let Some(arr) = v.and_then(Value::as_array) else {
        return GeometryType::Geometry;
    };
    if arr.len() != 1 {
        return GeometryType::Geometry;
    }
    let Some(s) = arr[0].as_str() else {
        return GeometryType::Geometry;
    };
    // GeoParquet 仕様の `Z`/`M`/`ZM` サフィックスは v0.1 では無視して基底型のみ使う。
    let base = s.trim_end_matches(" ZM").trim_end_matches(" Z").trim_end_matches(" M");
    match base {
        "Point" => GeometryType::Point,
        "LineString" => GeometryType::LineString,
        "Polygon" => GeometryType::Polygon,
        "MultiPoint" => GeometryType::MultiPoint,
        "MultiLineString" => GeometryType::MultiLineString,
        "MultiPolygon" => GeometryType::MultiPolygon,
        "GeometryCollection" => GeometryType::GeometryCollection,
        _ => GeometryType::Geometry,
    }
}

/// `crs` フィールドの PROJJSON から `Crs` を組み立てる。`null` または欠落は `None`。
fn parse_crs(v: Option<&Value>) -> Option<Crs> {
    let v = v?;
    if v.is_null() {
        return None;
    }
    // 最小 PROJJSON: `{"id": {"authority": "EPSG", "code": N}}`
    let id = v.get("id")?;
    let authority = id.get("authority").and_then(Value::as_str)?.to_string();
    let code = id.get("code").and_then(Value::as_u64)?;
    let code = u32::try_from(code).ok()?;
    Some(Crs {
        authority: Some((authority, code)),
        wkt: None,
        wkt_flavor: WktFlavor::V2,
        projjson: Some(serde_json::to_string(v).ok()?),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_with_epsg_4326_writes_minimal_projjson() {
        let meta = GeometryMeta::wkb(GeometryType::Polygon, Some(Crs::from_epsg(4326)));
        let json = build_geo_metadata("geometry", &meta).unwrap();
        let v: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["version"], "1.0.0");
        assert_eq!(v["primary_column"], "geometry");
        assert_eq!(v["columns"]["geometry"]["encoding"], "WKB");
        assert_eq!(v["columns"]["geometry"]["edges"], "planar");
        assert_eq!(
            v["columns"]["geometry"]["crs"]["id"]["authority"],
            "EPSG"
        );
        assert_eq!(v["columns"]["geometry"]["crs"]["id"]["code"], 4326);
        assert!(v["columns"]["geometry"]["geometry_types"].is_array());
    }

    #[test]
    fn build_without_crs_yields_null_crs() {
        let meta = GeometryMeta::wkb(GeometryType::Point, None);
        let json = build_geo_metadata("geometry", &meta).unwrap();
        let v: Value = serde_json::from_str(&json).unwrap();
        assert!(v["columns"]["geometry"]["crs"].is_null());
    }

    #[test]
    fn parse_roundtrip_with_epsg() {
        let original = GeometryMeta::wkb(GeometryType::Polygon, Some(Crs::from_epsg(3857)));
        let json = build_geo_metadata("geometry", &original).unwrap();
        let (primary, parsed) = parse_geo_metadata(&json).unwrap();
        assert_eq!(primary, "geometry");
        assert_eq!(parsed.encoding, GeometryEncoding::Wkb);
        assert_eq!(parsed.geometry_type, GeometryType::Geometry);
        assert_eq!(parsed.crs.as_ref().and_then(Crs::epsg_code), Some(3857));
        assert_eq!(parsed.edges, Edges::Planar);
    }

    #[test]
    fn parse_geometry_types_singleton_uses_specific_type() {
        let json = r#"{
            "version": "1.0.0",
            "primary_column": "geom",
            "columns": {
                "geom": {
                    "encoding": "WKB",
                    "geometry_types": ["Polygon"],
                    "crs": null
                }
            }
        }"#;
        let (primary, meta) = parse_geo_metadata(json).unwrap();
        assert_eq!(primary, "geom");
        assert_eq!(meta.geometry_type, GeometryType::Polygon);
        assert!(meta.crs.is_none());
    }

    #[test]
    fn parse_unknown_extra_fields_ignored() {
        let json = r#"{
            "version": "1.1.0",
            "primary_column": "geometry",
            "columns": {
                "geometry": {
                    "encoding": "WKB",
                    "geometry_types": [],
                    "crs": null,
                    "edges": "planar",
                    "covering": {"bbox": {"xmin": ["bbox", "xmin"]}}
                }
            }
        }"#;
        let (_, meta) = parse_geo_metadata(json).unwrap();
        assert_eq!(meta.encoding, GeometryEncoding::Wkb);
    }

    #[test]
    fn parse_unsupported_encoding_errors() {
        let json = r#"{
            "version": "1.1.0",
            "primary_column": "geometry",
            "columns": {
                "geometry": {"encoding": "WKT", "geometry_types": []}
            }
        }"#;
        let err = parse_geo_metadata(json).unwrap_err();
        assert!(matches!(err, Error::Format(_)));
    }
}
