//! SQL Server reader/writer (table モード) の往復統合テスト。
//!
//! `SHPX_TEST_SQLSERVER_URL` 環境変数が設定されている場合のみ実行する（未設定なら
//! `eprintln!` を 1 行出して `return`）。CI では `services.mssql` を立てて
//! `SHPX_TEST_SQLSERVER_URL=mssql://sa:Shpx_test_pw1!@localhost:1433/shpx_test` を渡す。
//!
//! ローカル実行例:
//! ```bash
//! docker compose up -d mssql
//! docker exec shpx-mssql /opt/mssql-tools18/bin/sqlcmd \
//!   -S localhost -U sa -P 'Shpx_test_pw1!' -C \
//!   -Q "IF DB_ID('shpx_test') IS NULL CREATE DATABASE shpx_test"
//! SHPX_TEST_SQLSERVER_URL='mssql://sa:Shpx_test_pw1!@localhost:1433/shpx_test' \
//!     cargo test -p shpx-driver-sqlserver --locked
//! ```

use std::sync::Arc;

use arrow_array::{
    builder::{
        BinaryBuilder, Float64Builder, Int32Builder, StringBuilder, TimestampMicrosecondBuilder,
    },
    cast::AsArray,
    types::{Float64Type, Int32Type, TimestampMicrosecondType},
    Array, ArrayRef, RecordBatch,
};
use arrow_schema::{DataType, Field, TimeUnit};
use shpx_core::{schema::GeometryType, Crs, Driver, ReadOpts, Uri};
use shpx_driver_sqlserver::SqlServerDriver;
use shpx_geom::wkb::{self, Geom};

mod common;
use common::{cleanup, mssql_url, schema_with_geom, unique_table, uri_with_table, write_opts};

#[test]
fn point_with_attributes_roundtrip() {
    let Some(url) = mssql_url() else {
        eprintln!("SHPX_TEST_SQLSERVER_URL unset; skipping integration test");
        return;
    };
    let table = unique_table("shpx_pt");
    let uri = uri_with_table(&url, &table);

    let schema = schema_with_geom(
        vec![
            Field::new("name", DataType::Utf8, true),
            Field::new("count", DataType::Int32, true),
        ],
        GeometryType::Point,
        Some(Crs::from_epsg(4326)),
    );

    let mut name_b = StringBuilder::new();
    name_b.append_value("alpha");
    name_b.append_value("beta");
    name_b.append_null();
    let mut count_b = Int32Builder::new();
    count_b.append_value(1);
    count_b.append_value(2);
    count_b.append_value(3);
    let mut geom_b = BinaryBuilder::new();
    geom_b.append_value(wkb::encode(&Geom::Point(1.0, 2.0)).unwrap());
    geom_b.append_value(wkb::encode(&Geom::Point(3.5, -4.5)).unwrap());
    geom_b.append_null();

    let cols: Vec<ArrayRef> = vec![
        Arc::new(name_b.finish()),
        Arc::new(count_b.finish()),
        Arc::new(geom_b.finish()),
    ];
    let batch = RecordBatch::try_new(schema.clone(), cols).unwrap();

    let driver = SqlServerDriver::new();
    let mut w = driver
        .open_write(
            &uri,
            schema.clone(),
            Some(Crs::from_epsg(4326)),
            &write_opts(),
        )
        .expect("open_write");
    w.write_batch(&batch).expect("write_batch");
    w.finish().expect("finish");

    let mut r = driver
        .open_read(&uri, &ReadOpts::default())
        .expect("open_read");
    assert_eq!(r.crs().cloned(), Some(Crs::from_epsg(4326)));
    let back_schema = r.schema();
    assert_eq!(back_schema.field(0).name(), "name");
    assert_eq!(back_schema.field(1).name(), "count");
    assert_eq!(back_schema.field(2).name(), "geom");

    let batches: Vec<_> = r
        .batches()
        .collect::<Result<_, _>>()
        .expect("collect batches");
    assert_eq!(batches.len(), 1);
    let b = &batches[0];
    assert_eq!(b.num_rows(), 3);

    let name_back = b.column(0).as_string::<i32>();
    assert_eq!(name_back.value(0), "alpha");
    assert_eq!(name_back.value(1), "beta");
    assert!(name_back.is_null(2));

    let count_back = b.column(1).as_primitive::<Int32Type>();
    assert_eq!(count_back.value(0), 1);
    assert_eq!(count_back.value(1), 2);
    assert_eq!(count_back.value(2), 3);

    let geom_back = b.column(2).as_binary::<i32>();
    assert_eq!(
        wkb::decode(geom_back.value(0)).unwrap(),
        Geom::Point(1.0, 2.0)
    );
    assert_eq!(
        wkb::decode(geom_back.value(1)).unwrap(),
        Geom::Point(3.5, -4.5)
    );
    assert!(geom_back.is_null(2));

    drop(r);
    cleanup(&url, &table);
}

