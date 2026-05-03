//! Parquet driver の bench 経路。
//!
//! 入力 Parquet をそのまま `LayerReader` で読み出す identity 経路。`prepare` は
//! file copy も行わない (元の Parquet を再利用する)。

use std::path::Path;

use shpx_core::{Driver, LayerReader, ReadOpts, Result, Uri};
use shpx_driver_parquet::ParquetDriver;

use super::{BenchDriver, NativeInput};

pub struct ParquetBench;

impl BenchDriver for ParquetBench {
    fn name(&self) -> &'static str {
        "parquet"
    }

    fn prepare(&self, parquet_input: &Path, _dir: &Path, _rows: usize) -> Result<NativeInput> {
        Ok(NativeInput::File(parquet_input.to_path_buf()))
    }

    fn open_read(&self, native: &NativeInput) -> Result<Box<dyn LayerReader>> {
        let path = match native {
            NativeInput::File(p) => p,
            NativeInput::DbTable { .. } => {
                return Err(shpx_core::Error::Format(
                    "parquet driver expects file input".into(),
                ));
            }
        };
        let uri = Uri::from_path(path.display().to_string());
        ParquetDriver.open_read(&uri, &ReadOpts::default())
    }
}
