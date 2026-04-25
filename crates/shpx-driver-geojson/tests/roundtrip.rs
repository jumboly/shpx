//! GeoJSON / GeoJSONL reader/writer の往復テスト。
//!
//! `tempfile::tempdir` 上で write → read で完結させる（CSV ドライバの構成と並列）。
//! Reader 側に手書き JSON ファイルを与えるテストは reader ロジック専用に使う。

use std::sync::Arc;

use arrow_array::{
    builder::{BinaryBuilder, StringBuilder},
    Array, ArrayRef, BinaryArray, RecordBatch,
};
use arrow_schema::{DataType, Field, Schema};
use shpx_core::{
    schema::{GeometryMeta, GeometryType, GEOMETRY_META_KEY},
    Crs, Driver, ReadOpts, Uri,
};
use shpx_driver_geojson::GeoJsonDriver;
use shpx_geom::wkb::{self, Geom};

#[allow(dead_code)]
fn schema_with_geom(extra: Vec<Field>, gt: GeometryType, crs: Option<Crs>) -> Arc<Schema> {
    let mut fields = extra;
    let meta = GeometryMeta::wkb(gt, crs);
    let mut field = Field::new("geometry", DataType::Binary, true);
    let mut m = std::collections::HashMap::new();
    m.insert(GEOMETRY_META_KEY.to_string(), meta.to_json().unwrap());
    field.set_metadata(m);
    fields.push(field);
    Arc::new(Schema::new(fields))
}

fn read_back(
    path: &std::path::Path,
    src_crs: Option<Crs>,
) -> (Arc<Schema>, Option<Crs>, Vec<RecordBatch>) {
    let driver = GeoJsonDriver::new();
    let uri = Uri::from_path(path.to_string_lossy().to_string());
    let opts = ReadOpts {
        src_crs,
        ..Default::default()
    };
    let mut r = driver.open_read(&uri, &opts).unwrap();
    let schema = r.schema();
    let crs = r.crs().cloned();
    let batches: Vec<_> = r.batches().collect::<Result<_, _>>().unwrap();
    (schema, crs, batches)
}

fn geom_col(batch: &RecordBatch) -> &BinaryArray {
    batch
        .column_by_name("geometry")
        .unwrap()
        .as_any()
        .downcast_ref::<BinaryArray>()
        .unwrap()
}

fn write_geojson(path: &std::path::Path, json: &str) {
    std::fs::write(path, json).unwrap();
}

// ---- Reader 単体テスト（手書き JSON 入力） ----

const POINT_FC: &str = r#"{
  "type": "FeatureCollection",
  "features": [
    {"type":"Feature","geometry":{"type":"Point","coordinates":[1.0,2.0]},"properties":{"name":"alpha"}},
    {"type":"Feature","geometry":{"type":"Point","coordinates":[-3.5,4.25]},"properties":{"name":"beta"}}
  ]
}"#;

#[test]
fn feature_collection_point_read() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("points.geojson");
    write_geojson(&p, POINT_FC);
    let (schema, crs, batches) = read_back(&p, None);

    // geometry 列はスキーマの末尾。
    assert_eq!(schema.fields().len(), 2);
    assert_eq!(schema.field(0).name(), "name");
    assert_eq!(schema.field(0).data_type(), &DataType::Utf8);
    assert_eq!(schema.field(1).name(), "geometry");
    assert_eq!(schema.field(1).data_type(), &DataType::Binary);

    // RFC 7946 既定の EPSG:4326 が補完されること。
    assert_eq!(crs.as_ref().and_then(Crs::epsg_code), Some(4326));

    assert_eq!(batches.len(), 1);
    assert_eq!(batches[0].num_rows(), 2);
    let g = geom_col(&batches[0]);
    assert_eq!(wkb::decode(g.value(0)).unwrap(), Geom::Point(1.0, 2.0));
    assert_eq!(wkb::decode(g.value(1)).unwrap(), Geom::Point(-3.5, 4.25));
}

