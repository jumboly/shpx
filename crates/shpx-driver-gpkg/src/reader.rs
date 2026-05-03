//! GeoPackage の `LayerReader` 実装。
//!
//! ストリーミング戦略: `shpx_rdb_common::streaming::KeysetRowsIter` 経由の rowid keyset
//! pagination で行データを `batches()` から 65536 行ずつ取り出す。`open()` 時点で
//! schema と CRS を確定し、`SELECT COUNT(*)` で row_count_hint を 1 度だけ算出する。
//! `Statement` / `Rows` の lifetime は `KeysetRowsIter::next_batch()` のスコープ内に
//! 閉じることで self-referential を回避している。

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
use rusqlite::{types::Value, Connection};
use shpx_core::{
    schema::{GeometryMeta, GeometryType, GEOMETRY_META_KEY},
    Crs, Error, LayerReader, ReadOpts, Result, Uri, WktFlavor,
};
use shpx_rdb_common::streaming::{KeysetRowsIter, RowBatch};

use crate::conn;
use crate::meta;
use crate::options::{strip_query, ResolvedReadOpts};
use crate::type_map;
use crate::util::{driver_err, driver_msg, quote_ident, DRIVER_NAME};

const READ_BATCH_SIZE: usize = 65_536;

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
    geom_column: String,
    row_count: usize,
    /// SELECT 末尾に rowid を含む keyset pagination SQL テンプレート。
    /// `?1` に最終 rowid、`?2` に LIMIT を bind する。
    sql_template: String,
    conn: Connection,
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

        // 行データは streaming で読むが、進捗バーのため row_count を 1 度だけ正確に算出する。
        // SQLite の COUNT(*) は full-table scan だが、GPKG feature テーブルの典型サイズ
        // (数万〜数千万行) では数 ms〜数百 ms で完了するため許容する。
        let row_count = count_rows(&conn, &table)?;

        let sql_template = build_keyset_sql(&table, &columns, &geom_column);

        Ok(Self {
            schema,
            crs,
            columns,
            geom_column,
            row_count,
            sql_template,
            conn,
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
        // columns/geom_column/schema は BatchIter の lifetime 中固定なので clone で持つ
        // (ColumnPlan は数十要素、SchemaRef は Arc なので cheap)。
        let inner = KeysetRowsIter::new(
            &mut self.conn,
            self.sql_template.clone(),
            READ_BATCH_SIZE,
            DRIVER_NAME,
        );
        Box::new(BatchIter {
            inner,
            columns: self.columns.clone(),
            geom_column: self.geom_column.clone(),
            schema: self.schema.clone(),
        })
    }
}

struct BatchIter<'a> {
    inner: KeysetRowsIter<'a>,
    columns: Vec<ColumnPlan>,
    geom_column: String,
    schema: SchemaRef,
}

