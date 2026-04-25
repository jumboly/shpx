//! GeoPackage の `LayerReader` 実装。
//!
//! v0.2 では SHP/CSV と同じく、open() で全行を `Vec<Row>` に読み込み、`batches()` で
//! チャンクとして取り出す eager-load 方式を採る。GPKG は典型的にギガバイト級になりにくく、
//! rusqlite の `Statement`/`Rows` のライフタイムを `LayerReader::batches` の戻り値型に
//! 載せるのが煩雑なため、cycle 3 では複雑度を上げない方針。完全 streaming は v0.3 以降で。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use arrow_array::builder::{
    BinaryBuilder, BooleanBuilder, Date32Builder, Float64Builder, Int64Builder, StringBuilder,
    TimestampMicrosecondBuilder,
};
use arrow_array::{ArrayRef, RecordBatch};
use arrow_schema::{DataType, Field, Schema, SchemaRef, TimeUnit};
use chrono::NaiveDate;
use rusqlite::{types::ValueRef, Connection};
use shpx_core::{
    schema::{GeometryMeta, GeometryType, GEOMETRY_META_KEY},
    Crs, Error, LayerReader, ReadOpts, Result, Uri, WktFlavor,
};

use crate::conn;
use crate::meta;
use crate::options::{strip_query, ResolvedReadOpts};
use crate::type_map;
use crate::util::{driver_err, driver_msg, quote_ident};

const READ_BATCH_SIZE: usize = 4096;

/// epoch 1970-01-01 (Date32 起点)。
fn epoch() -> NaiveDate {
    NaiveDate::from_ymd_opt(1970, 1, 1).expect("1970-01-01 valid")
}

/// パース済み属性値。SQLite の dynamic typing 値を Arrow builder に積みやすい形にしておく。
#[derive(Debug, Clone)]
enum AttrValue {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Text(String),
    Blob(Vec<u8>),
    Date(i32),
    /// Microsecond 単位の i64（`Timestamp(Microsecond, None)`）。
    TimestampUs(i64),
}

#[derive(Debug)]
struct Row {
    attrs: Vec<AttrValue>,
    geom: Option<Vec<u8>>,
}

/// 列計画。属性列のみ（geometry 列は別管理）。
#[derive(Debug, Clone)]
struct ColumnPlan {
    name: String,
    arrow_type: DataType,
}

pub struct GpkgReader {
    schema: SchemaRef,
    crs: Option<Crs>,
    columns: Vec<ColumnPlan>,
    rows: std::collections::VecDeque<Row>,
    row_count: usize,
}

impl GpkgReader {
    pub fn open(uri: &Uri, opts: &ReadOpts) -> Result<Self> {
        let resolved = ResolvedReadOpts::resolve(uri, opts)?;
        let path = PathBuf::from(strip_query(uri.path()));

        let conn = conn::open_read(&path)?;
        conn::verify_application_id(&conn)?;
        verify_required_meta_tables(&conn)?;

        let table = resolve_table_name(&conn, resolved.table.as_deref())?;
        let (geom_column, geom_type_name, srs_id) = read_geometry_column(&conn, &table)?;

        // CRS は `--src-crs` を最優先にし、無ければ gpkg_spatial_ref_sys から復元。
        let crs = match resolved.src_crs {
            Some(c) => Some(c),
            None => read_crs_for_srs(&conn, srs_id)?,
        };

        let columns = read_attribute_schema(&conn, &table, &geom_column)?;
        let geom_type = type_map::geom_type_from_name(&geom_type_name);
        let schema = build_schema(&columns, &geom_column, geom_type, crs.as_ref())?;

        let rows = load_all_rows(&conn, &table, &columns, &geom_column)?;
        let row_count = rows.len();

        Ok(Self {
            schema,
            crs,
            columns,
            rows: rows.into(),
            row_count,
        })
    }
}

impl LayerReader for GpkgReader {
    fn schema(&self) -> SchemaRef {
        self.schema.clone()
    }

    fn crs(&self) -> Option<&Crs> {
        self.crs.as_ref()
    }

