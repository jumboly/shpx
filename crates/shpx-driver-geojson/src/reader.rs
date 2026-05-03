//! GeoJSON / GeoJSONL を Arrow `RecordBatch` ストリームとして読み出す。
//!
//! v0.8 cycle 3 で eager-load (`Vec<Feature>`) を撤廃した。`open()` ではファイル head を
//! 軽量プローブして `crs` メンバ抽出と先頭 N=1024 feature の型推論サンプリングを行い、
//! その後ファイルを開き直して features 配列を真にストリーミングで列挙する。
//!
//! - FeatureCollection: `geojson::FeatureReader::from_reader(R).features()` を使う
//!   (`crates/shpx-driver-geojson/src/stream.rs::open_feature_collection`)。
//! - NDJSON: `BufRead::lines()` ベースで空行 / `#` コメント行をスキップしつつ
//!   1 行 1 Feature をパース (`stream.rs::open_ndjson`)。
//!
//! 属性 (properties) の Arrow 型はサンプル N 件だけスニフして決める。ファイルがそれを
//! 超える行を含む場合、本番ストリームで型不一致が見つかったら以下のように振る:
//! - 数値 / Bool / null は append_value で値変換できるので問題なし。
//! - String 列に Number / Bool / Object / Array が来た場合は `JsonValue::to_string()` で
//!   文字列化して詰める (既存の Utf8 demote ロジックと同じ)。
//! - 整数列に Float が来た場合のみ精度が落ちる。サンプル数を `SHPX_GEOJSON_INFER_SAMPLE`
//!   env で増やして対処する想定。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use arrow_array::{
    builder::{BinaryBuilder, BooleanBuilder, Float64Builder, Int64Builder, StringBuilder},
    ArrayRef, RecordBatch,
};
use arrow_schema::{DataType, Field, Schema, SchemaRef};
use geojson::{Feature, Value as GjValue};
use serde_json::Value as JsonValue;
use shpx_core::{
    schema::{GeometryMeta, GeometryType, GEOMETRY_META_KEY},
    Crs, Error, LayerReader, ReadOpts, Result, Uri,
};
use shpx_geom::wkb;

use crate::geom_convert::geometry_to_geom;
use crate::options::{OutputFormat, ResolvedReadOpts};
use crate::stream::{self, FeatureStream};
use crate::util::{driver_err, driver_msg};

/// Geometry 列名（出力時固定。RFC 7946 Feature の `geometry` フィールドに対応）。
pub const GEOM_COLUMN_NAME: &str = "geometry";

/// 1 batch あたりの行数。
const READ_BATCH_SIZE: usize = 4096;

/// 型推論用にサンプリングする feature 件数の既定値。
const DEFAULT_INFER_SAMPLE: usize = 1024;

/// 環境変数: 型推論サンプル数 override (`0` で無効化、未指定で `DEFAULT_INFER_SAMPLE`)。
pub const ENV_INFER_SAMPLE: &str = "SHPX_GEOJSON_INFER_SAMPLE";

/// GeoJSON / GeoJSONL の `LayerReader` 実装。
pub struct GeoJsonReader {
    schema: SchemaRef,
    crs: Option<Crs>,
    /// Properties 列の出力スキーマ順 + その Arrow 型。
    columns: Vec<ColumnPlan>,
    /// 真のストリーミング iterator (FeatureCollection / NDJSON 共通)。
    stream: Option<FeatureStream>,
}

#[derive(Debug, Clone)]
struct ColumnPlan {
    name: String,
    data_type: DataType,
}

