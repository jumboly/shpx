//! v0.3 cycle 2: COPY BINARY (`BulkLoadWriter`) 経路の往復統合テスト。
//!
//! `SHPX_TEST_PG_URL` 環境変数が設定されている場合のみ実行する（未設定なら eprintln + return）。
//! cycle 1 の `roundtrip.rs` と同じパターンで env-gate しているため、PG が無いローカル環境でも
//! `cargo test` は緑のまま。

use std::sync::Arc;

use arrow_array::{
    builder::{
        BinaryBuilder, Decimal128Builder, Int32Builder, StringBuilder, TimestampMicrosecondBuilder,
    },
    cast::AsArray,
    types::{Decimal128Type, Int32Type, TimestampMicrosecondType},
    Array, ArrayRef, RecordBatch,
};
use arrow_schema::{DataType, Field, SchemaRef, TimeUnit};
use shpx_core::{schema::GeometryType, Crs, Driver, ReadOpts, Result, Uri};
use shpx_driver_postgis::PostgisDriver;
use shpx_geom::wkb::{self, Geom};

mod common;
use common::{cleanup, pg_url, schema_with_geom, unique_table, uri_with_table, write_opts};

/// バッチを bulk 経路で書き、改めて reader で読み出して RecordBatch 列を返す。
fn write_bulk_then_read(
    driver: PostgisDriver,
    uri: &Uri,
    schema: SchemaRef,
    crs: Option<Crs>,
    batches: Vec<RecordBatch>,
) -> Result<Vec<RecordBatch>> {
    let mut bulk = driver
        .open_bulk_write(uri, schema, crs, &write_opts())?
        .expect("PostgisDriver::open_bulk_write must return Some when bulk_load=true");
    let mut iter = batches.into_iter().map(Ok);
    bulk.bulk_write(&mut iter)?;
    bulk.finish()?;

    let mut r = driver.open_read(uri, &ReadOpts::default())?;
    r.batches().collect()
}