const POLYGON_FC: &str = r#"{
  "type":"FeatureCollection",
  "features":[{
    "type":"Feature",
    "geometry":{
      "type":"Polygon",
      "coordinates":[
        [[0,0],[4,0],[4,4],[0,4],[0,0]],
        [[1,1],[2,1],[2,2],[1,2],[1,1]]
      ]
    },
    "properties":{}
  }]
}"#;

#[test]
fn feature_collection_polygon_with_hole_read() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("poly.geojson");
    write_geojson(&p, POLYGON_FC);
    let (_s, _c, batches) = read_back(&p, None);
    let g = wkb::decode(geom_col(&batches[0]).value(0)).unwrap();
    assert_eq!(
        g,
        Geom::Polygon(vec![
            vec![(0.0, 0.0), (4.0, 0.0), (4.0, 4.0), (0.0, 4.0), (0.0, 0.0)],
            vec![(1.0, 1.0), (2.0, 1.0), (2.0, 2.0), (1.0, 2.0), (1.0, 1.0)],
        ])
    );
}

#[test]
fn null_geometry_read() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("null_geom.geojson");
    write_geojson(
        &p,
        r#"{"type":"FeatureCollection","features":[
            {"type":"Feature","geometry":null,"properties":{"k":"v"}},
            {"type":"Feature","geometry":{"type":"Point","coordinates":[1,2]},"properties":{"k":"w"}}
        ]}"#,
    );
    let (_s, _c, batches) = read_back(&p, None);
    let g = geom_col(&batches[0]);
    assert!(g.is_null(0));
    assert!(!g.is_null(1));
    assert_eq!(wkb::decode(g.value(1)).unwrap(), Geom::Point(1.0, 2.0));
}

#[test]
fn null_property_makes_column_nullable_and_handles_missing_keys() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("null_props.geojson");
    write_geojson(
        &p,
        r#"{"type":"FeatureCollection","features":[
            {"type":"Feature","geometry":{"type":"Point","coordinates":[0,0]},"properties":{"name":"a"}},
            {"type":"Feature","geometry":{"type":"Point","coordinates":[1,1]},"properties":{"name":null}},
            {"type":"Feature","geometry":{"type":"Point","coordinates":[2,2]},"properties":{}}
        ]}"#,
    );
    let (_s, _c, batches) = read_back(&p, None);
    let name = batches[0]
        .column_by_name("name")
        .unwrap()
        .as_any()
        .downcast_ref::<arrow_array::StringArray>()
        .unwrap();
    assert_eq!(name.value(0), "a");
    assert!(name.is_null(1));
    assert!(name.is_null(2));
}

#[test]
fn mixed_int_float_promotes_to_float64() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("mixed_num.geojson");
    write_geojson(
        &p,
        r#"{"type":"FeatureCollection","features":[
            {"type":"Feature","geometry":{"type":"Point","coordinates":[0,0]},"properties":{"v":1}},
            {"type":"Feature","geometry":{"type":"Point","coordinates":[1,1]},"properties":{"v":1.5}}
        ]}"#,
    );
    let (schema, _c, batches) = read_back(&p, None);
    assert_eq!(schema.field(0).data_type(), &DataType::Float64);
    let v = batches[0]
        .column_by_name("v")
        .unwrap()
        .as_any()
        .downcast_ref::<arrow_array::Float64Array>()
        .unwrap();
    assert!((v.value(0) - 1.0).abs() < 1e-12);
    assert!((v.value(1) - 1.5).abs() < 1e-12);
}

#[test]
fn mixed_int_string_falls_back_to_utf8() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("mixed_kind.geojson");
    write_geojson(
        &p,
        r#"{"type":"FeatureCollection","features":[
            {"type":"Feature","geometry":{"type":"Point","coordinates":[0,0]},"properties":{"v":1}},
            {"type":"Feature","geometry":{"type":"Point","coordinates":[1,1]},"properties":{"v":"abc"}}
        ]}"#,
    );
    let (schema, _c, batches) = read_back(&p, None);
    assert_eq!(schema.field(0).data_type(), &DataType::Utf8);
    let v = batches[0]
        .column_by_name("v")
        .unwrap()
        .as_any()
        .downcast_ref::<arrow_array::StringArray>()
        .unwrap();
    assert_eq!(v.value(0), "1");
    assert_eq!(v.value(1), "abc");
}

