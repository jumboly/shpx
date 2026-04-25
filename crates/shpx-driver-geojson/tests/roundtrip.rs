//! GeoJSON / GeoJSONL reader/writer の往復テスト。
//!
//! `tempfile::tempdir` 上で write → read で完結させる（CSV ドライバの構成と並列）。
//! Reader 側に手書き JSON ファイルを与えるテストは reader ロジック専用に使う。

use std::sync::Arc;

use arrow_array::{
    builder::{BinaryBuilder, Int64Builder, StringBuilder},
    Array, ArrayRef, BinaryArray, RecordBatch,
};
use arrow_schema::{DataType, Field, Schema};
use shpx_core::{
    schema::{GeometryMeta, GeometryType, GEOMETRY_META_KEY},
    Crs, Driver, ReadOpts, Uri, WriteOpts,
};
use shpx_driver_geojson::GeoJsonDriver;
use shpx_geom::wkb::{self, Geom};

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

// ---- GeoJSONL (NDJSON) reader テスト ----

#[test]
fn geojsonl_reads_one_feature_per_line() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("data.geojsonl");
    write_geojson(
        &p,
        concat!(
            r#"{"type":"Feature","geometry":{"type":"Point","coordinates":[1,2]},"properties":{"name":"a"}}"#,
            "\n",
            r#"{"type":"Feature","geometry":{"type":"Point","coordinates":[3,4]},"properties":{"name":"b"}}"#,
            "\n",
            r#"{"type":"Feature","geometry":{"type":"Point","coordinates":[5,6]},"properties":{"name":"c"}}"#,
            "\n",
        ),
    );
    let (_s, _c, batches) = read_back(&p, None);
    assert_eq!(batches.len(), 1);
    assert_eq!(batches[0].num_rows(), 3);
    let g = geom_col(&batches[0]);
    assert_eq!(wkb::decode(g.value(0)).unwrap(), Geom::Point(1.0, 2.0));
    assert_eq!(wkb::decode(g.value(2)).unwrap(), Geom::Point(5.0, 6.0));
}

#[test]
fn geojsonl_skips_blank_and_comment_lines() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("with_blanks.geojsonl");
    write_geojson(
        &p,
        concat!(
            "# header comment\n",
            "\n",
            r#"{"type":"Feature","geometry":{"type":"Point","coordinates":[1,2]},"properties":{}}"#,
            "\n",
            "   \n",
            r#"{"type":"Feature","geometry":{"type":"Point","coordinates":[3,4]},"properties":{}}"#,
            "\n",
        ),
    );
    let (_s, _c, batches) = read_back(&p, None);
    assert_eq!(batches[0].num_rows(), 2);
}

#[test]
fn ndjson_extension_uses_geojson_driver() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("data.ndjson");
    write_geojson(
        &p,
        concat!(
            r#"{"type":"Feature","geometry":{"type":"Point","coordinates":[0,0]},"properties":{}}"#,
            "\n",
        ),
    );
    let (_s, _c, batches) = read_back(&p, None);
    assert_eq!(batches[0].num_rows(), 1);
}

// ---- Writer + Roundtrip テスト ----

fn default_write_opts() -> WriteOpts {
    WriteOpts {
        overwrite: true,
        ..Default::default()
    }
}

fn write_geoms(
    path: &std::path::Path,
    schema: Arc<Schema>,
    geoms: &[Option<Geom>],
    attrs: Vec<ArrayRef>,
    crs: Option<Crs>,
    opts: &WriteOpts,
) {
    let mut bb = BinaryBuilder::new();
    for g in geoms {
        match g {
            Some(g) => bb.append_value(wkb::encode(g).unwrap()),
            None => bb.append_null(),
        }
    }
    let mut cols = attrs;
    cols.push(Arc::new(bb.finish()) as ArrayRef);
    let batch = RecordBatch::try_new(schema.clone(), cols).unwrap();

    let driver = GeoJsonDriver::new();
    let uri = Uri::from_path(path.to_string_lossy().to_string());
    let mut w = driver.open_write(&uri, schema, crs, opts).unwrap();
    w.write_batch(&batch).unwrap();
    w.finish().unwrap();
}

