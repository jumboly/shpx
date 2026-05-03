//! SpatiaLite の `LayerReader` 実装。
//!
//! ストリーミング戦略: `shpx_rdb_common::streaming::{KeysetRowsIter, OffsetRowsIter}`
//! 経由の真のストリーミング読みで、モードによって 2 種類の iterator を使い分ける。
//!
//! - **table モード** (`?table=...` / `--where` / `--select`): rowid keyset pagination
//!   (`SELECT ..., rowid FROM <table> WHERE rowid > ? ORDER BY rowid LIMIT ?`)。
//!   WITHOUT ROWID テーブルは v0.8 では明示エラー (rowid を持たないため)。
//! - **query モード** (`--query '<sql>'`): 任意 SQL を `LIMIT/OFFSET` でページングする
//!   `OffsetRowsIter` fallback。大きい OFFSET でスキャンが遅くなるため、巨大テーブルは
//!   table モードを推奨。
//!
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
use rusqlite::{
    types::{Value, ValueRef},
    Connection,
};
use shpx_core::{
    schema::{GeometryMeta, GeometryType, GEOMETRY_META_KEY},
    Crs, Error, LayerReader, ReadOpts, Result, Uri, WktFlavor,
};
use shpx_rdb_common::streaming::{KeysetRowsIter, OffsetRowsIter, RowBatch};

use crate::conn;
use crate::meta;
use crate::options::{strip_to_filepath, validate_user_query, ResolvedReadOpts};
use crate::type_map;
use crate::util::{driver_err, driver_msg, quote_ident, DRIVER_NAME};

const READ_BATCH_SIZE: usize = 65_536;

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

#[derive(Debug, Clone)]
struct ColumnPlan {
    name: String,
    arrow_type: DataType,
}

/// streaming pagination モード。`batches()` の分岐を表現する。
enum StreamMode {
    /// table モード: rowid keyset。SQL は `SELECT cols, geom, rowid FROM table WHERE ... AND rowid > ?1 ORDER BY rowid LIMIT ?2`。
    /// `geom_idx_in_row` は `RowBatch.rows[i]` の中での geometry 列の位置 (= columns.len())。
    Keyset {
        sql_template: String,
        geom_idx_in_row: usize,
    },
    /// query モード: LIMIT/OFFSET pagination。SQL は `SELECT * FROM (user_query) AS shpx_q LIMIT ?1 OFFSET ?2`。
    /// `geom_idx_in_row` は probe で確定した geometry 列の位置。
    Offset {
        sql_template: String,
        geom_idx_in_row: usize,
    },
}

pub struct SpatialiteReader {
    schema: SchemaRef,
    crs: Option<Crs>,
    columns: Vec<ColumnPlan>,
    geom_column: String,
    row_count: Option<usize>,
    mode: StreamMode,
    conn: Connection,
}

impl SpatialiteReader {
    pub fn open(uri: &Uri, opts: &ReadOpts) -> Result<Self> {
        if opts.query.is_some() && (opts.where_clause.is_some() || opts.select.is_some()) {
            return Err(driver_msg(format!(
                "{DRIVER_NAME}: --query is exclusive with --where / --select"
            )));
        }
        // 接続前に SQL 形状だけ早期バリデーション（`;` 混入は実行不可なので即エラー）。
        if let Some(q) = opts.query.as_deref() {
            validate_user_query(q)?;
        }

        let path = PathBuf::from(strip_to_filepath(uri.path()));
        let conn = conn::open_read(&path)?;

        if let Some(query) = opts.query.as_deref() {
            Self::open_query_mode(conn, query, opts)
        } else {
            let resolved = ResolvedReadOpts::resolve(uri, opts)?;
            Self::open_table_mode(conn, &resolved, opts)
        }
    }

