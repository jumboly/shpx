# shpx ロードマップ

各マイルストーンは「実装 → テスト → 1 commit でリリース可能な状態」を完了基準とする。

## v0.1 — コア骨格 / SHP ↔ GeoParquet PoC（リリース済み: 2026-04-25）

**スコープ**:
- `shpx-core` クレート: `Driver` / `LayerReader` / `LayerWriter` / `BulkLoadWriter` トレイト、`Schema` / `Capabilities` / `Crs` 型
- `shpx-geom` クレート: WKB encoder/decoder、`Crs` 構造体（EPSG コードのみ）
- `shpx-driver-shp`: Shapefile reader/writer（DBF cpg、WKT1 .prj 対応）
- `shpx-driver-parquet`: GeoParquet reader/writer
- `shpx-cli`: `convert` / `info` サブコマンド

**完了基準**:
- [x] `shpx convert input.shp output.parquet` で属性順保存 + WKB ジオメトリで往復可能
- [x] `shpx convert input.parquet output.shp` も動く
- [x] `shpx info <file>` でレコード数・スキーマ・CRS を表示
- [x] decimal / Date32 / Utf8 / Binary（GPKG/Parquet 側）の保全テスト pass
- [x] CRS は EPSG コードのみで保持・伝搬（reprojection は v0.2）
- [x] `cargo test --workspace` 緑、clippy warning ゼロ

**スコープ外（次マイルストーン以降）**:
- CRS 変換（reprojection）
- GPKG/PostGIS/SQL Server などの追加 Driver

---

## v0.2 — GPKG / GeoJSON / CSV / FlatGeobuf + Reprojection

**スコープ**:
- `shpx-driver-gpkg`: GeoPackage reader/writer（`gpkg_contents`/`gpkg_geometry_columns`/`gpkg_spatial_ref_sys` 初期化）
- `shpx-driver-geojson`: FeatureCollection と GeoJSONL (NDJSON, 1 feature/行) の双方
- `shpx-driver-csv`: WKT 列 + 属性カラムの CSV/TSV
- `shpx-driver-fgb`: FlatGeobuf
- `shpx-geom` への PROJ 統合（`proj` crate）
- `--reproject EPSG:xxxx` オプション
- 静的 driver レジストリを `inventory` 経由に移行（完了）
- `shpx drivers` / `shpx schema` サブコマンド追加（完了）

**完了基準**:
- [ ] 全フォーマット間（5フォーマット × 5）の往復ラウンドトリップテスト
- [ ] `--reproject EPSG:4326 → EPSG:3857` で既知点が誤差 1cm 以下
- [ ] GeoJSON は RFC 7946 準拠（WGS84 強制 reproject）、`--geojson-crs-extension` で CRS 保持可能
- [ ] GeoJSONL は `\n` 区切り、各行が単一 Feature

---

## v0.3 — PostGIS

**スコープ**:
- `shpx-driver-postgis`: `tokio-postgres` ベース
- 自前 COPY BINARY エンコーダ（int/bigint/float8/text/bytea/numeric/timestamptz/geometry-EWKB）
- reader: `--where` / `--select` / `--table` / `--query` 対応
- writer: `--create-table=if-not-exists|always|never`、GIST index オプション
- 接続: `pg://user:pass@host/db?table=...`

**完了基準**:
- [ ] 1000万行 × 10 属性のベンチで `ogr2ogr` の 50% 以上の速度
- [ ] decimal(38, 10) / timestamptz / bytea が往復で bit-identical
- [ ] geometry の SRID と座標が無損失
- [ ] CI で `docker compose up postgis` テスト

---

## v0.4 — SQL Server (Staging 経由 bulk)

**スコープ**:
- `shpx-driver-sqlserver`: `tiberius` ベース
- staging テーブル経由 bulk writer（案B、設計は DESIGN.md 参照）
- reader: `STAsBinary()` 経由で WKB 取得
- 接続: `mssql://user:pass@host/db?table=...&trusted_connection=true`
- `--insert-mode=bulk|batch`

**完了基準**:
- [ ] geometry / geography 双方で staging 経由 bulk insert が動く
- [ ] chunk size 1M でも tempdb 溢れなし（chunk ごと commit）
- [ ] CI で `docker compose up mssql` テスト

---

## v0.5 — SpatiaLite

**スコープ**:
- `shpx-driver-spatialite`: `mod_spatialite` 動的ロード
- SpatiaLite blob geometry エンコーダ/デコーダ
- `SELECT InitSpatialMetadata()` 自動実行

**完了基準**:
- [ ] SpatiaLite ↔ GPKG / Shapefile の往復
- [ ] 空間インデックス（R*Tree）のオプション作成

---

## v1.0 — 仕上げと配布

**スコープ**:
- 損失ポリシー（`--on-loss=error|warn|skip`）の完全実装
- `indicatif` で進捗バー、`tracing` でログレベル整備
- `shpx schema` / `shpx drivers` サブコマンド
- ドキュメント完備（README / docs / examples/）
- `cargo-dist` で macOS(arm64/x64) / Linux(x64/arm64) / Windows(x64) のバイナリリリース
- Homebrew tap、Docker image (ghcr.io)
- crates.io 公開
- ライセンス決定（MIT/Apache-2.0 dual を想定）

**完了基準**:
- [ ] `cargo install shpx` で導入可能
- [ ] `brew install <tap>/shpx` で導入可能
- [ ] `docker run ghcr.io/.../shpx` で導入可能
- [ ] チュートリアル形式の README / `examples/`

---

## v1.x 以降（候補）

- MS-SSCLRT UDT エンコーダで SQL Server 真の bulk
- 対話的 REPL モード（`shpx repl`）
- 動的プラグイン（dylib / WASM）
- 追加フォーマット: FileGDB、DXF、KML、GML、TopoJSON、ラスター（GeoTIFF）
- 並列パイプライン最適化（`tokio` task で reader/writer 分離）
- CDC / streaming 同期モード（PostgreSQL → Parquet 増分追記など）
