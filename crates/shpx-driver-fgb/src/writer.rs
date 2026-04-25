//! Arrow `RecordBatch` ストリームから FlatGeobuf を生成する。
//!
//! `flatgeobuf::FgbWriter` を内部に持ち、属性は geozero の `ColumnValue`、
//! geometry は WKB バイト列を `geozero::wkb::Wkb` でラップして渡す。
//!
//! 空間インデックス（packed Hilbert R-Tree）は cycle 4 ではスコープ外のため
//! `FgbWriterOptions { write_index: false, .. }` で出力する。

use std::cell::RefCell;
use std::fs::File;
use std::io::BufWriter;
use std::path::PathBuf;

use arrow_array::{
    cast::AsArray,
    types::{
        Date32Type, Date64Type, Decimal128Type, Float32Type, Float64Type, Int16Type, Int32Type,
        Int64Type, Int8Type, TimestampMicrosecondType, TimestampMillisecondType,
        TimestampNanosecondType, TimestampSecondType, UInt16Type, UInt32Type, UInt64Type,
        UInt8Type,
    },
    Array, ArrowPrimitiveType, PrimitiveArray, RecordBatch,
};
use arrow_schema::{DataType, Field, SchemaRef, TimeUnit};
use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use flatgeobuf::{ColumnType, FgbCrs, FgbWriter as InnerFgbWriter, FgbWriterOptions};
use geozero::{wkb::Wkb, PropertyProcessor};
use shpx_core::{
    schema::find_geometry_column, Crs, Error, LayerWriter, OnLoss, Result, Uri, WriteOpts,
};

use crate::type_map::{arrow_field_to_fgb_column, shpx_to_fgb_geometry_type, FgbColumnPlan};
use crate::util::{apply_on_loss, driver_err, driver_msg, loss_kind, DRIVER_NAME};
use crate::value::{borrow_column_value, OwnedValue};

/// FlatGeobuf の `LayerWriter` 実装。
pub struct FgbWriter {
    schema: SchemaRef,
    output_path: PathBuf,
    on_loss: OnLoss,
    /// `Option` で保持して `finish()` で take する（`Drop` 警告判定用）。
    inner: Option<InnerFgbWriter<'static>>,
    geom_index: usize,
    /// 出力する属性列の (schema 上の index, FGB 列計画)。geometry 列は除外。
    /// `add_feature_geom` の `feat.property(i, ...)` に渡す i は **FGB 列順** での index。
    attr_plan: Vec<(usize, FgbColumnPlan)>,
}

impl FgbWriter {
    pub fn open(uri: &Uri, schema: SchemaRef, crs: Option<&Crs>, opts: &WriteOpts) -> Result<Self> {
        let output_path = PathBuf::from(uri.path());
        if !opts.overwrite && output_path.exists() {
            return Err(Error::Format(format!(
                "output already exists: {} (use overwrite)",
                output_path.display()
            )));
        }

        let (geom_index, _, geom_meta) = find_geometry_column(&schema)?
            .ok_or_else(|| Error::Schema("no geometry column for FlatGeobuf writer".to_string()))?;

        let mut attr_plan: Vec<(usize, FgbColumnPlan)> = Vec::new();
        for (i, field) in schema.fields().iter().enumerate() {
            if i == geom_index {
                continue;
            }
            let plan = arrow_field_to_fgb_column(field)?;
            if matches!(
                field.data_type(),
                DataType::Decimal128(_, _) | DataType::Decimal256(_, _)
            ) {
                apply_on_loss(loss_kind::DECIMAL_ON_FGB, field.name(), opts.on_loss)?;
            }
            attr_plan.push((i, plan));
        }

        // dataset name にはファイル stem を使う（ogr2ogr 等の慣習に揃える）。
        let dataset_name = output_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("layer")
            .to_string();

        let geom_type = shpx_to_fgb_geometry_type(&geom_meta);
        let crs_args = build_crs_args(crs, opts.on_loss)?;

        let options = FgbWriterOptions {
            // packed Hilbert R-Tree は cycle 4 ではスコープ外。
            write_index: false,
            // shpx は GeometryMeta で型を確定させて渡すため、auto-detect は不要。
            detect_type: false,
            // `Polygon` 列に `MultiPolygon` を混ぜるなどの意図せぬ昇格は避けたい。
            promote_to_multi: false,
            crs: FgbCrs {
                org: crs_args.org.as_deref(),
                code: crs_args.code,
                name: None,
                description: None,
                wkt: crs_args.wkt.as_deref(),
                code_string: None,
            },
            ..Default::default()
        };

        let mut inner = InnerFgbWriter::create_with_options(&dataset_name, geom_type, options)
            .map_err(|e| driver_err(&e))?;

        for (_, plan) in &attr_plan {
            let nullable = plan.nullable;
            let precision = plan.precision;
            let scale = plan.scale;
            let width = plan.width;
            inner.add_column(&plan.name, plan.column_type, |_fbb, col| {
                col.nullable = nullable;
                if precision >= 0 {
                    col.precision = precision;
                }
                if scale >= 0 {
                    col.scale = scale;
                }
                if width >= 0 {
                    col.width = width;
                }
            });
        }

        Ok(Self {
            schema,
            output_path,
            on_loss: opts.on_loss,
            inner: Some(inner),
            geom_index,
            attr_plan,
        })
    }
}

