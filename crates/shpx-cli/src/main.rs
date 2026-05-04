//! shpx CLI のバイナリエントリポイント。
//!
//! 実体は `shpx_cli::run()` (lib 側) に集約されている。派生バイナリも
//! 同じエントリを再利用できる構造のため、ここは薄い shim に保つ。

fn main() -> std::process::ExitCode {
    shpx_cli::run()
}
