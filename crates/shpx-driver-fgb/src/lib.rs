//! shpx-driver-fgb — FlatGeobuf (.fgb) の `LayerReader` / `LayerWriter`。
//!
//! FlatGeobuf 公式 Rust 実装 (`flatgeobuf` crate) を採用。geozero を経由して
//! shpx の WKB 中間表現と FGB 内部の FlatBuffers geometry を相互変換する。
//!
//! v0.2 cycle 4 のスコープ:
//! - 1 ファイル = 1 レイヤ（FGB 仕様）
//! - geometry: Point / LineString / Polygon / Multi*（XY のみ、Z/M 未対応）
//! - 属性型: Bool / Int8..64 / UInt8..32 / Float32/64 / Utf8 / Binary、Date32 / Timestamp は ISO8601 文字列で `DateTime` 列に保存
//! - CRS: EPSG コード優先 + WKT2 フォールバック
//! - 空間インデックス: 出力時は `index_node_size = 0` で書き出さない（v0.3+ で再評価）
//!
//! 詳細は `docs/FGB.md` を参照。

use arrow_schema::SchemaRef;
use shpx_core::{
    Capabilities, Crs, Driver, LayerReader, LayerWriter, ReadOpts, Result, StringEncoding, Uri,
    WriteOpts,
};

pub mod reader;
pub mod type_map;
pub mod util;
pub mod value;
pub mod writer;

/// FlatGeobuf ドライバ。ステートレスな factory。
#[derive(Debug, Default, Clone, Copy)]
pub struct FgbDriver;

impl FgbDriver {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

const SUPPORTED_SCHEMES: &[&str] = &["fgb"];

impl Driver for FgbDriver {
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
            random_access: false,
            // packed Hilbert R-Tree を使った bbox query は v0.3+ のスコープ外。
            bulk_load: false,
            // FGB は `Binary` カラム型を持つため `Binary` 属性列を保存可能。
            supports_blob: true,
            // FGB に Decimal 型は無く Double + width/precision/scale 注記で受ける。
            // 無損失とは言えないので `false`。
            supports_decimal: false,
            // FGB の `DateTime` カラム型は ISO8601 文字列。タイムゾーンは Z / ±HH:MM が許容される。
            supports_timestamp_tz: true,
            // FlatBuffers の `string` フィールドは UTF-8 固定。
            string_encoding: StringEncoding::Fixed("utf-8"),
            max_decimal_precision: None,
        }
    }

    fn open_read(&self, uri: &Uri, opts: &ReadOpts) -> Result<Box<dyn LayerReader>> {
        let r = reader::FgbReader::open(uri, opts)?;
        Ok(Box::new(r))
    }

    fn open_write(
        &self,
        uri: &Uri,
        schema: SchemaRef,
        crs: Option<Crs>,
        opts: &WriteOpts,
    ) -> Result<Box<dyn LayerWriter>> {
        let w = writer::FgbWriter::open(uri, schema, crs.as_ref(), opts)?;
        Ok(Box::new(w))
    }
}

static FGB_DRIVER_INSTANCE: FgbDriver = FgbDriver;
shpx_core::inventory::submit! {
    shpx_core::DriverRegistration { driver: &FGB_DRIVER_INSTANCE }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn driver_basic_facts() {
        let d = FgbDriver::new();
        assert_eq!(d.name(), "fgb");
        assert_eq!(d.supported_schemes(), &["fgb"]);
        let caps = d.capabilities();
        assert!(caps.read && caps.write);
        assert!(caps.supports_blob);
        assert!(!caps.supports_decimal);
        assert!(caps.supports_timestamp_tz);
        match caps.string_encoding {
            StringEncoding::Fixed(label) => assert_eq!(label, "utf-8"),
            StringEncoding::Configurable(_) => panic!("fgb must be utf-8 fixed"),
        }
    }
}