impl GeoJsonReader {
    pub fn open(uri: &Uri, opts: &ReadOpts) -> Result<Self> {
        let resolved = ResolvedReadOpts::resolve(uri, opts)?;
        let path = PathBuf::from(uri.path());

        // Pass 1: head を probe して top-level CRS を取り出す。
        // GeoJSONL (NDJSON) は仕様上 root レベル CRS を持たない。
        let crs_from_doc = match resolved.format {
            OutputFormat::FeatureCollection => {
                let v = stream::extract_top_level_crs_value(&path)?;
                v.as_ref().map(parse_crs_member).transpose()?.flatten()
            }
            OutputFormat::Lines => None,
        };

        // ReadOpts.src_crs が指定されていればそれを優先する（CSV と同じ慣習）。
        // 未指定なら top-level `crs` メンバ → RFC 7946 既定 (EPSG:4326) の順で補完。
        let crs = resolved
            .src_crs
            .or(crs_from_doc)
            .or_else(|| Some(Crs::from_epsg(4326)));

        // Pass 2: 先頭 N feature をサンプリングして型推論。
        let sample_limit = resolve_infer_sample()?;
        let mut sample_iter = open_stream(resolved.format, &path)?;
        let mut sample: Vec<Feature> = Vec::with_capacity(sample_limit.min(1024));
        let mut sample_err: Option<Error> = None;
        for _ in 0..sample_limit {
            match sample_iter.next() {
                Some(Ok(f)) => sample.push(f),
                Some(Err(e)) => {
                    sample_err = Some(e);
                    break;
                }
                None => break,
            }
        }
        // sample_iter は drop して file を閉じる (Pass 3 で再 open する)。
        drop(sample_iter);
        if let Some(e) = sample_err {
            return Err(e);
        }

        let columns = infer_columns(&sample);
        let geom_type = unify_geometry_type(&sample)?;
        let schema = build_schema(&columns, geom_type, crs.as_ref())?;

        // Pass 3: 本番ストリーム。サンプル取得分も含めて先頭から再列挙する。
        let stream = open_stream(resolved.format, &path)?;

        Ok(Self {
            schema,
            crs,
            columns,
            stream: Some(stream),
        })
    }
}

fn open_stream(format: OutputFormat, path: &std::path::Path) -> Result<FeatureStream> {
    match format {
        OutputFormat::FeatureCollection => stream::open_feature_collection(path),
        OutputFormat::Lines => stream::open_ndjson(path),
    }
}

fn resolve_infer_sample() -> Result<usize> {
    match std::env::var(ENV_INFER_SAMPLE) {
        Ok(raw) => raw.parse::<usize>().map_err(|_| {
            driver_msg(format!(
                "{ENV_INFER_SAMPLE}: invalid value `{raw}` (expected non-negative integer)"
            ))
        }),
        Err(_) => Ok(DEFAULT_INFER_SAMPLE),
    }
}

/// 旧仕様の top-level `crs` メンバを `Crs` に解釈する。
///
/// 認識する形:
/// - `{"type":"name","properties":{"name":"urn:ogc:def:crs:EPSG::4326"}}`
/// - `{"type":"name","properties":{"name":"EPSG:4326"}}`
/// - `{"type":"name","properties":{"name":"urn:ogc:def:crs:OGC:1.3:CRS84"}}`
///
/// 解釈不能な値は [`Error::Crs`] で拒否する（無音で誤った CRS を仮定するより安全）。
fn parse_crs_member(value: &JsonValue) -> Result<Option<Crs>> {
    // null は CRS 無し扱い（RFC 7946 で `"crs": null` を書く実装が存在）。
    if value.is_null() {
        return Ok(None);
    }
    let obj = value
        .as_object()
        .ok_or_else(|| Error::Crs("GeoJSON `crs` member must be an object or null".to_string()))?;
    let crs_type = obj
        .get("type")
        .and_then(JsonValue::as_str)
        .ok_or_else(|| Error::Crs("GeoJSON `crs.type` is missing or not a string".to_string()))?;
    if crs_type != "name" {
        return Err(Error::Crs(format!(
            "unsupported GeoJSON crs type `{crs_type}` (only `name` is supported)"
        )));
    }
    let name = obj
        .get("properties")
        .and_then(JsonValue::as_object)
        .and_then(|p| p.get("name"))
        .and_then(JsonValue::as_str)
        .ok_or_else(|| {
            Error::Crs("GeoJSON `crs.properties.name` is missing or not a string".to_string())
        })?;
    parse_crs_name(name).map(Some)
}

/// `EPSG:4326` / `urn:ogc:def:crs:EPSG::4326` / `urn:ogc:def:crs:OGC:1.3:CRS84` 等を解釈する。
fn parse_crs_name(name: &str) -> Result<Crs> {
    let trimmed = name.trim();
    // 直接 EPSG:NNNN 形式
    if let Some(c) = Crs::parse_epsg(trimmed) {
        return Ok(c);
    }
    // OGC URN の特例
    if trimmed.eq_ignore_ascii_case("urn:ogc:def:crs:OGC:1.3:CRS84")
        || trimmed.eq_ignore_ascii_case("urn:ogc:def:crs:OGC::CRS84")
    {
        return Ok(Crs::from_epsg(4326));
    }
    // `urn:ogc:def:crs:EPSG::NNNN` / `urn:ogc:def:crs:EPSG:9.x:NNNN`
    if let Some(rest) = trimmed
        .strip_prefix("urn:ogc:def:crs:EPSG:")
        .or_else(|| trimmed.strip_prefix("urn:ogc:def:crs:epsg:"))
    {
        // `::NNNN` または `9.x:NNNN`。最後の `:` 以降の数値を取る。
        let last = rest.rsplit(':').next().unwrap_or("");
        if let Ok(code) = last.parse::<u32>() {
            return Ok(Crs::from_epsg(code));
        }
    }
    Err(Error::Crs(format!(
        "unsupported GeoJSON crs name `{name}` (expected EPSG:NNNN or urn:ogc:def:crs:EPSG::NNNN or urn:ogc:def:crs:OGC:1.3:CRS84)"
    )))
}

