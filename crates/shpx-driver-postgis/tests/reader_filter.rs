//! v0.3 cycle 3a: PostGIS reader の `--where` / `--select` / `--query` 統合テスト。
//!
//! `SHPX_TEST_PG_URL` 環境変数が設定されている場合のみ実行する（未設定なら eprintln + return）。
//! cycle 1/2 の `roundtrip.rs` / `bulk_roundtrip.rs` と同じ env-gate パターン。

use std::sync::Arc;

use arrow_array::{
    builder::{BinaryBuilder, Int32Builder, StringBuilder},
    cast::AsArray,
    types::Int32Type,
    ArrayRef, RecordBatch,
};
use arrow_schema::{DataType, Field};
use shpx_core::{schema::GeometryType, Crs, Driver, ReadOpts, Uri};
use shpx_driver_postgis::PostgisDriver;
use shpx_geom::wkb::{self, Geom};

mod common;
use common::{cleanup, pg_url, schema_with_geom, unique_table, uri_with_table, write_opts};

/// 5 行の (id, name, geom) を bulk 経路で投入し、共有 fixture とする。
fn seed_fixture(driver: PostgisDriver, uri: &Uri) {
    let schema = schema_with_geom(
        vec![
            Field::new("id", DataType::Int32, false),
            Field::new("name", DataType::Utf8, true),
        ],
        GeometryType::Point,
        Some(Crs::from_epsg(4326)),
    );

    let mut id_b = Int32Builder::new();
    let mut name_b = StringBuilder::new();
    let mut geom_b = BinaryBuilder::new();
    for i in 1..=5 {
        id_b.append_value(i);
        name_b.append_value(format!("row-{i}"));
        geom_b.append_value(wkb::encode(&Geom::Point(f64::from(i), f64::from(i) * 2.0)).unwrap());
    }
    let cols: Vec<ArrayRef> = vec![
        Arc::new(id_b.finish()),
        Arc::new(name_b.finish()),
        Arc::new(geom_b.finish()),
    ];
    let batch = RecordBatch::try_new(schema.clone(), cols).unwrap();

    let mut bulk = driver
        .open_bulk_write(uri, schema, Some(Crs::from_epsg(4326)), &write_opts())
        .expect("open_bulk_write")
        .expect("driver returns Some bulk writer");
    let mut iter = std::iter::once(Ok(batch));
    bulk.bulk_write(&mut iter).expect("bulk_write");
    bulk.finish().expect("finish");
}

/// `id` 列を取り出して Vec<i32> に正規化する。null は無い前提。
fn ids_of(batches: &[RecordBatch], col: usize) -> Vec<i32> {
    batches
        .iter()
        .flat_map(|b| {
            let arr = b.column(col).as_primitive::<Int32Type>();
            (0..b.num_rows()).map(|i| arr.value(i)).collect::<Vec<_>>()
        })
        .collect()
}

#[test]
fn where_clause_filters_rows() {
    let Some(url) = pg_url() else {
        eprintln!("SHPX_TEST_PG_URL unset; skipping reader_filter test");
        return;
    };
    let table = unique_table("shpx_filter_where");
    let uri = uri_with_table(&url, &table);
    let driver = PostgisDriver::new();
    seed_fixture(driver, &uri);

    let opts = ReadOpts {
        where_clause: Some("id < 3".into()),
        ..ReadOpts::default()
    };
    let mut r = driver.open_read(&uri, &opts).expect("open_read");
    let batches: Vec<_> = r.batches().collect::<Result<_, _>>().expect("collect");
    let total: usize = batches.iter().map(RecordBatch::num_rows).sum();
    assert_eq!(total, 2, "WHERE id < 3 should match 2 rows");
    let mut ids = ids_of(&batches, 0);
    ids.sort_unstable();
    assert_eq!(ids, vec![1, 2]);

    drop(r);
    cleanup(&url, &table);
}

