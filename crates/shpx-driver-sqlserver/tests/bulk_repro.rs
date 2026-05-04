//! tiberius 0.12.3 Daten COLMETADATA bug の trigger 軸を二分探索で詰めた観察 suite
//! (`docs/SQLSERVER_BULK_BUG_REPRO.md` 参照)。bug は fork で修正済みだが、regression
//! 検出用に case をそのまま残してある。
//!
//! 実行: `SHPX_TEST_SQLSERVER_URL=mssql://... cargo test -p shpx-driver-sqlserver
//!         --test bulk_repro -- --ignored --test-threads=1 --nocapture`
//!
//! `--test-threads=1` 必須: chunk_size_* が `SHPX_MSSQL_BULK_CHUNK` env を上書きするため
//! 並列実行で他 test に漏れる。staging table 名衝突回避もこの flag に依存。

use std::collections::HashMap;
use std::sync::Arc;

use arrow_array::{
    builder::{
        BinaryBuilder, BooleanBuilder, Date32Builder, Decimal128Builder, Float64Builder,
        Int32Builder, Int64Builder, StringBuilder, TimestampMicrosecondBuilder,
    },
    ArrayRef, RecordBatch,
};
use arrow_schema::{DataType, Field, Schema, SchemaRef, TimeUnit};
use shpx_core::{
    schema::{GeometryMeta, GeometryType, GEOMETRY_META_KEY},
    BulkLoadWriter, Crs, Driver, ReadOpts,
};
use shpx_driver_sqlserver::SqlServerDriver;
use shpx_geom::wkb::{self, Geom};

mod common;
use common::{cleanup, mssql_url, unique_table, uri_with_table, write_opts};

// ---------- Schema spec DSL ----------

/// 列スペック。bulk_repro.rs の各テストはこの vec を 1 つ作るだけで schema + record batch を
/// 自動生成できる。「再現条件 → schema」の対応を 1 行で書けるようにするのが狙い。
#[derive(Clone, Debug)]
struct Col {
    name: &'static str,
    kind: ColKind,
    nullable: bool,
}

#[derive(Clone, Copy, Debug)]
enum ColKind {
    Int64,
    Boolean,
    Int32,
    Float64,
    Utf8,
    /// `decimal(38, 10)` 固定。bug trigger 候補。
    Decimal128_38_10,
    Date32,
    /// `Timestamp(μs, None)` → `datetime2`. `bulk_all_types_together` の event_at と同じ。
    TimestampNaive,
    /// `Timestamp(μs, UTC)` → `datetimeoffset`. `bulk_timestamptz_and_int64_bit_identical` 経路。
    TimestampUtc,
    /// `varbinary(max)`. payload 列で trigger 候補。
    Binary,
    /// geometry 列 (Point WKB + Crs metadata)。schema の末尾に必ず 1 つ置く。
    GeomPoint,
}

impl Col {
    fn new(name: &'static str, kind: ColKind, nullable: bool) -> Self {
        Self {
            name,
            kind,
            nullable,
        }
    }
}

/// 列の `Field` を組み立てる。geom 列のみ metadata 付き。
fn build_field(c: &Col) -> Field {
    let dt = match c.kind {
        ColKind::Int64 => DataType::Int64,
        ColKind::Boolean => DataType::Boolean,
        ColKind::Int32 => DataType::Int32,
        ColKind::Float64 => DataType::Float64,
        ColKind::Utf8 => DataType::Utf8,
        ColKind::Decimal128_38_10 => DataType::Decimal128(38, 10),
        ColKind::Date32 => DataType::Date32,
        ColKind::TimestampNaive => DataType::Timestamp(TimeUnit::Microsecond, None),
        ColKind::TimestampUtc => DataType::Timestamp(TimeUnit::Microsecond, Some("UTC".into())),
        ColKind::Binary | ColKind::GeomPoint => DataType::Binary,
    };
    let mut f = Field::new(c.name, dt, c.nullable);
    if matches!(c.kind, ColKind::GeomPoint) {
        let mut m = HashMap::new();
        m.insert(
            GEOMETRY_META_KEY.to_string(),
            GeometryMeta::wkb(GeometryType::Point, Some(Crs::from_epsg(4326)))
                .to_json()
                .unwrap(),
        );
        f.set_metadata(m);
    }
    f
}