    fn open_table_mode(
        conn: Connection,
        resolved: &ResolvedReadOpts,
        opts: &ReadOpts,
    ) -> Result<Self> {
        let table = resolve_table_name(&conn, resolved.table.as_deref())?;
        // WITHOUT ROWID テーブルは rowid を持たないため keyset pagination 不可。
        // v0.8 streaming は明示エラーで弾く (LIMIT/OFFSET fallback はリスクが大きいので未採用)。
        if is_without_rowid_table(&conn, &table)? {
            return Err(driver_msg(format!(
                "{DRIVER_NAME}: table `{table}` is WITHOUT ROWID; not supported by v0.8 streaming reader. Use --query for arbitrary SQL or rebuild the table with rowid"
            )));
        }
        let (geom_column, geom_type_int, srid) = read_geometry_column(&conn, &table)?;

        let crs = match &resolved.src_crs {
            Some(c) => Some(c.clone()),
            None => read_crs_for_srid(&conn, srid)?,
        };

        let all_columns = read_attribute_schema(&conn, &table, &geom_column)?;
        let columns = match opts.select.as_deref() {
            None => all_columns,
            Some(names) => filter_columns_by_select_with_geom(&all_columns, names, &geom_column)?,
        };
        let geom_type = meta::geom_type_from_int(geom_type_int);
        let schema = build_schema(&columns, &geom_column, geom_type, crs.as_ref())?;

        let row_count = count_rows_table(&conn, &table, opts.where_clause.as_deref())?;
        let sql_template =
            build_table_keyset_sql(&table, &columns, &geom_column, opts.where_clause.as_deref());
        let geom_idx_in_row = columns.len();

        Ok(Self {
            schema,
            crs,
            columns,
            geom_column,
            row_count: Some(row_count),
            mode: StreamMode::Keyset {
                sql_template,
                geom_idx_in_row,
            },
            conn,
        })
    }