#[test]
fn select_columns_projects_in_specified_order() {
    let Some(url) = pg_url() else {
        eprintln!("SHPX_TEST_PG_URL unset; skipping reader_filter test");
        return;
    };
    let table = unique_table("shpx_filter_select");
    let uri = uri_with_table(&url, &table);
    let driver = PostgisDriver::new();
    seed_fixture(driver, &uri);

    // 順序を `id, geom` に絞る（name は省く）。
    let opts = ReadOpts {
        select: Some(vec!["id".into(), "geom".into()]),
        ..ReadOpts::default()
    };
    let mut r = driver.open_read(&uri, &opts).expect("open_read");
    let schema = r.schema();
    assert_eq!(schema.fields().len(), 2);
    assert_eq!(schema.field(0).name(), "id");
    assert_eq!(schema.field(1).name(), "geom");

    let batches: Vec<_> = r.batches().collect::<Result<_, _>>().expect("collect");
    let total: usize = batches.iter().map(RecordBatch::num_rows).sum();
    assert_eq!(total, 5);

    drop(r);
    cleanup(&url, &table);
}

#[test]
fn select_without_geometry_errors() {
    let Some(url) = pg_url() else {
        eprintln!("SHPX_TEST_PG_URL unset; skipping reader_filter test");
        return;
    };
    let table = unique_table("shpx_filter_no_geom");
    let uri = uri_with_table(&url, &table);
    let driver = PostgisDriver::new();
    seed_fixture(driver, &uri);

    let opts = ReadOpts {
        select: Some(vec!["id".into(), "name".into()]),
        ..ReadOpts::default()
    };
    let err = driver
        .open_read(&uri, &opts)
        .err()
        .expect("--select without geometry must error");
    let msg = err.to_string();
    assert!(
        msg.contains("geometry"),
        "error message should mention geometry: {msg}"
    );

    cleanup(&url, &table);
}

#[test]
fn query_subquery_filters_with_user_sql() {
    let Some(url) = pg_url() else {
        eprintln!("SHPX_TEST_PG_URL unset; skipping reader_filter test");
        return;
    };
    let table = unique_table("shpx_filter_query");
    let uri = uri_with_table(&url, &table);
    let driver = PostgisDriver::new();
    seed_fixture(driver, &uri);

    let user_sql = format!("SELECT id, geom FROM \"public\".\"{table}\" WHERE id IN (1, 5)");
    let opts = ReadOpts {
        query: Some(user_sql),
        ..ReadOpts::default()
    };
    let mut r = driver.open_read(&uri, &opts).expect("open_read");
    // SRID は probe で 4326 と判明する。
    assert_eq!(r.crs().cloned(), Some(Crs::from_epsg(4326)));
    let schema = r.schema();
    assert_eq!(schema.fields().len(), 2);
    assert_eq!(schema.field(0).name(), "id");
    assert_eq!(schema.field(1).name(), "geom");

    let batches: Vec<_> = r.batches().collect::<Result<_, _>>().expect("collect");
    let total: usize = batches.iter().map(RecordBatch::num_rows).sum();
    assert_eq!(total, 2);
    let mut ids = ids_of(&batches, 0);
    ids.sort_unstable();
    assert_eq!(ids, vec![1, 5]);

    drop(r);
    cleanup(&url, &table);
}

#[test]
fn query_with_semicolon_errors() {
    // 接続も DB も触れないので env チェックは不要だが、URI 整合性のためダミーを使う。
    let driver = PostgisDriver::new();
    let uri = Uri::from_path("pg://localhost/db".to_string());
    let opts = ReadOpts {
        query: Some("SELECT id, geom FROM t;".into()),
        ..ReadOpts::default()
    };
    let err = driver
        .open_read(&uri, &opts)
        .err()
        .expect("--query with `;` must error");
    let msg = err.to_string();
    assert!(
        msg.contains("must not contain `;`"),
        "error must mention semicolon: {msg}"
    );
}
