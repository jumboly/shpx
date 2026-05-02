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

## v0.2 — GPKG / GeoJSON / CSV / FlatGeobuf + Reprojection（リリース済み: 2026-04-25）

**スコープ**:
- `shpx-driver-gpkg`: GeoPackage reader/writer（`gpkg_contents`/`gpkg_geometry_columns`/`gpkg_spatial_ref_sys` 初期化）（cycle 3 完了）
- `shpx-driver-geojson`: FeatureCollection と GeoJSONL (NDJSON, 1 feature/行) の双方（cycle 2 完了）
- `shpx-driver-csv`: WKT 列 + 属性カラムの CSV/TSV（cycle 1 完了）
- `shpx-driver-fgb`: FlatGeobuf（cycle 4 完了）
- `shpx-geom` への PROJ 統合（`proj` crate）（cycle 5 完了）
- `--reproject EPSG:xxxx` オプション（cycle 5 完了）
- GeoJSON writer の RFC 7946 準拠（自動 WGS84 強制 reproject）（cycle 5 完了）
- 静的 driver レジストリを `inventory` 経由に移行（完了）
- `shpx drivers` / `shpx schema` サブコマンド追加（完了）
- `shpx-geom::wkt` の encode/decode 追加（cycle 1 完了、CSV/GeoJSON で再利用）
- `shpx-geom::gpkg_blob` の encode/decode 追加（cycle 3 完了）

**完了基準**:
- [x] 全フォーマット間の往復ラウンドトリップテスト
- [x] `--reproject EPSG:4326 → EPSG:3857` で既知点が誤差 1cm 以下
- [x] GeoJSON は RFC 7946 準拠（WGS84 強制 reproject）。`--geojson-crs-extension` は v0.3 で対応予定
- [x] GeoJSONL は `\n` 区切り、各行が単一 Feature

**進捗メモ**:
- cycle 1 (CSV): SHP ↔ CSV / Parquet ↔ CSV の往復テストが緑。型推定なし（全列 Utf8）方針で確定。詳細は `docs/CSV.md`。
- cycle 2 (GeoJSON): `.geojson` (FeatureCollection) / `.geojsonl` / `.ndjson` / `.jsonl` (NDJSON) を 1 ドライバで両対応。属性は JSON 型を Arrow に推論（Int↔Float 昇格、混在は Utf8）。書き出しは v0.2 cycle 2 時点では EPSG:4326 限定（cycle 5 で自動 reproject 化）。`--geojson-crs-extension` は v0.3 で対応予定。詳細は `docs/GEOJSON.md`。
- cycle 3 (GPKG): rusqlite (`bundled` SQLite) + 自前 GeoPackage Binary コーデック (`shpx-geom::gpkg_blob`) で実装。`gpkg_spatial_ref_sys` / `gpkg_contents` / `gpkg_geometry_columns` 初期化、SHP ↔ GPKG / Parquet ↔ GPKG / CSV ↔ GPKG / GeoJSON ↔ GPKG の往復テストが緑。テーブル名は URI クエリ `?table=...` または `SHPX_GPKG_TABLE` 環境変数で指定可能。geometry blob は envelope_type=0 固定で書き出し（spatial index 未対応のため）、reader は全 envelope_type を読み飛ばす。Z/M / 複数レイヤ / spatial index は v0.3 以降。詳細は `docs/GPKG.md`。
- cycle 4 (FGB): 公式 `flatgeobuf` 6.0 (BSD-2-Clause) + geozero で実装。geometry は WKB ↔ FGB FlatBuffers を相互変換、属性は `PropertyProcessor` 経由で 1 列ずつ受ける。SHP ↔ FGB の e2e roundtrip、Point/LineString/Polygon/MultiPolygon の各 geometry roundtrip、Boolean/Date32/Float64 等の属性 roundtrip テストが緑。**packed Hilbert R-Tree インデックスは未生成**（`index_node_size=0` 固定）、Z/M / null geometry / `select_bbox` / `Json` 列構造化は未対応。`flatgeobuf` 6.0 の MSRV (1.85) に合わせて workspace MSRV を 1.79 → 1.85 に引き上げ。詳細は `docs/FGB.md`。
- cycle 5 (Reprojection): `proj 0.28` クレート + システム libproj (pkg-config) で実装。`shpx-geom::Reprojector` が thread-local キャッシュ経由で `Proj` を生成、CLI `--reproject EPSG:xxxx`（WKT2 / proj-string 受理）と GeoJSON writer の自動 WGS84 強制で利用。4326 ↔ 3857 の精度テスト（既知点 1cm 以下）と SHP→reproject→Parquet などの e2e が緑。`bundled-proj` feature で libproj 同梱配布が可能（`cargo-dist`）。並列化（rayon）と GeoJSON `crs` extension は v0.3 へ。詳細は `docs/CRS.md`。

---

## v0.3 — PostGIS

