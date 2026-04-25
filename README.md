# shpx

GDAL 非依存・Rust 製の空間データ相互変換 CLI。Arrow RecordBatch を中間表現に、属性順序と厳密な型を保ちながら大容量データを streaming で扱う。

## 対応フォーマット

| フォーマット | Read | Write | Bulk |
|---|:-:|:-:|:-:|
| Shapefile (`.shp`) | ✓ | ✓ | – |
| GeoPackage (`.gpkg`) | – | – | TX batch |
| GeoParquet (`.parquet`) | ✓ | ✓ | – |
| FlatGeobuf (`.fgb`) | – | – | – |
| GeoJSON (`.geojson`) | ✓ | ✓ | – |
| GeoJSON Lines (`.geojsonl` / `.ndjson` / `.jsonl`) | ✓ | ✓ | – |
| CSV w/ WKT (`.csv` / `.tsv`) | ✓ | ✓ | – |
| PostGIS (`pg://`) | ✓ | ✓ | COPY BINARY |
| SQL Server (`mssql://`) | ✓ | ✓ | staging table → bulk_insert |
| SpatiaLite (`sqlite://`) | ✓ | ✓ | TX batch |

## 使い方（予定）

```bash
# 拡張子から推論
shpx convert parcels.shp parcels.gpkg
shpx convert parcels.gpkg parcels.parquet --reproject EPSG:4326

# URI で明示
shpx convert parcels.shp 'pg://user:pass@host/db?table=public.parcels&create=if-not-exists'
shpx convert 'mssql://host/db?table=dbo.cities' cities.parquet

# 情報表示
shpx info parcels.gpkg
shpx schema parcels.gpkg
shpx drivers
```

## 設計ドキュメント

- [docs/DESIGN.md](docs/DESIGN.md) — アーキテクチャと設計判断
- [docs/ROADMAP.md](docs/ROADMAP.md) — マイルストーン
- [docs/DATA_TYPES.md](docs/DATA_TYPES.md) — フォーマット間の型マッピング
- [docs/CRS.md](docs/CRS.md) — 座標参照系の扱い
- [docs/CONTRIBUTING.md](docs/CONTRIBUTING.md) — 新しいドライバの追加方法
- [docs/CSV.md](docs/CSV.md) — CSV / TSV ドライバ仕様
- [docs/GEOJSON.md](docs/GEOJSON.md) — GeoJSON / GeoJSON Lines ドライバ仕様

## ステータス

v0.1.0 リリース済み（2026-04-25）。v0.2 進行中で、現在は SHP / GeoParquet / CSV (WKT) / GeoJSON / GeoJSON Lines ドライバが利用可能（`cargo run -- convert` / `info` / `drivers`）。残作業は GPKG / FlatGeobuf ドライバと PROJ 統合 (`--reproject`)。変更履歴は [CHANGELOG.md](CHANGELOG.md)。

## ライセンス

未定（v1.0 までに決定）。
