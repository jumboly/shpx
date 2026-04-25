//! Arrow `RecordBatch` ストリームから GeoJSON / GeoJSONL を生成する。
//!
//! - FeatureCollection (`.geojson`): `{"type":"FeatureCollection","features":[...]}`
//! - GeoJSONL (`.geojsonl` / `.ndjson` / `.jsonl`): 1 行 1 Feature
//!
//! いずれの形式でも RFC 7946 §4 に従い、出力 CRS は EPSG:4326 のみ許可する。
//! それ以外の CRS は [`Error::Crs`] で停止し、ユーザに upstream での reproject を促す。
//! （driver 内蔵 reprojection は `docs/GEOJSON.md` の Future work 参照。）

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::PathBuf;

use arrow_array::{
    cast::AsArray,
    types::{
        Date32Type, Date64Type, Decimal128Type, Float16Type, Float32Type, Float64Type, Int16Type,
        Int32Type, Int64Type, Int8Type, TimestampMicrosecondType, TimestampMillisecondType,
        TimestampNanosecondType, TimestampSecondType, UInt16Type, UInt32Type, UInt64Type,
        UInt8Type,
    },
    Array, ArrowPrimitiveType, PrimitiveArray, RecordBatch,
};
use arrow_schema::{DataType, Field, SchemaRef, TimeUnit};
use chrono::{DateTime, FixedOffset, TimeZone, Utc};
use serde_json::{Map as JsonMap, Number, Value as JsonValue};
use shpx_core::{
    schema::find_geometry_column, Crs, Error, LayerWriter, OnLoss, Result, Uri, WriteOpts,
};
use shpx_geom::wkb;

use crate::geom_convert::geom_to_geometry;
use crate::options::{OutputFormat, ResolvedWriteOpts};
use crate::util::{apply_on_loss, date32, driver_err, driver_msg, loss_kind};

/// GeoJSON / GeoJSONL の `LayerWriter` 実装。
pub struct GeoJsonWriter {
    schema: SchemaRef,
    geom_index: Option<usize>,
    /// `Option` で保持して `finish()` で take する（`Drop` 警告判定にも使う）。
    inner: Option<BufWriter<File>>,
    format: OutputFormat,
    /// FeatureCollection 出力時のみ意味を持つ。最初の feature か否かを覚えてカンマ区切りを制御する。
    first_feature: bool,
    /// `apply_on_loss(.., Skip)` で除外された出力列インデックス（schema 上の位置）。
    skipped_cols: Vec<usize>,
    on_loss: OnLoss,
    pretty: bool,
}

impl GeoJsonWriter {
    pub fn open(uri: &Uri, schema: SchemaRef, crs: Option<&Crs>, opts: &WriteOpts) -> Result<Self> {
        let resolved = ResolvedWriteOpts::resolve(uri, opts)?;
        let path = PathBuf::from(uri.path());

        // RFC 7946 §4: WGS84 (EPSG:4326) のみ許可。reprojection 未実装のため明示エラーで停止する。
        match crs {
            None => {} // 既定 EPSG:4326 として扱う（書き出し時に `crs` メンバは出さない）
            Some(c) if c.epsg_code() == Some(4326) => {}
            Some(c) => {
                return Err(Error::Crs(format!(
                    "geojson writer requires EPSG:4326 (RFC 7946); got {:?}. \
                     Reproject upstream before writing.",
                    c.epsg_code()
                )));
            }
        }

        if !resolved.overwrite && path.exists() {
            return Err(Error::Format(format!(
                "output already exists: {} (use overwrite)",
                path.display()
            )));
        }

        let geom_index = find_geometry_column(&schema)?.map(|(i, _, _)| i);
        let skipped_cols = plan_skipped_columns(&schema, geom_index, opts.on_loss)?;

        let file = File::create(&path).map_err(Error::from)?;
        let mut inner = BufWriter::new(file);

        if matches!(resolved.format, OutputFormat::FeatureCollection) {
            // RFC 7946 §3.3 に従い、`crs` メンバは出力しない（WGS84 既定のため）。
            inner
                .write_all(br#"{"type":"FeatureCollection","features":["#)
                .map_err(Error::from)?;
        }

        Ok(Self {
            schema,
            geom_index,
            inner: Some(inner),
            format: resolved.format,
            first_feature: true,
            skipped_cols,
            on_loss: opts.on_loss,
            pretty: resolved.pretty,
        })
    }
}

impl LayerWriter for GeoJsonWriter {
    fn write_batch(&mut self, batch: &RecordBatch) -> Result<()> {
        let cols: Vec<&dyn Array> = (0..batch.num_columns())
            .map(|i| batch.column(i).as_ref())
            .collect();
        let fields: Vec<&Field> = self.schema.fields().iter().map(AsRef::as_ref).collect();

        let writer = self
            .inner
            .as_mut()
            .ok_or_else(|| driver_msg("write_batch called after finish"))?;

        for row in 0..batch.num_rows() {
            let feature = build_feature_json(
                &fields,
                &cols,
                row,
                self.geom_index,
                &self.skipped_cols,
                self.on_loss,
            )?;
            let serialized =
                if self.pretty && matches!(self.format, OutputFormat::FeatureCollection) {
                    serde_json::to_string_pretty(&feature)
                } else {
                    serde_json::to_string(&feature)
                }
                .map_err(|e| driver_err(&e))?;

            match self.format {
                OutputFormat::FeatureCollection => {
                    if !self.first_feature {
                        writer.write_all(b",").map_err(Error::from)?;
                    }
                    self.first_feature = false;
                    writer
                        .write_all(serialized.as_bytes())
                        .map_err(Error::from)?;
                }
                OutputFormat::Lines => {
                    // 1 行 1 Feature。pretty フラグは Lines では無視（改行を入れると不正な NDJSON になる）。
                    writer
                        .write_all(serialized.as_bytes())
                        .map_err(Error::from)?;
                    writer.write_all(b"\n").map_err(Error::from)?;
                }
            }
        }
        Ok(())
    }