impl Iterator for BatchIter<'_> {
    type Item = Result<RecordBatch>;
    fn next(&mut self) -> Option<Self::Item> {
        match self.inner.next_batch() {
            Ok(Some(rb)) => Some(build_record_batch(
                rb,
                &self.columns,
                &self.geom_column,
                &self.schema,
            )),
            Ok(None) => None,
            Err(e) => Some(Err(e)),
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
///
/// OGC 12-063 拡張で追加された `definition_12_063`（WKT2）列が存在し、かつ非空ならば
/// WKT2 を採用する。それ以外は標準 `definition`（WKT1）にフォールバックする。
fn read_crs_for_srs(conn: &Connection, srs_id: i32) -> Result<Option<Crs>> {
    // shpx は仕様必須の特殊 SRS（-1, 0）を「CRS 不明」として扱う。
    if srs_id == -1 || srs_id == 0 {
        return Ok(None);
    }

    let has_wkt2 = srs_table_has_wkt2_column(conn)?;
    let sql = if has_wkt2 {
        meta::SQL_SELECT_SRS_WITH_WKT2
    } else {
        meta::SQL_SELECT_SRS
    };
    let mut stmt = conn.prepare(sql).map_err(|e| driver_err(&e))?;
    let r = stmt.query_row([srs_id], |row| {
        let organization = row.get::<_, String>(1)?;
        let org_code = row.get::<_, i32>(2)?;
        let def_wkt1 = row.get::<_, String>(3)?;
        let def_wkt2 = if has_wkt2 {
            row.get::<_, Option<String>>(4)?
        } else {
            None
        };
        Ok((organization, org_code, def_wkt1, def_wkt2))
    });
    let (organization, org_code, def_wkt1, def_wkt2) = match r {
        Ok(v) => v,
        Err(rusqlite::Error::QueryReturnedNoRows) => {
            return Err(Error::Crs(format!(
                "dangling srs_id {srs_id}: not found in gpkg_spatial_ref_sys"
            )));
        }
        Err(e) => return Err(driver_err(&e)),
    };

    // WKT2 が非空なら優先。空文字列 / NULL は WKT1 にフォールバックする。
    let (wkt, wkt_flavor) = match def_wkt2.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(_) => (def_wkt2.expect("non-empty above"), WktFlavor::V2),
        None => (def_wkt1, WktFlavor::V1),
    };

    if organization.eq_ignore_ascii_case(meta::ORG_EPSG) {
        let code = u32::try_from(org_code).map_err(|_| {
            Error::Crs(format!(
                "invalid EPSG code {org_code} for srs_id {srs_id} (must be non-negative)"
            ))
        })?;
        Ok(Some(Crs::from_epsg(code)))
    } else {
        Ok(Some(Crs {
            authority: u32::try_from(org_code).ok().map(|c| (organization, c)),
            wkt: Some(wkt),
            wkt_flavor,
            projjson: None,
        }))
    }
}

/// `gpkg_spatial_ref_sys` に OGC 12-063 拡張列 `definition_12_063` があるか判定する。
fn srs_table_has_wkt2_column(conn: &Connection) -> Result<bool> {
    let mut stmt = conn
        .prepare("PRAGMA table_info(gpkg_spatial_ref_sys)")
        .map_err(|e| driver_err(&e))?;
    let names: Vec<String> = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|e| driver_err(&e))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| driver_err(&e))?;
    Ok(names.iter().any(|n| n == "definition_12_063"))
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

/// `SELECT COUNT(*) FROM <table>` を 1 度だけ実行して総行数を返す。進捗バーの分母用。
fn count_rows(conn: &Connection, table: &str) -> Result<usize> {
    let sql = format!("SELECT COUNT(*) FROM {}", quote_ident(table));
    let n: i64 = conn
        .query_row(&sql, [], |row| row.get(0))
        .map_err(|e| driver_err(&e))?;
    usize::try_from(n).map_err(|_| driver_msg(format!("row count {n} exceeds usize")))
}

/// `SELECT col1, col2, ..., geom, rowid FROM <table> WHERE rowid > ?1 ORDER BY rowid LIMIT ?2`
/// を構築する。columns 順 → geometry 列 → rowid (KeysetRowsIter が末尾列を消費する慣習)。
fn build_keyset_sql(table: &str, columns: &[ColumnPlan], geom_column: &str) -> String {
    let mut select_cols: Vec<String> = columns.iter().map(|c| quote_ident(&c.name)).collect();
    select_cols.push(quote_ident(geom_column));
    select_cols.push("rowid".to_string());
    format!(
        "SELECT {} FROM {} WHERE rowid > ?1 ORDER BY rowid LIMIT ?2",
        select_cols.join(", "),
        quote_ident(table)
    )
}

