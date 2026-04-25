//! PostGIS の `LayerWriter` / `BulkLoadWriter` 実装。
//!
//! - **batch 経路** (`LayerWriter::write_batch`): 行単位 prepared INSERT を 1 トランザクション
//!   per `write_batch` で発行する。geometry 列は `Crs::epsg_code()` の SRID で EWKB 化して
//!   `ST_GeomFromEWKB($N)` に bind。Decimal128 は [`PgNumeric`] newtype を `ToSql` で bind。
//! - **bulk 経路** (`BulkLoadWriter::bulk_write`, v0.3 cycle 2): `COPY <table> FROM STDIN BINARY`
//!   を 1 接続 = 1 COPY セッションで張り、[`copy_binary::BulkRowEncoder`] が組み立てた行
//!   バイト列を `tokio_postgres::CopyInSink<Bytes>` に送り続ける。

use arrow_array::{
    cast::AsArray,
    types::{
        Date32Type, Decimal128Type, Float32Type, Float64Type, Int16Type, Int32Type, Int64Type,
        TimestampMicrosecondType, TimestampMillisecondType, TimestampNanosecondType,
        TimestampSecondType,
    },
    Array, RecordBatch,
};
use arrow_schema::{DataType, Field, SchemaRef, TimeUnit};
use bytes::{Bytes, BytesMut};
use chrono::{DateTime, Duration, NaiveDate, NaiveDateTime, TimeZone, Utc};
use futures_util::SinkExt;
use shpx_core::{
    schema::{find_geometry_column, GeometryMeta, GeometryType},
    BulkLoadWriter, Crs, Error, LayerWriter, OnLoss, Result, Uri, WriteOpts,
};
use shpx_geom::ewkb;
use tokio_postgres::{types::ToSql, Client, Statement};

use crate::conn;
use crate::copy_binary::{write_copy_header, write_copy_trailer, BulkRowEncoder, PgNumeric};
use crate::options::ResolvedWriteOpts;
use crate::runtime::runtime;
use crate::type_map::{arrow_to_decl, geom_type_to_decl_name};
use crate::util::{
    apply_on_loss, driver_err, driver_msg, loss_kind, primitive, quote_ident, quote_qualified,
};

/// bulk 経路で 1 回の `sink.send` に詰める batch 数（行ではなく batch 数）。`feed` を
/// 呼びまくると per-batch overhead が支配するので、ある程度の塊で送る。`BytesMut` を
/// reuse するため allocation は溜まらない。
const COPY_FLUSH_THRESHOLD_BYTES: usize = 64 * 1024;

pub struct PostgisWriter {
    client: Option<Client>,
    schema: SchemaRef,
    geom_index: usize,
    /// geometry 列を除く属性列の Arrow インデックス。
    attr_indices: Vec<usize>,
    /// open() で 1 度だけ prepare した INSERT。`Statement` は内部 Arc なので Clone は安価。
    insert_stmt: Statement,
    qualified: String,
    srid: i32,
}

impl PostgisWriter {
    pub fn open(uri: &Uri, schema: SchemaRef, crs: Option<&Crs>, opts: &WriteOpts) -> Result<Self> {
        let resolved = ResolvedWriteOpts::resolve(uri, opts)?;
        let qualified = quote_qualified(&resolved.schema, &resolved.table);

        let (geom_index, _, geom_meta) = find_geometry_column(&schema)?
            .ok_or_else(|| Error::Schema("no geometry column for PostGIS writer".to_string()))?;

        let srid = resolve_srid(crs, &geom_meta, opts.on_loss)?;

        let attr_indices: Vec<usize> = (0..schema.fields().len())
            .filter(|i| *i != geom_index)
            .collect();

        let client = conn::connect(&resolved.url)?;

        if resolved.overwrite {
            // CASCADE は付けない（依存ビュー等を勝手に巻き込まないため）。
            conn::batch_execute(&client, &format!("DROP TABLE IF EXISTS {qualified}"))?;
        }

        let create_sql = build_create_table_sql(
            &schema,
            &attr_indices,
            geom_index,
            &qualified,
            geom_meta.geometry_type,
            srid,
        )?;
        conn::batch_execute(&client, &create_sql)?;

        let insert_sql = build_insert_sql(&schema, &attr_indices, geom_index, &qualified);
        let insert_stmt = conn::prepare(&client, &insert_sql)?;

        Ok(Self {
            client: Some(client),
            schema,
            geom_index,
            attr_indices,
            insert_stmt,
            qualified,
            srid,
        })
    }
}