    fn row_count_hint(&self) -> Option<usize> {
        Some(self.row_count)
    }

    fn batches(&mut self) -> Box<dyn Iterator<Item = Result<RecordBatch>> + Send + '_> {
        Box::new(BatchIter {
            reader: self,
            done: false,
        })
    }
}

struct BatchIter<'a> {
    reader: &'a mut GpkgReader,
    done: bool,
}

impl Iterator for BatchIter<'_> {
    type Item = Result<RecordBatch>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        match build_one_batch(self.reader) {
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

fn verify_required_meta_tables(conn: &Connection) -> Result<()> {
    for t in [
        "gpkg_spatial_ref_sys",
        "gpkg_contents",
        "gpkg_geometry_columns",
    ] {
        let exists: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                [t],
                |row| row.get(0),
            )
            .map_err(|e| driver_err(&e))?;
        if exists == 0 {
            return Err(driver_msg(format!(
                "not a GeoPackage: required table `{t}` is missing"
            )));
        }
    }
    Ok(())
}

fn resolve_table_name(conn: &Connection, hint: Option<&str>) -> Result<String> {
    let mut stmt = conn
        .prepare(meta::SQL_LIST_FEATURE_TABLES)
        .map_err(|e| driver_err(&e))?;
    let names: Vec<String> = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|e| driver_err(&e))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| driver_err(&e))?;
    if let Some(t) = hint {
        if names.iter().any(|n| n == t) {
            return Ok(t.to_string());
        }
        return Err(driver_msg(format!(
            "table `{t}` not found in gpkg_contents (data_type='features'). available: {names:?}"
        )));
    }
    match names.len() {
        0 => Err(driver_msg(
            "no feature tables in gpkg_contents (empty GeoPackage)",
        )),
        1 => Ok(names.into_iter().next().expect("len==1")),
        _ => Err(driver_msg(format!(
            "multiple feature tables found, specify one with `?table=<name>` or env SHPX_GPKG_TABLE: {names:?}"
        ))),
    }
}

/// gpkg_geometry_columns から geometry 列名・型名・srs_id を取得。
fn read_geometry_column(conn: &Connection, table: &str) -> Result<(String, String, i32)> {
    let mut stmt = conn
        .prepare(meta::SQL_SELECT_GEOM_COLUMN)
        .map_err(|e| driver_err(&e))?;
    let row = stmt
        .query_row([table], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i32>(2)?,
            ))
        })
        .map_err(|e| {
            driver_msg(format!(
                "gpkg_geometry_columns row missing for `{table}`: {e}"
            ))
        })?;
    Ok(row)
}

/// gpkg_spatial_ref_sys の 1 行から `Crs` を構築する。
fn read_crs_for_srs(conn: &Connection, srs_id: i32) -> Result<Option<Crs>> {
    // shpx は仕様必須の特殊 SRS（-1, 0）を「CRS 不明」として扱う。
    if srs_id == -1 || srs_id == 0 {
        return Ok(None);
    }
    let mut stmt = conn
        .prepare(meta::SQL_SELECT_SRS)
        .map_err(|e| driver_err(&e))?;
    let r: std::result::Result<(String, String, i32, String), _> =
        stmt.query_row([srs_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i32>(2)?,
                row.get::<_, String>(3)?,
            ))
        });
    let (_name, organization, org_code, definition) = match r {
        Ok(v) => v,
        Err(rusqlite::Error::QueryReturnedNoRows) => {
            return Err(Error::Crs(format!(
                "dangling srs_id {srs_id}: not found in gpkg_spatial_ref_sys"
            )));
        }
        Err(e) => return Err(driver_err(&e)),
    };
    if organization.eq_ignore_ascii_case(meta::ORG_EPSG) {
        let code = u32::try_from(org_code).map_err(|_| {
            Error::Crs(format!(
                "invalid EPSG code {org_code} for srs_id {srs_id} (must be non-negative)"
            ))
        })?;
        Ok(Some(Crs::from_epsg(code)))
    } else {
        // EPSG 以外は authority + definition (WKT1) を保持する。
        // gpkg_spatial_ref_sys.definition は WKT1 が標準。WKT2 拡張列 definition_12_063 は
        // v0.2 では読み出さない（OGC 12-063 拡張未対応の GPKG が多数のため）。
        Ok(Some(Crs {
            authority: u32::try_from(org_code)
                .ok()
                .map(|c| (organization.clone(), c)),
            wkt: Some(definition),
            wkt_flavor: WktFlavor::V1,
            projjson: None,
        }))
    }
}

