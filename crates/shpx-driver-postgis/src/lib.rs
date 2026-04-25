//! shpx-driver-postgis — PostGIS (PostgreSQL + PostGIS extension) の `LayerReader` / `LayerWriter`。
//!
//! `pg://user:pass@host:port/db?table=<name>` で接続。reader は `SELECT ST_AsEWKB(geom), ...`
//! を 1 度発行して全件 in-memory に取る。writer は `--overwrite` で `DROP TABLE IF EXISTS` →
//! `CREATE TABLE` → 行単位 prepared INSERT を行う。geometry は `shpx_geom::ewkb` で EWKB と
//! 標準 WKB を相互変換する。サポート型・制限・将来計画は `docs/POSTGIS.md` を参照。

use arrow_schema::SchemaRef;
use shpx_core::{
    BulkLoadWriter, Capabilities, Crs, Driver, LayerReader, LayerWriter, ReadOpts, Result,
    StringEncoding, Uri, WriteOpts,
};

pub mod conn;
pub mod copy_binary;
pub mod options;
pub mod reader;
pub mod runtime;
pub mod type_map;
pub mod util;
pub mod writer;

#[derive(Debug, Default, Clone, Copy)]
pub struct PostgisDriver;

impl PostgisDriver {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

/// `Uri` 側で `postgres`/`postgresql`/`pg` を `pg` に正規化するため、driver は
/// `pg` のみを宣言する。CLI からの拡張子推論経路ではマッチしないので URL 入力専用。
const SUPPORTED_SCHEMES: &[&str] = &["pg"];

impl Driver for PostgisDriver {
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
            // cycle 2 で `BulkLoadWriter` (COPY BINARY) 対応。
            bulk_load: true,
            supports_blob: true,
            // cycle 2 で Decimal128 ↔ numeric の bind/decode を実装。
            supports_decimal: true,
            supports_timestamp_tz: true,
            string_encoding: StringEncoding::Fixed("utf-8"),
            // Decimal128 の Arrow 表現に合わせて 38 を上限にする。Decimal256 は未対応。
            max_decimal_precision: Some(38),
        }
    }

    fn open_read(&self, uri: &Uri, opts: &ReadOpts) -> Result<Box<dyn LayerReader>> {
        let r = reader::PostgisReader::open(uri, opts)?;
        Ok(Box::new(r))
    }

    fn open_write(
        &self,
        uri: &Uri,
        schema: SchemaRef,
        crs: Option<Crs>,
        opts: &WriteOpts,
    ) -> Result<Box<dyn LayerWriter>> {
        let w = writer::PostgisWriter::open(uri, schema, crs.as_ref(), opts)?;
        Ok(Box::new(w))
    }

    fn open_bulk_write(
        &self,
        uri: &Uri,
        schema: SchemaRef,
        crs: Option<Crs>,
        opts: &WriteOpts,
    ) -> Result<Option<Box<dyn BulkLoadWriter>>> {
        let w = writer::PostgisWriter::open(uri, schema, crs.as_ref(), opts)?;
        Ok(Some(Box::new(w)))
    }
}

static POSTGIS_DRIVER_INSTANCE: PostgisDriver = PostgisDriver;
shpx_core::inventory::submit! {
    shpx_core::DriverRegistration { driver: &POSTGIS_DRIVER_INSTANCE }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn driver_basic_facts() {
        let d = PostgisDriver::new();
        assert_eq!(d.name(), "postgis");
        assert_eq!(d.supported_schemes(), &["pg"]);
        let caps = d.capabilities();
        assert!(caps.read && caps.write);
        assert!(caps.supports_blob);
        assert!(caps.supports_timestamp_tz);
        // cycle 2 で BulkLoadWriter (COPY BINARY) と Decimal128 が両方対応済み。
        assert!(caps.bulk_load);
        assert!(caps.supports_decimal);
        assert_eq!(caps.max_decimal_precision, Some(38));
        match caps.string_encoding {
            StringEncoding::Fixed(label) => assert_eq!(label, "utf-8"),
            StringEncoding::Configurable(_) => panic!("must be utf-8 fixed"),
        }
    }
}
