//! SQL Server の `LayerReader` 実装。
//!
//! v0.8 cycle 5 で eager-load (`Vec<RecordBatch>`) を撤廃し、PostGIS と同じ
//! background OS thread + `std::sync::mpsc::sync_channel(2)` パターンで
//! `tiberius::QueryStream` を逐次消費する真のストリーミングに置き換えた。
//!
//! 引き続き v0.4 の制約上 table モード固定 (`--where` / `--select` / `--query` は
//! 将来拡張)。
//!
//! geometry 列は `[col].STAsBinary() AS [col]` で OGC 標準 WKB を取得し、`STSrid` を
//! 併走列として取り出す。SRID は probe `SELECT TOP 1 ... STSrid, STGeometryType()` で
//! 解決する (cycle 5 で従来の SELECT 結果先頭行 fallback を削除し、probe 一本に集約)。

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
use rust_decimal::Decimal;
use shpx_core::{
    schema::{GeometryMeta, GeometryType, GEOMETRY_META_KEY},
    Crs, Error, LayerReader, ReadOpts, Result, Uri,
};
use tiberius::{QueryItem, Row};

use crate::conn::{self, SqlClient};
use crate::options::ResolvedReadOpts;
use crate::runtime::runtime;
use crate::type_map::{
    geom_type_from_sqlserver_name, is_geometry_type_name, sqlserver_type_to_arrow,
};
use crate::util::{driver_err, driver_msg, quote_ident, quote_qualified};

/// 1 batch あたりの既定行数。worker thread が `READ_BATCH_SIZE` 行ごとに
/// `RecordBatch` を組んで channel に送る。下流 (writer) bind バッチサイズに合わせて
/// 64Ki 固定。
const DEFAULT_BATCH_SIZE: usize = 65_536;

/// SRID 併走列の suffix。元の列名と衝突しにくいよう `__shpx_srid` を使う。
const SRID_SUFFIX: &str = "__shpx_srid";

/// background worker から send される 1 batch ぶん。
type BatchChunk = std::result::Result<RecordBatch, shpx_core::Error>;

pub struct SqlServerReader {
    schema: SchemaRef,
    crs: Option<Crs>,
    /// background worker thread からの batch 受信口。
    rx: Option<Receiver<BatchChunk>>,
}

impl SqlServerReader {
    pub fn open(uri: &Uri, opts: &ReadOpts) -> Result<Self> {
        // `--where` / `--select` / `--query` は将来拡張。CLI 利用者を早期に弾く。
        if opts.query.is_some() {
            return Err(driver_msg(
                "--query is not supported by sqlserver driver (planned for a later release)",
            ));
        }
        if opts.where_clause.is_some() {
            return Err(driver_msg(
                "--where is not supported by sqlserver driver (planned for a later release)",
            ));
        }
        if opts.select.is_some() {
            return Err(driver_msg(
                "--select is not supported by sqlserver driver (planned for a later release)",
            ));
        }

        let resolved = ResolvedReadOpts::resolve(uri, opts)?;
        let url = uri.path().to_string();
        // probe 用の client。schema describe + SRID/geom_type probe にだけ使い、最後に drop。
        let mut probe_client = conn::connect(&url)?;
        Self::open_table_mode(&mut probe_client, &resolved, opts, &url)
    }

    fn open_table_mode(
        client: &mut SqlClient,
        resolved: &ResolvedReadOpts,
        opts: &ReadOpts,
        url: &str,
    ) -> Result<Self> {
        let qualified = quote_qualified(&resolved.schema, &resolved.table);
        let columns = describe_columns(client, &resolved.schema, &resolved.table)?;
        let geom_idx = columns.iter().position(|c| c.is_geometry).ok_or_else(|| {
            driver_msg(format!(
                "table {qualified} does not contain a geometry/geography column"
            ))
        })?;

        let geom_col_name = columns[geom_idx].name.clone();
        let (probe_srid, geom_type) = probe_geometry_metadata(client, &qualified, &geom_col_name)?;

        let crs: Option<Crs> = opts
            .src_crs
            .clone()
            .or_else(|| probe_srid.and_then(epsg_to_crs));

        let schema = build_arrow_schema(&columns, geom_idx, geom_type, crs.as_ref())?;
        let select_sql = build_select_sql(&columns, geom_idx, &qualified);

        Self::spawn_streaming(url, select_sql, schema, crs, columns, geom_idx)
    }