fn read_attribute_schema(
    conn: &Connection,
    table: &str,
    geom_column: &str,
) -> Result<Vec<ColumnPlan>> {
    let pragma_sql = format!("PRAGMA table_info({})", quote_ident(table));
    let mut stmt = conn.prepare(&pragma_sql).map_err(|e| driver_err(&e))?;
    let rows: Vec<(String, String, i32)> = stmt
        .query_map([], |row| {
            // PRAGMA table_info: (cid, name, type, notnull, dflt_value, pk)
            Ok((
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i32>(5)?,
            ))
        })
        .map_err(|e| driver_err(&e))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| driver_err(&e))?;

    let mut columns = Vec::with_capacity(rows.len());
    for (name, decl, pk) in rows {
        if name == geom_column {
            continue;
        }
        // GPKG feature テーブル仕様で必須の整数 PK 列は shpx の中間表現には含めない。
        // 書き戻し時に writer 側が `fid INTEGER PRIMARY KEY AUTOINCREMENT` を再生成するため、
        // ここで列に持たせると roundtrip で重複が起きる。GDAL/QGIS が生成する GPKG では
        // `fid` / `id` / `OBJECTID` 等の名前が使われるが、shpx は名前ではなく
        // 「PK かつ INTEGER 宣言」の構造で判定する。
        if pk == 1 && decl.eq_ignore_ascii_case("INTEGER") {
            continue;
        }
        let arrow_type = type_map::decl_to_arrow(&decl)?;
        columns.push(ColumnPlan { name, arrow_type });
    }
    Ok(columns)
}

fn build_schema(
    columns: &[ColumnPlan],
    geom_column: &str,
    geom_type: GeometryType,
    crs: Option<&Crs>,
) -> Result<SchemaRef> {
    let mut fields: Vec<Arc<Field>> = Vec::with_capacity(columns.len() + 1);
    for c in columns {
        fields.push(Arc::new(Field::new(
            c.name.clone(),
            c.arrow_type.clone(),
            true,
        )));
    }
    let meta = GeometryMeta::wkb(geom_type, crs.cloned());
    let mut geom_field = Field::new(geom_column, DataType::Binary, true);
    let mut field_meta = HashMap::with_capacity(1);
    field_meta.insert(GEOMETRY_META_KEY.to_string(), meta.to_json()?);
    geom_field.set_metadata(field_meta);
    fields.push(Arc::new(geom_field));
    Ok(Arc::new(Schema::new(fields)))
}

fn load_all_rows(
    conn: &Connection,
    table: &str,
    columns: &[ColumnPlan],
    geom_column: &str,
) -> Result<Vec<Row>> {
    // 列順は `columns` の宣言順 + 末尾に geometry。geom 列を select 末尾に置くことで
    // batch ビルダ側のループも単純化する。
    let mut select_cols: Vec<String> = columns.iter().map(|c| quote_ident(&c.name)).collect();
    select_cols.push(quote_ident(geom_column));
    let sql = format!(
        "SELECT {} FROM {}",
        select_cols.join(", "),
        quote_ident(table)
    );

    let mut stmt = conn.prepare(&sql).map_err(|e| driver_err(&e))?;
    let mut rows = stmt.query([]).map_err(|e| driver_err(&e))?;

    let mut out = Vec::new();
    while let Some(row) = rows.next().map_err(|e| driver_err(&e))? {
        let mut attrs = Vec::with_capacity(columns.len());
        for (i, c) in columns.iter().enumerate() {
            let v = row.get_ref(i).map_err(|e| driver_err(&e))?;
            attrs.push(decode_value(v, &c.arrow_type, &c.name)?);
        }
        let geom_idx = columns.len();
        let geom_v = row.get_ref(geom_idx).map_err(|e| driver_err(&e))?;
        let geom = match geom_v {
            ValueRef::Null => None,
            ValueRef::Blob(b) => {
                // GPKG header を剥がして WKB 部分のみ Arrow Binary 列に格納する。
                let (_h, wkb_bytes) = shpx_geom::gpkg_blob::decode(b)?;
                Some(wkb_bytes.to_vec())
            }
            other => {
                return Err(driver_msg(format!(
                    "geometry column `{geom_column}` is not BLOB ({other:?})"
                )));
            }
        };
        out.push(Row { attrs, geom });
    }
    Ok(out)
}