impl LayerWriter for FgbWriter {
    fn write_batch(&mut self, batch: &RecordBatch) -> Result<()> {
        // 各フィールドを個別に借りておくことで、`inner` の `&mut` と `attr_plan` / `schema` の
        // `&` を同一スコープで共存させる（rustc の disjoint field borrow による）。これにより
        // `add_feature_geom` のクロージャから per-row clone なしで `attr_plan` を参照できる。
        let attr_plan = &self.attr_plan;
        let schema = &self.schema;
        let on_loss = self.on_loss;
        let geom_index = self.geom_index;
        let inner = self
            .inner
            .as_mut()
            .ok_or_else(|| driver_msg("write_batch called after finish"))?;

        let geom_arr = batch.column(geom_index);
        let geom_bin = geom_arr.as_binary::<i32>();

        for row in 0..batch.num_rows() {
            let mut owned: Vec<Option<OwnedValue>> = Vec::with_capacity(attr_plan.len());
            for (col_idx, plan) in attr_plan {
                let array = batch.column(*col_idx);
                let field = schema.field(*col_idx);
                owned.push(realize_value(field, array.as_ref(), row, on_loss, plan)?);
            }

            // FGB 仕様上 feature.geometry は optional だが、現行 `flatgeobuf::FeatureWriter` は
            // `process_geom` を必須とするため shpx 側で NULL 行を拒否する。
            if geom_bin.is_null(row) {
                return Err(driver_msg(
                    "FlatGeobuf writer does not support NULL geometry rows",
                ));
            }
            let wkb_bytes = geom_bin.value(row);

            // クロージャは `feat.property(..)` の `Result` を伝播できない（戻り値 `()` の `FnOnce`）
            // ため、`RefCell` で初出のエラーを捕捉して呼び出し元で `?` で扱う。
            let property_error: RefCell<Option<Error>> = RefCell::new(None);
            inner
                .add_feature_geom(Wkb(wkb_bytes), |feat| {
                    for (idx, (_, plan)) in attr_plan.iter().enumerate() {
                        // 値が NULL ならその列を書かない（FGB は欠落 = NULL）。
                        if let Some(cv) = borrow_column_value(&owned[idx]) {
                            if let Err(e) = feat.property(idx, &plan.name, &cv) {
                                if property_error.borrow().is_none() {
                                    *property_error.borrow_mut() = Some(driver_err(&e));
                                }
                                return;
                            }
                        }
                    }
                })
                .map_err(|e| driver_err(&e))?;
            if let Some(e) = property_error.into_inner() {
                return Err(e);
            }
        }
        Ok(())
    }

    fn finish(mut self: Box<Self>) -> Result<()> {
        let inner = self
            .inner
            .take()
            .ok_or_else(|| driver_msg("finish called twice"))?;
        let file = File::create(&self.output_path).map_err(Error::from)?;
        let mut out = BufWriter::new(file);
        inner.write(&mut out).map_err(|e| driver_err(&e))?;
        std::io::Write::flush(&mut out).map_err(Error::from)?;
        Ok(())
    }
}

