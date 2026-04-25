//! GeoPackage の `LayerWriter` 実装。
//!
//! - open() でファイル作成 → メタテーブル初期化 → SRS 登録 → CREATE TABLE。
//! - write_batch() は 1 トランザクションで全行 INSERT。
//! - finish() で gpkg_contents.bbox を反映してクローズ。
//!
//! トランザクション粒度を「`write_batch` 単位」にしている。`batch_size_hint` は呼び出し側
//! pipeline が概ね 4K〜65K 行で来る前提で、この粒度なら tempdb / WAL を圧迫しない。
//! 真の bulk load (10K 行ごと commit) は v0.3 以降で `BulkLoadWriter` を実装する。

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
use shpx_geom::{
    gpkg_blob::{self, GpkgBlobHeader},
    wkb,
};

use crate::conn;
use crate::meta::{self, BBox};
use crate::options::{strip_query, ResolvedWriteOpts};
use crate::type_map;
use crate::util::{apply_on_loss, driver_err, driver_msg, loss_kind, quote_ident};

const ID_COLUMN: &str = "fid";

pub struct GpkgWriter {
    conn: Option<Connection>,
    schema: SchemaRef,
    table: String,
    geom_index: usize,
    /// 属性列（geometry 以外）の Arrow 列インデックスと metadata。
    attr_indices: Vec<usize>,
    insert_sql: String,
    srs_id: i32,
    bbox: BBox,
    on_loss: OnLoss,
}

