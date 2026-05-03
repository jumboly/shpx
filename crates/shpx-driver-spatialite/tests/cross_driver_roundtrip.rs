//! SpatiaLite ↔ GPKG / SpatiaLite ↔ Shapefile の cross-driver e2e 往復。
//!
//! 対象ドライバ間で属性 + geometry が値レベルで保存されることを確認する。
//! mod_spatialite が必要なため env-gated。`SHPX_TEST_SPATIALITE=1` を立てて実行する。

mod common;

use std::sync::Arc;

use arrow_array::builder::BinaryBuilder;
use arrow_array::{
    cast::AsArray, Array, ArrayRef, BooleanArray, Float64Array, Int64Array, RecordBatch,
    StringArray,
};
use arrow_schema::{DataType, Field, SchemaRef};
use common::{default_write_opts, schema_with_geom, skip_if_not_enabled};
use shpx_core::{
    schema::{GeometryType, GEOMETRY_META_KEY},
    Crs, Driver, ReadOpts, Uri,
};
use shpx_driver_gpkg::GpkgDriver;
use shpx_driver_shp::ShpDriver;
use shpx_driver_spatialite::SpatialiteDriver;
use shpx_geom::wkb::{self, Geom};

fn write_via(
    driver: &dyn Driver,
    path: &std::path::Path,
    schema: SchemaRef,
    batches: &[RecordBatch],
    crs: Option<Crs>,
) {
    let uri = Uri::from_path(path.to_string_lossy().to_string());
    let mut w = driver
        .open_write(&uri, schema, crs, &default_write_opts())
        .unwrap();
    for b in batches {
        w.write_batch(b).unwrap();
    }
    w.finish().unwrap();
}

fn read_all(
    driver: &dyn Driver,
    path: &std::path::Path,
) -> (SchemaRef, Option<Crs>, Vec<RecordBatch>) {
    let uri = Uri::from_path(path.to_string_lossy().to_string());
    let opts = ReadOpts::default();
    let mut r = driver.open_read(&uri, &opts).unwrap();
    let schema = r.schema();
    let crs = r.crs().cloned();
    let batches: Vec<_> = r.batches().collect::<Result<_, _>>().unwrap();
    (schema, crs, batches)
}

/// 列名は driver により異なる (SpatiaLite は `geom`、SHP は `geometry`) ため、
/// geometry meta タグを持つ列を探す。
fn decode_geom_col(batches: &[RecordBatch]) -> Vec<Option<Geom>> {
    let mut out = Vec::new();
    for b in batches {
        let idx = b
            .schema()
            .fields()
            .iter()
            .position(|f| f.metadata().contains_key(GEOMETRY_META_KEY))
            .expect("geometry meta column not found");
        let arr = b.column(idx).as_binary::<i32>();
        for i in 0..arr.len() {
            if arr.is_null(i) {
                out.push(None);
            } else {
                out.push(Some(wkb::decode(arr.value(i)).unwrap()));
            }
        }
    }
    out
}

fn collect_string_col(batches: &[RecordBatch], col: &str) -> Vec<Option<String>> {
    let mut out = Vec::new();
    for b in batches {
        let idx = b.schema().index_of(col).unwrap();
        let a = b.column(idx).as_string::<i32>();
        for i in 0..a.len() {
            out.push(if a.is_null(i) {
                None
            } else {
                Some(a.value(i).to_string())
            });
        }
    }
    out
}

fn collect_i64_col(batches: &[RecordBatch], col: &str) -> Vec<Option<i64>> {
    let mut out = Vec::new();
    for b in batches {
        let idx = b.schema().index_of(col).unwrap();
        let a = b
            .column(idx)
            .as_primitive::<arrow_array::types::Int64Type>();
        for i in 0..a.len() {
            out.push(if a.is_null(i) { None } else { Some(a.value(i)) });
        }
    }
    out
}

fn collect_f64_col(batches: &[RecordBatch], col: &str) -> Vec<Option<f64>> {
    let mut out = Vec::new();
    for b in batches {
        let idx = b.schema().index_of(col).unwrap();
        let a = b
            .column(idx)
            .as_primitive::<arrow_array::types::Float64Type>();
        for i in 0..a.len() {
            out.push(if a.is_null(i) { None } else { Some(a.value(i)) });
        }
    }
    out
}

