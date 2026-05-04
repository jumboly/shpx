# 派生バイナリで shpx CLI を再利用する

shpx 標準 driver 9 種 (csv / fgb / geojson / gpkg / parquet / postgis / shp / spatialite / sqlserver) に加えて、業務固有 / 社内 driver を 1 本のバイナリで同居させたいケースを想定したガイド。`shpx-cli` は v1.1.0 から lib + bin の二本立て構成で、library として依存することで `shpx_cli::run()` 1 行で標準 CLI 全体を再利用できる。

## 動機

- shpx 本体に業務固有 driver を持ち込みたくない (公開リポジトリに社内ロジックを混ぜない)。
- 標準 9 driver を派生側で抱え直したくない (driver 増減のたびに追従が必要になる)。
- main.rs / registry.rs を派生側にコピペして並行保守したくない。

`shpx_cli::run()` を呼ぶだけで上記 3 つを同時に解決できる。

## 最小レシピ

派生 crate の `Cargo.toml`:

```toml
[package]
name    = "my-shpx"
version = "0.1.0"
edition = "2021"

[dependencies]
shpx-cli        = "1.1"
my-extra-driver = { path = "../my-extra-driver" }  # 自前 driver crate
```

派生 crate の `src/main.rs`:

```rust
//! `shpx_cli::run()` は標準 9 driver を含む全サブコマンド (convert / info /
//! schema / drivers) を提供する。`use my_extra_driver as _;` の 1 行で
//! `inventory::submit!` 経由の自前 driver も自動的に登録される。

use my_extra_driver as _;

fn main() -> std::process::ExitCode {
    shpx_cli::run()
}
```

これだけで `my-shpx drivers` に標準 9 + 自前 driver が並ぶ。

## 仕組み

1. **inventory による自動収集**: 各 driver crate は `shpx_core::inventory::submit!` で `DriverRegistration` を提出する。`shpx_cli::run()` 内部の `inventory::iter` がリンクされている全 crate のエントリを集約する。

2. **標準 9 driver の linker pin**: `shpx-cli` lib 内部の `registry` モジュールが `use shpx_driver_X as _;` で 9 crate を `extern crate` 経由で参照しているため、派生 bin が `shpx-cli` を依存に入れるだけで 9 crate も rlib 経由でリンクされる。派生側で 9 crate を個別に `[dependencies]` に並べる必要はない。

3. **追加 driver の linker pin**: 派生 crate 自身の main.rs で `use my_extra_driver as _;` を宣言すれば、派生 bin がその driver crate を直接参照することになり inventory に積まれる。

4. **CLI 表示名の自動切替**: `shpx_cli::run()` は `argv[0]` の file_stem を `clap::Command::name` / `bin_name` に注入する。`my-shpx --help` の `Usage:` 行は `my-shpx` と表示される。表示名を argv\[0\] と独立に固定したい場合は `shpx_cli::run_with_app_name("固定名")` を使う。

## 公開 API

```rust
// crates/shpx-cli/src/lib.rs
pub fn run() -> std::process::ExitCode;
pub fn run_with_app_name(app_name: &str) -> std::process::ExitCode;
```

`run()` で十分なケースが大半。symlink 経由の起動など argv\[0\] が予測しづらい状況で表示名を固定したい場合のみ `run_with_app_name` を使う。

## 動作確認用 example

リポジトリ同梱の `crates/shpx-cli/examples/embedded.rs` が最小派生バイナリの形:

```bash
cargo run --example embedded -p shpx-cli -- drivers
cargo run --example embedded -p shpx-cli -- --help   # "Usage: embedded [...]"
```

このサンプルは追加 driver を持たないが、派生 crate と同じビルド経路を踏むため、`shpx_cli::run` の API 契約 / argv\[0\] 由来の表示名切替 / inventory rlib pin がすべて smoke test として CI で検証される。

## 制限事項

- **`--version` 表示**: 現状 `shpx-cli` の `CARGO_PKG_VERSION` を返す。派生 bin 独自バージョンを表示したい場合は将来 `run_with_app_name_and_version` を追加予定 (v1.1.0 では未実装)。
- **`about` 文字列**: 派生 bin でも `shpx-cli` 側の about (標準 9 driver の列挙) が表示される。これも将来 override API を追加する余地あり。
- **同一 scheme の衝突**: 派生 driver と標準 driver が同じ scheme (例: `csv`) を宣言した場合、`select_driver` は `name()` のアルファベット順で先勝ちする (`registry.rs` の `sort_by_key`)。派生側で名前を `aaa-csv` のように接頭辞付けるか、独自 scheme (例: `mycsv`) を使うのが安全。
- **Rust ABI 互換**: `shpx-cli` を依存に取る派生 crate は同じ rustc / 同じ feature flag でビルドする必要がある。`cargo` 経由なら自動。事前ビルド済みの shpx-cli rlib をバイナリ配布して dynamic link する用途は想定外。

## 関連ドキュメント

- [`docs/CONTRIBUTING.md`](CONTRIBUTING.md) — 新しい driver crate の作り方 (`Driver` trait の実装、`inventory::submit!` の書き方)。派生 driver 自体を書く際にはこちらが先。
- [`docs/DESIGN.md`](DESIGN.md) — driver registry の設計判断と inventory 採用理由。
