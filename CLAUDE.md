# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## 概要

shpx — GDAL 非依存・Rust 製の空間データ変換 CLI。Shapefile / GeoPackage / GeoParquet / FlatGeobuf / GeoJSON(/Lines) / CSV(WKT) / PostGIS / SQL Server / SpatiaLite を読み書きし、**Arrow `RecordBatch` ストリームを中間表現**として無損失で相互変換する。

## コマンド

すべてリポジトリルートから実行する。toolchain は `stable` 固定（`rust-toolchain.toml`）。MSRV は **1.85**（`Cargo.toml` の `workspace.package.rust-version`）。

```bash
# ビルド / 実行
cargo build --workspace
cargo run -p shpx-cli -- convert examples/data/cities.shp /tmp/cities.parquet

# Lint（CI のゲート。clippy は warning ゼロが必須）
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings

# テスト
cargo test --workspace --locked           # workspace 全体
cargo test -p shpx-driver-shp             # 単一クレート
cargo test -p shpx-driver-shp -- roundtrip    # テスト名フィルタで単一実行
```

### システム依存

- **libproj** は reprojection を伴うビルドで必須（`--reproject`、GeoJSON の自動 WGS84 変換）。`pkg-config` で検出する。macOS: `brew install proj`、Ubuntu: `apt-get install libproj-dev pkg-config`。
- CLI には `bundled-proj`（および `bundled-spatialite`）feature があり、libproj/SQLite（および libgeos）を C ソースから static link して単一バイナリ化する。`cmake` + `clang` が必要。dev ビルドに cmake/clang を要求しないよう、既定では opt-in しない。

### DB 統合テストは環境変数が無いとスキップされる

DB ドライバの統合テストは、接続用の環境変数が無いと `eprintln` + `return` で**黙ってスキップ**する。そのため DB 無しのローカルでも `cargo test --workspace` は通る。実際に走らせるには（CI は `.github/workflows/ci.yml` の `services:` で、ローカルは `docker-compose.yml` で同等環境を提供）以下を設定する:

- `SHPX_TEST_PG_URL` — PostGIS。例 `pg://shpx:shpx@localhost:5432/shpx_test`
- `SHPX_TEST_SQLSERVER_URL` — SQL Server。例 `mssql://sa:<pw>@localhost:1433/shpx_test`
- `SHPX_TEST_SPATIALITE=1`（加えて `SHPX_SPATIALITE_PATH` で `mod_spatialite` を指定）— SpatiaLite

## アーキテクチャ

cargo workspace（`crates/*`）。データフローは単一パイプライン:

```
Reader (Driver) → Iterator<RecordBatch>（lazy・schema 付き）→ CRS 変換 → Writer（bulk または row）
```

- **中間表現**: Arrow `Schema` + `RecordBatch` ストリーム。ジオメトリは **`Binary` 列の WKB** として流し、列メタデータ `"shpx:geometry" = {"encoding":"WKB","crs":{...}}`（GeoParquet 慣習）を持たせる。属性順 / `decimal(p,s)` / timestamp の unit・tz / utf8 はすべて Arrow schema 経由で保全する。
- 全経路が **streaming/lazy** — driver は batch を yield し、パイプラインはデータ全体を materialize しない。driver 別戦略とピーク RSS 予算は `docs/STREAMING.md`。

### クレート構成

| クレート | 役割 |
|---|---|
| `shpx-core` | `Driver`/`LayerReader`/`LayerWriter`/`BulkLoadWriter` trait、`Capabilities`、`Uri`、`Crs`、Arrow schema helper、エラー型 |
| `shpx-geom` | WKB/WKT コーデック、CRS、reprojection（libproj） |
| `shpx-rdb-common` | RDB ドライバ共通ヘルパー（URI パース、on-loss、CRS→SRID） |
| `shpx-driver-*` | Driver 1 つにつき 1 クレート（shp, gpkg, parquet, fgb, geojson, csv, postgis, sqlserver, spatialite）。Driver ≠ Format（SQLite に gpkg / spatialite の 2 Driver）— 用語は `CONTEXT.md` 参照 |
| `shpx-cli` | clap CLI。**lib + bin**（`shpx_cli::run()` を派生バイナリで再利用可能 — `docs/EMBEDDING.md`） |
| `shpx-bench-rss` | ピーク RSS ベンチハーネス |