    fn finish(mut self: Box<Self>) -> Result<()> {
        let mut writer = self
            .inner
            .take()
            .ok_or_else(|| driver_msg("finish called twice"))?;
        if matches!(self.format, OutputFormat::FeatureCollection) {
            writer.write_all(b"]}").map_err(Error::from)?;
        }
        writer.flush().map_err(Error::from)?;
        Ok(())
    }
}

impl Drop for GeoJsonWriter {
    fn drop(&mut self) {
        if self.inner.is_some() {
            tracing::warn!(target: "shpx::geojson", "GeoJsonWriter dropped without finish()");
        }
    }
}

/// 1 Feature 分の `serde_json::Value` を組み立てる。
fn build_feature_json(
    fields: &[&Field],
    cols: &[&dyn Array],
    row: usize,
    geom_index: Option<usize>,
    skipped_cols: &[usize],
    on_loss: OnLoss,
) -> Result<JsonValue> {
    // 出力 JSON のキー順は serde_json::Map (BTreeMap) によって alphabetical になる。
    // roundtrip 時の Arrow 列順は reader 側の「最初の出現順」で復元されるため、
    // ここで挿入順を保つ必要は無い。
    let mut properties = JsonMap::new();
    for (i, field) in fields.iter().enumerate() {
        if Some(i) == geom_index || skipped_cols.contains(&i) {
            continue;
        }
        let value = arrow_value_to_json(field, cols[i], row, on_loss)?;
        properties.insert(field.name().clone(), value);
    }

    let geometry = match geom_index {
        Some(gi) if cols[gi].is_null(row) => JsonValue::Null,
        Some(gi) => {
            let bytes = cols[gi].as_binary::<i32>().value(row);
            let geom = wkb::decode(bytes)?;
            serde_json::to_value(geom_to_geometry(&geom)).map_err(|e| driver_err(&e))?
        }
        None => JsonValue::Null,
    };

    let mut feat = JsonMap::new();
    feat.insert("type".into(), JsonValue::String("Feature".into()));
    feat.insert("geometry".into(), geometry);
    feat.insert("properties".into(), JsonValue::Object(properties));
    Ok(JsonValue::Object(feat))
}

/// schema を走査して、出力できない型の列を `on_loss` に従って除外する。
fn plan_skipped_columns(
    schema: &SchemaRef,
    geom_index: Option<usize>,
    on_loss: OnLoss,
) -> Result<Vec<usize>> {
    let mut skipped = Vec::new();
    for (i, f) in schema.fields().iter().enumerate() {
        if Some(i) == geom_index {
            continue;
        }
        match f.data_type() {
            DataType::Binary | DataType::LargeBinary => {
                if !apply_on_loss(loss_kind::BINARY_ON_GEOJSON, f.name(), on_loss)? {
                    skipped.push(i);
                }
            }
            DataType::List(_)
            | DataType::LargeList(_)
            | DataType::Struct(_)
            | DataType::Map(_, _) => {
                // Warn 経路でも JSON 文字列化が未実装のため、Skip 以外は書き出し時に拒否する。
                if apply_on_loss(loss_kind::STRUCTURED_ON_GEOJSON, f.name(), on_loss)? {
                    return Err(driver_msg(format!(
                        "structured column `{}` ({:?}) is not supported by GeoJSON writer",
                        f.name(),
                        f.data_type()
                    )));
                }
                skipped.push(i);
            }
            _ => {}
        }
    }
    Ok(skipped)
}

/// Arrow 値を JSON 値に変換する。
fn arrow_value_to_json(
    field: &Field,
    array: &dyn Array,
    row: usize,
    on_loss: OnLoss,
) -> Result<JsonValue> {
    if array.is_null(row) {
        return Ok(JsonValue::Null);
    }
    let name = field.name();
    match field.data_type() {
        DataType::Boolean => Ok(JsonValue::Bool(array.as_boolean().value(row))),
        DataType::Int8 => Ok(int_to_json(i64::from(primitive::<Int8Type>(array, row)))),
        DataType::Int16 => Ok(int_to_json(i64::from(primitive::<Int16Type>(array, row)))),
        DataType::Int32 => Ok(int_to_json(i64::from(primitive::<Int32Type>(array, row)))),
        DataType::Int64 => Ok(int_to_json(primitive::<Int64Type>(array, row))),
        DataType::UInt8 => Ok(uint_to_json(u64::from(primitive::<UInt8Type>(array, row)))),
        DataType::UInt16 => Ok(uint_to_json(u64::from(primitive::<UInt16Type>(array, row)))),
        DataType::UInt32 => Ok(uint_to_json(u64::from(primitive::<UInt32Type>(array, row)))),
        DataType::UInt64 => uint64_to_json(primitive::<UInt64Type>(array, row), name, on_loss),
        DataType::Float16 => float_to_json(
            f64::from(primitive::<Float16Type>(array, row).to_f32()),
            name,
            on_loss,
        ),
        DataType::Float32 => float_to_json(
            f64::from(primitive::<Float32Type>(array, row)),
            name,
            on_loss,
        ),
        DataType::Float64 => float_to_json(primitive::<Float64Type>(array, row), name, on_loss),
        DataType::Decimal128(_p, s) => {
            apply_on_loss(loss_kind::DECIMAL_ON_GEOJSON, name, on_loss)?;
            Ok(JsonValue::String(format_decimal128(
                primitive::<Decimal128Type>(array, row),
                *s,
            )))
        }
        DataType::Utf8 => Ok(JsonValue::String(
            array.as_string::<i32>().value(row).to_string(),
        )),
        DataType::LargeUtf8 => Ok(JsonValue::String(
            array.as_string::<i64>().value(row).to_string(),
        )),
        DataType::Date32 => {
            let nd = date32::to_naive(primitive::<Date32Type>(array, row));
            Ok(JsonValue::String(nd.format("%Y-%m-%d").to_string()))
        }
        DataType::Date64 => {
            let ms = primitive::<Date64Type>(array, row);
            let dt = DateTime::from_timestamp_millis(ms)
                .ok_or_else(|| driver_msg(format!("invalid Date64 in field `{name}`")))?;
            Ok(JsonValue::String(
                dt.naive_utc().date().format("%Y-%m-%d").to_string(),
            ))
        }
        DataType::Timestamp(unit, tz) => {
            let raw = match unit {
                TimeUnit::Second => primitive::<TimestampSecondType>(array, row),
                TimeUnit::Millisecond => primitive::<TimestampMillisecondType>(array, row),
                TimeUnit::Microsecond => primitive::<TimestampMicrosecondType>(array, row),
                TimeUnit::Nanosecond => primitive::<TimestampNanosecondType>(array, row),
            };
            Ok(JsonValue::String(format_timestamp(
                raw,
                *unit,
                tz.as_deref(),
                name,
            )?))
        }
        // plan_skipped_columns で Skip 経路は除外済み。Warn 経路で残った場合は null に潰す。
        DataType::Binary | DataType::LargeBinary => Ok(JsonValue::Null),
        other => Err(driver_msg(format!(
            "unsupported Arrow → JSON mapping for field `{name}`: {other:?}"
        ))),
    }
}

fn int_to_json(v: i64) -> JsonValue {
    JsonValue::Number(Number::from(v))
}

fn uint_to_json(v: u64) -> JsonValue {
    JsonValue::Number(Number::from(v))
}

/// `UInt64` のうち `i64::MAX` 超は JSON Number で表現可能だが、後段の Reader 側で
/// `as_i64()` が `None` を返すため roundtrip しない。安全のため Warn 経路では文字列化、
/// Error 経路では停止する。
fn uint64_to_json(v: u64, field: &str, on_loss: OnLoss) -> Result<JsonValue> {
    if i64::try_from(v).is_ok() {
        return Ok(JsonValue::Number(Number::from(v)));
    }
    apply_on_loss(loss_kind::UINT64_OVERFLOW_ON_GEOJSON, field, on_loss)?;
    Ok(JsonValue::String(v.to_string()))
}

/// `f32` / `f64` の NaN / Infinity は RFC 8259 で JSON Number として禁じられているため、
/// Warn 経路では `null` に潰し、Error 経路では停止する。
fn float_to_json(v: f64, field: &str, on_loss: OnLoss) -> Result<JsonValue> {
    if let Some(n) = Number::from_f64(v) {
        return Ok(JsonValue::Number(n));
    }
    apply_on_loss(loss_kind::NONFINITE_FLOAT_ON_GEOJSON, field, on_loss)?;
    Ok(JsonValue::Null)
}

// 以下は CSV writer からの複製。3 ドライバで重複したため `shpx-core` への
// 共通化を `docs/GEOJSON.md` の Future work に記載済み。

fn format_decimal128(value: i128, scale: i8) -> String {
    if scale <= 0 {
        return value.to_string();
    }
    let s = usize::try_from(scale).expect("scale fits in usize when > 0");
    let negative = value < 0;
    let abs = if negative {
        value.unsigned_abs().to_string()
    } else {
        value.to_string()
    };
    let mut out = String::new();
    if negative {
        out.push('-');
    }
    if abs.len() > s {
        let split = abs.len() - s;
        out.push_str(&abs[..split]);
        out.push('.');
        out.push_str(&abs[split..]);
    } else {
        out.push_str("0.");
        for _ in 0..(s - abs.len()) {
            out.push('0');
        }
        out.push_str(&abs);
    }
    out
}

fn format_timestamp(raw: i64, unit: TimeUnit, tz: Option<&str>, field: &str) -> Result<String> {
    let (secs, subsec_nanos) = match unit {
        TimeUnit::Second => (raw, 0),
        TimeUnit::Millisecond => split_with_div(raw, 1_000, 1_000_000),
        TimeUnit::Microsecond => split_with_div(raw, 1_000_000, 1_000),
        TimeUnit::Nanosecond => split_with_div(raw, 1_000_000_000, 1),
    };
    let secs_i32 = u32::try_from(subsec_nanos).map_err(|_| {
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
    let body_format = format!("%Y-%m-%dT%H:%M:%S{frac_format}");
    match tz {
        None => {
            let dt = DateTime::from_timestamp(secs, secs_i32)
                .ok_or_else(|| driver_msg(format!("invalid naive timestamp in field `{field}`")))?
                .naive_utc();
            Ok(dt.format(&body_format).to_string())
        }
        Some("UTC" | "Z" | "+00:00" | "-00:00") => {
            let dt = Utc
                .timestamp_opt(secs, secs_i32)
                .single()
                .ok_or_else(|| driver_msg(format!("invalid UTC timestamp in field `{field}`")))?;
            Ok(format!("{}Z", dt.format(&body_format)))
        }
        Some(other) => {
            let offset = parse_fixed_offset(other).ok_or_else(|| {
                driver_msg(format!(
                    "unsupported timestamp tz `{other}` in field `{field}`"
                ))
            })?;
            let dt = offset
                .timestamp_opt(secs, secs_i32)
                .single()
                .ok_or_else(|| driver_msg(format!("invalid timestamp in field `{field}`")))?;
            Ok(dt.format(&format!("{body_format}%:z")).to_string())
        }
    }
}

fn split_with_div(raw: i64, seconds_factor: i64, sub_to_nanos: i64) -> (i64, i64) {
    let secs = raw.div_euclid(seconds_factor);
    let sub = raw.rem_euclid(seconds_factor);
    (secs, sub * sub_to_nanos)
}

fn parse_fixed_offset(tz: &str) -> Option<FixedOffset> {
    let bytes = tz.as_bytes();
    if bytes.len() != 6 {
        return None;
    }
    let sign = match bytes[0] {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    if bytes[3] != b':' {
        return None;
    }
    let h: i32 = std::str::from_utf8(&bytes[1..3]).ok()?.parse().ok()?;
    let m: i32 = std::str::from_utf8(&bytes[4..6]).ok()?.parse().ok()?;
    FixedOffset::east_opt(sign * (h * 3600 + m * 60))
}

fn primitive<T: ArrowPrimitiveType>(array: &dyn Array, row: usize) -> T::Native {
    array
        .as_any()
        .downcast_ref::<PrimitiveArray<T>>()
        .expect("primitive type checked by outer match")
        .value(row)
}
