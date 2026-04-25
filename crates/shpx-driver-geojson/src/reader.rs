//! GeoJSON / GeoJSONL を Arrow `RecordBatch` ストリームとして読み出す。
//!
//! v0.2 サイクル 2 では FeatureCollection / 単発 Feature / GeoJSONL のいずれでも
//! 全 Feature を一度メモリにロードしてから `batches()` で 4096 件ずつ流す
//! （巨大ファイルの streaming 読みは v0.3 の Future work）。
//!
//! 属性 (properties) の Arrow 型は **全 Feature を 1 回スニフ** して決める。
//! 同列に異なる型が混在する場合の昇格規則は以下:
//!
//! - `Int` ↔ `Int` → `Int64`
//! - `Int` ↔ `Float`（順不問）→ `Float64`
//! - `Bool` ↔ `Bool` → `Boolean`
//! - `String` ↔ `String` → `Utf8`
//! - 異種混在（例: `Int` と `String`）→ `Utf8`（値は `Value::to_string()` で詰める）
//! - 配列・オブジェクト出現 → `Utf8` へ降格 + `tracing::warn!` を 1 度だけ出す

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use arrow_array::{
    builder::{BinaryBuilder, BooleanBuilder, Float64Builder, Int64Builder, StringBuilder},
    ArrayRef, RecordBatch,
};
use arrow_schema::{DataType, Field, Schema, SchemaRef};
use geojson::{Feature, GeoJson, Value as GjValue};
use serde_json::Value as JsonValue;
use shpx_core::{
    schema::{GeometryMeta, GeometryType, GEOMETRY_META_KEY},
    Crs, Error, LayerReader, ReadOpts, Result, Uri,
};
use shpx_geom::wkb;

use crate::geom_convert::geometry_to_geom;
use crate::options::{OutputFormat, ResolvedReadOpts};
use crate::util::{driver_err, driver_msg};

/// Geometry 列名（出力時固定。RFC 7946 Feature の `geometry` フィールドに対応）。
pub const GEOM_COLUMN_NAME: &str = "geometry";

/// 1 batch あたりの行数。FeatureCollection / GeoJSONL いずれでもメモリ上に
/// `Vec<Feature>` を持つため、CSV と同等の 4096 にしておく。
const READ_BATCH_SIZE: usize = 4096;

/// GeoJSON / GeoJSONL の `LayerReader` 実装。
pub struct GeoJsonReader {
    schema: SchemaRef,
    crs: Option<Crs>,
    /// Properties 列の出力スキーマ順 + その Arrow 型。最終 column index は features.len()。
    columns: Vec<ColumnPlan>,
    features: Vec<Feature>,
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
        let body = read_text(&path)?;

        let (features, crs_from_doc) = match resolved.format {
            OutputFormat::FeatureCollection => parse_feature_collection(&body)?,
            // GeoJSONL は 1 行 1 Feature の NDJSON。top-level に CRS の概念は無い。
            OutputFormat::Lines => (parse_geojson_lines(&body)?, None),
        };

        // ReadOpts.src_crs が指定されていればそれを優先する（CSV と同じ慣習）。
        // 未指定なら top-level `crs` メンバ → RFC 7946 既定 (EPSG:4326) の順で補完。
        let crs = resolved
            .src_crs
            .or(crs_from_doc)
            .or_else(|| Some(Crs::from_epsg(4326)));

        let columns = infer_columns(&features);
        let geom_type = unify_geometry_type(&features)?;
        let schema = build_schema(&columns, geom_type, crs.as_ref())?;

        Ok(Self {
            schema,
            crs,
            columns,
            features,
        })
    }
}

/// ファイルを UTF-8 で読み、先頭 BOM を寛容に剥がす。
fn read_text(path: &PathBuf) -> Result<String> {
    let raw = fs::read_to_string(path).map_err(Error::from)?;
    if let Some(stripped) = raw.strip_prefix('\u{feff}') {
        Ok(stripped.to_string())
    } else {
        Ok(raw)
    }
}