impl LayerWriter for PostgisWriter {
    fn write_batch(&mut self, batch: &RecordBatch) -> Result<()> {
        // self の field を split borrow して async block 内で参照する。clone を毎 batch 取らない。
        let Self {
            client,
            schema,
            geom_index,
            attr_indices,
            insert_stmt,
            srid,
            ..
        } = self;
        let client = client
            .as_mut()
            .ok_or_else(|| driver_msg("write_batch called after finish"))?;
        let rt = runtime()?;

        rt.block_on(async {
            let tx = client.transaction().await.map_err(|e| driver_err(&e))?;
            for row in 0..batch.num_rows() {
                let owned = build_row_params(schema, batch, attr_indices, *geom_index, row, *srid)?;
                let refs: Vec<&(dyn ToSql + Sync)> =
                    owned.iter().map(|b| &**b as &(dyn ToSql + Sync)).collect();
                tx.execute(insert_stmt, &refs)
                    .await
                    .map_err(|e| driver_err(&e))?;
            }
            tx.commit().await.map_err(|e| driver_err(&e))?;
            Ok::<_, Error>(())
        })
    }

    fn finish(mut self: Box<Self>) -> Result<()> {
        // tokio_postgres::Client は Drop で内部 channel を閉じ、別 task の Connection が
        // 終了する。明示的な close API は無いので drop に任せる。
        let _ = self.client.take();
        Ok(())
    }
}

impl BulkLoadWriter for PostgisWriter {
    fn bulk_write(&mut self, batches: &mut dyn Iterator<Item = Result<RecordBatch>>) -> Result<()> {
        let Self {
            client,
            schema,
            geom_index,
            attr_indices,
            qualified,
            srid,
            ..
        } = self;
        let client = client
            .as_mut()
            .ok_or_else(|| driver_msg("bulk_write called after finish"))?;

        let copy_sql = build_copy_sql(schema, attr_indices, *geom_index, qualified);
        let encoder =
            BulkRowEncoder::new(schema.clone(), attr_indices.clone(), *geom_index, *srid)?;

        let rt = runtime()?;
        rt.block_on(async {
            let sink = client
                .copy_in::<_, Bytes>(copy_sql.as_str())
                .await
                .map_err(|e| driver_err(&e))?;
            futures_util::pin_mut!(sink);

            // 先頭にヘッダ。
            let mut buf = BytesMut::with_capacity(COPY_FLUSH_THRESHOLD_BYTES);
            write_copy_header(&mut buf);

            for batch_res in batches {
                let batch = batch_res?;
                for row in 0..batch.num_rows() {
                    encoder.encode_row(&batch, row, &mut buf)?;
                    if buf.len() >= COPY_FLUSH_THRESHOLD_BYTES {
                        let chunk = buf.split().freeze();
                        sink.send(chunk).await.map_err(|e| driver_err(&e))?;
                    }
                }
            }

            // 末尾トレーラ + 残バッファを送る。
            write_copy_trailer(&mut buf);
            if !buf.is_empty() {
                sink.send(buf.freeze()).await.map_err(|e| driver_err(&e))?;
            }
            // sink.finish() はサーバ側で COPY を確定させる（暗黙トランザクションで commit）。
            sink.as_mut().finish().await.map_err(|e| driver_err(&e))?;
            Ok::<_, Error>(())
        })
    }
}

impl Drop for PostgisWriter {
    fn drop(&mut self) {
        if self.client.is_some() {
            tracing::warn!(target: "shpx::postgis", table = %self.qualified, "PostgisWriter dropped without finish()");
        }
    }
}

