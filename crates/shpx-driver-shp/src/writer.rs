//! Arrow `RecordBatch` ストリームから Shapefile (.shp/.shx/.dbf/.prj/.cpg) を生成する。

use std::fs::File;
use std::io::BufWriter;
use std::path::PathBuf;

use arrow_array::{
    cast::AsArray,
    types::{
        Date32Type, Date64Type, Decimal128Type, Float16Type, Float32Type, Float64Type, Int16Type,
        Int32Type, Int64Type, Int8Type, UInt16Type, UInt32Type, UInt8Type,
    },
    Array, ArrowPrimitiveType, PrimitiveArray, RecordBatch,
};
use arrow_schema::{DataType, SchemaRef};
use encoding_rs::Encoding;
use shapefile::dbase::{Date as DbfDate, FieldValue, Record, TableWriterBuilder};
use shapefile::{ShapeType, Writer as ShpWriterInner};
use shpx_core::{
    schema::require_geometry_column, Crs, Error, LayerWriter, OnLoss, Result, Uri, WriteOpts,
};
use shpx_geom::wkb;

use crate::cpg;
use crate::crs_io;
use crate::dbf_schema::{plan_dbf_writer_schema, DbfFieldKind, DbfFieldPlan, DbfWritePlan};
use crate::geometry::{decide_output_shape_type, geom_to_shp, validate_geom_for_shape_type};
use crate::util::{
    apply_on_loss, date32, driver_err, driver_msg, loss_kind, sidecar_path,
    truncate_at_char_boundary,
};

/// Shapefile の `LayerWriter` 実装。
pub struct ShpWriter {
    plan: DbfWritePlan,
    /// プラン内で Decimal128 列に対する 10^(-scale) を per-row 計算しないよう事前計算した係数。
    /// `plan.fields` と同じ順序で並ぶ。Decimal 以外は `1.0`（未使用）。
    decimal_inv_scale: Vec<f64>,
    shape_type: ShapeType,
    geom_index: usize,
    inner: ShpWriterInner<BufWriter<File>>,
    encoding: &'static Encoding,
    on_loss: OnLoss,
    finished: bool,
}

impl ShpWriter {
    pub fn open(
        uri: &Uri,
        schema: &SchemaRef,
        crs: Option<&Crs>,
        opts: &WriteOpts,
    ) -> Result<Self> {
        let shp_path = PathBuf::from(uri.path());
        let prj_path = sidecar_path(&shp_path, "prj");
        let cpg_path = sidecar_path(&shp_path, "cpg");
        let shx_path = sidecar_path(&shp_path, "shx");
        let dbf_path = sidecar_path(&shp_path, "dbf");

        if !opts.overwrite {
            for p in [&shp_path, &shx_path, &dbf_path, &prj_path, &cpg_path] {
                if p.exists() {
                    return Err(Error::Format(format!(
                        "output already exists: {} (use overwrite)",
                        p.display()
                    )));
                }
            }
        }

        let (geom_index, _geom_name, geom_meta) = require_geometry_column(schema)?;
        let shape_type = decide_output_shape_type(geom_meta.geometry_type)?;

        let plan = plan_dbf_writer_schema(schema, Some(geom_index), opts.on_loss)?;
        let decimal_inv_scale = plan
            .fields
            .iter()
            .map(|f| match &f.data_type {
                DataType::Decimal128(_, s) => 10f64.powi(-i32::from(*s)),
                _ => 1.0,
            })
            .collect();

        let encoding = cpg::resolve_write_encoding(opts)?;
        let builder = plan.apply_to_builder(TableWriterBuilder::new())?;
        let inner = ShpWriterInner::from_path(&shp_path, builder).map_err(|e| driver_err(&e))?;

        // サイドカー (.prj / .cpg) を先に書いておく。本体 .shp/.shx/.dbf は writer drop で確定。
        if let Some(c) = crs {
            crs_io::write_prj(&prj_path, c, opts.on_loss)?;
        }
        cpg::write_cpg(&cpg_path, cpg::cpg_label_for(encoding))?;

        Ok(Self {
            plan,
            decimal_inv_scale,
            shape_type,
            geom_index,
            inner,
            encoding,
            on_loss: opts.on_loss,
            finished: false,
        })
    }
}