    fn open_query_mode(conn: Connection, user_query: &str, opts: &ReadOpts) -> Result<Self> {
        // ユーザ SQL を `LIMIT 1` でサブクエリ化し、(列名, 1 行目の値) から schema を
        // 推定する。0 行ヒット時は型推定不能のため明示エラー。geometry 列は probe ループ内
        // で「最初に spatialite_blob として decode できた BLOB 列」と判定し、SRID と
        // geometry 型もこの 1 行から取り出して本番での再 decode を避ける。
        let probe_sql = format!("SELECT * FROM ({user_query}) AS shpx_q LIMIT 1");
        let mut stmt = conn.prepare(&probe_sql).map_err(|e| driver_err(&e))?;
        let names: Vec<String> = stmt
            .column_names()
            .iter()
            .map(|s| (*s).to_string())
            .collect();

        let mut rows_iter = stmt.query([]).map_err(|e| driver_err(&e))?;
        let first_row = rows_iter
            .next()
            .map_err(|e| driver_err(&e))?
            .ok_or_else(|| {
                driver_msg(format!(
                    "{DRIVER_NAME}: --query returned 0 rows; cannot infer schema. Add a sample row or use a --where on the table"
                ))
            })?;

        let mut probe_kinds: Vec<ProbeKind> = Vec::with_capacity(names.len());
        let mut geom_info: Option<(usize, i32, GeometryType)> = None;
        for i in 0..names.len() {
            let v = first_row.get_ref(i).map_err(|e| driver_err(&e))?;
            // 最初の decodable BLOB を geometry 列として確定。bytes は ValueRef のスコープ
            // 内で消費し、所有コピーは作らない (複数 BLOB 列がある場合のメモリ削減)。
            if geom_info.is_none() {
                if let ValueRef::Blob(b) = v {
                    if let Ok((srid, wkb_bytes)) = shpx_geom::spatialite_blob::decode(b) {
                        let gt =
                            infer_geom_type_from_wkb(&wkb_bytes).unwrap_or(GeometryType::Geometry);
                        geom_info = Some((i, srid, gt));
                    }
                }
            }
            probe_kinds.push(ProbeKind::from(&v));
        }
        // probe stmt の borrow を切る。本番 SELECT は OffsetRowsIter で再 prepare する。
        drop(rows_iter);
        drop(stmt);

        let (geom_idx_in_row, geom_srid, geom_type) = geom_info.ok_or_else(|| {
            driver_msg(format!(
                "{DRIVER_NAME}: --query result does not contain a SpatiaLite geometry column"
            ))
        })?;
        let geom_column = names[geom_idx_in_row].clone();

        // 属性 schema (geometry 列を除く)。
        let mut columns: Vec<ColumnPlan> = Vec::with_capacity(names.len() - 1);
        for (i, name) in names.iter().enumerate() {
            if i == geom_idx_in_row {
                continue;
            }
            columns.push(ColumnPlan {
                name: name.clone(),
                arrow_type: probe_kinds[i].infer_arrow_type(),
            });
        }

        let crs = match &opts.src_crs {
            Some(c) => Some(c.clone()),
            None => read_crs_for_srid(&conn, geom_srid)?,
        };
        let schema = build_schema(&columns, &geom_column, geom_type, crs.as_ref())?;

        // query モードでは row_count は事前に取れない (任意 SQL の COUNT(*) は副作用化しうる)
        let sql_template = format!("SELECT * FROM ({user_query}) AS shpx_q LIMIT ?1 OFFSET ?2");

        Ok(Self {
            schema,
            crs,
            columns,
            geom_column,
            row_count: None,
            mode: StreamMode::Offset {
                sql_template,
                geom_idx_in_row,
            },
            conn,
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
        self.row_count
    }

    fn batches(&mut self) -> Box<dyn Iterator<Item = Result<RecordBatch>> + Send + '_> {
        let columns = self.columns.clone();
        let geom_column = self.geom_column.clone();
        let schema = self.schema.clone();
        match &self.mode {
            StreamMode::Keyset {
                sql_template,
                geom_idx_in_row,
            } => {
                let inner = KeysetRowsIter::new(
                    &mut self.conn,
                    sql_template.clone(),
                    READ_BATCH_SIZE,
                    DRIVER_NAME,
                );
                Box::new(KeysetBatchIter {
                    inner,
                    columns,
                    geom_column,
                    schema,
                    geom_idx_in_row: *geom_idx_in_row,
                })
            }
            StreamMode::Offset {
                sql_template,
                geom_idx_in_row,
            } => {
                let inner = OffsetRowsIter::new(
                    &mut self.conn,
                    sql_template.clone(),
                    READ_BATCH_SIZE,
                    DRIVER_NAME,
                );
                Box::new(OffsetBatchIter {
                    inner,
                    columns,
                    geom_column,
                    schema,
                    geom_idx_in_row: *geom_idx_in_row,
                })
            }
        }
    }
}

struct KeysetBatchIter<'a> {
    inner: KeysetRowsIter<'a>,
    columns: Vec<ColumnPlan>,
    geom_column: String,
    schema: SchemaRef,
    /// `RowBatch.rows[i]` の中で geometry 列が居るインデックス。table モードでは
    /// columns.len() (末尾、rowid は KeysetRowsIter が内部消費)。
    geom_idx_in_row: usize,
}

impl Iterator for KeysetBatchIter<'_> {
    type Item = Result<RecordBatch>;
    fn next(&mut self) -> Option<Self::Item> {
        match self.inner.next_batch() {
            Ok(Some(rb)) => Some(build_record_batch(
                rb,
                &self.columns,
                &self.geom_column,
                &self.schema,
                self.geom_idx_in_row,
            )),
            Ok(None) => None,
            Err(e) => Some(Err(e)),
        }
    }
}

struct OffsetBatchIter<'a> {
    inner: OffsetRowsIter<'a>,
    columns: Vec<ColumnPlan>,
    geom_column: String,
    schema: SchemaRef,
    geom_idx_in_row: usize,
}