/// サンプル feature の properties をスニフして列計画を作る。
///
/// 列順は最初の出現順（後続 Feature で初登場するキーは末尾に append）。
fn infer_columns(features: &[Feature]) -> Vec<ColumnPlan> {
    let mut order: Vec<String> = Vec::new();
    let mut state: HashMap<String, ColumnState> = HashMap::new();

    for f in features {
        let Some(props) = &f.properties else { continue };
        for (k, v) in props {
            let entry = state.entry(k.clone());
            if matches!(entry, std::collections::hash_map::Entry::Vacant(_)) {
                order.push(k.clone());
            }
            entry.or_default().update(v, k);
        }
    }

    order
        .into_iter()
        .map(|name| {
            let st = state.remove(&name).unwrap_or_default();
            ColumnPlan {
                data_type: st.into_arrow_type(),
                name,
            }
        })
        .collect()
}

#[derive(Debug, Default, Clone)]
struct ColumnState {
    /// 累積した代表型。
    inferred: Option<Inferred>,
    /// 配列 / オブジェクトが出現した（→ Utf8 降格 + 警告）。
    saw_structured: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Inferred {
    Bool,
    Int,
    Float,
    String,
    /// 異種混在。最終的に Utf8 になる。
    Mixed,
}

impl ColumnState {
    fn update(&mut self, v: &JsonValue, name: &str) {
        match v {
            JsonValue::Null => {}
            JsonValue::Bool(_) => {
                self.inferred = Some(merge(self.inferred, Inferred::Bool));
            }
            JsonValue::Number(n) => {
                let t = if n.is_i64() || n.as_u64().is_some_and(|v| i64::try_from(v).is_ok()) {
                    Inferred::Int
                } else {
                    Inferred::Float
                };
                self.inferred = Some(merge(self.inferred, t));
            }
            JsonValue::String(_) => {
                self.inferred = Some(merge(self.inferred, Inferred::String));
            }
            JsonValue::Array(_) | JsonValue::Object(_) => {
                if !self.saw_structured {
                    tracing::warn!(
                        target: "shpx::geojson",
                        kind = "structured-property",
                        field = name,
                        "GeoJSON property contains array/object; column demoted to Utf8"
                    );
                }
                self.saw_structured = true;
                self.inferred = Some(Inferred::String);
            }
        }
    }

