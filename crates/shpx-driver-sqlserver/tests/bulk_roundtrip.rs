//! SQL Server staging bulk writer (`#shpx_stage_<uuid>` 経由) の往復統合テスト (v0.4 cycle 2)。
//!
//! `SHPX_TEST_SQLSERVER_URL` 環境変数が設定されている場合のみ実行する。CI では
//! `services.mssql` 経由で実 DB に対して走る。
//!
//! ローカル実行例: tests/roundtrip.rs と同じ docker compose 起動 + DB 作成手順を参照。
//! chunk size 検証用に `SHPX_MSSQL_BULK_CHUNK=5` 等を上書きしてテストできる。

use std::sync::Arc;

use arrow_array::{
    builder::{
        BinaryBuilder, Decimal128Builder, Int32Builder, Int64Builder, StringBuilder,
        TimestampMicrosecondBuilder,
    },
    cast::AsArray,
    types::{Decimal128Type, Int64Type, TimestampMicrosecondType},
    Array, ArrayRef, RecordBatch,
};
use arrow_schema::{DataType, Field, TimeUnit};
use shpx_core::{schema::GeometryType, BulkLoadWriter, Crs, Driver, ReadOpts, Uri};
use shpx_driver_sqlserver::SqlServerDriver;
use shpx_geom::wkb::{self, Geom};

mod common;
use common::{cleanup, mssql_url, schema_with_geom, unique_table, uri_with_table, write_opts};

fn make_uri_with_geom_type(base_url: &str, table: &str, geom_type: &str) -> Uri {
    let sep = if base_url.contains('?') { '&' } else { '?' };
    Uri::from_path(format!("{base_url}{sep}table={table}&geom_type={geom_type}"))
}