#[test]
fn polygon_and_float_and_timestamp_roundtrip() {
    let Some(url) = mssql_url() else {
        eprintln!("SHPX_TEST_SQLSERVER_URL unset; skipping integration test");
        return;
    };
    let table = unique_table("shpx_poly");
    let uri = uri_with_table(&url, &table);

    let schema = schema_with_geom(
        vec![
            Field::new("score", DataType::Float64, true),
            Field::new(
                "ts",
                DataType::Timestamp(TimeUnit::Microsecond, Some("UTC".into())),
                true,
            ),
        ],
        GeometryType::Polygon,
        Some(Crs::from_epsg(3857)),
    );

    let mut score_b = Float64Builder::new();
    score_b.append_value(1.5);
    score_b.append_null();

    let mut ts_b = TimestampMicrosecondBuilder::new().with_timezone("UTC");
    let micros: i64 = 1_777_680_000_000_000;
    ts_b.append_value(micros);
    ts_b.append_value(micros + 86_400_000_000);

    let polygon = Geom::Polygon(vec![vec![
        (0.0, 0.0),
        (10.0, 0.0),
        (10.0, 10.0),
        (0.0, 10.0),
        (0.0, 0.0),
    ]]);
    let polygon2 = Geom::Polygon(vec![vec![
        (1.0, 1.0),
        (2.0, 1.0),
        (2.0, 2.0),
        (1.0, 2.0),
        (1.0, 1.0),
    ]]);
    let mut geom_b = BinaryBuilder::new();
    geom_b.append_value(wkb::encode(&polygon).unwrap());
    geom_b.append_value(wkb::encode(&polygon2).unwrap());

    let cols: Vec<ArrayRef> = vec![
        Arc::new(score_b.finish()),
        Arc::new(ts_b.finish()),
        Arc::new(geom_b.finish()),
    ];
    let batch = RecordBatch::try_new(schema.clone(), cols).unwrap();

    let driver = SqlServerDriver::new();
    let mut w = driver
        .open_write(
            &uri,
            schema.clone(),
            Some(Crs::from_epsg(3857)),
            &write_opts(),
        )
        .expect("open_write");
    w.write_batch(&batch).expect("write_batch");
    w.finish().expect("finish");

    let mut r = driver
        .open_read(&uri, &ReadOpts::default())
        .expect("open_read");
    assert_eq!(r.crs().cloned(), Some(Crs::from_epsg(3857)));
    let batches: Vec<_> = r.batches().collect::<Result<_, _>>().unwrap();
    assert_eq!(batches.len(), 1);
    let b = &batches[0];
    assert_eq!(b.num_rows(), 2);

    let score_back = b.column(0).as_primitive::<Float64Type>();
    assert!((score_back.value(0) - 1.5).abs() < f64::EPSILON);
    assert!(score_back.is_null(1));

    let ts_back = b.column(1).as_primitive::<TimestampMicrosecondType>();
    assert_eq!(ts_back.value(0), micros);
    assert_eq!(ts_back.value(1), micros + 86_400_000_000);

    let geom_back = b.column(2).as_binary::<i32>();
    assert_eq!(wkb::decode(geom_back.value(0)).unwrap(), polygon);
    assert_eq!(wkb::decode(geom_back.value(1)).unwrap(), polygon2);

    drop(r);
    cleanup(&url, &table);
}

#[test]
fn binary_and_date_roundtrip() {
    let Some(url) = mssql_url() else {
        eprintln!("SHPX_TEST_SQLSERVER_URL unset; skipping integration test");
        return;
    };
    let table = unique_table("shpx_bin");
    let uri = uri_with_table(&url, &table);

    let schema = schema_with_geom(
        vec![
            Field::new("blob", DataType::Binary, true),
            Field::new("d", DataType::Date32, true),
        ],
        GeometryType::Point,
        Some(Crs::from_epsg(4326)),
    );

    let mut blob_b = BinaryBuilder::new();
    blob_b.append_value(b"hello");
    blob_b.append_value([0xff, 0x00, 0x42]);
    blob_b.append_null();

    let mut d_b = arrow_array::builder::Date32Builder::new();
    d_b.append_value(20_564);
    d_b.append_value(20_565);
    d_b.append_null();

    let mut geom_b = BinaryBuilder::new();
    for _ in 0..3 {
        geom_b.append_value(wkb::encode(&Geom::Point(0.0, 0.0)).unwrap());
    }

    let cols: Vec<ArrayRef> = vec![
        Arc::new(blob_b.finish()),
        Arc::new(d_b.finish()),
        Arc::new(geom_b.finish()),
    ];
    let batch = RecordBatch::try_new(schema.clone(), cols).unwrap();

    let driver = SqlServerDriver::new();
    let mut w = driver
        .open_write(
            &uri,
            schema.clone(),
            Some(Crs::from_epsg(4326)),
            &write_opts(),
        )
        .expect("open_write");
    w.write_batch(&batch).expect("write_batch");
    w.finish().expect("finish");

    let mut r = driver
        .open_read(&uri, &ReadOpts::default())
        .expect("open_read");
    let batches: Vec<_> = r.batches().collect::<Result<_, _>>().unwrap();
    assert_eq!(batches.len(), 1);
    let b = &batches[0];

    let blob_back = b.column(0).as_binary::<i32>();
    assert_eq!(blob_back.value(0), b"hello");
    assert_eq!(blob_back.value(1), &[0xff, 0x00, 0x42]);
    assert!(blob_back.is_null(2));

    let d_back = b.column(1).as_primitive::<arrow_array::types::Date32Type>();
    assert_eq!(d_back.value(0), 20_564);
    assert_eq!(d_back.value(1), 20_565);
    assert!(d_back.is_null(2));

    drop(r);
    cleanup(&url, &table);
}