### Driver 登録（自明でない箇所）

ドライバは `inventory` クレートで起動時に自己登録する。各 driver crate は:

```rust
static MY_DRIVER_INSTANCE: MyDriver = MyDriver;     // &'static、Box/alloc なし
shpx_core::inventory::submit! { DriverRegistration { driver: &MY_DRIVER_INSTANCE } }
```

レジストリは `name()` 順に sort されるため、解決順は **リンカ順に依存せず決定的**。重要なのは、driver を `Cargo.toml` の dep に追加するだけでは**不十分**な点 — リンカは未参照と判断したクレートを strip し、`submit!` の副作用ごと落としてしまう。`crates/shpx-cli/src/registry.rs` は driver ごとに `use shpx_driver_<x> as _;` を 1 行保持し、リンクを強制している。driver 追加時はこのファイルと `shpx-cli/Cargo.toml` の両方を編集する。手順全体は `docs/CONTRIBUTING.md`。

### Capability 駆動のパス選択

各 `Driver::capabilities()` が `read`/`write`/`bulk_load`/`supports_decimal`/`string_encoding`/`max_decimal_precision` 等を宣言する。コアパイプラインはこれを読んで bulk か row かのパスを選び、エンコーディング変換を適用し、損失警告を出す。`--on-loss` の既定は `error`（安全側）。`warn`/`skip` で降格（decimal→double 等）を許可する。詳細は `docs/DATA_TYPES.md` と `docs/ON_LOSS.md`。

### RDB bulk-load 戦略

- **PostGIS**: 自前の `COPY BINARY` エンコーダを `tokio-postgres::CopyInSink` に流す。ジオメトリは EWKB を `bytea` 経由で送る。
- **SQL Server**: staging `#temp` テーブル → `tiberius` `bulk_insert` で WKB を流す → `INSERT … SELECT geometry::STGeomFromWKB(...)` → drop。**patch 済み tiberius fork**（`Cargo.toml` で `rev` 固定）を使用。`docs/SQLSERVER_BULK_BUG_REPRO.md` 参照。
- **SQLite (GPKG/SpatiaLite)**: 単一トランザクション + prepared-INSERT バッチ、WAL モード。

`Driver` trait の API は同期。非同期ドライバ（postgis, sqlserver）は内部に tokio runtime を保持し `block_on` で同期化する。

## 規約

- workspace 全体で `unsafe_code = "deny"`。clippy は `all` + `pedantic` を warning とし、ノイズの大きい一部の pedantic lint を `Cargo.toml` の `[workspace.lints]` で allow している。
- Arrow/Parquet のバージョンは **workspace レベルで pin**（現在 54）— 公開クレート同士で `arrow_array` の型を共有するため、workspace 内でバージョンを揃える必要がある。
- コメント・ドキュメントは主に **日本語**。*Why*（なぜそうするか）を書く（What ではなく）。
- 設計判断は `docs/` に集約 — `DESIGN.md`（アーキテクチャ）、`ROADMAP.md`（マイルストーン）、加えてフォーマット/関心事ごとの spec doc（`CRS.md`, `CSV.md`, `GPKG.md`, `POSTGIS.md`, `SQLSERVER.md`, `SPATIALITE.md` …）。ドライバを変更する前に該当 doc を読むこと。

## Agent skills

### Issue tracker

Issues are tracked as GitHub issues in `jumboly/shpx` (uses the `gh` CLI). See `docs/agents/issue-tracker.md`.

### Triage labels

Default canonical label vocabulary (`needs-triage`, `needs-info`, `ready-for-agent`, `ready-for-human`, `wontfix`). See `docs/agents/triage-labels.md`.

### Domain docs

Single-context layout (`CONTEXT.md` + `docs/adr/` at repo root). See `docs/agents/domain.md`.
