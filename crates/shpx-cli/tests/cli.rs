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
        .stdout(contains("- csv"))
        .stdout(contains("- geojson"))
        .stdout(contains("- gpkg"))
        .stdout(contains("- fgb"))
        .stdout(contains("schemes:"))
        .stdout(contains("read+write"));
}

#[test]
fn convert_shp_to_csv_then_back() {
    let dir = tempfile::tempdir().unwrap();
    let shp = dir.path().join("p.shp");
    let csv = dir.path().join("p.csv");
    let back = dir.path().join("back.shp");
    make_point_shp(&shp);

    // SHP → CSV
    Command::cargo_bin("shpx")
        .unwrap()
        .args([
            "convert",
            shp.to_str().unwrap(),
            csv.to_str().unwrap(),
            "--overwrite",
        ])
        .assert()
        .success();
    assert!(csv.exists());

    // CSV → SHP（src-crs で 4326 を補完。CSV はネイティブ CRS を持たないため）
    Command::cargo_bin("shpx")
        .unwrap()
        .args([
            "convert",
            csv.to_str().unwrap(),
            back.to_str().unwrap(),
            "--overwrite",
            "--src-crs",
            "EPSG:4326",
        ])
        .assert()
        .success();
    assert!(back.exists());
    assert!(back.with_extension("prj").exists());
}

#[test]
fn convert_shp_to_geojson_then_back() {
    let dir = tempfile::tempdir().unwrap();
    let shp = dir.path().join("p.shp");
    let geojson = dir.path().join("p.geojson");
    let back = dir.path().join("back.shp");
    make_point_shp(&shp);

    // SHP → GeoJSON（SHP の CRS は EPSG:4326 で WGS84 制約を満たす）
    Command::cargo_bin("shpx")
        .unwrap()
        .args([
            "convert",
            shp.to_str().unwrap(),
            geojson.to_str().unwrap(),
            "--overwrite",
        ])
        .assert()
        .success();
    assert!(geojson.exists());

    // info で 2 行・EPSG:4326 が表示されること。
    Command::cargo_bin("shpx")
        .unwrap()
        .args(["info", geojson.to_str().unwrap()])
        .assert()
        .success()
        .stdout(contains("driver:  geojson"))
        .stdout(contains("rows:    2"))
        .stdout(contains("crs:     EPSG:4326"));

    // GeoJSON → SHP
    Command::cargo_bin("shpx")
        .unwrap()
        .args([
            "convert",
            geojson.to_str().unwrap(),
            back.to_str().unwrap(),
            "--overwrite",
        ])
        .assert()
        .success();
    assert!(back.exists());
    assert!(back.with_extension("prj").exists());
}

#[test]
fn convert_shp_to_gpkg_then_back() {
    let dir = tempfile::tempdir().unwrap();
    let shp = dir.path().join("p.shp");
    let gpkg = dir.path().join("p.gpkg");
    let back = dir.path().join("back.shp");
    make_point_shp(&shp);

    // SHP → GPKG
    Command::cargo_bin("shpx")
        .unwrap()
        .args([
            "convert",
            shp.to_str().unwrap(),
            gpkg.to_str().unwrap(),
            "--overwrite",
        ])
        .assert()
        .success();
    assert!(gpkg.exists());

    Command::cargo_bin("shpx")
        .unwrap()
        .args(["info", gpkg.to_str().unwrap()])
        .assert()
        .success()
        .stdout(contains("driver:  gpkg"))
        .stdout(contains("rows:    2"))
        .stdout(contains("crs:     EPSG:4326"));

    // GPKG → SHP
    Command::cargo_bin("shpx")
        .unwrap()
        .args([
            "convert",
            gpkg.to_str().unwrap(),
            back.to_str().unwrap(),
            "--overwrite",
        ])
        .assert()
        .success();
    assert!(back.exists());
    assert!(back.with_extension("prj").exists());
}

#[test]
fn convert_shp_to_gpkg_then_geojson() {
    let dir = tempfile::tempdir().unwrap();
    let shp = dir.path().join("p.shp");
    let gpkg = dir.path().join("p.gpkg");
    let geojson = dir.path().join("p.geojson");
    make_point_shp(&shp);

    Command::cargo_bin("shpx")
        .unwrap()
        .args([
            "convert",
            shp.to_str().unwrap(),
            gpkg.to_str().unwrap(),
            "--overwrite",
        ])
        .assert()
        .success();

    // GPKG → GeoJSON（GPKG の CRS=4326 がそのまま GeoJSON 仕様の WGS84 制約を満たす）
    Command::cargo_bin("shpx")
        .unwrap()
        .args([
            "convert",
            gpkg.to_str().unwrap(),
            geojson.to_str().unwrap(),
            "--overwrite",
        ])
        .assert()
        .success();
    assert!(geojson.exists());
}

