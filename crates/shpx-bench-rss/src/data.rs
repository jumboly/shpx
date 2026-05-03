//! peak RSS bench で全 driver が共有する Parquet 入力 generator。
//!
//! ベースは `crates/shpx-driver-postgis/benches/gen.rs` の 10 属性 + Point geometry
//! schema だが、**GPKG / SpatiaLite reader が全 integer 列を `Int64` に正規化する
//! 仕様** (`crates/shpx-driver-gpkg/src/reader.rs::Int(Int64Builder)`) と整合させる
//! ため、`class` 列だけ `Int32` → `Int64` に置き換えてある。それ以外は同一。
//!
//! 出力先: `<input_dir>/bench_<rows>.parquet` + `bench_<rows>.MANIFEST`。PostGIS
//! bench の `points_<rows>.parquet` とは file 名・schema いずれも別物 (cache 共有
//! しない) で、共存可能。
//!
//! Parquet を起点とする理由は (1) Arrow 型を完全保存できる (Decimal128 等)、
//! (2) 全 driver の writer が `LayerReader` 経由で読めば 1 ファイルから native
//! format に変換可能、の 2 点。

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use arrow_array::builder::{
    BinaryBuilder, BooleanBuilder, Date32Builder, Decimal128Builder, Float64Builder, Int64Builder,
    StringBuilder, TimestampMicrosecondBuilder,
};
use arrow_array::{ArrayRef, RecordBatch};
use arrow_schema::{DataType, Field, Schema, SchemaRef, TimeUnit};
use shpx_core::{
    schema::{GeometryMeta, GeometryType, GEOMETRY_META_KEY},
    Crs, Driver, Uri, WriteOpts,
};
use shpx_driver_parquet::ParquetDriver;
use shpx_geom::wkb::{self, Geom};

const CHUNK: usize = 50_000;

/// 指定 row 数の Parquet を `dir/bench_<rows>.parquet` に確保する。既に同 row 数の
/// MANIFEST が存在すればファイル生成をスキップする。
pub fn ensure_parquet(rows: usize, dir: &Path) -> PathBuf {
    fs::create_dir_all(dir).expect("create bench-data dir");
    let path = dir.join(format!("bench_{rows}.parquet"));
    let manifest = dir.join(format!("bench_{rows}.MANIFEST"));
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

/// 型網羅スキーマ (PostGIS bench `bulk_all_types_together` ベース、`class` だけ
/// GPKG / SpatiaLite reader の Int64 正規化と整合させるため `Int32` → `Int64` に置換)。
pub fn build_schema() -> SchemaRef {
    let crs = Some(Crs::from_epsg(4326));
    let mut fields = vec![
        Field::new("id", DataType::Int64, false),
        Field::new("flag", DataType::Boolean, true),
        Field::new("class", DataType::Int64, true),
        Field::new("score", DataType::Float64, true),
        Field::new("name", DataType::Utf8, true),
        Field::new("tag", DataType::Utf8, true),
        Field::new("amount", DataType::Decimal128(38, 10), true),
        Field::new("created", DataType::Date32, true),
        Field::new(
            "event_at",
            DataType::Timestamp(TimeUnit::Microsecond, Some("UTC".into())),
            true,
        ),
        Field::new("payload", DataType::Binary, true),
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
    let mut flag_b = BooleanBuilder::with_capacity(n);
    let mut class_b = Int64Builder::with_capacity(n);
    let mut score_b = Float64Builder::with_capacity(n);
    let mut name_b = StringBuilder::with_capacity(n, n * 16);
    let mut tag_b = StringBuilder::with_capacity(n, n * 18);
    let mut amount_b = Decimal128Builder::with_capacity(n)
        .with_precision_and_scale(38, 10)
        .unwrap();
    let mut created_b = Date32Builder::with_capacity(n);
    let mut event_b = TimestampMicrosecondBuilder::with_capacity(n).with_timezone("UTC");
    let mut payload_b = BinaryBuilder::with_capacity(n, n * 16);
    let mut geom_b = BinaryBuilder::with_capacity(n, n * 21);

    // 2026-04-25T00:00:00Z UTC の microseconds since epoch。PostGIS bench
    // bulk_all_types_together::expected_row と同値を維持する (cache 互換のため)。
    let base_micros: i64 = 1_777_680_000_000_000;

    for k in 0..n {
        let idx = i64::try_from(start + k).expect("row index fits i64");
        id_b.append_value(idx);

        if idx % 11 == 0 {
            flag_b.append_null();
        } else {
            flag_b.append_value(idx % 2 == 0);
        }
        if idx % 13 == 0 {
            class_b.append_null();
        } else {
            class_b.append_value(idx % 7);
        }
        if idx % 17 == 0 {
            score_b.append_null();
        } else {
            // 1/8 刻みは f64 完全表現可能。PostGIS bench と bit-identical。
            score_b.append_value(f64::from(i32::try_from(idx & 0x7FFF_FFFF).unwrap()) * 0.125);
        }
        name_b.append_value(format!("name_{idx:010}"));
        let tag_len = 4 + usize::try_from(idx.rem_euclid(29)).expect("rem fits usize");
        let tag: String = (0..tag_len)
            .map(|j| {
                let j_i64 = i64::try_from(j).expect("tag_len<=32 fits i64");
                let off = u8::try_from((idx + j_i64).rem_euclid(26)).expect("rem fits u8");
                char::from(b'a' + off)
            })
            .collect();
        tag_b.append_value(&tag);

        let amount: i128 = i128::from(idx) * 1_234_567_890_123_456_789i128;
        amount_b.append_value(amount);
        let created = 20100 + i32::try_from(idx % 365).expect("idx%365 fits i32");
        created_b.append_value(created);
        event_b.append_value(base_micros + idx);
        let lo = u8::try_from(idx & 0xff).expect("masked fits u8");
        let payload: Vec<u8> = (0..16u8).map(|j| lo.wrapping_add(j)).collect();
        payload_b.append_value(&payload);

        let lon = f64::from(i32::try_from(idx % 360).expect("idx%360 fits i32")) - 180.0;
        let lat = f64::from(i32::try_from(idx % 180).expect("idx%180 fits i32")) - 90.0;
        geom_b.append_value(wkb::encode(&Geom::Point(lon, lat)).unwrap());
    }

    let cols: Vec<ArrayRef> = vec![
        Arc::new(id_b.finish()),
        Arc::new(flag_b.finish()),
        Arc::new(class_b.finish()),
        Arc::new(score_b.finish()),
        Arc::new(name_b.finish()),
        Arc::new(tag_b.finish()),
        Arc::new(amount_b.finish()),
        Arc::new(created_b.finish()),
        Arc::new(event_b.finish()),
        Arc::new(payload_b.finish()),
        Arc::new(geom_b.finish()),
    ];
    RecordBatch::try_new(schema, cols).expect("RecordBatch::try_new")
}
