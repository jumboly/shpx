//! authority のみが分かっている CRS から、最小限の PROJJSON を組み立てる。
//!
//! GeoParquet 1.x の `geo.columns.<>.crs` は PROJJSON を期待する。
//! v0.1 は PROJ 非依存のため、`{"id": {"authority": "EPSG", "code": N}}` 形式の
//! 「id だけある PROJJSON」を出力する。GDAL/pyogrio/QGIS のリーダはこれを EPSG として解釈できる。

use serde_json::{json, Value};

/// EPSG コードのみを保持した最小 PROJJSON を返す。
///
/// 厳密には PROJJSON 仕様は `type` などのフィールドも要求するが、
/// 主要リーダは `id` だけを見て authority+code 解決にフォールバックする。
pub fn minimal_for_epsg(code: u32) -> Value {
    json!({
        "id": {
            "authority": "EPSG",
            "code": code,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shape_for_4326() {
        let v = minimal_for_epsg(4326);
        let s = serde_json::to_string(&v).unwrap();
        assert!(s.contains("\"authority\":\"EPSG\""));
        assert!(s.contains("\"code\":4326"));
    }
}
