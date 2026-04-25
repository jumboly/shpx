//! Shapefile reader/writer の統合テスト。
//!
//! fixture をリポジトリに置かず、tempdir 上で writer→reader 往復のみで検証する。
//! テストランナは `cargo test -p shpx-driver-shp --test roundtrip`。

use std::sync::Arc;

use arrow_array::{
    builder::{BinaryBuilder, BooleanBuilder, Date32Builder, Decimal128Builder, StringBuilder},
    ArrayRef, RecordBatch,
};
use arrow_schema::{DataType, Field, Schema};
use shpx_core::{
    schema::{GeometryMeta, GeometryType, GEOMETRY_META_KEY},
    Crs, Driver, OnLoss, ReadOpts, Uri, WriteOpts,
};
use shpx_driver_shp::ShpDriver;
use shpx_geom::wkb::{self, Geom};

fn out_paths(dir: &std::path::Path, stem: &str) -> std::path::PathBuf {
    dir.join(format!("{stem}.shp"))
}

fn schema_with_geom(fields: Vec<Field>, geom_type: GeometryType, crs: Option<Crs>) -> Arc<Schema> {
    let mut all = fields;
    let meta = GeometryMeta::wkb(geom_type, crs);
    let mut field = Field::new("geometry", DataType::Binary, true);
    let mut m = std::collections::HashMap::new();
    m.insert(GEOMETRY_META_KEY.to_string(), meta.to_json().unwrap());
    field.set_metadata(m);
    all.push(field);
    Arc::new(Schema::new(all))
}

fn write_geoms(
    path: &std::path::Path,
    schema: Arc<Schema>,
    geoms: &[Option<Geom>],
    attr_columns: Vec<ArrayRef>,
    crs: Option<Crs>,
) {
    let mut bb = BinaryBuilder::new();
    for g in geoms {
        match g {
            Some(g) => {
                let bytes = wkb::encode(g).unwrap();
                bb.append_value(&bytes);
            }
            None => bb.append_null(),
        }
    }
    let mut cols = attr_columns;
    cols.push(Arc::new(bb.finish()) as ArrayRef);
    let batch = RecordBatch::try_new(schema.clone(), cols).unwrap();

    let driver = ShpDriver::new();
    let uri = Uri::from_path(path.to_string_lossy().to_string());
    let opts = WriteOpts {
        overwrite: true,
        ..Default::default()
    };
    let mut w = driver.open_write(&uri, schema, crs, &opts).unwrap();
    w.write_batch(&batch).unwrap();
    w.finish().unwrap();
}

fn read_back(path: &std::path::Path) -> (Arc<Schema>, Option<Crs>, Vec<RecordBatch>) {
    let driver = ShpDriver::new();
    let uri = Uri::from_path(path.to_string_lossy().to_string());
    let opts = ReadOpts::default();
    let mut r = driver.open_read(&uri, &opts).unwrap();
    let schema = r.schema();
    let crs = r.crs().cloned();
    let batches: Vec<_> = r.batches().collect::<Result<_, _>>().unwrap();
    (schema, crs, batches)
}

#[test]
fn point_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let p = out_paths(dir.path(), "point");
    let schema = schema_with_geom(vec![], GeometryType::Point, None);
    write_geoms(
        &p,
        schema.clone(),
        &[Some(Geom::Point(1.0, 2.0)), Some(Geom::Point(-3.5, 4.25))],
        vec![],
        None,
    );

    let (_s, _c, batches) = read_back(&p);
    assert_eq!(batches.len(), 1);
    assert_eq!(batches[0].num_rows(), 2);
    let geom = batches[0]
        .column_by_name("geometry")
        .unwrap()
        .as_any()
        .downcast_ref::<arrow_array::BinaryArray>()
        .unwrap();
    assert_eq!(wkb::decode(geom.value(0)).unwrap(), Geom::Point(1.0, 2.0));
    assert_eq!(wkb::decode(geom.value(1)).unwrap(), Geom::Point(-3.5, 4.25));
}