/// `Crs` から PostGIS の SRID を解決する。
///
/// 優先順位:
/// 1. open_write 引数の明示 CRS
/// 2. schema field metadata の CRS
/// 3. なし → `apply_on_loss(missing-crs-on-postgis)` で `error` なら停止、`warn`/`skip` なら srid=0
fn resolve_srid(crs_arg: Option<&Crs>, geom_meta: &GeometryMeta, on_loss: OnLoss) -> Result<i32> {
    let crs: Option<Crs> = crs_arg.cloned().or_else(|| geom_meta.crs.clone());
    if let Some(code) = crs.as_ref().and_then(Crs::epsg_code) {
        i32::try_from(code).map_err(|_| Error::Crs(format!("EPSG code {code} exceeds i32 range")))
    } else {
        // CRS 不明 or EPSG 化できない場合は srid=0 にフォールバック。
        // 未登録 EPSG の `spatial_ref_sys` 自動 INSERT は未対応。
        let _ = apply_on_loss(loss_kind::MISSING_CRS_ON_POSTGIS, "<srs>", on_loss)?;
        Ok(0)
    }
}

fn build_create_table_sql(
    schema: &SchemaRef,
    attr_indices: &[usize],
    geom_index: usize,
    qualified: &str,
    geom_type: GeometryType,
    srid: i32,
) -> Result<String> {
    let mut cols: Vec<String> = Vec::with_capacity(attr_indices.len() + 1);
    for &i in attr_indices {
        let f = schema.field(i);
        let decl = arrow_to_decl(f.data_type())?;
        cols.push(format!("{} {decl}", quote_ident(f.name())));
    }
    let geom_field = schema.field(geom_index);
    cols.push(format!(
        "{} geometry({}, {srid})",
        quote_ident(geom_field.name()),
        geom_type_to_decl_name(geom_type)
    ));
    Ok(format!(
        "CREATE TABLE {qualified} (\n  {}\n)",
        cols.join(",\n  ")
    ))
}

/// `COPY <qualified> ("col1", ..., "geom") FROM STDIN BINARY` を組み立てる。
/// 列順は `attr_indices` の後に geometry 列という規約で、bulk encoder の `field_order` と
/// 一致させる必要がある。
fn build_copy_sql(
    schema: &SchemaRef,
    attr_indices: &[usize],
    geom_index: usize,
    qualified: &str,
) -> String {
    let mut col_names: Vec<String> = attr_indices
        .iter()
        .map(|&i| quote_ident(schema.field(i).name()))
        .collect();
    col_names.push(quote_ident(schema.field(geom_index).name()));
    format!(
        "COPY {qualified} ({}) FROM STDIN (FORMAT BINARY)",
        col_names.join(", ")
    )
}

fn build_insert_sql(
    schema: &SchemaRef,
    attr_indices: &[usize],
    geom_index: usize,
    qualified: &str,
) -> String {
    let mut col_names: Vec<String> = attr_indices
        .iter()
        .map(|&i| quote_ident(schema.field(i).name()))
        .collect();
    col_names.push(quote_ident(schema.field(geom_index).name()));

    let mut placeholders: Vec<String> = (1..=attr_indices.len()).map(|i| format!("${i}")).collect();
    // geometry 列は ST_GeomFromEWKB でラップする。EWKB は SRID を埋め込んでいるので
    // 第 2 引数の SRID は不要。
    let geom_param = attr_indices.len() + 1;
    placeholders.push(format!("ST_GeomFromEWKB(${geom_param})"));

    format!(
        "INSERT INTO {qualified} ({}) VALUES ({})",
        col_names.join(", "),
        placeholders.join(", ")
    )
}

/// 1 行分の bind 値（`Box<dyn ToSql + Sync>` のベクタ）を作る。
fn build_row_params(
    schema: &SchemaRef,
    batch: &RecordBatch,
    attr_indices: &[usize],
    geom_index: usize,
    row: usize,
    srid: i32,
) -> Result<Vec<Box<dyn ToSql + Sync>>> {
    let mut params: Vec<Box<dyn ToSql + Sync>> = Vec::with_capacity(attr_indices.len() + 1);
    for &i in attr_indices {
        let field = schema.field(i);
        params.push(arrow_to_pg_value(field, batch.column(i).as_ref(), row)?);
    }

    let geom_arr = batch.column(geom_index).as_binary::<i32>();
    let geom_param: Box<dyn ToSql + Sync> = if geom_arr.is_null(row) {
        Box::new(Option::<Vec<u8>>::None)
    } else {
        let wkb_bytes = geom_arr.value(row);
        let ewkb_bytes = ewkb::encode_with_srid(wkb_bytes, srid)?;
        Box::new(Some(ewkb_bytes))
    };
    params.push(geom_param);
    Ok(params)
}

