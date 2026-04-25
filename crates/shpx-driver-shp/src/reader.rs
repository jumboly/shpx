//! Shapefile を Arrow `RecordBatch` ストリームとして読み出す。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use arrow_array::builder::{
    BinaryBuilder, BooleanBuilder, Date32Builder, Float64Builder, Int32Builder, StringBuilder,
};
use arrow_array::{ArrayRef, RecordBatch};
use arrow_schema::{DataType, Field, Schema, SchemaRef};
use shapefile::dbase::{self, FieldType, FieldValue};
use shapefile::Reader as ShpReaderInner;
use shpx_core::{
    schema::{GeometryMeta, GEOMETRY_META_KEY},
    Crs, LayerReader, ReadOpts, Result,
};
use shpx_geom::wkb;

use crate::cpg;
use crate::crs_io;
use crate::dbf_schema;
use crate::geometry::{shape_type_to_geometry_type, shp_to_geom};
use crate::util::{date32, dbf_field_type_label, driver_err, sidecar_path};

const READ_BATCH_SIZE: usize = 65_536;
/// Arrow 列メタに元 DBF 型を保存するためのキー。Writer 側で復元に使う。
pub const SOURCE_DBF_TYPE_KEY: &str = "shpx:shp:dbf_type";

/// Shapefile の `LayerReader` 実装。
pub struct ShpReader {
    schema: SchemaRef,
    crs: Option<Crs>,
    row_count: Option<usize>,
    /// 元 DBF フィールド型（Arrow への射影に使う）。
    dbf_field_types: Vec<FieldType>,
    /// DBF フィールドの元名（Record から取り出す際のキー）。
    dbf_field_names: Vec<String>,
    on_loss: shpx_core::OnLoss,
    /// 全件 (shape, record) を事前にロードした作業バッファ。
    ///
    /// `iter_shapes_and_records()` は呼び出すたびにファイル先頭から再列挙する設計のため、
    /// 複数 batch に跨る逐次読みには使えない。v0.1 では事前ロード方式で対処する
    /// （Shapefile はサイズが小さいユースケースが大半なので許容できる）。
    /// メモリ使用量を抑えたい場合は v0.2 で stateful iterator に置き換える余地あり。
    pending: std::collections::VecDeque<(shapefile::Shape, dbase::Record)>,
}

impl ShpReader {
    pub fn open(uri: &shpx_core::Uri, opts: &ReadOpts) -> Result<Self> {
        let shp_path = PathBuf::from(uri.path());
        let prj_path = sidecar_path(&shp_path, "prj");
        let cpg_path = sidecar_path(&shp_path, "cpg");
        let dbf_path = sidecar_path(&shp_path, "dbf");

        let crs = match opts.src_crs.clone() {
            Some(c) => Some(c),
            None => crs_io::read_prj(&prj_path)?,
        };
        let _encoding = cpg::resolve_read_encoding(&cpg_path, opts)?;

        let mut inner = ShpReaderInner::from_path(&shp_path).map_err(|e| driver_err(&e))?;
        let shape_type = inner.header().shape_type;

        // shapefile::Reader は dbase_reader が private のため、fields() 取得用に独立して 1 回開く。
        let dbf_reader_for_fields =
            dbase::Reader::from_path(&dbf_path).map_err(|e| driver_err(&e))?;
        let dbf_fields = dbf_reader_for_fields.fields().to_vec();
        drop(dbf_reader_for_fields);

        let mut arrow_fields: Vec<Field> = Vec::with_capacity(dbf_fields.len() + 1);
        let mut dbf_field_types = Vec::with_capacity(dbf_fields.len());
        let mut dbf_field_names = Vec::with_capacity(dbf_fields.len());
        for info in &dbf_fields {
            let mut field = dbf_schema::dbf_field_to_arrow(info);
            let mut meta = HashMap::with_capacity(1);
            meta.insert(
                SOURCE_DBF_TYPE_KEY.to_string(),
                dbf_field_type_label(info.field_type()).to_string(),
            );
            field.set_metadata(meta);
            arrow_fields.push(field);
            dbf_field_types.push(info.field_type());
            dbf_field_names.push(info.name().to_string());
        }

        let geometry_type = shape_type_to_geometry_type(shape_type);
        let meta = GeometryMeta::wkb(geometry_type, crs.clone());
        let mut geom_field_meta: HashMap<String, String> = HashMap::with_capacity(1);
        geom_field_meta.insert(GEOMETRY_META_KEY.to_string(), meta.to_json()?);
        let mut geom_field = Field::new("geometry", DataType::Binary, true);
        geom_field.set_metadata(geom_field_meta);
        arrow_fields.push(geom_field);

        let schema = Arc::new(Schema::new(arrow_fields));
        let row_count = inner.shape_count().ok();

        let mut pending = std::collections::VecDeque::with_capacity(row_count.unwrap_or(0));
        for item in inner.iter_shapes_and_records() {
            let (shape, record) = item.map_err(|e| driver_err(&e))?;
            pending.push_back((shape, record));
        }

        Ok(Self {
            schema,
            crs,
            row_count,
            dbf_field_types,
            dbf_field_names,
            on_loss: shpx_core::OnLoss::Warn,
            pending,
        })
    }
}

