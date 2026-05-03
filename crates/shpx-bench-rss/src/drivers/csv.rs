//! CSV driver の bench 経路。Binary 不可なので Skip 降格。

use std::path::Path;

use shpx_core::{Driver, LayerReader, OnLoss, ReadOpts, Result, Uri};
use shpx_driver_csv::CsvDriver;

use super::shp::file_path;
use super::{cache_valid, prepare_via_writer, write_manifest, BenchDriver, NativeInput};

pub struct CsvBench;

impl BenchDriver for CsvBench {
    fn name(&self) -> &'static str {
        "csv"
    }

    fn prepare(&self, parquet_input: &Path, dir: &Path, rows: usize) -> Result<NativeInput> {
        let out = dir.join(format!("csv_{rows}.csv"));
        let manifest = dir.join(format!("csv_{rows}.MANIFEST"));
        if !cache_valid(&manifest, rows) {
            let _ = std::fs::remove_file(&out);
            let uri = Uri::from_path(out.display().to_string());
            prepare_via_writer(&CsvDriver, parquet_input, &uri, OnLoss::Skip)?;
            write_manifest(&manifest, rows)?;
        }
        Ok(NativeInput::File(out))
    }

    fn open_read(&self, native: &NativeInput) -> Result<Box<dyn LayerReader>> {
        let path = file_path(native, "csv")?;
        CsvDriver.open_read(&Uri::from_path(path.display().to_string()), &ReadOpts::default())
    }
}
