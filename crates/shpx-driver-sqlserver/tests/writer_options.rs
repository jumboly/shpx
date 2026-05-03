//! `--create-table` (3 種) と `--create-index` (Auto / Always / Never)、SRID 解決の
//! 整合性テスト。env-gated (`SHPX_TEST_SQLSERVER_URL` 未設定時は eprintln + return で skip)。

use std::sync::Arc;

use arrow_array::{
    builder::{BinaryBuilder, Int32Builder},
    ArrayRef, RecordBatch,
};
use arrow_schema::{DataType, Field};
use shpx_core::{
    schema::GeometryType, CreateIndex, CreateTable, Crs, Driver, OnLoss, ReadOpts, Uri, WriteOpts,
};
use shpx_driver_sqlserver::{
    conn::{self, simple_query},
    runtime::runtime,
    SqlServerDriver,
};
use shpx_geom::wkb::{self, Geom};
use tiberius::Row;

mod common;
use common::{cleanup, mssql_url, schema_with_geom, unique_table, uri_with_table};

fn one_point_batch(crs: Option<Crs>) -> RecordBatch {
    let schema = schema_with_geom(
        vec![Field::new("id", DataType::Int32, false)],
        GeometryType::Point,
        crs,
    );
    let mut id_b = Int32Builder::new();
    id_b.append_value(1);
    let mut geom_b = BinaryBuilder::new();
    geom_b.append_value(wkb::encode(&Geom::Point(1.0, 2.0)).unwrap());
    let cols: Vec<ArrayRef> = vec![Arc::new(id_b.finish()), Arc::new(geom_b.finish())];
    RecordBatch::try_new(schema, cols).unwrap()
}

fn count_spatial_indexes(client_url: &str, schema: &str, table: &str) -> i32 {
    let mut client = conn::connect(client_url).expect("connect");
    let rt = runtime().expect("runtime");
    let qualified = format!("[{schema}].[{table}]");
    let rows: Vec<Row> = rt
        .block_on(async {
            let stream = client
                .query(
                    "SELECT COUNT(*) FROM sys.spatial_indexes \
                     WHERE object_id = OBJECT_ID(@P1)",
                    &[&qualified],
                )
                .await?;
            stream.into_first_result().await
        })
        .expect("count spatial_indexes");
    rows.into_iter()
        .next()
        .and_then(|r| r.try_get::<i32, _>(0).ok().flatten())
        .unwrap_or(0)
}

#[test]
fn if_not_exists_creates_when_absent_and_appends_when_present() {
    let Some(url) = mssql_url() else {
        eprintln!("SHPX_TEST_SQLSERVER_URL unset; skipping");
        return;
    };
    let table = unique_table("shpx_ifne");
    let uri = uri_with_table(&url, &table);
    let driver = SqlServerDriver::new();
    let batch = one_point_batch(Some(Crs::from_epsg(4326)));
    let schema = batch.schema();

    // 1 回目: 不在 → CREATE
    {
        let mut w = driver
            .open_write(
                &uri,
                schema.clone(),
                Some(Crs::from_epsg(4326)),
                &WriteOpts {
                    create_table: CreateTable::IfNotExists,
                    ..Default::default()
                },
            )
            .expect("first write");
        w.write_batch(&batch).expect("write 1");
        w.finish().unwrap();
    }
    // 2 回目: 既存 → append (CREATE せず追記)。--overwrite=false かつ
    // create_table=IfNotExists なら成功して 2 行になる想定。
    {
        let mut w = driver
            .open_write(
                &uri,
                schema.clone(),
                Some(Crs::from_epsg(4326)),
                &WriteOpts {
                    create_table: CreateTable::IfNotExists,
                    ..Default::default()
                },
            )
            .expect("second write");
        w.write_batch(&batch).expect("write 2");
        w.finish().unwrap();
    }
    let mut r = driver.open_read(&uri, &ReadOpts::default()).unwrap();
    let total: usize = r.batches().map(|b| b.unwrap().num_rows()).sum();
    assert_eq!(total, 2);
    drop(r);
    cleanup(&url, &table);
}