#[test]
fn point_roundtrip_feature_collection() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("point_rt.geojson");
    let schema = schema_with_geom(
        vec![Field::new("name", DataType::Utf8, true)],
        GeometryType::Point,
        Some(Crs::from_epsg(4326)),
    );
    let mut name = StringBuilder::new();
    name.append_value("alpha");
    name.append_value("beta");
    let attrs: Vec<ArrayRef> = vec![Arc::new(name.finish())];
    write_geoms(
        &p,
        schema.clone(),
        &[Some(Geom::Point(1.0, 2.0)), Some(Geom::Point(-3.5, 4.25))],
        attrs,
        Some(Crs::from_epsg(4326)),
        &default_write_opts(),
    );

    let (_s, _c, batches) = read_back(&p, None);
    assert_eq!(batches.len(), 1);
    assert_eq!(batches[0].num_rows(), 2);
    let g = geom_col(&batches[0]);
    assert_eq!(wkb::decode(g.value(0)).unwrap(), Geom::Point(1.0, 2.0));
    let name = batches[0]
        .column_by_name("name")
        .unwrap()
        .as_any()
        .downcast_ref::<arrow_array::StringArray>()
        .unwrap();
    assert_eq!(name.value(0), "alpha");
    assert_eq!(name.value(1), "beta");
}

#[test]
fn polygon_with_hole_roundtrip_feature_collection() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("poly_rt.geojson");
    let schema = schema_with_geom(vec![], GeometryType::Polygon, None);
    let g = Geom::Polygon(vec![
        vec![(0.0, 0.0), (4.0, 0.0), (4.0, 4.0), (0.0, 4.0), (0.0, 0.0)],
        vec![(1.0, 1.0), (1.0, 2.0), (2.0, 2.0), (2.0, 1.0), (1.0, 1.0)],
    ]);
    write_geoms(
        &p,
        schema.clone(),
        &[Some(g.clone())],
        vec![],
        None,
        &default_write_opts(),
    );
    let (_s, _c, batches) = read_back(&p, None);
    assert_eq!(wkb::decode(geom_col(&batches[0]).value(0)).unwrap(), g);
}

#[test]
fn multilinestring_roundtrip_feature_collection() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("mls_rt.geojson");
    let schema = schema_with_geom(vec![], GeometryType::MultiLineString, None);
    let g = Geom::MultiLineString(vec![
        vec![(0.0, 0.0), (1.0, 1.0)],
        vec![(10.0, 10.0), (11.0, 11.0), (12.0, 10.5)],
    ]);
    write_geoms(
        &p,
        schema.clone(),
        &[Some(g.clone())],
        vec![],
        None,
        &default_write_opts(),
    );
    let (_s, _c, batches) = read_back(&p, None);
    assert_eq!(wkb::decode(geom_col(&batches[0]).value(0)).unwrap(), g);
}

#[test]
fn null_geometry_roundtrip_feature_collection() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("null_rt.geojson");
    let schema = schema_with_geom(vec![], GeometryType::Geometry, None);
    write_geoms(
        &p,
        schema.clone(),
        &[None, Some(Geom::Point(1.0, 2.0))],
        vec![],
        None,
        &default_write_opts(),
    );
    let (_s, _c, batches) = read_back(&p, None);
    let g = geom_col(&batches[0]);
    assert!(g.is_null(0));
    assert!(!g.is_null(1));
}