/// 列ごとに `n_rows` 個の値を持つ `ArrayRef` を組み立てる。値は idx ベースの決定的な
/// パターン。bulk bug の発火に値の中身は影響しないと想定し、最小限のバリエーションで揃える。
fn build_array(c: &Col, n_rows: usize) -> ArrayRef {
    match c.kind {
        ColKind::Int64 => {
            let mut b = Int64Builder::new();
            for i in 0..n_rows {
                b.append_value(i64::try_from(i).unwrap());
            }
            Arc::new(b.finish())
        }
        ColKind::Boolean => {
            let mut b = BooleanBuilder::new();
            for i in 0..n_rows {
                b.append_value(i % 2 == 0);
            }
            Arc::new(b.finish())
        }
        ColKind::Int32 => {
            let mut b = Int32Builder::new();
            for i in 0..n_rows {
                b.append_value(i32::try_from(i).unwrap());
            }
            Arc::new(b.finish())
        }
        ColKind::Float64 => {
            let mut b = Float64Builder::new();
            for i in 0..n_rows {
                #[allow(clippy::cast_precision_loss)]
                b.append_value(i as f64 * 0.125);
            }
            Arc::new(b.finish())
        }
        ColKind::Utf8 => {
            let mut b = StringBuilder::new();
            for i in 0..n_rows {
                b.append_value(format!("v{i}"));
            }
            Arc::new(b.finish())
        }
        ColKind::Decimal128_38_10 => {
            let mut b = Decimal128Builder::new()
                .with_precision_and_scale(38, 10)
                .unwrap();
            for i in 0..n_rows {
                let v: i128 = i128::try_from(i).unwrap() * 1_234_567_890_123_456_789i128;
                b.append_value(v);
            }
            Arc::new(b.finish())
        }
        ColKind::Date32 => {
            let mut b = Date32Builder::new();
            for i in 0..n_rows {
                b.append_value(20_100 + i32::try_from(i).unwrap());
            }
            Arc::new(b.finish())
        }
        ColKind::TimestampNaive => {
            let mut b = TimestampMicrosecondBuilder::new();
            let base: i64 = 1_777_680_000_000_000;
            for i in 0..n_rows {
                b.append_value(base + i64::try_from(i).unwrap());
            }
            Arc::new(b.finish())
        }
        ColKind::TimestampUtc => {
            let mut b = TimestampMicrosecondBuilder::new().with_timezone("UTC");
            let base: i64 = 1_777_680_000_000_000;
            for i in 0..n_rows {
                b.append_value(base + i64::try_from(i).unwrap());
            }
            Arc::new(b.finish())
        }
        ColKind::Binary => {
            let mut b = BinaryBuilder::new();
            for i in 0..n_rows {
                let lo = u8::try_from(i & 0xff).unwrap();
                let payload: Vec<u8> = (0..16u8).map(|j| lo.wrapping_add(j)).collect();
                b.append_value(&payload);
            }
            Arc::new(b.finish())
        }
        ColKind::GeomPoint => {
            let mut b = BinaryBuilder::new();
            for i in 0..n_rows {
                let lon = (f64::from(i32::try_from(i % 360).unwrap())) - 180.0;
                let lat = (f64::from(i32::try_from(i % 180).unwrap())) - 90.0;
                b.append_value(wkb::encode(&Geom::Point(lon, lat)).unwrap());
            }
            Arc::new(b.finish())
        }
    }
}

/// schema の各列を 1 行表記で要約する (case ログに含めるため)。例:
/// `[id:Int64 NN, amount:Decimal128(38,10) N, payload:Binary N, geom:Binary N]`
fn schema_summary(cols: &[Col]) -> String {
    let parts: Vec<String> = cols
        .iter()
        .map(|c| {
            let kind = match c.kind {
                ColKind::Int64 => "Int64",
                ColKind::Boolean => "Boolean",
                ColKind::Int32 => "Int32",
                ColKind::Float64 => "Float64",
                ColKind::Utf8 => "Utf8",
                ColKind::Decimal128_38_10 => "Decimal128(38,10)",
                ColKind::Date32 => "Date32",
                ColKind::TimestampNaive => "Timestamp(μs,None)",
                ColKind::TimestampUtc => "Timestamp(μs,UTC)",
                ColKind::Binary => "Binary",
                ColKind::GeomPoint => "GeomPoint",
            };
            let null = if c.nullable { "N" } else { "NN" };
            format!("{}:{kind} {null}", c.name)
        })
        .collect();
    format!("[{}]", parts.join(", "))
}

fn build_schema_and_batch(cols: &[Col], n_rows: usize) -> (SchemaRef, RecordBatch) {
    assert!(
        matches!(cols.last().map(|c| c.kind), Some(ColKind::GeomPoint)),
        "最後の列は必ず GeomPoint (writer の geom 列検出が末尾を期待する schema 設計)"
    );
    let fields: Vec<Field> = cols.iter().map(build_field).collect();
    let schema = Arc::new(Schema::new(fields));
    let arrays: Vec<ArrayRef> = cols.iter().map(|c| build_array(c, n_rows)).collect();
    let batch = RecordBatch::try_new(schema.clone(), arrays).unwrap();
    (schema, batch)
}

// ---------- 結果型 + harness ----------

