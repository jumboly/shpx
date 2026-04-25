//! v0.3 cycle 2: COPY BINARY (`BulkLoadWriter`) 経路の往復統合テスト。
//!
//! `SHPX_TEST_PG_URL` 環境変数が設定されている場合のみ実行する（未設定なら eprintln + return）。
//! cycle 1 の `roundtrip.rs` と同じパターンで env-gate しているため、PG が無いローカル環境でも
//! `cargo test` は緑のまま。

use std::collections::HashMap;
use std::sync::Arc;

use arrow_array::{
    builder::{
        BinaryBuilder, BooleanBuilder, Date32Builder, Decimal128Builder, Float64Builder,
        Int32Builder, Int64Builder, StringBuilder, TimestampMicrosecondBuilder,
    },
    cast::AsArray,
    types::{
        Date32Type, Decimal128Type, Float64Type, Int32Type, Int64Type, TimestampMicrosecondType,
    },
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

struct AllTypesRow {
    flag: Option<bool>,
    class: Option<i32>,
    score: Option<f64>,
    name: String,
    tag: String,
    amount: i128,
    created: i32,
    event_at: i64,
    payload: Vec<u8>,
    geom: Vec<u8>,
}

/// `benches/gen.rs` のベンチスキーマと 1:1 揃えた 10 列を同時に往復させ、列バッファの
/// 境界や null bitmap の越境を 1 ファイル内で再現させるための型網羅テスト。10 列を
/// 個別ヘルパに分割すると意味が薄れるため 1 関数に束ねる。
#[allow(clippy::too_many_lines)]
#[test]
fn bulk_all_types_together() {
    let Some(url) = pg_url() else {
        eprintln!("SHPX_TEST_PG_URL unset; skipping bulk integration test");
        return;
    };
    let table = unique_table("shpx_bulk_all");
    let uri = uri_with_table(&url, &table);
    let n: i64 = 1000;
    // 2026-04-25T00:00:00Z UTC の microseconds since epoch。bit-identical 検証用に固定。
    let base_micros: i64 = 1_777_680_000_000_000;

    let schema = schema_with_geom(
        vec![
            Field::new("id", DataType::Int64, false),
            Field::new("flag", DataType::Boolean, true),
            Field::new("class", DataType::Int32, true),
            Field::new("score", DataType::Float64, true),
            Field::new("name", DataType::Utf8, true),
            Field::new("tag", DataType::Utf8, true),
            Field::new("amount", DataType::Decimal128(38, 10), true),
            Field::new("created", DataType::Date32, true),
            Field::new(
                "event_at",
                DataType::Timestamp(TimeUnit::Microsecond, Some("UTC".into())),
                true,
            ),
            Field::new("payload", DataType::Binary, true),
        ],
        GeometryType::Point,
        Some(Crs::from_epsg(4326)),
    );

    let mut id_b = Int64Builder::new();
    let mut flag_b = BooleanBuilder::new();
    let mut class_b = Int32Builder::new();
    let mut score_b = Float64Builder::new();
    let mut name_b = StringBuilder::new();
    let mut tag_b = StringBuilder::new();
    let mut amount_b = Decimal128Builder::new()
        .with_precision_and_scale(38, 10)
        .unwrap();
    let mut created_b = Date32Builder::new();
    let mut event_b = TimestampMicrosecondBuilder::new().with_timezone("UTC");
    let mut payload_b = BinaryBuilder::new();
    let mut geom_b = BinaryBuilder::new();

    for i in 0..n {
        let row = expected_row(i, base_micros);
        id_b.append_value(i);
        match row.flag {
            Some(v) => flag_b.append_value(v),
            None => flag_b.append_null(),
        }
        match row.class {
            Some(v) => class_b.append_value(v),
            None => class_b.append_null(),
        }
        match row.score {
            Some(v) => score_b.append_value(v),
            None => score_b.append_null(),
        }
        name_b.append_value(&row.name);
        tag_b.append_value(&row.tag);
        amount_b.append_value(row.amount);
        created_b.append_value(row.created);
        event_b.append_value(row.event_at);
        payload_b.append_value(&row.payload);
        geom_b.append_value(&row.geom);
    }

    let cols: Vec<ArrayRef> = vec![
        Arc::new(id_b.finish()),
        Arc::new(flag_b.finish()),
        Arc::new(class_b.finish()),
        Arc::new(score_b.finish()),
        Arc::new(name_b.finish()),
        Arc::new(tag_b.finish()),
        Arc::new(amount_b.finish()),
        Arc::new(created_b.finish()),
        Arc::new(event_b.finish()),
        Arc::new(payload_b.finish()),
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

    let total: usize = batches.iter().map(RecordBatch::num_rows).sum();
    assert_eq!(total, usize::try_from(n).unwrap());

    // SELECT は順序保証が無いため、id をキーに行を回収して expected と突き合わせる。
    let mut got: HashMap<i64, AllTypesRow> = HashMap::with_capacity(usize::try_from(n).unwrap());
    for b in &batches {
        let id_a = b.column(0).as_primitive::<Int64Type>();
        let flag_a = b.column(1).as_boolean();
        let class_a = b.column(2).as_primitive::<Int32Type>();
        let score_a = b.column(3).as_primitive::<Float64Type>();
        let name_a = b.column(4).as_string::<i32>();
        let tag_a = b.column(5).as_string::<i32>();
        let amount_a = b.column(6).as_primitive::<Decimal128Type>();
        let created_a = b.column(7).as_primitive::<Date32Type>();
        let event_a = b.column(8).as_primitive::<TimestampMicrosecondType>();
        let payload_a = b.column(9).as_binary::<i32>();
        let geom_a = b.column(10).as_binary::<i32>();
        for i in 0..b.num_rows() {
            let id = id_a.value(i);
            got.insert(
                id,
                AllTypesRow {
                    flag: if flag_a.is_null(i) {
                        None
                    } else {
                        Some(flag_a.value(i))
                    },
                    class: if class_a.is_null(i) {
                        None
                    } else {
                        Some(class_a.value(i))
                    },
                    score: if score_a.is_null(i) {
                        None
                    } else {
                        Some(score_a.value(i))
                    },
                    name: name_a.value(i).to_owned(),
                    tag: tag_a.value(i).to_owned(),
                    amount: amount_a.value(i),
                    created: created_a.value(i),
                    event_at: event_a.value(i),
                    payload: payload_a.value(i).to_owned(),
                    geom: geom_a.value(i).to_owned(),
                },
            );
        }
    }

    assert_eq!(got.len(), usize::try_from(n).unwrap());
    for i in 0..n {
        let row = got.get(&i).unwrap_or_else(|| panic!("row id={i} missing"));
        let expected = expected_row(i, base_micros);
        assert_eq!(row.flag, expected.flag, "row {i} flag");
        assert_eq!(row.class, expected.class, "row {i} class");
        // f64 は bit-identical を to_bits 比較で確認（COPY BINARY は IEEE 754 BE 直書きのため）。
        match (row.score, expected.score) {
            (Some(a), Some(b)) => {
                assert_eq!(a.to_bits(), b.to_bits(), "row {i} score bits");
            }
            (None, None) => {}
            _ => panic!("row {i} score null mismatch"),
        }
        assert_eq!(row.name, expected.name, "row {i} name");
        assert_eq!(row.tag, expected.tag, "row {i} tag");
        assert_eq!(row.amount, expected.amount, "row {i} amount");
        assert_eq!(row.created, expected.created, "row {i} created");
        assert_eq!(row.event_at, expected.event_at, "row {i} event_at");
        assert_eq!(row.payload, expected.payload, "row {i} payload");
        assert_eq!(row.geom, expected.geom, "row {i} geom");
    }

    cleanup(&url, &table);
}

/// 行 i の期待値。null pattern (素数 11/13/17) と各定数係数を `benches/gen.rs::build_batch`
/// と完全に揃えること。値がずれると bit-identical テストが ベンチデータを通らなくなる。
fn expected_row(i: i64, base_micros: i64) -> AllTypesRow {
    let flag = if i % 11 == 0 { None } else { Some(i % 2 == 0) };
    let class = if i % 13 == 0 {
        None
    } else {
        Some(i32::try_from(i % 7).expect("i%7 fits i32"))
    };
    let score = if i % 17 == 0 {
        None
    } else {
        // 1/8 刻みは f64 完全表現可能。Parquet→PostgreSQL 経由でも bit-identical。
        Some(f64::from(i32::try_from(i).expect("n<=1000 fits i32")) * 0.125)
    };
    let name = format!("name_{i:010}");
    let tag_len = 4 + usize::try_from(i.rem_euclid(29)).expect("rem fits usize");
    let tag: String = (0..tag_len)
        .map(|j| {
            let j_i64 = i64::try_from(j).expect("tag_len<=32 fits i64");
            let off = u8::try_from((i + j_i64).rem_euclid(26)).expect("rem fits u8");
            char::from(b'a' + off)
        })
        .collect();
    // i=999 で約 1.23×10^21、Decimal128(38,10) の値域に収まる係数。
    let amount: i128 = i128::from(i) * 1_234_567_890_123_456_789i128;
    let created = 20100 + i32::try_from(i % 365).expect("i%365 fits i32");
    let event_at = base_micros + i;
    let payload: Vec<u8> = (0..16u8)
        .map(|j| (u8::try_from(i & 0xff).expect("masked fits u8")).wrapping_add(j))
        .collect();
    let lon = f64::from(i32::try_from(i % 360).expect("i%360 fits i32")) - 180.0;
    let lat = f64::from(i32::try_from(i % 180).expect("i%180 fits i32")) - 90.0;
    let geom = wkb::encode(&Geom::Point(lon, lat)).unwrap();
    AllTypesRow {
        flag,
        class,
        score,
        name,
        tag,
        amount,
        created,
        event_at,
        payload,
        geom,
    }
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
