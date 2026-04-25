//! shpx-driver-geojson — GeoJSON / GeoJSONL の読み書きドライバ。
//!
//! - `.geojson` — 単一の `FeatureCollection` オブジェクト
//! - `.geojsonl` / `.ndjson` / `.jsonl` — 1 行 1 Feature (NDJSON)
//!
//! どちらも 1 ドライバで扱い、`Uri::scheme` から出力形式を切り替える。
//! ジオメトリは `geojson::Geometry` ↔ `shpx_geom::Geom` を経由して WKB に詰め直し、
//! Arrow 中間表現では `Binary` 列 + `shpx:geometry` メタデータで保持する。
//!
//! 詳細仕様は `docs/GEOJSON.md` を参照。

use arrow_schema::SchemaRef;
use shpx_core::{
    Capabilities, Crs, Driver, LayerReader, LayerWriter, ReadOpts, Result, StringEncoding, Uri,
    WriteOpts,
};

pub mod geom_convert;
pub mod options;
pub mod reader;
pub mod util;
pub mod writer;

/// GeoJSON ドライバ。ステートレスな factory。
#[derive(Debug, Default, Clone, Copy)]
pub struct GeoJsonDriver;

impl GeoJsonDriver {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

/// 担当 scheme（`Uri::from_path` が小文字化済みの拡張子と照合される）。
const SUPPORTED_SCHEMES: &[&str] = &["geojson", "geojsonl", "ndjson", "jsonl"];

impl Driver for GeoJsonDriver {
    fn name(&self) -> &'static str {
        util::DRIVER_NAME
    }

    fn supported_schemes(&self) -> &[&'static str] {
        SUPPORTED_SCHEMES
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            read: true,
            write: true,
            // FeatureCollection / GeoJSONL のいずれも先頭から逐次読みのため。
            random_access: false,
            bulk_load: false,
            // GeoJSON はバイナリ列を表現できない（base64 経由は v1 で検討、`docs/GEOJSON.md` 参照）。
            supports_blob: false,
            // Decimal は JSON Number へ正確に詰めると f64 経由で精度欠落する。
            // 文字列降格 (Warn 経路) に留めるため driver capability としては未サポート扱い。
            supports_decimal: false,
            // ISO 8601 文字列で +HH:MM オフセットを保持できる（CSV と同じ）。
            supports_timestamp_tz: true,
            // RFC 8259 / RFC 7946 上 JSON は UTF-8 固定。
            string_encoding: StringEncoding::Fixed("utf-8"),
            max_decimal_precision: None,
        }
    }

    fn open_read(&self, uri: &Uri, opts: &ReadOpts) -> Result<Box<dyn LayerReader>> {
        let r = reader::GeoJsonReader::open(uri, opts)?;
        Ok(Box::new(r))
    }

    fn open_write(
        &self,
        uri: &Uri,
        schema: SchemaRef,
        crs: Option<Crs>,
        opts: &WriteOpts,
    ) -> Result<Box<dyn LayerWriter>> {
        let w = writer::GeoJsonWriter::open(uri, schema, crs.as_ref(), opts)?;
        Ok(Box::new(w))
    }
}

static GEOJSON_DRIVER_INSTANCE: GeoJsonDriver = GeoJsonDriver;
shpx_core::inventory::submit! {
    shpx_core::DriverRegistration { driver: &GEOJSON_DRIVER_INSTANCE }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn driver_basic_facts() {
        let d = GeoJsonDriver::new();
        assert_eq!(d.name(), "geojson");
        assert_eq!(
            d.supported_schemes(),
            &["geojson", "geojsonl", "ndjson", "jsonl"]
        );
        let caps = d.capabilities();
        assert!(caps.read && caps.write);
        assert!(!caps.supports_blob);
        assert!(!caps.supports_decimal);
        assert!(caps.supports_timestamp_tz);
        match caps.string_encoding {
            StringEncoding::Fixed(label) => assert_eq!(label, "utf-8"),
            StringEncoding::Configurable(_) => {
                panic!("geojson must be utf-8 fixed encoding")
            }
        }
    }
}
