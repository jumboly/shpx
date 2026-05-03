//! shpx CLI のエントリポイント。

use std::process::ExitCode;

use clap::Parser;
use tracing_subscriber::EnvFilter;

mod cli;
mod commands;
mod registry;

fn main() -> ExitCode {
    let parsed = cli::Cli::parse();
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