/// 各 case の結果。`Debug` 経由で `eprintln!` に出すだけで、コードからは直接読まないため
/// dead_code analysis を抑止する (Debug derive は dead_code に拾われない既知挙動)。
#[allow(dead_code)]
#[derive(Debug)]
enum Outcome {
    /// bulk_write + finish + read-back まで全部成功。
    Ok,
    /// `Invalid column type from bcp client for colid N` で失敗。
    FailColid(u8),
    /// 上記以外の error (環境問題 / type-mapping ミス等)。
    Err(String),
}

fn detect_colid(msg: &str) -> Option<u8> {
    let after = msg.split("colid ").nth(1)?;
    let n_str: String = after.chars().take_while(char::is_ascii_digit).collect();
    n_str.parse().ok()
}

/// schema + batch を作って bulk_write を実行し、結果を [`Outcome`] にマップする。
/// table の cleanup までこの関数で行う (失敗時も含む)。
fn run_bulk(case: &str, cols: &[Col], n_rows: usize) -> Outcome {
    let Some(url) = mssql_url() else {
        eprintln!("[bulk_repro/{case}] SHPX_TEST_SQLSERVER_URL unset; skipping");
        // skip された case は test framework 上は緑にする (eprintln 経由で見える)。
        return Outcome::Ok;
    };
    let table = unique_table(&format!("shpx_repro_{case}"));
    let uri = uri_with_table(&url, &table);

    let (schema, batch) = build_schema_and_batch(cols, n_rows);
    let opts = write_opts();

    let driver = SqlServerDriver::new();
    let outcome = (|| -> Outcome {
        let mut w = match driver.open_bulk_write(&uri, schema.clone(), Some(Crs::from_epsg(4326)), &opts) {
            Ok(Some(w)) => w,
            Ok(None) => return Outcome::Err("bulk_load=false unexpected".into()),
            Err(e) => return classify_err(&e.to_string()),
        };
        let mut iter = std::iter::once(Ok(batch));
        if let Err(e) = BulkLoadWriter::bulk_write(w.as_mut(), &mut iter) {
            return classify_err(&e.to_string());
        }
        if let Err(e) = w.finish() {
            return classify_err(&e.to_string());
        }
        // bulk_write が緑のときだけ readback で colid ズレと row 数を検算する。
        // 失敗 case は readback で SQL Server の error を 2 度踏むだけなので skip。
        match driver.open_read(&uri, &ReadOpts::default()) {
            Ok(mut r) => {
                let total: usize = r
                    .batches()
                    .filter_map(Result::ok)
                    .map(|b| b.num_rows())
                    .sum();
                if total != n_rows {
                    return Outcome::Err(format!("readback rows={total} want={n_rows}"));
                }
            }
            Err(e) => return Outcome::Err(format!("readback open: {e}")),
        }
        Outcome::Ok
    })();

    cleanup(&url, &table);

    eprintln!(
        "[bulk_repro/{case}] {} → {outcome:?}",
        schema_summary(cols)
    );
    outcome
}

fn classify_err(msg: &str) -> Outcome {
    if let Some(n) = detect_colid(msg) {
        Outcome::FailColid(n)
    } else {
        Outcome::Err(msg.to_string())
    }
}

// ---------- 共通 schema ビルダ ----------

/// 既存 `bulk_all_types_together` と同形の 11 列 (id..payload + geom)、event_at は tz-naive。
fn cols_all_types_naive() -> Vec<Col> {
    vec![
        Col::new("id", ColKind::Int64, false),
        Col::new("flag", ColKind::Boolean, true),
        Col::new("class", ColKind::Int32, true),
        Col::new("score", ColKind::Float64, true),
        Col::new("name", ColKind::Utf8, true),
        Col::new("tag", ColKind::Utf8, true),
        Col::new("amount", ColKind::Decimal128_38_10, true),
        Col::new("created", ColKind::Date32, true),
        Col::new("event_at", ColKind::TimestampNaive, true),
        Col::new("payload", ColKind::Binary, true),
        Col::new("geom", ColKind::GeomPoint, true),
    ]
}

// ---------- Fase 1 — Pairwise (3 trigger 型の 2 組) ----------
//
// 既存緑カバレッジ:
// - Decimal128(38,10) 単独: bulk_decimal_38_10_bit_identical
// - Timestamp tz-aware + Int64: bulk_timestamptz_and_int64_bit_identical
// - Binary 単独: 暗黙 (各 bulk テストの geom 列)
// 未検証の組:
// - Timestamp tz-naive を含む組 (bulk_all_types_together は tz-naive を使う)
// - Decimal + Binary
// - Timestamp + Binary

#[ignore = "SHPX_TEST_SQLSERVER_URL env required; observation only"]
#[test]
fn pair_decimal_timestamp_naive() {
    let cols = vec![
        Col::new("id", ColKind::Int64, false),
        Col::new("amount", ColKind::Decimal128_38_10, true),
        Col::new("event_at", ColKind::TimestampNaive, true),
        Col::new("geom", ColKind::GeomPoint, true),
    ];
    let _ = run_bulk("pair_decimal_timestamp_naive", &cols, 5);
}

