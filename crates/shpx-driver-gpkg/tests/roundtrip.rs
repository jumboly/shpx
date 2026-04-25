//! GeoPackage reader/writer の往復統合テスト。
//!
//! tempdir 上で完結させ、`assert_cmd` は使わない。GPKG 内部のメタテーブルは
//! 別接続で `rusqlite` を直接叩いて検証する。

use std::collections::HashMap;
use std::sync::Arc;

use arrow_array::builder::{BinaryBuilder, BooleanBuilder, Date32Builder};
use arrow_array::{
    cast::AsArray, Array, ArrayRef, BooleanArray, Float64Array, Int64Array, RecordBatch,
    StringArray,
};
use arrow_schema::{DataType, Field, Schema};
use rusqlite::Connection;
use shpx_core::{
    schema::{GeometryMeta, GeometryType, GEOMETRY_META_KEY},
    Crs, Driver, ReadOpts, Uri, WriteOpts,
};
use shpx_driver_gpkg::GpkgDriver;
use shpx_geom::wkb::{self, Geom};

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

    let driver = GpkgDriver::new();
    let uri = Uri::from_path(path.to_string_lossy().to_string());
    let mut w = driver.open_write(&uri, schema, crs, opts).unwrap();
    w.write_batch(&batch).unwrap();
    w.finish().unwrap();
}

fn read_back(
    path: &std::path::Path,
    src_crs: Option<Crs>,
) -> (Arc<Schema>, Option<Crs>, Vec<RecordBatch>) {
    let driver = GpkgDriver::new();
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
    let path = dir.path().join("a.gpkg");

    let schema = schema_with_geom(
        vec![
            Field::new("name", DataType::Utf8, true),
            Field::new("count", DataType::Int64, true),
        ],
        GeometryType::Point,
        Some(Crs::from_epsg(4326)),
    );

    let names: ArrayRef = Arc::new(StringArray::from(vec![Some("alpha"), Some("beta"), None]));
    let counts: ArrayRef = Arc::new(Int64Array::from(vec![Some(1), Some(2), Some(3)]));
    let geoms = [
        Some(Geom::Point(1.0, 2.0)),
        Some(Geom::Point(3.5, -4.5)),
        None,
    ];

    write_geoms(
        &path,
        schema,
        &geoms,
        vec![names, counts],
        Some(Crs::from_epsg(4326)),
        &default_write_opts(),
    );

    let (back_schema, back_crs, batches) = read_back(&path, None);
    assert_eq!(back_crs, Some(Crs::from_epsg(4326)));
    assert_eq!(batches.len(), 1);
    let batch = &batches[0];
    assert_eq!(batch.num_rows(), 3);

    // 列順: name, count, geom
    assert_eq!(back_schema.field(0).name(), "name");
    assert_eq!(back_schema.field(1).name(), "count");
    assert_eq!(back_schema.field(2).name(), "geom");

    let name_col = batch.column(0).as_string::<i32>();
    assert_eq!(name_col.value(0), "alpha");
    assert!(name_col.is_null(2));
    let count_col = batch
        .column(1)
        .as_primitive::<arrow_array::types::Int64Type>();
    assert_eq!(count_col.value(2), 3);

    // geometry 列は WKB なので decode して比較
    let geom_arr = geom_col(batch);
    assert_eq!(
        wkb::decode(geom_arr.value(0)).unwrap(),
        Geom::Point(1.0, 2.0)
    );
    assert_eq!(
        wkb::decode(geom_arr.value(1)).unwrap(),
        Geom::Point(3.5, -4.5)
    );
    assert!(geom_arr.is_null(2));
}

#[test]
fn polygon_with_hole_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("b.gpkg");
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
fn boolean_and_date_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("c.gpkg");
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
    // 2026-04-25: epoch + (2026-04-25 - 1970-01-01).num_days()
    let target = chrono::NaiveDate::from_ymd_opt(2026, 4, 25).unwrap();
    let epoch = chrono::NaiveDate::from_ymd_opt(1970, 1, 1).unwrap();
    let days = i32::try_from(target.signed_duration_since(epoch).num_days()).unwrap();
    db.append_value(days);
    db.append_null();

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
    assert_eq!(back_schema.field(1).data_type(), &DataType::Date32);
    let batch = &batches[0];
    let flag_col = batch.column(0).as_boolean();
    assert!(flag_col.value(0));
    assert!(!flag_col.value(1));
    let date_col = batch
        .column(1)
        .as_primitive::<arrow_array::types::Date32Type>();
    assert_eq!(date_col.value(0), days);
    assert!(date_col.is_null(1));
}

#[test]
fn missing_crs_with_on_loss_error_fails() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("err.gpkg");
    let schema = schema_with_geom(
        vec![Field::new("x", DataType::Int64, true)],
        GeometryType::Point,
        None,
    );
    let ids: ArrayRef = Arc::new(Int64Array::from(vec![1_i64]));
    let mut bb = BinaryBuilder::new();
    bb.append_value(wkb::encode(&Geom::Point(0.0, 0.0)).unwrap());
    let cols: Vec<ArrayRef> = vec![ids, Arc::new(bb.finish())];
    let batch = RecordBatch::try_new(schema.clone(), cols).unwrap();

    let driver = GpkgDriver::new();
    let uri = Uri::from_path(path.to_string_lossy().to_string());
    // on_loss=Error (既定) で CRS なし → register_srs が apply_on_loss で停止する。
    let opts = WriteOpts {
        overwrite: true,
        ..Default::default()
    };
    let err = driver.open_write(&uri, schema, None, &opts);
    assert!(
        err.is_err(),
        "open_write should fail when CRS is missing under OnLoss::Error"
    );
    let _ = batch; // 未使用警告抑制
}

