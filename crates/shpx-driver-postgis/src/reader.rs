//! PostGIS の `LayerReader` 実装。
//!
//! ストリーミング戦略: background OS thread + `std::sync::mpsc::sync_channel(2)` で
//! `tokio_postgres::RowStream` を逐次消費する async-to-sync mpsc bridge。
//!
//! - probe 系 (geometry_columns view、ST_SRID/ST_GeometryType の 1 行 LIMIT) は
//!   `conn::query_opt` を使って `open()` 内で同期的に解決する。
//! - 本番 SELECT は **新しい client を 1 本別途 connect** して background thread に move し、
//!   `query_raw` で得た `RowStream` を `try_next()` ループで pull、`READ_BATCH_SIZE`
//!   行ごとに `RecordBatch` 化して channel へ送る。reader 用 client を別建てるのは、
//!   probe client と worker thread の寿命を切り離して所有関係を単純化するため。
//!   channel 容量 2 で receiver が drop されると次 `tx.send` が Err になり worker が
//!   自然終了する (`tests/reader_cancel.rs` で検証)。
//!
//! geometry 列は `ST_AsEWKB(<col>)` で取得し、`shpx_geom::ewkb::strip_srid` で
//! 標準 WKB と SRID に分離する。SRID は `geometry_columns` view → 先頭 non-NULL 行の
//! `ST_SRID()` の順に解決して `Crs` に反映する。
//!
//! 2 つの読み出しモードを持つ:
//! - **table モード** (`--query` 未指定): `pg_attribute` を引いて列メタを取り、
//!   `--where` / `--select` を SQL に埋め込む。geometry 列は必須。
//! - **query モード** (`--query 'SELECT ...'`): ユーザ SQL を `LIMIT 0` でサブクエリ化
//!   して列メタを取り、本番 SQL では geometry 列を `ST_AsEWKB` で包んで再発行する。

use std::collections::HashMap;
use std::sync::mpsc::{sync_channel, Receiver};
use std::sync::Arc;
use std::thread;

use arrow_array::{
    builder::{
        ArrayBuilder, BinaryBuilder, BooleanBuilder, Date32Builder, Decimal128Builder,
        Float32Builder, Float64Builder, Int16Builder, Int32Builder, Int64Builder, StringBuilder,
        TimestampMicrosecondBuilder,
    },
    ArrayRef, RecordBatch,
};
use arrow_schema::{DataType, Field, Schema, SchemaRef, TimeUnit};
use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc};
use futures_util::TryStreamExt;
use shpx_core::{
    schema::{GeometryMeta, GeometryType, GEOMETRY_META_KEY},
    Crs, Error, LayerReader, ReadOpts, Result, Uri,
};
use shpx_geom::ewkb;
use tokio_postgres::{types::Type as PgType, Client, Row, Statement};

use crate::conn;
use crate::copy_binary::PgNumeric;
use crate::options::{validate_user_query, ResolvedReadOpts};
use crate::type_map::{geom_type_from_st_name, pg_to_arrow};
use crate::util::{
    driver_err, driver_msg, is_geometry_typname, quote_ident, quote_qualified, DRIVER_NAME,
};

/// 1 batch あたりの既定行数。`batch_size_hint` 未指定時に使う。
const DEFAULT_BATCH_SIZE: usize = 65_536;

/// background worker から send される 1 batch ぶん。
/// `Err` の場合はその時点で worker が早期終了したことを意味する。
type BatchChunk = std::result::Result<RecordBatch, shpx_core::Error>;

pub struct PostgisReader {
    schema: SchemaRef,
    crs: Option<Crs>,
    /// background worker thread からの batch 受信口。`batches()` で take する。
    /// rx の drop が worker 側 tx.send Err を誘発し worker 自然終了に繋がる。
    rx: Option<Receiver<BatchChunk>>,
}