impl Drop for FgbWriter {
    fn drop(&mut self) {
        if self.inner.is_some() {
            tracing::warn!(target: "shpx::fgb", "FgbWriter dropped without finish()");
        }
    }
}

/// FGB header 用の CRS 引数。`FgbCrs` に渡す前段で `String` を保持する。
struct CrsArgs {
    org: Option<String>,
    code: i32,
    wkt: Option<String>,
}

fn build_crs_args(crs: Option<&Crs>, on_loss: OnLoss) -> Result<CrsArgs> {
    let Some(c) = crs else {
        apply_on_loss(loss_kind::MISSING_CRS_ON_FGB, "<crs>", on_loss)?;
        return Ok(CrsArgs {
            org: None,
            code: 0,
            wkt: None,
        });
    };

    let (org, code) = match c.authority.as_ref() {
        Some((auth, code)) => (Some(auth.clone()), i32::try_from(*code).unwrap_or(0)),
        None => (None, 0),
    };
    // EPSG コードが分かっていれば WKT は付けず、WKT のみ持つ場合は WKT を載せる。
    let wkt = if org.is_some() && code > 0 {
        None
    } else {
        c.wkt.clone()
    };
    Ok(CrsArgs { org, code, wkt })
}

fn realize_value(
    field: &Field,
    array: &dyn Array,
    row: usize,
    on_loss: OnLoss,
    plan: &FgbColumnPlan,
) -> Result<Option<OwnedValue>> {
    if array.is_null(row) {
        return Ok(None);
    }
    let name = field.name();
    let value = match (field.data_type(), plan.column_type) {
        (DataType::Boolean, ColumnType::Bool) => OwnedValue::Bool(array.as_boolean().value(row)),
        (DataType::Int8, ColumnType::Byte) => OwnedValue::Byte(primitive::<Int8Type>(array, row)),
        (DataType::Int16, ColumnType::Short) => {
            OwnedValue::Short(primitive::<Int16Type>(array, row))
        }
        (DataType::Int32, ColumnType::Int) => OwnedValue::Int(primitive::<Int32Type>(array, row)),
        (DataType::Int64, ColumnType::Long) => OwnedValue::Long(primitive::<Int64Type>(array, row)),
        (DataType::UInt8, ColumnType::UByte) => {
            OwnedValue::UByte(primitive::<UInt8Type>(array, row))
        }
        (DataType::UInt16, ColumnType::UShort) => {
            OwnedValue::UShort(primitive::<UInt16Type>(array, row))
        }
        (DataType::UInt32, ColumnType::UInt) => {
            OwnedValue::UInt(primitive::<UInt32Type>(array, row))
        }
        (DataType::UInt64, ColumnType::ULong) => {
            let v = primitive::<UInt64Type>(array, row);
            // `i64::MAX` 超は `as_i64` 経由の reader を持つ後段（PostgreSQL bigint 等）で
            // roundtrip しないため、Warn 経路でも警告を出して続行する。
            if i64::try_from(v).is_err() {
                apply_on_loss(loss_kind::UINT64_OVERFLOW_ON_FGB, name, on_loss)?;
            }
            OwnedValue::ULong(v)
        }
        (DataType::Float32, ColumnType::Float) => {
            OwnedValue::Float(primitive::<Float32Type>(array, row))
        }
        (DataType::Float64, ColumnType::Double) => {
            OwnedValue::Double(primitive::<Float64Type>(array, row))
        }
        (DataType::Utf8, ColumnType::String) => {
            OwnedValue::String(array.as_string::<i32>().value(row).to_string())
        }
        (DataType::LargeUtf8, ColumnType::String) => {
            OwnedValue::String(array.as_string::<i64>().value(row).to_string())
        }
        (DataType::Binary, ColumnType::Binary) => {
            OwnedValue::Binary(array.as_binary::<i32>().value(row).to_vec())
        }
        (DataType::LargeBinary, ColumnType::Binary) => {
            OwnedValue::Binary(array.as_binary::<i64>().value(row).to_vec())
        }
        (DataType::Date32, ColumnType::DateTime) => {
            let nd = date32_to_naive(primitive::<Date32Type>(array, row));
            OwnedValue::DateTime(nd.format("%Y-%m-%d").to_string())
        }
        (DataType::Date64, ColumnType::DateTime) => {
            let ms = primitive::<Date64Type>(array, row);
            let dt = DateTime::from_timestamp_millis(ms)
                .ok_or_else(|| driver_msg(format!("invalid Date64 in field `{name}`")))?;
            OwnedValue::DateTime(dt.naive_utc().date().format("%Y-%m-%d").to_string())
        }
        (DataType::Timestamp(unit, tz), ColumnType::DateTime) => {
            let raw = match unit {
                TimeUnit::Second => primitive::<TimestampSecondType>(array, row),
                TimeUnit::Millisecond => primitive::<TimestampMillisecondType>(array, row),
                TimeUnit::Microsecond => primitive::<TimestampMicrosecondType>(array, row),
                TimeUnit::Nanosecond => primitive::<TimestampNanosecondType>(array, row),
            };
            OwnedValue::DateTime(format_timestamp(raw, *unit, tz.as_deref(), name)?)
        }
        // Decimal は OnLoss を `open` で適用済みの前提で Double に降格する。
        (DataType::Decimal128(_p, s), ColumnType::Double) => {
            let raw = primitive::<Decimal128Type>(array, row);
            // f64 表現で精度が落ちることを許容（FGB 仕様上 Decimal を保持できないため）。
            #[allow(clippy::cast_precision_loss)]
            let scaled = (raw as f64) / 10f64.powi(i32::from(*s));
            OwnedValue::Double(scaled)
        }
        (other, ct) => {
            return Err(Error::UnsupportedType {
                from: format!("{other:?} -> {ct:?}"),
                to: DRIVER_NAME.to_string(),
                field: name.clone(),
            });
        }
    };
    Ok(Some(value))
}

