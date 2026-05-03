//! CSV/TSV を Arrow `RecordBatch` ストリームとして読み出す。
//!
//! 型推定は行わず、geometry 以外は **すべて `Utf8`** 列として読む。
//! geometry 列は WKT を `shpx-geom::wkt::decode` でパースし、`shpx-geom::wkb::encode`
//! で WKB に詰め直して `Binary` 列に格納する。
//!
//! # v0.8 cycle 1 — ストリーミング化
//!
//! eager-load (`body: Option<String>` で全行を文字列に展開) を廃止し、
//! `csv::Reader<Box<dyn Read + Send>>` を field に保持して逐次読みに置き換えた。
//! 巨大 CSV (10M 行) でも reader 開時のピーク RSS が batch サイズで頭打ちになる。
//!
//! geometry 型 sniff (最初の non-empty WKT 1 件から判定) はファイルを **2 回開く**
//! 2-pass 方式で実装する。1 pass 目で header + 最初の non-empty geometry セルを取り、
//! 2 pass 目で本番の streaming csv::Reader を構築する。CSV は通常そこまで巨大ではなく、
//! 2 度 open する I/O オーバーヘッドは streaming 化の利点 (一定 RSS) に対して許容範囲。
//!
//! UTF-8 以外のエンコーディング (Shift_JIS など) は **暫定で eager に decode** したうえで
//! `Cursor<String>` を `Box<dyn Read + Send>` として渡す (encoding_rs 単体に streaming
//! Read アダプタがないため)。UTF-8 ファイル (実運用の大半) では真の streaming で動く。

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Cursor, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use arrow_array::{
    builder::{BinaryBuilder, StringBuilder},
    ArrayRef, RecordBatch,
};
use arrow_schema::{DataType, Field, Schema, SchemaRef};
use encoding_rs::{Encoding, UTF_8};
use shpx_core::{
    schema::{GeometryMeta, GeometryType, GEOMETRY_META_KEY},
    Crs, Error, LayerReader, ReadOpts, Result, Uri,
};
use shpx_geom::{wkb, wkt};

use crate::options::ResolvedReadOpts;
use crate::util::{driver_err, driver_msg};

/// 1 batch あたりの行数。
const READ_BATCH_SIZE: usize = 65_536;

/// geometry 列名の候補（同定優先順位、case-insensitive）。
const GEOM_NAME_CANDIDATES: &[&str] = &["geometry", "geom", "wkt", "the_geom"];

/// CSV/TSV の `LayerReader` 実装。
pub struct CsvReader {
    schema: SchemaRef,
    crs: Option<Crs>,
    geom_index: usize,
    /// ストリーミング csv::Reader。`open()` で header を消費済みの状態。
    /// `batches()` で `Option::take` して所有権を `BatchIter` に渡す。
    inner: Option<csv::Reader<Box<dyn Read + Send>>>,
}

impl CsvReader {
    pub fn open(uri: &Uri, opts: &ReadOpts) -> Result<Self> {
        let resolved = ResolvedReadOpts::resolve(uri, opts)?;
        let path = PathBuf::from(uri.path());

        // pass 1: header + sniff geometry type。
        let (header_names, geom_index, geom_type) = sniff_pass(&path, &resolved)?;

        let schema = build_schema(
            &header_names,
            geom_index,
            geom_type,
            resolved.src_crs.as_ref(),
        )?;

        // pass 2: 本番 streaming reader。header は has_headers=true なら csv::Reader が
        // 内部で消費するため batches() からは attribute 行のみが見える。
        let inner = open_streaming_reader(&path, &resolved)?;

        Ok(Self {
            schema,
            crs: resolved.src_crs,
            geom_index,
            inner: Some(inner),
        })
    }
}

