//! SpatiaLite の `LayerWriter` 実装。
//!
//! - open() でファイル open → InitSpatialMetadata → CREATE TABLE → AddGeometryColumn。
//! - write_batch() は 1 トランザクションで全行 INSERT。geometry 列は `GeomFromWKB(?, srid)`
//!   で SpatiaLite に encode を委譲する（spatialite_blob::encode との実装ずれを回避）。
//! - finish() でクローズ。bbox 反映や R*Tree 生成は v0.5 cycle 2 で `--create-index` 経路から行う。

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
use chrono::{DateTime, Duration, FixedOffset, NaiveDate, TimeZone, Utc};
use rusqlite::{params_from_iter, types::Value as SqlValue, Connection};
use shpx_core::{
    schema::{find_geometry_column, GeometryMeta},
    Crs, Error, LayerWriter, OnLoss, Result, Uri, WriteOpts,
};

use crate::conn;
use crate::meta;
use crate::options::{strip_to_filepath, ResolvedWriteOpts};
use crate::type_map;
use crate::util::{apply_on_loss, driver_err, driver_msg, loss_kind, quote_ident};

const ID_COLUMN: &str = "fid";

pub struct SpatialiteWriter {
    conn: Option<Connection>,
    schema: SchemaRef,
    geom_index: usize,
    attr_indices: Vec<usize>,
    insert_sql: String,
    on_loss: OnLoss,
}

impl SpatialiteWriter {
    pub fn open(uri: &Uri, schema: SchemaRef, crs: Option<&Crs>, opts: &WriteOpts) -> Result<Self> {
        let resolved = ResolvedWriteOpts::resolve(uri, opts)?;
        let path = PathBuf::from(strip_to_filepath(uri.path()));
        let table = resolved
            .table
            .clone()
            .or_else(|| {
                path.file_stem()
                    .and_then(|s| s.to_str())
                    .map(sanitize_table_name)
            })
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "features".to_string());

        if path.exists() {
            if !resolved.overwrite {
                return Err(Error::Format(format!(
                    "output already exists: {} (use --overwrite)",
                    path.display()
                )));
            }
            std::fs::remove_file(&path).map_err(Error::from)?;
            for sfx in ["-wal", "-shm", "-journal"] {
                let p = path.with_file_name(format!(
                    "{}{}",
                    path.file_name().and_then(|s| s.to_str()).unwrap_or(""),
                    sfx
                ));
                let _ = std::fs::remove_file(&p);
            }
        }

        let conn = conn::open_write_new(&path)?;

        let (geom_index, _, geom_meta) = find_geometry_column(&schema)?
            .ok_or_else(|| Error::Schema("no geometry column for SpatiaLite writer".to_string()))?;
        let geom_column = schema.field(geom_index).name().clone();
        let srid = register_srs(&conn, &geom_meta, crs, opts.on_loss)?;

        let attr_indices: Vec<usize> = (0..schema.fields().len())
            .filter(|i| *i != geom_index)
            .collect();

        // CREATE TABLE は属性列のみ。geometry 列は AddGeometryColumn で追加する
        // (geometry_columns への登録と列宣言を一括で行うため)。
        let create_sql = build_create_table_sql(&schema, &attr_indices, &table)?;
        conn.execute(&create_sql, []).map_err(|e| driver_err(&e))?;

        // AddGeometryColumn(table, column, srid, type_name, dimension)。
        // SpatiaLite 4.x で大文字の型名 ('POINT', 'LINESTRING' など) を要求する。
        // dimension = 'XY' は coord_dimension=2。
        let geom_type_name = meta::geom_type_to_name(geom_meta.geometry_type);
        conn.query_row(
            "SELECT AddGeometryColumn(?1, ?2, ?3, ?4, 'XY')",
            rusqlite::params![table, geom_column, srid, geom_type_name],
            |_| Ok(()),
        )
        .map_err(|e| {
            driver_msg(format!(
                "AddGeometryColumn(table={table}, col={geom_column}, srid={srid}, type={geom_type_name}) failed: {e}"
            ))
        })?;

