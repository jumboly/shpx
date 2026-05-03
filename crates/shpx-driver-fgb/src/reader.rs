//! FlatGeobuf を Arrow `RecordBatch` ストリームとして読み出す。
//!
//! - ヘッダから列定義 / CRS / geometry_type を取得し Arrow Schema を構築
//! - 各 feature の geometry は geozero `WkbWriter` で WKB バイト列に変換
//! - 属性は `PropertyProcessor` で 1 列ずつ受け取り、Arrow `ArrayBuilder` に蓄積
//!
//! v0.8 cycle 1 で eager-load (`VecDeque<Row>`) をやめ、`FeatureIter<BufReader<File>,
//! NotSeekable>` を field に保持する真のストリーミング化を行った。`FallibleStreamingIterator`
//! の特性上、`feature_iter.next()?` は 1 feature ずつ進むので READ_BATCH_SIZE 件回せば
//! 1 batch 完成。
//!
//! DateTime → Date32 の refine 推定は streaming と相性が悪い (全行を見ないと型が確定
//! しない) ため、**最初の SAMPLE_LIMIT 件 (= 1 batch ぶん) を open() で先読みして
//! refine 判定** に使い、その判定が「全行 walk 済み」なら Date32 に絞る。サンプルが
//! ファイル全体を覆えなかった場合は安全側に倒し header 宣言の `Timestamp` を採用する。

use std::fs::File;
use std::io::BufReader;
use std::path::PathBuf;
use std::sync::Arc;

use arrow_array::builder::{
    BinaryBuilder, BooleanBuilder, Date32Builder, Float32Builder, Float64Builder, Int16Builder,
    Int32Builder, Int64Builder, Int8Builder, StringBuilder, TimestampMicrosecondBuilder,
    UInt16Builder, UInt32Builder, UInt64Builder, UInt8Builder,
};
use arrow_array::{ArrayRef, RecordBatch};
use arrow_schema::{DataType, Field, Schema, SchemaRef};
use chrono::NaiveDate;
use flatgeobuf::{
    ColumnType, FallibleStreamingIterator, FeatureIter, FgbReader as InnerFgbReader,
    GeometryType as FgbGeomType, NotSeekable,
};
use geozero::error::GeozeroError;
use geozero::{
    wkb::{WkbDialect, WkbWriter},
    ColumnValue, FeatureProperties, GeozeroGeometry, PropertyProcessor,
};
use shpx_core::{
    schema::{GeometryMeta, GeometryType, GEOMETRY_META_KEY},
    Crs, Error, LayerReader, ReadOpts, Result, Uri, WktFlavor,
};

use crate::type_map::{fgb_column_to_arrow_field, fgb_to_shpx_geometry_type};
use crate::util::{driver_err, driver_msg};
use crate::value::{from_column_value, OwnedValue};

/// 1 batch あたりの行数。
const READ_BATCH_SIZE: usize = 65_536;
/// open() 時に DateTime → Date32 refine のため先読みするサンプル上限。
/// サンプルでファイル末尾に到達できれば全行 walk 済みなので安全に Date32 に絞れる。
const SAMPLE_LIMIT: usize = READ_BATCH_SIZE;

/// FlatGeobuf の `LayerReader` 実装。
pub struct FgbReader {
    schema: SchemaRef,
    crs: Option<Crs>,
    /// FGB 列名 / FGB ColumnType / Arrow 列計画。
    columns: Vec<ColumnPlan>,
    has_z: bool,
    has_m: bool,
    /// open() 時に refine 用に先読みした sample。batches() で先に流す。
    sample: Option<Vec<Row>>,
    /// streaming 用の FeatureIter。`select_all_seq()` で `FgbReader` を消費して
    /// 内部 reader を所有させているため self-referential にならない。
    feature_iter: Option<FeatureIter<BufReader<File>, NotSeekable>>,
    row_count_hint: Option<usize>,
}

