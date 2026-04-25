//! shpx-driver-parquet — GeoParquet (.parquet) の読み書きドライバ。
//!
//! `arrow` / `parquet` crate を直接利用し、ファイルレベル KeyValue メタデータの
//! `geo` キー（GeoParquet 1.x 仕様）と Arrow field metadata の
//! `shpx:geometry` キーを相互変換する。

use arrow_schema::SchemaRef;
use shpx_core::{
    Capabilities, Crs, Driver, LayerReader, LayerWriter, ReadOpts, Result, StringEncoding, Uri,
    WriteOpts,
};

pub mod geo_meta;
pub mod reader;
pub mod util;
pub mod writer;

pub use util::DRIVER_NAME;

/// GeoParquet ドライバ。ステートレスな factory。
#[derive(Debug, Default, Clone, Copy)]
pub struct ParquetDriver;

impl ParquetDriver {
    /// 新しいインスタンスを作成する。
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl Driver for ParquetDriver {
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
            supports_blob: true,
            supports_decimal: true,
            supports_timestamp_tz: true,
            // GeoParquet は文字列カラムが UTF-8 固定（Arrow Utf8/LargeUtf8 ともに UTF-8）。
            string_encoding: StringEncoding::Fixed("utf-8"),
            max_decimal_precision: Some(38),
        }
    }

    fn open_read(&self, uri: &Uri, opts: &ReadOpts) -> Result<Box<dyn LayerReader>> {
        let r = reader::ParquetReader::open(uri, opts)?;
        Ok(Box::new(r))
    }

    fn open_write(
        &self,
        uri: &Uri,
        schema: SchemaRef,
        crs: Option<Crs>,
        opts: &WriteOpts,
    ) -> Result<Box<dyn LayerWriter>> {
        let w = writer::ParquetWriter::open(uri, schema, crs, opts)?;
        Ok(Box::new(w))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn driver_basic_facts() {
        let d = ParquetDriver::new();
        assert_eq!(d.name(), "parquet");
        assert_eq!(d.supported_schemes(), &["parquet"]);
        let caps = d.capabilities();
        assert!(caps.read && caps.write);
        assert!(caps.supports_blob);
        assert!(caps.supports_decimal);
        assert!(caps.supports_timestamp_tz);
        assert_eq!(caps.max_decimal_precision, Some(38));
        match caps.string_encoding {
            StringEncoding::Fixed(s) => assert_eq!(s, "utf-8"),
            StringEncoding::Configurable(_) => panic!("parquet must be fixed UTF-8"),
        }
    }
}