impl PostgisReader {
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
        let client = conn::connect(uri.path())?;
        let url = uri.path().to_string();
        if let Some(query) = opts.query.as_deref() {
            Self::open_query_mode(&client, query, opts, &url)
        } else {
            let resolved = ResolvedReadOpts::resolve(uri, opts)?;
            Self::open_table_mode(&client, &resolved, opts, &url)
        }
    }

    fn open_table_mode(
        client: &Client,
        resolved: &ResolvedReadOpts,
        opts: &ReadOpts,
        url: &str,
    ) -> Result<Self> {
        let qualified = quote_qualified(&resolved.schema, &resolved.table);
        let all_columns = describe_columns(client, &resolved.schema, &resolved.table)?;
        let columns = match opts.select.as_deref() {
            None => all_columns,
            Some(names) => filter_columns_by_select(&all_columns, names)?,
        };
        let geom_idx = columns
            .iter()
            .position(|c| c.is_geometry)
            .ok_or_else(|| {
                driver_msg(if opts.select.is_some() {
                    "--select must include the geometry column; geometry-less extraction is not supported in v0.3"
                } else {
                    "selected table does not contain a PostGIS geometry/geography column"
                })
            })?;

        // SRID は geometry 列がある場合のみ問い合わせる。geometry_columns view が未登録
        // なら NULL になりうるので、テーブル先頭行の `ST_SRID() / ST_GeometryType()` も
        // フォールバックに使う。後者は 1 query に集約してラウンドトリップを 1 つ削る。
        let col_name = &columns[geom_idx].name;
        let (crs_from_table, geom_type_from_table) = probe_geometry_metadata(
            client,
            &resolved.schema,
            &resolved.table,
            col_name,
            &qualified,
        )?;

        // ReadOpts.src_crs があれば最優先（CLI --src-crs）。
        let crs: Option<Crs> = opts.src_crs.clone().or(crs_from_table);

        let schema = build_arrow_schema(&columns, geom_idx, geom_type_from_table, crs.as_ref())?;

        let select_sql =
            build_select_sql_table(&columns, geom_idx, &qualified, opts.where_clause.as_deref());
        Self::spawn_streaming(url, &select_sql, schema, crs, columns, geom_idx)
    }

    fn open_query_mode(client: &Client, query: &str, opts: &ReadOpts, url: &str) -> Result<Self> {
        // ユーザ SQL を LIMIT 0 でサブクエリ化し、列メタだけ先取り。geometry 列は
        // PostGIS の動的 OID で発行されるが、tokio-postgres は pg_catalog から
        // typname を解決済みなので `Type::name()` で判定できる。
        let probe_sql = format!("SELECT * FROM ({query}) AS shpx_q LIMIT 0");
        let stmt = conn::prepare(client, &probe_sql)?;
        let columns = build_columns_from_statement(&stmt)?;
        let geom_idx = columns.iter().position(|c| c.is_geometry).ok_or_else(|| {
            driver_msg("--query result does not contain a PostGIS geometry/geography column")
        })?;
        let geom_col_name = columns[geom_idx].name.clone();

        // SRID と代表 geometry 型はサブクエリ全体を再走査して 1 行だけ取り出す。
        // テーブル名が無いので geometry_columns view は使えない。
        let probe_geom_sql = format!(
            "SELECT ST_SRID({col}), ST_GeometryType({col}) \
             FROM ({query}) AS shpx_q WHERE {col} IS NOT NULL LIMIT 1",
            col = quote_ident(&geom_col_name),
        );
        let (probe_srid, geom_type) = match conn::query_opt(client, &probe_geom_sql, &[])? {
            Some(row) => {
                let srid: i32 = row.try_get(0).map_err(|e| driver_msg(e.to_string()))?;
                let name: String = row.try_get(1).map_err(|e| driver_msg(e.to_string()))?;
                (Some(srid), geom_type_from_st_name(&name))
            }
            None => (None, GeometryType::Geometry),
        };

        let crs: Option<Crs> = opts
            .src_crs
            .clone()
            .or_else(|| probe_srid.and_then(epsg_to_crs));
        let schema = build_arrow_schema(&columns, geom_idx, geom_type, crs.as_ref())?;

        let real_sql = build_select_sql_query(&columns, geom_idx, query);
        Self::spawn_streaming(url, &real_sql, schema, crs, columns, geom_idx)
    }

    /// background OS thread を起動し、reader 専用に新規 connect した client から
    /// `query_raw` で `RowStream` を pull、`READ_BATCH_SIZE` 行ごとに `RecordBatch`
    /// を組んで channel へ送る。
    ///
    /// 専用 client を別途 connect する理由: 呼び出し側 (probe で使った既存 client) を
    /// move すると probe 後の client lifetime と worker thread 寿命が結びついてしまい、
    /// `&Client` を閉じる順序が複雑化する。CLI scope では connect 1 回 (数 ms) の
    /// オーバーヘッドは無視できるため、reader 用に都度 1 本立てる方針に倒す。
    fn spawn_streaming(
        url: &str,
        sql: &str,
        schema: SchemaRef,
        crs: Option<Crs>,
        columns: Vec<ColumnInfo>,
        geom_idx: usize,
    ) -> Result<Self> {
        let bg_client = conn::connect(url)?;
        let rt = crate::runtime::runtime()?;
        let sql = sql.to_string();
        let schema_for_worker = schema.clone();
        let (tx, rx) = sync_channel::<BatchChunk>(2);

        thread::spawn(move || {
            rt.block_on(async move {
                let stream = match bg_client.query_raw(&sql, std::iter::empty::<i32>()).await {
                    Ok(s) => s,
                    Err(e) => {
                        let _ = tx.send(Err(driver_err(&e)));
                        return;
                    }
                };
                tokio::pin!(stream);

                let mut buf: Vec<Row> = Vec::with_capacity(DEFAULT_BATCH_SIZE);
                loop {
                    match stream.try_next().await {
                        Ok(Some(row)) => {
                            buf.push(row);
                            if buf.len() >= DEFAULT_BATCH_SIZE {
                                let chunk = std::mem::replace(
                                    &mut buf,
                                    Vec::with_capacity(DEFAULT_BATCH_SIZE),
                                );
                                match rows_to_record_batch(
                                    &schema_for_worker,
                                    &columns,
                                    geom_idx,
                                    &chunk,
                                ) {
                                    Ok(batch) => {
                                        if tx.send(Ok(batch)).is_err() {
                                            return; // receiver dropped
                                        }
                                    }
                                    Err(e) => {
                                        let _ = tx.send(Err(e));
                                        return;
                                    }
                                }
                            }
                        }
                        Ok(None) => break,
                        Err(e) => {
                            let _ = tx.send(Err(driver_err(&e)));
                            return;
                        }
                    }
                }
                if !buf.is_empty() {
                    match rows_to_record_batch(&schema_for_worker, &columns, geom_idx, &buf) {
                        Ok(batch) => {
                            let _ = tx.send(Ok(batch));
                        }
                        Err(e) => {
                            let _ = tx.send(Err(e));
                        }
                    }
                }
                // bg_client は async block 終端で drop され、Connection task も自然終了する。
            });
        });

        Ok(Self {
            schema,
            crs,
            rx: Some(rx),
        })
    }
}

