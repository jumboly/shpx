//! CSV/TSV を Arrow `RecordBatch` ストリームとして読み出す。
//!
//! 型推定は行わず、geometry 以外は **すべて `Utf8`** 列として読む。
//! geometry 列は WKT を `shpx-geom::wkt::decode` でパースし、`shpx-geom::wkb::encode`
//! で WKB に詰め直して `Binary` 列に格納する。

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufReader, Read};
use std::path::PathBuf;
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

/// 1 batch あたりの行数（hint）。CSV は per-row パース負荷が小さいので大きめに 4096。
const READ_BATCH_SIZE: usize = 4096;

/// geometry 列名の候補（同定優先順位、case-insensitive）。
const GEOM_NAME_CANDIDATES: &[&str] = &["geometry", "geom", "wkt", "the_geom"];

/// CSV/TSV の `LayerReader` 実装。
pub struct CsvReader {
    schema: SchemaRef,
    crs: Option<Crs>,
    geom_index: usize,
    /// 入力データ（UTF-8 復号済み）。`batches()` で 1 度だけ取り出す。
    body: Option<String>,
    delimiter: u8,
    has_header: bool,
}

impl CsvReader {
    pub fn open(uri: &Uri, opts: &ReadOpts) -> Result<Self> {
        let resolved = ResolvedReadOpts::resolve(uri, opts)?;
        let path = PathBuf::from(uri.path());

        // UTF-8 はストリーム前提でも 1 度全読込で良い（CSV のサイズ感に対しては許容）。
        // ただし非 UTF-8 はそもそも全読込してから encoding_rs で復号する必要があるため、
        // どちらの経路でも `String` として保持してから `csv::Reader` に流す。
        let body = read_decoded(&path, resolved.encoding)?;

        // ヘッダ行 + 残データから schema を作る。
        let mut rdr = csv::ReaderBuilder::new()
            .delimiter(resolved.delimiter)
            .has_headers(resolved.has_header)
            .from_reader(body.as_bytes());

        let header_names: Vec<String> = if resolved.has_header {
            rdr.headers()
                .map_err(|e| driver_err(&e))?
                .iter()
                .map(str::to_string)
                .collect()
        } else {
            // ヘッダなし: 1 行目の列数から `c0..cN` を仮割当。
            // v0.2 サイクル 1 では未対応として弾く（`docs/CSV.md` に明記）。
            return Err(driver_msg(
                "CSV without header is not supported in v0.2 cycle 1 (set SHPX_CSV_HAS_HEADER=true)",
            ));
        };

        let geom_index = pick_geometry_column(&header_names, resolved.geometry_column.as_deref())?;

        // 1 件目の non-empty WKT をスニフして geometry_type を決める。
        // SHP writer のように単一型を要求する下流に変換する場合、`Geometry` は受け付けられない。
        // 全行スキャンはしない（同じ列に複数型が混在する場合は最初の型を採用する近似）。
        let geom_type =
            sniff_geometry_type(&body, geom_index, resolved.delimiter, resolved.has_header)?;

        let schema = build_schema(
            &header_names,
            geom_index,
            geom_type,
            resolved.src_crs.as_ref(),
        )?;

        Ok(Self {
            schema,
            crs: resolved.src_crs,
            geom_index,
            body: Some(body),
            delimiter: resolved.delimiter,
            has_header: resolved.has_header,
        })
    }
}

fn read_decoded(path: &PathBuf, encoding: &'static Encoding) -> Result<String> {
    if encoding == UTF_8 {
        // ストリームでも良いが API シンプル化のため文字列で持つ。BOM は剥がす。
        let mut s = String::new();
        let mut f = BufReader::new(File::open(path).map_err(Error::from)?);
        f.read_to_string(&mut s).map_err(Error::from)?;
        if let Some(stripped) = s.strip_prefix('\u{feff}') {
            return Ok(stripped.to_string());
        }
        Ok(s)
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
        Ok(cow.into_owned())
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

/// 最初の non-empty geometry セルから WKT keyword を抽出して [`GeometryType`] を決める。
///
/// 全件確定型ではなく「最初に出てきた型を採用」する近似スニフ。SHP のように単一型を要求する
/// 下流向けに型注釈を付ける目的で、混在 CSV では型不一致のエラーが SHP 側で出る想定。
fn sniff_geometry_type(
    body: &str,
    geom_index: usize,
    delimiter: u8,
    has_header: bool,
) -> Result<GeometryType> {
    let mut rdr = csv::ReaderBuilder::new()
        .delimiter(delimiter)
        .has_headers(has_header)
        .from_reader(body.as_bytes());
    for row in rdr.records() {
        let row = row.map_err(|e| driver_err(&e))?;
        if let Some(value) = row.get(geom_index) {
            if !value.is_empty() {
                return Ok(wkt_keyword_to_type(value));
            }
        }
    }
    Ok(GeometryType::Geometry)
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
        let body = self.body.take();
        Box::new(BatchIter {
            schema: self.schema.clone(),
            geom_index: self.geom_index,
            inner: body.map(|b| {
                csv::ReaderBuilder::new()
                    .delimiter(self.delimiter)
                    .has_headers(self.has_header)
                    .from_reader(std::io::Cursor::new(b.into_bytes()))
            }),
            done: false,
        })
    }
}

type BodyReader = csv::Reader<std::io::Cursor<Vec<u8>>>;

struct BatchIter {
    schema: SchemaRef,
    geom_index: usize,
    inner: Option<BodyReader>,
    done: bool,
}

impl BatchIter {
    fn next_batch(&mut self) -> Result<Option<RecordBatch>> {
        let Some(inner) = self.inner.as_mut() else {
            return Ok(None);
        };
        let n_cols = self.schema.fields().len();
        // geom_index の位置だけ `None` にしておき、attribute 列にだけ StringBuilder を割り当てる。
        let mut string_builders: Vec<Option<StringBuilder>> = (0..n_cols)
            .map(|i| (i != self.geom_index).then(StringBuilder::new))
            .collect();
        let mut binary_builder = BinaryBuilder::new();
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
        if self.done {
            return None;
        }
        match self.next_batch() {
            Ok(Some(batch)) => Some(Ok(batch)),
            Ok(None) => {
                self.done = true;
                None
            }
            Err(e) => {
                self.done = true;
                Some(Err(e))
            }
        }
    }
}