/// Arrow の 1 セル値を `Box<dyn ToSql + Sync>` に変換する。
///
/// NULL は `Option::<T>::None` として渡す（型を保つため、DataType に応じて T を変える）。
fn arrow_to_pg_value(
    field: &Field,
    array: &dyn Array,
    row: usize,
) -> Result<Box<dyn ToSql + Sync>> {
    if array.is_null(row) {
        return Ok(make_null(field.data_type()));
    }
    let name = field.name();
    Ok(match field.data_type() {
        DataType::Boolean => Box::new(Some(array.as_boolean().value(row))),
        DataType::Int16 => Box::new(Some(primitive::<Int16Type>(array, row))),
        DataType::Int32 => Box::new(Some(primitive::<Int32Type>(array, row))),
        DataType::Int64 => Box::new(Some(primitive::<Int64Type>(array, row))),
        DataType::Float32 => Box::new(Some(primitive::<Float32Type>(array, row))),
        DataType::Float64 => Box::new(Some(primitive::<Float64Type>(array, row))),
        DataType::Utf8 => Box::new(Some(array.as_string::<i32>().value(row).to_string())),
        DataType::LargeUtf8 => Box::new(Some(array.as_string::<i64>().value(row).to_string())),
        DataType::Binary => Box::new(Some(array.as_binary::<i32>().value(row).to_vec())),
        DataType::LargeBinary => Box::new(Some(array.as_binary::<i64>().value(row).to_vec())),
        DataType::Date32 => {
            let days = primitive::<Date32Type>(array, row);
            let nd = NaiveDate::from_ymd_opt(1970, 1, 1).expect("epoch")
                + Duration::days(i64::from(days));
            Box::new(Some(nd))
        }
        DataType::Timestamp(unit, tz) => {
            let ndt = arrow_ts_to_naive(array, row, *unit, name)?;
            if tz.is_none() {
                Box::new(Some(ndt))
            } else {
                // 任意 tz 文字列の解釈は未実装。常に UTC として書き出す。
                let dt: DateTime<Utc> = Utc.from_utc_datetime(&ndt);
                Box::new(Some(dt))
            }
        }
        DataType::Decimal128(_p, s) => {
            let v: i128 = primitive::<Decimal128Type>(array, row);
            let scale = u8::try_from(*s).map_err(|_| {
                Error::Schema(format!(
                    "field `{name}`: Decimal128 scale {s} not supported (must be 0..=38)"
                ))
            })?;
            Box::new(Some(PgNumeric::from_i128_scale(v, scale)))
        }
        other => {
            return Err(Error::Schema(format!(
                "field `{name}`: unsupported Arrow type for PostGIS writer: {other:?}"
            )));
        }
    })
}

fn arrow_ts_to_naive(
    array: &dyn Array,
    row: usize,
    unit: TimeUnit,
    field: &str,
) -> Result<NaiveDateTime> {
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
    // u32 に収まる。
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let nanos = (micros.rem_euclid(1_000_000) * 1_000) as u32;
    chrono::DateTime::<Utc>::from_timestamp(secs, nanos)
        .map(|dt| dt.naive_utc())
        .ok_or_else(|| driver_msg(format!("field `{field}`: invalid timestamp value")))
}