impl GpkgWriter {
    pub fn open(uri: &Uri, schema: SchemaRef, crs: Option<&Crs>, opts: &WriteOpts) -> Result<Self> {
        let resolved = ResolvedWriteOpts::resolve(uri, opts)?;
        let path = PathBuf::from(strip_query(uri.path()));
        let table = resolved
            .table
            .clone()
            .or_else(|| {
                // テーブル名未指定 → ファイル名 stem を使う。`my_data.gpkg` → `my_data`。
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
            // WAL/SHM サイドカーが残っていると次回オープン時に旧 DB がリストアされうる。
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
        init_meta_tables(&conn)?;

        let (geom_index, _, geom_meta) = find_geometry_column(&schema)?
            .ok_or_else(|| Error::Schema("no geometry column for GPKG writer".to_string()))?;
        let geom_column = schema.field(geom_index).name().clone();
        let srs_id = register_srs(&conn, &geom_meta, crs, opts.on_loss)?;

        // attribute 列インデックス（geometry を除外）。
        let attr_indices: Vec<usize> = (0..schema.fields().len())
            .filter(|i| *i != geom_index)
            .collect();

        // CREATE TABLE。`fid INTEGER PRIMARY KEY AUTOINCREMENT` を先頭に置くのが GPKG 仕様。
        let create_sql = build_create_table_sql(&schema, &attr_indices, geom_index, &table)?;
        conn.execute(&create_sql, []).map_err(|e| driver_err(&e))?;

        // gpkg_contents / gpkg_geometry_columns へ登録。
        let identifier = table.clone();
        conn.execute(
            meta::SQL_INSERT_CONTENTS,
            rusqlite::params![identifier, "", srs_id],
        )
        .map_err(|e| driver_err(&e))?;
        conn.execute(
            meta::SQL_INSERT_GEOMETRY_COLUMN,
            rusqlite::params![
                table,
                geom_column,
                type_map::geom_type_to_name(geom_meta.geometry_type),
                srs_id
            ],
        )
        .map_err(|e| driver_err(&e))?;

        let insert_sql = build_insert_sql(&schema, &attr_indices, geom_index, &table);

        Ok(Self {
            conn: Some(conn),
            schema,
            table,
            geom_index,
            attr_indices,
            insert_sql,
            srs_id,
            bbox: BBox::default(),
            on_loss: opts.on_loss,
        })
    }
}

impl LayerWriter for GpkgWriter {
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
                // geometry blob: WKB を GPKG header で包む。
                let geom_arr = batch.column(self.geom_index).as_binary::<i32>();
                let geom_param: SqlValue = if geom_arr.is_null(row) {
                    SqlValue::Null
                } else {
                    let wkb_bytes = geom_arr.value(row);
                    let g = wkb::decode(wkb_bytes)?;
                    self.bbox.update_geom(&g);
                    let header = GpkgBlobHeader::standard(self.srs_id);
                    SqlValue::Blob(gpkg_blob::encode(&header, wkb_bytes))
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

        if self.bbox.is_initialized() {
            conn.execute(
                meta::SQL_UPDATE_CONTENTS_BBOX,
                rusqlite::params![
                    self.table,
                    self.bbox.min_x,
                    self.bbox.min_y,
                    self.bbox.max_x,
                    self.bbox.max_y
                ],
            )
            .map_err(|e| driver_err(&e))?;
        }
        // PRAGMA optimize は SQLite 推奨のクローズ前手続き。失敗しても致命ではない。
        let _ = conn.execute_batch("PRAGMA optimize");
        conn.close().map_err(|(_, e)| driver_err(&e))?;
        Ok(())
    }
}

impl Drop for GpkgWriter {
    fn drop(&mut self) {
        if self.conn.is_some() {
            tracing::warn!(target: "shpx::gpkg", "GpkgWriter dropped without finish()");
        }
    }
}

fn init_meta_tables(conn: &Connection) -> Result<()> {
    conn.execute_batch(meta::SQL_CREATE_GPKG_SPATIAL_REF_SYS)
        .map_err(|e| driver_err(&e))?;
    conn.execute_batch(meta::SQL_CREATE_GPKG_CONTENTS)
        .map_err(|e| driver_err(&e))?;
    conn.execute_batch(meta::SQL_CREATE_GPKG_GEOMETRY_COLUMNS)
        .map_err(|e| driver_err(&e))?;
    // 仕様必須 SRS（-1, 0, 4326）。4326 の definition は WKT1 を流し込む。
    let wkt = shpx_geom::epsg_to_wkt1(4326).unwrap_or("GEOGCS[\"WGS 84\"]");
    conn.execute(meta::SQL_INSERT_REQUIRED_SRS, rusqlite::params![wkt])
        .map_err(|e| driver_err(&e))?;
    Ok(())
}

/// Crs から gpkg_spatial_ref_sys.srs_id を確定し、必要なら新規行を INSERT する。
fn register_srs(
    conn: &Connection,
    geom_meta: &GeometryMeta,
    crs_arg: Option<&Crs>,
    on_loss: OnLoss,
) -> Result<i32> {
    // open_write 経路で渡される明示 CRS を最優先、なければ schema metadata の CRS。
    let crs: Option<Crs> = crs_arg.cloned().or_else(|| geom_meta.crs.clone());

    match crs {
        None => {
            // CRS 不明: srs_id=0 (Undefined geographic) を使う。on_loss=error なら停止。
            let _ = apply_on_loss(loss_kind::MISSING_CRS_ON_GPKG, "<srs>", on_loss)?;
            Ok(0)
        }
        Some(c) => {
            if let Some(code) = c.epsg_code() {
                let code_i32 = i32::try_from(code)
                    .map_err(|_| Error::Crs(format!("EPSG code {code} exceeds i32 range")))?;
                // 既存行があれば INSERT OR IGNORE で何もしない。definition が衝突しても触らない。
                let definition = c
                    .wkt
                    .clone()
                    .or_else(|| shpx_geom::epsg_to_wkt1(code).map(str::to_string))
                    .unwrap_or_else(|| "undefined".to_string());
                let srs_name = format!("EPSG:{code}");
                conn.execute(
                    meta::SQL_INSERT_SRS,
                    rusqlite::params![
                        srs_name,
                        code_i32,
                        meta::ORG_EPSG,
                        code_i32,
                        definition,
                        ""
                    ],
                )
                .map_err(|e| driver_err(&e))?;
                Ok(code_i32)
            } else if let Some(wkt) = c.wkt.clone() {
                // EPSG なし、WKT のみ → 採番した srs_id で `organization='shpx'` 行を新規追加する。
                let next: i32 = conn
                    .query_row(
                        "SELECT COALESCE(MAX(srs_id), 0) + 1 FROM gpkg_spatial_ref_sys WHERE srs_id > 4326",
                        [],
                        |row| row.get(0),
                    )
                    .map_err(|e| driver_err(&e))?;
                let next = next.max(100_000);
                conn.execute(
                    meta::SQL_INSERT_SRS,
                    rusqlite::params![
                        "shpx custom SRS",
                        next,
                        meta::ORG_SHPX,
                        next,
                        wkt,
                        "WKT-only CRS preserved by shpx"
                    ],
                )
                .map_err(|e| driver_err(&e))?;
                Ok(next)
            } else {
                let _ = apply_on_loss(loss_kind::MISSING_CRS_ON_GPKG, "<srs>", on_loss)?;
                Ok(0)
            }
        }
    }
}

fn build_create_table_sql(
    schema: &SchemaRef,
    attr_indices: &[usize],
    geom_index: usize,
    table: &str,
) -> Result<String> {
    let mut cols: Vec<String> = Vec::with_capacity(attr_indices.len() + 2);
    // GPKG 仕様: feature テーブルは整数 PK 列を持たなければならない。`fid` を予約名として固定する。
    cols.push(format!(
        "{} INTEGER PRIMARY KEY AUTOINCREMENT",
        quote_ident(ID_COLUMN)
    ));
    for &i in attr_indices {
        let f = schema.field(i);
        let decl = type_map::arrow_to_decl(f.data_type())?;
        cols.push(format!("{} {}", quote_ident(f.name()), decl));
    }
    let geom_field = schema.field(geom_index);
    cols.push(format!("{} BLOB", quote_ident(geom_field.name())));
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
) -> String {
    let mut col_names: Vec<String> = attr_indices
        .iter()
        .map(|&i| quote_ident(schema.field(i).name()))
        .collect();
    col_names.push(quote_ident(schema.field(geom_index).name()));
    let placeholders: Vec<String> = (1..=col_names.len()).map(|i| format!("?{i}")).collect();
    format!(
        "INSERT INTO {} ({}) VALUES ({})",
        quote_ident(table),
        col_names.join(", "),
        placeholders.join(", ")
    )
}

/// テーブル名として安全な文字に正規化する（GPKG 仕様で識別子に許される範囲に揃える）。
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
        // SQLite は識別子先頭の数字を許すが、GPKG 仕様準拠のために _ プレフィクス。
        out.insert(0, '_');
    }
    out
}