**スコープ**:
- `shpx-driver-postgis`: `tokio-postgres` ベース
- 自前 COPY BINARY エンコーダ（int/bigint/float8/text/bytea/numeric/timestamptz/geometry-EWKB）
- reader: `--where` / `--select` / `--table` / `--query` 対応
- writer: `--create-table=if-not-exists|always|never`、GIST index オプション
- 接続: `pg://user:pass@host/db?table=...`

**完了基準**:
- [x] 1000万行 × 10 属性のベンチで `ogr2ogr` の 50% 以上の速度（実測 0.453、`docs/POSTGIS.md` Benchmark 節参照）
- [x] decimal(38, 10) / timestamptz / bytea が往復で bit-identical（cycle 2 の `tests/bulk_roundtrip.rs::bulk_decimal128_38_10_bit_identical` ほか）
- [x] geometry の SRID と座標が無損失（cycle 1 の `tests/roundtrip.rs`）
- [x] CI で `docker compose up postgis` テスト（`.github/workflows/ci.yml` の `services.postgis`）

**サブ cycle 構成** (v0.2 と同じく cycle ごとに `/clear` して clean に再開する):

- **cycle 1 — 基盤と最小往復**（完了）: `shpx-core::Uri` の URL スキーム検出、CLI 引数 String 化、`shpx-geom::ewkb` 追加、`shpx-driver-postgis` の最小 reader (`SELECT *`) + 行 INSERT writer (`ST_GeomFromEWKB`)、`docker compose` + GitHub Actions services + `SHPX_TEST_PG_URL` env-gated integration テスト。詳細は `docs/POSTGIS.md` 参照。
- **cycle 2 — COPY BINARY と BulkLoadWriter**（完了）: `crates/shpx-driver-postgis/src/copy_binary.rs` で pg binary COPY format の自前エンコーダ（bool / int{2,4,8} / float{4,8} / text / bytea / numeric / date / timestamp / timestamptz / geometry-EWKB の big-endian 直書き）。`BulkLoadWriter::bulk_write` を `PostgisWriter` で実装、`Driver::open_bulk_write` を `shpx-core` に追加し driver で override。`shpx-cli` に `--insert-mode=auto|bulk|batch` を追加（既定 `auto`、bulk_load 対応 driver なら bulk、それ以外は silently batch）。Decimal128 を batch / bulk 両経路でサポートし、`Capabilities::supports_decimal = true` に。decimal(38, 10) / timestamptz / bytea が bit-identical 往復することを env-gated 統合テストで確認。`ogr2ogr 50%` の正式ベンチは cycle 3c 完了時に再計測する。
- **cycle 3 は 3a / 3b / 3c に分割する**:
  - **cycle 3a — reader 拡張**（完了）: `shpx-core::ReadOpts` に `where_clause` / `select` / `query` を追加。`shpx-cli` の `convert` に `--where '<sql>'` / `--select col1,col2` / `--query '<sql>'` を追加（`--query` は他 2 つと clap 排他）。PostGIS reader を「table モード（WHERE / 列絞り）」と「query モード（任意 SQL のサブクエリ化）」の 2 経路に分け、geometry 列は `ST_AsEWKB` でラップしたまま再利用する。geometry 列を含まない抽出と `--query` の末尾セミコロン混入はエラー。
  - **cycle 3b — writer 拡張**（完了）: `shpx-core::WriteOpts` に `CreateTable` / `CreateIndex` enum を追加し、`shpx-cli` に `--create-table=if-not-exists|always|never`（既定 `if-not-exists`）と `--create-index=auto|always|never`（既定 `auto`）を追加。`--create-table` と既存 `--overwrite` は直交し、`--overwrite && --create-table=never` は整合性エラー。`--create-index=auto` は `create_table != Never` のときのみ GIST index を生成し、bulk 経路では COPY 完了後に発行する。SRID 解決時に `spatial_ref_sys` を probe し、欠けていれば `Crs.wkt` または `shpx_geom::epsg_to_wkt1(code)` 同梱マップから `srtext` を組み立てて `INSERT ... ON CONFLICT (srid) DO NOTHING` で best-effort 登録する。WKT が解決できない場合は INSERT スキップ（PostGIS の geometry 列は spatial_ref_sys 行が無くても動作するため）。env-gated の統合テスト 9 件と option 整合性ユニットテスト 2 件を追加。
  - **cycle 3c — ベンチと完了基準**（完了）: 1000万行 × 10 属性のベンチを criterion で整備（`crates/shpx-driver-postgis/benches/copy_binary.rs` + `gen.rs`）、`scripts/bench-vs-ogr.sh` で ogr2ogr との median wall-clock 比較。実測値 (10M 行 × 3 runs median) は shpx 28.46 s / ogr2ogr 62.84 s / 比 0.453 で完了基準（≤ 2.0）をクリア。詳細は `docs/POSTGIS.md` の Benchmark 節参照。型網羅 bit-identical テスト `bulk_all_types_together` も追加（`tests/bulk_roundtrip.rs`）。

---

## v0.4 — SQL Server (Staging 経由 bulk)

