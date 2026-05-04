# shpx

GDAL 非依存・Rust 製の空間データ相互変換 CLI。Arrow RecordBatch を中間表現に、属性順序と厳密な型を保ちながら大容量データを streaming で扱う。

## 5 分チュートリアル

### 1. インストール

shell installer (Linux / macOS / Windows、推奨):

```bash
curl -LsSf https://github.com/jumboly/shpx/releases/download/v1.0.0/shpx-cli-installer.sh | sh
```

`cargo-dist` で生成された installer がアーキを判定し、`$CARGO_HOME/bin` に `shpx` バイナリを展開する。`aarch64-apple-darwin` / `x86_64-apple-darwin` / `aarch64-unknown-linux-gnu` / `x86_64-unknown-linux-gnu` / `x86_64-pc-windows-msvc` の 5 triples を配布。

ローカルビルド (libproj が host に必要):

```bash
git clone https://github.com/jumboly/shpx
cd shpx
cargo install --path crates/shpx-cli
```

`bundled-proj` feature で libproj / SQLite を同梱した単一バイナリをビルド可能 (cmake / clang が必要):

```bash
cargo install --path crates/shpx-cli --features bundled-proj
```

### 2. SHP → GeoParquet

最初の変換は拡張子推論で完結する。サンプルデータは `examples/data/cities.shp` (5 都市 / WGS84):

```bash
shpx convert examples/data/cities.shp /tmp/cities.parquet
shpx schema  /tmp/cities.parquet --format=text
```

詳細スクリプト: [examples/01-shp-to-parquet.sh](examples/01-shp-to-parquet.sh)

### 3. ローカル PostGIS にバルクロード

`docker compose up -d postgis` で接続先を起動した後、`pg://` URI で書き込み。`COPY BINARY` 経路で大容量も高速:

```bash
shpx convert examples/data/cities.shp \
  'pg://shpx:shpx@localhost:5432/shpx_test?table=public.cities&create=if-not-exists'
```

詳細スクリプト: [examples/02-shp-to-postgis.sh](examples/02-shp-to-postgis.sh)

### 4. 投影変換

`--reproject` 1 つで EPSG 間を往復:

```bash
shpx convert examples/data/cities-3857.shp /tmp/cities-wgs84.parquet \
  --reproject EPSG:4326
```

詳細スクリプト: [examples/04-reproject.sh](examples/04-reproject.sh)

## 対応フォーマット

| フォーマット | Read | Write | Bulk |
|---|:-:|:-:|:-:|
| Shapefile (`.shp`) | ✓ | ✓ | – |
| GeoPackage (`.gpkg`) | ✓ | ✓ | TX batch |
| GeoParquet (`.parquet`) | ✓ | ✓ | – |
| FlatGeobuf (`.fgb`) | ✓ | ✓ | – |
| GeoJSON (`.geojson`) | ✓ | ✓ | – |
| GeoJSON Lines (`.geojsonl` / `.ndjson` / `.jsonl`) | ✓ | ✓ | – |
| CSV w/ WKT (`.csv` / `.tsv`) | ✓ | ✓ | – |
| PostGIS (`pg://` / `postgres://` / `postgresql://`) | ✓ | ✓ | COPY BINARY |
| SQL Server (`mssql://`) | ✓ | ✓ | staging table → bulk_insert |
| SpatiaLite (`sqlite://`) | ✓ | ✓ | TX batch |

## 使い方

```bash
# 拡張子から推論
shpx convert parcels.shp parcels.gpkg
shpx convert parcels.gpkg parcels.parquet --reproject EPSG:4326
shpx convert parcels.shp parcels.geojson  # 非 WGS84 入力は自動で WGS84 へ変換

# URI で明示（v0.3 以降）
shpx convert parcels.shp 'pg://user:pass@host/db?table=public.parcels&create=if-not-exists'
shpx convert 'mssql://host/db?table=dbo.cities' cities.parquet

# 情報表示
shpx info parcels.gpkg
shpx schema parcels.gpkg --format=text   # 人間可読
shpx schema parcels.gpkg --format=json   # 機械可読 (default)
shpx drivers --format=json | jq '.[].name'  # CI / scripting 向け
```

## examples

