//! SpatiaLite の `LayerReader` 実装。
//!
//! v0.5 cycle 1 では GPKG reader と同じ eager-load 方式 (`Vec<Row>`) を採用する。
//! geometry 列は生 BLOB を取り出して [`shpx_geom::spatialite_blob::decode`] で SRID と
//! 標準 WKB を分離する。SRID は `geometry_columns` (列メタ) と `spatial_ref_sys` (CRS 定義)
//! の 2 段で解決する。

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
use crate::options::{strip_to_filepath, ResolvedReadOpts};
use crate::type_map;
use crate::util::{driver_err, driver_msg, quote_ident};

const READ_BATCH_SIZE: usize = 4096;

fn epoch() -> NaiveDate {
    NaiveDate::from_ymd_opt(1970, 1, 1).expect("1970-01-01 valid")
}

#[derive(Debug, Clone)]
enum AttrValue {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Text(String),
    Blob(Vec<u8>),
    Date(i32),
    TimestampUs(i64),
}

#[derive(Debug)]
struct Row {
    attrs: Vec<AttrValue>,
    geom: Option<Vec<u8>>,
}

#[derive(Debug, Clone)]
struct ColumnPlan {
    name: String,
    arrow_type: DataType,
}

pub struct SpatialiteReader {
    schema: SchemaRef,
    crs: Option<Crs>,
    columns: Vec<ColumnPlan>,
    rows: std::collections::VecDeque<Row>,
    row_count: usize,
}

impl SpatialiteReader {
    pub fn open(uri: &Uri, opts: &ReadOpts) -> Result<Self> {
        let resolved = ResolvedReadOpts::resolve(uri, opts)?;
        let path = PathBuf::from(strip_to_filepath(uri.path()));

        let conn = conn::open_read(&path)?;

        let table = resolve_table_name(&conn, resolved.table.as_deref())?;
        let (geom_column, geom_type_int, srid) = read_geometry_column(&conn, &table)?;

        let crs = match resolved.src_crs {
            Some(c) => Some(c),
            None => read_crs_for_srid(&conn, srid)?,
        };

        let columns = read_attribute_schema(&conn, &table, &geom_column)?;
        let geom_type = meta::geom_type_from_int(geom_type_int);
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

impl LayerReader for SpatialiteReader {
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
    reader: &'a mut SpatialiteReader,
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
        if names.iter().any(|n| n.eq_ignore_ascii_case(t)) {
            return Ok(t.to_string());
        }
        return Err(driver_msg(format!(
            "table `{t}` not found in geometry_columns. available: {names:?}"
        )));
    }
    match names.len() {
        0 => Err(driver_msg(
            "no feature tables in geometry_columns (empty SpatiaLite database)",
        )),
        1 => Ok(names.into_iter().next().expect("len==1")),
        _ => Err(driver_msg(format!(
            "multiple feature tables found, specify one with `?table=<name>` or env SHPX_SPATIALITE_TABLE: {names:?}"
        ))),
    }
}

/// `geometry_columns` 1 行の生型 (geom_column, geom_type_int, coord_dim, srid)。
type GeomColumnRow = (String, i32, i32, i32);

/// `spatial_ref_sys` 1 行の生型 (auth_name, auth_srid, ref_sys_name, proj4text, srtext)。
type SrsRow = (String, i32, Option<String>, Option<String>, Option<String>);

/// `geometry_columns` から column_name と geometry_type と srid を取得。
/// coord_dimension は v0.5 では XY (=2) のみ対応とし、それ以外は明示エラーにする。
fn read_geometry_column(conn: &Connection, table: &str) -> Result<(String, i32, i32)> {
    let mut stmt = conn
        .prepare(meta::SQL_SELECT_GEOM_COLUMN)
        .map_err(|e| driver_err(&e))?;
    let row: GeomColumnRow = stmt
        .query_row([table], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i32>(1)?,
                row.get::<_, i32>(2)?,
                row.get::<_, i32>(3)?,
            ))
        })
        .map_err(|e| driver_msg(format!("geometry_columns row missing for `{table}`: {e}")))?;
    let (geom_column, geom_type, coord_dim, srid) = row;
    if coord_dim != 2 {
        return Err(driver_msg(format!(
            "table `{table}` has coord_dimension={coord_dim} (Z/M not supported in v0.5)"
        )));
    }
    Ok((geom_column, geom_type, srid))
}

fn read_crs_for_srid(conn: &Connection, srid: i32) -> Result<Option<Crs>> {
    if srid <= 0 {
        return Ok(None);
    }
    let mut stmt = conn
        .prepare(meta::SQL_SELECT_SRS)
        .map_err(|e| driver_err(&e))?;
    // (auth_name, auth_srid, ref_sys_name, proj4text, srtext)
    let r: std::result::Result<SrsRow, _> = stmt.query_row([srid], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i32>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, Option<String>>(3)?,
            row.get::<_, Option<String>>(4)?,
        ))
    });
    let (auth_name, auth_srid, _ref_sys_name, _proj4text, srtext) = match r {
        Ok(v) => v,
        Err(rusqlite::Error::QueryReturnedNoRows) => {
            return Err(Error::Crs(format!(
                "dangling srid {srid}: not found in spatial_ref_sys"
            )));
        }
        Err(e) => return Err(driver_err(&e)),
    };
    if auth_name.eq_ignore_ascii_case("EPSG") {
        let code = u32::try_from(auth_srid).map_err(|_| {
            Error::Crs(format!(
                "invalid EPSG code {auth_srid} for srid {srid} (must be non-negative)"
            ))
        })?;
        Ok(Some(Crs::from_epsg(code)))
    } else {
        // EPSG 以外: srtext があればそれを使い、authority は (auth_name, auth_srid)。
        Ok(Some(Crs {
            authority: u32::try_from(auth_srid)
                .ok()
                .map(|c| (auth_name.clone(), c)),
            wkt: srtext,
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
        if name.eq_ignore_ascii_case(geom_column) {
            continue;
        }
        // shpx writer が生成する `fid INTEGER PRIMARY KEY AUTOINCREMENT` 列は
        // 中間表現に含めず、書き戻し時に再生成する。GDAL/QGIS の慣習にも合致する。
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
                let (_srid, wkb_bytes) = shpx_geom::spatialite_blob::decode(b)?;
                Some(wkb_bytes)
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

fn parse_iso_timestamp_micros(s: &str) -> std::result::Result<i64, String> {
    let trimmed = s.trim();
    let with_t = trimmed.replacen(' ', "T", 1);
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(&with_t) {
        return Ok(dt.timestamp_micros());
    }
    let formats = ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%dT%H:%M:%S"];
    for f in formats {
        if let Ok(ndt) = chrono::NaiveDateTime::parse_from_str(&with_t, f) {
            return Ok(ndt.and_utc().timestamp_micros());
        }
    }
    Err(format!("unrecognized timestamp format: `{s}`"))
}

fn build_one_batch(r: &mut SpatialiteReader) -> Result<Option<RecordBatch>> {
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
                    "unsupported builder for {other:?} in SpatiaLite reader"
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