impl LayerReader for PostgisReader {
    fn schema(&self) -> SchemaRef {
        self.schema.clone()
    }

    fn crs(&self) -> Option<&Crs> {
        self.crs.as_ref()
    }

    fn row_count_hint(&self) -> Option<usize> {
        // streaming のため事前に行数は確定しない (`SELECT COUNT(*)` を別途叩くと
        // ラウンドトリップが増えるため敢えてやらない)。
        None
    }

    fn batches(&mut self) -> Box<dyn Iterator<Item = Result<RecordBatch>> + Send + '_> {
        let rx = self.rx.take();
        Box::new(BatchIter { rx })
    }
}

/// `LayerReader::batches()` から返されるストリーム iterator。
struct BatchIter {
    rx: Option<Receiver<BatchChunk>>,
}

impl Iterator for BatchIter {
    type Item = Result<RecordBatch>;

    fn next(&mut self) -> Option<Self::Item> {
        let rx = self.rx.as_ref()?;
        match rx.recv() {
            Ok(Ok(b)) => Some(Ok(b)),
            Ok(Err(e)) => {
                // worker からのエラー後はそれ以上読まない。
                self.rx = None;
                Some(Err(e))
            }
            Err(_) => {
                // tx の drop = 全 batch 送信完了 (または worker 早期 return)。
                self.rx = None;
                None
            }
        }
    }
}

#[derive(Debug, Clone)]
struct ColumnInfo {
    name: String,
    /// PostgreSQL の型名（geometry / geography 判定用）。
    type_name: String,
    /// `tokio_postgres::types::Type` から得た PgType。geometry/geography の場合は本フィールドを参照しない。
    pg_type: PgType,
    /// `pg_attribute.atttypmod`。`numeric` で precision/scale を取り出すのに使う。-1 は未指定。
    typmod: i32,
    /// PostGIS の geometry/geography 列か。
    is_geometry: bool,
    /// NOT NULL 制約。
    nullable: bool,
}

