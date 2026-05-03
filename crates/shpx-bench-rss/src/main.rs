//! shpx-bench-rss — v0.8 reader streaming peak RSS bench harness。
//!
//! 1 プロセス 1 計測。`/proc/self/status::VmHWM` (高水位 RSS) はリセット不可なため、
//! driver × rows の組ごとに本バイナリを起動して JSON 1 行を吐く運用にする。CI workflow
//! (`.github/workflows/bench-peak-rss.yml`) では prepare phase と read phase を別
//! process で実行することで、prepare 段階の writer メモリ確保が reader 計測値に
//! 混入しないようにしている (`--prepare-only` → `--read-only` の 2 step)。

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use shpx_core::Result;

mod data;
mod drivers;
mod runner;

#[derive(Parser, Debug)]
#[command(
    name = "shpx-bench-rss",
    about = "v0.8 reader streaming peak RSS bench harness",
    version
)]
struct Args {
    /// 対象 driver (`parquet` / `shp` / `fgb` / `csv` / `geojson-fc` / `geojson-ndjson` /
    /// `gpkg` / `spatialite` / `postgis` / `sqlserver`)。
    #[arg(long, value_enum)]
    driver: drivers::DriverKind,

    /// 入力行数。
    #[arg(long)]
    rows: usize,

    /// Parquet 入力 + native format 出力先ディレクトリ。manifest cache はこの中で管理する。
    #[arg(long, default_value = "target/bench-data")]
    input_dir: PathBuf,

    /// 該当 `rows` の Parquet 入力 + 全 driver の native cache (`*_<rows>.MANIFEST` と
    /// `bench_<rows>.parquet`) を破棄して再生成する。driver 別の native ファイル本体は
    /// 次回 prepare で overwrite されるため明示削除しない。
    #[arg(long)]
    reset: bool,

    /// prepare (Parquet 入力 + native format) のみ実行して exit する。
    #[arg(long, conflicts_with = "read_only")]
    prepare_only: bool,

    /// reader 計測のみ実行する。`prepare-only` で事前に cache を作っておく前提
    /// (CI で peak RSS 汚染を避けるための分離実行に必須)。
    #[arg(long, conflicts_with = "prepare_only")]
    read_only: bool,

    /// PostGIS 接続 URL (`pg://user:pass@host:port/db`、table query は付けない)。
    #[arg(long, env = "SHPX_TEST_PG_URL")]
    postgis_url: Option<String>,

    /// SQL Server 接続 URL (`mssql://user:pass@host:port/db`)。
    #[arg(long, env = "SHPX_TEST_SQLSERVER_URL")]
    sqlserver_url: Option<String>,
}

fn main() -> ExitCode {
    let args = Args::parse();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &Args) -> Result<()> {
    let bench = drivers::dispatch(
        args.driver,
        args.postgis_url.as_deref(),
        args.sqlserver_url.as_deref(),
    )?;

    if args.reset {
        sweep_caches(&args.input_dir, args.rows);
    }

    let parquet_input = data::ensure_parquet(args.rows, &args.input_dir);
    let native = bench.prepare(&parquet_input, &args.input_dir, args.rows)?;

    if args.prepare_only {
        return Ok(());
    }

    let mut reader = bench.open_read(&native)?;
    let result = runner::measure_read(bench.name(), args.rows, reader.as_mut())?;

    let json = serde_json::to_string(&result)
        .map_err(|e| shpx_core::Error::Format(format!("serialize result: {e}")))?;
    println!("{json}");
    Ok(())
}

/// `<input_dir>/bench_<rows>.parquet` と `*_<rows>.MANIFEST` を全削除する。
/// driver ごとの native ファイル本体 (gpkg/csv/shp 等) は次回 prepare で overwrite
/// されるため触らない。
fn sweep_caches(dir: &std::path::Path, rows: usize) {
    let parquet = format!("bench_{rows}.parquet");
    let manifest_suffix = format!("_{rows}.MANIFEST");
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let s = name.to_string_lossy();
        if s == parquet || s.ends_with(&manifest_suffix) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}