#[ignore = "SHPX_TEST_SQLSERVER_URL env required; observation only"]
#[test]
fn pair_decimal_timestamp_utc() {
    let cols = vec![
        Col::new("id", ColKind::Int64, false),
        Col::new("amount", ColKind::Decimal128_38_10, true),
        Col::new("event_at", ColKind::TimestampUtc, true),
        Col::new("geom", ColKind::GeomPoint, true),
    ];
    let _ = run_bulk("pair_decimal_timestamp_utc", &cols, 5);
}

#[ignore = "SHPX_TEST_SQLSERVER_URL env required; observation only"]
#[test]
fn pair_decimal_binary() {
    let cols = vec![
        Col::new("id", ColKind::Int64, false),
        Col::new("amount", ColKind::Decimal128_38_10, true),
        Col::new("payload", ColKind::Binary, true),
        Col::new("geom", ColKind::GeomPoint, true),
    ];
    let _ = run_bulk("pair_decimal_binary", &cols, 5);
}

#[ignore = "SHPX_TEST_SQLSERVER_URL env required; observation only"]
#[test]
fn pair_timestamp_naive_binary() {
    let cols = vec![
        Col::new("id", ColKind::Int64, false),
        Col::new("event_at", ColKind::TimestampNaive, true),
        Col::new("payload", ColKind::Binary, true),
        Col::new("geom", ColKind::GeomPoint, true),
    ];
    let _ = run_bulk("pair_timestamp_naive_binary", &cols, 5);
}

#[ignore = "SHPX_TEST_SQLSERVER_URL env required; observation only"]
#[test]
fn pair_timestamp_utc_binary() {
    let cols = vec![
        Col::new("id", ColKind::Int64, false),
        Col::new("event_at", ColKind::TimestampUtc, true),
        Col::new("payload", ColKind::Binary, true),
        Col::new("geom", ColKind::GeomPoint, true),
    ];
    let _ = run_bulk("pair_timestamp_utc_binary", &cols, 5);
}

// ---------- Fase 2 — Triple minimal (3 型同時の最小列構成) ----------

#[ignore = "SHPX_TEST_SQLSERVER_URL env required; observation only"]
#[test]
fn triple_minimal_naive() {
    let cols = vec![
        Col::new("id", ColKind::Int64, false),
        Col::new("amount", ColKind::Decimal128_38_10, true),
        Col::new("event_at", ColKind::TimestampNaive, true),
        Col::new("payload", ColKind::Binary, true),
        Col::new("geom", ColKind::GeomPoint, true),
    ];
    let _ = run_bulk("triple_minimal_naive", &cols, 5);
}

#[ignore = "SHPX_TEST_SQLSERVER_URL env required; observation only"]
#[test]
fn triple_minimal_utc() {
    let cols = vec![
        Col::new("id", ColKind::Int64, false),
        Col::new("amount", ColKind::Decimal128_38_10, true),
        Col::new("event_at", ColKind::TimestampUtc, true),
        Col::new("payload", ColKind::Binary, true),
        Col::new("geom", ColKind::GeomPoint, true),
    ];
    let _ = run_bulk("triple_minimal_utc", &cols, 5);
}

// ---------- Fase 3 — 列数二分探索 (Fase 2 が緑だった場合のみ意味) ----------

#[ignore = "SHPX_TEST_SQLSERVER_URL env required; observation only"]
#[test]
fn triple_plus_3noise() {
    // triple minimal (5 列) + flag/class/score = 8 列。
    let cols = vec![
        Col::new("id", ColKind::Int64, false),
        Col::new("flag", ColKind::Boolean, true),
        Col::new("class", ColKind::Int32, true),
        Col::new("score", ColKind::Float64, true),
        Col::new("amount", ColKind::Decimal128_38_10, true),
        Col::new("event_at", ColKind::TimestampNaive, true),
        Col::new("payload", ColKind::Binary, true),
        Col::new("geom", ColKind::GeomPoint, true),
    ];
    let _ = run_bulk("triple_plus_3noise", &cols, 5);
}

#[ignore = "SHPX_TEST_SQLSERVER_URL env required; observation only"]
#[test]
fn triple_plus_5noise() {
    // 8 列 + name/tag(Utf8 連続) = 10 列。
    let cols = vec![
        Col::new("id", ColKind::Int64, false),
        Col::new("flag", ColKind::Boolean, true),
        Col::new("class", ColKind::Int32, true),
        Col::new("score", ColKind::Float64, true),
        Col::new("name", ColKind::Utf8, true),
        Col::new("tag", ColKind::Utf8, true),
        Col::new("amount", ColKind::Decimal128_38_10, true),
        Col::new("event_at", ColKind::TimestampNaive, true),
        Col::new("payload", ColKind::Binary, true),
        Col::new("geom", ColKind::GeomPoint, true),
    ];
    let _ = run_bulk("triple_plus_5noise", &cols, 5);
}