#[derive(Debug, Clone)]
struct ColumnPlan {
    name: String,
    column_type: ColumnType,
    arrow_type: DataType,
    nullable: bool,
}

/// 1 行分の値。geometry は WKB バイト列、属性は OwnedValue で持つ。
struct Row {
    geometry: Option<Vec<u8>>,
    /// 列順は ColumnPlan と一致。`None` は NULL。
    values: Vec<Option<OwnedValue>>,
}

impl FgbReader {
    pub fn open(uri: &Uri, opts: &ReadOpts) -> Result<Self> {
        let path = PathBuf::from(uri.path());
        let file = File::open(&path).map_err(Error::from)?;
        let reader = BufReader::new(file);
        let fgb = InnerFgbReader::open(reader).map_err(|e| driver_err(&e))?;

        // ヘッダから列・geometry_type・CRS を抽出する。`select_all_seq` は `fgb` を
        // 消費するため、ヘッダから必要な値はあらかじめ owned 形に取り出しておく。
        let (columns, geom_type, crs_from_header, has_z, has_m, features_count) = {
            let header = fgb.header();
            let columns: Vec<ColumnPlan> = match header.columns() {
                None => Vec::new(),
                Some(cols) => (0..cols.len())
                    .map(|i| {
                        let c = cols.get(i);
                        let column_type = c.type_();
                        let nullable = c.nullable();
                        let field = fgb_column_to_arrow_field(c.name(), column_type, nullable);
                        ColumnPlan {
                            name: c.name().to_string(),
                            column_type,
                            arrow_type: field.data_type().clone(),
                            nullable,
                        }
                    })
                    .collect(),
            };
            let geom_type = match header.geometry_type() {
                FgbGeomType::Unknown => GeometryType::Geometry,
                other => fgb_to_shpx_geometry_type(other),
            };
            let crs_from_header = decode_header_crs(&header);
            let features_count = header.features_count();
            (
                columns,
                geom_type,
                crs_from_header,
                header.has_z(),
                header.has_m(),
                features_count,
            )
        };

        let crs = opts.src_crs.clone().or(crs_from_header);

        let mut feature_iter = fgb.select_all_seq().map_err(|e| driver_err(&e))?;

        // DateTime 列の有無で sample 先読みの要否が決まる: refine 対象が無ければサンプル不要で
        // 即 streaming に入れる (典型 FGB は DateTime 列なし、peak RSS と open() latency を削減)。
        let needs_refine = columns
            .iter()
            .any(|c| matches!(c.column_type, ColumnType::DateTime));

        // refine が必要な場合のみ、先頭 SAMPLE_LIMIT 件をバッファに先読みする。
        // ファイルが SAMPLE_LIMIT 以下で全行 walk 済みなら、observe-based refine で型を絞る。
        // 越えた場合は header 宣言型 (= Timestamp) のままにし、誤った Date32 化を避ける。
        let (sample, sample_exhausted_file) = if needs_refine {
            let mut buf: Vec<Row> = Vec::with_capacity(SAMPLE_LIMIT);
            let mut exhausted = false;
            loop {
                match feature_iter.next() {
                    Ok(None) => {
                        exhausted = true;
                        break;
                    }
                    Ok(Some(feat)) => {
                        let row = collect_row(feat, &columns, has_z, has_m)?;
                        buf.push(row);
                        if buf.len() >= SAMPLE_LIMIT {
                            break;
                        }
                    }
                    Err(e) => return Err(driver_err(&e)),
                }
            }
            (buf, exhausted)
        } else {
            (Vec::new(), false)
        };

        // 列の Arrow 型を観測値で絞り込む（DateTime → Date32 / Timestamp）。
        // sample がファイル全体を覆っている時のみ適用する。
        let arrow_types = if sample_exhausted_file {
            refine_arrow_types(&columns, &sample)
        } else {
            columns.iter().map(|c| c.arrow_type.clone()).collect()
        };
        let columns: Vec<ColumnPlan> = columns
            .into_iter()
            .zip(arrow_types)
            .map(|(mut c, dt)| {
                c.arrow_type = dt;
                c
            })
            .collect();

        let schema = build_schema(&columns, geom_type, crs.as_ref())?;

        // FGB header の features_count は信頼できるなら使う (ストリーミング reader でも
        // 進捗バー分母を提供するため)。0 は「不定」を表す慣習。
        let row_count_hint = match (sample_exhausted_file, features_count) {
            (true, _) => Some(sample.len()),
            (false, n) if n > 0 => usize::try_from(n).ok(),
            _ => None,
        };

        // sample 経路が走らなかった (DateTime 列なし) or 走ったがファイル末尾に到達 した
        // 場合は feature_iter を保持する必要なし。
        let feature_iter = if sample_exhausted_file {
            None
        } else {
            Some(feature_iter)
        };

        Ok(Self {
            schema,
            crs,
            columns,
            has_z,
            has_m,
            sample: Some(sample),
            feature_iter,
            row_count_hint,
        })
    }
}

