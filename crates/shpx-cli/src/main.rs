//! shpx CLI のエントリポイント。

use std::process::ExitCode;

use clap::Parser;
use tracing_subscriber::EnvFilter;

mod cli;
mod commands;
mod registry;

fn main() -> ExitCode {
    let parsed = cli::Cli::parse();
    init_tracing(parsed.verbose);

    let result = match parsed.command {
        cli::Cmd::Convert(args) => commands::convert::run(args),
        cli::Cmd::Info(args) => commands::info::run(args),
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
fn init_tracing(verbose: u8) {
    let default_level = match verbose {
        0 => "warn",
        1 => "info",
        2 => "debug",
        _ => "trace",
    };
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(default_level));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(true)
        .with_writer(std::io::stderr)
        .try_init();
}
