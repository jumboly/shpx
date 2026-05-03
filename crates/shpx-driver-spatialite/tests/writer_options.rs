//! `--create-table` / `--create-index` / `--overwrite` の組み合わせ検証。
//!
//! mod_spatialite が必要なため env-gated。
//! `SHPX_TEST_SPATIALITE=1` (+ 必要なら `SHPX_SPATIALITE_PATH` で .so/.dylib のパス) を
//! セットして実行する。env 未設定時は skip。

use std::collections::HashMap;
use std::sync::Arc;

use arrow_array::builder::BinaryBuilder;
use arrow_array::{ArrayRef, Int64Array, RecordBatch, StringArray};
use arrow_schema::{DataType, Field, Schema};
use rusqlite::Connection;
use shpx_core::{
    schema::{GeometryMeta, GeometryType, GEOMETRY_META_KEY},
    CreateIndex, CreateTable, Crs, Driver, Uri, WriteOpts,
};
use shpx_driver_spatialite::SpatialiteDriver;
use shpx_geom::wkb::{self, Geom};

fn skip_if_not_enabled() -> bool {
    match std::env::var("SHPX_TEST_SPATIALITE") {
        Ok(v) if !v.is_empty() && v != "0" => false,
        _ => {
            eprintln!(
                "SHPX_TEST_SPATIALITE not set; skipping (install mod_spatialite + set the env to run)"
            );
            true
        }
    }
}

fn schema_with_geom(extra: Vec<Field>, gt: GeometryType, crs: Option<Crs>) -> Arc<Schema> {
    let mut fields = extra;
    let meta = GeometryMeta::wkb(gt, crs);
    let mut field = Field::new("geom", DataType::Binary, true);
    let mut m = HashMap::new();
    m.insert(GEOMETRY_META_KEY.to_string(), meta.to_json().unwrap());
    field.set_metadata(m);
    fields.push(field);
    Arc::new(Schema::new(fields))
}

fn write_simple(
    path: &std::path::Path,
    rows: &[(&str, i64, Geom)],
    opts: &WriteOpts,
    extra_field: Option<&str>,
) {
    let mut fields = vec![
        Field::new("name", DataType::Utf8, true),
        Field::new("count", DataType::Int64, true),
    ];
    if let Some(name) = extra_field {
        fields.push(Field::new(name, DataType::Utf8, true));
    }
    let schema = schema_with_geom(fields, GeometryType::Point, Some(Crs::from_epsg(4326)));

    let names: Vec<Option<&str>> = rows.iter().map(|r| Some(r.0)).collect();
    let counts: Vec<Option<i64>> = rows.iter().map(|r| Some(r.1)).collect();
    let mut bb = BinaryBuilder::new();
    for r in rows {
        bb.append_value(wkb::encode(&r.2).unwrap());
    }
    let mut cols: Vec<ArrayRef> = vec![
        Arc::new(StringArray::from(names)) as _,
        Arc::new(Int64Array::from(counts)) as _,
    ];
    if extra_field.is_some() {
        let extras: Vec<Option<&str>> = (0..rows.len()).map(|_| Some("x")).collect();
        cols.push(Arc::new(StringArray::from(extras)) as _);
    }
    cols.push(Arc::new(bb.finish()) as ArrayRef);
    let batch = RecordBatch::try_new(schema.clone(), cols).unwrap();

    let driver = SpatialiteDriver::new();
    let uri = Uri::from_path(path.to_string_lossy().to_string());
    let mut w = driver
        .open_write(&uri, schema, Some(Crs::from_epsg(4326)), opts)
        .unwrap();
    w.write_batch(&batch).unwrap();
    w.finish().unwrap();
}

fn count_rows(path: &std::path::Path, table: &str) -> i64 {
    let conn = Connection::open(path).unwrap();
    conn.query_row(
        &format!("SELECT COUNT(*) FROM \"{table}\""),
        [],
        |row| row.get::<_, i64>(0),
    )
    .unwrap()
}

fn table_exists(path: &std::path::Path, table: &str) -> bool {
    let conn = Connection::open(path).unwrap();
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND lower(name) = lower(?1)",
            rusqlite::params![table],
            |row| row.get(0),
        )
        .unwrap();
    n > 0
}

fn rtree_shadow_exists(path: &std::path::Path, idx_name: &str) -> bool {
    let conn = Connection::open(path).unwrap();
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name = ?1",
            rusqlite::params![idx_name],
            |row| row.get(0),
        )
        .unwrap();
    n > 0
}

#[test]
fn create_table_if_not_exists_creates_when_missing() {
    if skip_if_not_enabled() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.sqlite");
    let opts = WriteOpts {
        create_table: CreateTable::IfNotExists,
        ..Default::default()
    };
    write_simple(&path, &[("k", 1, Geom::Point(1.0, 2.0))], &opts, None);
    assert!(table_exists(&path, "a"));
    assert_eq!(count_rows(&path, "a"), 1);
}

#[test]
fn create_table_if_not_exists_appends_to_existing() {
    if skip_if_not_enabled() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.sqlite");
    // 1 回目で作成。
    write_simple(
        &path,
        &[("k", 1, Geom::Point(1.0, 2.0))],
        &WriteOpts::default(),
        None,
    );
    // 2 回目で append（同一 schema）。`overwrite=false` で同一ファイルへ追記する。
    write_simple(
        &path,
        &[("k2", 2, Geom::Point(3.0, 4.0))],
        &WriteOpts::default(),
        None,
    );
    assert_eq!(count_rows(&path, "a"), 2);
}

