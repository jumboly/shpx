//! Shapefile を Arrow `RecordBatch` ストリームとして読み出す。
//!
//! # 実装メモ — なぜ worker thread + sync_channel か
//!
//! `shapefile::Reader::iter_shapes_and_records()` は内部で `ShapeIterator` を
//! `current_pos = HEADER_SIZE` でゼロから初期化するため、**呼び出すたびにファイル先頭から
//! 再列挙する** (`shapefile-0.6.0/src/reader.rs:344-352`)。借用ベースの batch iterator
//! では「1 batch ぶん消費 → 次 batch で続きから」が成立しない。
//!
//! 一方 `iter_shapes_and_records()` の戻り値 `ShapeRecordIterator<'_>` は `&mut Reader`
//! を借用するため、`BatchIter` の field として保持しようとすると self-referential に
//! なってコンパイルが通らない。crate 外の owning iterator API も提供されていない。
//!
//! 解決策として **専用 OS スレッドが Reader を所有し、`sync_channel` 経由で 1 batch 単位の
//! `Vec<(Shape, Record)>` を main スレッドへ送る** 方式を採る。channel 容量は 2 (= 高々
//! 2 batch のみ in-flight) で、receiver 側 (BatchIter) が Drop されると `tx.send` が
//! Err を返してスレッドが自然終了する。`JoinHandle` は捨てる (Drop で detach されるが、
//! receiver 終了を契機にスレッド側も自然停止するため join 不要)。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{sync_channel, Receiver};
use std::sync::Arc;
use std::thread;

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

/// worker thread から送られてくる 1 batch ぶんの records。
/// `Err` の場合はその batch で打ち切り (worker は早期 return)。
type RecordChunk = std::result::Result<Vec<(shapefile::Shape, dbase::Record)>, shpx_core::Error>;

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
    /// worker thread からの batch 受信口。`batches()` で take() して BatchIter に渡す。
    /// rx の drop が worker 側 tx.send Err を誘発し、スレッド自然終了に繋がる。
    rx: Option<Receiver<RecordChunk>>,
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

        // schema / row_count / DBF field 一覧を確定するための Reader を 1 つ開く。
        // この Reader は schema 抽出後すぐ drop し、worker thread には別途新規に open する。
        // (.shp ヘッダ + .dbf ヘッダの読み込みは数 KB 程度なので 2 度開くオーバーヘッドは無視可能)
        let probe_reader = ShpReaderInner::from_path(&shp_path).map_err(|e| driver_err(&e))?;
        let shape_type = probe_reader.header().shape_type;
        let row_count = probe_reader.shape_count().ok();
        drop(probe_reader);

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

        // worker thread を起動: Reader を所有し、READ_BATCH_SIZE 件ごとに chunk を送る。
        // channel 容量 2 = main 側が 1 batch 処理中にも次の 1 batch を生産できる。
        let (tx, rx) = sync_channel::<RecordChunk>(2);
        let shp_path_clone = shp_path.clone();
        thread::spawn(move || {
            let mut inner = match ShpReaderInner::from_path(&shp_path_clone) {
                Ok(r) => r,
                Err(e) => {
                    let _ = tx.send(Err(driver_err(&e)));
                    return;
                }
            };
            let mut buf: Vec<(shapefile::Shape, dbase::Record)> =
                Vec::with_capacity(READ_BATCH_SIZE);
            for item in inner.iter_shapes_and_records() {
                match item {
                    Ok(t) => {
                        buf.push(t);
                        if buf.len() >= READ_BATCH_SIZE {
                            let chunk = std::mem::replace(
                                &mut buf,
                                Vec::with_capacity(READ_BATCH_SIZE),
                            );
                            if tx.send(Ok(chunk)).is_err() {
                                return; // receiver dropped, exit cleanly
                            }
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(Err(driver_err(&e)));
                        return;
                    }
                }
            }
            if !buf.is_empty() {
                let _ = tx.send(Ok(buf));
            }
        });

        Ok(Self {
            schema,
            crs,
            row_count,
            dbf_field_types,
            dbf_field_names,
            on_loss: shpx_core::OnLoss::Warn,
            rx: Some(rx),
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
        let rx = self.rx.take();
        Box::new(BatchIter {
            rx,
            schema: self.schema.clone(),
            dbf_field_types: self.dbf_field_types.clone(),
            dbf_field_names: self.dbf_field_names.clone(),
            on_loss: self.on_loss,
        })
    }
}

struct BatchIter {
    rx: Option<Receiver<RecordChunk>>,
    schema: SchemaRef,
    dbf_field_types: Vec<FieldType>,
    dbf_field_names: Vec<String>,
    on_loss: shpx_core::OnLoss,
}

impl Iterator for BatchIter {
    type Item = Result<RecordBatch>;

    fn next(&mut self) -> Option<Self::Item> {
        let rx = self.rx.as_ref()?;
        match rx.recv() {
            // worker からの Err は無条件で error 化して以降は終了。
            Ok(Err(e)) => {
                self.rx = None;
                Some(Err(e))
            }
            Ok(Ok(chunk)) => match build_record_batch(
                &chunk,
                &self.schema,
                &self.dbf_field_types,
                &self.dbf_field_names,
                self.on_loss,
            ) {
                Ok(b) => Some(Ok(b)),
                Err(e) => {
                    self.rx = None;
                    Some(Err(e))
                }
            },
            // recv() Err は worker thread が tx を drop した = データ尽きた。
            Err(_) => {
                self.rx = None;
                None
            }
        }
    }
}

fn build_record_batch(
    chunk: &[(shapefile::Shape, dbase::Record)],
    schema: &SchemaRef,
    dbf_field_types: &[FieldType],
    dbf_field_names: &[String],
    on_loss: shpx_core::OnLoss,
) -> Result<RecordBatch> {
    let n_rows = chunk.len();

    let mut attr_builders: Vec<AttrBuilder> = dbf_field_types
        .iter()
        .map(|t| AttrBuilder::new(*t, n_rows))
        .collect();
    let mut geom_builder = BinaryBuilder::with_capacity(n_rows, n_rows * 32);

    for (shape, record) in chunk {
        for (i, builder) in attr_builders.iter_mut().enumerate() {
            builder.push(record.get(&dbf_field_names[i]))?;
        }

        match shp_to_geom(shape, on_loss)? {
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

    RecordBatch::try_new(schema.clone(), columns)
        .map_err(|e| shpx_core::Error::Schema(e.to_string()))
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
