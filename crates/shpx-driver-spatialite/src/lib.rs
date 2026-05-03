//! shpx-driver-spatialite — SpatiaLite (`sqlite://`, `*.sqlite`, `*.db`) ドライバ。
//!
//! `rusqlite` + `mod_spatialite` 動的ロードで SpatiaLite 4.x の geometry 列を読み書きする。
//! GPKG driver と同じファイルベース DB だが、blob format / メタテーブル / 必要 extension が異なる。
//! 詳細は v0.5 cycle で追加される `docs/SPATIALITE.md` を参照。

// `bundled-spatialite` 有効時に libgeos (C++) を static link する。`link-cplusplus`
// crate の build.rs が C++ stdlib リンク指定を出すが、この crate を実コードから
// 参照していないと Rust 1.x の autolink が build script のメタデータを最終バイナリの
// link graph に伝搬しないため、明示的に no-op で参照する。
#[cfg(feature = "bundled-spatialite")]
use link_cplusplus as _;

use arrow_schema::SchemaRef;
use shpx_core::{
    Capabilities, Crs, Driver, LayerReader, LayerWriter, ReadOpts, Result, StringEncoding, Uri,
    WriteOpts,
};

pub mod conn;
pub mod meta;
pub mod options;
pub mod reader;
pub mod type_map;
pub mod util;
pub mod writer;

#[derive(Debug, Default, Clone, Copy)]
pub struct SpatialiteDriver;

impl SpatialiteDriver {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

/// `sqlite://path` URL と `.sqlite` / `.db` / `.spatialite` 拡張子をすべて受け持つ。
/// `.gpkg` は別 driver (gpkg) で処理する。
const SUPPORTED_SCHEMES: &[&str] = &["sqlite", "db", "spatialite"];

impl Driver for SpatialiteDriver {
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
            // SQLite はトランザクションごと INSERT が pragmatic な最速ルート。
            // BulkLoadWriter は v0.5 範囲外（GPKG driver と同方針）。
            bulk_load: false,
            supports_blob: true,
            // Decimal は SQLite TEXT 降格扱いのため無損失でない。
            supports_decimal: false,
            supports_timestamp_tz: true,
            string_encoding: StringEncoding::Fixed("utf-8"),
            max_decimal_precision: None,
        }
    }

    fn open_read(&self, uri: &Uri, opts: &ReadOpts) -> Result<Box<dyn LayerReader>> {
        let r = reader::SpatialiteReader::open(uri, opts)?;
        Ok(Box::new(r))
    }

    fn open_write(
        &self,
        uri: &Uri,
        schema: SchemaRef,
        crs: Option<Crs>,
        opts: &WriteOpts,
    ) -> Result<Box<dyn LayerWriter>> {
        let w = writer::SpatialiteWriter::open(uri, schema, crs.as_ref(), opts)?;
        Ok(Box::new(w))
    }
}

static SPATIALITE_DRIVER_INSTANCE: SpatialiteDriver = SpatialiteDriver;
shpx_core::inventory::submit! {
    shpx_core::DriverRegistration { driver: &SPATIALITE_DRIVER_INSTANCE }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn driver_basic_facts() {
        let d = SpatialiteDriver::new();
        assert_eq!(d.name(), "spatialite");
        assert_eq!(d.supported_schemes(), &["sqlite", "db", "spatialite"]);
        let caps = d.capabilities();
        assert!(caps.read && caps.write);
        assert!(caps.supports_blob);
        assert!(!caps.supports_decimal);
        assert!(!caps.bulk_load);
        match caps.string_encoding {
            StringEncoding::Fixed(label) => assert_eq!(label, "utf-8"),
            StringEncoding::Configurable(_) => panic!("must be utf-8 fixed"),
        }
    }
}