impl LayerReader for ShpReader {
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
        Box::new(BatchIter {
            reader: self,
            finished: false,
        })
    }
}

struct BatchIter<'a> {
    reader: &'a mut ShpReader,
    finished: bool,
}

impl Iterator for BatchIter<'_> {
    type Item = Result<RecordBatch>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.finished {
            return None;
        }
        match read_one_batch(self.reader) {
            Ok(Some(batch)) => Some(Ok(batch)),
            Ok(None) => {
                self.finished = true;
                None
            }
            Err(e) => {
                self.finished = true;
                Some(Err(e))
            }
        }
    }
}

fn read_one_batch(r: &mut ShpReader) -> Result<Option<RecordBatch>> {
    if r.pending.is_empty() {
        return Ok(None);
    }
    // 残量と batch 上限の小さい方で alloc。最終 batch で 65536 行ぶんの空 alloc を抱えない。
    let n_rows = r.pending.len().min(READ_BATCH_SIZE);

    let mut attr_builders: Vec<AttrBuilder> = r
        .dbf_field_types
        .iter()
        .map(|t| AttrBuilder::new(*t, n_rows))
        .collect();
    let mut geom_builder = BinaryBuilder::with_capacity(n_rows, n_rows * 32);

    for _ in 0..n_rows {
        let (shape, record) = r.pending.pop_front().expect("invariant: n_rows ≤ pending");

        for (i, builder) in attr_builders.iter_mut().enumerate() {
            builder.push(record.get(&r.dbf_field_names[i]))?;
        }

        match shp_to_geom(&shape, r.on_loss)? {
            Some(g) => {
                let bytes = wkb::encode(&g)?;
                geom_builder.append_value(&bytes);
            }
            None => geom_builder.append_null(),
        }
    }

    let mut columns: Vec<ArrayRef> = Vec::with_capacity(attr_builders.len() + 1);
    for b in attr_builders {
        columns.push(b.finish());
    }
    columns.push(Arc::new(geom_builder.finish()) as ArrayRef);

    let batch = RecordBatch::try_new(r.schema.clone(), columns)
        .map_err(|e| shpx_core::Error::Schema(e.to_string()))?;
    Ok(Some(batch))
}

/// 1 列分のビルダ。`FieldType` ごとに内部表現を切り替える。
enum AttrBuilder {
    Utf8(StringBuilder),
    Bool(BooleanBuilder),
    Int32(Int32Builder),
    Float64(Float64Builder),
    Date(Date32Builder),
}

impl AttrBuilder {
    fn new(ty: FieldType, capacity: usize) -> Self {
        match ty {
            FieldType::Character | FieldType::Memo => {
                Self::Utf8(StringBuilder::with_capacity(capacity, capacity * 16))
            }
            FieldType::Logical => Self::Bool(BooleanBuilder::with_capacity(capacity)),
            FieldType::Integer => Self::Int32(Int32Builder::with_capacity(capacity)),
            FieldType::Numeric | FieldType::Float | FieldType::Double | FieldType::Currency => {
                Self::Float64(Float64Builder::with_capacity(capacity))
            }
            FieldType::Date | FieldType::DateTime => {
                Self::Date(Date32Builder::with_capacity(capacity))
            }
        }
    }

    fn push(&mut self, val: Option<&FieldValue>) -> Result<()> {
        // dbase 0.5 は Reader 側で encoding を内部的に適用済み（UnicodeLossy がデフォルト）。
        // ここでは値の取り出しと Arrow 変換のみ行う。
        match self {
            Self::Utf8(b) => match val {
                Some(FieldValue::Character(Some(s)) | FieldValue::Memo(s)) => b.append_value(s),
                _ => b.append_null(),
            },
            Self::Bool(b) => match val {
                Some(FieldValue::Logical(Some(v))) => b.append_value(*v),
                _ => b.append_null(),
            },
            Self::Int32(b) => match val {
                Some(FieldValue::Integer(v)) => b.append_value(*v),
                _ => b.append_null(),
            },
            Self::Float64(b) => match val {
                Some(
                    FieldValue::Numeric(Some(v)) | FieldValue::Double(v) | FieldValue::Currency(v),
                ) => b.append_value(*v),
                Some(FieldValue::Float(Some(v))) => b.append_value(f64::from(*v)),
                _ => b.append_null(),
            },
            Self::Date(b) => {
                let date_val = match val {
                    Some(FieldValue::Date(Some(d))) => Some(*d),
                    Some(FieldValue::DateTime(dt)) => Some(dt.date()),
                    _ => None,
                };
                if let Some(d) = date_val {
                    let y = i32::try_from(d.year()).unwrap_or(0);
                    b.append_value(date32::from_ymd(y, d.month(), d.day())?);
                } else {
                    b.append_null();
                }
            }
        }
        Ok(())
    }

    fn finish(self) -> ArrayRef {
        match self {
            Self::Utf8(mut b) => Arc::new(b.finish()),
            Self::Bool(mut b) => Arc::new(b.finish()),
            Self::Int32(mut b) => Arc::new(b.finish()),
            Self::Float64(mut b) => Arc::new(b.finish()),
            Self::Date(mut b) => Arc::new(b.finish()),
        }
    }
}
