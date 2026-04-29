//! shpx-driver-sqlserver — Microsoft SQL Server / Azure SQL の `LayerReader` /
//! `LayerWriter`。
//!
//! `mssql://user:pass@host:1433/db?table=<name>` で接続。reader は
//! `SELECT [geom].STAsBinary() AS [geom], ...` で WKB を取得する。writer は
//! `--insert-mode=bulk` 時に `#shpx_stage_<uuid>` 経由の staging bulk
//! (案 B、`docs/DESIGN.md` 参照) で `geometry::STGeomFromWKB(...)` に
//! 流し込む。サポート型・制限・将来計画は `docs/SQLSERVER.md`（cycle 3b で追加）を
//! 参照。
//!
//! 本ファイルは v0.4 cycle 1 commit 1 の workspace 配線時点。reader/writer の
//! 実装は cycle 1 commit 3 以降の各サブモジュールで埋めていく。

use arrow_schema::SchemaRef;
use shpx_core::{
    BulkLoadWriter, Capabilities, Crs, Driver, LayerReader, LayerWriter, ReadOpts, Result,
    StringEncoding, Uri, WriteOpts,
};

pub mod conn;
pub mod options;
pub mod runtime;
pub mod util;

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
            // cycle 2 で staging 経由 `BulkLoadWriter` を実装する。cycle 1 では prepared
            // INSERT のみなので false で開始する。
            bulk_load: false,
            supports_blob: true,
            // cycle 2 で `rust_decimal::Decimal` 経由の Decimal128 ↔ T-SQL `decimal(p,s)` を実装。
            supports_decimal: true,
            supports_timestamp_tz: true,
            string_encoding: StringEncoding::Fixed("utf-8"),
            // T-SQL `decimal` の最大精度。Decimal128 の Arrow 表現と一致。
            max_decimal_precision: Some(38),
        }
    }

    fn open_read(&self, _uri: &Uri, _opts: &ReadOpts) -> Result<Box<dyn LayerReader>> {
        // cycle 1 commit 3 で `reader::SqlServerReader::open` に差し替える。
        Err(util::driver_msg(
            "reader is not yet implemented (v0.4 cycle 1 commit 3)",
        ))
    }

    fn open_write(
        &self,
        _uri: &Uri,
        _schema: SchemaRef,
        _crs: Option<Crs>,
        _opts: &WriteOpts,
    ) -> Result<Box<dyn LayerWriter>> {
        // cycle 1 commit 4 で `writer::SqlServerWriter::open` に差し替える。
        Err(util::driver_msg(
            "writer is not yet implemented (v0.4 cycle 1 commit 4)",
        ))
    }

    fn open_bulk_write(
        &self,
        _uri: &Uri,
        _schema: SchemaRef,
        _crs: Option<Crs>,
        _opts: &WriteOpts,
    ) -> Result<Option<Box<dyn BulkLoadWriter>>> {
        // cycle 1 では bulk_load=false のため、`select_writer` 側でこの経路は呼ばれない。
        // cycle 2 で staging 経由実装に差し替える。
        Ok(None)
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
        // cycle 1 commit 1 時点では bulk_load は未実装。cycle 2 で true に切り替える。
        assert!(!caps.bulk_load);
        assert_eq!(caps.max_decimal_precision, Some(38));
        match caps.string_encoding {
            StringEncoding::Fixed(label) => assert_eq!(label, "utf-8"),
            StringEncoding::Configurable(_) => panic!("must be utf-8 fixed"),
        }
    }
}
