//! GPKG (GeoPackage / SQLite) driver の bench 経路。
//!
//! テーブル名は URI の `?table=` クエリで指定する。同 file 内に複数 driver の bench
//! 結果を残すと cache key 区別が面倒になるため、`gpkg_<rows>.gpkg` の単一テーブル
//! `bench` に書き出して都度 overwrite する。

use std::path::Path;

use shpx_core::{Driver, LayerReader, OnLoss, ReadOpts, Result, Uri};
use shpx_driver_gpkg::GpkgDriver;

use super::shp::file_path;
use super::{
    append_table_query, cache_valid, prepare_via_writer, write_manifest, BenchDriver, NativeInput,
};

pub struct GpkgBench;

impl BenchDriver for GpkgBench {
    fn name(&self) -> &'static str {
        "gpkg"
    }

    fn prepare(&self, parquet_input: &Path, dir: &Path, rows: usize) -> Result<NativeInput> {
        let out = dir.join(format!("gpkg_{rows}.gpkg"));
        let manifest = dir.join(format!("gpkg_{rows}.MANIFEST"));
        if !cache_valid(&manifest, rows) {
            // SQLite WAL モードの sidecar が古い接続由来で残っていると open 時に
            // 不整合になることがあるため、main file と一緒に削除する。
            let _ = std::fs::remove_file(&out);
            let _ = std::fs::remove_file(out.with_extension("gpkg-wal"));
            let _ = std::fs::remove_file(out.with_extension("gpkg-shm"));
            let uri = Uri::from_path(append_table_query(&out.display().to_string(), "bench"));
            prepare_via_writer(&GpkgDriver, parquet_input, &uri, OnLoss::Warn)?;
            write_manifest(&manifest, rows)?;
        }
        Ok(NativeInput::File(out))
    }

    fn open_read(&self, native: &NativeInput) -> Result<Box<dyn LayerReader>> {
        let path = file_path(native, "gpkg")?;
        let uri = Uri::from_path(append_table_query(&path.display().to_string(), "bench"));
        GpkgDriver.open_read(&uri, &ReadOpts::default())
    }
}