    /// background OS thread を起動し、reader 専用に新規 connect した client から
    /// `simple_query` で `QueryStream` を pull、`READ_BATCH_SIZE` 行ごとに
    /// `RecordBatch` を組んで channel へ送る。
    ///
    /// PostGIS と同じ async-to-sync mpsc bridge パターン。reader 用 client を別途
    /// 立てる理由は cycle 4 と同じ (probe client の lifetime と worker thread 寿命を
    /// 切り離して所有関係を単純化)。
    fn spawn_streaming(
        url: &str,
        sql: String,
        schema: SchemaRef,
        crs: Option<Crs>,
        columns: Vec<ColumnInfo>,
        geom_idx: usize,
    ) -> Result<Self> {
        let mut bg_client = conn::connect(url)?;
        let rt = runtime()?;
        let schema_for_worker = schema.clone();
        let (tx, rx) = sync_channel::<BatchChunk>(2);

        thread::spawn(move || {
            rt.block_on(async move {
                let stream = match bg_client.simple_query(sql).await {
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
                        Ok(Some(QueryItem::Row(row))) => {
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
                        // Metadata token は読み飛ばす (列メタは describe_columns で別途取得済み)。
                        Ok(Some(QueryItem::Metadata(_))) => {}
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
                // bg_client は async block 終端で drop され、TCP 接続も自然終了する。
            });
        });

        Ok(Self {
            schema,
            crs,
            rx: Some(rx),
        })
    }
}

impl LayerReader for SqlServerReader {
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
                self.rx = None;
                Some(Err(e))
            }
            Err(_) => {
                self.rx = None;
                None
            }
        }
    }
}

#[derive(Debug, Clone)]
struct ColumnInfo {
    name: String,
    /// Arrow に対応する型（geometry の場合は `Binary` で代用）。
    arrow_type: DataType,
    /// SQL Server の geometry / geography 列か。
    is_geometry: bool,
    nullable: bool,
}

/// `INFORMATION_SCHEMA.COLUMNS` を引いて列順 + 型を取る。
fn describe_columns(client: &mut SqlClient, schema: &str, table: &str) -> Result<Vec<ColumnInfo>> {
    // `INFORMATION_SCHEMA.COLUMNS` の `DATA_TYPE` は UDT (geometry/geography) 列で
    // 空文字を返すバージョンがあり、追加列 `USER_DEFINED_TYPE_NAME` は標準 view に
    // 存在しない (実装依存)。確実なのは `sys.columns` + `sys.types` で `t.name` から
    // UDT 名を直接取る方法。is_nullable は bit、precision/scale は tinyint で返る。
    let sql = "
        SELECT
            c.name,
            t.name AS type_name,
            c.is_nullable,
            c.precision,
            c.scale
        FROM sys.columns c
        INNER JOIN sys.types t ON t.user_type_id = c.user_type_id
        INNER JOIN sys.objects o ON o.object_id = c.object_id
        INNER JOIN sys.schemas s ON s.schema_id = o.schema_id
        WHERE s.name = @P1 AND o.name = @P2 AND o.type = 'U'
        ORDER BY c.column_id
    ";

    let rt = runtime()?;
    let rows: Vec<Row> = rt.block_on(async {
        let stream = client
            .query(sql, &[&schema, &table])
            .await
            .map_err(|e| driver_err(&e))?;
        stream.into_first_result().await.map_err(|e| driver_err(&e))
    })?;

    if rows.is_empty() {
        return Err(driver_msg(format!("table not found: {schema}.{table}")));
    }

    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let name: String = row
            .try_get::<&str, _>(0)
            .map_err(|e| driver_err(&e))?
            .unwrap_or("")
            .to_string();
        let type_name: &str = row.try_get(1).map_err(|e| driver_err(&e))?.unwrap_or("");
        let type_name_lc = type_name.to_ascii_lowercase();
        let nullable: bool = row.try_get(2).map_err(|e| driver_err(&e))?.unwrap_or(true);

        let is_geom = is_geometry_type_name(&type_name_lc);
        let arrow_type = if is_geom {
            DataType::Binary
        } else {
            // sys.columns.precision / scale は tinyint。tiberius は i32 経由で取れない
            // ので u8 で取って i32 にキャストする。NULL になることは無い (NOT NULL 列だが、
            // 念のため Option で受ける)。
            let p_u8: Option<u8> = row.try_get(3).map_err(|e| driver_err(&e))?;
            let s_u8: Option<u8> = row.try_get(4).map_err(|e| driver_err(&e))?;
            let p = p_u8.map(i32::from);
            let s = s_u8.map(i32::from);
            sqlserver_type_to_arrow(&type_name_lc, p, s).map_err(|e| match e {
                Error::Schema(msg) => Error::Schema(format!("column `{name}`: {msg}")),
                other => other,
            })?
        };

        out.push(ColumnInfo {
            name,
            arrow_type,
            is_geometry: is_geom,
            nullable,
        });
    }
    Ok(out)
}

