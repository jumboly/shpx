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
use arrow_schema::SchemaRef;
use shpx_core::{
    Capabilities, Crs, Driver, DriverRegistration, LayerReader, LayerWriter,
    ReadOpts, Result, StringEncoding, Uri, WriteOpts,
};

#[derive(Debug, Default, Clone, Copy)]
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

// Driver 本体は `static` に置き、`&'static dyn Driver` として登録する
// （`Box::new` は使わない — アロケーション無し）。
static MY_DRIVER_INSTANCE: MyDriver = MyDriver;
shpx_core::inventory::submit! {
    DriverRegistration { driver: &MY_DRIVER_INSTANCE }
}
```

`inventory::submit!` により、CLI 起動時に `Driver` レジストリへ自動登録される。レジストリは `name()` 順で sort され、リンカ順に依存しない決定的な解決順を持つ。

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

`crates/shpx-cli/Cargo.toml` の `[dependencies]` に新 driver crate を追加する:

```toml
[dependencies]
shpx-driver-myformat = { path = "../shpx-driver-myformat" }
```

加えて `crates/shpx-cli/src/registry.rs` 末尾の `use _` ブロックに 1 行加える:

```rust
use shpx_driver_myformat as _;
```

これは `inventory::submit!` の副作用（自動登録）を起こすために driver crate を
リンカに保持させるためのもので、実体は何もインポートしない。`Cargo.toml` への
dep 追加だけではリンカが未参照と判断して crate ごと strip してしまう。

ビルド:

```bash
cargo build --release
```

`shpx drivers` で登録されているか確認できる（capabilities 一覧も同時に表示される）。

> Note: v0.2 時点では feature flag による driver 取捨選択は採用していない。
> リリースバイナリのサイズが問題になった段階で `[features]` 化を検討する。

## RDB driver を追加する場合

PostGIS / SQL Server / SpatiaLite の 3 driver で重複していたヘルパーは `shpx-rdb-common` crate
に集約してある。新しい RDB driver (例: MySQL) を追加するときは `shpx-driver-postgis` /
`shpx-driver-sqlserver` / `shpx-driver-spatialite` の `options.rs` / `util.rs` /
`writer.rs` を参考にしつつ、以下を `shpx-rdb-common` から再利用する:

| 機能 | API |
|---|---|
| `?key=value` の percent decode | `shpx_rdb_common::percent_decode` / `query_pairs` / `query_get` |
| `schema.name` 分割 | `shpx_rdb_common::split_qualified(s, default_schema)` |
| `?table=` / 環境変数 fallback | `shpx_rdb_common::resolve_table_name(query_table, env_var, driver_name)` |
| `--overwrite + create_table=Never` reject | `shpx_rdb_common::validate_overwrite_compat(opts, driver_name)` |
| `OnLoss` ポリシー適用 | `shpx_rdb_common::apply_on_loss(kind, field, on_loss, warn_fn)` |
| `--src-crs` / schema CRS マージ | `shpx_rdb_common::merge_crs(crs_arg, geom_meta)` |
| EPSG → i32 SRID | `shpx_rdb_common::resolve_epsg_srid(crs)` |
| `Error::Driver` 構築 | `shpx_rdb_common::driver_err(name, e)` / `driver_msg(name, msg)` |
| Arrow primitive 取り出し | `shpx_rdb_common::primitive::<T>(array, row)` |

driver 固有のまま残すもの: SQL 識別子クオート (`quote_ident`)、文字列リテラル
(`quote_literal`)、catalog クエリ (`describe_columns` 相当)、`build_create_table_sql` /
`build_insert_sql`、tracing target、ジオメトリ型判定 (`is_geometry_typname` 相当)。

ファイルベースの SQLite driver (GPKG / SpatiaLite) を新規に追加する場合の差分:

- SQLite には schema 概念が無いため `split_qualified` / `resolve_table_name` (schema 修飾の解決) は呼ばない。テーブル名は単一識別子として扱う
- 接続先はネットワーク URL ではなくファイルパスのため、`?table=` のクエリ部分を URI から切り出す `strip_to_filepath` 相当のヘルパーが必要 (PostGIS / SQL Server には不要)
- `validate_overwrite_compat` / `apply_on_loss` / `merge_crs` / `resolve_epsg_srid` / `primitive` は他 RDB driver と同じく再利用する
- `inventory::submit!` で登録する `supported_schemes` は複数挙げてよい (例: SpatiaLite は `["sqlite", "db", "spatialite"]`)。`*.gpkg` 等の他 driver と排他になる文字列は重ねない

`apply_on_loss` の `warn_fn` には driver 固有 tracing target を含めたクロージャを渡す:

```rust
shpx_rdb_common::apply_on_loss(kind, field, on_loss, || {
    tracing::warn!(target: "shpx::myrdb", kind, field, "lossy conversion");
})?;
```

`tracing::warn!(target: ...)` の target はマクロ展開時の const 要求があるため、
クロージャごと driver 側に置く設計にしている。

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
