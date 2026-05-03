//! SQL Server driver の bench 経路。PostGIS と同型 (URI に `?table=` を付与)。

use std::path::Path;

use shpx_core::{Driver, Error, LayerReader, ReadOpts, Result, Uri};
use shpx_driver_sqlserver::SqlServerDriver;

use super::{
    append_table_query, cache_valid, prepare_via_bulk, write_manifest, BenchDriver, NativeInput,
};

pub struct SqlserverBench {
    pub url: String,
}

impl BenchDriver for SqlserverBench {
    fn name(&self) -> &'static str {
        "sqlserver"
    }

    fn prepare(&self, parquet_input: &Path, dir: &Path, rows: usize) -> Result<NativeInput> {
        let table = format!("shpx_bench_rss_{rows}");
        let manifest = dir.join(format!("sqlserver_{rows}.MANIFEST"));
        if !cache_valid(&manifest, rows) {
            let uri = Uri::from_path(append_table_query(&self.url, &table));
            prepare_via_bulk(&SqlServerDriver, parquet_input, &uri)?;
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
                return Err(Error::Format(
                    "sqlserver driver expects DbTable input".into(),
                ));
            }
        };
        let uri = Uri::from_path(append_table_query(url, table));
        SqlServerDriver.open_read(&uri, &ReadOpts::default())
    }
}
