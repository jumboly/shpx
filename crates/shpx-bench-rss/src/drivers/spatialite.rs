//! SpatiaLite driver の bench 経路。GPKG と同じく URI `?table=` でテーブル指定。
//!
//! 動的 link 環境では `mod_spatialite` が `SPATIALITE_LIBRARY_PATH` 等で発見できる
//! 必要がある。CI ではビルド時に `--features bundled-spatialite` を有効化する。

use std::path::Path;

use shpx_core::{Driver, LayerReader, OnLoss, ReadOpts, Result, Uri};
use shpx_driver_spatialite::SpatialiteDriver;

use super::shp::file_path;
use super::{
    append_table_query, cache_valid, prepare_via_writer, write_manifest, BenchDriver, NativeInput,
};

pub struct SpatialiteBench;

impl BenchDriver for SpatialiteBench {
    fn name(&self) -> &'static str {
        "spatialite"
    }

    fn prepare(&self, parquet_input: &Path, dir: &Path, rows: usize) -> Result<NativeInput> {
        let out = dir.join(format!("spatialite_{rows}.sqlite"));
        let manifest = dir.join(format!("spatialite_{rows}.MANIFEST"));
        if !cache_valid(&manifest, rows) {
            let _ = std::fs::remove_file(&out);
            let _ = std::fs::remove_file(out.with_extension("sqlite-wal"));
            let _ = std::fs::remove_file(out.with_extension("sqlite-shm"));
            let uri = Uri::from_path(append_table_query(&out.display().to_string(), "bench"));
            prepare_via_writer(&SpatialiteDriver, parquet_input, &uri, OnLoss::Warn)?;
            write_manifest(&manifest, rows)?;
        }
        Ok(NativeInput::File(out))
    }

    fn open_read(&self, native: &NativeInput) -> Result<Box<dyn LayerReader>> {
        let path = file_path(native, "spatialite")?;
        let uri = Uri::from_path(append_table_query(&path.display().to_string(), "bench"));
        SpatialiteDriver.open_read(&uri, &ReadOpts::default())
    }
}