#[test]
fn linestring_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let p = out_paths(dir.path(), "ls");
    let schema = schema_with_geom(vec![], GeometryType::LineString, None);
    let g = Geom::LineString(vec![(0.0, 0.0), (1.0, 1.0), (2.0, 0.5)]);
    write_geoms(&p, schema.clone(), &[Some(g.clone())], vec![], None);
    let (_s, _c, batches) = read_back(&p);
    let geom = batches[0]
        .column_by_name("geometry")
        .unwrap()
        .as_any()
        .downcast_ref::<arrow_array::BinaryArray>()
        .unwrap();
    assert_eq!(wkb::decode(geom.value(0)).unwrap(), g);
}

#[test]
fn polygon_with_hole_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let p = out_paths(dir.path(), "poly");
    let schema = schema_with_geom(vec![], GeometryType::Polygon, None);
    let g = Geom::Polygon(vec![
        vec![(0.0, 0.0), (4.0, 0.0), (4.0, 4.0), (0.0, 4.0), (0.0, 0.0)],
        vec![(1.0, 1.0), (1.0, 2.0), (2.0, 2.0), (2.0, 1.0), (1.0, 1.0)],
    ]);
    write_geoms(&p, schema.clone(), &[Some(g.clone())], vec![], None);
    let (_s, _c, batches) = read_back(&p);
    let geom = batches[0]
        .column_by_name("geometry")
        .unwrap()
        .as_any()
        .downcast_ref::<arrow_array::BinaryArray>()
        .unwrap();
    let back = wkb::decode(geom.value(0)).unwrap();
    let Geom::Polygon(rings) = back else {
        panic!("expected polygon")
    };
    assert_eq!(rings.len(), 2);
    assert_eq!(rings[0].len(), 5);
    assert_eq!(rings[1].len(), 5);
}

#[test]
fn multipoint_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let p = out_paths(dir.path(), "mp");
    let schema = schema_with_geom(vec![], GeometryType::MultiPoint, None);
    let g = Geom::MultiPoint(vec![(0.0, 0.0), (1.0, 1.0), (2.0, -2.0)]);
    write_geoms(&p, schema.clone(), &[Some(g.clone())], vec![], None);
    let (_s, _c, batches) = read_back(&p);
    let geom = batches[0]
        .column_by_name("geometry")
        .unwrap()
        .as_any()
        .downcast_ref::<arrow_array::BinaryArray>()
        .unwrap();
    assert_eq!(wkb::decode(geom.value(0)).unwrap(), g);
}

#[test]
fn multilinestring_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let p = out_paths(dir.path(), "mls");
    let schema = schema_with_geom(vec![], GeometryType::MultiLineString, None);
    let g = Geom::MultiLineString(vec![
        vec![(0.0, 0.0), (1.0, 1.0)],
        vec![(2.0, 2.0), (3.0, 3.0), (4.0, 4.0)],
    ]);
    write_geoms(&p, schema.clone(), &[Some(g.clone())], vec![], None);
    let (_s, _c, batches) = read_back(&p);
    let geom = batches[0]
        .column_by_name("geometry")
        .unwrap()
        .as_any()
        .downcast_ref::<arrow_array::BinaryArray>()
        .unwrap();
    assert_eq!(wkb::decode(geom.value(0)).unwrap(), g);
}

