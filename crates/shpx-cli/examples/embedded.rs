//! 派生バイナリの最小例。
//!
//! `shpx_cli::run()` を呼ぶだけで標準 driver 9 種類が利用できる。追加 driver を
//! 載せる場合は `[dependencies]` に追加 driver crate を入れ、ここに
//! `use my_extra_driver as _;` を 1 行書くだけで `inventory` 経由で登録される。
//!
//! 動作確認:
//! ```text
//! cargo run --example embedded -p shpx-cli -- drivers
//! ```
//!
//! `--help` の "Usage:" 行は argv\[0\] の file_stem (= `embedded`) になり、
//! `shpx_cli::run` が argv\[0\] 起点で CLI 表示名を切り替える経路の smoke test
//! も兼ねている。

fn main() -> std::process::ExitCode {
    shpx_cli::run()
}