/// `pg_attribute` を引いて列順 + 型を取る。
fn describe_columns(client: &Client, schema: &str, table: &str) -> Result<Vec<ColumnInfo>> {
    // information_schema.columns の `udt_name` だけだと PostGIS geometry の
    // 詳細型 (Point/Polygon 等) は取れないが、geometry/geography 判定には十分。
    // PgType への変換は OID 経由で `Type::from_oid` を使う（tokio-postgres が builtin OID を解決）。
    let sql = "
        SELECT a.attname, a.atttypid, a.atttypmod, a.attnotnull, t.typname
        FROM pg_attribute a
        JOIN pg_class c ON c.oid = a.attrelid
        JOIN pg_namespace n ON n.oid = c.relnamespace
        JOIN pg_type t ON t.oid = a.atttypid
        WHERE n.nspname = $1
          AND c.relname = $2
          AND a.attnum > 0
          AND NOT a.attisdropped
        ORDER BY a.attnum
    ";
    let rows = conn::query(client, sql, &[&schema, &table])?;
    if rows.is_empty() {
        return Err(driver_msg(format!("table not found: {schema}.{table}")));
    }

    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let name: String = row
            .try_get("attname")
            .map_err(|e| driver_msg(e.to_string()))?;
        let oid: u32 = row
            .try_get("atttypid")
            .map_err(|e| driver_msg(e.to_string()))?;
        let typmod: i32 = row
            .try_get("atttypmod")
            .map_err(|e| driver_msg(e.to_string()))?;
        let notnull: bool = row
            .try_get("attnotnull")
            .map_err(|e| driver_msg(e.to_string()))?;
        let typname: String = row
            .try_get("typname")
            .map_err(|e| driver_msg(e.to_string()))?;

        let is_geom = is_geometry_typname(&typname);
        // builtin 以外の OID（PostGIS geometry など）は Type::from_oid が None を返す。
        // その場合は便宜的に PgType::BYTEA を入れる（geometry 列は本フィールドを使わないため）。
        let pg_type = if is_geom {
            PgType::BYTEA
        } else {
            PgType::from_oid(oid).ok_or_else(|| {
                Error::Schema(format!(
                    "unknown PostgreSQL OID {oid} for column `{name}` (typname `{typname}`)"
                ))
            })?
        };

        out.push(ColumnInfo {
            name,
            type_name: typname,
            pg_type,
            typmod,
            is_geometry: is_geom,
            nullable: !notnull,
        });
    }
    Ok(out)
}

/// PG `numeric` の `pg_attribute.atttypmod` から `(precision, scale)` を取り出す。
///
/// レイアウト: `atttypmod = ((p << 16) | s) + VARHDRSZ` で VARHDRSZ = 4。
/// `atttypmod = -1`（未指定）または precision が 38 を超えるなど Decimal128 に
/// 収まらない場合は `(38, 0)` フォールバック。
///
/// PG13 以降は scale が負（trailing zero round to integer）も許されるが、Arrow Decimal128
/// で素直に表現できないため `(38, 0)` フォールバックする。
fn numeric_typmod_to_p_s(typmod: i32) -> (u8, u8) {
    if typmod < 0 {
        return (38, 0);
    }
    let m = typmod - 4;
    let p = (m >> 16) & 0xFFFF;
    let s = m & 0xFFFF;
    if !(1..=38).contains(&p) || !(0..=38).contains(&s) || s > p {
        return (38, 0);
    }
    // 1..=38 に bound 済みなので u8 に必ず収まる。
    (
        u8::try_from(p).expect("precision fits u8"),
        u8::try_from(s).expect("scale fits u8"),
    )
}

/// geometry 列の SRID と代表 geometry 型を 2 query で取得する:
///
/// 1. `geometry_columns` view（メタ由来、テーブルが空でも srid を返す）
/// 2. 先頭の non-NULL 行から `ST_SRID()` と `ST_GeometryType()` を 1 query で取る
///    （view から srid が取れた場合の fallback と geometry 型の取得を兼ねる）
///
/// view の srid を優先し、見つからなければ probe の srid を使う。
fn probe_geometry_metadata(
    client: &Client,
    schema: &str,
    table: &str,
    geom_col: &str,
    qualified: &str,
) -> Result<(Option<Crs>, GeometryType)> {
    let view_sql = "
        SELECT srid
        FROM geometry_columns
        WHERE f_table_schema = $1 AND f_table_name = $2 AND f_geometry_column = $3
        LIMIT 1
    ";
    let view_srid: Option<i32> =
        match conn::query_opt(client, view_sql, &[&schema, &table, &geom_col])? {
            Some(row) => Some(row.try_get(0).map_err(|e| driver_msg(e.to_string()))?),
            None => None,
        };

    let probe_sql = format!(
        "SELECT ST_SRID({col}), ST_GeometryType({col}) \
         FROM {qualified} WHERE {col} IS NOT NULL LIMIT 1",
        col = quote_ident(geom_col),
    );
    let (probe_srid, geom_type) = match conn::query_opt(client, &probe_sql, &[])? {
        Some(row) => {
            let srid: i32 = row.try_get(0).map_err(|e| driver_msg(e.to_string()))?;
            let name: String = row.try_get(1).map_err(|e| driver_msg(e.to_string()))?;
            (Some(srid), geom_type_from_st_name(&name))
        }
        None => (None, GeometryType::Geometry),
    };

    let crs = view_srid.or(probe_srid).and_then(epsg_to_crs);
    Ok((crs, geom_type))
}