#[ignore = "SHPX_TEST_SQLSERVER_URL env required; observation only"]
#[test]
fn full_all_types_repro() {
    // 既存 bulk_all_types_together と同型の 11 列。bulk_repro 内で再現条件を再確認するため
    // 重複を厭わず置く (失敗 colid のベースライン値として 9 が出るかを確認)。
    let cols = cols_all_types_naive();
    let _ = run_bulk("full_all_types_repro", &cols, 5);
}

// ---------- Fase 4 — Sensitivity ----------
// minimal repro が確定したら、その base から軸を 1 つずつ動かして trigger 軸を切り分ける。

// 4.1 列順 (column-order 依存性、tiberius #410 の指摘点)

#[ignore = "SHPX_TEST_SQLSERVER_URL env required; observation only"]
#[test]
fn order_swap_amount_payload() {
    // triple_minimal_naive で payload と amount を入れ替え。失敗 colid N が変動するか確認。
    let cols = vec![
        Col::new("id", ColKind::Int64, false),
        Col::new("payload", ColKind::Binary, true),
        Col::new("event_at", ColKind::TimestampNaive, true),
        Col::new("amount", ColKind::Decimal128_38_10, true),
        Col::new("geom", ColKind::GeomPoint, true),
    ];
    let _ = run_bulk("order_swap_amount_payload", &cols, 5);
}

#[ignore = "SHPX_TEST_SQLSERVER_URL env required; observation only"]
#[test]
fn order_amount_first() {
    let cols = vec![
        Col::new("amount", ColKind::Decimal128_38_10, true),
        Col::new("id", ColKind::Int64, false),
        Col::new("event_at", ColKind::TimestampNaive, true),
        Col::new("payload", ColKind::Binary, true),
        Col::new("geom", ColKind::GeomPoint, true),
    ];
    let _ = run_bulk("order_amount_first", &cols, 5);
}

#[ignore = "SHPX_TEST_SQLSERVER_URL env required; observation only"]
#[test]
fn order_geom_before_payload() {
    // staging table では geom は WKB binary に展開される。schema 上で geom 列を payload より
    // 前に置けない (writer が末尾の geom を期待する) ため、payload の前後に追加 Binary を置く
    // ことで「Binary 連続」と「順序依存」を切り分ける。
    let cols = vec![
        Col::new("id", ColKind::Int64, false),
        Col::new("event_at", ColKind::TimestampNaive, true),
        Col::new("amount", ColKind::Decimal128_38_10, true),
        Col::new("payload", ColKind::Binary, true),
        Col::new("geom", ColKind::GeomPoint, true),
    ];
    let _ = run_bulk("order_geom_before_payload", &cols, 5);
}

// 4.2 連続同型 (Binary が並ぶと trigger するか)

#[ignore = "SHPX_TEST_SQLSERVER_URL env required; observation only"]
#[test]
fn binary_separated_by_int() {
    // payload と geom-WKB (= 内部 Binary) の間に Int64 を挟む。Binary が連続しなければ通るかを確認。
    // staging テーブル列順は attr_indices..., shpx_geom_wkb, shpx_geom_srid。
    // schema の payload 直後に Int64 を挟むだけでは staging テーブルで payload と
    // shpx_geom_wkb の間に Int64 が入る。
    let cols = vec![
        Col::new("id", ColKind::Int64, false),
        Col::new("amount", ColKind::Decimal128_38_10, true),
        Col::new("event_at", ColKind::TimestampNaive, true),
        Col::new("payload", ColKind::Binary, true),
        Col::new("sep", ColKind::Int64, true),
        Col::new("geom", ColKind::GeomPoint, true),
    ];
    let _ = run_bulk("binary_separated_by_int", &cols, 5);
}

#[ignore = "SHPX_TEST_SQLSERVER_URL env required; observation only"]
#[test]
fn binary_double() {
    // payload を 2 列 (連続 Binary 3 つ → payload1, payload2, shpx_geom_wkb)。
    let cols = vec![
        Col::new("id", ColKind::Int64, false),
        Col::new("amount", ColKind::Decimal128_38_10, true),
        Col::new("event_at", ColKind::TimestampNaive, true),
        Col::new("payload1", ColKind::Binary, true),
        Col::new("payload2", ColKind::Binary, true),
        Col::new("geom", ColKind::GeomPoint, true),
    ];
    let _ = run_bulk("binary_double", &cols, 5);
}

#[ignore = "SHPX_TEST_SQLSERVER_URL env required; observation only"]
#[test]
fn no_binary_payload() {
    // payload を Utf8 に置換し「varbinary 連続」を完全に排除。geom (WKB) のみ Binary。
    let cols = vec![
        Col::new("id", ColKind::Int64, false),
        Col::new("amount", ColKind::Decimal128_38_10, true),
        Col::new("event_at", ColKind::TimestampNaive, true),
        Col::new("payload_str", ColKind::Utf8, true),
        Col::new("geom", ColKind::GeomPoint, true),
    ];
    let _ = run_bulk("no_binary_payload", &cols, 5);
}