/// 1 feature 分の geometry / properties をまとめて取り出す。
fn collect_row(
    feature: &flatgeobuf::FgbFeature,
    columns: &[ColumnPlan],
    has_z: bool,
    has_m: bool,
) -> Result<Row> {
    // Geometry → WKB
    let geometry = if feature.geometry().is_some() {
        let mut buf: Vec<u8> = Vec::new();
        let mut writer = WkbWriter::with_opts(
            &mut buf,
            WkbDialect::Wkb,
            geozero::CoordDimensions {
                z: has_z,
                m: has_m,
                t: false,
                tm: false,
            },
            None,
            Vec::new(),
        );
        feature
            .process_geom(&mut writer)
            .map_err(|e| driver_err(&e))?;
        Some(buf)
    } else {
        None
    };

    // Properties → OwnedValue 配列
    let mut collector = PropCollector::new(columns.len());
    feature
        .process_properties(&mut collector)
        .map_err(|e| driver_err(&e))?;

    Ok(Row {
        geometry,
        values: collector.values,
    })
}

struct PropCollector {
    values: Vec<Option<OwnedValue>>,
}

impl PropCollector {
    fn new(n: usize) -> Self {
        Self {
            values: vec![None; n],
        }
    }
}

impl PropertyProcessor for PropCollector {
    fn property(
        &mut self,
        idx: usize,
        _name: &str,
        value: &ColumnValue,
    ) -> std::result::Result<bool, GeozeroError> {
        if idx >= self.values.len() {
            return Ok(false);
        }
        self.values[idx] = Some(from_column_value(value));
        Ok(false)
    }
}

/// FGB header の `crs` フィールドを `Crs` に変換する。
///
/// FGB 仕様: `crs.org` が NULL のときは EPSG とみなす。`code == 0` は authority 不在を表す。
fn decode_header_crs(header: &flatgeobuf::Header) -> Option<Crs> {
    let crs = header.crs()?;
    let code = crs.code();
    let wkt = crs.wkt().map(str::to_string);

    let authority = match (crs.org(), code) {
        (Some(org), c) if !org.is_empty() && c != 0 => {
            Some((org.to_string(), u32::try_from(c).unwrap_or(0)))
        }
        (None, c) if c != 0 => Some(("EPSG".to_string(), u32::try_from(c).unwrap_or(0))),
        _ => None,
    };

    if authority.is_none() && wkt.is_none() {
        return None;
    }
    Some(Crs {
        authority,
        wkt,
        wkt_flavor: WktFlavor::V2,
        projjson: None,
    })
}