impl Iterator for OffsetBatchIter<'_> {
    type Item = Result<RecordBatch>;
    fn next(&mut self) -> Option<Self::Item> {
        match self.inner.next_batch() {
            Ok(Some(rb)) => Some(build_record_batch(
                rb,
                &self.columns,
                &self.geom_column,
                &self.schema,
                self.geom_idx_in_row,
            )),
            Ok(None) => None,
            Err(e) => Some(Err(e)),
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

/// CREATE TABLE SQL に `WITHOUT ROWID` 修飾子があるかを判定する。
/// SQLite が sqlite_master.sql にユーザ宣言を保持しているため、文字列マッチで判定可能。
///
/// **既知の偽陽性**: CREATE TABLE 文中のコメント (`-- WITHOUT ROWID for ref`) や、
/// 列名 / リテラルに `WITHOUT ROWID` 文字列が偶然含まれている場合に誤検出する。
/// この場合 streaming reader は open() で reject されるが、SQLite には PRAGMA で
/// WITHOUT ROWID を直接判定する API が無いため (`sqlite_master.sql` テキストマッチが
/// 正攻法)、ユーザは該当の文字列を回避するか driver の table を再構築する必要がある。
fn is_without_rowid_table(conn: &Connection, table: &str) -> Result<bool> {
    let sql = "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1 AND UPPER(sql) LIKE '%WITHOUT ROWID%' LIMIT 1";
    let n: std::result::Result<i64, _> = conn.query_row(sql, [table], |row| row.get(0));
    match n {
        Ok(_) => Ok(true),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(false),
        Err(e) => Err(driver_err(&e)),
    }
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

/// `--select` で指定された列名を `read_attribute_schema` 結果から並べ替えて取り出す。
/// 順序は **`--select` の順** を尊重する (PostGIS と同形)。
/// `--select` には geometry 列名を含めて指定する必要がある。含まれない場合はエラー。
fn filter_columns_by_select_with_geom(
    all: &[ColumnPlan],
    names: &[String],
    geom_column: &str,
) -> Result<Vec<ColumnPlan>> {
    let mut found_geom = false;
    let mut out = Vec::with_capacity(names.len());
    for name in names {
        if name.eq_ignore_ascii_case(geom_column) {
            found_geom = true;
            continue;
        }
        if let Some(c) = all.iter().find(|c| c.name == *name) {
            out.push(c.clone());
        } else {
            let available: Vec<&str> = all.iter().map(|c| c.name.as_str()).collect();
            return Err(driver_msg(format!(
                "{DRIVER_NAME}: --select references unknown column `{name}` (available: {} + geometry `{geom_column}`)",
                available.join(", ")
            )));
        }
    }
    if !found_geom {
        return Err(driver_msg(format!(
            "{DRIVER_NAME}: --select must include the geometry column `{geom_column}`; geometry-less extraction is not supported"
        )));
    }
    Ok(out)
}

/// query モード probe で取得した 1 行目の値タグ (Arrow 型推定用)。geometry 列の
/// 特定と SRID 抽出は呼び出し側で ValueRef のスコープ内に終わらせるため、ここでは
/// bytes を所有しない (複数 BLOB 列がある場合のメモリ削減)。
#[derive(Debug, Clone, Copy)]
enum ProbeKind {
    Null,
    Integer,
    Real,
    Text,
    Blob,
}

impl ProbeKind {
    fn from(v: &ValueRef<'_>) -> Self {
        match v {
            ValueRef::Null => Self::Null,
            ValueRef::Integer(_) => Self::Integer,
            ValueRef::Real(_) => Self::Real,
            ValueRef::Text(_) => Self::Text,
            ValueRef::Blob(_) => Self::Blob,
        }
    }

    /// Null は推定不能のため Utf8 fallback。
    fn infer_arrow_type(self) -> DataType {
        match self {
            Self::Null | Self::Text => DataType::Utf8,
            Self::Integer => DataType::Int64,
            Self::Real => DataType::Float64,
            Self::Blob => DataType::Binary,
        }
    }
}

/// WKB の geometry type code から `GeometryType` を逆引きする。SpatiaLite blob から
/// `spatialite_blob::decode` で取り出した WKB のヘッダ (byte order + uint32) を読む。
fn infer_geom_type_from_wkb(wkb: &[u8]) -> Option<GeometryType> {
    if wkb.len() < 5 {
        return None;
    }
    // byte 0: byte order (0 = big endian, 1 = little endian)
    let little = wkb[0] == 1;
    let bytes = [wkb[1], wkb[2], wkb[3], wkb[4]];
    let code = if little {
        u32::from_le_bytes(bytes)
    } else {
        u32::from_be_bytes(bytes)
    };
    // ベース geometry type のみ抽出 (XY / XYZ / XYM / XYZM の上位ビットを落とす)。
    let base = code % 1000;
    Some(match base {
        1 => GeometryType::Point,
        2 => GeometryType::LineString,
        3 => GeometryType::Polygon,
        4 => GeometryType::MultiPoint,
        5 => GeometryType::MultiLineString,
        6 => GeometryType::MultiPolygon,
        7 => GeometryType::GeometryCollection,
        _ => return None,
    })
}

/// table モード用 keyset SQL を構築する。`--where` がある場合は WHERE (user) AND rowid > ?1。
/// SELECT の列順は columns 順 → geometry 列 → rowid (KeysetRowsIter が末尾を消費)。
fn build_table_keyset_sql(
    table: &str,
    columns: &[ColumnPlan],
    geom_column: &str,
    where_clause: Option<&str>,
) -> String {
    let mut select_cols: Vec<String> = columns.iter().map(|c| quote_ident(&c.name)).collect();
    select_cols.push(quote_ident(geom_column));
    select_cols.push("rowid".to_string());
    match where_clause.map(str::trim).filter(|s| !s.is_empty()) {
        Some(w) => format!(
            "SELECT {} FROM {} WHERE ({w}) AND rowid > ?1 ORDER BY rowid LIMIT ?2",
            select_cols.join(", "),
            quote_ident(table)
        ),
        None => format!(
            "SELECT {} FROM {} WHERE rowid > ?1 ORDER BY rowid LIMIT ?2",
            select_cols.join(", "),
            quote_ident(table)
        ),
    }
}

fn count_rows_table(
    conn: &Connection,
    table: &str,
    where_clause: Option<&str>,
) -> Result<usize> {
    let sql = match where_clause.map(str::trim).filter(|s| !s.is_empty()) {
        Some(w) => format!("SELECT COUNT(*) FROM {} WHERE ({w})", quote_ident(table)),
        None => format!("SELECT COUNT(*) FROM {}", quote_ident(table)),
    };
    let n: i64 = conn
        .query_row(&sql, [], |row| row.get(0))
        .map_err(|e| driver_err(&e))?;
    usize::try_from(n).map_err(|_| driver_msg(format!("row count {n} exceeds usize")))
}

/// `RowBatch` (rusqlite::Value 配列) を Arrow RecordBatch に変換する。
/// table / query モード共通: `geom_idx_in_row` で row 内の geometry 列位置を指す。
///
/// `rb` を所有権で受けて各 Value を move 消費することで、Text / Blob の double clone を回避する
/// (`row.get::<_, Value>(i)` で 1 回 alloc 済みのため、再 clone は無駄)。
fn build_record_batch(
    rb: RowBatch,
    columns: &[ColumnPlan],
    geom_column: &str,
    schema: &SchemaRef,
    geom_idx_in_row: usize,
) -> Result<RecordBatch> {
    let n_rows = rb.rows.len();
    let mut attr_builders: Vec<AttrBuilder> = columns
        .iter()
        .map(|c| AttrBuilder::new(&c.arrow_type, n_rows))
        .collect::<Result<Vec<_>>>()?;
    let mut geom_builder = BinaryBuilder::with_capacity(n_rows, n_rows * 32);

    for row in rb.rows {
        // row 中、geom_idx_in_row の Value だけ取り出して geometry に、残りの Value は
        // 順序通り attrs に流し込む。Vec から個別 index で move out できないので、
        // `into_iter` で順次取り出して enumerate で位置判定する。
        let mut attr_pos = 0usize;
        let mut geom_value: Option<Value> = None;
        for (i, v) in row.into_iter().enumerate() {
            if i == geom_idx_in_row {
                geom_value = Some(v);
            } else {
                let c = &columns[attr_pos];
                let attr = decode_value(v, &c.arrow_type, &c.name)?;
                attr_builders[attr_pos].push_owned(attr, &c.name)?;
                attr_pos += 1;
            }
        }
        let geom_v = geom_value.ok_or_else(|| {
            driver_msg(format!(
                "row missing geometry column at index {geom_idx_in_row}"
            ))
        })?;
        match geom_v {
            Value::Null => geom_builder.append_null(),
            Value::Blob(b) => {
                let (_srid, wkb_bytes) = shpx_geom::spatialite_blob::decode(&b)?;
                geom_builder.append_value(&wkb_bytes);
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