/// FeatureCollection 全体を `Vec<Feature>` と top-level CRS にパースする。
/// 単発 `Feature` も許容（1 件入りの Vec として返す）。
fn parse_feature_collection(body: &str) -> Result<(Vec<Feature>, Option<Crs>)> {
    let value: JsonValue = serde_json::from_str(body).map_err(|e| driver_err(&e))?;

    // 先に top-level `crs` メンバを抜く。FeatureCollection / Feature どちらにも付与されている場合がある。
    let crs = value
        .as_object()
        .and_then(|o| o.get("crs"))
        .map(parse_crs_member)
        .transpose()?
        .flatten();

    let gj: GeoJson = serde_json::from_value(value).map_err(|e| driver_err(&e))?;
    let features = match gj {
        GeoJson::FeatureCollection(fc) => fc.features,
        GeoJson::Feature(f) => vec![f],
        GeoJson::Geometry(_) => {
            return Err(driver_msg(
                "top-level Geometry is not supported (expected FeatureCollection or Feature)",
            ));
        }
    };
    Ok((features, crs))
}

/// GeoJSON Lines (NDJSON) を `Vec<Feature>` にパースする。
///
/// - 空行 (whitespace のみ) は skip
/// - `#` で始まる行は skip（NDJSON 規格上は不要だが、コメント行を入れる実装が現実に存在するため寛容に）
/// - 1 行 = 1 Feature を要求する。`FeatureCollection` 行を含むのは仕様外として拒否する
fn parse_geojson_lines(body: &str) -> Result<Vec<Feature>> {
    let mut features = Vec::new();
    for (lineno, raw) in body.lines().enumerate() {
        let trimmed = raw.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let f: Feature = serde_json::from_str(trimmed).map_err(|e| {
            driver_msg(format!(
                "GeoJSONL line {}: {e}",
                // 0-indexed → 人間向けに 1-indexed
                lineno + 1
            ))
        })?;
        features.push(f);
    }
    Ok(features)
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

/// 全 Feature の properties をスニフして列計画を作る。
///
/// 列順は **最初の出現順**（後続 Feature で初登場するキーは末尾に append）。
/// すべて null / 配列 / オブジェクトのみだった列は `Utf8` に降格する。
fn infer_columns(features: &[Feature]) -> Vec<ColumnPlan> {
    use std::collections::BTreeSet;
    // 出現順（重複を弾くため BTreeSet で seen 管理しつつ Vec に push）。
    let mut order: Vec<String> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut state: HashMap<String, ColumnState> = HashMap::new();

    for f in features {
        let Some(props) = &f.properties else { continue };
        for (k, v) in props {
            if !seen.contains(k) {
                seen.insert(k.clone());
                order.push(k.clone());
            }
            let st = state.entry(k.clone()).or_default();
            st.update(v, k);
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

/// 1 列分の型推論状態。
#[derive(Debug, Default, Clone)]
struct ColumnState {
    /// `null` 以外の値が 1 件でもあったか。
    has_value: bool,
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
            JsonValue::Null => {} // 型不変、nullable 判定は build 時に行う
            JsonValue::Bool(_) => {
                self.has_value = true;
                self.inferred = Some(merge(self.inferred, Inferred::Bool));
            }
            JsonValue::Number(n) => {
                self.has_value = true;
                let t = if n.is_i64() || n.as_u64().is_some_and(|v| i64::try_from(v).is_ok()) {
                    Inferred::Int
                } else {
                    Inferred::Float
                };
                self.inferred = Some(merge(self.inferred, t));
            }
            JsonValue::String(_) => {
                self.has_value = true;
                self.inferred = Some(merge(self.inferred, Inferred::String));
            }
            JsonValue::Array(_) | JsonValue::Object(_) => {
                self.has_value = true;
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
        // 構造化値が出ていたら強制 Utf8。
        if self.saw_structured {
            return DataType::Utf8;
        }
        // None は全 null 列。値が無いので最も無害な Utf8 として扱う。
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

/// 全 Feature の geometry 型を集約する。
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
                    "GeometryCollection is not supported in v0.2 cycle 2".into(),
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
        Some(self.features.len())
    }

    fn batches(&mut self) -> Box<dyn Iterator<Item = Result<RecordBatch>> + Send + '_> {
        let features = std::mem::take(&mut self.features);
        Box::new(BatchIter {
            schema: self.schema.clone(),
            columns: self.columns.clone(),
            features: features.into_iter(),
            done: false,
        })
    }
}

struct BatchIter {
    schema: SchemaRef,
    columns: Vec<ColumnPlan>,
    features: std::vec::IntoIter<Feature>,
    done: bool,
}

impl BatchIter {
    fn next_batch(&mut self) -> Result<Option<RecordBatch>> {
        let mut builders = ColumnBuilders::new(&self.columns);
        let mut row_count = 0usize;

        for feature in self.features.by_ref() {
            builders.append_row(&feature, &self.columns)?;
            row_count += 1;
            if row_count >= READ_BATCH_SIZE {
                break;
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
