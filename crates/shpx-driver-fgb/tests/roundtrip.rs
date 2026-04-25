//! FlatGeobuf reader/writer の往復統合テスト。
//!
//! tempdir 上で完結。fixture はテスト内で生成する（リポジトリ内に .fgb サンプルは置かない）。

use std::collections::HashMap;
use std::sync::Arc;

use arrow_array::builder::{BinaryBuilder, BooleanBuilder, Date32Builder};
use arrow_array::{cast::AsArray, ArrayRef, Float64Array, Int64Array, RecordBatch, StringArray};
use arrow_schema::{DataType, Field, Schema};
use shpx_core::{
    schema::{GeometryMeta, GeometryType, GEOMETRY_META_KEY},
    Crs, Driver, ReadOpts, Uri, WriteOpts,
};
use shpx_driver_fgb::FgbDriver;
use shpx_geom::wkb::{self, Geom};

fn schema_with_geom(extra: Vec<Field>, gt: GeometryType, crs: Option<Crs>) -> Arc<Schema> {
    let mut fields = extra;
    let meta = GeometryMeta::wkb(gt, crs);
    let mut field = Field::new("geometry", DataType::Binary, true);
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

    let driver = FgbDriver::new();
    let uri = Uri::from_path(path.to_string_lossy().to_string());
    let mut w = driver.open_write(&uri, schema, crs, opts).unwrap();
    w.write_batch(&batch).unwrap();
    w.finish().unwrap();
}

fn read_back(
    path: &std::path::Path,
    src_crs: Option<Crs>,
) -> (Arc<Schema>, Option<Crs>, Vec<RecordBatch>) {
    let driver = FgbDriver::new();
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
    let idx = batch.num_columns() - 1;
    batch.column(idx).as_binary::<i32>()
}

#[test]
fn point_with_attributes_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.fgb");

    let schema = schema_with_geom(
        vec![
            Field::new("name", DataType::Utf8, true),
            Field::new("count", DataType::Int64, true),
            Field::new("ratio", DataType::Float64, true),
        ],
        GeometryType::Point,
        Some(Crs::from_epsg(4326)),
    );

    let names: ArrayRef = Arc::new(StringArray::from(vec![
        Some("alpha"),
        Some("beta"),
        Some("gamma"),
    ]));
    let counts: ArrayRef = Arc::new(Int64Array::from(vec![Some(1), Some(2), Some(3)]));
    let ratios: ArrayRef = Arc::new(Float64Array::from(vec![Some(0.5), Some(1.5), Some(2.5)]));
    let geoms = [
        Some(Geom::Point(1.0, 2.0)),
        Some(Geom::Point(3.5, -4.5)),
        Some(Geom::Point(7.0, 8.0)),
    ];

    write_geoms(
        &path,
        schema,
        &geoms,
        vec![names, counts, ratios],
        Some(Crs::from_epsg(4326)),
        &default_write_opts(),
    );

    let (back_schema, back_crs, batches) = read_back(&path, None);
    assert_eq!(back_crs, Some(Crs::from_epsg(4326)));
    assert_eq!(batches.len(), 1);
    let batch = &batches[0];
    assert_eq!(batch.num_rows(), 3);

    // 列順: name, count, ratio, geometry
    assert_eq!(back_schema.field(0).name(), "name");
    assert_eq!(back_schema.field(1).name(), "count");
    assert_eq!(back_schema.field(2).name(), "ratio");
    assert_eq!(back_schema.field(3).name(), "geometry");

    let name_col = batch.column(0).as_string::<i32>();
    assert_eq!(name_col.value(0), "alpha");
    assert_eq!(name_col.value(2), "gamma");
    let count_col = batch
        .column(1)
        .as_primitive::<arrow_array::types::Int64Type>();
    assert_eq!(count_col.value(2), 3);
    let ratio_col = batch
        .column(2)
        .as_primitive::<arrow_array::types::Float64Type>();
    assert!((ratio_col.value(1) - 1.5).abs() < f64::EPSILON);

    // geometry 列は WKB なので decode して比較。
    let geom_arr = geom_col(batch);
    assert_eq!(
        wkb::decode(geom_arr.value(0)).unwrap(),
        Geom::Point(1.0, 2.0)
    );
    assert_eq!(
        wkb::decode(geom_arr.value(1)).unwrap(),
        Geom::Point(3.5, -4.5)
    );
}

#[test]
fn polygon_with_hole_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("b.fgb");
    let schema = schema_with_geom(
        vec![Field::new("id", DataType::Int64, true)],
        GeometryType::Polygon,
        Some(Crs::from_epsg(3857)),
    );
    let ids: ArrayRef = Arc::new(Int64Array::from(vec![1_i64]));
    let poly = Geom::Polygon(vec![
        vec![
            (0.0, 0.0),
            (10.0, 0.0),
            (10.0, 10.0),
            (0.0, 10.0),
            (0.0, 0.0),
        ],
        vec![(2.0, 2.0), (3.0, 2.0), (3.0, 3.0), (2.0, 3.0), (2.0, 2.0)],
    ]);
    write_geoms(
        &path,
        schema,
        &[Some(poly.clone())],
        vec![ids],
        Some(Crs::from_epsg(3857)),
        &default_write_opts(),
    );

    let (_, crs, batches) = read_back(&path, None);
    assert_eq!(crs, Some(Crs::from_epsg(3857)));
    assert_eq!(batches.len(), 1);
    let geom_arr = geom_col(&batches[0]);
    assert_eq!(wkb::decode(geom_arr.value(0)).unwrap(), poly);
}