fn epoch() -> NaiveDate {
    NaiveDate::from_ymd_opt(1970, 1, 1).expect("1970-01-01 valid")
}

/// Arrow 値を rusqlite `Value` に変換する。型不一致は `Error::Schema` で停止。
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
                let _ = apply_on_loss(loss_kind::UINT64_OVERFLOW_ON_GPKG, name, on_loss)?;
                // Warn 経路: i64::MAX に飽和（roundtrip 不能だが続行）。Skip 経路: NULL。
                if matches!(on_loss, OnLoss::Skip) {
                    SqlValue::Null
                } else {
                    SqlValue::Integer(i64::MAX)
                }
            } else {
                // 上のガードで `v <= i64::MAX as u64` が確定しているので wrap しない。
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
            let _ = apply_on_loss(loss_kind::DECIMAL_ON_GPKG, name, on_loss)?;
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
                "field `{name}`: unsupported Arrow type for GPKG writer: {other:?}"
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
    // 上のガードで scale > 0 が確定しているので符号落ちしない。
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
    // rem_euclid(1_000_000) は 0..1_000_000 の範囲なので * 1_000 しても 1_000_000_000 未満、
    // u32 (max 4_294_967_295) に収まる。
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
            // TZ 文字列を `+HH:MM` などとして解釈する。chrono::TimeZone の
            // 全タイムゾーン解決は重いため、本実装では「offset 表記」のみサポートし、
            // それ以外の名前付き TZ は UTC + 警告として書き出す。
            if let Ok(off) = tz_str.parse::<FixedOffset>() {
                let local = utc_dt.with_timezone(&off);
                Ok(local.format("%Y-%m-%dT%H:%M:%S%.6f%:z").to_string())
            } else {
                tracing::warn!(
                    target: "shpx::gpkg",
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

    #[test]
    fn sanitize_table_name_replaces_special_chars() {
        assert_eq!(sanitize_table_name("my-data.gpkg"), "my_data_gpkg");
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
        use arrow_schema::Schema;
        let mut g = Field::new("geom", DataType::Binary, true);
        let mut m = HashMap::new();
        let geom_meta = GeometryMeta::wkb(shpx_core::schema::GeometryType::Point, None);
        m.insert(
            shpx_core::schema::GEOMETRY_META_KEY.to_string(),
            geom_meta.to_json().unwrap(),
        );
        g.set_metadata(m);
        let schema = Arc::new(Schema::new(vec![
            Field::new("name", DataType::Utf8, true),
            Field::new("count", DataType::Int64, true),
            g,
        ]));
        let attr_indices = vec![0, 1];
        let sql = build_create_table_sql(&schema, &attr_indices, 2, "places").unwrap();
        assert!(sql.contains("\"fid\" INTEGER PRIMARY KEY AUTOINCREMENT"));
        assert!(sql.contains("\"name\" TEXT"));
        assert!(sql.contains("\"count\" INTEGER"));
        assert!(sql.contains("\"geom\" BLOB"));
    }
}