#[test]
fn date32_preservation() {
    let dir = tempfile::tempdir().unwrap();
    let p = out_paths(dir.path(), "date");
    let date_field = Field::new("d", DataType::Date32, true);
    let schema = schema_with_geom(vec![date_field], GeometryType::Point, None);

    let mut db = Date32Builder::new();
    // 1900-12-31, 1970-01-01, 2026-04-25 を端境としてカバー。
    db.append_value(days_since_epoch(1900, 12, 31));
    db.append_value(0);
    db.append_value(days_since_epoch(2026, 4, 25));
    let date_array: ArrayRef = Arc::new(db.finish());

    write_geoms(
        &p,
        schema.clone(),
        &[
            Some(Geom::Point(0.0, 0.0)),
            Some(Geom::Point(1.0, 1.0)),
            Some(Geom::Point(2.0, 2.0)),
        ],
        vec![date_array],
        None,
    );
    let (_s, _c, batches) = read_back(&p);
    let arr = batches[0]
        .column_by_name("d")
        .unwrap()
        .as_any()
        .downcast_ref::<arrow_array::Date32Array>()
        .unwrap();
    assert_eq!(arr.value(0), days_since_epoch(1900, 12, 31));
    assert_eq!(arr.value(1), 0);
    assert_eq!(arr.value(2), days_since_epoch(2026, 4, 25));
}

fn days_since_epoch(y: i32, m: u32, d: u32) -> i32 {
    use chrono::NaiveDate;
    let target = NaiveDate::from_ymd_opt(y, m, d).unwrap();
    let epoch = NaiveDate::from_ymd_opt(1970, 1, 1).unwrap();
    i32::try_from(target.signed_duration_since(epoch).num_days()).unwrap()
}

#[test]
fn decimal_preservation() {
    let dir = tempfile::tempdir().unwrap();
    let p = out_paths(dir.path(), "dec");
    let dec_field = Field::new("amt", DataType::Decimal128(10, 3), true);
    let schema = schema_with_geom(vec![dec_field], GeometryType::Point, None);

    let mut b = Decimal128Builder::new()
        .with_precision_and_scale(10, 3)
        .unwrap();
    // 12.345, -6.789, 0.000
    b.append_value(12_345);
    b.append_value(-6_789);
    b.append_value(0);
    let arr: ArrayRef = Arc::new(b.finish());

    write_geoms(
        &p,
        schema.clone(),
        &[
            Some(Geom::Point(0.0, 0.0)),
            Some(Geom::Point(1.0, 1.0)),
            Some(Geom::Point(2.0, 2.0)),
        ],
        vec![arr],
        None,
    );

    let (_s, _c, batches) = read_back(&p);
    // dbase 0.5 では Numeric は Float64 として読み出す（dbf_schema の規約）。
    let arr = batches[0]
        .column_by_name("amt")
        .unwrap()
        .as_any()
        .downcast_ref::<arrow_array::Float64Array>()
        .unwrap();
    assert!((arr.value(0) - 12.345).abs() < 1e-9);
    assert!((arr.value(1) - -6.789).abs() < 1e-9);
    assert!((arr.value(2) - 0.0).abs() < 1e-9);
}

#[test]
fn utf8_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let p = out_paths(dir.path(), "utf");
    let f = Field::new("name", DataType::Utf8, true);
    let schema = schema_with_geom(vec![f], GeometryType::Point, None);

    let mut sb = StringBuilder::new();
    sb.append_value("Tokyo");
    sb.append_value("New York");
    sb.append_value("São Paulo"); // 非 ASCII
    let arr: ArrayRef = Arc::new(sb.finish());
    write_geoms(
        &p,
        schema.clone(),
        &[
            Some(Geom::Point(0.0, 0.0)),
            Some(Geom::Point(1.0, 1.0)),
            Some(Geom::Point(2.0, 2.0)),
        ],
        vec![arr],
        None,
    );
    let (_s, _c, batches) = read_back(&p);
    let arr = batches[0]
        .column_by_name("name")
        .unwrap()
        .as_any()
        .downcast_ref::<arrow_array::StringArray>()
        .unwrap();
    assert_eq!(arr.value(0), "Tokyo");
    assert_eq!(arr.value(1), "New York");
    assert_eq!(arr.value(2), "São Paulo");
}