#[test]
fn linestring_and_multipolygon_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let path_ls = dir.path().join("ls.fgb");
    let schema_ls = schema_with_geom(
        vec![Field::new("name", DataType::Utf8, true)],
        GeometryType::LineString,
        Some(Crs::from_epsg(4326)),
    );
    let names: ArrayRef = Arc::new(StringArray::from(vec![Some("road")]));
    let ls = Geom::LineString(vec![(0.0, 0.0), (1.0, 1.0), (2.0, 0.0)]);
    write_geoms(
        &path_ls,
        schema_ls,
        &[Some(ls.clone())],
        vec![names],
        Some(Crs::from_epsg(4326)),
        &default_write_opts(),
    );
    let (_, _, batches) = read_back(&path_ls, None);
    assert_eq!(wkb::decode(geom_col(&batches[0]).value(0)).unwrap(), ls);

    let path_mp = dir.path().join("mp.fgb");
    let schema_mp = schema_with_geom(
        vec![Field::new("id", DataType::Int64, true)],
        GeometryType::MultiPolygon,
        Some(Crs::from_epsg(4326)),
    );
    let ids: ArrayRef = Arc::new(Int64Array::from(vec![42_i64]));
    let mp = Geom::MultiPolygon(vec![
        vec![vec![(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 0.0)]],
        vec![vec![
            (2.0, 2.0),
            (3.0, 2.0),
            (3.0, 3.0),
            (2.0, 3.0),
            (2.0, 2.0),
        ]],
    ]);
    write_geoms(
        &path_mp,
        schema_mp,
        &[Some(mp.clone())],
        vec![ids],
        Some(Crs::from_epsg(4326)),
        &default_write_opts(),
    );
    let (_, _, batches) = read_back(&path_mp, None);
    assert_eq!(wkb::decode(geom_col(&batches[0]).value(0)).unwrap(), mp);
}

#[test]
fn boolean_and_date_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("c.fgb");
    let schema = schema_with_geom(
        vec![
            Field::new("flag", DataType::Boolean, true),
            Field::new("when", DataType::Date32, true),
        ],
        GeometryType::Point,
        Some(Crs::from_epsg(4326)),
    );
    let mut bb = BooleanBuilder::new();
    bb.append_value(true);
    bb.append_value(false);
    let mut db = Date32Builder::new();
    let target = chrono::NaiveDate::from_ymd_opt(2026, 4, 25).unwrap();
    let epoch = chrono::NaiveDate::from_ymd_opt(1970, 1, 1).unwrap();
    let days = i32::try_from(target.signed_duration_since(epoch).num_days()).unwrap();
    db.append_value(days);
    db.append_value(days + 1);

    let attrs: Vec<ArrayRef> = vec![Arc::new(bb.finish()), Arc::new(db.finish())];
    write_geoms(
        &path,
        schema,
        &[Some(Geom::Point(0.0, 0.0)), Some(Geom::Point(1.0, 1.0))],
        attrs,
        Some(Crs::from_epsg(4326)),
        &default_write_opts(),
    );

    let (back_schema, _, batches) = read_back(&path, None);
    assert_eq!(back_schema.field(0).data_type(), &DataType::Boolean);
    // ISO 日付は時刻部を持たないため reader が Date32 に絞り込む。
    assert_eq!(back_schema.field(1).data_type(), &DataType::Date32);
    let batch = &batches[0];
    let flag_col = batch.column(0).as_boolean();
    assert!(flag_col.value(0));
    assert!(!flag_col.value(1));
    let date_col = batch
        .column(1)
        .as_primitive::<arrow_array::types::Date32Type>();
    assert_eq!(date_col.value(0), days);
    assert_eq!(date_col.value(1), days + 1);
}

#[test]
fn missing_crs_with_on_loss_error_fails() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("err.fgb");
    let schema = schema_with_geom(
        vec![Field::new("x", DataType::Int64, true)],
        GeometryType::Point,
        None,
    );
    let driver = FgbDriver::new();
    let uri = Uri::from_path(path.to_string_lossy().to_string());
    let opts = WriteOpts {
        overwrite: true,
        ..Default::default()
    };
    let err = driver.open_write(&uri, schema, None, &opts);
    assert!(
        err.is_err(),
        "open_write should fail when CRS is missing under OnLoss::Error"
    );
}

#[test]
fn overwrite_replaces_existing_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ow.fgb");
    let schema = schema_with_geom(
        vec![Field::new("v", DataType::Int64, true)],
        GeometryType::Point,
        Some(Crs::from_epsg(4326)),
    );
    let ids: ArrayRef = Arc::new(Int64Array::from(vec![1_i64]));
    write_geoms(
        &path,
        schema.clone(),
        &[Some(Geom::Point(0.0, 0.0))],
        vec![ids.clone()],
        Some(Crs::from_epsg(4326)),
        &default_write_opts(),
    );

    // overwrite=false で 2 度目を試みると失敗する。
    let driver = FgbDriver::new();
    let uri = Uri::from_path(path.to_string_lossy().to_string());
    let no_ow = WriteOpts {
        overwrite: false,
        ..Default::default()
    };
    let err = driver.open_write(&uri, schema.clone(), Some(Crs::from_epsg(4326)), &no_ow);
    assert!(err.is_err());

    // overwrite=true で再書き込みは成功する。
    write_geoms(
        &path,
        schema,
        &[Some(Geom::Point(2.0, 3.0))],
        vec![ids],
        Some(Crs::from_epsg(4326)),
        &default_write_opts(),
    );
    let (_, _, batches) = read_back(&path, None);
    let g = wkb::decode(geom_col(&batches[0]).value(0)).unwrap();
    assert_eq!(g, Geom::Point(2.0, 3.0));
}