impl LayerWriter for ShpWriter {
    fn write_batch(&mut self, batch: &RecordBatch) -> Result<()> {
        let geom_arr = batch.column(self.geom_index);
        let geom_bin = geom_arr.as_binary::<i32>();

        // 行ループ前に列参照を確定して `batch.column()` の繰り返し呼び出しを避ける。
        let cols: Vec<&dyn Array> = self
            .plan
            .fields
            .iter()
            .map(|p| batch.column(p.input_index).as_ref())
            .collect();

        for row in 0..batch.num_rows() {
            let shape = if geom_bin.is_null(row) {
                shapefile::Shape::NullShape
            } else {
                let bytes = geom_bin.value(row);
                let geom = wkb::decode(bytes)?;
                validate_geom_for_shape_type(&geom, self.shape_type)?;
                geom_to_shp(&geom)?
            };

            let mut record = Record::default();
            for (k, plan_field) in self.plan.fields.iter().enumerate() {
                let value = arrow_value_to_dbf(
                    plan_field,
                    cols[k],
                    row,
                    self.decimal_inv_scale[k],
                    self.encoding,
                    self.on_loss,
                )?;
                record.insert(plan_field.dbf_name.clone(), value);
            }

            write_shape_dispatch(&mut self.inner, &shape, &record, self.shape_type)?;
        }
        Ok(())
    }

    fn finish(mut self: Box<Self>) -> Result<()> {
        // shapefile crate の Writer は Drop で finalize する。明示の close API は無いため、
        // drop に任せる。ここではフラグだけ立てて Drop 警告を抑止する。
        self.finished = true;
        Ok(())
    }
}

impl Drop for ShpWriter {
    fn drop(&mut self) {
        if !self.finished {
            tracing::warn!(target: "shpx::shp", "ShpWriter dropped without finish()");
        }
    }
}

/// `shapefile::Writer::write_shape_and_record` は `EsriShape` 型を要求するため、
/// 列の `ShapeType` ごとに具体型へ展開してから呼び出す。
///
/// NullShape が来た場合は `Error::Geometry` で reject する。`shapefile` crate は
/// shape_type 確定前 (= 1 件目) に `NullShape` を書けないため、安全側で全行に
/// 「具体的なジオメトリが必要」と要求する方針。null geometry を書きたいケースが
/// 出てきたら v0.2 で `Shape::NullShape` を許す経路を別途設計する。
fn write_shape_dispatch(
    w: &mut ShpWriterInner<BufWriter<File>>,
    shape: &shapefile::Shape,
    record: &Record,
    shape_type: ShapeType,
) -> Result<()> {
    match (shape, shape_type) {
        (shapefile::Shape::Point(p), ShapeType::Point) => w
            .write_shape_and_record(p, record)
            .map_err(|e| driver_err(&e)),
        (shapefile::Shape::Polyline(pl), ShapeType::Polyline) => w
            .write_shape_and_record(pl, record)
            .map_err(|e| driver_err(&e)),
        (shapefile::Shape::Polygon(pg), ShapeType::Polygon) => w
            .write_shape_and_record(pg, record)
            .map_err(|e| driver_err(&e)),
        (shapefile::Shape::Multipoint(mp), ShapeType::Multipoint) => w
            .write_shape_and_record(mp, record)
            .map_err(|e| driver_err(&e)),
        (shapefile::Shape::NullShape, _) => Err(Error::Geometry(
            "null geometry rows are not supported by shp writer in v0.1".into(),
        )),
        (s, st) => Err(Error::Geometry(format!(
            "row geometry {s} does not match column shape type {st:?}"
        ))),
    }
}