/// FGB ヘッダ宣言が `DateTime` でも、観測値が `YYYY-MM-DD` のみなら `Date32` に絞る。
fn refine_arrow_types(columns: &[ColumnPlan], rows: &[Row]) -> Vec<DataType> {
    let mut types: Vec<DataType> = columns.iter().map(|c| c.arrow_type.clone()).collect();
    for (col_idx, plan) in columns.iter().enumerate() {
        if !matches!(plan.column_type, ColumnType::DateTime) {
            continue;
        }
        // 全行の observed string が `YYYY-MM-DD`（時刻部なし）なら Date32 とする。
        let mut all_dates_only = true;
        let mut saw_any_value = false;
        for row in rows {
            match &row.values[col_idx] {
                Some(OwnedValue::DateTime(s)) => {
                    saw_any_value = true;
                    if s.contains('T') || s.contains(' ') {
                        all_dates_only = false;
                        break;
                    }
                }
                Some(_) => {
                    all_dates_only = false;
                    break;
                }
                None => {}
            }
        }
        if saw_any_value && all_dates_only {
            types[col_idx] = DataType::Date32;
        }
    }
    types
}

fn build_schema(
    columns: &[ColumnPlan],
    geom_type: GeometryType,
    crs: Option<&Crs>,
) -> Result<SchemaRef> {
    let mut fields: Vec<Arc<Field>> = Vec::with_capacity(columns.len() + 1);
    for c in columns {
        fields.push(Arc::new(Field::new(
            &c.name,
            c.arrow_type.clone(),
            c.nullable,
        )));
    }
    let meta = GeometryMeta::wkb(geom_type, crs.cloned());
    let mut geom_field = Field::new("geometry", DataType::Binary, true);
    let mut metadata = std::collections::HashMap::new();
    metadata.insert(GEOMETRY_META_KEY.to_string(), meta.to_json()?);
    geom_field.set_metadata(metadata);
    fields.push(Arc::new(geom_field));
    Ok(Arc::new(Schema::new(fields)))
}

impl LayerReader for FgbReader {
    fn schema(&self) -> SchemaRef {
        self.schema.clone()
    }

    fn crs(&self) -> Option<&Crs> {
        self.crs.as_ref()
    }

    fn row_count_hint(&self) -> Option<usize> {
        self.row_count_hint
    }

    fn batches(&mut self) -> Box<dyn Iterator<Item = Result<RecordBatch>> + Send + '_> {
        let sample = self.sample.take().unwrap_or_default();
        let feature_iter = self.feature_iter.take();
        Box::new(BatchIter {
            schema: self.schema.clone(),
            columns: self.columns.clone(),
            has_z: self.has_z,
            has_m: self.has_m,
            sample_iter: sample.into_iter(),
            feature_iter,
        })
    }
}

struct BatchIter {
    schema: SchemaRef,
    columns: Vec<ColumnPlan>,
    has_z: bool,
    has_m: bool,
    sample_iter: std::vec::IntoIter<Row>,
    /// `None` = 全 streaming 完了 (sample_iter も使い切った後の終了状態を表す)。
    feature_iter: Option<FeatureIter<BufReader<File>, NotSeekable>>,
}

impl BatchIter {
    fn next_batch(&mut self) -> Result<Option<RecordBatch>> {
        let mut chunk: Vec<Row> = Vec::with_capacity(READ_BATCH_SIZE);

        // sample 残を先に流す。`for` loop は `break` 後 IntoIter を中断状態のまま残す。
        for row in self.sample_iter.by_ref() {
            chunk.push(row);
            if chunk.len() >= READ_BATCH_SIZE {
                break;
            }
        }

        // sample が尽きたら feature_iter から streaming で取り込む。
        // chunk が既に READ_BATCH_SIZE に達していれば下の while は条件不成立で skip される。
        if let Some(iter) = self.feature_iter.as_mut() {
            while chunk.len() < READ_BATCH_SIZE {
                match iter.next() {
                    Ok(None) => {
                        // file 末尾。以降は読まない。
                        self.feature_iter = None;
                        break;
                    }
                    Ok(Some(feat)) => {
                        let row = collect_row(feat, &self.columns, self.has_z, self.has_m)?;
                        chunk.push(row);
                    }
                    Err(e) => {
                        self.feature_iter = None;
                        return Err(driver_err(&e));
                    }
                }
            }
        }

        if chunk.is_empty() {
            return Ok(None);
        }

        let row_count = chunk.len();
        let mut arrays: Vec<ArrayRef> = Vec::with_capacity(self.columns.len() + 1);
        for (col_idx, plan) in self.columns.iter().enumerate() {
            arrays.push(build_array(plan, &chunk, col_idx, row_count)?);
        }

        // geometry 列。
        let mut bb = BinaryBuilder::new();
        for row in &chunk {
            match &row.geometry {
                Some(bytes) => bb.append_value(bytes),
                None => bb.append_null(),
            }
        }
        arrays.push(Arc::new(bb.finish()) as ArrayRef);

        let batch =
            RecordBatch::try_new(self.schema.clone(), arrays).map_err(|e| driver_err(&e))?;
        Ok(Some(batch))
    }
}

