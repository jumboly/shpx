//! shpx-driver-sqlserver — Microsoft SQL Server / Azure SQL の `LayerReader` /
//! `LayerWriter`。
//!
//! `mssql://user:pass@host:1433/db?table=<name>` で接続。reader は
//! `SELECT [geom].STAsBinary() AS [geom], ...` で WKB を取得し、writer は
//! `--insert-mode=bulk` 時に `#shpx_stage_<uuid>` 経由の staging bulk
//! (案 B、`docs/DESIGN.md` L.219-) で `geometry::STGeomFromWKB(...)` に流し込む。
//! サポート型・制限・将来計画は `docs/SQLSERVER.md` を参照。

use arrow_schema::SchemaRef;
use shpx_core::{
    BulkLoadWriter, Capabilities, Crs, Driver, LayerReader, LayerWriter, ReadOpts, Result,
    StringEncoding, Uri, WriteOpts,
};

pub mod bulk;
pub mod conn;
pub mod options;
pub mod reader;
pub mod runtime;
pub mod staging;
pub mod type_map;
pub mod util;
pub mod writer;

#[derive(Debug, Default, Clone, Copy)]
pub struct SqlServerDriver;

impl SqlServerDriver {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

/// `mssql` のみを宣言する。`sqlserver://` 等のエイリアス対応は v0.5+ で
/// `shpx-core::uri::normalize_scheme` を拡張する形で検討する。
const SUPPORTED_SCHEMES: &[&str] = &["mssql"];

impl Driver for SqlServerDriver {
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
            bulk_load: true,
            supports_blob: true,
            supports_decimal: true,
            supports_timestamp_tz: true,
            string_encoding: StringEncoding::Fixed("utf-8"),
            // T-SQL `decimal` の最大精度。Decimal128 の Arrow 表現と一致。
            max_decimal_precision: Some(38),
        }
    }

    fn open_read(&self, uri: &Uri, opts: &ReadOpts) -> Result<Box<dyn LayerReader>> {
        let r = reader::SqlServerReader::open(uri, opts)?;
        Ok(Box::new(r))
    }

    fn open_write(
        &self,
        uri: &Uri,
        schema: SchemaRef,
        crs: Option<Crs>,
        opts: &WriteOpts,
    ) -> Result<Box<dyn LayerWriter>> {
        let w = writer::SqlServerWriter::open(uri, schema, crs.as_ref(), opts)?;
        Ok(Box::new(w))
    }

    fn open_bulk_write(
        &self,
        uri: &Uri,
        schema: SchemaRef,
        crs: Option<Crs>,
        opts: &WriteOpts,
    ) -> Result<Option<Box<dyn BulkLoadWriter>>> {
        let w = writer::SqlServerWriter::open(uri, schema, crs.as_ref(), opts)?;
        Ok(Some(Box::new(w)))
    }
}

static SQLSERVER_DRIVER_INSTANCE: SqlServerDriver = SqlServerDriver;
shpx_core::inventory::submit! {
    shpx_core::DriverRegistration { driver: &SQLSERVER_DRIVER_INSTANCE }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn driver_basic_facts() {
        let d = SqlServerDriver::new();
        assert_eq!(d.name(), "sqlserver");
        assert_eq!(d.supported_schemes(), &["mssql"]);
        let caps = d.capabilities();
        assert!(caps.read && caps.write);
        assert!(caps.supports_blob);
        assert!(caps.supports_timestamp_tz);
        assert!(caps.supports_decimal);
        assert!(caps.bulk_load);
        assert_eq!(caps.max_decimal_precision, Some(38));
        match caps.string_encoding {
            StringEncoding::Fixed(label) => assert_eq!(label, "utf-8"),
            StringEncoding::Configurable(_) => panic!("must be utf-8 fixed"),
        }
    }
}
