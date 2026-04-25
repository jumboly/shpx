//! shpx-driver-csv — WKT 列付き CSV/TSV の読み書きドライバ。
//!
//! - geometry 列は WKT 文字列として保存し、Arrow 中間表現では `Binary` (WKB) に変換する。
//! - 数値・日付の型推定は行わず、geometry 以外は **すべて `Utf8`** で読む。型保全用途は GeoParquet を使う前提。
//! - CSV 固有オプションは v0.2 サイクル 1 では環境変数（`SHPX_CSV_*`）で受け取る。
//!   詳細は `docs/CSV.md` を参照。

use arrow_schema::SchemaRef;
use shpx_core::{
    Capabilities, Crs, Driver, LayerReader, LayerWriter, ReadOpts, Result, StringEncoding, Uri,
    WriteOpts,
};

pub mod options;
pub mod reader;
pub mod util;
pub mod writer;

/// CSV ドライバ。ステートレスな factory。
#[derive(Debug, Default, Clone, Copy)]
pub struct CsvDriver;

impl CsvDriver {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

/// SHP と同じ encoding リストで揃える。次サイクルで `shpx-core` に共通定数を切り出す。
const SUPPORTED_ENCODINGS: &[&str] = &["utf-8", "cp932", "latin-1"];

impl Driver for CsvDriver {
    fn name(&self) -> &'static str {
        util::DRIVER_NAME
    }

    fn supported_schemes(&self) -> &[&'static str] {
        // `.csv` と `.tsv` を同一 driver で扱う。delimiter は scheme から決まる。
        &["csv", "tsv"]
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            read: true,
            write: true,
            random_access: false,
            bulk_load: false,
            // CSV はバイナリ列を表現できない（base64 等は別オプションで将来対応）。
            supports_blob: false,
            // decimal は文字列として保全するため、driver capabilities としては未サポート扱い。
            supports_decimal: false,
            // ISO 8601 文字列で +HH:MM のオフセットを保持できる。
            supports_timestamp_tz: true,
            string_encoding: StringEncoding::Configurable(SUPPORTED_ENCODINGS),
            max_decimal_precision: None,
        }
    }

    fn open_read(&self, uri: &Uri, opts: &ReadOpts) -> Result<Box<dyn LayerReader>> {
        let r = reader::CsvReader::open(uri, opts)?;
        Ok(Box::new(r))
    }

    fn open_write(
        &self,
        uri: &Uri,
        schema: SchemaRef,
        crs: Option<Crs>,
        opts: &WriteOpts,
    ) -> Result<Box<dyn LayerWriter>> {
        let w = writer::CsvWriter::open(uri, schema, crs, opts)?;
        Ok(Box::new(w))
    }
}

static CSV_DRIVER_INSTANCE: CsvDriver = CsvDriver;
shpx_core::inventory::submit! {
    shpx_core::DriverRegistration { driver: &CSV_DRIVER_INSTANCE }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn driver_basic_facts() {
        let d = CsvDriver::new();
        assert_eq!(d.name(), "csv");
        assert_eq!(d.supported_schemes(), &["csv", "tsv"]);
        let caps = d.capabilities();
        assert!(caps.read && caps.write);
        assert!(!caps.supports_blob);
        match caps.string_encoding {
            StringEncoding::Configurable(list) => {
                assert!(list.contains(&"utf-8"));
            }
            StringEncoding::Fixed(_) => panic!("csv must be configurable encoding"),
        }
    }
}
