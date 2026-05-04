//! SQL Server driver の bench 経路。PostGIS と同型 (URI に `?table=` を付与)。
//!
//! prepare 経路は **batch INSERT** (`open_write`) を使う。tiberius 0.12 の bulk
//! (bcp) は `datetimeoffset` / `varbinary(max)` の組み合わせで `Invalid column
//! type from bcp client` を返す既知不整合があり (v0.4 cycle 3b の bench は最小
//! schema を採用してこの問題を避けた)、bench-rss は型網羅 schema (Date32 +
//! Timestamp(us, UTC) + Binary を含む) で回すため bulk 経路は踏まない。reader
//! peak RSS の計測対象は writer 経路ではないので、prepare が遅くても結果に影響
//! しない。

use std::path::Path;

use shpx_core::{
    CreateTable, Driver, Error, LayerReader, OnLoss, ReadOpts, Result, Uri, WriteOpts,
};
use shpx_driver_parquet::ParquetDriver;
use shpx_driver_sqlserver::SqlServerDriver;

use super::{append_table_query, cache_valid, write_manifest, BenchDriver, NativeInput};

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
            prepare_via_insert(&SqlServerDriver, parquet_input, &uri)?;
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

/// `prepare_via_bulk` の `open_bulk_write` を踏まない版。tiberius bcp の既知
/// 不整合を避けるため SQL Server 専用にバッチ INSERT 経路で投入する。
fn prepare_via_insert(driver: &dyn Driver, parquet_input: &Path, out_uri: &Uri) -> Result<()> {
    let pq_uri = Uri::from_path(parquet_input.display().to_string());
    let mut pq = ParquetDriver.open_read(&pq_uri, &ReadOpts::default())?;
    let schema = pq.schema();
    let crs = pq.crs().cloned();
    let opts = WriteOpts {
        overwrite: true,
        on_loss: OnLoss::Error,
        create_table: CreateTable::Always,
        ..Default::default()
    };
    let mut writer = driver.open_write(out_uri, schema, crs, &opts)?;
    let iter = pq.batches();
    for batch in iter {
        let batch = batch?;
        writer.write_batch(&batch)?;
    }
    writer.finish()?;
    Ok(())
}