#[test]
fn convert_shp_to_geojsonl_uses_lines_format() {
    let dir = tempfile::tempdir().unwrap();
    let shp = dir.path().join("p.shp");
    let ndjson = dir.path().join("p.geojsonl");
    make_point_shp(&shp);

    Command::cargo_bin("shpx")
        .unwrap()
        .args([
            "convert",
            shp.to_str().unwrap(),
            ndjson.to_str().unwrap(),
            "--overwrite",
        ])
        .assert()
        .success();
    let raw = std::fs::read_to_string(&ndjson).unwrap();
    let lines: Vec<&str> = raw.lines().collect();
    // 2 件の Point → 2 行。各行が Feature object。
    assert_eq!(lines.len(), 2);
    assert!(lines[0].starts_with('{'));
    assert!(lines[0].contains(r#""type":"Feature""#));
}

#[test]
fn convert_shp_to_fgb_then_back() {
    let dir = tempfile::tempdir().unwrap();
    let shp = dir.path().join("p.shp");
    let fgb = dir.path().join("p.fgb");
    let back = dir.path().join("back.shp");
    make_point_shp(&shp);

    // SHP → FGB
    Command::cargo_bin("shpx")
        .unwrap()
        .args([
            "convert",
            shp.to_str().unwrap(),
            fgb.to_str().unwrap(),
            "--overwrite",
        ])
        .assert()
        .success();
    assert!(fgb.exists());

    Command::cargo_bin("shpx")
        .unwrap()
        .args(["info", fgb.to_str().unwrap()])
        .assert()
        .success()
        .stdout(contains("driver:  fgb"))
        .stdout(contains("rows:    2"))
        .stdout(contains("crs:     EPSG:4326"));

    // FGB → SHP
    Command::cargo_bin("shpx")
        .unwrap()
        .args([
            "convert",
            fgb.to_str().unwrap(),
            back.to_str().unwrap(),
            "--overwrite",
        ])
        .assert()
        .success();
    assert!(back.exists());
    assert!(back.with_extension("prj").exists());
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

/// `--reproject EPSG:3857` で出力 Parquet が EPSG:3857 にタグされ、
/// 座標が経緯度から Web メルカトル (m オーダー) に変換されていること。
#[test]
fn convert_shp_to_parquet_with_reproject() {
    use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
    use std::fs::File;

    let dir = tempfile::tempdir().unwrap();
    let shp = dir.path().join("p.shp");
    let parquet = dir.path().join("p.parquet");
    make_point_shp(&shp);

    Command::cargo_bin("shpx")
        .unwrap()
        .args([
            "convert",
            shp.to_str().unwrap(),
            parquet.to_str().unwrap(),
            "--overwrite",
            "--reproject",
            "EPSG:3857",
        ])
        .assert()
        .success();

    Command::cargo_bin("shpx")
        .unwrap()
        .args(["info", parquet.to_str().unwrap()])
        .assert()
        .success()
        .stdout(contains("crs:     EPSG:3857"));

    // 元の Point は (139.7, 35.7) [lon, lat]。3857 後は数百万 m スケール。
    let file = File::open(&parquet).unwrap();
    let builder = ParquetRecordBatchReaderBuilder::try_new(file).unwrap();
    let mut reader = builder.build().unwrap();
    let batch = reader.next().unwrap().unwrap();
    let geom_col = batch
        .column_by_name("geometry")
        .expect("geometry column present");
    let arr = geom_col
        .as_any()
        .downcast_ref::<arrow_array::BinaryArray>()
        .unwrap();
    let g = wkb::decode(arr.value(0)).unwrap();
    let Geom::Point(x, y) = g else {
        panic!("not Point");
    };
    // 139.7 度 ≈ 15555000 m, 35.7 度 ≈ 4260000 m (Web メルカトル定義式)。
    assert!(
        (15_500_000.0..16_000_000.0).contains(&x),
        "x out of 3857 range: {x}"
    );
    assert!(
        (4_200_000.0..4_300_000.0).contains(&y),
        "y out of 3857 range: {y}"
    );
}

/// 入力 CRS が解決できないファイル (CRS 無し CSV) で `--reproject` を指定すると
/// エラーで停止する。`--src-crs` を併用すれば成功する。
#[test]
fn reproject_without_src_crs_errors() {
    let dir = tempfile::tempdir().unwrap();
    let shp = dir.path().join("p.shp");
    let csv = dir.path().join("p.csv");
    let parquet = dir.path().join("p.parquet");
    make_point_shp(&shp);

    // CSV を経由して CRS 情報を落とす。
    Command::cargo_bin("shpx")
        .unwrap()
        .args([
            "convert",
            shp.to_str().unwrap(),
            csv.to_str().unwrap(),
            "--overwrite",
        ])
        .assert()
        .success();

    // --src-crs 無しでの --reproject はエラー。
    Command::cargo_bin("shpx")
        .unwrap()
        .args([
            "convert",
            csv.to_str().unwrap(),
            parquet.to_str().unwrap(),
            "--overwrite",
            "--reproject",
            "EPSG:3857",
        ])
        .assert()
        .failure()
        .stderr(contains("--reproject requires source CRS"));

    // --src-crs 併用で成功する。
    Command::cargo_bin("shpx")
        .unwrap()
        .args([
            "convert",
            csv.to_str().unwrap(),
            parquet.to_str().unwrap(),
            "--overwrite",
            "--reproject",
            "EPSG:3857",
            "--src-crs",
            "EPSG:4326",
        ])
        .assert()
        .success();
}

/// src と target が同一 CRS なら no-op パスが選ばれる（出力データの整合性を確認）。
#[test]
fn reproject_identity_succeeds() {
    let dir = tempfile::tempdir().unwrap();
    let shp = dir.path().join("p.shp");
    let parquet = dir.path().join("p.parquet");
    make_point_shp(&shp);

    Command::cargo_bin("shpx")
        .unwrap()
        .args([
            "convert",
            shp.to_str().unwrap(),
            parquet.to_str().unwrap(),
            "--overwrite",
            "--reproject",
            "EPSG:4326",
        ])
        .assert()
        .success();

    Command::cargo_bin("shpx")
        .unwrap()
        .args(["info", parquet.to_str().unwrap()])
        .assert()
        .success()
        .stdout(contains("rows:    2"))
        .stdout(contains("crs:     EPSG:4326"));
}
