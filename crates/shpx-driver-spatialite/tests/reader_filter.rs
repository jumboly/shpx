//! SpatiaLite reader の `--where` / `--select` / `--query` 統合テスト。
//!
//! `SHPX_TEST_SPATIALITE` 環境変数が設定されている場合のみ実行する (PostGIS の
//! `tests/reader_filter.rs` と同型の env-gate)。`query_with_semicolon_errors` のみ
//! DB を触らないため env なしで動く。

mod common;

use std::sync::Arc;

use arrow_array::builder::{BinaryBuilder, Int64Builder, StringBuilder};
use arrow_array::{cast::AsArray, types::Int64Type, ArrayRef, RecordBatch};
use arrow_schema::{DataType, Field};
use common::{default_write_opts, schema_with_geom, skip_if_not_enabled};
use shpx_core::{schema::GeometryType, Crs, Driver, ReadOpts, Uri};
use shpx_driver_spatialite::SpatialiteDriver;
use shpx_geom::wkb::{self, Geom};

/// 5 行 `(id, name, geom)` の fixture を作る。各テストで個別ファイルを切る。
fn seed_fixture(path: &std::path::Path) {
    let schema = schema_with_geom(
        vec![
            Field::new("id", DataType::Int64, false),
            Field::new("name", DataType::Utf8, true),
        ],
        GeometryType::Point,
        Some(Crs::from_epsg(4326)),
    );

    let mut id_b = Int64Builder::new();
    let mut name_b = StringBuilder::new();
    let mut geom_b = BinaryBuilder::new();
    for i in 1..=5_i64 {
        id_b.append_value(i);
        name_b.append_value(format!("row-{i}"));
        #[allow(clippy::cast_precision_loss)]
        geom_b.append_value(wkb::encode(&Geom::Point(i as f64, (i as f64) * 2.0)).unwrap());
    }
    let cols: Vec<ArrayRef> = vec![
        Arc::new(id_b.finish()),
        Arc::new(name_b.finish()),
        Arc::new(geom_b.finish()),
    ];
    let batch = RecordBatch::try_new(schema.clone(), cols).unwrap();

    let driver = SpatialiteDriver::new();
    let uri = Uri::from_path(path.to_string_lossy().to_string());
    let mut w = driver
        .open_write(
            &uri,
            schema,
            Some(Crs::from_epsg(4326)),
            &default_write_opts(),
        )
        .expect("open_write");
    w.write_batch(&batch).expect("write_batch");
    w.finish().expect("finish");
}

fn ids_of(batches: &[RecordBatch], col: usize) -> Vec<i64> {
    batches
        .iter()
        .flat_map(|b| {
            let arr = b.column(col).as_primitive::<Int64Type>();
            (0..b.num_rows()).map(|i| arr.value(i)).collect::<Vec<_>>()
        })
        .collect()
}

#[test]
fn where_clause_filters_rows() {
    if skip_if_not_enabled() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("where.sqlite");
    seed_fixture(&path);

    let driver = SpatialiteDriver::new();
    let uri = Uri::from_path(path.to_string_lossy().to_string());
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
}

#[test]
fn select_columns_projects_in_specified_order() {
    if skip_if_not_enabled() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("select.sqlite");
    seed_fixture(&path);

    let driver = SpatialiteDriver::new();
    let uri = Uri::from_path(path.to_string_lossy().to_string());
    let opts = ReadOpts {
        select: Some(vec!["id".into(), "geom".into()]),
        ..ReadOpts::default()
    };
    let mut r = driver.open_read(&uri, &opts).expect("open_read");
    let schema = r.schema();
    // geometry 列はスキーマの末尾に置かれる慣習 (table モードの read_attribute_schema 順序)。
    let names: Vec<&str> = schema.fields().iter().map(|f| f.name().as_str()).collect();
    assert_eq!(names, vec!["id", "geom"]);

    let batches: Vec<_> = r.batches().collect::<Result<_, _>>().expect("collect");
    let total: usize = batches.iter().map(RecordBatch::num_rows).sum();
    assert_eq!(total, 5);
}

#[test]
fn select_without_geometry_errors() {
    if skip_if_not_enabled() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("no_geom.sqlite");
    seed_fixture(&path);

    let driver = SpatialiteDriver::new();
    let uri = Uri::from_path(path.to_string_lossy().to_string());
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
}

#[test]
fn query_subquery_filters_with_user_sql() {
    if skip_if_not_enabled() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("query.sqlite");
    seed_fixture(&path);

    let driver = SpatialiteDriver::new();
    let uri = Uri::from_path(path.to_string_lossy().to_string());

    // SpatiaLite writer はテーブル名を URI 由来のファイル名から導出する。fixture の
    // ファイル名は `query` なので、デフォルトテーブル名も `query` (writer の sanitize 経由)。
    // 実テーブル名を確定させるため URI 経由で reader にも同じパスを渡し、 LIST_FEATURE_TABLES
    // から取り出した名前で SQL を組み立てる必要がある。ここでは安直に `?table=query` を
    // 使わず、URI から resolve される単一テーブルがあるという前提で `*` を query する。
    let user_sql = "SELECT id, geom FROM query WHERE id IN (1, 5)";
    let opts = ReadOpts {
        query: Some(user_sql.into()),
        ..ReadOpts::default()
    };
    let mut r = driver.open_read(&uri, &opts).expect("open_read");
    // SRID 4326 が geometry blob から拾えること。
    assert_eq!(r.crs().cloned(), Some(Crs::from_epsg(4326)));
    let schema = r.schema();
    let names: Vec<&str> = schema.fields().iter().map(|f| f.name().as_str()).collect();
    assert_eq!(names, vec!["id", "geom"]);

    let batches: Vec<_> = r.batches().collect::<Result<_, _>>().expect("collect");
    let total: usize = batches.iter().map(RecordBatch::num_rows).sum();
    assert_eq!(total, 2);
    let mut ids = ids_of(&batches, 0);
    ids.sort_unstable();
    assert_eq!(ids, vec![1, 5]);
}

#[test]
fn query_with_semicolon_errors() {
    // `;` 包含エラーは driver 側の早期バリデーションで弾かれるため DB 接続不要。
    let driver = SpatialiteDriver::new();
    let uri = Uri::from_path("/tmp/shpx_filter_dummy.sqlite".to_string());
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
