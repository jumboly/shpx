# shpx examples

`shpx` のシナリオ別 1-shot スクリプト集。各スクリプトは独立に動き、`bash
examples/01-shp-to-parquet.sh` のように直接実行できる。

## 共通環境変数

| 変数 | 既定 | 説明 |
|---|---|---|
| `SHPX_BIN` | `cargo run --release -p shpx-cli --` | shpx 実行コマンド。release バイナリを `~/.cargo/bin/shpx` に置いた場合は `SHPX_BIN=shpx bash examples/01-*.sh` で短縮可能 |
| `OUT` | `/tmp/shpx-examples` | 出力先ディレクトリ。スクリプトが `mkdir -p` する |
| `PG_URL` | `pg://shpx:shpx@localhost:5432/shpx_test` | PostGIS 接続文字列 (02 / 03 / 06 で利用) |

## シナリオ一覧

| #   | スクリプト                  | 内容                                            | DB 必要 |
| --- | --------------------------- | ----------------------------------------------- | :-----: |
| 01  | `01-shp-to-parquet.sh`      | SHP → GeoParquet                                |    –    |
| 02  | `02-shp-to-postgis.sh`      | SHP → PostGIS (COPY バルク)                     |   ✓    |
| 03  | `03-postgis-to-fgb.sh`      | PostGIS → FlatGeobuf                            |   ✓    |
| 04  | `04-reproject.sh`           | EPSG:3857 SHP → EPSG:4326 GeoParquet            |    –    |
| 05  | `05-on-loss.sh`             | `--on-loss=error/warn/skip` の差を CSV → SHP で |    –    |
| 06  | `06-bulk-load.sh`           | `--insert-mode=bulk` vs `batch` の経路比較      |   ✓    |

DB が必要なものは事前に `docker compose up -d postgis` を実行する。

## test data

`examples/data/` に小さな fixture をコミット済み:

- `cities.csv` / `.shp` ほか — 5 都市 (Tokyo / NYC / London / Sydney / Cairo)、
  WGS84
- `cities-3857.shp` ほか — 上を Web Mercator に再投影したもの
- `lossy.csv` — DBF 10-byte 制限に引っ掛かる長い列名を持つ素材

再生成手順は `examples/data/REGENERATE.md` を参照。
