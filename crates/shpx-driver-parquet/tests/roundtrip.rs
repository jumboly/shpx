//! GeoParquet reader/writer の統合テスト。
//!
//! tempdir 上で writer→reader の往復だけで検証する。`assert_cmd` は使わない。
//! テストランナは `cargo test -p shpx-driver-parquet --test roundtrip`。

use std::sync::Arc;

use arrow_array::{
    builder::{BinaryBuilder, BooleanBuilder, Date32Builder, Decimal128Builder, StringBuilder},
    ArrayRef, RecordBatch,
};
use arrow_schema::{DataType, Field, Schema};
use shpx_core::{
    schema::{GeometryMeta, GeometryType, GEOMETRY_META_KEY},
    Crs, Driver, ReadOpts, Uri, WriteOpts,
};
use shpx_driver_parquet::ParquetDriver;
use shpx_geom::wkb::{self, Geom};

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
    opts: &WriteOpts,
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

    let driver = ParquetDriver::new();
    let uri = Uri::from_path(path.to_string_lossy().to_string());
    let mut w = driver.open_write(&uri, schema, crs, opts).unwrap();
    w.write_batch(&batch).unwrap();
    w.finish().unwrap();
}

fn read_back(path: &std::path::Path) -> (Arc<Schema>, Option<Crs>, Vec<RecordBatch>) {
    let driver = ParquetDriver::new();
    let uri = Uri::from_path(path.to_string_lossy().to_string());
    let opts = ReadOpts::default();
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
fn point_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("point.parquet");
    let schema = schema_with_geom(vec![], GeometryType::Point, None);
    write_geoms(
        &p,
        schema.clone(),
        &[Some(Geom::Point(1.0, 2.0)), Some(Geom::Point(-3.5, 4.25))],
        vec![],
        None,
        &default_write_opts(),
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
fn polygon_with_hole_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("polygon.parquet");
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
    let p = dir.path().join("mls.parquet");
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
fn epsg_4326_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("crs.parquet");
    let schema = schema_with_geom(vec![], GeometryType::Point, Some(Crs::from_epsg(4326)));
    write_geoms(
        &p,
        schema.clone(),
        &[Some(Geom::Point(0.0, 0.0))],
        vec![],
        Some(Crs::from_epsg(4326)),
        &default_write_opts(),
    );
    let (_s, crs, _batches) = read_back(&p);
    assert_eq!(crs.as_ref().and_then(Crs::epsg_code), Some(4326));
}

#[test]
fn attribute_types_preserved() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("attrs.parquet");

    let fields = vec![
        Field::new("name", DataType::Utf8, true),
        Field::new("active", DataType::Boolean, true),
        Field::new("amount", DataType::Decimal128(10, 3), true),
        Field::new("birth", DataType::Date32, true),
    ];
    let schema = schema_with_geom(fields, GeometryType::Point, None);

    let mut name = StringBuilder::new();
    name.append_value("São Paulo");
    name.append_value("Tokyo");
    let mut active = BooleanBuilder::new();
    active.append_value(true);
    active.append_value(false);
    let mut amount = Decimal128Builder::new().with_data_type(DataType::Decimal128(10, 3));
    amount.append_value(123_456); // 123.456
    amount.append_value(-1_001); //  -1.001
    let mut birth = Date32Builder::new();
    birth.append_value(0); // 1970-01-01
    birth.append_value(20_563); // 2026-04-25 周辺
    let attrs: Vec<ArrayRef> = vec![
        Arc::new(name.finish()),
        Arc::new(active.finish()),
        Arc::new(amount.finish()),
        Arc::new(birth.finish()),
    ];
    write_geoms(
        &p,
        schema.clone(),
        &[Some(Geom::Point(0.0, 0.0)), Some(Geom::Point(1.0, 1.0))],
        attrs,
        None,
        &default_write_opts(),
    );

    let (out_schema, _crs, batches) = read_back(&p);
    assert_eq!(batches[0].num_rows(), 2);
    // 型が保全されていることを確認（Parquet/Arrow ネイティブ）。
    assert_eq!(
        out_schema.field_with_name("amount").unwrap().data_type(),
        &DataType::Decimal128(10, 3)
    );
    assert_eq!(
        out_schema.field_with_name("birth").unwrap().data_type(),
        &DataType::Date32
    );
    let name_arr = batches[0]
        .column_by_name("name")
        .unwrap()
        .as_any()
        .downcast_ref::<arrow_array::StringArray>()
        .unwrap();
    assert_eq!(name_arr.value(0), "São Paulo");
}

#[test]
fn overwrite_false_fails_when_exists() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("exists.parquet");
    let schema = schema_with_geom(vec![], GeometryType::Point, None);

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

    let driver = ParquetDriver::new();
    let uri = Uri::from_path(p.to_string_lossy().to_string());
    let Err(err) = driver.open_write(&uri, schema, None, &WriteOpts::default()) else {
        panic!("open_write must fail when overwrite=false and file exists");
    };
    assert!(matches!(err, shpx_core::Error::Format(_)));
}

#[test]
fn batch_size_hint_splits_row_groups() {
    // batch_size_hint=2 で 5 行を書き込めば、row group が 3 つに分かれる。
    // 読み出し側の batch 分割は parquet 内部の row group 単位ではなく
    // ParquetRecordBatchReader の batch_size に依存するため、ここでは
    // 行数が一致することのみ検証する（writer が落ちず合計行数を保持する点が要旨）。
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("split.parquet");
    let schema = schema_with_geom(vec![], GeometryType::Point, None);

    let mut bb = BinaryBuilder::new();
    let mut geoms = vec![];
    for i in 0..5 {
        let g = Geom::Point(f64::from(i), 0.0);
        geoms.push(g.clone());
        bb.append_value(wkb::encode(&g).unwrap());
    }
    let cols: Vec<ArrayRef> = vec![Arc::new(bb.finish())];
    let batch = RecordBatch::try_new(schema.clone(), cols).unwrap();

    let driver = ParquetDriver::new();
    let uri = Uri::from_path(p.to_string_lossy().to_string());
    let opts = WriteOpts {
        overwrite: true,
        batch_size_hint: Some(2),
        ..Default::default()
    };
    let mut w = driver.open_write(&uri, schema, None, &opts).unwrap();
    w.write_batch(&batch).unwrap();
    w.finish().unwrap();

    let (_s, _c, batches) = read_back(&p);
    let total: usize = batches.iter().map(RecordBatch::num_rows).sum();
    assert_eq!(total, 5);
}