/// SQLite の `ValueRef` を Arrow 型に合わせて `AttrValue` に変換する。
///
/// 値型と宣言型がずれた場合は文字列降格（GPKG の dynamic typing 救済）。
fn decode_value(v: ValueRef<'_>, target: &DataType, field: &str) -> Result<AttrValue> {
    if matches!(v, ValueRef::Null) {
        return Ok(AttrValue::Null);
    }
    match target {
        DataType::Boolean => Ok(AttrValue::Bool(coerce_int(v, field)? != 0)),
        DataType::Int8 | DataType::Int16 | DataType::Int32 | DataType::Int64 => {
            Ok(AttrValue::Int(coerce_int(v, field)?))
        }
        DataType::Float32 | DataType::Float64 => Ok(AttrValue::Float(coerce_float(v, field)?)),
        DataType::Utf8 => Ok(AttrValue::Text(coerce_text(v))),
        DataType::Binary => match v {
            ValueRef::Blob(b) => Ok(AttrValue::Blob(b.to_vec())),
            other => Err(driver_msg(format!(
                "field `{field}`: expected BLOB, got {other:?}"
            ))),
        },
        DataType::Date32 => match v {
            ValueRef::Text(s) | ValueRef::Blob(s) => {
                let txt = std::str::from_utf8(s).map_err(|e| driver_err(&e))?;
                let nd = NaiveDate::parse_from_str(txt, "%Y-%m-%d").map_err(|e| {
                    driver_msg(format!("field `{field}`: invalid DATE `{txt}`: {e}"))
                })?;
                let days = nd.signed_duration_since(epoch()).num_days();
                let days32 = i32::try_from(days).map_err(|_| {
                    driver_msg(format!("field `{field}`: DATE out of Date32 range: {txt}"))
                })?;
                Ok(AttrValue::Date(days32))
            }
            other => Err(driver_msg(format!(
                "field `{field}`: expected DATE TEXT, got {other:?}"
            ))),
        },
        DataType::Timestamp(TimeUnit::Microsecond, None) => match v {
            ValueRef::Text(s) | ValueRef::Blob(s) => {
                let txt = std::str::from_utf8(s).map_err(|e| driver_err(&e))?;
                let micros = parse_iso_timestamp_micros(txt).map_err(|e| {
                    driver_msg(format!("field `{field}`: invalid DATETIME `{txt}`: {e}"))
                })?;
                Ok(AttrValue::TimestampUs(micros))
            }
            other => Err(driver_msg(format!(
                "field `{field}`: expected DATETIME TEXT, got {other:?}"
            ))),
        },
        other => Err(Error::Schema(format!(
            "field `{field}`: unsupported target Arrow type {other:?}"
        ))),
    }
}

fn coerce_int(v: ValueRef<'_>, field: &str) -> Result<i64> {
    match v {
        ValueRef::Integer(i) => Ok(i),
        // SQLite の REAL→INTEGER は明示降格。i64 範囲外は飽和されるが、shpx は
        // 「宣言型と値型のずれ」の救済としての降格に限るため、特別な丸め保証は不要。
        #[allow(clippy::cast_possible_truncation)]
        ValueRef::Real(f) => Ok(f as i64),
        ValueRef::Text(s) | ValueRef::Blob(s) => {
            let txt = std::str::from_utf8(s).map_err(|e| driver_err(&e))?;
            txt.parse::<i64>().map_err(|e| {
                driver_msg(format!(
                    "field `{field}`: cannot coerce TEXT `{txt}` to INTEGER: {e}"
                ))
            })
        }
        ValueRef::Null => Err(driver_msg(format!(
            "field `{field}`: NULL passed to coerce_int"
        ))),
    }
}

