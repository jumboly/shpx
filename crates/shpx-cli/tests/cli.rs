//! shpx CLI の統合テスト（assert_cmd 経由）。
//!
//! v0.1 完了基準の `shpx convert SHP↔Parquet` 往復および `shpx info` 表示を確認する。

use std::path::Path;
use std::sync::Arc;

use arrow_array::{builder::BinaryBuilder, ArrayRef, RecordBatch};
use arrow_schema::{DataType, Field, Schema};
use assert_cmd::Command;
use predicates::str::contains;
use shpx_core::{
    schema::{GeometryMeta, GeometryType, GEOMETRY_META_KEY},
    Crs, Driver, Uri, WriteOpts,
};
use shpx_driver_shp::ShpDriver;
use shpx_geom::wkb::{self, Geom};

/// 単一 Point + EPSG:4326 の最小 SHP を生成する（テスト fixture）。
fn make_point_shp(path: &Path) {
    let mut field = Field::new("geometry", DataType::Binary, true);
    let meta = GeometryMeta::wkb(GeometryType::Point, Some(Crs::from_epsg(4326)));
    let mut m = std::collections::HashMap::new();
    m.insert(GEOMETRY_META_KEY.to_string(), meta.to_json().unwrap());
    field.set_metadata(m);
    let schema = Arc::new(Schema::new(vec![field]));

    let mut bb = BinaryBuilder::new();
    bb.append_value(wkb::encode(&Geom::Point(139.7, 35.7)).unwrap());
    bb.append_value(wkb::encode(&Geom::Point(-122.4, 37.8)).unwrap());
    let cols: Vec<ArrayRef> = vec![Arc::new(bb.finish())];
    let batch = RecordBatch::try_new(schema.clone(), cols).unwrap();

    let driver = ShpDriver::new();
    let uri = Uri::from_path(path.to_string_lossy().to_string());
    let mut writer = driver
        .open_write(
            &uri,
            schema,
            Some(Crs::from_epsg(4326)),
            &WriteOpts {
                overwrite: true,
                ..Default::default()
            },
        )
        .unwrap();
    writer.write_batch(&batch).unwrap();
    writer.finish().unwrap();
}

#[test]
fn info_on_shp_prints_driver_and_crs() {
    let dir = tempfile::tempdir().unwrap();
    let shp = dir.path().join("p.shp");
    make_point_shp(&shp);

    Command::cargo_bin("shpx")
        .unwrap()
        .args(["info", shp.to_str().unwrap()])
        .assert()
        .success()
        .stdout(contains("driver:  shp"))
        .stdout(contains("rows:    2"))
        .stdout(contains("crs:     EPSG:4326"))
        .stdout(contains("geometry:"));
}

#[test]
fn convert_shp_to_parquet_then_back() {
    let dir = tempfile::tempdir().unwrap();
    let shp = dir.path().join("p.shp");
    let parquet = dir.path().join("p.parquet");
    let back = dir.path().join("back.shp");
    make_point_shp(&shp);

    // SHP → Parquet
    Command::cargo_bin("shpx")
        .unwrap()
        .args([
            "convert",
            shp.to_str().unwrap(),
            parquet.to_str().unwrap(),
            "--overwrite",
        ])
        .assert()
        .success();
    assert!(parquet.exists());

    // 出力した Parquet を info で見られる + CRS が保持されている
    Command::cargo_bin("shpx")
        .unwrap()
        .args(["info", parquet.to_str().unwrap()])
        .assert()
        .success()
        .stdout(contains("driver:  parquet"))
        .stdout(contains("rows:    2"))
        .stdout(contains("crs:     EPSG:4326"));

    // Parquet → SHP
    Command::cargo_bin("shpx")
        .unwrap()
        .args([
            "convert",
            parquet.to_str().unwrap(),
            back.to_str().unwrap(),
            "--overwrite",
        ])
        .assert()
        .success();
    assert!(back.exists());
    // .prj は EPSG:4326 が hardcode テーブルに含まれるので生成される
    assert!(back.with_extension("prj").exists());
}

#[test]
fn convert_without_overwrite_fails_when_target_exists() {
    let dir = tempfile::tempdir().unwrap();
    let shp = dir.path().join("p.shp");
    let parquet = dir.path().join("p.parquet");
    make_point_shp(&shp);

    // 1 度目は成功
    Command::cargo_bin("shpx")
        .unwrap()
        .args([
            "convert",
            shp.to_str().unwrap(),
            parquet.to_str().unwrap(),
            "--overwrite",
        ])
        .assert()
        .success();

    // 2 度目は --overwrite 無しで失敗
    Command::cargo_bin("shpx")
        .unwrap()
        .args(["convert", shp.to_str().unwrap(), parquet.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(contains("already exists"));
}

#[test]
fn schema_emits_valid_json() {
    let dir = tempfile::tempdir().unwrap();
    let shp = dir.path().join("p.shp");
    make_point_shp(&shp);

    let output = Command::cargo_bin("shpx")
        .unwrap()
        .args(["schema", shp.to_str().unwrap()])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output).unwrap();
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("schema output must be valid JSON");
    assert_eq!(parsed["driver"], "shp");
    let fields = parsed["fields"].as_array().expect("fields must be array");
    assert!(fields.iter().any(|f| f["name"] == "geometry"));
    // `shpx:geometry` は文字列ではなくパースされた object として埋め込む。
    let geom_field = fields.iter().find(|f| f["name"] == "geometry").unwrap();
    let meta = &geom_field["metadata"]["shpx:geometry"];
    assert!(meta.is_object(), "shpx:geometry must be parsed JSON object");
}

#[test]
fn drivers_lists_shp_and_parquet() {
    Command::cargo_bin("shpx")
        .unwrap()
        .args(["drivers"])
        .assert()
        .success()
        .stdout(contains("- shp"))
        .stdout(contains("- parquet"))
        .stdout(contains("schemes:"))
        .stdout(contains("read+write"));
}

#[test]
fn info_with_unknown_extension_fails() {
    let dir = tempfile::tempdir().unwrap();
    let nope = dir.path().join("foo.unknown");
    std::fs::write(&nope, "x").unwrap();

    Command::cargo_bin("shpx")
        .unwrap()
        .args(["info", nope.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(contains("no driver"));
}