#[test]
fn typed_int_property_roundtrip_feature_collection() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("int_rt.geojson");
    let schema = schema_with_geom(
        vec![Field::new("count", DataType::Int64, true)],
        GeometryType::Point,
        None,
    );
    let mut count = Int64Builder::new();
    count.append_value(42);
    count.append_value(-7);
    let attrs: Vec<ArrayRef> = vec![Arc::new(count.finish())];
    write_geoms(
        &p,
        schema.clone(),
        &[Some(Geom::Point(0.0, 0.0)), Some(Geom::Point(1.0, 1.0))],
        attrs,
        None,
        &default_write_opts(),
    );

    let (read_schema, _c, batches) = read_back(&p, None);
    assert_eq!(read_schema.field(0).data_type(), &DataType::Int64);
    let count = batches[0]
        .column_by_name("count")
        .unwrap()
        .as_any()
        .downcast_ref::<arrow_array::Int64Array>()
        .unwrap();
    assert_eq!(count.value(0), 42);
    assert_eq!(count.value(1), -7);
}

#[test]
fn writer_rejects_non_wgs84_crs() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("3857.geojson");
    let schema = schema_with_geom(vec![], GeometryType::Point, Some(Crs::from_epsg(3857)));
    let driver = GeoJsonDriver::new();
    let uri = Uri::from_path(p.to_string_lossy().to_string());
    let r = driver.open_write(
        &uri,
        schema,
        Some(Crs::from_epsg(3857)),
        &default_write_opts(),
    );
    match r {
        Err(shpx_core::Error::Crs(msg)) => assert!(msg.contains("EPSG:4326")),
        Err(other) => panic!("expected Error::Crs, got {other:?}"),
        Ok(_) => panic!("expected Error::Crs, got Ok(_)"),
    }
}

#[test]
fn writer_accepts_none_and_wgs84() {
    let dir = tempfile::tempdir().unwrap();
    let schema = schema_with_geom(vec![], GeometryType::Point, None);
    let driver = GeoJsonDriver::new();

    // None
    let p1 = dir.path().join("none.geojson");
    let w = driver
        .open_write(
            &Uri::from_path(p1.to_string_lossy().to_string()),
            schema.clone(),
            None,
            &default_write_opts(),
        )
        .unwrap();
    w.finish().unwrap();
    assert!(p1.exists());

    // EPSG:4326
    let p2 = dir.path().join("4326.geojson");
    let w = driver
        .open_write(
            &Uri::from_path(p2.to_string_lossy().to_string()),
            schema,
            Some(Crs::from_epsg(4326)),
            &default_write_opts(),
        )
        .unwrap();
    w.finish().unwrap();
    assert!(p2.exists());
}

#[test]
fn writer_overwrite_false_fails_when_exists() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("exists.geojson");
    let schema = schema_with_geom(vec![], GeometryType::Point, None);
    write_geoms(
        &p,
        schema.clone(),
        &[Some(Geom::Point(0.0, 0.0))],
        vec![],
        None,
        &default_write_opts(),
    );

    let driver = GeoJsonDriver::new();
    let uri = Uri::from_path(p.to_string_lossy().to_string());
    let r = driver.open_write(&uri, schema, None, &WriteOpts::default());
    assert!(matches!(r, Err(shpx_core::Error::Format(_))));
}

#[test]
fn feature_collection_writer_emits_correct_envelope() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("envelope.geojson");
    let schema = schema_with_geom(vec![], GeometryType::Point, None);
    write_geoms(
        &p,
        schema,
        &[Some(Geom::Point(1.0, 2.0)), Some(Geom::Point(3.0, 4.0))],
        vec![],
        None,
        &default_write_opts(),
    );

    let raw = std::fs::read_to_string(&p).unwrap();
    assert!(raw.starts_with(r#"{"type":"FeatureCollection","features":["#));
    assert!(raw.ends_with("]}"));
    // 2 件目の feature は `,` で区切られていること（feature 間の `},{`）。
    assert!(
        raw.contains("},{"),
        "expected comma-separated features in `{raw}`"
    );
    // Feature 文字列が 2 件あること。
    assert_eq!(raw.matches(r#""type":"Feature""#).count(), 2);
}
