//! FGB (FlatGeobuf) driver の bench 経路。1 ファイル format。

use std::path::Path;

use shpx_core::{Driver, LayerReader, OnLoss, ReadOpts, Result, Uri};
use shpx_driver_fgb::FgbDriver;

use super::shp::file_path;
use super::{cache_valid, prepare_via_writer, write_manifest, BenchDriver, NativeInput};

pub struct FgbBench;

impl BenchDriver for FgbBench {
    fn name(&self) -> &'static str {
        "fgb"
    }

    fn prepare(&self, parquet_input: &Path, dir: &Path, rows: usize) -> Result<NativeInput> {
        let out = dir.join(format!("fgb_{rows}.fgb"));
        let manifest = dir.join(format!("fgb_{rows}.MANIFEST"));
        if !cache_valid(&manifest, rows) {
            let _ = std::fs::remove_file(&out);
            let uri = Uri::from_path(out.display().to_string());
            // Decimal は Double 降格、それ以外は概ねロスレス。
            prepare_via_writer(&FgbDriver, parquet_input, &uri, OnLoss::Warn)?;
            write_manifest(&manifest, rows)?;
        }
        Ok(NativeInput::File(out))
    }

    fn open_read(&self, native: &NativeInput) -> Result<Box<dyn LayerReader>> {
        let path = file_path(native, "fgb")?;
        FgbDriver.open_read(
            &Uri::from_path(path.display().to_string()),
            &ReadOpts::default(),
        )
    }
}
