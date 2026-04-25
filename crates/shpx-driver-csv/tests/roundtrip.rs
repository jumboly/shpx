//! CSV/TSV reader/writer の往復テスト。
//!
//! `tempfile::tempdir` 上で write → read で完結させる。`assert_cmd` は使わない。

use std::sync::Arc;

use arrow_array::{
    builder::{BinaryBuilder, StringBuilder},
    ArrayRef, RecordBatch,
};
use arrow_schema::{DataType, Field, Schema};
use shpx_core::{
    schema::{GeometryMeta, GeometryType, GEOMETRY_META_KEY},
    Crs, Driver, ReadOpts, Uri, WriteOpts,
};
use shpx_driver_csv::CsvDriver;
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

    let driver = CsvDriver::new();
    let uri = Uri::from_path(path.to_string_lossy().to_string());
    let mut w = driver.open_write(&uri, schema, crs, opts).unwrap();
    w.write_batch(&batch).unwrap();
    w.finish().unwrap();
}

fn read_back(
    path: &std::path::Path,
    src_crs: Option<Crs>,
) -> (Arc<Schema>, Option<Crs>, Vec<RecordBatch>) {
    let driver = CsvDriver::new();
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

fn default_write_opts() -> WriteOpts {
    WriteOpts {
        overwrite: true,
        ..Default::default()
    }
}

fn geom_col(batch: &RecordBatch) -> &arrow_array::BinaryArray {
    batch
        .column_by_name("geometry")
        .unwrap()
        .as_any()
        .downcast_ref::<arrow_array::BinaryArray>()
        .unwrap()
}

#[test]
fn point_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("point.csv");
    let schema = schema_with_geom(vec![], GeometryType::Geometry, None);
    write_geoms(
        &p,
        schema.clone(),
        &[Some(Geom::Point(1.0, 2.0)), Some(Geom::Point(-3.5, 4.25))],
        vec![],
        None,
        &default_write_opts(),
    );

    let (_s, _c, batches) = read_back(&p, None);
    assert_eq!(batches.len(), 1);
    let geom = geom_col(&batches[0]);
    assert_eq!(wkb::decode(geom.value(0)).unwrap(), Geom::Point(1.0, 2.0));
    assert_eq!(wkb::decode(geom.value(1)).unwrap(), Geom::Point(-3.5, 4.25));
}

#[test]
fn polygon_with_hole_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("poly.csv");
    let schema = schema_with_geom(vec![], GeometryType::Geometry, None);
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
fn multilinestring_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("mls.csv");
    let schema = schema_with_geom(vec![], GeometryType::Geometry, None);
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
fn multipoint_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("mp.csv");
    let schema = schema_with_geom(vec![], GeometryType::Geometry, None);
    let g = Geom::MultiPoint(vec![(0.0, 0.0), (1.0, 1.0)]);
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
fn multipolygon_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("mpg.csv");
    let schema = schema_with_geom(vec![], GeometryType::Geometry, None);
    let g = Geom::MultiPolygon(vec![
        vec![vec![(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 0.0)]],
        vec![vec![(2.0, 2.0), (3.0, 2.0), (3.0, 3.0), (2.0, 2.0)]],
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
fn utf8_attribute_preserved_with_bom() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("utf8.csv");
    let schema = schema_with_geom(
        vec![Field::new("name", DataType::Utf8, true)],
        GeometryType::Geometry,
        None,
    );

    let mut name = StringBuilder::new();
    name.append_value("São Paulo");
    name.append_value("東京");
    let attrs: Vec<ArrayRef> = vec![Arc::new(name.finish())];

    write_geoms(
        &p,
        schema.clone(),
        &[Some(Geom::Point(0.0, 0.0)), Some(Geom::Point(1.0, 1.0))],
        attrs,
        None,
        &default_write_opts(),
    );

    // BOM が先頭に書かれていること（UTF-8 既定）。
    let raw = std::fs::read(&p).unwrap();
    assert_eq!(&raw[..3], &[0xEF, 0xBB, 0xBF]);

    let (_s, _c, batches) = read_back(&p, None);
    let name_arr = batches[0]
        .column_by_name("name")
        .unwrap()
        .as_any()
        .downcast_ref::<arrow_array::StringArray>()
        .unwrap();
    assert_eq!(name_arr.value(0), "São Paulo");
    assert_eq!(name_arr.value(1), "東京");
}

#[test]
fn tsv_extension_uses_tab_delimiter() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("data.tsv");
    let schema = schema_with_geom(
        vec![Field::new("name", DataType::Utf8, true)],
        GeometryType::Geometry,
        None,
    );

    let mut name = StringBuilder::new();
    name.append_value("alpha");
    let attrs: Vec<ArrayRef> = vec![Arc::new(name.finish())];

    write_geoms(
        &p,
        schema.clone(),
        &[Some(Geom::Point(1.0, 2.0))],
        attrs,
        None,
        &default_write_opts(),
    );

    let raw = std::fs::read_to_string(&p).unwrap();
    // header 行に tab が含まれること、 comma は含まれないこと（POINT(1 2) には含まれない）。
    assert!(raw.contains("name\tgeometry"));

    let (_s, _c, batches) = read_back(&p, None);
    let name_arr = batches[0]
        .column_by_name("name")
        .unwrap()
        .as_any()
        .downcast_ref::<arrow_array::StringArray>()
        .unwrap();
    assert_eq!(name_arr.value(0), "alpha");
}

#[test]
fn overwrite_false_fails_when_exists() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("exists.csv");
    let schema = schema_with_geom(vec![], GeometryType::Geometry, None);

    write_geoms(
        &p,
        schema.clone(),
        &[Some(Geom::Point(0.0, 0.0))],
        vec![],
        None,
        &WriteOpts {
            overwrite: true,
            ..Default::default()
        },
    );

    let driver = CsvDriver::new();
    let uri = Uri::from_path(p.to_string_lossy().to_string());
    let r = driver.open_write(&uri, schema, None, &WriteOpts::default());
    assert!(matches!(r, Err(shpx_core::Error::Format(_))));
}

#[test]
fn src_crs_propagates_through_csv() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("crs.csv");
    let schema = schema_with_geom(vec![], GeometryType::Geometry, None);

    write_geoms(
        &p,
        schema.clone(),
        &[Some(Geom::Point(0.0, 0.0))],
        vec![],
        None,
        &default_write_opts(),
    );

    let (_s, crs, _b) = read_back(&p, Some(Crs::from_epsg(4326)));
    assert_eq!(crs.as_ref().and_then(Crs::epsg_code), Some(4326));
}

#[test]
fn wkt_empty_in_csv_is_rejected() {
    // 手動で `geometry` 列に `POINT EMPTY` を含む CSV を作り、reader が拒否することを確認。
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("empty.csv");
    std::fs::write(&p, "name,geometry\nfoo,POINT EMPTY\n").unwrap();

    let driver = CsvDriver::new();
    let uri = Uri::from_path(p.to_string_lossy().to_string());
    let mut r = driver.open_read(&uri, &ReadOpts::default()).unwrap();
    let err = r.batches().next().unwrap().unwrap_err();
    assert!(matches!(err, shpx_core::Error::Geometry(ref m) if m.contains("EMPTY")));
}