/// 先頭の non-NULL 行から `[col].STSrid` と `[col].STGeometryType()` を取得する。
/// SQL Server には PostGIS の `geometry_columns` view 相当の集中メタが無いため、
/// 値経由で取るのが基本。テーブルが空 / 全 NULL の場合は `(None, Geometry)` を返す。
fn probe_geometry_metadata(
    client: &mut SqlClient,
    qualified: &str,
    geom_col: &str,
) -> Result<(Option<i32>, GeometryType)> {
    let col = quote_ident(geom_col);
    let sql = format!(
        "SELECT TOP 1 {col}.STSrid AS shpx_srid, {col}.STGeometryType() AS shpx_gt \
         FROM {qualified} WHERE {col} IS NOT NULL"
    );
    let rt = runtime()?;
    let rows: Vec<Row> = rt.block_on(async {
        let stream = client.simple_query(sql).await.map_err(|e| driver_err(&e))?;
        stream.into_first_result().await.map_err(|e| driver_err(&e))
    })?;

    if let Some(row) = rows.into_iter().next() {
        let srid: Option<i32> = row.try_get(0).map_err(|e| driver_err(&e))?;
        let gt: &str = row.try_get(1).map_err(|e| driver_err(&e))?.unwrap_or("");
        Ok((srid, geom_type_from_sqlserver_name(gt)))
    } else {
        Ok((None, GeometryType::Geometry))
    }
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
            fields.push(Field::new(&c.name, c.arrow_type.clone(), c.nullable));
        }
    }
    Ok(Arc::new(Schema::new(fields)))
}

/// SELECT SQL を組み立てる。geometry 列は `STAsBinary` でラップし、SRID は
/// 別カラム（`<col>__shpx_srid`）として併走させる。
fn build_select_sql(columns: &[ColumnInfo], geom_idx: usize, qualified: &str) -> String {
    let parts: Vec<String> = columns
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let q = quote_ident(&c.name);
            if i == geom_idx {
                let srid_alias = quote_ident(&format!("{}{}", c.name, SRID_SUFFIX));
                format!("{q}.STAsBinary() AS {q}, {q}.STSrid AS {srid_alias}")
            } else {
                q
            }
        })
        .collect();
    format!("SELECT {} FROM {qualified}", parts.join(", "))
}