#[test]
fn bulk_point_with_attributes_roundtrip() {
    let Some(url) = mssql_url() else {
        eprintln!("SHPX_TEST_SQLSERVER_URL unset; skipping bulk integration test");
        return;
    };
    let table = unique_table("shpx_bulk_pt");
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
    count_b.append_value(10);
    count_b.append_value(20);
    count_b.append_value(30);
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
        .open_bulk_write(
            &uri,
            schema.clone(),
            Some(Crs::from_epsg(4326)),
            &write_opts(),
        )
        .expect("open_bulk_write")
        .expect("driver supports bulk_write");
    let mut iter = std::iter::once(Ok(batch));
    BulkLoadWriter::bulk_write(w.as_mut(), &mut iter).expect("bulk_write");
    w.finish().expect("finish");

    // 読み戻し
    let mut r = driver
        .open_read(&uri, &ReadOpts::default())
        .expect("open_read");
    assert_eq!(r.crs().cloned(), Some(Crs::from_epsg(4326)));
    let batches: Vec<_> = r.batches().collect::<Result<_, _>>().unwrap();
    assert_eq!(batches.len(), 1);
    let b = &batches[0];
    assert_eq!(b.num_rows(), 3);

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
fn bulk_decimal_38_10_bit_identical() {
    let Some(url) = mssql_url() else {
        eprintln!("SHPX_TEST_SQLSERVER_URL unset; skipping bulk decimal test");
        return;
    };
    let table = unique_table("shpx_bulk_dec");
    let uri = uri_with_table(&url, &table);

    let schema = schema_with_geom(
        vec![Field::new("amount", DataType::Decimal128(38, 10), true)],
        GeometryType::Point,
        Some(Crs::from_epsg(4326)),
    );

    // 既知の i128 値で bit-identical 確認。`123_456_789_012_345.6789012345` 相当。
    let val: i128 = 1_234_567_890_123_456_789_012_345;
    let val_neg: i128 = -1_234_567_890_123_456_789_012_345;

    let mut amt_b = Decimal128Builder::new()
        .with_precision_and_scale(38, 10)
        .unwrap();
    amt_b.append_value(val);
    amt_b.append_value(val_neg);
    amt_b.append_null();

    let mut geom_b = BinaryBuilder::new();
    for _ in 0..3 {
        geom_b.append_value(wkb::encode(&Geom::Point(0.0, 0.0)).unwrap());
    }

    let cols: Vec<ArrayRef> = vec![Arc::new(amt_b.finish()), Arc::new(geom_b.finish())];
    let batch = RecordBatch::try_new(schema.clone(), cols).unwrap();

    let driver = SqlServerDriver::new();
    let mut w = driver
        .open_bulk_write(
            &uri,
            schema.clone(),
            Some(Crs::from_epsg(4326)),
            &write_opts(),
        )
        .expect("open_bulk_write")
        .expect("driver supports bulk_write");
    let mut iter = std::iter::once(Ok(batch));
    BulkLoadWriter::bulk_write(w.as_mut(), &mut iter).expect("bulk_write");
    w.finish().expect("finish");

    let mut r = driver
        .open_read(&uri, &ReadOpts::default())
        .expect("open_read");
    let batches: Vec<_> = r.batches().collect::<Result<_, _>>().unwrap();
    let b = &batches[0];

    let amt_back = b.column(0).as_primitive::<Decimal128Type>();
    assert_eq!(amt_back.value(0), val, "positive decimal must be bit-identical");
    assert_eq!(amt_back.value(1), val_neg, "negative decimal must be bit-identical");
    assert!(amt_back.is_null(2));

    drop(r);
    cleanup(&url, &table);
}

#[test]
fn bulk_timestamptz_and_int64_bit_identical() {
    let Some(url) = mssql_url() else {
        eprintln!("SHPX_TEST_SQLSERVER_URL unset; skipping bulk timestamp test");
        return;
    };
    let table = unique_table("shpx_bulk_ts");
    let uri = uri_with_table(&url, &table);

    let schema = schema_with_geom(
        vec![
            Field::new("amount", DataType::Int64, false),
            Field::new(
                "ts",
                DataType::Timestamp(TimeUnit::Microsecond, Some("UTC".into())),
                true,
            ),
        ],
        GeometryType::Point,
        Some(Crs::from_epsg(4326)),
    );

    let mut amount_b = Int64Builder::new();
    amount_b.append_value(1);
    amount_b.append_value(i64::MAX);
    let mut ts_b = TimestampMicrosecondBuilder::new().with_timezone("UTC");
    let micros: i64 = 1_777_680_000_000_000;
    ts_b.append_value(micros);
    ts_b.append_null();
    let mut geom_b = BinaryBuilder::new();
    geom_b.append_value(wkb::encode(&Geom::Point(0.0, 0.0)).unwrap());
    geom_b.append_value(wkb::encode(&Geom::Point(1.0, 1.0)).unwrap());

    let cols: Vec<ArrayRef> = vec![
        Arc::new(amount_b.finish()),
        Arc::new(ts_b.finish()),
        Arc::new(geom_b.finish()),
    ];
    let batch = RecordBatch::try_new(schema.clone(), cols).unwrap();

    let driver = SqlServerDriver::new();
    let mut w = driver
        .open_bulk_write(
            &uri,
            schema.clone(),
            Some(Crs::from_epsg(4326)),
            &write_opts(),
        )
        .expect("open_bulk_write")
        .expect("driver supports bulk_write");
    let mut iter = std::iter::once(Ok(batch));
    BulkLoadWriter::bulk_write(w.as_mut(), &mut iter).expect("bulk_write");
    w.finish().expect("finish");

    let mut r = driver
        .open_read(&uri, &ReadOpts::default())
        .expect("open_read");
    let batches: Vec<_> = r.batches().collect::<Result<_, _>>().unwrap();
    let b = &batches[0];

    let amt_back = b.column(0).as_primitive::<Int64Type>();
    assert_eq!(amt_back.value(0), 1);
    assert_eq!(amt_back.value(1), i64::MAX);

    let ts_back = b.column(1).as_primitive::<TimestampMicrosecondType>();
    assert_eq!(ts_back.value(0), micros);
    assert!(ts_back.is_null(1));

    drop(r);
    cleanup(&url, &table);
}

#[test]
fn bulk_chunked_at_5_rows() {
    let Some(url) = mssql_url() else {
        eprintln!("SHPX_TEST_SQLSERVER_URL unset; skipping chunked bulk test");
        return;
    };
    // Note: SHPX_MSSQL_BULK_CHUNK は OnceLock 経由でキャッシュされるため、テスト全体で
    // 1 度だけ effective になる。ここでは未設定でも default 100,000 で動くことを
    // 確認しつつ、12 行が単一トランザクションで完走することを見る (chunk 内挙動の sanity)。
    let table = unique_table("shpx_bulk_chunk");
    let uri = uri_with_table(&url, &table);

    let schema = schema_with_geom(
        vec![Field::new("idx", DataType::Int64, false)],
        GeometryType::Point,
        Some(Crs::from_epsg(4326)),
    );

    let mut idx_b = Int64Builder::new();
    let mut geom_b = BinaryBuilder::new();
    for i in 0..12i32 {
        idx_b.append_value(i64::from(i));
        geom_b.append_value(wkb::encode(&Geom::Point(f64::from(i), 0.0)).unwrap());
    }
    let cols: Vec<ArrayRef> = vec![Arc::new(idx_b.finish()), Arc::new(geom_b.finish())];
    let batch = RecordBatch::try_new(schema.clone(), cols).unwrap();

    let driver = SqlServerDriver::new();
    let mut w = driver
        .open_bulk_write(
            &uri,
            schema.clone(),
            Some(Crs::from_epsg(4326)),
            &write_opts(),
        )
        .expect("open_bulk_write")
        .expect("driver supports bulk_write");
    let mut iter = std::iter::once(Ok(batch));
    BulkLoadWriter::bulk_write(w.as_mut(), &mut iter).expect("bulk_write");
    w.finish().expect("finish");

    let mut r = driver
        .open_read(&uri, &ReadOpts::default())
        .expect("open_read");
    let batches: Vec<_> = r.batches().collect::<Result<_, _>>().unwrap();
    let total: usize = batches.iter().map(arrow_array::RecordBatch::num_rows).sum();
    assert_eq!(total, 12);
    drop(r);
    cleanup(&url, &table);
}

#[test]
fn bulk_geography_roundtrip() {
    let Some(url) = mssql_url() else {
        eprintln!("SHPX_TEST_SQLSERVER_URL unset; skipping geography bulk test");
        return;
    };
    let table = unique_table("shpx_bulk_geog");
    let uri = make_uri_with_geom_type(&url, &table, "geography");

    let schema = schema_with_geom(
        vec![Field::new("name", DataType::Utf8, true)],
        GeometryType::Point,
        Some(Crs::from_epsg(4326)),
    );

    let mut name_b = StringBuilder::new();
    name_b.append_value("tokyo");
    let mut geom_b = BinaryBuilder::new();
    // geography は経緯度として lon/lat (WKB) で受けて lat/lon に変換される。
    // 東京駅近辺の (lon=139.767, lat=35.681)。
    geom_b.append_value(wkb::encode(&Geom::Point(139.767, 35.681)).unwrap());

    let cols: Vec<ArrayRef> = vec![Arc::new(name_b.finish()), Arc::new(geom_b.finish())];
    let batch = RecordBatch::try_new(schema.clone(), cols).unwrap();

    let driver = SqlServerDriver::new();
    let mut w = driver
        .open_bulk_write(
            &uri,
            schema.clone(),
            Some(Crs::from_epsg(4326)),
            &write_opts(),
        )
        .expect("open_bulk_write")
        .expect("driver supports bulk_write");
    let mut iter = std::iter::once(Ok(batch));
    BulkLoadWriter::bulk_write(w.as_mut(), &mut iter).expect("bulk_write");
    w.finish().expect("finish");

    let mut r = driver
        .open_read(&uri, &ReadOpts::default())
        .expect("open_read");
    let batches: Vec<_> = r.batches().collect::<Result<_, _>>().unwrap();
    let b = &batches[0];
    assert_eq!(b.num_rows(), 1);

    let geom_back = b.column(1).as_binary::<i32>();
    let decoded = wkb::decode(geom_back.value(0)).unwrap();
    // SQL Server geography は WKB で lon/lat 順を保つので decode 値が一致する想定。
    match decoded {
        Geom::Point(lon, lat) => {
            assert!((lon - 139.767).abs() < 1e-6);
            assert!((lat - 35.681).abs() < 1e-6);
        }
        other => panic!("expected Point, got {other:?}"),
    }

    drop(r);
    cleanup(&url, &table);
}
