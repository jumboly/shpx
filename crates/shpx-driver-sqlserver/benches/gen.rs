//! SQL Server bench 用の合成データ生成器。
//!
//! v0.4 では tiberius 0.12 の bulk encode に既知不整合があり (`datetime2` /
//! `datetimeoffset` / 連続 `varbinary(max)` の組み合わせで `Invalid column type
//! from bcp client` を踏むケースがある)、bench は確実に動く最小スキーマ
//! (Int64 / Utf8 / Float64 / Point) で wall-clock 比較を取る。bit-identical な
//! 型網羅は別途 `tests/bulk_roundtrip.rs` の単独テストで確認している。

#![allow(dead_code)]

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use arrow_array::builder::{BinaryBuilder, Float64Builder, Int64Builder, StringBuilder};
use arrow_array::{ArrayRef, RecordBatch};
use arrow_schema::{DataType, Field, Schema, SchemaRef};
use shpx_core::{
    schema::{GeometryMeta, GeometryType, GEOMETRY_META_KEY},
    Crs, Driver, Uri, WriteOpts,
};
use shpx_driver_parquet::ParquetDriver;
use shpx_geom::wkb::{self, Geom};

const CHUNK: usize = 50_000;

pub fn ensure_parquet(rows: usize, dir: &Path) -> PathBuf {
    fs::create_dir_all(dir).expect("create bench-data dir");
    let path = dir.join(format!("points_mssql_{rows}.parquet"));
    let manifest = dir.join(format!("points_mssql_{rows}.MANIFEST"));
    let want = format!("rows={rows}\n");

    if path.exists() && manifest.exists() {
        let got = fs::read_to_string(&manifest).unwrap_or_default();
        if got == want {
            return path;
        }
    }
    let _ = fs::remove_file(&path);
    write_synthetic(rows, &path);
    fs::write(&manifest, want).expect("manifest");
    path
}

fn write_synthetic(rows: usize, path: &Path) {
    let driver = ParquetDriver;
    let schema = build_schema();
    let crs = Some(Crs::from_epsg(4326));
    let uri = Uri::from_path(path.display().to_string());
    let opts = WriteOpts {
        overwrite: true,
        ..Default::default()
    };
    let mut writer = driver
        .open_write(&uri, schema.clone(), crs, &opts)
        .expect("parquet open_write");

    let mut written: usize = 0;
    while written < rows {
        let n = CHUNK.min(rows - written);
        let batch = build_batch(schema.clone(), written, n);
        writer.write_batch(&batch).expect("write_batch");
        written += n;
    }
    writer.finish().expect("parquet finish");
}

pub fn build_schema() -> SchemaRef {
    let crs = Some(Crs::from_epsg(4326));
    let mut fields = vec![
        Field::new("id", DataType::Int64, false),
        Field::new("name", DataType::Utf8, true),
        Field::new("score", DataType::Float64, true),
    ];
    let mut g = Field::new("geom", DataType::Binary, true);
    let mut m = HashMap::new();
    m.insert(
        GEOMETRY_META_KEY.to_string(),
        GeometryMeta::wkb(GeometryType::Point, crs)
            .to_json()
            .unwrap(),
    );
    g.set_metadata(m);
    fields.push(g);
    Arc::new(Schema::new(fields))
}

fn build_batch(schema: SchemaRef, start: usize, n: usize) -> RecordBatch {
    let mut id_b = Int64Builder::with_capacity(n);
    let mut name_b = StringBuilder::with_capacity(n, n * 16);
    let mut score_b = Float64Builder::with_capacity(n);
    let mut geom_b = BinaryBuilder::with_capacity(n, n * 21);

    for k in 0..n {
        let idx = i64::try_from(start + k).expect("row index fits i64");
        id_b.append_value(idx);
        name_b.append_value(format!("name_{idx:010}"));
        if idx % 17 == 0 {
            score_b.append_null();
        } else {
            score_b.append_value(f64::from(i32::try_from(idx & 0x7FFF_FFFF).unwrap()) * 0.125);
        }
        let lon = f64::from(i32::try_from(idx % 360).expect("idx%360 fits i32")) - 180.0;
        let lat = f64::from(i32::try_from(idx % 180).expect("idx%180 fits i32")) - 90.0;
        geom_b.append_value(wkb::encode(&Geom::Point(lon, lat)).unwrap());
    }

    let cols: Vec<ArrayRef> = vec![
        Arc::new(id_b.finish()),
        Arc::new(name_b.finish()),
        Arc::new(score_b.finish()),
        Arc::new(geom_b.finish()),
    ];
    RecordBatch::try_new(schema, cols).expect("RecordBatch::try_new")
}