        let insert_sql = build_insert_sql(&schema, &attr_indices, geom_index, &table, srid);

        Ok(Self {
            conn: Some(conn),
            schema,
            geom_index,
            attr_indices,
            insert_sql,
            on_loss: opts.on_loss,
        })
    }
}

impl LayerWriter for SpatialiteWriter {
    fn write_batch(&mut self, batch: &RecordBatch) -> Result<()> {
        let conn = self
            .conn
            .as_mut()
            .ok_or_else(|| driver_msg("write_batch called after finish"))?;

        let tx = conn.transaction().map_err(|e| driver_err(&e))?;
        {
            let mut stmt = tx
                .prepare_cached(&self.insert_sql)
                .map_err(|e| driver_err(&e))?;
            for row in 0..batch.num_rows() {
                let mut params: Vec<SqlValue> = Vec::with_capacity(self.attr_indices.len() + 1);
                for &i in &self.attr_indices {
                    let field = self.schema.field(i);
                    let v = arrow_to_sql_value(field, batch.column(i).as_ref(), row, self.on_loss)?;
                    params.push(v);
                }
                // geometry: SpatiaLite 内蔵 GeomFromWKB(?, srid) に WKB を渡す。
                // SpatiaLite 自身が型・SRID を validate して内部 blob に encode する。
                let geom_arr = batch.column(self.geom_index).as_binary::<i32>();
                let geom_param: SqlValue = if geom_arr.is_null(row) {
                    SqlValue::Null
                } else {
                    SqlValue::Blob(geom_arr.value(row).to_vec())
                };
                params.push(geom_param);
                stmt.execute(params_from_iter(params))
                    .map_err(|e| driver_err(&e))?;
            }
        }
        tx.commit().map_err(|e| driver_err(&e))?;
        Ok(())
    }

    fn finish(mut self: Box<Self>) -> Result<()> {
        let conn = self
            .conn
            .take()
            .ok_or_else(|| driver_msg("finish called twice"))?;
        let _ = conn.execute_batch("PRAGMA optimize");
        conn.close().map_err(|(_, e)| driver_err(&e))?;
        Ok(())
    }
}

impl Drop for SpatialiteWriter {
    fn drop(&mut self) {
        if self.conn.is_some() {
            tracing::warn!(target: "shpx::spatialite", "SpatialiteWriter dropped without finish()");
        }
    }
}

/// `Crs` から SRID を確定し、必要なら `spatial_ref_sys` に best-effort INSERT する。
///
/// v0.5 cycle 1 では:
/// - EPSG コードがあれば、そのコードを SRID として返す（FastInit シードに含まれる WGS84 系
///   なら spatial_ref_sys に既に存在、それ以外は cycle 2 の自動 INSERT で対応）。
/// - EPSG なし → on_loss を発火して SRID 0 (unknown)。
fn register_srs(
    conn: &Connection,
    geom_meta: &GeometryMeta,
    crs_arg: Option<&Crs>,
    on_loss: OnLoss,
) -> Result<i32> {
    let crs: Option<Crs> = crs_arg.cloned().or_else(|| geom_meta.crs.clone());

    match crs {
        None => {
            let _ = apply_on_loss(loss_kind::MISSING_CRS_ON_SPATIALITE, "<srs>", on_loss)?;
            Ok(0)
        }
        Some(c) => {
            if let Some(code) = c.epsg_code() {
                let code_i32 = i32::try_from(code)
                    .map_err(|_| Error::Crs(format!("EPSG code {code} exceeds i32 range")))?;
                // EPSG 行が無ければ best-effort で INSERT (FastInit に含まれない CRS 用)。
                // 失敗しても geometry 列は valid なまま動くので無視する。
                let definition_wkt = c
                    .wkt
                    .clone()
                    .or_else(|| shpx_geom::epsg_to_wkt1(code).map(str::to_string))
                    .unwrap_or_default();
                let proj4 = String::new();
                let srs_name = format!("EPSG:{code}");
                let _ = conn.execute(
                    meta::SQL_INSERT_SRS,
                    rusqlite::params![code_i32, "EPSG", code_i32, srs_name, proj4, definition_wkt],
                );
                Ok(code_i32)
            } else {
                // EPSG なし: WKT のみ CRS は cycle 2 で `spatial_ref_sys` への shpx 採番 INSERT
                // で対応する。cycle 1 では fallback として on_loss を発火。
                let _ = apply_on_loss(loss_kind::MISSING_CRS_ON_SPATIALITE, "<srs>", on_loss)?;
                Ok(0)
            }
        }
    }
}