fn primitive<T: ArrowPrimitiveType>(array: &dyn Array, row: usize) -> T::Native {
    array
        .as_any()
        .downcast_ref::<PrimitiveArray<T>>()
        .expect("primitive type checked by outer match")
        .value(row)
}

fn date32_to_naive(days: i32) -> NaiveDate {
    NaiveDate::from_ymd_opt(1970, 1, 1).expect("1970-01-01 valid")
        + chrono::Duration::days(i64::from(days))
}

/// `Timestamp` を ISO8601 文字列に整形する。タイムゾーン付き値もすべて UTC に正規化して
/// `Z` 終端で書き出す（reader 側は Z 終端を期待し、ローカル時刻として曖昧にならない）。
fn format_timestamp(raw: i64, unit: TimeUnit, _tz: Option<&str>, field: &str) -> Result<String> {
    let (secs, subsec_nanos) = match unit {
        TimeUnit::Second => (raw, 0),
        TimeUnit::Millisecond => split_with_div(raw, 1_000, 1_000_000),
        TimeUnit::Microsecond => split_with_div(raw, 1_000_000, 1_000),
        TimeUnit::Nanosecond => split_with_div(raw, 1_000_000_000, 1),
    };
    let ns = u32::try_from(subsec_nanos).map_err(|_| {
        driver_msg(format!(
            "invalid sub-second component for timestamp in field `{field}`"
        ))
    })?;
    let frac_format = match unit {
        TimeUnit::Second => "",
        TimeUnit::Millisecond => "%.3f",
        TimeUnit::Microsecond => "%.6f",
        TimeUnit::Nanosecond => "%.9f",
    };
    let body = format!("%Y-%m-%dT%H:%M:%S{frac_format}");
    let dt = Utc
        .timestamp_opt(secs, ns)
        .single()
        .ok_or_else(|| driver_msg(format!("invalid timestamp in field `{field}`")))?;
    Ok(format!("{}Z", dt.format(&body)))
}

fn split_with_div(raw: i64, seconds_factor: i64, sub_to_nanos: i64) -> (i64, i64) {
    let secs = raw.div_euclid(seconds_factor);
    let sub = raw.rem_euclid(seconds_factor);
    (secs, sub * sub_to_nanos)
}