#[test]
fn meta_tables_have_required_rows() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("meta.gpkg");
    let schema = schema_with_geom(
        vec![Field::new("name", DataType::Utf8, true)],
        GeometryType::Point,
        Some(Crs::from_epsg(4326)),
    );
    let names: ArrayRef = Arc::new(StringArray::from(vec![Some("p1")]));
    write_geoms(
        &path,
        schema,
        &[Some(Geom::Point(10.0, 20.0))],
        vec![names],
        Some(Crs::from_epsg(4326)),
        &default_write_opts(),
    );

    // 別接続で生 SQLite を覗く。
    let conn = Connection::open(&path).unwrap();

    let app_id: i32 = conn
        .pragma_query_value(None, "application_id", |row| row.get(0))
        .unwrap();
    assert_eq!(app_id, 0x4750_4b47);

    let user_v: i32 = conn
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(user_v, 10_300);

    // 必須 SRS 行 3 件 (-1, 0, 4326) が存在する。
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM gpkg_spatial_ref_sys WHERE srs_id IN (-1, 0, 4326)",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 3);

    // gpkg_contents に feature レイヤが 1 件、bbox が更新されている。
    let (table, srs_id, min_x, max_x): (String, i32, f64, f64) = conn
        .query_row(
            "SELECT table_name, srs_id, min_x, max_x FROM gpkg_contents WHERE data_type='features'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(srs_id, 4326);
    assert!((min_x - 10.0).abs() < f64::EPSILON);
    assert!((max_x - 10.0).abs() < f64::EPSILON);

    // gpkg_geometry_columns が登録されている。
    let (col, gtype, gsrs): (String, String, i32) = conn
        .query_row(
            "SELECT column_name, geometry_type_name, srs_id FROM gpkg_geometry_columns WHERE table_name=?1",
            [&table],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(col, "geom");
    assert_eq!(gtype, "POINT");
    assert_eq!(gsrs, 4326);
}

#[test]
fn explicit_table_via_query_string() {
    // 1 つの GPKG ファイルに 2 つの feature テーブルを手で作り、
    // ?table=... で曖昧解決を切り抜けられることを検証する。
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("multi.gpkg");
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
        vec![ids],
        Some(Crs::from_epsg(4326)),
        &default_write_opts(),
    );

    // 既存 GPKG に手動で feature テーブルをもう 1 つ作って gpkg_contents に登録する。
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE \"second\" (fid INTEGER PRIMARY KEY AUTOINCREMENT, v INTEGER, geom BLOB);",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO gpkg_contents (table_name, data_type, identifier, srs_id) VALUES (?1, 'features', ?1, 4326)",
            ["second"],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO gpkg_geometry_columns (table_name, column_name, geometry_type_name, srs_id, z, m) VALUES (?1, 'geom', 'POINT', 4326, 0, 0)",
            ["second"],
        )
        .unwrap();
    }

    // テーブル指定なしで開くと曖昧エラー。
    let driver = GpkgDriver::new();
    let uri_ambig = Uri::from_path(path.to_string_lossy().to_string());
    let err = driver.open_read(&uri_ambig, &ReadOpts::default()).err();
    assert!(err.is_some(), "ambiguous table should error");

    // ?table=multi で stem テーブルを開ける。
    let stem = path.file_stem().unwrap().to_str().unwrap();
    let raw = format!("{}?table={}", path.to_string_lossy(), stem);
    let uri_explicit = Uri::from_path(raw);
    let r = driver
        .open_read(&uri_explicit, &ReadOpts::default())
        .unwrap();
    let _schema = r.schema();
}

#[test]
fn overwrite_replaces_existing_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ow.gpkg");
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
    let driver = GpkgDriver::new();
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

#[test]
fn rejects_plain_sqlite_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("plain.sqlite");
    Connection::open(&path).unwrap().close().unwrap();
    let driver = GpkgDriver::new();
    // 拡張子は .gpkg でなくとも、driver を直接呼べばリーダーは通る経路を確認する。
    let uri = Uri {
        scheme: "gpkg".to_string(),
        raw: path.to_string_lossy().to_string(),
    };
    let err = driver.open_read(&uri, &ReadOpts::default()).err().unwrap();
    let msg = format!("{err}");
    assert!(msg.contains("GeoPackage") || msg.contains("application_id"));
}

#[test]
fn float_array_unused_warning_silencer() {
    // arrow_array::Float64Array の import を統合テスト全体で 1 度使うため。
    let arr = Float64Array::from(vec![1.0, 2.0]);
    assert!((arr.value(0) - 1.0).abs() < f64::EPSILON);
    let bools: BooleanArray = vec![Some(true)].into();
    assert!(bools.value(0));
}