fn coerce_float(v: ValueRef<'_>, field: &str) -> Result<f64> {
    match v {
        ValueRef::Real(f) => Ok(f),
        // i64 → f64 は仮数 52 bits を超える整数で精度落ちが起こり得るが、
        // dynamic typing の救済経路として許容する（呼び出し側は宣言型 FLOAT/DOUBLE）。
        #[allow(clippy::cast_precision_loss)]
        ValueRef::Integer(i) => Ok(i as f64),
        ValueRef::Text(s) | ValueRef::Blob(s) => {
            let txt = std::str::from_utf8(s).map_err(|e| driver_err(&e))?;
            txt.parse::<f64>().map_err(|e| {
                driver_msg(format!(
                    "field `{field}`: cannot coerce TEXT `{txt}` to FLOAT: {e}"
                ))
            })
        }
        ValueRef::Null => Err(driver_msg(format!(
            "field `{field}`: NULL passed to coerce_float"
        ))),
    }
}

fn coerce_text(v: ValueRef<'_>) -> String {
    match v {
        ValueRef::Text(s) | ValueRef::Blob(s) => String::from_utf8_lossy(s).into_owned(),
        ValueRef::Integer(i) => i.to_string(),
        ValueRef::Real(f) => f.to_string(),
        ValueRef::Null => String::new(),
    }
}

/// `YYYY-MM-DDTHH:MM:SS[.fff][Z|+/-HH:MM]` を Microsecond 単位の i64 に。UTC 仮定（offset 無しは Z 扱い）。
fn parse_iso_timestamp_micros(s: &str) -> std::result::Result<i64, String> {
    // RFC 3339 / ISO 8601 を chrono のフォーマットでパース。
    // Z 末尾は `%Z` で受け付けないため、明示分岐する。
    let trimmed = s.trim();
    let with_t = trimmed.replacen(' ', "T", 1);
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(&with_t) {
        return Ok(dt.timestamp_micros());
    }
    // タイムゾーン無し → UTC 仮定。秒小数点ありなしを両方試す。
    let formats = ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%dT%H:%M:%S"];
    for f in formats {
        if let Ok(ndt) = chrono::NaiveDateTime::parse_from_str(&with_t, f) {
            return Ok(ndt.and_utc().timestamp_micros());
        }
    }
    Err(format!("unrecognized timestamp format: `{s}`"))
}

fn build_one_batch(r: &mut GpkgReader) -> Result<Option<RecordBatch>> {
    if r.rows.is_empty() {
        return Ok(None);
    }
    let n_rows = r.rows.len().min(READ_BATCH_SIZE);

    let mut attr_builders: Vec<AttrBuilder> = r
        .columns
        .iter()
        .map(|c| AttrBuilder::new(&c.arrow_type, n_rows))
        .collect::<Result<Vec<_>>>()?;
    let mut geom_builder = BinaryBuilder::with_capacity(n_rows, n_rows * 32);

    for _ in 0..n_rows {
        let row = r.rows.pop_front().expect("invariant: n_rows ≤ rows.len()");
        for (i, b) in attr_builders.iter_mut().enumerate() {
            b.push(&row.attrs[i], &r.columns[i].name)?;
        }
        match row.geom {
            Some(bytes) => geom_builder.append_value(&bytes),
            None => geom_builder.append_null(),
        }
    }

    let mut columns: Vec<ArrayRef> = Vec::with_capacity(attr_builders.len() + 1);
    for b in attr_builders {
        columns.push(b.finish());
    }
    columns.push(Arc::new(geom_builder.finish()) as ArrayRef);

    let batch = RecordBatch::try_new(r.schema.clone(), columns)
        .map_err(|e| driver_msg(format!("RecordBatch::try_new failed: {e}")))?;
    Ok(Some(batch))
}