#[test]
fn new_key_in_later_feature_appended() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("new_key.geojson");
    write_geojson(
        &p,
        r#"{"type":"FeatureCollection","features":[
            {"type":"Feature","geometry":{"type":"Point","coordinates":[0,0]},"properties":{"a":1}},
            {"type":"Feature","geometry":{"type":"Point","coordinates":[1,1]},"properties":{"a":2,"b":"x"}}
        ]}"#,
    );
    let (schema, _c, batches) = read_back(&p, None);
    // 列順: 最初の Feature の properties 順 + 新規キー追加。geometry が末尾。
    assert_eq!(schema.field(0).name(), "a");
    assert_eq!(schema.field(1).name(), "b");
    assert_eq!(schema.field(2).name(), "geometry");
    let b = batches[0]
        .column_by_name("b")
        .unwrap()
        .as_any()
        .downcast_ref::<arrow_array::StringArray>()
        .unwrap();
    assert!(b.is_null(0));
    assert_eq!(b.value(1), "x");
}

#[test]
fn empty_feature_collection_yields_no_batches() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("empty.geojson");
    write_geojson(&p, r#"{"type":"FeatureCollection","features":[]}"#);
    let (_s, _c, batches) = read_back(&p, None);
    assert!(batches.is_empty());
}

#[test]
fn legacy_top_level_crs_member_recognized() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("legacy_crs.geojson");
    write_geojson(
        &p,
        r#"{
          "type":"FeatureCollection",
          "crs":{"type":"name","properties":{"name":"urn:ogc:def:crs:EPSG::3857"}},
          "features":[{"type":"Feature","geometry":{"type":"Point","coordinates":[0,0]},"properties":{}}]
        }"#,
    );
    let (_s, crs, _b) = read_back(&p, None);
    assert_eq!(crs.as_ref().and_then(Crs::epsg_code), Some(3857));
}

#[test]
fn geometry_collection_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("gc.geojson");
    write_geojson(
        &p,
        r#"{"type":"FeatureCollection","features":[
            {"type":"Feature","geometry":{"type":"GeometryCollection","geometries":[{"type":"Point","coordinates":[0,0]}]},"properties":{}}
        ]}"#,
    );
    let driver = GeoJsonDriver::new();
    let uri = Uri::from_path(p.to_string_lossy().to_string());
    let r = driver.open_read(&uri, &ReadOpts::default());
    let err = r.err().unwrap();
    match err {
        shpx_core::Error::Geometry(m) => assert!(m.contains("GeometryCollection")),
        other => panic!("expected Error::Geometry, got {other:?}"),
    }
}

#[test]
fn malformed_json_returns_driver_error() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("bad.geojson");
    write_geojson(&p, "{ not json");
    let driver = GeoJsonDriver::new();
    let uri = Uri::from_path(p.to_string_lossy().to_string());
    let r = driver.open_read(&uri, &ReadOpts::default());
    assert!(matches!(r, Err(shpx_core::Error::Driver { .. })));
}

#[test]
fn three_d_coordinates_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("z.geojson");
    write_geojson(
        &p,
        r#"{"type":"FeatureCollection","features":[
            {"type":"Feature","geometry":{"type":"Point","coordinates":[1,2,3]},"properties":{}}
        ]}"#,
    );
    let driver = GeoJsonDriver::new();
    let uri = Uri::from_path(p.to_string_lossy().to_string());
    let mut r = driver.open_read(&uri, &ReadOpts::default()).unwrap();
    let err = r.batches().next().unwrap().unwrap_err();
    match err {
        shpx_core::Error::Geometry(m) => assert!(m.contains("3D")),
        other => panic!("expected Error::Geometry, got {other:?}"),
    }
}

// ---- Writer 連携テスト用のスタブ（次コミットで body 追加） ----

#[allow(dead_code)]
fn _writer_helpers_placeholder(
    _schema: Arc<Schema>,
    _geoms: &[Option<Geom>],
    _attrs: Vec<ArrayRef>,
    _bb: BinaryBuilder,
    _sb: StringBuilder,
) {
    // Writer roundtrip テストは Commit 5 / 6 で追加する。
}