#[test]
fn create_table_always_drops_and_recreates() {
    if skip_if_not_enabled() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.sqlite");
    // 1 回目: 2 列の schema で作成。
    write_simple(
        &path,
        &[("k", 1, Geom::Point(1.0, 2.0))],
        &WriteOpts::default(),
        None,
    );
    assert_eq!(count_rows(&path, "a"), 1);
    // 2 回目: `Always` で別 schema (extra 列付き) に再作成。
    let opts = WriteOpts {
        create_table: CreateTable::Always,
        ..Default::default()
    };
    write_simple(
        &path,
        &[("k", 1, Geom::Point(1.0, 2.0))],
        &opts,
        Some("extra"),
    );
    // 元の 1 行は drop されているので、この時点で 1 行のみ。
    assert_eq!(count_rows(&path, "a"), 1);
    // 新 schema に extra 列が存在する。
    let conn = Connection::open(&path).unwrap();
    let mut stmt = conn.prepare("PRAGMA table_info(\"a\")").unwrap();
    let cols: Vec<String> = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    assert!(cols.iter().any(|c| c == "extra"), "cols = {cols:?}");
}

#[test]
fn create_table_never_errors_when_missing() {
    if skip_if_not_enabled() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.sqlite");
    let opts = WriteOpts {
        create_table: CreateTable::Never,
        ..Default::default()
    };
    // open_write 自体がエラーになることを assert したいので、driver を直接叩く。
    let schema = schema_with_geom(
        vec![Field::new("name", DataType::Utf8, true)],
        GeometryType::Point,
        Some(Crs::from_epsg(4326)),
    );
    let driver = SpatialiteDriver::new();
    let uri = Uri::from_path(path.to_string_lossy().to_string());
    let Err(err) = driver.open_write(&uri, schema, Some(Crs::from_epsg(4326)), &opts) else {
        panic!("expected error from create_table=never on missing file");
    };
    let msg = format!("{err}");
    assert!(msg.contains("create-table=never"), "msg was: {msg}");
}

#[test]
fn create_table_never_appends_to_existing() {
    if skip_if_not_enabled() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.sqlite");
    // 1 回目: IfNotExists で作成。
    write_simple(
        &path,
        &[("k", 1, Geom::Point(1.0, 2.0))],
        &WriteOpts::default(),
        None,
    );
    let table_count_before: i64 = {
        let conn = Connection::open(&path).unwrap();
        conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table'",
            [],
            |r| r.get(0),
        )
        .unwrap()
    };
    // 2 回目: Never で同一テーブルへ append、テーブル数は変化しない。
    let opts = WriteOpts {
        create_table: CreateTable::Never,
        ..Default::default()
    };
    write_simple(
        &path,
        &[("k2", 2, Geom::Point(3.0, 4.0))],
        &opts,
        None,
    );
    assert_eq!(count_rows(&path, "a"), 2);
    let table_count_after: i64 = {
        let conn = Connection::open(&path).unwrap();
        conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table'",
            [],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_eq!(table_count_before, table_count_after);
}

#[test]
fn overwrite_with_create_table_never_is_rejected() {
    if skip_if_not_enabled() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.sqlite");
    let schema = schema_with_geom(
        vec![Field::new("name", DataType::Utf8, true)],
        GeometryType::Point,
        Some(Crs::from_epsg(4326)),
    );
    let opts = WriteOpts {
        overwrite: true,
        create_table: CreateTable::Never,
        ..Default::default()
    };
    let driver = SpatialiteDriver::new();
    let uri = Uri::from_path(path.to_string_lossy().to_string());
    let Err(err) = driver.open_write(&uri, schema, Some(Crs::from_epsg(4326)), &opts) else {
        panic!("expected error from --overwrite + create_table=never combination");
    };
    let msg = format!("{err}");
    assert!(msg.contains("--overwrite"), "msg was: {msg}");
    assert!(msg.contains("--create-table=never"), "msg was: {msg}");
}

#[test]
fn create_index_always_creates_rtree() {
    if skip_if_not_enabled() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.sqlite");
    let opts = WriteOpts {
        create_index: CreateIndex::Always,
        ..Default::default()
    };
    write_simple(&path, &[("k", 1, Geom::Point(1.0, 2.0))], &opts, None);
    // SpatiaLite の R*Tree shadow virtual table は `idx_<table>_<geom>` という名前。
    assert!(rtree_shadow_exists(&path, "idx_a_geom"));
}

#[test]
fn create_index_never_skips_rtree() {
    if skip_if_not_enabled() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.sqlite");
    let opts = WriteOpts {
        create_index: CreateIndex::Never,
        create_table: CreateTable::IfNotExists,
        ..Default::default()
    };
    write_simple(&path, &[("k", 1, Geom::Point(1.0, 2.0))], &opts, None);
    assert!(!rtree_shadow_exists(&path, "idx_a_geom"));
}

#[test]
fn create_index_auto_creates_only_on_new_table() {
    if skip_if_not_enabled() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.sqlite");
    // 1 回目: Auto は新規 CREATE 経路で R*Tree を生成する。
    let opts = WriteOpts {
        create_index: CreateIndex::Auto,
        ..Default::default()
    };
    write_simple(&path, &[("k", 1, Geom::Point(1.0, 2.0))], &opts, None);
    assert!(rtree_shadow_exists(&path, "idx_a_geom"));

    // 2 回目: append（既存テーブル）では Auto は触らない（既存 R*Tree もそのまま）。
    // 既存 R*Tree がある状態で再度 CreateSpatialIndex を呼ぶと SpatiaLite はエラーを返すため、
    // Auto が「append 経路で発行しない」契約は finish() がエラーにならないことで担保される。
    write_simple(
        &path,
        &[("k2", 2, Geom::Point(3.0, 4.0))],
        &opts,
        None,
    );
    assert!(rtree_shadow_exists(&path, "idx_a_geom"));
}