impl Iterator for BatchIter {
    type Item = Result<RecordBatch>;

    fn next(&mut self) -> Option<Self::Item> {
        match self.next_batch() {
            Ok(Some(b)) => Some(Ok(b)),
            Ok(None) => None,
            Err(e) => {
                // エラー後はバッファを空にして次回以降 None を返すよう保証する。
                self.feature_iter = None;
                self.sample_iter = Vec::new().into_iter();
                Some(Err(e))
            }
        }
    }
}

#[allow(clippy::too_many_lines)]
fn build_array(plan: &ColumnPlan, rows: &[Row], col_idx: usize, _n: usize) -> Result<ArrayRef> {
    macro_rules! build_primitive {
        ($Builder:ty, $variant:ident) => {{
            let mut b = <$Builder>::new();
            for row in rows {
                match &row.values[col_idx] {
                    Some(OwnedValue::$variant(v)) => b.append_value(*v),
                    None => b.append_null(),
                    Some(other) => {
                        return Err(driver_msg(format!(
                            "type mismatch in column `{}`: expected {}, got {:?}",
                            plan.name,
                            stringify!($variant),
                            other
                        )));
                    }
                }
            }
            Arc::new(b.finish()) as ArrayRef
        }};
    }

    let arr: ArrayRef = match plan.column_type {
        ColumnType::Bool => build_primitive!(BooleanBuilder, Bool),
        ColumnType::Byte => build_primitive!(Int8Builder, Byte),
        ColumnType::UByte => build_primitive!(UInt8Builder, UByte),
        ColumnType::Short => build_primitive!(Int16Builder, Short),
        ColumnType::UShort => build_primitive!(UInt16Builder, UShort),
        ColumnType::Int => build_primitive!(Int32Builder, Int),
        ColumnType::UInt => build_primitive!(UInt32Builder, UInt),
        ColumnType::Long => build_primitive!(Int64Builder, Long),
        ColumnType::ULong => build_primitive!(UInt64Builder, ULong),
        ColumnType::Float => build_primitive!(Float32Builder, Float),
        ColumnType::Double => build_primitive!(Float64Builder, Double),
        ColumnType::String | ColumnType::Json => {
            let mut b = StringBuilder::new();
            for row in rows {
                match &row.values[col_idx] {
                    Some(OwnedValue::String(s)) => b.append_value(s),
                    None => b.append_null(),
                    Some(other) => {
                        return Err(driver_msg(format!(
                            "type mismatch in column `{}`: expected String, got {other:?}",
                            plan.name
                        )));
                    }
                }
            }
            Arc::new(b.finish()) as ArrayRef
        }
        ColumnType::Binary => {
            let mut b = BinaryBuilder::new();
            for row in rows {
                match &row.values[col_idx] {
                    Some(OwnedValue::Binary(v)) => b.append_value(v),
                    None => b.append_null(),
                    Some(other) => {
                        return Err(driver_msg(format!(
                            "type mismatch in column `{}`: expected Binary, got {other:?}",
                            plan.name
                        )));
                    }
                }
            }
            Arc::new(b.finish()) as ArrayRef
        }
        ColumnType::DateTime => match plan.arrow_type {
            DataType::Date32 => {
                let mut b = Date32Builder::new();
                for row in rows {
                    match &row.values[col_idx] {
                        Some(OwnedValue::DateTime(s)) => {
                            b.append_value(parse_date32(s, &plan.name)?);
                        }
                        None => b.append_null(),
                        Some(other) => {
                            return Err(driver_msg(format!(
                                "type mismatch in column `{}`: expected DateTime, got {other:?}",
                                plan.name
                            )));
                        }
                    }
                }
                Arc::new(b.finish()) as ArrayRef
            }
            DataType::Timestamp(_, _) => {
                let mut b = TimestampMicrosecondBuilder::new().with_timezone("UTC");
                for row in rows {
                    match &row.values[col_idx] {
                        Some(OwnedValue::DateTime(s)) => {
                            b.append_value(parse_timestamp_micros(s, &plan.name)?);
                        }
                        None => b.append_null(),
                        Some(other) => {
                            return Err(driver_msg(format!(
                                "type mismatch in column `{}`: expected DateTime, got {other:?}",
                                plan.name
                            )));
                        }
                    }
                }
                Arc::new(b.finish()) as ArrayRef
            }
            _ => unreachable!("DateTime column refined to non-Date/Timestamp arrow type"),
        },
        // 未対応値は文字列扱い（type_map の逆変換が String 化したもの）。`String` 以外は NULL。
        _ => {
            let mut b = StringBuilder::new();
            for row in rows {
                match &row.values[col_idx] {
                    Some(OwnedValue::String(s)) => b.append_value(s),
                    _ => b.append_null(),
                }
            }
            Arc::new(b.finish()) as ArrayRef
        }
    };
    Ok(arr)
}

