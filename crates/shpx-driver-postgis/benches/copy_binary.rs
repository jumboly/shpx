//! v0.3 cycle 3c: PostGIS COPY BINARY 経路の criterion ベンチ。
//!
//! `SHPX_TEST_PG_URL` が設定されているときのみ実 DB に書き込む。未設定なら eprintln
//! で skip して exit 0（ローカル/CI どちらでも `cargo bench --no-run` が緑になる）。
//!
//! 行数は環境変数 `SHPX_BENCH_ROWS` で切替（default 100k = smoke、CI workflow_dispatch
//! では 10M を渡す）。1 イテレーションが数十秒〜数分のため `sample_size(10)` 必須。
//!
//! 計測対象は `bulk_write` + `finish`（COPY BINARY 全フェーズ）。Parquet 読み込みも
//! ループ内に含まれるが、`StreamReader` 部分の純粋オーバーヘッドは PostGIS 書き込みに
//! 比べて十分小さい想定。ogr2ogr 比較も同じ Parquet を入力にするため公平性は保たれる。

mod gen;

use std::env;
use std::path::{Path, PathBuf};
use std::time::Duration;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use shpx_core::{Crs, Driver, ReadOpts, Uri, WriteOpts};
use shpx_driver_parquet::ParquetDriver;
use shpx_driver_postgis::{conn, PostgisDriver};

fn pg_url() -> Option<String> {
    env::var("SHPX_TEST_PG_URL").ok().filter(|s| !s.is_empty())
}

/// `target/bench-data/` を返す。`CARGO_MANIFEST_DIR` (= `crates/shpx-driver-postgis`)
/// から 2 段親の workspace ルート → `target/bench-data` に解決する。
fn bench_data_dir() -> PathBuf {
    let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace = crate_dir
        .parent()
        .and_then(Path::parent)
        .expect("crate_dir has 2 ancestors");
    workspace.join("target").join("bench-data")
}

fn bench_pg_write(c: &mut Criterion) {
    let Some(url) = pg_url() else {
        eprintln!("SHPX_TEST_PG_URL unset; skipping postgis bench");
        return;
    };

    // smoke: 100k、リリース計測: 10M（ROADMAP の完了基準）。
    let rows: usize = env::var("SHPX_BENCH_ROWS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(100_000);

    let dir = bench_data_dir();
    let input = gen::ensure_parquet(rows, &dir);

    let mut group = c.benchmark_group("postgis_bulk");
    group.sample_size(10);
    group.warm_up_time(Duration::from_secs(5));
    group.measurement_time(Duration::from_secs(900));
    group.throughput(Throughput::Elements(
        u64::try_from(rows).expect("rows<=u64"),
    ));

    group.bench_with_input(
        BenchmarkId::new("copy_binary", rows),
        &(url.clone(), input.clone(), rows),
        |b, (url, input, rows)| {
            b.iter(|| run_bulk_write(url, input, *rows));
        },
    );

    group.finish();

    // bench 終了時にテーブルを掃除する（次回 bench / 統合テストで残骸が衝突しないように）。
    drop_bench_table(&url, rows);
}

fn run_bulk_write(url: &str, input: &Path, rows: usize) {
    let driver = PostgisDriver::new();
    let table = bench_table_name(rows);
    drop_bench_table(url, rows);

    let pg_uri = make_pg_uri(url, &table);

    let parquet = ParquetDriver;
    let parquet_uri = Uri::from_path(input.display().to_string());
    let mut reader = parquet
        .open_read(&parquet_uri, &ReadOpts::default())
        .expect("parquet open_read");
    let schema = reader.schema();

    let opts = WriteOpts {
        overwrite: true,
        ..Default::default()
    };
    let mut bulk = driver
        .open_bulk_write(&pg_uri, schema, Some(Crs::from_epsg(4326)), &opts)
        .expect("open_bulk_write")
        .expect("bulk_load=true");
    let mut iter = reader.batches();
    bulk.bulk_write(&mut iter).expect("bulk_write");
    bulk.finish().expect("bulk finish");
}

fn bench_table_name(rows: usize) -> String {
    format!("shpx_bench_pg_{rows}")
}

fn make_pg_uri(url: &str, table: &str) -> Uri {
    let sep = if url.contains('?') { '&' } else { '?' };
    Uri::from_path(format!("{url}{sep}table={table}"))
}

fn drop_bench_table(url: &str, rows: usize) {
    let Ok(client) = conn::connect(url) else {
        return;
    };
    let table = bench_table_name(rows);
    let _ = conn::batch_execute(
        &client,
        &format!("DROP TABLE IF EXISTS \"public\".\"{table}\""),
    );
}

criterion_group!(benches, bench_pg_write);
criterion_main!(benches);