**スコープ**:
- `shpx-driver-sqlserver`: `tiberius` ベース
- staging テーブル経由 bulk writer（案B、設計は DESIGN.md 参照）
- reader: `STAsBinary()` 経由で WKB 取得（v0.4 は table モード固定、`--where`/`--select`/`--query` は v0.5+）
- 接続: `mssql://user:pass@host/db?table=...&geom_type=geometry|geography`
- `--insert-mode=bulk|batch`

**完了基準**:
- [x] geometry / geography 双方で staging 経由 bulk insert が動く（`tests/bulk_roundtrip.rs::bulk_geography_all_geom_types` ほか、env-gated）
- [ ] chunk size 1M でも tempdb 溢れなし（chunk ごと commit）— 実 SQL Server に対する 10M 行ベンチで確認予定
- [x] CI で `docker compose up mssql` テスト（`.github/workflows/ci.yml` の `services.mssql`）

**確定済み設計判断**:
- **reader 拡張は v0.5+ に先送り**: writer (staging bulk) と完了基準達成を優先。v0.4 reader は table モード固定。
- **`--create-index=auto` は no-op**: SQL Server `CREATE SPATIAL INDEX` は `BOUNDING_BOX` 必須で未知 SRID では失敗するため、Auto は黙って何もしないに振る。`Always` 指定時のみ既知 EPSG（4326/3857）の同梱 bbox 表で生成、未知 SRID は明示エラー。geography は `BOUNDING_BOX` 不要。
- **staging chunk 既定 100,000 行**: Express edition / 低メモリ dev 環境で安全側。bench 時のみ `SHPX_MSSQL_BULK_CHUNK=1000000` で 1M に上げる。
- **geometry vs geography 切替**: URI クエリ `?geom_type=geometry|geography`、未指定は `geometry`。schema field metadata の `edges=spherical` がある場合のみ既定を `geography` に上書き。
- **認証**: SQL 認証のみ。`?trusted_connection=true` は CLI 受理 → driver で「未対応」エラー（v0.5+ 予約）。
- **SRS 自動登録は不要**: `sys.spatial_reference_systems` は SQL Server 同梱 seed 済みのため、PostGIS の `register_srs_if_missing` 相当は実装しない。
- **`--insert-mode`**: `bulk|batch` のみ（CLI の `auto` は `Capabilities::bulk_load = true` 経由で実質 `auto = bulk`）。

**サブ cycle 構成** (v0.3 と同じく cycle ごとに `/clear` して clean に再開する):

- **cycle 1 — 基盤と最小往復**: workspace に `crates/shpx-driver-sqlserver` 追加、`mssql://` URI を tiberius `Config` に変換、reader (table モード固定、`STAsBinary` + `STSrid` ラップ)、writer (`CREATE TABLE` + 行単位 prepared INSERT で `geometry::STGeomFromWKB` 経由)、`Capabilities { read, write, !bulk_load }`。`docker-compose.yml` に `mssql` service、`.github/workflows/ci.yml` に `services.mssql` + `SHPX_TEST_SQLSERVER_URL` env + DB 作成 step。
- **cycle 2 — staging bulk writer (案B)**: `BulkLoadWriter` 実装、`Capabilities::bulk_load = true`、chunk loop（既定 100K 行、`SHPX_MSSQL_BULK_CHUNK` env override 可）で `BEGIN TRAN` → `#shpx_stage_<uuid>` 作成 → `tiberius::Client::bulk_insert` → `INSERT INTO target SELECT ..., {geometry|geography}::STGeomFromWKB(geom_wkb, geom_srid) FROM #stage` → `TRUNCATE` → `COMMIT TRAN` → 次 chunk。decimal は `rust_decimal::Decimal` 経由（tiberius Numeric write バグ回避）。geography 時は CRS 不在で既定 4326 にフォールバック。bit-identical 往復テスト（decimal(38,10) / timestamptz / bytea）。
- **cycle 3a — writer 拡張**: `--create-table=if-not-exists|always|never` (PostGIS と同形)、`--create-index` 対応（`Auto` は no-op、`Always` のみ `CREATE SPATIAL INDEX [...] WITH (BOUNDING_BOX = ...)` を発行 / geography は BOUNDING_BOX なし）、SRID 解決（`--src-crs` → schema metadata → on_loss フォールバック）。`--overwrite && create_table=Never` 整合性エラー。
- **cycle 3b — ベンチと完了基準 + ドキュメント**: criterion bench（1M / 10M 行 × 10 属性 + Point）、`scripts/bench-vs-ogr-mssql.sh` で `ogr2ogr -f MSSQLSpatial` との median wall-clock 比較。完了基準目標 ≤ 0.6 倍（PostGIS の 0.5 より緩い、staging のラウンドトリップが 1 余分のため）。`bulk_all_types_together` / `bulk_geography_all_geom_types` 追加。`docs/SQLSERVER.md` 新設、`docs/DATA_TYPES.md` の SQL Server 列確定、`Cargo.toml` を `0.4.0` へ bump、release commit。

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
