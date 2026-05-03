//! SpatiaLite reader/writer の往復統合テスト。
//!
//! mod_spatialite が必要なため env-gated。
//! `SHPX_TEST_SPATIALITE=1` (+ 必要なら `SHPX_SPATIALITE_PATH` で .so/.dylib のパス) を
//! セットして実行する。env 未設定時は skip。

use std::collections::HashMap;
use std::sync::Arc;

use arrow_array::builder::BinaryBuilder;
use arrow_array::{
    cast::AsArray, Array, ArrayRef, BooleanArray, Float64Array, Int64Array, RecordBatch,
    StringArray,
};
use arrow_schema::{DataType, Field, Schema};
use shpx_core::{
    schema::{GeometryMeta, GeometryType, GEOMETRY_META_KEY},
    Crs, Driver, ReadOpts, Uri, WriteOpts,
};
use shpx_driver_spatialite::SpatialiteDriver;
use shpx_geom::wkb::{self, Geom};

/// `SHPX_TEST_SPATIALITE` env が未設定なら eprintln + return で skip。
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

    let driver = SpatialiteDriver::new();
    let uri = Uri::from_path(path.to_string_lossy().to_string());
    let mut w = driver.open_write(&uri, schema, crs, opts).unwrap();
    w.write_batch(&batch).unwrap();
    w.finish().unwrap();
}

fn read_back(
    path: &std::path::Path,
    src_crs: Option<Crs>,
) -> (Arc<Schema>, Option<Crs>, Vec<RecordBatch>) {
    let driver = SpatialiteDriver::new();
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

#[test]
fn point_with_attrs_roundtrip() {
    if skip_if_not_enabled() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.sqlite");

    let schema = schema_with_geom(
        vec![
            Field::new("name", DataType::Utf8, true),
            Field::new("count", DataType::Int64, true),
            Field::new("ratio", DataType::Float64, true),
            Field::new("flag", DataType::Boolean, true),
        ],
        GeometryType::Point,
        Some(Crs::from_epsg(4326)),
    );

    let geoms = vec![
        Some(Geom::Point(135.0, 35.0)),
        Some(Geom::Point(139.7, 35.7)),
        None,
    ];
    let attrs: Vec<ArrayRef> = vec![
        Arc::new(StringArray::from(vec![Some("Kyoto"), Some("Tokyo"), None])) as _,
        Arc::new(Int64Array::from(vec![Some(1), Some(2), Some(3)])) as _,
        Arc::new(Float64Array::from(vec![Some(0.5), None, Some(0.25)])) as _,
        Arc::new(BooleanArray::from(vec![Some(true), Some(false), None])) as _,
    ];

    write_geoms(
        &path,
        schema.clone(),
        &geoms,
        attrs,
        Some(Crs::from_epsg(4326)),
        &default_write_opts(),
    );

    let (back_schema, back_crs, batches) = read_back(&path, None);
    assert_eq!(back_crs.and_then(|c| c.epsg_code()), Some(4326));
    assert_eq!(batches.iter().map(RecordBatch::num_rows).sum::<usize>(), 3);
    // 列順は属性 (name, count, ratio, flag) + geom。
    let f_names: Vec<&str> = back_schema
        .fields()
        .iter()
        .map(|f| f.name().as_str())
        .collect();
    assert_eq!(f_names, vec!["name", "count", "ratio", "flag", "geom"]);

    // 値レベルの確認 (1 batch)。
    let b = &batches[0];
    let names = b.column(0).as_string::<i32>();
    assert_eq!(names.value(0), "Kyoto");
    assert!(names.is_null(2));

    let counts = b.column(1).as_primitive::<arrow_array::types::Int64Type>();
    assert_eq!(counts.value(0), 1);
    assert_eq!(counts.value(1), 2);

    let geom_arr = b.column(4).as_binary::<i32>();
    assert!(!geom_arr.is_null(0));
    assert!(geom_arr.is_null(2));
    let g0 = wkb::decode(geom_arr.value(0)).unwrap();
    assert_eq!(g0, Geom::Point(135.0, 35.0));
}

#[test]
fn each_geometry_type_roundtrips() {
    if skip_if_not_enabled() {
        return;
    }
    let cases: Vec<(GeometryType, Geom)> = vec![
        (GeometryType::Point, Geom::Point(1.0, 2.0)),
        (
            GeometryType::LineString,
            Geom::LineString(vec![(0.0, 0.0), (1.0, 1.0), (2.0, 0.5)]),
        ),
        (
            GeometryType::Polygon,
            Geom::Polygon(vec![vec![
                (0.0, 0.0),
                (4.0, 0.0),
                (4.0, 4.0),
                (0.0, 4.0),
                (0.0, 0.0),
            ]]),
        ),
        (
            GeometryType::MultiPoint,
            Geom::MultiPoint(vec![(0.0, 0.0), (1.0, 1.0)]),
        ),
        (
            GeometryType::MultiLineString,
            Geom::MultiLineString(vec![
                vec![(0.0, 0.0), (1.0, 1.0)],
                vec![(2.0, 2.0), (3.0, 3.0)],
            ]),
        ),
        (
            GeometryType::MultiPolygon,
            Geom::MultiPolygon(vec![vec![vec![
                (0.0, 0.0),
                (1.0, 0.0),
                (1.0, 1.0),
                (0.0, 0.0),
            ]]]),
        ),
    ];

    let dir = tempfile::tempdir().unwrap();
    for (i, (gt, g)) in cases.iter().enumerate() {
        let path = dir.path().join(format!("g_{i}.sqlite"));
        let schema = schema_with_geom(vec![], *gt, Some(Crs::from_epsg(4326)));
        write_geoms(
            &path,
            schema,
            &[Some(g.clone())],
            vec![],
            Some(Crs::from_epsg(4326)),
            &default_write_opts(),
        );

        let (_back_schema, _back_crs, batches) = read_back(&path, None);
        assert_eq!(batches.len(), 1);
        let geom_arr = batches[0].column(0).as_binary::<i32>();
        let back = wkb::decode(geom_arr.value(0)).unwrap();
        assert_eq!(&back, g, "geometry {gt:?} did not roundtrip");
    }
}