#[test]
fn bulk_point_with_attributes_roundtrip() {
    let Some(url) = pg_url() else {
        eprintln!("SHPX_TEST_PG_URL unset; skipping bulk integration test");
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

    let driver = PostgisDriver::new();
    let batches = write_bulk_then_read(
        driver,
        &uri,
        schema.clone(),
        Some(Crs::from_epsg(4326)),
        vec![batch],
    )
    .expect("bulk roundtrip");

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

    cleanup(&url, &table);
}

#[test]
fn bulk_decimal128_38_10_bit_identical() {
    let Some(url) = pg_url() else {
        eprintln!("SHPX_TEST_PG_URL unset; skipping bulk integration test");
        return;
    };
    let table = unique_table("shpx_bulk_dec");
    let uri = uri_with_table(&url, &table);

    let schema = schema_with_geom(
        vec![Field::new("amount", DataType::Decimal128(38, 10), true)],
        GeometryType::Point,
        Some(Crs::from_epsg(4326)),
    );

    // 境界値・通常値・負値・0 を含む。
    // max_38 = 10^38 - 1。Decimal128(38, 10) として有効な i128 値域の上限。
    let max_38: i128 = 10i128.pow(38) - 1;
    let values: Vec<Option<i128>> = vec![
        Some(0),
        Some(1),
        Some(-1),
        Some(15_000),      // scale=10 のため 0.0000015000
        Some(123_456_789), // 0.0123456789
        Some(max_38),
        Some(-max_38),
        None,
    ];

    let mut dec_b = Decimal128Builder::new()
        .with_precision_and_scale(38, 10)
        .unwrap();
    for v in &values {
        match v {
            Some(x) => dec_b.append_value(*x),
            None => dec_b.append_null(),
        }
    }
    let mut geom_b = BinaryBuilder::new();
    for _ in 0..values.len() {
        geom_b.append_value(wkb::encode(&Geom::Point(0.0, 0.0)).unwrap());
    }
    let cols: Vec<ArrayRef> = vec![Arc::new(dec_b.finish()), Arc::new(geom_b.finish())];
    let batch = RecordBatch::try_new(schema.clone(), cols).unwrap();

    let driver = PostgisDriver::new();
    let batches = write_bulk_then_read(
        driver,
        &uri,
        schema.clone(),
        Some(Crs::from_epsg(4326)),
        vec![batch],
    )
    .expect("bulk roundtrip");

    assert_eq!(batches.len(), 1);
    let b = &batches[0];
    assert_eq!(b.num_rows(), values.len());
    let dec_back = b.column(0).as_primitive::<Decimal128Type>();
    for (i, expected) in values.iter().enumerate() {
        match expected {
            Some(x) => assert_eq!(
                dec_back.value(i),
                *x,
                "row {i}: expected {x}, got {}",
                dec_back.value(i)
            ),
            None => assert!(dec_back.is_null(i), "row {i} should be null"),
        }
    }

    cleanup(&url, &table);
}

#[test]
fn bulk_bytea_and_timestamptz_bit_identical() {
    let Some(url) = pg_url() else {
        eprintln!("SHPX_TEST_PG_URL unset; skipping bulk integration test");
        return;
    };
    let table = unique_table("shpx_bulk_bin_ts");
    let uri = uri_with_table(&url, &table);

    let schema = schema_with_geom(
        vec![
            Field::new("blob", DataType::Binary, true),
            Field::new(
                "ts",
                DataType::Timestamp(TimeUnit::Microsecond, Some("UTC".into())),
                true,
            ),
        ],
        GeometryType::Point,
        Some(Crs::from_epsg(4326)),
    );

    // bytea: 0x00..0xff の全 256 値 + ランダム長の 100 行
    let mut blob_b = BinaryBuilder::new();
    let mut ts_b = TimestampMicrosecondBuilder::new().with_timezone("UTC");
    let mut geom_b = BinaryBuilder::new();
    let base_micros: i64 = 1_777_680_000_000_000; // 2026-04-25T00:00:00Z

    let n: usize = 100;
    let mut expected_blobs: Vec<Vec<u8>> = Vec::with_capacity(n);
    let mut expected_ts: Vec<i64> = Vec::with_capacity(n);
    for i in 0..n {
        let len = u8::try_from(i % 17 + 1).expect("len fits u8");
        let blob: Vec<u8> = (0..len).collect();
        blob_b.append_value(&blob);
        expected_blobs.push(blob);

        // 1 マイクロ秒ずつずらして bit-identical を確認。
        let micros = base_micros + i64::try_from(i).expect("100 fits i64");
        ts_b.append_value(micros);
        expected_ts.push(micros);

        geom_b.append_value(wkb::encode(&Geom::Point(0.0, 0.0)).unwrap());
    }

    let cols: Vec<ArrayRef> = vec![
        Arc::new(blob_b.finish()),
        Arc::new(ts_b.finish()),
        Arc::new(geom_b.finish()),
    ];
    let batch = RecordBatch::try_new(schema.clone(), cols).unwrap();

    let driver = PostgisDriver::new();
    let batches = write_bulk_then_read(
        driver,
        &uri,
        schema.clone(),
        Some(Crs::from_epsg(4326)),
        vec![batch],
    )
    .expect("bulk roundtrip");

    assert_eq!(batches.len(), 1);
    let b = &batches[0];
    assert_eq!(b.num_rows(), n);

    let blob_back = b.column(0).as_binary::<i32>();
    for (i, expected) in expected_blobs.iter().enumerate() {
        assert_eq!(blob_back.value(i), expected.as_slice(), "row {i}");
    }
    let ts_back = b.column(1).as_primitive::<TimestampMicrosecondType>();
    for (i, &expected) in expected_ts.iter().enumerate() {
        assert_eq!(ts_back.value(i), expected, "row {i}");
    }

    cleanup(&url, &table);
}

#[test]
fn bulk_thousand_rows_with_nulls_roundtrip() {
    let Some(url) = pg_url() else {
        eprintln!("SHPX_TEST_PG_URL unset; skipping bulk integration test");
        return;
    };
    let table = unique_table("shpx_bulk_1k");
    let uri = uri_with_table(&url, &table);

    let schema = schema_with_geom(
        vec![Field::new("n", DataType::Int32, true)],
        GeometryType::Point,
        Some(Crs::from_epsg(4326)),
    );

    // 1000 行、3 行に 1 行 NULL、3 行に 1 行 geom NULL。
    let n: i32 = 1000;
    let mut nb = Int32Builder::new();
    let mut gb = BinaryBuilder::new();
    for i in 0..n {
        if i % 3 == 0 {
            nb.append_null();
        } else {
            nb.append_value(i);
        }
        if i % 7 == 0 {
            gb.append_null();
        } else {
            let f = f64::from(i);
            gb.append_value(wkb::encode(&Geom::Point(f, -f)).unwrap());
        }
    }
    let cols: Vec<ArrayRef> = vec![Arc::new(nb.finish()), Arc::new(gb.finish())];
    let batch = RecordBatch::try_new(schema.clone(), cols).unwrap();

    let driver = PostgisDriver::new();
    let batches = write_bulk_then_read(
        driver,
        &uri,
        schema.clone(),
        Some(Crs::from_epsg(4326)),
        vec![batch],
    )
    .expect("bulk roundtrip");

    let total: usize = batches.iter().map(RecordBatch::num_rows).sum();
    assert_eq!(total, usize::try_from(n).unwrap());

    // null pattern を集計（順序は SELECT で保証されないので合計で見る）。
    let mut int_nulls = 0;
    let mut geom_nulls = 0;
    for b in &batches {
        let int_arr = b.column(0).as_primitive::<Int32Type>();
        let geom_arr = b.column(1).as_binary::<i32>();
        for i in 0..b.num_rows() {
            if int_arr.is_null(i) {
                int_nulls += 1;
            }
            if geom_arr.is_null(i) {
                geom_nulls += 1;
            }
        }
    }
    let expected_int_nulls = (0..n).filter(|i| i % 3 == 0).count();
    let expected_geom_nulls = (0..n).filter(|i| i % 7 == 0).count();
    assert_eq!(int_nulls, expected_int_nulls);
    assert_eq!(geom_nulls, expected_geom_nulls);

    cleanup(&url, &table);
}

#[test]
fn bulk_empty_iterator_writes_only_header_and_trailer() {
    let Some(url) = pg_url() else {
        eprintln!("SHPX_TEST_PG_URL unset; skipping bulk integration test");
        return;
    };
    let table = unique_table("shpx_bulk_empty");
    let uri = uri_with_table(&url, &table);

    let schema = schema_with_geom(
        vec![Field::new("v", DataType::Int32, true)],
        GeometryType::Point,
        Some(Crs::from_epsg(4326)),
    );

    let driver = PostgisDriver::new();
    let batches = write_bulk_then_read(
        driver,
        &uri,
        schema,
        Some(Crs::from_epsg(4326)),
        vec![], // 0 batch
    )
    .expect("bulk roundtrip empty");

    let total: usize = batches.iter().map(RecordBatch::num_rows).sum();
    assert_eq!(total, 0);

    cleanup(&url, &table);
}
