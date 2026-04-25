//! Arrow `RecordBatch` ストリームから GeoParquet ファイルを生成する。

use std::fs::{File, OpenOptions};
use std::path::PathBuf;

use arrow_array::RecordBatch;
use arrow_schema::SchemaRef;
use parquet::arrow::ArrowWriter;
use parquet::file::metadata::KeyValue;
use parquet::file::properties::WriterProperties;
use shpx_core::{
    schema::{require_geometry_column, GeometryMeta},
    Crs, Error, LayerWriter, Result, Uri, WriteOpts,
};

use crate::geo_meta::{self, GEO_KV_KEY};
use crate::util::driver_err;

/// GeoParquet の `LayerWriter` 実装。
///
/// 状態は `inner: Option<...>` 1 つで表現する。`Some` なら未 finalize、
/// `None` なら `finish()` 済みもしくは drop 中。
pub struct ParquetWriter {
    inner: Option<ArrowWriter<File>>,
}

impl ParquetWriter {
    pub fn open(
        uri: &Uri,
        schema: SchemaRef,
        crs: Option<Crs>,
        opts: &WriteOpts,
    ) -> Result<Self> {
        let path = PathBuf::from(uri.path());

        // overwrite=false で既存ファイルがあれば create_new が EEXIST を返す。
        let file = if opts.overwrite {
            OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(&path)?
        } else {
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .map_err(|e| {
                    if e.kind() == std::io::ErrorKind::AlreadyExists {
                        Error::Format(format!(
                            "output already exists: {} (use --overwrite)",
                            path.display()
                        ))
                    } else {
                        Error::Io(e)
                    }
                })?
        };

        // geometry 列を検出して `geo` JSON を組み立てる。
        let (primary_column, geom_meta) = build_primary_meta(&schema, crs)?;
        let geo_json = geo_meta::build_geo_metadata(&primary_column, &geom_meta)?;

        let mut props_builder = WriterProperties::builder().set_key_value_metadata(Some(vec![
            KeyValue {
                key: GEO_KV_KEY.to_string(),
                value: Some(geo_json),
            },
        ]));
        if let Some(n) = opts.batch_size_hint {
            if n > 0 {
                props_builder = props_builder.set_max_row_group_size(n);
            }
        }
        let props = props_builder.build();

        let inner =
            ArrowWriter::try_new(file, schema, Some(props)).map_err(|e| driver_err(&e))?;
        Ok(Self { inner: Some(inner) })
    }
}

/// schema から geometry 列名と書き出し用の `GeometryMeta` を組み立てる。
///
/// 引数の `crs`（reader 由来 or `--src-crs` 等で確定したもの）があれば、
/// schema の field metadata に書かれた `crs` よりそちらを優先する。
fn build_primary_meta(schema: &SchemaRef, crs: Option<Crs>) -> Result<(String, GeometryMeta)> {
    let (_idx, name, mut meta) = require_geometry_column(schema)?;
    if crs.is_some() {
        meta.crs = crs;
    }
    Ok((name, meta))
}

impl LayerWriter for ParquetWriter {
    fn write_batch(&mut self, batch: &RecordBatch) -> Result<()> {
        let inner = self
            .inner
            .as_mut()
            .ok_or_else(|| Error::Format("ParquetWriter already finished".into()))?;
        inner.write(batch).map_err(|e| driver_err(&e))?;
        Ok(())
    }

    fn finish(mut self: Box<Self>) -> Result<()> {
        if let Some(inner) = self.inner.take() {
            inner.close().map_err(|e| driver_err(&e))?;
        }
        Ok(())
    }
}

impl Drop for ParquetWriter {
    fn drop(&mut self) {
        if self.inner.is_some() {
            tracing::warn!(target: "shpx::parquet", "ParquetWriter dropped without finish()");
        }
    }
}
