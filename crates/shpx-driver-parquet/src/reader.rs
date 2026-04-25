//! GeoParquet を Arrow `RecordBatch` ストリームとして読み出す。
//!
//! 単一ファイル内に格納された KeyValue `geo` メタデータから CRS / geometry 型を抽出し、
//! Arrow フィールドの `shpx:geometry` field metadata に詰め直してパイプライン下流へ渡す。

use std::collections::HashMap;
use std::fs::File;
use std::path::PathBuf;
use std::sync::Arc;

use arrow_array::RecordBatch;
use arrow_schema::{Field, Schema, SchemaRef};
use parquet::arrow::arrow_reader::{ParquetRecordBatchReader, ParquetRecordBatchReaderBuilder};
use shpx_core::{
    schema::{GeometryMeta, GEOMETRY_META_KEY},
    Crs, Error, LayerReader, ReadOpts, Result, Uri,
};

use crate::geo_meta::{self, GEO_KV_KEY};
use crate::util::{driver_err, driver_msg};

const READ_BATCH_SIZE: usize = 65_536;

/// GeoParquet の `LayerReader` 実装。
pub struct ParquetReader {
    schema: SchemaRef,
    crs: Option<Crs>,
    row_count: Option<usize>,
    /// `batches()` で 1 度だけ取り出して所有権を返す。
    /// `ParquetRecordBatchReader` はそれ自体が `Iterator` のため、
    /// SHP ドライバのような `&mut self` 借用イテレータではなく所有形にする。
    inner: Option<ParquetRecordBatchReader>,
}

impl ParquetReader {
    pub fn open(uri: &Uri, opts: &ReadOpts) -> Result<Self> {
        let path = PathBuf::from(uri.path());
        let file = File::open(&path)?;
        let builder =
            ParquetRecordBatchReaderBuilder::try_new(file).map_err(|e| driver_err(&e))?;

        let row_count = usize::try_from(builder.metadata().file_metadata().num_rows())
            .ok()
            .map(|n| if n == 0 { 0 } else { n });

        let kv = builder.metadata().file_metadata().key_value_metadata();
        let geo_value = kv.and_then(|list| {
            list.iter()
                .find(|kv| kv.key == GEO_KV_KEY)
                .and_then(|kv| kv.value.clone())
        });

        // KeyValue から GeoParquet 情報を取れた場合の (primary_column 名, GeometryMeta)。
        let geo_info = match geo_value {
            Some(s) => Some(geo_meta::parse_geo_metadata(&s)?),
            None => None,
        };

        let raw_schema = builder.schema().clone();
        let (schema, geom_crs) = enrich_schema(&raw_schema, geo_info.as_ref())?;

        // `--src-crs` での補完を最終層で適用する。
        let crs = opts.src_crs.clone().or(geom_crs);

        let reader = builder
            .with_batch_size(READ_BATCH_SIZE)
            .build()
            .map_err(|e| driver_err(&e))?;

        Ok(Self {
            schema,
            crs,
            row_count,
            inner: Some(reader),
        })
    }
}

/// raw schema に対して geometry 列の field metadata を埋める。
///
/// 優先順位:
/// 1. 入力ファイルの Arrow field metadata に既に `shpx:geometry` がある → そのまま信用
/// 2. KeyValue `geo` がある → primary_column の field metadata を上書き
/// 3. どちらも無い → schema は変更せず、CRS も None
///
/// 戻り値の `Crs` は geometry 列に紐付く CRS。
fn enrich_schema(
    raw: &SchemaRef,
    geo: Option<&(String, GeometryMeta)>,
) -> Result<(SchemaRef, Option<Crs>)> {
    // 既に shpx:geometry を持つ列があれば、それを信用する。
    let preexisting_idx = raw
        .fields()
        .iter()
        .position(|f| f.metadata().contains_key(GEOMETRY_META_KEY));
    if let Some(i) = preexisting_idx {
        let json = raw.field(i).metadata().get(GEOMETRY_META_KEY).expect("just checked");
        let meta = GeometryMeta::from_json(json)?;
        return Ok((raw.clone(), meta.crs));
    }

    let Some((primary, meta)) = geo else {
        return Ok((raw.clone(), None));
    };
    let idx = raw.fields().iter().position(|f| f.name() == primary);
    let Some(idx) = idx else {
        return Err(driver_msg(format!(
            "primary geometry column `{primary}` not found in Parquet schema"
        )));
    };
    let mut fields: Vec<Arc<Field>> = raw.fields().iter().cloned().collect();
    let mut new_field = Field::clone(&fields[idx]);
    let mut metadata: HashMap<String, String> = new_field.metadata().clone();
    metadata.insert(GEOMETRY_META_KEY.to_string(), meta.to_json()?);
    new_field.set_metadata(metadata);
    fields[idx] = Arc::new(new_field);

    let mut schema = Schema::new(fields);
    // Arrow Schema 自身のファイルレベル metadata は内容を維持する。
    schema.metadata.clone_from(raw.metadata());
    Ok((Arc::new(schema), meta.crs.clone()))
}

impl LayerReader for ParquetReader {
    fn schema(&self) -> SchemaRef {
        self.schema.clone()
    }

    fn crs(&self) -> Option<&Crs> {
        self.crs.as_ref()
    }

    fn row_count_hint(&self) -> Option<usize> {
        self.row_count
    }

    fn batches(&mut self) -> Box<dyn Iterator<Item = Result<RecordBatch>> + Send + '_> {
        let inner = self.inner.take();
        Box::new(BatchIter { inner })
    }
}

struct BatchIter {
    inner: Option<ParquetRecordBatchReader>,
}

impl Iterator for BatchIter {
    type Item = Result<RecordBatch>;

    fn next(&mut self) -> Option<Self::Item> {
        let inner = self.inner.as_mut()?;
        match inner.next()? {
            Ok(batch) => Some(Ok(batch)),
            Err(e) => {
                // エラー後は以降の next() で None を返す。
                self.inner = None;
                Some(Err(Error::Format(e.to_string())))
            }
        }
    }
}