fn collect_bool_col(batches: &[RecordBatch], col: &str) -> Vec<Option<bool>> {
    let mut out = Vec::new();
    for b in batches {
        let idx = b.schema().index_of(col).unwrap();
        let a = b.column(idx).as_boolean();
        for i in 0..a.len() {
            out.push(if a.is_null(i) { None } else { Some(a.value(i)) });
        }
    }
    out
}

/// SpatiaLite ↔ GPKG (GPKG が SpatiaLite blob と互換ではないため、属性 + WKB 経由)
/// の双方向往復: SpatiaLite に書く → GPKG にコピー → SpatiaLite に書き戻す。
/// 値が完全に一致することを確認。
#[test]
fn spatialite_gpkg_roundtrip() {
    if skip_if_not_enabled() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let sl1 = dir.path().join("a.sqlite");
    let gpkg = dir.path().join("b.gpkg");
    let sl2 = dir.path().join("c.sqlite");

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

    let names: ArrayRef = Arc::new(StringArray::from(vec![Some("Kyoto"), Some("Tokyo"), None]));
    let counts: ArrayRef = Arc::new(Int64Array::from(vec![Some(1), Some(2), Some(3)]));
    let ratios: ArrayRef = Arc::new(Float64Array::from(vec![Some(0.5), None, Some(0.25)]));
    let flags: ArrayRef = Arc::new(BooleanArray::from(vec![Some(true), Some(false), None]));
    let geoms = vec![
        Some(Geom::Point(135.0, 35.0)),
        Some(Geom::Point(139.7, 35.7)),
        None,
    ];

    let mut gb = BinaryBuilder::new();
    for g in &geoms {
        match g {
            Some(g) => gb.append_value(wkb::encode(g).unwrap()),
            None => gb.append_null(),
        }
    }
    let cols: Vec<ArrayRef> = vec![names, counts, ratios, flags, Arc::new(gb.finish())];
    let original = RecordBatch::try_new(schema.clone(), cols).unwrap();

    let crs = Some(Crs::from_epsg(4326));
    let sl = SpatialiteDriver::new();
    let gp = GpkgDriver::new();

    // 1) 元データを SpatiaLite に書く
    write_via(
        &sl,
        &sl1,
        schema.clone(),
        std::slice::from_ref(&original),
        crs.clone(),
    );
    let (sch_a, crs_a, batches_a) = read_all(&sl, &sl1);
    assert_eq!(crs_a.and_then(|c| c.epsg_code()), Some(4326));

    // 2) SpatiaLite から読んだ batches を GPKG に流す
    write_via(&gp, &gpkg, sch_a, &batches_a, crs.clone());
    let (sch_b, crs_b, batches_b) = read_all(&gp, &gpkg);
    assert_eq!(crs_b.and_then(|c| c.epsg_code()), Some(4326));

    // 3) GPKG から読んだ batches を再び SpatiaLite に流す
    write_via(&sl, &sl2, sch_b, &batches_b, crs.clone());
    let (_sch_c, crs_c, batches_c) = read_all(&sl, &sl2);
    assert_eq!(crs_c.and_then(|c| c.epsg_code()), Some(4326));

    // 値レベル比較: 元データ ↔ 最終 batches_c
    assert_eq!(
        collect_string_col(&batches_c, "name"),
        vec![Some("Kyoto".to_string()), Some("Tokyo".to_string()), None]
    );
    assert_eq!(
        collect_i64_col(&batches_c, "count"),
        vec![Some(1), Some(2), Some(3)]
    );
    assert_eq!(
        collect_f64_col(&batches_c, "ratio"),
        vec![Some(0.5), None, Some(0.25)]
    );
    assert_eq!(
        collect_bool_col(&batches_c, "flag"),
        vec![Some(true), Some(false), None]
    );
    assert_eq!(decode_geom_col(&batches_c), geoms);
}