/// `RowBatch` (rusqlite::Value 配列) を Arrow RecordBatch に変換する。
///
/// `rb` を所有権で受けて `into_iter` で各 row / 各 Value を move 消費することで、
/// Text / Blob の double clone を回避する (`row.get::<_, Value>(i)` で 1 回 alloc 済み)。
fn build_record_batch(
    rb: RowBatch,
    columns: &[ColumnPlan],
    geom_column: &str,
    schema: &SchemaRef,
) -> Result<RecordBatch> {
    let n_rows = rb.rows.len();
    let mut attr_builders: Vec<AttrBuilder> = columns
        .iter()
        .map(|c| AttrBuilder::new(&c.arrow_type, n_rows))
        .collect::<Result<Vec<_>>>()?;
    let mut geom_builder = BinaryBuilder::with_capacity(n_rows, n_rows * 32);

    let expected_cols = columns.len() + 1;
    for row in rb.rows {
        if row.len() != expected_cols {
            return Err(driver_msg(format!(
                "row has {} columns, expected {}",
                row.len(),
                expected_cols
            )));
        }
        let mut values = row.into_iter();
        for (i, c) in columns.iter().enumerate() {
            let v = values.next().expect("column count verified above");
            let attr = decode_value(v, &c.arrow_type, &c.name)?;
            attr_builders[i].push_owned(attr, &c.name)?;
        }
        match values.next().expect("geometry column at end") {
            Value::Null => geom_builder.append_null(),
            Value::Blob(b) => {
                // GPKG header を剥がして WKB 部分のみ Arrow Binary 列に格納する。
                let (_h, wkb_bytes) = shpx_geom::gpkg_blob::decode(&b)?;
                geom_builder.append_value(wkb_bytes);
            }
            other => {
                return Err(driver_msg(format!(
                    "geometry column `{geom_column}` is not BLOB ({other:?})"
                )));
            }
        }
    }

    let mut out_columns: Vec<ArrayRef> = Vec::with_capacity(attr_builders.len() + 1);
    for b in attr_builders {
        out_columns.push(b.finish());
    }
    out_columns.push(Arc::new(geom_builder.finish()) as ArrayRef);

    RecordBatch::try_new(schema.clone(), out_columns)
        .map_err(|e| driver_msg(format!("RecordBatch::try_new failed: {e}")))
}

/// SQLite の `Value` を Arrow 型に合わせて `AttrValue` に変換する。
///
/// 値型と宣言型がずれた場合は文字列降格（GPKG の dynamic typing 救済）。
/// `v` を所有権で受け、Text / Blob は move で AttrValue へ流し込み、clone を回避する。
fn decode_value(v: Value, target: &DataType, field: &str) -> Result<AttrValue> {
    if matches!(v, Value::Null) {
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
            Value::Blob(b) => Ok(AttrValue::Blob(b)),
            other => Err(driver_msg(format!(
                "field `{field}`: expected BLOB, got {other:?}"
            ))),
        },
        DataType::Date32 => decode_date32(v, field),
        DataType::Timestamp(TimeUnit::Microsecond, None) => decode_timestamp_us(v, field),
        other => Err(Error::Schema(format!(
            "field `{field}`: unsupported target Arrow type {other:?}"
        ))),
    }
}

fn decode_date32(v: Value, field: &str) -> Result<AttrValue> {
    let txt: String = match v {
        Value::Text(s) => s,
        Value::Blob(b) => {
            String::from_utf8(b).map_err(|e| driver_msg(format!("field `{field}`: {e}")))?
        }
        other => {
            return Err(driver_msg(format!(
                "field `{field}`: expected DATE TEXT, got {other:?}"
            )))
        }
    };
    let nd = NaiveDate::parse_from_str(&txt, "%Y-%m-%d")
        .map_err(|e| driver_msg(format!("field `{field}`: invalid DATE `{txt}`: {e}")))?;
    let days = nd.signed_duration_since(epoch()).num_days();
    let days32 = i32::try_from(days)
        .map_err(|_| driver_msg(format!("field `{field}`: DATE out of Date32 range: {txt}")))?;
    Ok(AttrValue::Date(days32))
}

fn decode_timestamp_us(v: Value, field: &str) -> Result<AttrValue> {
    let txt: String = match v {
        Value::Text(s) => s,
        Value::Blob(b) => {
            String::from_utf8(b).map_err(|e| driver_msg(format!("field `{field}`: {e}")))?
        }
        other => {
            return Err(driver_msg(format!(
                "field `{field}`: expected DATETIME TEXT, got {other:?}"
            )))
        }
    };
    let micros = parse_iso_timestamp_micros(&txt)
        .map_err(|e| driver_msg(format!("field `{field}`: invalid DATETIME `{txt}`: {e}")))?;
    Ok(AttrValue::TimestampUs(micros))
}