fn epsg_to_crs(srid: i32) -> Option<Crs> {
    if srid <= 0 {
        return None;
    }
    u32::try_from(srid).ok().map(Crs::from_epsg)
}

fn build_arrow_schema(
    columns: &[ColumnInfo],
    geom_idx: usize,
    geom_type: GeometryType,
    crs: Option<&Crs>,
) -> Result<SchemaRef> {
    let mut fields: Vec<Field> = Vec::with_capacity(columns.len());
    for (i, c) in columns.iter().enumerate() {
        if i == geom_idx {
            let mut f = Field::new(&c.name, DataType::Binary, c.nullable);
            let mut m = HashMap::new();
            m.insert(
                GEOMETRY_META_KEY.to_string(),
                GeometryMeta::wkb(geom_type, crs.cloned()).to_json()?,
            );
            f.set_metadata(m);
            fields.push(f);
        } else {
            let dt = if c.pg_type == PgType::NUMERIC {
                let (p, s) = numeric_typmod_to_p_s(c.typmod);
                // Arrow Decimal128 は precision: u8、scale: i8。p <= 38, s <= p ≤ 38 のため i8 に必ず収まる。
                let s_i8 = i8::try_from(s).expect("scale 0..=38 fits i8");
                DataType::Decimal128(p, s_i8)
            } else {
                pg_to_arrow(&c.pg_type).map_err(|e| {
                    if let Error::Schema(msg) = e {
                        Error::Schema(format!("column `{}` ({}): {}", c.name, c.type_name, msg))
                    } else {
                        e
                    }
                })?
            };
            fields.push(Field::new(&c.name, dt, c.nullable));
        }
    }
    Ok(Arc::new(Schema::new(fields)))
}

fn projection_parts(columns: &[ColumnInfo], geom_idx: usize) -> Vec<String> {
    columns
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let q = quote_ident(&c.name);
            if i == geom_idx {
                // geometry を EWKB として取得し、列名は元の名前を保つ。
                format!("ST_AsEWKB({q}) AS {q}")
            } else {
                q
            }
        })
        .collect()
}

fn build_select_sql_table(
    columns: &[ColumnInfo],
    geom_idx: usize,
    qualified: &str,
    where_clause: Option<&str>,
) -> String {
    let parts = projection_parts(columns, geom_idx);
    let projection = parts.join(", ");
    match where_clause.map(str::trim).filter(|s| !s.is_empty()) {
        Some(w) => format!("SELECT {projection} FROM {qualified} WHERE {w}"),
        None => format!("SELECT {projection} FROM {qualified}"),
    }
}

fn build_select_sql_query(columns: &[ColumnInfo], geom_idx: usize, query: &str) -> String {
    let parts = projection_parts(columns, geom_idx);
    format!("SELECT {} FROM ({query}) AS shpx_q", parts.join(", "))
}

/// `--select` で指定された列名のリストを既存の `ColumnInfo` 集合から並べ替えて取り出す。
/// 順序は **`--select` の順** を尊重する（CLI 利用者が結果の列順を制御できるようにする）。
fn filter_columns_by_select(all: &[ColumnInfo], names: &[String]) -> Result<Vec<ColumnInfo>> {
    let mut out = Vec::with_capacity(names.len());
    for name in names {
        if let Some(c) = all.iter().find(|c| &c.name == name) {
            out.push(c.clone());
        } else {
            let available: Vec<&str> = all.iter().map(|c| c.name.as_str()).collect();
            return Err(driver_msg(format!(
                "--select references unknown column `{name}` (available: {})",
                available.join(", ")
            )));
        }
    }
    Ok(out)
}