fn parse_date32(s: &str, field: &str) -> Result<i32> {
    let nd = NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .map_err(|e| driver_msg(format!("invalid Date32 `{s}` in field `{field}`: {e}")))?;
    let epoch = NaiveDate::from_ymd_opt(1970, 1, 1).expect("1970-01-01 valid");
    let days = nd.signed_duration_since(epoch).num_days();
    i32::try_from(days)
        .map_err(|_| driver_msg(format!("Date32 out of range `{s}` in field `{field}`")))
}

/// FGB DateTime（ISO8601 文字列）を Timestamp(Microsecond, UTC) の i64 に変換する。
fn parse_timestamp_micros(s: &str, field: &str) -> Result<i64> {
    // 対応する形式:
    // - "YYYY-MM-DDTHH:MM:SS"
    // - "YYYY-MM-DDTHH:MM:SS.frac"
    // - "YYYY-MM-DDTHH:MM:SSZ"
    // - "YYYY-MM-DDTHH:MM:SS.fracZ"
    // - "YYYY-MM-DDTHH:MM:SS+HH:MM"
    let trimmed = s.trim();
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(trimmed) {
        let micros = dt.timestamp() * 1_000_000 + i64::from(dt.timestamp_subsec_micros());
        return Ok(micros);
    }
    // フォールバック: "Z" や offset 抜きの naive 形式を UTC として扱う。
    let no_z = trimmed.trim_end_matches('Z');
    if let Ok(naive) = chrono::NaiveDateTime::parse_from_str(no_z, "%Y-%m-%dT%H:%M:%S%.f") {
        return Ok(naive.and_utc().timestamp() * 1_000_000
            + i64::from(naive.and_utc().timestamp_subsec_micros()));
    }
    if let Ok(naive) = chrono::NaiveDateTime::parse_from_str(no_z, "%Y-%m-%dT%H:%M:%S") {
        return Ok(naive.and_utc().timestamp() * 1_000_000);
    }
    Err(driver_msg(format!(
        "invalid DateTime `{s}` in field `{field}`"
    )))
}
