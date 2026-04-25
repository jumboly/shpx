//! shpx-driver-gpkg — GeoPackage (.gpkg) の `LayerReader` / `LayerWriter`。
//!
//! OGC GeoPackage 1.3 の feature テーブル方式（タイル / アトリビュート専用テーブルは未対応）に対応する。
//! - `gpkg_spatial_ref_sys` / `gpkg_contents` / `gpkg_geometry_columns` を初期化
//! - geometry 列は `Binary` (WKB) を [`shpx_geom::gpkg_blob`] でラップして保存
//! - SRS は EPSG コードから WKT1 を流し込み、それ以外は元 WKT を保持
//!
//! 詳細は `docs/GPKG.md` を参照。

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
pub struct GpkgDriver;

impl GpkgDriver {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

const SUPPORTED_SCHEMES: &[&str] = &["gpkg"];

impl Driver for GpkgDriver {
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
            // SQLite はトランザクションごと INSERT が pragmatic な最速ルートなので、
            // BulkLoadWriter は v0.2 では実装しない（行単位 INSERT で十分速い）。
            bulk_load: false,
            supports_blob: true,
            // Decimal128/256 は TEXT 降格扱いのため、無損失とは言えない。
            supports_decimal: false,
            supports_timestamp_tz: true,
            // SQLite TEXT は内部的に UTF-8 を要求する。
            string_encoding: StringEncoding::Fixed("utf-8"),
            max_decimal_precision: None,
        }
    }

    fn open_read(&self, uri: &Uri, opts: &ReadOpts) -> Result<Box<dyn LayerReader>> {
        let r = reader::GpkgReader::open(uri, opts)?;
        Ok(Box::new(r))
    }

    fn open_write(
        &self,
        uri: &Uri,
        schema: SchemaRef,
        crs: Option<Crs>,
        opts: &WriteOpts,
    ) -> Result<Box<dyn LayerWriter>> {
        let w = writer::GpkgWriter::open(uri, schema, crs.as_ref(), opts)?;
        Ok(Box::new(w))
    }
}

static GPKG_DRIVER_INSTANCE: GpkgDriver = GpkgDriver;
shpx_core::inventory::submit! {
    shpx_core::DriverRegistration { driver: &GPKG_DRIVER_INSTANCE }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn driver_basic_facts() {
        let d = GpkgDriver::new();
        assert_eq!(d.name(), "gpkg");
        assert_eq!(d.supported_schemes(), &["gpkg"]);
        let caps = d.capabilities();
        assert!(caps.read && caps.write);
        assert!(caps.supports_blob);
        assert!(!caps.supports_decimal);
        assert!(caps.supports_timestamp_tz);
        match caps.string_encoding {
            StringEncoding::Fixed(label) => assert_eq!(label, "utf-8"),
            StringEncoding::Configurable(_) => panic!("must be utf-8 fixed"),
        }
    }
}