/// SpatiaLite ↔ Shapefile の双方向往復。
/// SHP の DBF は Numeric 列を読み戻すと一律 Float64 になるため、整数型の bit-identical 比較は
/// 別ドライバの roundtrip で担保する。本テストは SpatiaLite と SHP が cross-driver で
/// 機能することを Utf8 / Boolean / Float64 / geometry で確認する。
/// SHP は 1 ファイルあたり 1 geometry type のため Point に限定。
#[test]
fn spatialite_shapefile_roundtrip() {
    if skip_if_not_enabled() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let sl1 = dir.path().join("a.sqlite");
    let shp = dir.path().join("b.shp");
    let sl2 = dir.path().join("c.sqlite");

    let schema = schema_with_geom(
        vec![
            Field::new("name", DataType::Utf8, true),
            Field::new("ratio", DataType::Float64, true),
            Field::new("flag", DataType::Boolean, true),
        ],
        GeometryType::Point,
        Some(Crs::from_epsg(4326)),
    );

    let names: ArrayRef = Arc::new(StringArray::from(vec![Some("Kyoto"), Some("Tokyo")]));
    let ratios: ArrayRef = Arc::new(Float64Array::from(vec![Some(0.5), Some(0.25)]));
    let flags: ArrayRef = Arc::new(BooleanArray::from(vec![Some(true), Some(false)]));
    let geoms = vec![
        Some(Geom::Point(135.0, 35.0)),
        Some(Geom::Point(139.7, 35.7)),
    ];

    let mut gb = BinaryBuilder::new();
    for g in &geoms {
        if let Some(g) = g {
            gb.append_value(wkb::encode(g).unwrap());
        } else {
            gb.append_null();
        }
    }
    let cols: Vec<ArrayRef> = vec![names, ratios, flags, Arc::new(gb.finish())];
    let original = RecordBatch::try_new(schema.clone(), cols).unwrap();

    let crs = Some(Crs::from_epsg(4326));
    let sl = SpatialiteDriver::new();
    let sp = ShpDriver::new();

    write_via(
        &sl,
        &sl1,
        schema.clone(),
        std::slice::from_ref(&original),
        crs.clone(),
    );
    let (sch_a, _crs_a, batches_a) = read_all(&sl, &sl1);

    write_via(&sp, &shp, sch_a, &batches_a, crs.clone());
    let (sch_b, _crs_b, batches_b) = read_all(&sp, &shp);

    write_via(&sl, &sl2, sch_b, &batches_b, crs.clone());
    let (_sch_c, _crs_c, batches_c) = read_all(&sl, &sl2);

    assert_eq!(
        collect_string_col(&batches_c, "name"),
        vec![Some("Kyoto".to_string()), Some("Tokyo".to_string())]
    );
    assert_eq!(
        collect_f64_col(&batches_c, "ratio"),
        vec![Some(0.5), Some(0.25)]
    );
    assert_eq!(
        collect_bool_col(&batches_c, "flag"),
        vec![Some(true), Some(false)]
    );
    assert_eq!(decode_geom_col(&batches_c), geoms);
}

/// LineString / Polygon の geometry も SpatiaLite ↔ GPKG で往復する。
#[test]
fn spatialite_gpkg_geometry_types() {
    if skip_if_not_enabled() {
        return;
    }
    let cases: Vec<(GeometryType, Geom)> = vec![
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
    let sl = SpatialiteDriver::new();
    let gp = GpkgDriver::new();

    for (i, (gt, g)) in cases.iter().enumerate() {
        let sl1 = dir.path().join(format!("a_{i}.sqlite"));
        let gpkg = dir.path().join(format!("b_{i}.gpkg"));

        let schema = schema_with_geom(vec![], *gt, Some(Crs::from_epsg(4326)));
        let mut bb = BinaryBuilder::new();
        bb.append_value(wkb::encode(g).unwrap());
        let cols: Vec<ArrayRef> = vec![Arc::new(bb.finish())];
        let original = RecordBatch::try_new(schema.clone(), cols).unwrap();

        write_via(
            &sl,
            &sl1,
            schema.clone(),
            &[original],
            Some(Crs::from_epsg(4326)),
        );
        let (sch_a, _, batches_a) = read_all(&sl, &sl1);

        write_via(&gp, &gpkg, sch_a, &batches_a, Some(Crs::from_epsg(4326)));
        let (_, _, batches_b) = read_all(&gp, &gpkg);

        assert_eq!(decode_geom_col(&batches_b), vec![Some(g.clone())]);
    }
}