fn arrow_value_to_dbf(
    plan: &DbfFieldPlan,
    array: &dyn Array,
    row: usize,
    decimal_inv_scale: f64,
    encoding: &'static Encoding,
    on_loss: OnLoss,
) -> Result<FieldValue> {
    if array.is_null(row) {
        return Ok(null_value_for(&plan.field_type));
    }
    match (&plan.data_type, &plan.field_type) {
        (DataType::Utf8, DbfFieldKind::Character { length }) => {
            Ok(FieldValue::Character(Some(truncate_for_dbf(
                array.as_string::<i32>().value(row),
                *length,
                encoding,
                &plan.source_name,
                on_loss,
            )?)))
        }
        (DataType::LargeUtf8, DbfFieldKind::Character { length }) => {
            Ok(FieldValue::Character(Some(truncate_for_dbf(
                array.as_string::<i64>().value(row),
                *length,
                encoding,
                &plan.source_name,
                on_loss,
            )?)))
        }
        (DataType::Boolean, DbfFieldKind::Logical) => {
            Ok(FieldValue::Logical(Some(array.as_boolean().value(row))))
        }
        (DataType::Int8, DbfFieldKind::Numeric { .. }) => {
            Ok(numeric_from_int::<Int8Type>(array, row))
        }
        (DataType::Int16, DbfFieldKind::Numeric { .. }) => {
            Ok(numeric_from_int::<Int16Type>(array, row))
        }
        (DataType::Int32, DbfFieldKind::Numeric { .. }) => {
            Ok(numeric_from_int::<Int32Type>(array, row))
        }
        (DataType::Int64, DbfFieldKind::Numeric { .. }) => {
            // Int64 → f64 は 53bit 超で精度落ちが起こり得るが、`apply_on_loss` 検査は
            // schema 構築時に済ませているため許容する。
            #[allow(clippy::cast_precision_loss)]
            let v = primitive::<Int64Type>(array, row) as f64;
            Ok(FieldValue::Numeric(Some(v)))
        }
        (DataType::UInt8, DbfFieldKind::Numeric { .. }) => {
            Ok(numeric_from_int::<UInt8Type>(array, row))
        }
        (DataType::UInt16, DbfFieldKind::Numeric { .. }) => {
            Ok(numeric_from_int::<UInt16Type>(array, row))
        }
        (DataType::UInt32, DbfFieldKind::Numeric { .. }) => {
            Ok(numeric_from_int::<UInt32Type>(array, row))
        }
        (DataType::Float16, DbfFieldKind::Float { .. }) => Ok(FieldValue::Float(Some(
            primitive::<Float16Type>(array, row).to_f32(),
        ))),
        (DataType::Float32, DbfFieldKind::Float { .. }) => Ok(FieldValue::Float(Some(
            primitive::<Float32Type>(array, row),
        ))),
        (DataType::Float64, DbfFieldKind::Float { .. }) => {
            // dbase 0.5 の Float は f32 のため切り詰めが起こり得る。
            #[allow(clippy::cast_possible_truncation)]
            let v = primitive::<Float64Type>(array, row) as f32;
            Ok(FieldValue::Float(Some(v)))
        }
        (DataType::Decimal128(_, _), DbfFieldKind::Numeric { .. }) => {
            // `decimal_inv_scale` は plan 構築時に `10^(-scale)` を 1 度だけ計算している。
            #[allow(clippy::cast_precision_loss)]
            let raw = primitive::<Decimal128Type>(array, row) as f64;
            Ok(FieldValue::Numeric(Some(raw * decimal_inv_scale)))
        }
        (DataType::Date32, DbfFieldKind::Date) => {
            let nd = date32::to_naive(primitive::<Date32Type>(array, row));
            Ok(FieldValue::Date(Some(naivedate_to_dbf(nd))))
        }
        (DataType::Date64, DbfFieldKind::Date) => {
            let ms = primitive::<Date64Type>(array, row);
            let nd = chrono::DateTime::from_timestamp_millis(ms)
                .ok_or_else(|| driver_msg("invalid Date64"))?
                .naive_utc()
                .date();
            Ok(FieldValue::Date(Some(naivedate_to_dbf(nd))))
        }
        (dt, kind) => Err(driver_msg(format!(
            "unsupported Arrow → DBF mapping: {dt:?} → {kind:?} (field `{}`)",
            plan.source_name
        ))),
    }
}

fn primitive<T: ArrowPrimitiveType>(array: &dyn Array, row: usize) -> T::Native {
    array
        .as_any()
        .downcast_ref::<PrimitiveArray<T>>()
        .expect("primitive type checked by outer match")
        .value(row)
}

fn numeric_from_int<T: ArrowPrimitiveType>(array: &dyn Array, row: usize) -> FieldValue
where
    T::Native: Into<f64>,
{
    FieldValue::Numeric(Some(primitive::<T>(array, row).into()))
}

fn null_value_for(kind: &DbfFieldKind) -> FieldValue {
    match kind {
        DbfFieldKind::Character { .. } => FieldValue::Character(None),
        DbfFieldKind::Numeric { .. } => FieldValue::Numeric(None),
        DbfFieldKind::Float { .. } => FieldValue::Float(None),
        DbfFieldKind::Logical => FieldValue::Logical(None),
        DbfFieldKind::Date => FieldValue::Date(None),
    }
}

fn naivedate_to_dbf(nd: chrono::NaiveDate) -> DbfDate {
    use chrono::Datelike;
    // DBF Date は YYYYMMDD の正の年に限る (1900-01-01 以前は v0.1 範囲外として 0 に飽和)。
    let y = u32::try_from(nd.year()).unwrap_or(0);
    DbfDate::new(nd.day(), nd.month(), y)
}

fn truncate_for_dbf(
    s: &str,
    max_bytes: u8,
    encoding: &'static Encoding,
    field: &str,
    on_loss: OnLoss,
) -> Result<String> {
    let (encoded, _, had_unmapped) = encoding.encode(s);
    if had_unmapped {
        apply_on_loss(loss_kind::UTF8_CP_UNMAPPABLE, field, on_loss)?;
    }
    if encoded.len() > usize::from(max_bytes) {
        apply_on_loss(loss_kind::UTF8_LENGTH_ON_DBF, field, on_loss)?;
        // 出力 codec が CP932 等の可変長の場合、UTF-8 byte 境界で切ると codec 上の長さが
        // 厳密に max_bytes 以下にならない可能性があるが、最終的に dbase 側でも fixed-length
        // padding/cut が走るため安全側で動く。精密な codec 境界調整は v0.2。
        return Ok(truncate_at_char_boundary(s, max_bytes as usize).to_string());
    }
    Ok(s.to_string())
}