/// multi-row VALUES の chunk 境界を跨ぐ batch 投入で全行が roundtrip することを確認する。
///
/// `chunk_rows` を実測してから「`chunk_rows * 2 + 1` 行」を投入することで、
/// 「full chunk × 2 + 末尾 remainder 1 行」のパスを通す。chunk loop の境界バグや
/// SQL placeholder 番号の連番ズレを検出する。
#[test]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::redundant_closure_for_method_calls
)]
fn multirow_values_chunk_boundary_roundtrip() {
    let Some(url) = mssql_url() else {
        eprintln!("SHPX_TEST_SQLSERVER_URL unset; skipping integration test");
        return;
    };
    let table = unique_table("shpx_chunk");
    let uri = uri_with_table(&url, &table);

    let schema = schema_with_geom(
        vec![Field::new("idx", DataType::Int32, false)],
        GeometryType::Point,
        Some(Crs::from_epsg(4326)),
    );

    // params_per_row = 1 (idx) + 2 (WKB + SRID) = 3。SQL Server 上限 2100 / margin 16
    // から chunk_rows = floor(2084 / 3) = 694。`chunk_rows * 2 + 1` = 1389 行を投入。
    let chunk_rows = shpx_rdb_common::multirow_chunk_rows(3, 2100, 16);
    let total = chunk_rows * 2 + 1;

    let mut idx_b = Int32Builder::new();
    let mut geom_b = BinaryBuilder::new();
    for i in 0..total {
        idx_b.append_value(i as i32);
        geom_b.append_value(wkb::encode(&Geom::Point(i as f64, -(i as f64))).unwrap());
    }
    let cols: Vec<ArrayRef> = vec![Arc::new(idx_b.finish()), Arc::new(geom_b.finish())];
    let batch = RecordBatch::try_new(schema.clone(), cols).unwrap();

    let driver = SqlServerDriver::new();
    let mut w = driver
        .open_write(
            &uri,
            schema.clone(),
            Some(Crs::from_epsg(4326)),
            &write_opts(),
        )
        .expect("open_write");
    w.write_batch(&batch).expect("write_batch");
    w.finish().expect("finish");

    let mut r = driver
        .open_read(&uri, &ReadOpts::default())
        .expect("open_read");
    let batches: Vec<_> = r.batches().collect::<Result<_, _>>().expect("read");
    let row_count: usize = batches.iter().map(|b| b.num_rows()).sum();
    assert_eq!(
        row_count, total,
        "all rows must roundtrip across chunk boundary"
    );

    // 最後の行 (末尾 remainder の境界) を検証して param 番号ズレが無いことを確認する。
    let last_batch = batches.last().expect("at least one batch");
    let last_row = last_batch.num_rows() - 1;
    let idx_back = last_batch.column(0).as_primitive::<Int32Type>();
    assert_eq!(idx_back.value(last_row), (total - 1) as i32);
    // schema_with_geom は extras + geom の順なので geom 列は index 1 (column 0 = idx, column 1 = geom)。
    // commit 9a33962 で test 追加時 column(2) と書いていたが SQL Server CI が初運転で OOB panic。
    let geom_back = last_batch.column(1).as_binary::<i32>();
    assert_eq!(
        wkb::decode(geom_back.value(last_row)).unwrap(),
        Geom::Point((total - 1) as f64, -((total - 1) as f64))
    );

    drop(r);
    cleanup(&url, &table);
}

#[test]
fn missing_table_param_errors() {
    // CRUD は走らないので env 不要。URL に `?table` が無いと open_read が即エラー。
    // ただし、SHPX_MSSQL_TABLE が偶然 env に設定されていれば成功してしまうので skip。
    if std::env::var(shpx_driver_sqlserver::options::ENV_TABLE).is_ok() {
        return;
    }
    let driver = SqlServerDriver::new();
    let uri = Uri::from_path("mssql://sa:pw@localhost/db".to_string());
    let err = driver.open_read(&uri, &ReadOpts::default()).err();
    assert!(err.is_some(), "open_read without ?table should fail");
}