fn rows_to_record_batch(
    schema: &SchemaRef,
    columns: &[ColumnInfo],
    geom_idx: usize,
    rows: &[Row],
) -> Result<RecordBatch> {
    let mut builders: Vec<Box<dyn ArrayBuilder>> = schema
        .fields()
        .iter()
        .map(|f| make_builder(f.data_type()))
        .collect();

    for row in rows {
        // 行内の tiberius 側カラムインデックスは ORDINAL_POSITION と一致する。geometry の
        // 直後には SRID 併走列があるため、後続の論理列は + 1 の補正が必要。
        let mut tds_idx = 0usize;
        for (i, c) in columns.iter().enumerate() {
            if i == geom_idx {
                let bb = builders[i]
                    .as_any_mut()
                    .downcast_mut::<BinaryBuilder>()
                    .expect("geometry builder is BinaryBuilder");
                let wkb: Option<&[u8]> = row.try_get(tds_idx).map_err(|e| driver_err(&e))?;
                match wkb {
                    Some(b) => bb.append_value(b),
                    None => bb.append_null(),
                }
                // geometry 列の直後 (`STSrid AS ...`) を読み飛ばす。
                tds_idx += 2;
            } else {
                append_value(builders[i].as_mut(), c, row, tds_idx)?;
                tds_idx += 1;
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
        // schema 構築段階で reject されるはず。来たらバグ。
        other => panic!("make_builder: unexpected DataType {other:?}"),
    }
}

#[allow(clippy::too_many_lines)]
fn append_value(
    builder: &mut dyn ArrayBuilder,
    column: &ColumnInfo,
    row: &Row,
    tds_idx: usize,
) -> Result<()> {
    let name = &column.name;
    match &column.arrow_type {
        DataType::Boolean => {
            let b = builder
                .as_any_mut()
                .downcast_mut::<BooleanBuilder>()
                .unwrap();
            let v: Option<bool> = row.try_get(tds_idx).map_err(|e| driver_err(&e))?;
            match v {
                Some(x) => b.append_value(x),
                None => b.append_null(),
            }
        }
        DataType::Int16 => {
            let b = builder.as_any_mut().downcast_mut::<Int16Builder>().unwrap();
            let v: Option<i16> = row.try_get(tds_idx).map_err(|e| driver_err(&e))?;
            match v {
                Some(x) => b.append_value(x),
                None => b.append_null(),
            }
        }
        DataType::Int32 => {
            let b = builder.as_any_mut().downcast_mut::<Int32Builder>().unwrap();
            let v: Option<i32> = row.try_get(tds_idx).map_err(|e| driver_err(&e))?;
            match v {
                Some(x) => b.append_value(x),
                None => b.append_null(),
            }
        }
        DataType::Int64 => {
            let b = builder.as_any_mut().downcast_mut::<Int64Builder>().unwrap();
            let v: Option<i64> = row.try_get(tds_idx).map_err(|e| driver_err(&e))?;
            match v {
                Some(x) => b.append_value(x),
                None => b.append_null(),
            }
        }
        DataType::Float32 => {
            let b = builder
                .as_any_mut()
                .downcast_mut::<Float32Builder>()
                .unwrap();
            let v: Option<f32> = row.try_get(tds_idx).map_err(|e| driver_err(&e))?;
            match v {
                Some(x) => b.append_value(x),
                None => b.append_null(),
            }
        }
        DataType::Float64 => {
            let b = builder
                .as_any_mut()
                .downcast_mut::<Float64Builder>()
                .unwrap();
            let v: Option<f64> = row.try_get(tds_idx).map_err(|e| driver_err(&e))?;
            match v {
                Some(x) => b.append_value(x),
                None => b.append_null(),
            }
        }
        DataType::Utf8 => {
            let b = builder
                .as_any_mut()
                .downcast_mut::<StringBuilder>()
                .unwrap();
            let v: Option<&str> = row.try_get(tds_idx).map_err(|e| driver_err(&e))?;
            match v {
                Some(x) => b.append_value(x),
                None => b.append_null(),
            }
        }
        DataType::Binary => {
            let b = builder
                .as_any_mut()
                .downcast_mut::<BinaryBuilder>()
                .unwrap();
            let v: Option<&[u8]> = row.try_get(tds_idx).map_err(|e| driver_err(&e))?;
            match v {
                Some(x) => b.append_value(x),
                None => b.append_null(),
            }
        }
        DataType::Date32 => {
            let b = builder
                .as_any_mut()
                .downcast_mut::<Date32Builder>()
                .unwrap();
            let v: Option<NaiveDate> = row.try_get(tds_idx).map_err(|e| driver_err(&e))?;
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
        DataType::Timestamp(TimeUnit::Microsecond, None) => {
            let b = builder
                .as_any_mut()
                .downcast_mut::<TimestampMicrosecondBuilder>()
                .unwrap();
            let v: Option<NaiveDateTime> = row.try_get(tds_idx).map_err(|e| driver_err(&e))?;
            match v {
                Some(ndt) => b.append_value(ndt.and_utc().timestamp_micros()),
                None => b.append_null(),
            }
        }
        DataType::Timestamp(TimeUnit::Microsecond, Some(_)) => {
            let b = builder
                .as_any_mut()
                .downcast_mut::<TimestampMicrosecondBuilder>()
                .unwrap();
            let v: Option<DateTime<Utc>> = row.try_get(tds_idx).map_err(|e| driver_err(&e))?;
            match v {
                Some(dt) => b.append_value(dt.timestamp_micros()),
                None => b.append_null(),
            }
        }
        DataType::Decimal128(_p, target_scale) => {
            let b = builder
                .as_any_mut()
                .downcast_mut::<Decimal128Builder>()
                .unwrap();
            let v: Option<Decimal> = row.try_get(tds_idx).map_err(|e| driver_err(&e))?;
            match v {
                Some(d) => {
                    let i = decimal_to_i128(d, *target_scale, name)?;
                    b.append_value(i);
                }
                None => b.append_null(),
            }
        }
        other => {
            return Err(Error::Schema(format!(
                "column `{name}`: unsupported Arrow type at runtime: {other:?}"
            )));
        }
    }
    Ok(())
}

/// `rust_decimal::Decimal` を Arrow Decimal128 の i128 表現に変換する。
///
/// rust_decimal の内部 scale と target_scale が一致しない場合は 10 のべき乗で
/// 補正する（rust_decimal の scale は 0..=28、Arrow Decimal128 の scale は 0..=38、
/// 補正のために 10^delta を i128 で乗除する）。
fn decimal_to_i128(d: Decimal, target_scale: i8, col: &str) -> Result<i128> {
    let mantissa = d.mantissa(); // i128
    let src_scale = i32::try_from(d.scale())
        .map_err(|_| driver_msg(format!("column `{col}`: decimal scale overflow")))?;
    let target_scale_i32 = i32::from(target_scale);
    let delta = target_scale_i32 - src_scale;
    match delta.cmp(&0) {
        std::cmp::Ordering::Equal => Ok(mantissa),
        std::cmp::Ordering::Greater => {
            let factor =
                i128::checked_pow(10, u32::try_from(delta).expect("delta>0")).ok_or_else(|| {
                    driver_msg(format!("column `{col}`: decimal scale upshift overflow"))
                })?;
            mantissa.checked_mul(factor).ok_or_else(|| {
                driver_msg(format!(
                    "column `{col}`: decimal mantissa overflow when scaling up"
                ))
            })
        }
        std::cmp::Ordering::Less => {
            let factor = i128::checked_pow(10, u32::try_from(-delta).expect("delta<0"))
                .ok_or_else(|| {
                    driver_msg(format!("column `{col}`: decimal scale downshift overflow"))
                })?;
            // Arrow Decimal128 は固定 scale なので、scale 縮小で精度落ちする場合は除算で丸める。
            // SQL Server 側で precision/scale が schema 通りに格納されている場合 delta は 0 になる
            // ため、ここに落ちるのは rust_decimal が trailing zero を内部的に縮約したケース。
            Ok(mantissa / factor)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_columns() -> Vec<ColumnInfo> {
        vec![
            ColumnInfo {
                name: "name".into(),
                arrow_type: DataType::Utf8,
                is_geometry: false,
                nullable: true,
            },
            ColumnInfo {
                name: "geom".into(),
                arrow_type: DataType::Binary,
                is_geometry: true,
                nullable: true,
            },
        ]
    }

    #[test]
    fn epsg_to_crs_handles_zero_and_negative() {
        assert_eq!(epsg_to_crs(0), None);
        assert_eq!(epsg_to_crs(-1), None);
        assert_eq!(epsg_to_crs(4326), Some(Crs::from_epsg(4326)));
    }

    #[test]
    fn build_select_sql_wraps_geometry_with_st_asbinary() {
        let sql = build_select_sql(&sample_columns(), 1, "[dbo].[t]");
        assert_eq!(
            sql,
            "SELECT [name], [geom].STAsBinary() AS [geom], \
             [geom].STSrid AS [geom__shpx_srid] FROM [dbo].[t]"
        );
    }

    #[test]
    fn decimal_to_i128_same_scale() {
        let d = Decimal::new(12_345, 2); // 123.45
        assert_eq!(decimal_to_i128(d, 2, "x").unwrap(), 12_345);
    }

    #[test]
    fn decimal_to_i128_scale_up() {
        let d = Decimal::new(123, 0); // 123 (scale 0)
                                      // target_scale=3 → 123_000 (123.000)
        assert_eq!(decimal_to_i128(d, 3, "x").unwrap(), 123_000);
    }

    #[test]
    fn decimal_to_i128_scale_down() {
        let d = Decimal::new(12_345, 4); // 1.2345 (scale 4)
                                         // target_scale=2 → 123 (truncate to 1.23)
        assert_eq!(decimal_to_i128(d, 2, "x").unwrap(), 123);
    }

}