/// DataType に応じた型付き NULL を作る。`tokio_postgres` の `Option::<T>::None` ToSql は
/// PostgreSQL の NULL に変換される。型を間違えると prepare 段階の型推論と衝突するので、
/// CREATE TABLE と整合する型で None を返す。
fn make_null(dt: &DataType) -> Box<dyn ToSql + Sync> {
    match dt {
        DataType::Boolean => Box::new(Option::<bool>::None),
        DataType::Int16 => Box::new(Option::<i16>::None),
        DataType::Int32 => Box::new(Option::<i32>::None),
        DataType::Int64 => Box::new(Option::<i64>::None),
        DataType::Float32 => Box::new(Option::<f32>::None),
        DataType::Float64 => Box::new(Option::<f64>::None),
        DataType::Utf8 | DataType::LargeUtf8 => Box::new(Option::<String>::None),
        DataType::Binary | DataType::LargeBinary => Box::new(Option::<Vec<u8>>::None),
        DataType::Date32 => Box::new(Option::<NaiveDate>::None),
        DataType::Timestamp(_, None) => Box::new(Option::<NaiveDateTime>::None),
        DataType::Timestamp(_, Some(_)) => Box::new(Option::<DateTime<Utc>>::None),
        DataType::Decimal128(_, _) => Box::new(Option::<PgNumeric>::None),
        // arrow_to_decl が同じ DataType セットを reject するので、ここには到達しない。
        // 未対応型を見たら schema validation のバグなので fail-fast で止める。
        other => unreachable!("make_null reached for unsupported DataType: {other:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Arc;

    use arrow_schema::{Field as AField, Schema as ASchema};
    use shpx_core::schema::{GeometryMeta, GeometryType, GEOMETRY_META_KEY};

    fn schema_with_geom(extras: Vec<AField>, gt: GeometryType, crs: Option<Crs>) -> SchemaRef {
        let mut fields = extras;
        let mut g = AField::new("geom", DataType::Binary, true);
        let mut m = HashMap::new();
        m.insert(
            GEOMETRY_META_KEY.to_string(),
            GeometryMeta::wkb(gt, crs).to_json().unwrap(),
        );
        g.set_metadata(m);
        fields.push(g);
        Arc::new(ASchema::new(fields))
    }

    #[test]
    fn build_create_table_sql_emits_attr_then_geom() {
        let s = schema_with_geom(
            vec![
                AField::new("name", DataType::Utf8, true),
                AField::new("count", DataType::Int64, true),
            ],
            GeometryType::Point,
            Some(Crs::from_epsg(4326)),
        );
        let attrs = vec![0, 1];
        let sql = build_create_table_sql(
            &s,
            &attrs,
            2,
            "\"public\".\"places\"",
            GeometryType::Point,
            4326,
        )
        .unwrap();
        assert!(sql.contains("\"name\" text"));
        assert!(sql.contains("\"count\" bigint"));
        assert!(sql.contains("\"geom\" geometry(Point, 4326)"));
        assert!(sql.starts_with("CREATE TABLE \"public\".\"places\""));
    }

    #[test]
    fn build_insert_sql_uses_st_geom_from_ewkb() {
        let s = schema_with_geom(
            vec![AField::new("v", DataType::Int32, true)],
            GeometryType::Point,
            Some(Crs::from_epsg(4326)),
        );
        let attrs = vec![0];
        let sql = build_insert_sql(&s, &attrs, 1, "\"public\".\"t\"");
        assert_eq!(
            sql,
            "INSERT INTO \"public\".\"t\" (\"v\", \"geom\") VALUES ($1, ST_GeomFromEWKB($2))"
        );
    }

    #[test]
    fn resolve_srid_from_arg_overrides_meta() {
        let meta = GeometryMeta::wkb(GeometryType::Point, Some(Crs::from_epsg(4326)));
        let srid = resolve_srid(Some(&Crs::from_epsg(3857)), &meta, OnLoss::Error).unwrap();
        assert_eq!(srid, 3857);
    }

    #[test]
    fn resolve_srid_falls_back_to_meta() {
        let meta = GeometryMeta::wkb(GeometryType::Point, Some(Crs::from_epsg(4326)));
        let srid = resolve_srid(None, &meta, OnLoss::Error).unwrap();
        assert_eq!(srid, 4326);
    }

    #[test]
    fn resolve_srid_missing_with_error_aborts() {
        let meta = GeometryMeta::wkb(GeometryType::Point, None);
        assert!(resolve_srid(None, &meta, OnLoss::Error).is_err());
    }

    #[test]
    fn resolve_srid_missing_with_warn_returns_zero() {
        let meta = GeometryMeta::wkb(GeometryType::Point, None);
        assert_eq!(resolve_srid(None, &meta, OnLoss::Warn).unwrap(), 0);
    }
}