#[test]
fn boolean_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let p = out_paths(dir.path(), "bool");
    let f = Field::new("flag", DataType::Boolean, true);
    let schema = schema_with_geom(vec![f], GeometryType::Point, None);

    let mut bb = BooleanBuilder::new();
    bb.append_value(true);
    bb.append_value(false);
    let arr: ArrayRef = Arc::new(bb.finish());
    write_geoms(
        &p,
        schema.clone(),
        &[Some(Geom::Point(0.0, 0.0)), Some(Geom::Point(1.0, 1.0))],
        vec![arr],
        None,
    );
    let (_s, _c, batches) = read_back(&p);
    let arr = batches[0]
        .column_by_name("flag")
        .unwrap()
        .as_any()
        .downcast_ref::<arrow_array::BooleanArray>()
        .unwrap();
    assert!(arr.value(0));
    assert!(!arr.value(1));
}

#[test]
fn epsg_4326_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let p = out_paths(dir.path(), "crs");
    let crs = Crs::from_epsg(4326);
    let schema = schema_with_geom(vec![], GeometryType::Point, Some(crs.clone()));
    write_geoms(
        &p,
        schema.clone(),
        &[Some(Geom::Point(135.0, 35.0))],
        vec![],
        Some(crs),
    );
    let (_s, c, _b) = read_back(&p);
    assert_eq!(c.and_then(|c| c.epsg_code()), Some(4326));
}

#[test]
fn unsupported_binary_field_under_error_aborts() {
    let dir = tempfile::tempdir().unwrap();
    let p = out_paths(dir.path(), "bin_err");
    // 属性に Binary 列を含めて Error モードで開く → エラー。
    let bf = Field::new("blob", DataType::Binary, true);
    let schema = schema_with_geom(vec![bf], GeometryType::Point, None);

    let driver = ShpDriver::new();
    let uri = Uri::from_path(p.to_string_lossy().to_string());
    let opts = WriteOpts {
        on_loss: OnLoss::Error,
        overwrite: true,
        ..Default::default()
    };
    let r = driver.open_write(&uri, schema, None, &opts);
    match r {
        Err(shpx_core::Error::OnLoss { kind, .. }) => assert_eq!(kind, "binary-on-shp"),
        Err(other) => panic!("expected OnLoss binary-on-shp, got {other:?}"),
        Ok(_) => panic!("expected error, got Ok"),
    }
}

#[test]
fn unsupported_binary_field_under_skip_drops_column() {
    let dir = tempfile::tempdir().unwrap();
    let p = out_paths(dir.path(), "bin_skip");
    let bf = Field::new("blob", DataType::Binary, true);
    // ok 列も用意して、blob だけ skip され ok は残ることを確認。
    let ok = Field::new("ok", DataType::Boolean, true);
    let schema = schema_with_geom(vec![bf, ok], GeometryType::Point, None);

    let mut bb = BinaryBuilder::new();
    bb.append_value([1u8, 2, 3]);
    let blob: ArrayRef = Arc::new(bb.finish());
    let mut bo = BooleanBuilder::new();
    bo.append_value(true);
    let ok_arr: ArrayRef = Arc::new(bo.finish());

    let mut geo = BinaryBuilder::new();
    geo.append_value(wkb::encode(&Geom::Point(0.0, 0.0)).unwrap());
    let geo_arr: ArrayRef = Arc::new(geo.finish());

    let batch = RecordBatch::try_new(schema.clone(), vec![blob, ok_arr, geo_arr]).unwrap();

    let driver = ShpDriver::new();
    let uri = Uri::from_path(p.to_string_lossy().to_string());
    let opts = WriteOpts {
        on_loss: OnLoss::Skip,
        overwrite: true,
        ..Default::default()
    };
    let mut w = driver.open_write(&uri, schema, None, &opts).unwrap();
    w.write_batch(&batch).unwrap();
    w.finish().unwrap();

    let (read_schema, _, batches) = read_back(&p);
    // blob 列は dropped、ok 列のみ残る (+ geometry)。
    assert!(read_schema.column_with_name("blob").is_none());
    assert!(read_schema.column_with_name("ok").is_some());
    let arr = batches[0]
        .column_by_name("ok")
        .unwrap()
        .as_any()
        .downcast_ref::<arrow_array::BooleanArray>()
        .unwrap();
    assert!(arr.value(0));
}

