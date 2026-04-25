//! v0.3 cycle 3c: PostGIS bench 用の合成データ生成器。
//!
//! `benches/copy_binary.rs` から `mod gen;` で読み込まれる内部モジュール。
//!
//! # 中間フォーマット
//!
//! Parquet を採用する（プランで検討した GPKG では Decimal128 が SQLite affinity の
//! 影響でロスレス保存できないため）。Parquet は Arrow 型を完全保存し、`ogr2ogr` も
//! GDAL 3.7+ の Parquet driver で同じファイルを直接読めるので、shpx と ogr2ogr に
//! 同一バイト列の入力を与える「公平な比較」が成立する。
//!
//! # 再現性
//!
//! `StdRng::seed_from_u64(SEED)` 固定。row 数とサイズを `MANIFEST.txt` に書き出し、
//! 同じ row 数で再生成要求が来た場合はファイル再利用してベンチ全体の起動時間を短縮する。

#![allow(dead_code)] // bench main から呼ばれない関数は warning 抑制対象。

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use arrow_array::builder::{
    BinaryBuilder, BooleanBuilder, Date32Builder, Decimal128Builder, Float64Builder, Int32Builder,
    Int64Builder, StringBuilder, TimestampMicrosecondBuilder,
};
use arrow_array::{ArrayRef, RecordBatch};
use arrow_schema::{DataType, Field, Schema, SchemaRef, TimeUnit};
use rand::rngs::StdRng;
use rand::SeedableRng;
use shpx_core::{
    schema::{GeometryMeta, GeometryType, GEOMETRY_META_KEY},
    Crs, Driver, Uri, WriteOpts,
};
use shpx_driver_parquet::ParquetDriver;
use shpx_geom::wkb::{self, Geom};

/// 合成 GPKG/Parquet の決定論的乱数シード。`tests/bulk_roundtrip.rs::expected_row` と
/// 同じ生成ルールに揃えれば 10 列同居 bit-identical テストも兼任できる構造。
pub const SEED: u64 = 0x0011_0303_C0DE;

/// 1 batch あたりの行数。Arrow ビルダのメモリ使用量と Parquet row group の粒度を
/// バランスする実用値（`shpx-driver-parquet` の write_batch は受けたバッチを 1 つの
/// row group として扱う実装と仮定）。
const CHUNK: usize = 50_000;

/// 指定 row 数の Parquet を `dir/points_<rows>.parquet` に確保する。既に同 row 数の
/// MANIFEST が存在すればファイル生成をスキップする。
pub fn ensure_parquet(rows: usize, dir: &Path) -> PathBuf {
    fs::create_dir_all(dir).expect("create bench-data dir");
    let path = dir.join(format!("points_{rows}.parquet"));
    let manifest = dir.join(format!("points_{rows}.MANIFEST"));
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

    // RNG は使用しないが、将来的に乱数化したくなった際の入口として保持。
    let mut _rng = StdRng::seed_from_u64(SEED);

    let mut written: usize = 0;
    while written < rows {
        let n = CHUNK.min(rows - written);
        let batch = build_batch(schema.clone(), written, n);
        writer.write_batch(&batch).expect("write_batch");
        written += n;
    }
    writer.finish().expect("parquet finish");
}

/// ベンチスキーマ。`tests/bulk_roundtrip.rs::bulk_all_types_together` と 1:1 で揃え、
/// 型網羅テストとベンチデータが同一ロジックで生成されることを保証する。
pub fn build_schema() -> SchemaRef {
    let crs = Some(Crs::from_epsg(4326));
    let mut fields = vec![
        Field::new("id", DataType::Int64, false),
        Field::new("flag", DataType::Boolean, true),
        Field::new("class", DataType::Int32, true),
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
    let mut class_b = Int32Builder::with_capacity(n);
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

    let base_micros: i64 = 1_777_680_000_000_000; // 2026-04-25T00:00:00Z

    for k in 0..n {
        let idx = i64::try_from(start + k).expect("row index fits i64");
        id_b.append_value(idx);

        // null pattern: tests/bulk_roundtrip.rs::expected_row と同一の素数で散らす。
        if idx % 11 == 0 {
            flag_b.append_null();
        } else {
            flag_b.append_value(idx % 2 == 0);
        }
        if idx % 13 == 0 {
            class_b.append_null();
        } else {
            class_b.append_value(i32::try_from(idx % 7).expect("idx%7 fits i32"));
        }
        if idx % 17 == 0 {
            score_b.append_null();
        } else {
            // 1/8 刻みは f64 完全表現可能。bench input の Parquet も bit-identical を保つ。
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
        // 2025-01-01 を起点にした緩い分布。bit-identical 比較は不要だが日付列を埋める。
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