fn build_create_table_sql(
    schema: &SchemaRef,
    attr_indices: &[usize],
    table: &str,
) -> Result<String> {
    let mut cols: Vec<String> = Vec::with_capacity(attr_indices.len() + 1);
    cols.push(format!(
        "{} INTEGER PRIMARY KEY AUTOINCREMENT",
        quote_ident(ID_COLUMN)
    ));
    for &i in attr_indices {
        let f = schema.field(i);
        let decl = type_map::arrow_to_decl(f.data_type())?;
        cols.push(format!("{} {}", quote_ident(f.name()), decl));
    }
    Ok(format!(
        "CREATE TABLE {} (\n  {}\n)",
        quote_ident(table),
        cols.join(",\n  ")
    ))
}

fn build_insert_sql(
    schema: &SchemaRef,
    attr_indices: &[usize],
    geom_index: usize,
    table: &str,
    srid: i32,
) -> String {
    let mut col_names: Vec<String> = attr_indices
        .iter()
        .map(|&i| quote_ident(schema.field(i).name()))
        .collect();
    col_names.push(quote_ident(schema.field(geom_index).name()));
    let n_attrs = attr_indices.len();
    let placeholders: Vec<String> = (1..=n_attrs).map(|i| format!("?{i}")).collect();
    let geom_placeholder = format!("GeomFromWKB(?{}, {srid})", n_attrs + 1);
    let mut all = placeholders;
    all.push(geom_placeholder);
    format!(
        "INSERT INTO {} ({}) VALUES ({})",
        quote_ident(table),
        col_names.join(", "),
        all.join(", ")
    )
}

fn sanitize_table_name(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_ascii_alphanumeric() || c == '_' {
            out.push(c);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        return out;
    }
    if out.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        out.insert(0, '_');
    }
    out
}

fn epoch() -> NaiveDate {
    NaiveDate::from_ymd_opt(1970, 1, 1).expect("1970-01-01 valid")
}