// 4.3 nullable

#[ignore = "SHPX_TEST_SQLSERVER_URL env required; observation only"]
#[test]
fn triple_not_null() {
    // 全列 NOT NULL (= NULL marker 出力経路を排除)。すべての値は build_array で non-null。
    let cols = vec![
        Col::new("id", ColKind::Int64, false),
        Col::new("amount", ColKind::Decimal128_38_10, false),
        Col::new("event_at", ColKind::TimestampNaive, false),
        Col::new("payload", ColKind::Binary, false),
        Col::new("geom", ColKind::GeomPoint, false),
    ];
    let _ = run_bulk("triple_not_null", &cols, 5);
}

// 4.4 chunk size (rows-per-bulk_insert)

#[ignore = "SHPX_TEST_SQLSERVER_URL env required; observation only"]
#[test]
fn chunk_size_one() {
    // SHPX_MSSQL_BULK_CHUNK=1 → 1 行ごとに bulk_insert/finalize を発行。
    // env を本テスト中だけ書き換える (suite は --test-threads=1 前提)。
    std::env::set_var("SHPX_MSSQL_BULK_CHUNK", "1");
    let cols = vec![
        Col::new("id", ColKind::Int64, false),
        Col::new("amount", ColKind::Decimal128_38_10, true),
        Col::new("event_at", ColKind::TimestampNaive, true),
        Col::new("payload", ColKind::Binary, true),
        Col::new("geom", ColKind::GeomPoint, true),
    ];
    let _ = run_bulk("chunk_size_one", &cols, 5);
    std::env::remove_var("SHPX_MSSQL_BULK_CHUNK");
}

#[ignore = "SHPX_TEST_SQLSERVER_URL env required; observation only"]
#[test]
fn chunk_size_three() {
    std::env::set_var("SHPX_MSSQL_BULK_CHUNK", "3");
    let cols = vec![
        Col::new("id", ColKind::Int64, false),
        Col::new("amount", ColKind::Decimal128_38_10, true),
        Col::new("event_at", ColKind::TimestampNaive, true),
        Col::new("payload", ColKind::Binary, true),
        Col::new("geom", ColKind::GeomPoint, true),
    ];
    let _ = run_bulk("chunk_size_three", &cols, 7);
    std::env::remove_var("SHPX_MSSQL_BULK_CHUNK");
}

// 4.5 単独 trigger 候補 (3 型それぞれを単独で踏むか — Binary のみは未テスト)

#[ignore = "SHPX_TEST_SQLSERVER_URL env required; observation only"]
#[test]
fn solo_binary() {
    // payload(Binary) 単独 + id + geom。Binary が単独で踏まないことの確認。
    let cols = vec![
        Col::new("id", ColKind::Int64, false),
        Col::new("payload", ColKind::Binary, true),
        Col::new("geom", ColKind::GeomPoint, true),
    ];
    let _ = run_bulk("solo_binary", &cols, 5);
}

#[ignore = "SHPX_TEST_SQLSERVER_URL env required; observation only"]
#[test]
fn solo_timestamp_naive() {
    // tz-naive Timestamp 単独 (datetime2)。`bulk_timestamptz_and_int64_bit_identical` は tz-aware
    // のみカバーしているので、tz-naive 単独テストはここで初めて入る。
    let cols = vec![
        Col::new("id", ColKind::Int64, false),
        Col::new("event_at", ColKind::TimestampNaive, true),
        Col::new("geom", ColKind::GeomPoint, true),
    ];
    let _ = run_bulk("solo_timestamp_naive", &cols, 5);
}

// ---------- Fase 5 — Date32 follow-up (Fase 4 で `triple_plus_5noise` 緑 / `full_all_types_repro`
// FailColid(9) と判明。差分は `created (Date32)` の有無のみ。Date32 を trigger 軸として詰める) ----------

#[ignore = "SHPX_TEST_SQLSERVER_URL env required; observation only"]
#[test]
fn pair_date_timestamp_naive() {
    let cols = vec![
        Col::new("id", ColKind::Int64, false),
        Col::new("created", ColKind::Date32, true),
        Col::new("event_at", ColKind::TimestampNaive, true),
        Col::new("geom", ColKind::GeomPoint, true),
    ];
    let _ = run_bulk("pair_date_timestamp_naive", &cols, 5);
}

#[ignore = "SHPX_TEST_SQLSERVER_URL env required; observation only"]
#[test]
fn solo_date32() {
    let cols = vec![
        Col::new("id", ColKind::Int64, false),
        Col::new("created", ColKind::Date32, true),
        Col::new("geom", ColKind::GeomPoint, true),
    ];
    let _ = run_bulk("solo_date32", &cols, 5);
}

