//! shpx-driver-shp — Shapefile (.shp/.shx/.dbf/.prj/.cpg) の読み書きドライバ。
//!
//! `shapefile` crate を基礎に、`.cpg` による文字エンコーディングと
//! DBF の decimal 精度（field length / decimal places）を取り扱う薄い拡張を加える。

use arrow_schema::SchemaRef;
use shpx_core::{
    Capabilities, Crs, Driver, LayerReader, LayerWriter, ReadOpts, Result, StringEncoding, Uri,
    WriteOpts,
};

pub mod cpg;
pub mod crs_io;
pub mod dbf_schema;
pub mod geometry;
pub mod reader;
pub mod util;
pub mod writer;

pub use util::DRIVER_NAME;

/// Shapefile ドライバ。ステートレスな factory。
#[derive(Debug, Default, Clone, Copy)]
pub struct ShpDriver;

impl ShpDriver {
    /// 新しいインスタンスを作成する。
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl Driver for ShpDriver {
    fn name(&self) -> &'static str {
        DRIVER_NAME
    }

    fn supported_schemes(&self) -> &[&'static str] {
        &[DRIVER_NAME]
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            read: true,
            write: true,
            random_access: false,
            bulk_load: false,
            supports_blob: false,
            supports_decimal: true,
            supports_timestamp_tz: false,
            // 第 1 要素 ("utf-8") が既定。Shapefile の慣習として
            // `.cpg` 不在時は UTF-8 をデフォルトに据える。
            string_encoding: StringEncoding::Configurable(&["utf-8", "cp932", "latin-1"]),
            max_decimal_precision: Some(18),
        }
    }

    fn open_read(&self, uri: &Uri, opts: &ReadOpts) -> Result<Box<dyn LayerReader>> {
        let r = reader::ShpReader::open(uri, opts)?;
        Ok(Box::new(r))
    }

    fn open_write(
        &self,
        uri: &Uri,
        schema: SchemaRef,
        crs: Option<Crs>,
        opts: &WriteOpts,
    ) -> Result<Box<dyn LayerWriter>> {
        let w = writer::ShpWriter::open(uri, &schema, crs.as_ref(), opts)?;
        Ok(Box::new(w))
    }
}

// `inventory` レジストリへの自動登録。`shpx-cli` がこの crate に dep を貼っている
// 限り、リンカは本ユニットを保持し、CLI 起動時に Driver が利用可能になる。
static SHP_DRIVER_INSTANCE: ShpDriver = ShpDriver;
shpx_core::inventory::submit! {
    shpx_core::DriverRegistration { driver: &SHP_DRIVER_INSTANCE }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capabilities_defaults_to_utf8_first() {
        let d = ShpDriver::new();
        assert_eq!(d.name(), "shp");
        assert_eq!(d.supported_schemes(), &["shp"]);
        let caps = d.capabilities();
        assert!(caps.read && caps.write);
        assert!(!caps.bulk_load);
        assert!(!caps.supports_blob);
        assert!(caps.supports_decimal);
        assert!(!caps.supports_timestamp_tz);
        assert_eq!(caps.max_decimal_precision, Some(18));
        match caps.string_encoding {
            StringEncoding::Configurable(list) => {
                // 既定エンコーディングは UTF-8（先頭要素）。`.cpg` 不在時のフォールバック。
                assert_eq!(list.first().copied(), Some("utf-8"));
            }
            StringEncoding::Fixed(_) => panic!("shp must be configurable"),
        }
    }
}
