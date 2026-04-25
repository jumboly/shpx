# Contributing

shpx は v1 時点で「ソース拡張プラグイン」方式を採用する。新フォーマットを追加するには、`Driver` trait を実装した crate を workspace に追加し、`shpx-cli` で feature flag を有効化して再ビルドする。

## 新しい Driver の追加手順

### 1. Crate を作成

```bash
cargo new --lib crates/shpx-driver-<format>
```

`Cargo.toml`:

```toml
[package]
name = "shpx-driver-<format>"
version = "0.1.0"
edition = "2021"

[dependencies]
shpx-core = { path = "../shpx-core" }
shpx-geom = { path = "../shpx-geom" }
arrow = { workspace = true }
# 必要なフォーマット固有依存をここに
```

### 2. Driver trait を実装

```rust
use shpx_core::{
    Driver, LayerReader, LayerWriter, Capabilities, Crs, Result, Uri,
    ReadOpts, WriteOpts, SchemaRef, StringEncoding,
};

pub struct MyDriver;

impl Driver for MyDriver {
    fn name(&self) -> &'static str { "myformat" }

    fn supported_schemes(&self) -> &[&'static str] { &["myfmt"] }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            read: true,
            write: true,
            random_access: false,
            bulk_load: false,
            supports_blob: true,
            supports_decimal: true,
            supports_timestamp_tz: true,
            string_encoding: StringEncoding::Fixed("utf-8"),
            max_decimal_precision: Some(38),
        }
    }

    fn open_read(&self, uri: &Uri, opts: &ReadOpts) -> Result<Box<dyn LayerReader>> {
        // ...
    }

    fn open_write(
        &self,
        uri: &Uri,
        schema: SchemaRef,
        crs: Option<Crs>,
        opts: &WriteOpts,
    ) -> Result<Box<dyn LayerWriter>> {
        // ...
    }
}

shpx_core::inventory::submit! {
    Box::new(MyDriver) as Box<dyn Driver>
}
```

`inventory::submit!` により、CLI 起動時に `Driver` レジストリへ自動登録される。

### 3. Bulk load 対応（オプション）

巨大データ向けに最適化したい場合は `BulkLoadWriter` も実装する:

```rust
impl BulkLoadWriter for MyWriter {
    fn bulk_write(
        &mut self,
        batches: &mut dyn Iterator<Item = Result<RecordBatch>>,
    ) -> Result<()> {
        // フォーマット固有のバルク投入パス（例: PostgreSQL COPY、SQL Server bulk_insert）
    }
}
```

`Capabilities::bulk_load = true` を返すと、コアパイプラインが bulk path を選択する。

### 4. shpx-cli への組み込み

`crates/shpx-cli/Cargo.toml`:

```toml
[features]
default = ["shp", "gpkg", "parquet", "geojson", "csv", "fgb", "postgis", "sqlserver", "spatialite"]
myformat = ["dep:shpx-driver-myformat"]

[dependencies]
shpx-driver-myformat = { path = "../shpx-driver-myformat", optional = true }
```

ビルド:

```bash
cargo build --release --features myformat
```

`shpx drivers` で登録されているか確認できる。

## ジオメトリの扱い

中間表現は **WKB**（Well-Known Binary）。`shpx-geom` の `wkb::encode` / `wkb::decode` を使う。

ジオメトリは Arrow `Binary` 列で、列のメタデータに `"shpx:geometry"` キーで JSON を持たせる。詳細は [DESIGN.md](DESIGN.md) と [DATA_TYPES.md](DATA_TYPES.md)。

## CRS の扱い

`Crs` 構造体（`shpx-geom::Crs`）を使う。詳細は [CRS.md](CRS.md)。

## テスト

各 driver は以下のテストを最低限備えること:

- `tests/roundtrip_<format>.rs`: そのフォーマット単体の read → write → read で値・順序が一致
- `tests/cross_<format>.rs`: 別フォーマットとの相互変換で代表的な型が無損失（または既知の損失パターンに合致）
- 大きいデータの fixture は `tests/fixtures/` に置く（公開可能なもののみ）。著作権付きデータは置かない

## コーディング規約

- `cargo fmt` / `cargo clippy --workspace -- -D warnings` を pass する
- public API には rustdoc を書く（短くて構わないが、空白は不可）
- `unsafe` を使う場合は SAFETY コメント必須
- エラー型は `thiserror` ベースの `shpx_core::Error`

## コミット

- 小さな commit を積む（1 commit = 1 論理変更）
- メッセージは `<scope>: <imperative>` 形式（例: `driver-shp: support cpg encoding`）
- マイルストーン完了時は `vX.Y` の git tag を打つ