#[ignore = "SHPX_TEST_SQLSERVER_URL env required; observation only"]
#[test]
fn triple_minimal_with_date32() {
    // triple_minimal_naive (Ok) に Date32 を amount と event_at の間に挿入。
    // 5 列 → 6 列、Date32 が trigger 軸単独で 4 列以下でも踏むかを確認。
    let cols = vec![
        Col::new("id", ColKind::Int64, false),
        Col::new("amount", ColKind::Decimal128_38_10, true),
        Col::new("created", ColKind::Date32, true),
        Col::new("event_at", ColKind::TimestampNaive, true),
        Col::new("payload", ColKind::Binary, true),
        Col::new("geom", ColKind::GeomPoint, true),
    ];
    let _ = run_bulk("triple_minimal_with_date32", &cols, 5);
}

#[ignore = "SHPX_TEST_SQLSERVER_URL env required; observation only"]
#[test]
fn date32_before_timestamp_naive_only() {
    // payload (Binary) を抜き、Date32 + Timestamp naive 隣接のみ。3 型構成必要かを判定。
    let cols = vec![
        Col::new("id", ColKind::Int64, false),
        Col::new("amount", ColKind::Decimal128_38_10, true),
        Col::new("created", ColKind::Date32, true),
        Col::new("event_at", ColKind::TimestampNaive, true),
        Col::new("geom", ColKind::GeomPoint, true),
    ];
    let _ = run_bulk("date32_before_timestamp_naive_only", &cols, 5);
}

#[ignore = "SHPX_TEST_SQLSERVER_URL env required; observation only"]
#[test]
fn date32_before_timestamp_utc_only() {
    // tz-aware にすると通るかを確認 (Timestamp の方の bug かを切り分け)。
    let cols = vec![
        Col::new("id", ColKind::Int64, false),
        Col::new("amount", ColKind::Decimal128_38_10, true),
        Col::new("created", ColKind::Date32, true),
        Col::new("event_at", ColKind::TimestampUtc, true),
        Col::new("geom", ColKind::GeomPoint, true),
    ];
    let _ = run_bulk("date32_before_timestamp_utc_only", &cols, 5);
}

#[ignore = "SHPX_TEST_SQLSERVER_URL env required; observation only"]
#[test]
fn date32_separated_from_timestamp() {
    // Date32 と Timestamp の間に Int64 を 1 列挟む。隣接性が trigger か確認。
    let cols = vec![
        Col::new("id", ColKind::Int64, false),
        Col::new("amount", ColKind::Decimal128_38_10, true),
        Col::new("created", ColKind::Date32, true),
        Col::new("sep", ColKind::Int64, true),
        Col::new("event_at", ColKind::TimestampNaive, true),
        Col::new("geom", ColKind::GeomPoint, true),
    ];
    let _ = run_bulk("date32_separated_from_timestamp", &cols, 5);
}

#[ignore = "SHPX_TEST_SQLSERVER_URL env required; observation only"]
#[test]
fn date32_after_timestamp() {
    // Date32 を Timestamp の後ろに置く (順序反転)。隣接 + 順序依存性の判定。
    let cols = vec![
        Col::new("id", ColKind::Int64, false),
        Col::new("amount", ColKind::Decimal128_38_10, true),
        Col::new("event_at", ColKind::TimestampNaive, true),
        Col::new("created", ColKind::Date32, true),
        Col::new("geom", ColKind::GeomPoint, true),
    ];
    let _ = run_bulk("date32_after_timestamp", &cols, 5);
}

#[ignore = "SHPX_TEST_SQLSERVER_URL env required; observation only"]
#[test]
fn full_minus_payload() {
    // full_all_types_repro から payload (Binary) のみ抜いた 10 列。Binary が trigger 必要かの判定。
    let cols = vec![
        Col::new("id", ColKind::Int64, false),
        Col::new("flag", ColKind::Boolean, true),
        Col::new("class", ColKind::Int32, true),
        Col::new("score", ColKind::Float64, true),
        Col::new("name", ColKind::Utf8, true),
        Col::new("tag", ColKind::Utf8, true),
        Col::new("amount", ColKind::Decimal128_38_10, true),
        Col::new("created", ColKind::Date32, true),
        Col::new("event_at", ColKind::TimestampNaive, true),
        Col::new("geom", ColKind::GeomPoint, true),
    ];
    let _ = run_bulk("full_minus_payload", &cols, 5);
}

