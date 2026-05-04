//! shpx CLI のライブラリエントリ。
//!
//! 派生バイナリは `[dependencies] shpx-cli = "1.1"` を追加し、`shpx_cli::run()`
//! を呼ぶだけで標準 driver 9 種類 (csv / fgb / geojson / gpkg / parquet / postgis /
//! shp / spatialite / sqlserver) を含む shpx の全サブコマンドを再利用できる。
//! 派生 crate 側で `use my_extra_driver as _;` を main.rs 等に書いておけば、
//! `inventory` の集約経路で追加 driver も自動的に登録される。
//!
//! ```ignore
//! use my_extra_driver as _;
//!
//! fn main() -> std::process::ExitCode {
//!     shpx_cli::run()
//! }
//! ```

use std::process::ExitCode;

use clap::{CommandFactory, FromArgMatches};
use tracing_subscriber::EnvFilter;

mod cli;
mod commands;
mod registry;

/// argv\[0\] の file_stem を CLI 表示名として用いる標準エントリ。
///
/// 派生バイナリでは多くの場合これを呼べば十分。`my-shpx` という名前で起動された
/// バイナリでは `--help` の "Usage:" 行も `my-shpx` になる。
pub fn run() -> ExitCode {
    let app_name = std::env::args_os()
        .next()
        .and_then(|arg0| {
            std::path::PathBuf::from(&arg0)
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "shpx".to_string());
    run_with_app_name(&app_name)
}

/// CLI 表示名を明示的に指定するエントリ。
///
/// argv\[0\] とは独立に固定したいケース (例: `bun` が node を内製化しているような
/// brand 上の都合) のために用意する。
pub fn run_with_app_name(app_name: &str) -> ExitCode {
    // clap 4.6 の `Command::name` / `bin_name` は `Into<Str>` を要求し、`Str` への
    // `From<String>` が無い (= `&'static str` か `&Str` のみ受け付ける) ため、
    // 起動時 1 回だけ `Box::leak` で `'static` 化する。CLI process は数秒で
    // 終了するためリークは無害。
    let app_name_static: &'static str = Box::leak(Box::<str>::from(app_name));
    let cmd = cli::Cli::command()
        .name(app_name_static)
        .bin_name(app_name_static);
    let matches = cmd.get_matches();
    let parsed = match cli::Cli::from_arg_matches(&matches) {
        Ok(p) => p,
        Err(e) => e.exit(),
    };

    init_tracing(parsed.verbose, parsed.quiet);

    let result = match parsed.command {
        cli::Cmd::Convert(args) => commands::convert::run(&args, parsed.quiet),
        cli::Cmd::Info(args) => commands::info::run(args),
        cli::Cmd::Schema(args) => commands::schema::run(args),
        cli::Cmd::Drivers(args) => commands::drivers::run(&args),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `RUST_LOG` を尊重しつつ、`--verbose` 回数で最低レベルを底上げする。
/// `--quiet` 指定時は `error` まで落とし、自前の info ログ (`shpx::cli` 等) を
/// 出さない。`RUST_LOG` がセットされている場合はそちらを優先するため、
/// `RUST_LOG=...` での明示制御は `--quiet` より強い。
///
/// 派生バイナリが先に独自の tracing subscriber を `init` 済みでも、`try_init`
/// が黙って no-op するため二重初期化で panic しない。
fn init_tracing(verbose: u8, quiet: bool) {
    let default_level = if quiet {
        "error"
    } else {
        match verbose {
            0 => "warn",
            1 => "info",
            2 => "debug",
            _ => "trace",
        }
    };
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_level));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(true)
        .with_writer(std::io::stderr)
        .try_init();
}