/// pass 1: ファイルを 1 度開いて header と最初の non-empty geometry セルから geom_type を sniff する。
fn sniff_pass(
    path: &Path,
    resolved: &ResolvedReadOpts,
) -> Result<(Vec<String>, usize, GeometryType)> {
    let r = open_decoded_reader(path, resolved.encoding)?;
    let mut rdr = csv::ReaderBuilder::new()
        .delimiter(resolved.delimiter)
        .has_headers(resolved.has_header)
        .from_reader(r);

    let header_names: Vec<String> = if resolved.has_header {
        rdr.headers()
            .map_err(|e| driver_err(&e))?
            .iter()
            .map(str::to_string)
            .collect()
    } else {
        // ヘッダなし: v0.2 cycle 1 と同じく未対応として弾く。
        return Err(driver_msg(
            "CSV without header is not supported in v0.2 cycle 1 (set SHPX_CSV_HAS_HEADER=true)",
        ));
    };

    let geom_index = pick_geometry_column(&header_names, resolved.geometry_column.as_deref())?;

    // 1 件目の non-empty WKT セルから geometry_type を判定する。
    // 全行は走査しない (混在 CSV では最初の型を採用する近似)。
    let mut geom_type = GeometryType::Geometry;
    for row in rdr.records() {
        let row = row.map_err(|e| driver_err(&e))?;
        if let Some(value) = row.get(geom_index) {
            if !value.is_empty() {
                geom_type = wkt_keyword_to_type(value);
                break;
            }
        }
    }

    Ok((header_names, geom_index, geom_type))
}

/// pass 2: 本番 streaming csv::Reader。`has_headers=true` のとき先頭行は内部で skip される。
fn open_streaming_reader(
    path: &Path,
    resolved: &ResolvedReadOpts,
) -> Result<csv::Reader<Box<dyn Read + Send>>> {
    let r = open_decoded_reader(path, resolved.encoding)?;
    Ok(csv::ReaderBuilder::new()
        .delimiter(resolved.delimiter)
        .has_headers(resolved.has_header)
        .from_reader(r))
}

/// path を開き、必要なら encoding を decode して `Box<dyn Read + Send>` を返す。
///
/// - **UTF-8**: 真の streaming (BufReader<File> + BOM strip)。
/// - **その他**: 全件 read → `encoding_rs` で decode → `Cursor<Vec<u8>>` で wrap (eager)。
///   encoding_rs 単体では Read アダプタが提供されないため。実運用ではほぼ UTF-8 なので
///   この経路に入るのは稀。
fn open_decoded_reader(path: &Path, encoding: &'static Encoding) -> Result<Box<dyn Read + Send>> {
    if encoding == UTF_8 {
        let f = File::open(path).map_err(Error::from)?;
        let mut br = BufReader::new(f);
        // BOM (EF BB BF) を fill_buf + consume(3) で剥がす。seek 不要なので Read の最小要件で動作。
        let buf = br.fill_buf().map_err(Error::from)?;
        if buf.starts_with(b"\xEF\xBB\xBF") {
            br.consume(3);
        }
        Ok(Box::new(br))
    } else {
        let raw = fs::read(path).map_err(Error::from)?;
        let (cow, _, had_errors) = encoding.decode(&raw);
        if had_errors {
            tracing::warn!(
                target: "shpx::csv",
                encoding = encoding.name(),
                "encoding decode produced replacement characters"
            );
        }
        let bytes = cow.into_owned().into_bytes();
        Ok(Box::new(Cursor::new(bytes)))
    }
}

fn pick_geometry_column(header: &[String], explicit: Option<&str>) -> Result<usize> {
    if let Some(name) = explicit {
        return header
            .iter()
            .position(|h| h == name)
            .ok_or_else(|| driver_msg(format!("geometry column `{name}` not found in header")));
    }
    for cand in GEOM_NAME_CANDIDATES {
        if let Some(pos) = header.iter().position(|h| h.eq_ignore_ascii_case(cand)) {
            return Ok(pos);
        }
    }
    Err(Error::Schema(format!(
        "no geometry-like column found in CSV header (expected one of {GEOM_NAME_CANDIDATES:?} or set SHPX_CSV_GEOMETRY_COLUMN)"
    )))
}

fn build_schema(
    header: &[String],
    geom_index: usize,
    geom_type: GeometryType,
    crs: Option<&Crs>,
) -> Result<SchemaRef> {
    let mut fields: Vec<Arc<Field>> = Vec::with_capacity(header.len());
    for (i, name) in header.iter().enumerate() {
        if i == geom_index {
            let meta = GeometryMeta::wkb(geom_type, crs.cloned());
            let mut field = Field::new(name, DataType::Binary, true);
            let mut metadata = HashMap::new();
            metadata.insert(GEOMETRY_META_KEY.to_string(), meta.to_json()?);
            field.set_metadata(metadata);
            fields.push(Arc::new(field));
        } else {
            fields.push(Arc::new(Field::new(name, DataType::Utf8, true)));
        }
    }
    Ok(Arc::new(Schema::new(fields)))
}