fn coerce_int(v: Value, field: &str) -> Result<i64> {
    match v {
        Value::Integer(i) => Ok(i),
        // SQLite の REAL→INTEGER は明示降格。i64 範囲外は飽和されるが、shpx は
        // 「宣言型と値型のずれ」の救済としての降格に限るため、特別な丸め保証は不要。
        #[allow(clippy::cast_possible_truncation)]
        Value::Real(f) => Ok(f as i64),
        Value::Text(s) => s.parse::<i64>().map_err(|e| {
            driver_msg(format!(
                "field `{field}`: cannot coerce TEXT `{s}` to INTEGER: {e}"
            ))
        }),
        Value::Blob(b) => {
            let txt = std::str::from_utf8(&b).map_err(|e| driver_err(&e))?;
            txt.parse::<i64>().map_err(|e| {
                driver_msg(format!(
                    "field `{field}`: cannot coerce TEXT `{txt}` to INTEGER: {e}"
                ))
            })
        }
        Value::Null => Err(driver_msg(format!(
            "field `{field}`: NULL passed to coerce_int"
        ))),
    }
}

fn coerce_float(v: Value, field: &str) -> Result<f64> {
    match v {
        Value::Real(f) => Ok(f),
        // i64 → f64 は仮数 52 bits を超える整数で精度落ちが起こり得るが、
        // dynamic typing の救済経路として許容する（呼び出し側は宣言型 FLOAT/DOUBLE）。
        #[allow(clippy::cast_precision_loss)]
        Value::Integer(i) => Ok(i as f64),
        Value::Text(s) => s.parse::<f64>().map_err(|e| {
            driver_msg(format!(
                "field `{field}`: cannot coerce TEXT `{s}` to FLOAT: {e}"
            ))
        }),
        Value::Blob(b) => {
            let txt = std::str::from_utf8(&b).map_err(|e| driver_err(&e))?;
            txt.parse::<f64>().map_err(|e| {
                driver_msg(format!(
                    "field `{field}`: cannot coerce TEXT `{txt}` to FLOAT: {e}"
                ))
            })
        }
        Value::Null => Err(driver_msg(format!(
            "field `{field}`: NULL passed to coerce_float"
        ))),
    }
}

fn coerce_text(v: Value) -> String {
    match v {
        Value::Text(s) => s,
        Value::Blob(b) => String::from_utf8_lossy(&b).into_owned(),
        Value::Integer(i) => i.to_string(),
        Value::Real(f) => f.to_string(),
        Value::Null => String::new(),
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

    /// `AttrValue` を所有権ごと受け取る (Text / Blob を move して clone を回避する経路)。
    fn push_owned(&mut self, v: AttrValue, field: &str) -> Result<()> {
        match (self, v) {
            (Self::Bool(b), AttrValue::Null) => b.append_null(),
            (Self::Bool(b), AttrValue::Bool(x)) => b.append_value(x),
            (Self::Bool(b), AttrValue::Int(i)) => b.append_value(i != 0),
            (Self::Int(b), AttrValue::Null) => b.append_null(),
            (Self::Int(b), AttrValue::Int(x)) => b.append_value(x),
            (Self::Float(b), AttrValue::Null) => b.append_null(),
            (Self::Float(b), AttrValue::Float(x)) => b.append_value(x),
            // INTEGER → FLOAT 列の救済降格。i64 全域では精度落ちが起こり得るが、
            // dynamic typing で混在した値を読めるようにするための妥協。
            #[allow(clippy::cast_precision_loss)]
            (Self::Float(b), AttrValue::Int(x)) => b.append_value(x as f64),
            (Self::Text(b), AttrValue::Null) => b.append_null(),
            (Self::Text(b), AttrValue::Text(x)) => b.append_value(&x),
            (Self::Binary(b), AttrValue::Null) => b.append_null(),
            (Self::Binary(b), AttrValue::Blob(x)) => b.append_value(&x),
            (Self::Date(b), AttrValue::Null) => b.append_null(),
            (Self::Date(b), AttrValue::Date(d)) => b.append_value(d),
            (Self::Timestamp(b), AttrValue::Null) => b.append_null(),
            (Self::Timestamp(b), AttrValue::TimestampUs(t)) => b.append_value(t),
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