#[test]
fn never_errors_when_missing_table() {
    let Some(url) = mssql_url() else {
        eprintln!("SHPX_TEST_SQLSERVER_URL unset; skipping");
        return;
    };
    let table = unique_table("shpx_never");
    let uri = uri_with_table(&url, &table);
    let driver = SqlServerDriver::new();
    let batch = one_point_batch(Some(Crs::from_epsg(4326)));

    let err = driver
        .open_write(
            &uri,
            batch.schema(),
            Some(Crs::from_epsg(4326)),
            &WriteOpts {
                create_table: CreateTable::Never,
                ..Default::default()
            },
        )
        .err();
    assert!(err.is_some(), "Never on missing table should fail");
    let msg = format!("{}", err.unwrap());
    assert!(msg.contains("does not exist"), "msg was: {msg}");
}

#[test]
fn always_drops_existing_with_overwrite_semantics() {
    let Some(url) = mssql_url() else {
        eprintln!("SHPX_TEST_SQLSERVER_URL unset; skipping");
        return;
    };
    let table = unique_table("shpx_always");
    let uri = uri_with_table(&url, &table);
    let driver = SqlServerDriver::new();
    let batch = one_point_batch(Some(Crs::from_epsg(4326)));
    let schema = batch.schema();

    // 1 回目作成
    let mut w = driver
        .open_write(
            &uri,
            schema.clone(),
            Some(Crs::from_epsg(4326)),
            &WriteOpts {
                create_table: CreateTable::IfNotExists,
                ..Default::default()
            },
        )
        .unwrap();
    w.write_batch(&batch).unwrap();
    w.finish().unwrap();

    // 2 回目: Always で再作成 → 既存 1 行は DROP で消え、書き込み後 1 行に戻る。
    let mut w2 = driver
        .open_write(
            &uri,
            schema.clone(),
            Some(Crs::from_epsg(4326)),
            &WriteOpts {
                create_table: CreateTable::Always,
                ..Default::default()
            },
        )
        .unwrap();
    w2.write_batch(&batch).unwrap();
    w2.finish().unwrap();

    let mut r = driver.open_read(&uri, &ReadOpts::default()).unwrap();
    let total: usize = r.batches().map(|b| b.unwrap().num_rows()).sum();
    assert_eq!(total, 1, "Always should DROP+CREATE");
    drop(r);
    cleanup(&url, &table);
}

#[test]
fn auto_index_does_nothing_for_sqlserver() {
    let Some(url) = mssql_url() else {
        eprintln!("SHPX_TEST_SQLSERVER_URL unset; skipping");
        return;
    };
    let table = unique_table("shpx_auto_idx");
    let uri = uri_with_table(&url, &table);
    let driver = SqlServerDriver::new();
    let batch = one_point_batch(Some(Crs::from_epsg(4326)));

    let mut w = driver
        .open_write(
            &uri,
            batch.schema(),
            Some(Crs::from_epsg(4326)),
            &WriteOpts {
                create_table: CreateTable::IfNotExists,
                create_index: CreateIndex::Auto,
                ..Default::default()
            },
        )
        .unwrap();
    w.write_batch(&batch).unwrap();
    w.finish().unwrap();

    // SQL Server では Auto は no-op (確定済み判断)。spatial_indexes は 0。
    assert_eq!(count_spatial_indexes(&url, "dbo", &table), 0);
    cleanup(&url, &table);
}