enum AttrBuilder {
    Bool(BooleanBuilder),
    Int(Int64Builder),
    Float(Float64Builder),
    Text(StringBuilder),
    Binary(BinaryBuilder),
    Date(Date32Builder),
    Timestamp(TimestampMicrosecondBuilder),
}

impl AttrBuilder {
    fn new(t: &DataType, capacity: usize) -> Result<Self> {
        Ok(match t {
            DataType::Boolean => Self::Bool(BooleanBuilder::with_capacity(capacity)),
            // shpx の中間表現は `Int64` に揃える。Int8/16/32 で来ても i64 として保つ
            // （writer 側は宣言型だけ TINYINT/SMALLINT に書き分ける）。v0.2 はシンプルさを優先。
            DataType::Int8 | DataType::Int16 | DataType::Int32 | DataType::Int64 => {
                Self::Int(Int64Builder::with_capacity(capacity))
            }
            DataType::Float32 | DataType::Float64 => {
                Self::Float(Float64Builder::with_capacity(capacity))
            }
            DataType::Utf8 => Self::Text(StringBuilder::with_capacity(capacity, capacity * 16)),
            DataType::Binary => Self::Binary(BinaryBuilder::with_capacity(capacity, capacity * 16)),
            DataType::Date32 => Self::Date(Date32Builder::with_capacity(capacity)),
            DataType::Timestamp(TimeUnit::Microsecond, None) => {
                Self::Timestamp(TimestampMicrosecondBuilder::with_capacity(capacity))
            }
            other => {
                return Err(Error::Schema(format!(
                    "unsupported builder for {other:?} in GPKG reader"
                )))
            }
        })
    }

    fn push(&mut self, v: &AttrValue, field: &str) -> Result<()> {
        match (self, v) {
            (Self::Bool(b), AttrValue::Null) => b.append_null(),
            (Self::Bool(b), AttrValue::Bool(x)) => b.append_value(*x),
            (Self::Bool(b), AttrValue::Int(i)) => b.append_value(*i != 0),
            (Self::Int(b), AttrValue::Null) => b.append_null(),
            (Self::Int(b), AttrValue::Int(x)) => b.append_value(*x),
            (Self::Float(b), AttrValue::Null) => b.append_null(),
            (Self::Float(b), AttrValue::Float(x)) => b.append_value(*x),
            // INTEGER → FLOAT 列の救済降格。i64 全域では精度落ちが起こり得るが、
            // dynamic typing で混在した値を読めるようにするための妥協。
            #[allow(clippy::cast_precision_loss)]
            (Self::Float(b), AttrValue::Int(x)) => b.append_value(*x as f64),
            (Self::Text(b), AttrValue::Null) => b.append_null(),
            (Self::Text(b), AttrValue::Text(x)) => b.append_value(x),
            (Self::Binary(b), AttrValue::Null) => b.append_null(),
            (Self::Binary(b), AttrValue::Blob(x)) => b.append_value(x),
            (Self::Date(b), AttrValue::Null) => b.append_null(),
            (Self::Date(b), AttrValue::Date(d)) => b.append_value(*d),
            (Self::Timestamp(b), AttrValue::Null) => b.append_null(),
            (Self::Timestamp(b), AttrValue::TimestampUs(t)) => b.append_value(*t),
            (_, _) => {
                return Err(driver_msg(format!(
                    "field `{field}`: builder/value type mismatch"
                )))
            }
        }
        Ok(())
    }

    fn finish(self) -> ArrayRef {
        match self {
            Self::Bool(mut b) => Arc::new(b.finish()),
            Self::Int(mut b) => Arc::new(b.finish()),
            Self::Float(mut b) => Arc::new(b.finish()),
            Self::Text(mut b) => Arc::new(b.finish()),
            Self::Binary(mut b) => Arc::new(b.finish()),
            Self::Date(mut b) => Arc::new(b.finish()),
            Self::Timestamp(mut b) => Arc::new(b.finish()),
        }
    }
}