    fn into_arrow_type(self) -> DataType {
        if self.saw_structured {
            return DataType::Utf8;
        }
        match self.inferred {
            Some(Inferred::Bool) => DataType::Boolean,
            Some(Inferred::Int) => DataType::Int64,
            Some(Inferred::Float) => DataType::Float64,
            None | Some(Inferred::String | Inferred::Mixed) => DataType::Utf8,
        }
    }
}

/// `Inferred` 同士のマージ規則。
fn merge(prev: Option<Inferred>, new: Inferred) -> Inferred {
    match prev {
        None => new,
        Some(p) if p == new => p,
        Some(Inferred::Int) if new == Inferred::Float => Inferred::Float,
        Some(Inferred::Float) if new == Inferred::Int => Inferred::Float,
        Some(_) => Inferred::Mixed,
    }
}

/// サンプル feature の geometry 型を集約する。
fn unify_geometry_type(features: &[Feature]) -> Result<GeometryType> {
    let mut found: Option<GeometryType> = None;
    for f in features {
        let Some(geom) = &f.geometry else { continue };
        let ty = match &geom.value {
            GjValue::Point(_) => GeometryType::Point,
            GjValue::LineString(_) => GeometryType::LineString,
            GjValue::Polygon(_) => GeometryType::Polygon,
            GjValue::MultiPoint(_) => GeometryType::MultiPoint,
            GjValue::MultiLineString(_) => GeometryType::MultiLineString,
            GjValue::MultiPolygon(_) => GeometryType::MultiPolygon,
            GjValue::GeometryCollection(_) => {
                return Err(Error::Geometry(
                    "GeometryCollection is not supported".into(),
                ));
            }
        };
        match found {
            None => found = Some(ty),
            Some(prev) if prev == ty => {}
            // 混在。Geometry に格上げして全行受けられるようにする。
            Some(_) => found = Some(GeometryType::Geometry),
        }
    }
    Ok(found.unwrap_or(GeometryType::Geometry))
}

fn build_schema(
    columns: &[ColumnPlan],
    geom_type: GeometryType,
    crs: Option<&Crs>,
) -> Result<SchemaRef> {
    let mut fields: Vec<Arc<Field>> = Vec::with_capacity(columns.len() + 1);
    for c in columns {
        // 型推論段階で nullable 判定の根拠を持たないため、属性列は常に nullable=true で安全側に倒す。
        fields.push(Arc::new(Field::new(&c.name, c.data_type.clone(), true)));
    }
    let meta = GeometryMeta::wkb(geom_type, crs.cloned());
    let mut geom_field = Field::new(GEOM_COLUMN_NAME, DataType::Binary, true);
    let mut metadata = HashMap::new();
    metadata.insert(GEOMETRY_META_KEY.to_string(), meta.to_json()?);
    geom_field.set_metadata(metadata);
    fields.push(Arc::new(geom_field));
    Ok(Arc::new(Schema::new(fields)))
}

impl LayerReader for GeoJsonReader {
    fn schema(&self) -> SchemaRef {
        self.schema.clone()
    }

    fn crs(&self) -> Option<&Crs> {
        self.crs.as_ref()
    }

    fn row_count_hint(&self) -> Option<usize> {
        // streaming のため事前に行数は分からない。
        None
    }

    fn batches(&mut self) -> Box<dyn Iterator<Item = Result<RecordBatch>> + Send + '_> {
        let stream = self.stream.take();
        Box::new(BatchIter {
            schema: self.schema.clone(),
            columns: self.columns.clone(),
            stream,
            done: false,
        })
    }
}

struct BatchIter {
    schema: SchemaRef,
    columns: Vec<ColumnPlan>,
    stream: Option<FeatureStream>,
    done: bool,
}