/// `LIMIT 0` の prepared statement から列メタを `ColumnInfo` に変換する。
///
/// `Statement::columns()` は typmod を露出しないため、`numeric` 列の precision/scale は
/// 取得できず `(38, 0)` フォールバックになる（query モード固有の制約）。
fn build_columns_from_statement(stmt: &Statement) -> Result<Vec<ColumnInfo>> {
    let mut out = Vec::with_capacity(stmt.columns().len());
    for col in stmt.columns() {
        let pg_ty = col.type_();
        let typname = pg_ty.name().to_string();
        let is_geom = is_geometry_typname(&typname);
        // builtin 以外の OID（PostGIS geometry など）は from_oid が None を返す。
        // geometry 列は本フィールドを参照しないため BYTEA で代用する（describe_columns と同じ慣習）。
        let pg_type = if is_geom {
            PgType::BYTEA
        } else {
            PgType::from_oid(pg_ty.oid()).ok_or_else(|| {
                Error::Schema(format!(
                    "unknown PostgreSQL type `{}` (OID {}) for column `{}` in --query result",
                    typname,
                    pg_ty.oid(),
                    col.name()
                ))
            })?
        };
        out.push(ColumnInfo {
            name: col.name().to_string(),
            type_name: typname,
            pg_type,
            // typmod は prepared statement のメタからは取得できない。
            // numeric は precision を保てないため (38, 0) になる。
            typmod: -1,
            is_geometry: is_geom,
            // 列の nullability も prepared statement では分からないので nullable とする。
            nullable: true,
        });
    }
    Ok(out)
}

fn rows_to_record_batch(
    schema: &SchemaRef,
    columns: &[ColumnInfo],
    geom_idx: usize,
    rows: &[Row],
) -> Result<RecordBatch> {
    // 各列の builder を schema に基づいて作る。geometry 列は BinaryBuilder。
    let mut builders: Vec<Box<dyn ArrayBuilder>> = schema
        .fields()
        .iter()
        .map(|f| make_builder(f.data_type()))
        .collect();

    for row in rows {
        for (i, c) in columns.iter().enumerate() {
            if i == geom_idx {
                let bb = builders[i]
                    .as_any_mut()
                    .downcast_mut::<BinaryBuilder>()
                    .expect("geometry builder is BinaryBuilder");
                let ewkb_bytes: Option<Vec<u8>> =
                    row.try_get(i).map_err(|e| driver_msg(e.to_string()))?;
                match ewkb_bytes {
                    Some(b) => {
                        let (wkb_bytes, _srid) = ewkb::strip_srid(&b)?;
                        bb.append_value(&wkb_bytes);
                    }
                    None => bb.append_null(),
                }
            } else {
                let field = schema.field(i);
                append_value(
                    builders[i].as_mut(),
                    &c.pg_type,
                    field.data_type(),
                    row,
                    i,
                    &c.name,
                )?;
            }
        }
    }

    let arrays: Vec<ArrayRef> = builders
        .iter_mut()
        .map(|b| ArrayBuilder::finish(b.as_mut()))
        .collect();
    RecordBatch::try_new(schema.clone(), arrays).map_err(|e| Error::Schema(e.to_string()))
}

fn make_builder(dt: &DataType) -> Box<dyn ArrayBuilder> {
    match dt {
        DataType::Boolean => Box::new(BooleanBuilder::new()),
        DataType::Int16 => Box::new(Int16Builder::new()),
        DataType::Int32 => Box::new(Int32Builder::new()),
        DataType::Int64 => Box::new(Int64Builder::new()),
        DataType::Float32 => Box::new(Float32Builder::new()),
        DataType::Float64 => Box::new(Float64Builder::new()),
        DataType::Utf8 => Box::new(StringBuilder::new()),
        DataType::Binary => Box::new(BinaryBuilder::new()),
        DataType::Date32 => Box::new(Date32Builder::new()),
        DataType::Timestamp(TimeUnit::Microsecond, None) => {
            Box::new(TimestampMicrosecondBuilder::new())
        }
        DataType::Timestamp(TimeUnit::Microsecond, Some(_)) => {
            Box::new(TimestampMicrosecondBuilder::new().with_timezone("UTC"))
        }
        DataType::Decimal128(p, s) => Box::new(
            Decimal128Builder::new()
                .with_precision_and_scale(*p, *s)
                .expect("validated by build_arrow_schema"),
        ),
        // サポート外の型は schema 構築段階で reject されるので、ここに来た時点でバグ。
        other => panic!("make_builder: unexpected DataType {other:?}"),
    }
}

