//! Arrow `RecordBatch` ストリームから CSV/TSV を生成する。

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
use encoding_rs::{Encoding, UTF_8};
use shpx_core::{
    schema::find_geometry_column, Crs, Error, LayerWriter, OnLoss, Result, Uri, WriteOpts,
};
use shpx_geom::{wkb, wkt};

use crate::options::{BomPolicy, ResolvedWriteOpts};
use crate::util::{apply_on_loss, date32, driver_err, driver_msg, loss_kind};

/// CSV/TSV の `LayerWriter` 実装。
pub struct CsvWriter {
    schema: SchemaRef,
    geom_index: Option<usize>,
    /// `Option` で保持しておくと、`finish()` で所有権を奪って `into_inner()` できる。
    /// 同時に `Some` か否かが「`finish()` 未呼び出し」のフラグになるため `Drop` 警告判定にも使う。
    inner: Option<csv::Writer<EncodingWriter>>,
    /// `apply_on_loss(.., Skip)` で除外された出力列インデックス（schema 上の位置）。
    skipped_cols: Vec<usize>,
}

impl CsvWriter {
    pub fn open(uri: &Uri, schema: SchemaRef, _crs: Option<Crs>, opts: &WriteOpts) -> Result<Self> {
        let resolved = ResolvedWriteOpts::resolve(uri, opts)?;
        let path = PathBuf::from(uri.path());

        if !resolved.overwrite && path.exists() {
            return Err(Error::Format(format!(
                "output already exists: {} (use overwrite)",
                path.display()
            )));
        }

        let file = File::create(&path).map_err(Error::from)?;
        let mut sink = EncodingWriter::new(BufWriter::new(file), resolved.encoding);

        // BOM は生のバイトとして CSV writer 経由ではなく直接書く。
        if should_write_bom(resolved.bom, resolved.encoding) {
            sink.write_raw(&[0xEF, 0xBB, 0xBF])
                .map_err(|e| driver_err(&e))?;
        }

        let geom_index = find_geometry_column(&schema)?.map(|(i, _, _)| i);

        // schema を走査し、出力できない型を持つ列を on_loss に従って除外する。
        let skipped_cols = plan_skipped_columns(&schema, geom_index, opts.on_loss)?;

        let mut builder = csv::WriterBuilder::new();
        builder.delimiter(resolved.delimiter);
        // CSV writer は flush を `finish()` 経由で明示する。
        builder.has_headers(false);
        let mut inner = builder.from_writer(sink);

        // ヘッダ行を書く。
        let header: Vec<&str> = schema
            .fields()
            .iter()
            .enumerate()
            .filter_map(|(i, f)| {
                if skipped_cols.contains(&i) {
                    None
                } else {
                    Some(f.name().as_str())
                }
            })
            .collect();
        inner.write_record(&header).map_err(|e| driver_err(&e))?;

        Ok(Self {
            schema,
            geom_index,
            inner: Some(inner),
            skipped_cols,
        })
    }
}

fn arrow_value_to_csv(field: &Field, array: &dyn Array, row: usize) -> Result<String> {
    if array.is_null(row) {
        return Ok(String::new());
    }
    let name = field.name();
    match field.data_type() {
        DataType::Utf8 => Ok(array.as_string::<i32>().value(row).to_string()),
        DataType::LargeUtf8 => Ok(array.as_string::<i64>().value(row).to_string()),
        DataType::Boolean => Ok(if array.as_boolean().value(row) {
            "true".to_string()
        } else {
            "false".to_string()
        }),
        DataType::Int8 => Ok(primitive::<Int8Type>(array, row).to_string()),
        DataType::Int16 => Ok(primitive::<Int16Type>(array, row).to_string()),
        DataType::Int32 => Ok(primitive::<Int32Type>(array, row).to_string()),
        DataType::Int64 => Ok(primitive::<Int64Type>(array, row).to_string()),
        DataType::UInt8 => Ok(primitive::<UInt8Type>(array, row).to_string()),
        DataType::UInt16 => Ok(primitive::<UInt16Type>(array, row).to_string()),
        DataType::UInt32 => Ok(primitive::<UInt32Type>(array, row).to_string()),
        DataType::UInt64 => Ok(primitive::<UInt64Type>(array, row).to_string()),
        DataType::Float16 => Ok(primitive::<Float16Type>(array, row).to_f32().to_string()),
        DataType::Float32 => Ok(primitive::<Float32Type>(array, row).to_string()),
        DataType::Float64 => Ok(primitive::<Float64Type>(array, row).to_string()),
        DataType::Decimal128(_p, s) => Ok(format_decimal128(
            primitive::<Decimal128Type>(array, row),
            *s,
        )),
        DataType::Date32 => {
            let nd = date32::to_naive(primitive::<Date32Type>(array, row));
            Ok(nd.format("%Y-%m-%d").to_string())
        }
        DataType::Date64 => {
            let ms = primitive::<Date64Type>(array, row);
            let dt = DateTime::from_timestamp_millis(ms)
                .ok_or_else(|| driver_msg(format!("invalid Date64 in field `{name}`")))?;
            Ok(dt.naive_utc().date().format("%Y-%m-%d").to_string())
        }
        DataType::Timestamp(unit, tz) => {
            let raw = match unit {
                TimeUnit::Second => primitive::<TimestampSecondType>(array, row),
                TimeUnit::Millisecond => primitive::<TimestampMillisecondType>(array, row),
                TimeUnit::Microsecond => primitive::<TimestampMicrosecondType>(array, row),
                TimeUnit::Nanosecond => primitive::<TimestampNanosecondType>(array, row),
            };
            Ok(format_timestamp(raw, *unit, tz.as_deref(), name)?)
        }
        DataType::Binary | DataType::LargeBinary => {
            // schema 構築時に on_loss を適用済みなので、ここに来るのは Warn 経路（空文字に潰す）。
            Ok(String::new())
        }
        other => Err(driver_msg(format!(
            "unsupported Arrow → CSV mapping for field `{name}`: {other:?}"
        ))),
    }
}

