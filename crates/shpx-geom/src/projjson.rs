//! authority のみが分かっている CRS から、最小限の PROJJSON を組み立てる encode と、
//! GeoParquet などから渡ってきた PROJJSON を `Crs` に best-effort で取り込む decode の両方。
//!
//! GeoParquet 1.x の `geo.columns.<>.crs` は PROJJSON を期待する。
//! v0.1 は PROJ 非依存のため、`{"id": {"authority": "EPSG", "code": N}}` 形式の
//! 「id だけある PROJJSON」を出力する。GDAL/pyogrio/QGIS のリーダはこれを EPSG として解釈できる。
//!
//! decode 側は逆方向: GeoParquet 1.1 が出す full PROJJSON (datum / coordinate_system 等を含む)
//! を受け取った場合、`id` から authority を抽出しつつ、原文 PROJJSON 全体を `Crs.projjson` に保存する。
//! 非 EPSG CRS (例: ESRI:102100) もこれで `Crs` 内に保持できる。

use serde_json::{json, Value};
use shpx_core::{Crs, WktFlavor};

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

/// PROJJSON `Value` から `Crs` を best-effort で組み立てる。
///
/// - `Value::Null` や非オブジェクトは `None`（CRS 情報無し）
/// - `id.authority` + `id.code` (整数) があれば `Crs.authority` を埋める
/// - 入力 PROJJSON 全体は `Crs.projjson` に保存し、authority に落とせない情報を温存する
/// - `id` が欠けていても、オブジェクトであれば `Crs.projjson` のみ埋めた `Some` を返す
pub fn decode_value(v: &Value) -> Option<Crs> {
    if !v.is_object() {
        return None;
    }

    let authority = v.get("id").and_then(|id| {
        let auth = id.get("authority").and_then(Value::as_str)?;
        let code = id.get("code").and_then(Value::as_u64)?;
        let code = u32::try_from(code).ok()?;
        Some((auth.to_string(), code))
    });

    let projjson = serde_json::to_string(v).ok();

    Some(Crs {
        authority,
        wkt: None,
        wkt_flavor: WktFlavor::V2,
        projjson,
    })
}

/// JSON 文字列を parse し、PROJJSON として `Crs` を best-effort で抽出する。
///
/// JSON が壊れている場合のみ `Err`、`null` や object でない場合は `Ok(None)`。
pub fn decode(json: &str) -> Result<Option<Crs>, serde_json::Error> {
    let v: Value = serde_json::from_str(json)?;
    Ok(decode_value(&v))
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

    #[test]
    fn decode_minimal_epsg_4326() {
        let crs = decode(r#"{"id":{"authority":"EPSG","code":4326}}"#)
            .unwrap()
            .unwrap();
        assert_eq!(crs.epsg_code(), Some(4326));
        assert!(crs.projjson.is_some());
    }

    #[test]
    fn decode_non_epsg_authority_kept_in_authority() {
        // ESRI authority も id 形式なら authority に落ちる
        let crs = decode(r#"{"id":{"authority":"ESRI","code":102100}}"#)
            .unwrap()
            .unwrap();
        assert_eq!(crs.epsg_code(), None);
        assert_eq!(crs.authority, Some(("ESRI".to_string(), 102_100)));
        assert!(crs.projjson.as_deref().unwrap().contains("ESRI"));
    }

    #[test]
    fn decode_full_projjson_without_id_keeps_projjson() {
        // datum / coordinate_system だけの PROJJSON でも projjson は温存する
        let json = r#"{
            "type": "GeographicCRS",
            "name": "Custom",
            "datum": {"type": "GeodeticReferenceFrame", "name": "Custom Datum"},
            "coordinate_system": {"subtype": "ellipsoidal", "axis": []}
        }"#;
        let crs = decode(json).unwrap().unwrap();
        assert_eq!(crs.authority, None);
        assert!(crs
            .projjson
            .as_deref()
            .unwrap()
            .contains("\"name\":\"Custom\""));
    }

    #[test]
    fn decode_null_returns_none() {
        assert!(decode("null").unwrap().is_none());
    }

    #[test]
    fn decode_invalid_json_errors() {
        assert!(decode("{not json").is_err());
    }

    #[test]
    fn decode_value_handles_null() {
        assert!(decode_value(&Value::Null).is_none());
        assert!(decode_value(&json!("string")).is_none());
        assert!(decode_value(&json!(42)).is_none());
    }
}
