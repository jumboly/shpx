//! SHP driver の bench 経路。
//!
//! SHP は `.shp` / `.shx` / `.dbf` (+ `.prj` / `.cpg`) の sidecar 群を生成する。
//! cache 再生成時に古い sidecar を残すと shapefile reader が部分破損として読みに
//! いくため、manifest mismatch 時は **同 stem の関連 file を全削除**してから書く。

use std::path::Path;

use shpx_core::{Driver, Error, LayerReader, OnLoss, ReadOpts, Result, Uri};
use shpx_driver_shp::ShpDriver;

use super::{cache_valid, prepare_via_writer, write_manifest, BenchDriver, NativeInput};

const SHP_SIDECARS: &[&str] = &["shp", "shx", "dbf", "prj", "cpg"];

pub struct ShpBench;

impl BenchDriver for ShpBench {
    fn name(&self) -> &'static str {
        "shp"
    }

    fn prepare(&self, parquet_input: &Path, dir: &Path, rows: usize) -> Result<NativeInput> {
        let out = dir.join(format!("shp_{rows}.shp"));
        let manifest = dir.join(format!("shp_{rows}.MANIFEST"));
        if !cache_valid(&manifest, rows) {
            for ext in SHP_SIDECARS {
                let _ = std::fs::remove_file(out.with_extension(ext));
            }
            let uri = Uri::from_path(out.display().to_string());
            // SHP は Binary / Timestamp_tz が型として乗らないため Skip 降格。
            prepare_via_writer(&ShpDriver, parquet_input, &uri, OnLoss::Skip)?;
            write_manifest(&manifest, rows)?;
        }
        Ok(NativeInput::File(out))
    }

    fn open_read(&self, native: &NativeInput) -> Result<Box<dyn LayerReader>> {
        let path = file_path(native, "shp")?;
        ShpDriver.open_read(
            &Uri::from_path(path.display().to_string()),
            &ReadOpts::default(),
        )
    }
}

pub(super) fn file_path<'a>(native: &'a NativeInput, driver: &'static str) -> Result<&'a Path> {
    match native {
        NativeInput::File(p) => Ok(p.as_path()),
        NativeInput::DbTable { .. } => {
            Err(Error::Format(format!("{driver} driver expects file input")))
        }
    }
}