fn wkt_keyword_to_type(s: &str) -> GeometryType {
    let trimmed = s.trim_start();
    let upper: String = trimmed
        .bytes()
        .take_while(u8::is_ascii_alphabetic)
        .map(|b| b.to_ascii_uppercase() as char)
        .collect();
    match upper.as_str() {
        "POINT" => GeometryType::Point,
        "LINESTRING" => GeometryType::LineString,
        "POLYGON" => GeometryType::Polygon,
        "MULTIPOINT" => GeometryType::MultiPoint,
        "MULTILINESTRING" => GeometryType::MultiLineString,
        "MULTIPOLYGON" => GeometryType::MultiPolygon,
        _ => GeometryType::Geometry,
    }
}

impl LayerReader for CsvReader {
    fn schema(&self) -> SchemaRef {
        self.schema.clone()
    }

    fn crs(&self) -> Option<&Crs> {
        self.crs.as_ref()
    }

    fn row_count_hint(&self) -> Option<usize> {
        None
    }

    fn batches(&mut self) -> Box<dyn Iterator<Item = Result<RecordBatch>> + Send + '_> {
        let inner = self.inner.take();
        Box::new(BatchIter {
            schema: self.schema.clone(),
            geom_index: self.geom_index,
            inner,
        })
    }
}

struct BatchIter {
    schema: SchemaRef,
    geom_index: usize,
    /// `None` = 全 streaming 完了。エラー後も None にして再呼び出しを停止させる。
    inner: Option<csv::Reader<Box<dyn Read + Send>>>,
}

impl BatchIter {
    fn next_batch(&mut self) -> Result<Option<RecordBatch>> {
        let Some(inner) = self.inner.as_mut() else {
            return Ok(None);
        };
        let n_cols = self.schema.fields().len();
        // geom_index の位置だけ `None` にしておき、attribute 列にだけ StringBuilder を割り当てる。
        // capacity 予約で 65536 行 append 中の realloc を回避する。
        let mut string_builders: Vec<Option<StringBuilder>> = (0..n_cols)
            .map(|i| {
                (i != self.geom_index)
                    .then(|| StringBuilder::with_capacity(READ_BATCH_SIZE, READ_BATCH_SIZE * 16))
            })
            .collect();
        let mut binary_builder = BinaryBuilder::with_capacity(READ_BATCH_SIZE, READ_BATCH_SIZE * 32);
        let mut row_count = 0usize;

        for row in inner.records() {
            let row = row.map_err(|e| driver_err(&e))?;
            if row.len() != n_cols {
                return Err(driver_msg(format!(
                    "row has {} columns, expected {}",
                    row.len(),
                    n_cols
                )));
            }
            for (i, value) in row.iter().enumerate() {
                if i == self.geom_index {
                    if value.is_empty() {
                        binary_builder.append_null();
                    } else {
                        let g = wkt::decode(value)?;
                        let bytes = wkb::encode(&g)?;
                        binary_builder.append_value(&bytes);
                    }
                } else {
                    string_builders[i]
                        .as_mut()
                        .expect("non-geom slot has Some(builder)")
                        .append_value(value);
                }
            }
            row_count += 1;
            if row_count >= READ_BATCH_SIZE {
                break;
            }
        }

        if row_count == 0 {
            return Ok(None);
        }

        let mut columns: Vec<ArrayRef> = Vec::with_capacity(n_cols);
        for (i, sb) in string_builders.into_iter().enumerate() {
            if i == self.geom_index {
                columns.push(Arc::new(binary_builder.finish()));
            } else {
                columns.push(Arc::new(
                    sb.expect("non-geom slot has Some(builder)").finish(),
                ));
            }
        }

        let batch =
            RecordBatch::try_new(self.schema.clone(), columns).map_err(|e| driver_err(&e))?;
        Ok(Some(batch))
    }
}

impl Iterator for BatchIter {
    type Item = Result<RecordBatch>;

    fn next(&mut self) -> Option<Self::Item> {
        match self.next_batch() {
            Ok(Some(batch)) => Some(Ok(batch)),
            Ok(None) => {
                self.inner = None;
                None
            }
            Err(e) => {
                self.inner = None;
                Some(Err(e))
            }
        }
    }
}