fn arrow_to_sql_value(
    field: &Field,
    array: &dyn Array,
    row: usize,
    on_loss: OnLoss,
) -> Result<SqlValue> {
    if array.is_null(row) {
        return Ok(SqlValue::Null);
    }
    let name = field.name();
    Ok(match field.data_type() {
        DataType::Boolean => SqlValue::Integer(i64::from(array.as_boolean().value(row))),
        DataType::Int8 => SqlValue::Integer(i64::from(primitive::<Int8Type>(array, row))),
        DataType::Int16 => SqlValue::Integer(i64::from(primitive::<Int16Type>(array, row))),
        DataType::Int32 => SqlValue::Integer(i64::from(primitive::<Int32Type>(array, row))),
        DataType::Int64 => SqlValue::Integer(primitive::<Int64Type>(array, row)),
        DataType::UInt8 => SqlValue::Integer(i64::from(primitive::<UInt8Type>(array, row))),
        DataType::UInt16 => SqlValue::Integer(i64::from(primitive::<UInt16Type>(array, row))),
        DataType::UInt32 => SqlValue::Integer(i64::from(primitive::<UInt32Type>(array, row))),
        DataType::UInt64 => {
            let v = primitive::<UInt64Type>(array, row);
            if v > i64::MAX as u64 {
                let _ = apply_on_loss(loss_kind::UINT64_OVERFLOW_ON_SPATIALITE, name, on_loss)?;
                if matches!(on_loss, OnLoss::Skip) {
                    SqlValue::Null
                } else {
                    SqlValue::Integer(i64::MAX)
                }
            } else {
                #[allow(clippy::cast_possible_wrap)]
                SqlValue::Integer(v as i64)
            }
        }
        DataType::Float16 => {
            SqlValue::Real(f64::from(primitive::<Float16Type>(array, row).to_f32()))
        }
        DataType::Float32 => SqlValue::Real(f64::from(primitive::<Float32Type>(array, row))),
        DataType::Float64 => SqlValue::Real(primitive::<Float64Type>(array, row)),
        DataType::Decimal128(_p, s) => {
            let _ = apply_on_loss(loss_kind::DECIMAL_ON_SPATIALITE, name, on_loss)?;
            SqlValue::Text(format_decimal128(
                primitive::<Decimal128Type>(array, row),
                *s,
            ))
        }
        DataType::Utf8 => SqlValue::Text(array.as_string::<i32>().value(row).to_string()),
        DataType::LargeUtf8 => SqlValue::Text(array.as_string::<i64>().value(row).to_string()),
        DataType::Binary => SqlValue::Blob(array.as_binary::<i32>().value(row).to_vec()),
        DataType::LargeBinary => SqlValue::Blob(array.as_binary::<i64>().value(row).to_vec()),
        DataType::Date32 => {
            let days = primitive::<Date32Type>(array, row);
            let nd = epoch() + Duration::days(i64::from(days));
            SqlValue::Text(nd.format("%Y-%m-%d").to_string())
        }
        DataType::Date64 => {
            let ms = primitive::<Date64Type>(array, row);
            let nd = epoch() + Duration::milliseconds(ms);
            SqlValue::Text(nd.format("%Y-%m-%d").to_string())
        }
        DataType::Timestamp(unit, tz) => {
            SqlValue::Text(format_timestamp(array, row, *unit, tz.as_deref(), name)?)
        }
        other => {
            return Err(Error::Schema(format!(
                "field `{name}`: unsupported Arrow type for SpatiaLite writer: {other:?}"
            )))
        }
    })
}

fn primitive<T: ArrowPrimitiveType>(array: &dyn Array, row: usize) -> T::Native {
    let arr = array
        .as_any()
        .downcast_ref::<PrimitiveArray<T>>()
        .expect("primitive downcast");
    arr.value(row)
}

fn format_decimal128(value: i128, scale: i8) -> String {
    if scale <= 0 {
        return value.to_string();
    }
    #[allow(clippy::cast_sign_loss)]
    let s = scale as usize;
    let neg = value < 0;
    let abs = if neg { -value } else { value };
    let str_abs = abs.to_string();
    let (whole, frac) = if str_abs.len() > s {
        let split = str_abs.len() - s;
        (&str_abs[..split], &str_abs[split..])
    } else {
        ("0", str_abs.as_str())
    };
    let frac_padded = format!("{frac:0>s$}");
    if neg {
        format!("-{whole}.{frac_padded}")
    } else {
        format!("{whole}.{frac_padded}")
    }
}