impl BatchIter {
    fn next_batch(&mut self) -> Result<Option<RecordBatch>> {
        let Some(stream) = self.stream.as_mut() else {
            return Ok(None);
        };
        let mut builders = ColumnBuilders::new(&self.columns);
        let mut row_count = 0usize;

        for _ in 0..READ_BATCH_SIZE {
            match stream.next() {
                Some(Ok(feature)) => {
                    builders.append_row(&feature, &self.columns)?;
                    row_count += 1;
                }
                Some(Err(e)) => return Err(e),
                None => break,
            }
        }

        if row_count == 0 {
            return Ok(None);
        }

        let arrays = builders.finish();
        let batch =
            RecordBatch::try_new(self.schema.clone(), arrays).map_err(|e| driver_err(&e))?;
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
            Ok(Some(b)) => Some(Ok(b)),
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

/// 列ビルダー集合。型ごとに用意しておき、行ごとに append する。
enum ColumnBuilder {
    Bool(BooleanBuilder),
    Int(Int64Builder),
    Float(Float64Builder),
    String(StringBuilder),
}

struct ColumnBuilders {
    attrs: Vec<ColumnBuilder>,
    geom: BinaryBuilder,
}

impl ColumnBuilders {
    fn new(columns: &[ColumnPlan]) -> Self {
        let attrs = columns
            .iter()
            .map(|c| match c.data_type {
                DataType::Boolean => ColumnBuilder::Bool(BooleanBuilder::new()),
                DataType::Int64 => ColumnBuilder::Int(Int64Builder::new()),
                DataType::Float64 => ColumnBuilder::Float(Float64Builder::new()),
                DataType::Utf8 => ColumnBuilder::String(StringBuilder::new()),
                ref other => {
                    unreachable!("infer_columns must yield only supported types: {other:?}")
                }
            })
            .collect();
        Self {
            attrs,
            geom: BinaryBuilder::new(),
        }
    }

    fn append_row(&mut self, feature: &Feature, columns: &[ColumnPlan]) -> Result<()> {
        let props = feature.properties.as_ref();
        for (i, col) in columns.iter().enumerate() {
            let v = props.and_then(|p| p.get(&col.name));
            append_value(&mut self.attrs[i], &col.data_type, v, &col.name)?;
        }
        match &feature.geometry {
            None => self.geom.append_null(),
            Some(g) => {
                let geom = geometry_to_geom(g)?;
                let bytes = wkb::encode(&geom)?;
                self.geom.append_value(&bytes);
            }
        }
        Ok(())
    }

    fn finish(mut self) -> Vec<ArrayRef> {
        let mut out: Vec<ArrayRef> = self
            .attrs
            .into_iter()
            .map(|b| match b {
                ColumnBuilder::Bool(mut b) => Arc::new(b.finish()) as ArrayRef,
                ColumnBuilder::Int(mut b) => Arc::new(b.finish()) as ArrayRef,
                ColumnBuilder::Float(mut b) => Arc::new(b.finish()) as ArrayRef,
                ColumnBuilder::String(mut b) => Arc::new(b.finish()) as ArrayRef,
            })
            .collect();
        out.push(Arc::new(self.geom.finish()) as ArrayRef);
        out
    }
}

/// 1 セル分の値を builder に append する。
///
/// JSON 値が列の Arrow 型と一致しない場合、可能なら型変換を試み、
/// それでも合わなければ `null` を append する（型混在は infer 段階で `Utf8` になっている前提）。
fn append_value(
    builder: &mut ColumnBuilder,
    data_type: &DataType,
    value: Option<&JsonValue>,
    field: &str,
) -> Result<()> {
    let Some(v) = value else {
        // properties に該当キーが無い → null として扱う
        append_null(builder);
        return Ok(());
    };
    if v.is_null() {
        append_null(builder);
        return Ok(());
    }
    match (builder, data_type) {
        (ColumnBuilder::Bool(b), DataType::Boolean) => match v.as_bool() {
            Some(x) => b.append_value(x),
            None => b.append_null(),
        },
        (ColumnBuilder::Int(b), DataType::Int64) => match v.as_i64() {
            Some(x) => b.append_value(x),
            None => b.append_null(),
        },
        (ColumnBuilder::Float(b), DataType::Float64) => match v.as_f64() {
            Some(x) => b.append_value(x),
            None => b.append_null(),
        },
        (ColumnBuilder::String(b), DataType::Utf8) => {
            // 異種混在 → 文字列化して保持。number/bool は JSON 表現で詰める。
            let s = match v {
                JsonValue::String(s) => s.clone(),
                other => other.to_string(),
            };
            b.append_value(&s);
        }
        // build_schema が data_type と builder の組を一致させる契約のためここには来ない。
        _ => {
            return Err(driver_msg(format!(
                "internal: builder/data_type mismatch for field `{field}`"
            )));
        }
    }
    Ok(())
}

fn append_null(builder: &mut ColumnBuilder) {
    match builder {
        ColumnBuilder::Bool(b) => b.append_null(),
        ColumnBuilder::Int(b) => b.append_null(),
        ColumnBuilder::Float(b) => b.append_null(),
        ColumnBuilder::String(b) => b.append_null(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_crs_name_handles_epsg_forms() {
        assert_eq!(parse_crs_name("EPSG:4326").unwrap().epsg_code(), Some(4326));
        assert_eq!(
            parse_crs_name("urn:ogc:def:crs:EPSG::3857")
                .unwrap()
                .epsg_code(),
            Some(3857)
        );
        assert_eq!(
            parse_crs_name("urn:ogc:def:crs:EPSG:9.1:4326")
                .unwrap()
                .epsg_code(),
            Some(4326)
        );
        assert_eq!(
            parse_crs_name("urn:ogc:def:crs:OGC:1.3:CRS84")
                .unwrap()
                .epsg_code(),
            Some(4326)
        );
    }

    #[test]
    fn parse_crs_name_rejects_unknown() {
        assert!(parse_crs_name("ESRI:102100").is_err());
    }

    #[test]
    fn merge_int_and_float_promotes() {
        assert_eq!(merge(Some(Inferred::Int), Inferred::Float), Inferred::Float);
        assert_eq!(merge(Some(Inferred::Float), Inferred::Int), Inferred::Float);
    }

    #[test]
    fn merge_int_and_string_falls_back_to_mixed() {
        assert_eq!(
            merge(Some(Inferred::Int), Inferred::String),
            Inferred::Mixed
        );
    }
}
