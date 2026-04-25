# shpx

GDAL 非依存・Rust 製の空間データ相互変換 CLI。Arrow RecordBatch を中間表現に、属性順序と厳密な型を保ちながら大容量データを streaming で扱う。

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
| PostGIS (`pg://` / `postgres://` / `postgresql://`) | ✓ | ✓ | (cycle 2 で COPY BINARY) |
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
shpx schema parcels.gpkg
shpx drivers
```

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
- [docs/CONTRIBUTING.md](docs/CONTRIBUTING.md) — 新しいドライバの追加方法
- [docs/CSV.md](docs/CSV.md) — CSV / TSV ドライバ仕様
- [docs/GEOJSON.md](docs/GEOJSON.md) — GeoJSON / GeoJSON Lines ドライバ仕様
- [docs/GPKG.md](docs/GPKG.md) — GeoPackage ドライバ仕様
- [docs/FGB.md](docs/FGB.md) — FlatGeobuf ドライバ仕様
- [docs/POSTGIS.md](docs/POSTGIS.md) — PostGIS ドライバ仕様

## ステータス

v0.2.0 リリース済み（2026-04-25）。次マイルストーン v0.3 は PostGIS ドライバ。
**v0.3 cycle 1 (進行中)**: `pg://` / `postgres://` / `postgresql://` URL での read/write、SHP / Parquet ↔ PostGIS の最小往復、行 INSERT writer。COPY BINARY は cycle 2、`--where`/`--select`/`--query` 等は cycle 3 で対応予定。詳細は [docs/POSTGIS.md](docs/POSTGIS.md) と [docs/ROADMAP.md](docs/ROADMAP.md)、変更履歴は [CHANGELOG.md](CHANGELOG.md)。

## ライセンス

未定（v1.0 までに決定）。