[`examples/`](examples/) にシナリオ別の 1-shot スクリプト 6 本を同梱。`bash examples/01-shp-to-parquet.sh` のように単独実行できる。

| #   | スクリプト                                                       | 内容                                          | DB 必要 |
| --- | ---------------------------------------------------------------- | --------------------------------------------- | :----:  |
| 01  | [01-shp-to-parquet.sh](examples/01-shp-to-parquet.sh)            | SHP → GeoParquet                              |    –    |
| 02  | [02-shp-to-postgis.sh](examples/02-shp-to-postgis.sh)            | SHP → PostGIS (COPY バルク)                   |   ✓    |
| 03  | [03-postgis-to-fgb.sh](examples/03-postgis-to-fgb.sh)            | PostGIS → FlatGeobuf                          |   ✓    |
| 04  | [04-reproject.sh](examples/04-reproject.sh)                      | EPSG:3857 SHP → EPSG:4326 GeoParquet          |    –    |
| 05  | [05-on-loss.sh](examples/05-on-loss.sh)                          | `--on-loss=error/warn/skip` の比較            |    –    |
| 06  | [06-bulk-load.sh](examples/06-bulk-load.sh)                      | `--insert-mode=bulk` vs `batch` の経路比較    |   ✓    |

詳細は [examples/README.md](examples/README.md) を参照。

## ビルド要件

- Rust 1.85 以上
- システムに **libproj** がインストール済みであること（`--reproject` および GeoJSON writer の自動 WGS84 変換で利用）
  - macOS: `brew install proj`
  - Ubuntu / Debian: `apt-get install libproj-dev pkg-config`
  - Windows: vcpkg 等で `proj` を導入する
- `cargo install shpx --features bundled-proj` で libproj/SQLite を同梱した単一バイナリをビルドできる（`cmake` / `clang` が必要）。`cargo-dist` で配布するバイナリも本フラグでビルドする想定。

## 設計ドキュメント

- [docs/DESIGN.md](docs/DESIGN.md) — アーキテクチャと設計判断
- [docs/ROADMAP.md](docs/ROADMAP.md) — マイルストーン
- [docs/DATA_TYPES.md](docs/DATA_TYPES.md) — フォーマット間の型マッピング
- [docs/CRS.md](docs/CRS.md) — 座標参照系の扱い
- [docs/ON_LOSS.md](docs/ON_LOSS.md) — 損失検出 (`--on-loss`) のドライバ × kind マトリクス
- [docs/STREAMING.md](docs/STREAMING.md) — driver 別ストリーミング戦略とピーク RSS
- [docs/CONTRIBUTING.md](docs/CONTRIBUTING.md) — 新しいドライバの追加方法
- [docs/CSV.md](docs/CSV.md) — CSV / TSV ドライバ仕様
- [docs/GEOJSON.md](docs/GEOJSON.md) — GeoJSON / GeoJSON Lines ドライバ仕様
- [docs/GPKG.md](docs/GPKG.md) — GeoPackage ドライバ仕様
- [docs/FGB.md](docs/FGB.md) — FlatGeobuf ドライバ仕様
- [docs/POSTGIS.md](docs/POSTGIS.md) — PostGIS ドライバ仕様
- [docs/SQLSERVER.md](docs/SQLSERVER.md) — SQL Server ドライバ仕様
- [docs/SPATIALITE.md](docs/SPATIALITE.md) — SpatiaLite ドライバ仕様

## ステータス

v1.0.0 リリース済み（2026-05-04）。reader 全 9 driver の真ストリーミング化に加え、`cargo-dist` による 5 target × 3 OS 単一バイナリ配布、進捗バー、examples / README 5 分チュートリアル、`shpx schema` / `shpx drivers --format=json` まで一通り揃った。次マイルストーンは **v1.x** で、crates.io 公開 / Homebrew tap / Docker image を順次検討する。詳細は [docs/ROADMAP.md](docs/ROADMAP.md) と [CHANGELOG.md](CHANGELOG.md)。

## ライセンス

Apache-2.0 OR MIT のデュアルライセンス。詳細は [LICENSE-APACHE](LICENSE-APACHE) / [LICENSE-MIT](LICENSE-MIT) / [NOTICE](NOTICE) を参照。