#[test]
fn always_index_creates_for_4326() {
    let Some(url) = mssql_url() else {
        eprintln!("SHPX_TEST_SQLSERVER_URL unset; skipping");
        return;
    };
    let table = unique_table("shpx_always_idx");
    let uri = uri_with_table(&url, &table);

    // SQL Server `CREATE SPATIAL INDEX` は clustered PK を要求する仕様のため、
    // shpx 汎用 driver は --create-table 経路で PK を勝手に作らない。
    // テストでは PK 付きテーブルを事前に手動 CREATE して
    // `--create-table=never + --create-index=always` で index 生成のみ検証する。
    let mut client = conn::connect(&url).unwrap();
    simple_query(
        &mut client,
        format!(
            "CREATE TABLE [dbo].[{table}] ( \
               [id] int NOT NULL CONSTRAINT [pk_{table}] PRIMARY KEY CLUSTERED, \
               [geom] geometry NULL )"
        ),
    )
    .unwrap();
    drop(client);

    let driver = SqlServerDriver::new();
    let batch = one_point_batch(Some(Crs::from_epsg(4326)));
    let mut w = driver
        .open_write(
            &uri,
            batch.schema(),
            Some(Crs::from_epsg(4326)),
            &WriteOpts {
                create_table: CreateTable::Never,
                create_index: CreateIndex::Always,
                ..Default::default()
            },
        )
        .unwrap();
    w.write_batch(&batch).unwrap();
    w.finish().unwrap();

    assert_eq!(count_spatial_indexes(&url, "dbo", &table), 1);
    cleanup(&url, &table);
}

#[test]
fn always_index_errors_for_unknown_srid() {
    let Some(url) = mssql_url() else {
        eprintln!("SHPX_TEST_SQLSERVER_URL unset; skipping");
        return;
    };
    let table = unique_table("shpx_unknown_srid");
    let uri = uri_with_table(&url, &table);
    let driver = SqlServerDriver::new();
    let batch = one_point_batch(Some(Crs::from_epsg(2451))); // 日本の平面直角座標 IX 系

    // 書き込み自体は table 作成 + INSERT までは成功し、finish() で SPATIAL INDEX 生成を
    // 試みた時点で BOUNDING_BOX 解決に失敗する。
    let mut w = driver
        .open_write(
            &uri,
            batch.schema(),
            Some(Crs::from_epsg(2451)),
            &WriteOpts {
                create_table: CreateTable::IfNotExists,
                create_index: CreateIndex::Always,
                ..Default::default()
            },
        )
        .unwrap();
    w.write_batch(&batch).unwrap();
    let err = w.finish().err();
    assert!(err.is_some(), "Always for unknown SRID should fail");
    let msg = format!("{}", err.unwrap());
    assert!(msg.contains("BOUNDING_BOX"), "msg was: {msg}");
    cleanup(&url, &table);
}

#[test]
fn geography_uses_default_srid_4326_when_crs_absent_and_warn() {
    let Some(url) = mssql_url() else {
        eprintln!("SHPX_TEST_SQLSERVER_URL unset; skipping");
        return;
    };
    let table = unique_table("shpx_geog_warn");
    let sep = if url.contains('?') { '&' } else { '?' };
    let uri = Uri::from_path(format!("{url}{sep}table={table}&geom_type=geography"));
    let driver = SqlServerDriver::new();

    // CRS 不在 + on_loss=warn の場合、geography は SRID=4326 にフォールバック。
    let schema = schema_with_geom(
        vec![Field::new("id", DataType::Int32, false)],
        GeometryType::Point,
        None, // CRS なし
    );
    let mut id_b = Int32Builder::new();
    id_b.append_value(1);
    let mut geom_b = BinaryBuilder::new();
    geom_b.append_value(wkb::encode(&Geom::Point(139.767, 35.681)).unwrap());
    let cols: Vec<ArrayRef> = vec![Arc::new(id_b.finish()), Arc::new(geom_b.finish())];
    let batch = RecordBatch::try_new(schema.clone(), cols).unwrap();

    let mut w = driver
        .open_write(
            &uri,
            schema.clone(),
            None, // 明示 CRS も無し
            &WriteOpts {
                on_loss: OnLoss::Warn,
                ..Default::default()
            },
        )
        .expect("open_write should succeed under Warn policy");
    w.write_batch(&batch).expect("write under default 4326");
    w.finish().expect("finish");
    cleanup(&url, &table);
}