impl LayerWriter for CsvWriter {
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
            let mut record: Vec<String> =
                Vec::with_capacity(fields.len() - self.skipped_cols.len());
            for (i, field) in fields.iter().enumerate() {
                if self.skipped_cols.contains(&i) {
                    continue;
                }
                let value = if Some(i) == self.geom_index {
                    if cols[i].is_null(row) {
                        String::new()
                    } else {
                        let bytes = cols[i].as_binary::<i32>().value(row);
                        let g = wkb::decode(bytes)?;
                        wkt::encode(&g)?
                    }
                } else {
                    arrow_value_to_csv(field, cols[i], row)?
                };
                record.push(value);
            }
            writer.write_record(&record).map_err(|e| driver_err(&e))?;
        }
        Ok(())
    }

    fn finish(mut self: Box<Self>) -> Result<()> {
        let mut writer = self
            .inner
            .take()
            .ok_or_else(|| driver_msg("finish called twice"))?;
        writer.flush().map_err(|e| driver_err(&e))?;
        let sink = writer.into_inner().map_err(|e| driver_err(&e.error()))?;
        sink.into_inner_flush().map_err(|e| driver_err(&e))?;
        Ok(())
    }
}

impl Drop for CsvWriter {
    fn drop(&mut self) {
        if self.inner.is_some() {
            tracing::warn!(target: "shpx::csv", "CsvWriter dropped without finish()");
        }
    }
}

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
                if !apply_on_loss(loss_kind::BINARY_ON_CSV, f.name(), on_loss)? {
                    skipped.push(i);
                }
            }
            DataType::List(_)
            | DataType::LargeList(_)
            | DataType::Struct(_)
            | DataType::Map(_, _) => {
                if apply_on_loss(loss_kind::STRUCTURED_ON_CSV, f.name(), on_loss)? {
                    // Warn でも v0.2 サイクル 1 では JSON 文字列化を実装しないため、書き出し時に拒否する。
                    return Err(driver_msg(format!(
                        "structured column `{}` ({:?}) is not supported by CSV writer in v0.2 cycle 1",
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

fn format_decimal128(value: i128, scale: i8) -> String {
    if scale <= 0 {
        // scale が 0 以下なら整数表記。負の scale (10^|s| 倍) は v0.2 で扱わない（Arrow 仕様上稀）。
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
        // 整数部が 0 で先頭に 0 埋めが必要なケース。
        out.push_str("0.");
        for _ in 0..(s - abs.len()) {
            out.push('0');
        }
        out.push_str(&abs);
    }
    out
}

fn format_timestamp(raw: i64, unit: TimeUnit, tz: Option<&str>, field: &str) -> Result<String> {
    // 値を nanos に揃えて split する（loss 抑止）。
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

    // フォーマット文字列を unit ごとに切り替え、不要な末尾 0 を出さない。
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
            // chrono の `%:z` は `+09:00` 形式。
            Ok(dt.format(&format!("{body_format}%:z")).to_string())
        }
    }
}

/// `raw` を `seconds_factor` 単位の秒に変換し、残り (sub-second) を nanos で返す。
fn split_with_div(raw: i64, seconds_factor: i64, sub_to_nanos: i64) -> (i64, i64) {
    let secs = raw.div_euclid(seconds_factor);
    let sub = raw.rem_euclid(seconds_factor);
    (secs, sub * sub_to_nanos)
}

fn parse_fixed_offset(tz: &str) -> Option<FixedOffset> {
    // chrono は `%:z` パースを直接 expose しないため、`+09:00` / `-08:30` を手動で解釈する。
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

fn should_write_bom(policy: BomPolicy, encoding: &'static Encoding) -> bool {
    match policy {
        // Auto と Always は両方とも UTF-8 のときだけ BOM を書く（CP932 等に BOM を付けない）。
        BomPolicy::Auto | BomPolicy::Always => encoding == UTF_8,
        BomPolicy::Never => false,
    }
}

/// `csv::Writer` から `BufWriter` を取り出すためのラッパ。
/// 書き込み時にエンコーディング変換 (UTF-8 → 任意 codec) を挟む。
pub struct EncodingWriter {
    inner: BufWriter<File>,
    encoding: &'static Encoding,
}

impl EncodingWriter {
    fn new(inner: BufWriter<File>, encoding: &'static Encoding) -> Self {
        Self { inner, encoding }
    }

    fn write_raw(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        self.inner.write_all(bytes)
    }

    fn into_inner_flush(mut self) -> std::io::Result<()> {
        self.inner.flush()?;
        Ok(())
    }
}

impl Write for EncodingWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        // UTF-8 同士なら codec 変換は no-op。`encoding_rs::UTF_8::encode` 経由でも結果は同じだが
        // 毎回 `Cow` の中身をコピーするため、ここで bypass してアロケーションを避ける。
        if self.encoding == UTF_8 {
            return self.inner.write(buf);
        }
        // `csv::Writer` は valid UTF-8 のレコードしか流さないため、`from_utf8` は失敗しない。
        let s = std::str::from_utf8(buf)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let (encoded, _, _had_unmapped) = self.encoding.encode(s);
        self.inner.write_all(&encoded)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}