#[allow(clippy::too_many_lines)]
fn append_value(
    builder: &mut dyn ArrayBuilder,
    pg_type: &PgType,
    target_dt: &DataType,
    row: &Row,
    col: usize,
    name: &str,
) -> Result<()> {
    let read_err =
        |e: tokio_postgres::Error| -> Error { driver_msg(format!("column `{name}`: {e}")) };
    match *pg_type {
        PgType::BOOL => {
            let b = builder
                .as_any_mut()
                .downcast_mut::<BooleanBuilder>()
                .unwrap();
            let v: Option<bool> = row.try_get(col).map_err(read_err)?;
            match v {
                Some(x) => b.append_value(x),
                None => b.append_null(),
            }
        }
        PgType::INT2 => {
            let b = builder.as_any_mut().downcast_mut::<Int16Builder>().unwrap();
            let v: Option<i16> = row.try_get(col).map_err(read_err)?;
            match v {
                Some(x) => b.append_value(x),
                None => b.append_null(),
            }
        }
        PgType::INT4 => {
            let b = builder.as_any_mut().downcast_mut::<Int32Builder>().unwrap();
            let v: Option<i32> = row.try_get(col).map_err(read_err)?;
            match v {
                Some(x) => b.append_value(x),
                None => b.append_null(),
            }
        }
        PgType::INT8 => {
            let b = builder.as_any_mut().downcast_mut::<Int64Builder>().unwrap();
            let v: Option<i64> = row.try_get(col).map_err(read_err)?;
            match v {
                Some(x) => b.append_value(x),
                None => b.append_null(),
            }
        }
        PgType::FLOAT4 => {
            let b = builder
                .as_any_mut()
                .downcast_mut::<Float32Builder>()
                .unwrap();
            let v: Option<f32> = row.try_get(col).map_err(read_err)?;
            match v {
                Some(x) => b.append_value(x),
                None => b.append_null(),
            }
        }
        PgType::FLOAT8 => {
            let b = builder
                .as_any_mut()
                .downcast_mut::<Float64Builder>()
                .unwrap();
            let v: Option<f64> = row.try_get(col).map_err(read_err)?;
            match v {
                Some(x) => b.append_value(x),
                None => b.append_null(),
            }
        }
        PgType::TEXT | PgType::VARCHAR | PgType::BPCHAR | PgType::NAME => {
            let b = builder
                .as_any_mut()
                .downcast_mut::<StringBuilder>()
                .unwrap();
            let v: Option<String> = row.try_get(col).map_err(read_err)?;
            match v {
                Some(x) => b.append_value(x),
                None => b.append_null(),
            }
        }
        PgType::BYTEA => {
            let b = builder
                .as_any_mut()
                .downcast_mut::<BinaryBuilder>()
                .unwrap();
            let v: Option<Vec<u8>> = row.try_get(col).map_err(read_err)?;
            match v {
                Some(x) => b.append_value(&x),
                None => b.append_null(),
            }
        }
        PgType::DATE => {
            let b = builder
                .as_any_mut()
                .downcast_mut::<Date32Builder>()
                .unwrap();
            let v: Option<NaiveDate> = row.try_get(col).map_err(read_err)?;
            match v {
                Some(d) => {
                    let epoch = NaiveDate::from_ymd_opt(1970, 1, 1).expect("epoch");
                    let days = i32::try_from(d.signed_duration_since(epoch).num_days())
                        .map_err(|_| driver_msg(format!("column `{name}`: date overflow")))?;
                    b.append_value(days);
                }
                None => b.append_null(),
            }
        }
        PgType::TIMESTAMP => {
            let b = builder
                .as_any_mut()
                .downcast_mut::<TimestampMicrosecondBuilder>()
                .unwrap();
            let v: Option<NaiveDateTime> = row.try_get(col).map_err(read_err)?;
            match v {
                Some(ndt) => {
                    let micros = ndt.and_utc().timestamp_micros();
                    b.append_value(micros);
                }
                None => b.append_null(),
            }
        }
        PgType::TIMESTAMPTZ => {
            let b = builder
                .as_any_mut()
                .downcast_mut::<TimestampMicrosecondBuilder>()
                .unwrap();
            let v: Option<DateTime<Utc>> = row.try_get(col).map_err(read_err)?;
            match v {
                Some(dt) => b.append_value(dt.timestamp_micros()),
                None => b.append_null(),
            }
        }
        PgType::NUMERIC => {
            let b = builder
                .as_any_mut()
                .downcast_mut::<Decimal128Builder>()
                .unwrap();
            let v: Option<PgNumeric> = row.try_get(col).map_err(read_err)?;
            let scale = match target_dt {
                DataType::Decimal128(_, s) => u8::try_from(*s).map_err(|_| {
                    driver_msg(format!("column `{name}`: invalid Decimal128 scale {s}"))
                })?,
                other => {
                    return Err(Error::Schema(format!(
                        "column `{name}`: NUMERIC mapped to non-Decimal128 type: {other:?}"
                    )));
                }
            };
            match v {
                Some(n) => {
                    let i = n.to_i128_with_scale(scale)?;
                    b.append_value(i);
                }
                None => b.append_null(),
            }
        }
        ref other => {
            return Err(Error::Schema(format!(
                "column `{name}`: unsupported PostgreSQL type at runtime: {} (OID {})",
                other.name(),
                other.oid()
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epsg_to_crs_handles_zero_and_negative() {
        assert_eq!(epsg_to_crs(0), None);
        assert_eq!(epsg_to_crs(-1), None);
        assert_eq!(epsg_to_crs(4326), Some(Crs::from_epsg(4326)));
    }

    fn sample_columns() -> Vec<ColumnInfo> {
        vec![
            ColumnInfo {
                name: "name".into(),
                type_name: "text".into(),
                pg_type: PgType::TEXT,
                typmod: -1,
                is_geometry: false,
                nullable: true,
            },
            ColumnInfo {
                name: "geom".into(),
                type_name: "geometry".into(),
                pg_type: PgType::BYTEA,
                typmod: -1,
                is_geometry: true,
                nullable: true,
            },
        ]
    }

    #[test]
    fn build_select_sql_table_wraps_geometry_with_st_asewkb() {
        let sql = build_select_sql_table(&sample_columns(), 1, "\"public\".\"t\"", None);
        assert_eq!(
            sql,
            "SELECT \"name\", ST_AsEWKB(\"geom\") AS \"geom\" FROM \"public\".\"t\""
        );
    }

    #[test]
    fn build_select_sql_table_appends_where_clause() {
        let sql = build_select_sql_table(&sample_columns(), 1, "\"public\".\"t\"", Some("id < 10"));
        assert_eq!(
            sql,
            "SELECT \"name\", ST_AsEWKB(\"geom\") AS \"geom\" FROM \"public\".\"t\" WHERE id < 10"
        );
    }

    #[test]
    fn build_select_sql_table_ignores_blank_where_clause() {
        let sql = build_select_sql_table(&sample_columns(), 1, "\"public\".\"t\"", Some("   "));
        assert_eq!(
            sql,
            "SELECT \"name\", ST_AsEWKB(\"geom\") AS \"geom\" FROM \"public\".\"t\""
        );
    }

    #[test]
    fn build_select_sql_query_wraps_subquery() {
        let sql = build_select_sql_query(&sample_columns(), 1, "SELECT name, geom FROM t");
        assert_eq!(
            sql,
            "SELECT \"name\", ST_AsEWKB(\"geom\") AS \"geom\" FROM (SELECT name, geom FROM t) AS shpx_q"
        );
    }

    #[test]
    fn filter_columns_by_select_preserves_specified_order() {
        let all = sample_columns();
        let got =
            filter_columns_by_select(&all, &["geom".to_string(), "name".to_string()]).unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].name, "geom");
        assert_eq!(got[1].name, "name");
    }

    #[test]
    fn filter_columns_by_select_rejects_unknown_name() {
        let all = sample_columns();
        let err = filter_columns_by_select(&all, &["geom".into(), "missing".into()]);
        assert!(err.is_err());
    }

    #[test]
    fn validate_user_query_rejects_semicolon() {
        use crate::options::validate_user_query;
        assert!(validate_user_query("SELECT 1; SELECT 2").is_err());
        assert!(validate_user_query("SELECT 1;").is_err());
        assert!(validate_user_query("").is_err());
        assert!(validate_user_query("   ").is_err());
        assert!(validate_user_query("SELECT geom FROM t").is_ok());
    }

    #[test]
    fn numeric_typmod_decode() {
        // numeric(10, 2): typmod = ((10 << 16) | 2) + 4 = 655_366
        assert_eq!(numeric_typmod_to_p_s(655_366), (10, 2));
        // numeric(38, 10): typmod = ((38 << 16) | 10) + 4 = 2_490_382
        assert_eq!(numeric_typmod_to_p_s(2_490_382), (38, 10));
        // unknown typmod → fallback
        assert_eq!(numeric_typmod_to_p_s(-1), (38, 0));
        // out-of-range precision (39) → fallback
        let p39_s0 = ((39_i32) << 16) + 4;
        assert_eq!(numeric_typmod_to_p_s(p39_s0), (38, 0));
        // s > p → fallback (numeric(5, 10) is invalid for Decimal128)
        let p5_s10 = ((5_i32) << 16) | 0xA;
        assert_eq!(numeric_typmod_to_p_s(p5_s10 + 4), (38, 0));
    }
}