#[test]
fn pointz_input_xy_drop_via_warn() {
    // 自前で PointZ Shapefile を作って読み戻す。
    use shapefile::{PointZ, ShapeWriter};
    let dir = tempfile::tempdir().unwrap();
    let p = out_paths(dir.path(), "ptz");

    // 最小の DBF を空フィールドで作る (ShapeWriter のみだと .dbf が無く Reader が開けない)。
    {
        let dbf_path = p.with_extension("dbf");
        let builder = shapefile::dbase::TableWriterBuilder::new()
            .add_character_field(shapefile::dbase::FieldName::try_from("name").unwrap(), 10);
        let mut tw = builder.build_with_file_dest(&dbf_path).unwrap();
        let mut rec = shapefile::dbase::Record::default();
        rec.insert(
            "name".to_string(),
            shapefile::dbase::FieldValue::Character(Some("a".to_string())),
        );
        tw.write_record(&rec).unwrap();
    }
    {
        let mut sw = ShapeWriter::from_path(&p).unwrap();
        sw.write_shape(&PointZ::new(10.0, 20.0, 30.0, 40.0))
            .unwrap();
    }

    // OnLoss::Warn でも CRS = None の通常 read を行う (既定 OnLoss::Warn)。
    let driver = ShpDriver::new();
    let uri = Uri::from_path(p.to_string_lossy().to_string());
    let mut r = driver.open_read(&uri, &ReadOpts::default()).unwrap();
    let batch = r.batches().next().unwrap().unwrap();
    let geom = batch
        .column_by_name("geometry")
        .unwrap()
        .as_any()
        .downcast_ref::<arrow_array::BinaryArray>()
        .unwrap();
    assert_eq!(wkb::decode(geom.value(0)).unwrap(), Geom::Point(10.0, 20.0));
}

#[test]
fn attribute_name_truncation_with_warn() {
    let dir = tempfile::tempdir().unwrap();
    let p = out_paths(dir.path(), "name_trunc");
    let f1 = Field::new("very_long_field_name_a", DataType::Boolean, true);
    let f2 = Field::new("very_long_field_name_b", DataType::Boolean, true);
    let schema = schema_with_geom(vec![f1, f2], GeometryType::Point, None);

    let mut b1 = BooleanBuilder::new();
    b1.append_value(true);
    let mut b2 = BooleanBuilder::new();
    b2.append_value(false);

    let mut geo = BinaryBuilder::new();
    geo.append_value(wkb::encode(&Geom::Point(0.0, 0.0)).unwrap());

    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(b1.finish()) as ArrayRef,
            Arc::new(b2.finish()),
            Arc::new(geo.finish()),
        ],
    )
    .unwrap();

    let driver = ShpDriver::new();
    let uri = Uri::from_path(p.to_string_lossy().to_string());
    let opts = WriteOpts {
        on_loss: OnLoss::Warn,
        overwrite: true,
        ..Default::default()
    };
    let mut w = driver.open_write(&uri, schema, None, &opts).unwrap();
    w.write_batch(&batch).unwrap();
    w.finish().unwrap();

    let (read_schema, _, _) = read_back(&p);
    let names: Vec<&str> = read_schema
        .fields()
        .iter()
        .map(|f| f.name().as_str())
        .collect();
    // very_long_ と very_long1 が並び (最後に geometry)。
    assert!(names.contains(&"very_long_"));
    assert!(names.contains(&"very_long1"));
}