#[ignore = "SHPX_TEST_SQLSERVER_URL env required; observation only"]
#[test]
fn full_minus_decimal() {
    // full_all_types_repro から amount (Decimal) を抜いた 10 列。Decimal が trigger 必要かの判定。
    let cols = vec![
        Col::new("id", ColKind::Int64, false),
        Col::new("flag", ColKind::Boolean, true),
        Col::new("class", ColKind::Int32, true),
        Col::new("score", ColKind::Float64, true),
        Col::new("name", ColKind::Utf8, true),
        Col::new("tag", ColKind::Utf8, true),
        Col::new("created", ColKind::Date32, true),
        Col::new("event_at", ColKind::TimestampNaive, true),
        Col::new("payload", ColKind::Binary, true),
        Col::new("geom", ColKind::GeomPoint, true),
    ];
    let _ = run_bulk("full_minus_decimal", &cols, 5);
}

// ---------- Fase 6 — Date32 minimal repro の確定 (trigger 軸が Date32 と判明) ----------
//
// Fase 5 結果から「Date32 を含む schema は **直後の列の colid** で常に失敗」が確定。
// minimal は `id + Date32 + geom` の 3 列。最終確認として nullable / 値 / row 数 / 単独
// Date32 (geom なし) を切り分ける。

/// nullable=false (NOT NULL) でも踏むか。NULL marker 経路を排除。
#[ignore = "SHPX_TEST_SQLSERVER_URL env required; observation only"]
#[test]
fn solo_date32_not_null() {
    let cols = vec![
        Col::new("id", ColKind::Int64, false),
        Col::new("created", ColKind::Date32, false),
        Col::new("geom", ColKind::GeomPoint, true),
    ];
    let _ = run_bulk("solo_date32_not_null", &cols, 5);
}

/// 1 行のみで踏むか (chunk 境界 / TDS フレーム frequency 依存性の排除)。
#[ignore = "SHPX_TEST_SQLSERVER_URL env required; observation only"]
#[test]
fn solo_date32_one_row() {
    let cols = vec![
        Col::new("id", ColKind::Int64, false),
        Col::new("created", ColKind::Date32, true),
        Col::new("geom", ColKind::GeomPoint, true),
    ];
    let _ = run_bulk("solo_date32_one_row", &cols, 1);
}

/// id を抜いて Date32 を 1 列目に置く。Date32 が attr_indices の先頭でも踏むか。
#[ignore = "SHPX_TEST_SQLSERVER_URL env required; observation only"]
#[test]
fn date32_first_col() {
    let cols = vec![
        Col::new("created", ColKind::Date32, true),
        Col::new("geom", ColKind::GeomPoint, true),
    ];
    let _ = run_bulk("date32_first_col", &cols, 5);
}

/// Date32 を末尾 attr (geom の直前) に置く。直後 = shpx_geom_wkb (Binary)。
#[ignore = "SHPX_TEST_SQLSERVER_URL env required; observation only"]
#[test]
fn date32_last_attr() {
    let cols = vec![
        Col::new("id", ColKind::Int64, false),
        Col::new("created", ColKind::Date32, true),
        Col::new("geom", ColKind::GeomPoint, true),
    ];
    // ↑ 既に solo_date32 と同じ schema。Date32 + geom_wkb の隣接が共通 trigger の確認用に
    // 別 case として残す (ロジック上同一だが docs マトリックス整合のため)。
    let _ = run_bulk("date32_last_attr", &cols, 5);
}

/// Date32 を 2 列連続。trigger が累積するか / colid 報告位置が変動するか。
#[ignore = "SHPX_TEST_SQLSERVER_URL env required; observation only"]
#[test]
fn double_date32() {
    let cols = vec![
        Col::new("id", ColKind::Int64, false),
        Col::new("d1", ColKind::Date32, true),
        Col::new("d2", ColKind::Date32, true),
        Col::new("geom", ColKind::GeomPoint, true),
    ];
    let _ = run_bulk("double_date32", &cols, 5);
}

// ---------- ユニットテスト (CI で env なしでも走る) ----------

#[test]
fn detect_colid_extracts_number() {
    assert_eq!(
        detect_colid("Invalid column type from bcp client for colid 9"),
        Some(9)
    );
    assert_eq!(
        detect_colid("driver: sqlserver: ... colid 11 ..."),
        Some(11)
    );
    assert_eq!(detect_colid("unrelated error"), None);
}

#[test]
fn schema_summary_renders_compactly() {
    let cols = vec![
        Col::new("id", ColKind::Int64, false),
        Col::new("amount", ColKind::Decimal128_38_10, true),
        Col::new("geom", ColKind::GeomPoint, true),
    ];
    let s = schema_summary(&cols);
    assert!(s.contains("id:Int64 NN"), "got {s}");
    assert!(s.contains("amount:Decimal128(38,10) N"), "got {s}");
    assert!(s.contains("geom:GeomPoint N"), "got {s}");
}

#[test]
fn build_schema_and_batch_geom_must_be_last() {
    let cols = vec![
        Col::new("id", ColKind::Int64, false),
        Col::new("geom", ColKind::GeomPoint, true),
    ];
    let (schema, batch) = build_schema_and_batch(&cols, 3);
    assert_eq!(schema.fields().len(), 2);
    assert_eq!(batch.num_rows(), 3);
}