fn format_timestamp(
    array: &dyn Array,
    row: usize,
    unit: TimeUnit,
    tz: Option<&str>,
    field: &str,
) -> Result<String> {
    let micros: i64 = match unit {
        TimeUnit::Second => primitive::<TimestampSecondType>(array, row)
            .checked_mul(1_000_000)
            .ok_or_else(|| driver_msg(format!("field `{field}`: timestamp seconds overflow")))?,
        TimeUnit::Millisecond => primitive::<TimestampMillisecondType>(array, row)
            .checked_mul(1_000)
            .ok_or_else(|| driver_msg(format!("field `{field}`: timestamp ms overflow")))?,
        TimeUnit::Microsecond => primitive::<TimestampMicrosecondType>(array, row),
        TimeUnit::Nanosecond => primitive::<TimestampNanosecondType>(array, row) / 1_000,
    };
    let secs = micros.div_euclid(1_000_000);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let nanos = (micros.rem_euclid(1_000_000) * 1_000) as u32;
    let utc_dt: DateTime<Utc> = Utc
        .timestamp_opt(secs, nanos)
        .single()
        .ok_or_else(|| driver_msg(format!("field `{field}`: invalid timestamp")))?;
    match tz {
        None => Ok(utc_dt.format("%Y-%m-%dT%H:%M:%S%.6fZ").to_string()),
        Some(tz_str) if tz_str.eq_ignore_ascii_case("UTC") || tz_str == "Z" => {
            Ok(utc_dt.format("%Y-%m-%dT%H:%M:%S%.6fZ").to_string())
        }
        Some(tz_str) => {
            if let Ok(off) = tz_str.parse::<FixedOffset>() {
                let local = utc_dt.with_timezone(&off);
                Ok(local.format("%Y-%m-%dT%H:%M:%S%.6f%:z").to_string())
            } else {
                tracing::warn!(
                    target: "shpx::spatialite",
                    field, tz = tz_str,
                    "non-offset timestamp tz; writing as UTC"
                );
                Ok(utc_dt.format("%Y-%m-%dT%H:%M:%S%.6fZ").to_string())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use super::*;
    use arrow_schema::Schema;
    use shpx_core::schema::{GeometryMeta, GeometryType, GEOMETRY_META_KEY};

    #[test]
    fn sanitize_table_name_replaces_special_chars() {
        assert_eq!(sanitize_table_name("my-data.sqlite"), "my_data_sqlite");
        assert_eq!(sanitize_table_name("hello world"), "hello_world");
        assert_eq!(sanitize_table_name("123abc"), "_123abc");
        assert_eq!(sanitize_table_name(""), "");
    }

    #[test]
    fn format_decimal128_handles_scale() {
        assert_eq!(format_decimal128(12345, 2), "123.45");
        assert_eq!(format_decimal128(-12345, 2), "-123.45");
        assert_eq!(format_decimal128(5, 3), "0.005");
        assert_eq!(format_decimal128(123, 0), "123");
    }

    #[test]
    fn build_create_table_sql_has_fid_first() {
        let mut g = Field::new("geom", DataType::Binary, true);
        let mut m = HashMap::new();
        let geom_meta = GeometryMeta::wkb(GeometryType::Point, None);
        m.insert(GEOMETRY_META_KEY.to_string(), geom_meta.to_json().unwrap());
        g.set_metadata(m);
        let schema = Arc::new(Schema::new(vec![
            Field::new("name", DataType::Utf8, true),
            Field::new("count", DataType::Int64, true),
            g,
        ]));
        let attr_indices = vec![0, 1];
        let sql = build_create_table_sql(&schema, &attr_indices, "places").unwrap();
        assert!(sql.contains("\"fid\" INTEGER PRIMARY KEY AUTOINCREMENT"));
        assert!(sql.contains("\"name\" TEXT"));
        assert!(sql.contains("\"count\" INTEGER"));
        // SpatiaLite では geometry 列を CREATE TABLE で宣言しない (AddGeometryColumn で追加)。
        assert!(!sql.contains("\"geom\""));
    }

    #[test]
    fn build_insert_sql_uses_geomfromwkb() {
        let mut g = Field::new("geom", DataType::Binary, true);
        let mut m = HashMap::new();
        let geom_meta = GeometryMeta::wkb(GeometryType::Point, None);
        m.insert(GEOMETRY_META_KEY.to_string(), geom_meta.to_json().unwrap());
        g.set_metadata(m);
        let schema = Arc::new(Schema::new(vec![
            Field::new("name", DataType::Utf8, true),
            g,
        ]));
        let sql = build_insert_sql(&schema, &[0], 1, "places", 4326);
        assert!(sql.contains("GeomFromWKB(?2, 4326)"));
        assert!(sql.contains("\"name\""));
        assert!(sql.contains("\"geom\""));
    }
}
