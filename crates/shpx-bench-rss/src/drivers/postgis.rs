//! PostGIS driver の bench 経路。
//!
//! `--postgis-url` (env: `SHPX_TEST_PG_URL`) の URL に `?table=shpx_bench_rss_<rows>`
//! を付与してテーブルを毎回 overwrite create する。bulk 経路 (`open_bulk_write` →
//! `bulk_write`) を優先利用 (`prepare_via_bulk` で COPY BINARY 経路に乗る)。

use std::path::Path;

use shpx_core::{Driver, Error, LayerReader, ReadOpts, Result, Uri};
use shpx_driver_postgis::PostgisDriver;

use super::{
    append_table_query, cache_valid, prepare_via_bulk, write_manifest, BenchDriver, NativeInput,
};

pub struct PostgisBench {
    pub url: String,
}

impl BenchDriver for PostgisBench {
    fn name(&self) -> &'static str {
        "postgis"
    }

    fn prepare(&self, parquet_input: &Path, dir: &Path, rows: usize) -> Result<NativeInput> {
        let table = format!("shpx_bench_rss_{rows}");
        let manifest = dir.join(format!("postgis_{rows}.MANIFEST"));
        if !cache_valid(&manifest, rows) {
            let uri = Uri::from_path(append_table_query(&self.url, &table));
            prepare_via_bulk(&PostgisDriver, parquet_input, &uri)?;
            write_manifest(&manifest, rows)?;
        }
        Ok(NativeInput::DbTable {
            url: self.url.clone(),
            table,
        })
    }

    fn open_read(&self, native: &NativeInput) -> Result<Box<dyn LayerReader>> {
        let (url, table) = match native {
            NativeInput::DbTable { url, table } => (url, table),
            NativeInput::File(_) => {
                return Err(Error::Format("postgis driver expects DbTable input".into()));
            }
        };
        let uri = Uri::from_path(append_table_query(url, table));
        PostgisDriver.open_read(&uri, &ReadOpts::default())
    }
}
