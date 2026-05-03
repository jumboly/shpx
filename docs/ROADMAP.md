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
- `shpx-driver-spatialite`: `rusqlite` ベース + `mod_spatialite` 動的ロード
- 自前 SpatiaLite blob (geometry binary) エンコーダ/デコーダ — `shpx-geom::spatialite_blob`
- 接続後に `SELECT InitSpatialMetadata(1)` を idempotent 発行（FastInit、空 DB の seed 数秒を回避）
- reader: `?table=` / `SHPX_SPATIALITE_TABLE` 解決、`AsBinary(geom)` で WKB 取得、SRID は `geometry_columns.srid` 参照
- writer: 1 トランザクション + 行単位 prepared `INSERT ... GeomFromWKB(?, srid)`、`--create-table=if-not-exists|always|never`、`--create-index=auto|always|never` (Always で `SELECT CreateSpatialIndex(...)` の R*Tree)
- 接続: `sqlite://path?table=...` / `*.sqlite` / `*.db` / `*.spatialite` ファイル拡張子
- `bundled-spatialite` feature の本実装は v0.6 に繰り延べ。v0.5 はシステム libspatialite (CI / 開発機の apt 等) 前提で出荷し、feature 宣言は v0.6 予約として no-op で残す

**完了基準**:
- [ ] SpatiaLite ↔ GPKG / Shapefile の往復
- [ ] 空間インデックス（R*Tree）のオプション作成 (`--create-index=always` で `CreateSpatialIndex` 発行)

**確定済み設計判断**:
- **URI scheme は SpatiaLite が `sqlite` を専有**: `*.sqlite` / `*.db` / `*.spatialite` / `sqlite://...` はすべて SpatiaLite driver に解決する。GPKG は `*.gpkg` / `gpkg` scheme のみのまま。`?mod_spatialite=true` フラグ運用は採用しない（DESIGN.md L.149-160 表は v0.5 cycle 3 で訂正）。content-sniffing による自動振り分けは v1.0 以降の検討事項。
- **`mod_spatialite` ロード経路の優先順**: (1) `cfg(feature = "bundled-spatialite")` 時は静的リンク版 `spatialite_init` を呼ぶ、(2) `SHPX_SPATIALITE_PATH` env でパス上書き、(3) OS 既定検索パス (`mod_spatialite`)。CI Linux は (3) で apt の `libsqlite3-mod-spatialite` を見る。
- **`Capabilities::bulk_load = false`**: GPKG と同じく TX batch で十分。`open_bulk_write` は実装しない。`--insert-mode=auto` は自動的に batch に倒れる、`--insert-mode=bulk` 明示指定は invalid（PostGIS / SQL Server と同様の挙動）。
- **`shpx-rdb-common` の使い分け**: `query_get` / `percent_decode` / `merge_crs` / `resolve_epsg_srid` / `apply_on_loss` / `validate_overwrite_compat` / `primitive` を再利用。SQLite に schema 概念がないため `split_qualified` / `resolve_table_name` は呼ばない。
- **SpatiaLite blob の対応範囲**: v0.5 では XY のみ。Z/M / EMPTY / GeometryCollection は `Error::Geometry` で拒否（v1.0 以降で拡張）。MBR は WKB から走査して算出する。
- **SRID 解決順序**: `--src-crs` > schema field metadata > `apply_on_loss` フォールバック (PostGIS / SQL Server と同型)。fallback は SRID 0 (SpatiaLite 慣習で unknown)。
- **`spatial_ref_sys` への登録**: PostGIS と同パターンで `Crs.wkt` または `epsg_to_wkt1(code)` から組み立て、`INSERT OR IGNORE INTO spatial_ref_sys (...)` で best-effort 登録。
- **`bundled-spatialite` は v0.6 で実装**: workspace に `build.rs` ファイルが一つも無く bundled C ビルドの足場がゼロであること、libspatialite が GEOS / PROJ にも依存し vendor 範囲が cycle 1 つに収まらないことから、v0.5 はシステム libspatialite + CI apt 経由のみで出荷する。`bundled-spatialite` feature 宣言は driver / CLI の Cargo.toml に v0.6 予約として残し、有効化しても no-op (システム libspatialite を `load_extension` で見る挙動と同じ)。本実装の build 戦略 (libspatialite を `cc` で vendor、GEOS / PROJ の調達方針) は v0.6 セクションで詳細化する。

**サブ cycle 構成** (v0.3 / v0.4 と同じく cycle ごとに `/clear` して clean に再開する):

- **cycle 1 — 基盤と最小往復**: workspace に `crates/shpx-driver-spatialite` 追加、`shpx-geom::spatialite_blob` モジュール (encode/decode + ユニットテスト)、`SpatialiteDriver` 雛形 (`supported_schemes = &["sqlite", "db", "spatialite"]`、`Capabilities { read, write, !bulk_load }`)、`conn.rs` で `load_extension` + `InitSpatialMetadata(1)`、reader (`AsBinary` + SRID 解決)、writer (`--overwrite` のみ、行単位 prepared INSERT)、`shpx-cli/src/registry.rs` 登録、`bundled-spatialite` feature の宣言のみ。`.github/workflows/ci.yml` (Linux) に `apt-get install libsqlite3-mod-spatialite` + env。env-gated 統合テスト 1-2 件 (`SHPX_TEST_SPATIALITE`)。
- **cycle 2 — writer 拡張 + R*Tree**: `--create-table` 3 種 (PostGIS と同形)、`--create-index` 3 種 (`Always` で `SELECT CreateSpatialIndex(?, ?)` の R*Tree、`Auto` は `create_table != Never` のときのみ生成)、SRID 解決順序の統一 (`--src-crs` > schema metadata > `apply_on_loss` フォールバック / SRID 0)、`spatial_ref_sys` への best-effort INSERT (`INSERT OR IGNORE`)、`--overwrite && create_table=Never` 整合性エラー (`shpx_rdb_common::validate_overwrite_compat`)、env-gated 統合テスト `tests/writer_options.rs` 9 件。`bundled-spatialite` 本実装と CI smoke job は v0.6 に繰り延べ (上記「確定済み設計判断」参照)。
- **cycle 3 — docs + 完了基準 + 0.5.0 release**: `docs/SPATIALITE.md` 新設 (POSTGIS.md / SQLSERVER.md と同型構造)、`docs/DATA_TYPES.md` の SpatiaLite 列確定、`docs/DESIGN.md` L.149-160 訂正、`docs/CRS.md` / `docs/CONTRIBUTING.md` / `README.md` 更新、`CHANGELOG.md` v0.5.0 セクション、workspace `Cargo.toml` を `0.5.0` へ bump、release commit。完了基準テスト (SpatiaLite ↔ GPKG / SpatiaLite ↔ Shapefile の e2e 往復、CI Linux env-gated)。

---

## v0.6 — bundled-spatialite + 配布バイナリ準備

**スコープ**:
- `crates/shpx-driver-spatialite/build.rs` を新規作成し、libspatialite C ソースを vendor して `cc` で static link
- `shpx-cli/Cargo.toml` の `bundled-spatialite` feature 伝播は v0.5 で枠組み済みのため、本実装の中身を埋めるのみ
- CI に `bundled-spatialite` smoke job を追加 (Linux ubuntu-latest、システム libspatialite 不在状態でビルドが通ることを確認)
- v1.0 の `cargo-dist` リリースに向けた前提整備 (Linux/macOS/Windows での bundled ビルド検証)

**完了基準**:
- [x] `cargo build -p shpx-cli --features bundled-spatialite` がシステム libspatialite 不在環境で成功
- [x] CI に `bundled-spatialite` smoke job (Linux) が追加され、緑

**確定済み設計判断**:
- **GEOS / PROJ 調達方針**: GEOS は `geos-src` crate 経由 (無ければ自前 vendor)、PROJ は既存 `shpx-geom/bundled-proj` (外部 `proj` crate の `bundled_proj` feature) を再利用。libsqlite3 は `rusqlite` の `bundled` feature で既に vendor 済み (v0.5 cycle 1)。
- **build.rs の段階的実装**: (1) libspatialite C ソース vendor + `cc` 静的コンパイル → (2) GEOS リンク → (3) PROJ リンク → (4) `mod_spatialite` の静的初期化 (`extern "C" fn spatialite_init()` を `load_extension` の代わりに直接呼ぶ)。
- **macOS / Windows の優先度**: cycle 当初は Linux のみ smoke job、macOS / Windows は v1.0 の `cargo-dist` 配信時に追加対応。
- **詰まった場合の縮退**: それでも v0.6 内で詰まった場合は v0.7 に再縮退する (v0.5 と同じ判断パターン)。

**サブ cycle 構成** (v0.3 / v0.4 / v0.5 と同じく cycle ごとに `/clear` して clean に再開する):

- **cycle 1 — build.rs 足場 + libspatialite vendor (GEOS / PROJ 抜き最小構成)**: libspatialite 5.1.0 の release tarball を `crates/shpx-driver-spatialite/vendor/libspatialite-5.1.0/` に in-tree commit (`vendor/SHA256SUMS` で再現性固定、download は build.rs では行わない)。`crates/shpx-driver-spatialite/build.rs` を新規作成し、`#[cfg(feature = "bundled-spatialite")]` 内でのみ `cc::Build` で C ソースを集めて `.compile("spatialite_bundled")`。preprocessor define で GEOS / PROJ / RTTOPO / libxml2 / freexl / iconv / minizip を全て off (`OMIT_GEOS` / `OMIT_PROJ` / `OMIT_GEOCALLBACKS` 等、`configure.ac` 由来) し、純粋な geometry blob I/O + R\*Tree (R\*Tree は SQLite native) のみで build を通す。`crates/shpx-driver-spatialite/Cargo.toml` に `build = "build.rs"` と `[build-dependencies] cc` を追加 (workspace dep にも `cc = "1"`)。`crates/shpx-driver-spatialite/src/conn.rs` の cfg 分岐に FFI 実体投入 (`extern "C" fn sqlite3_modspatialite_init(db, pzErrMsg, pApi)` を rusqlite の `Connection::handle()` raw pointer に直接呼ぶ)。`crates/shpx-driver-spatialite/NOTICE` を新設し libspatialite triple license (MPL 1.1 / GPL 2.0 / LGPL 2.1) と vendor バージョンを明記。完了基準: `cargo build -p shpx-cli --features bundled-spatialite` がシステム libspatialite 不在で成功 + geometry roundtrip e2e (Point / LineString / Polygon の WKB ↔ GeomFromWKB) が緑。
- **cycle 2 — GEOS リンク**: `geos-src = "2"` (libgeos 3.x C ソース vendor + cc build) を build-dep として workspace + driver Cargo.toml に追加。`geos-src` の build script が export する `cargo:include=` / `cargo:rustc-link-lib=` を build.rs で受け取り、libspatialite ビルドの `cc::Build` に `.include(geos_include)` で feed。build.rs の `OMIT_FEATURES` から `"GEOS"` を削除し、GEOS 依存ファイルもコンパイル対象に追加。`tests/bundled_geos_smoke.rs` で `SELECT ST_Buffer(GeomFromWKB(?, 4326), 0.1)` を 1 件叩く smoke test を追加。**v0.6 cycle 1 で入れた `vendor/libspatialite-5.1.0/src/spatialite/spatialite.c` の 2 箇所の `shpx-patch` (`#ifndef OMIT_GEOS` ガード)** はこの cycle で dead branch になるため物理削除し、上流通りに復元する (vendor を bit-identical に近づける)。`NOTICE` の「shpx local modifications」から patch 項目も外す (除外ファイルリストは残す)。並行して上流 fossil tracker (https://www.gaia-gis.it/fossil/libspatialite/) に同 patch を提案 (ただし shpx 側からは依存させない、上流が受理しなくても完了基準に影響しない)。完了基準: cycle 1 完了基準 + `ST_Buffer` が bundled で成功 + `grep "shpx-patch" vendor/` がゼロ件。
- **cycle 3 — PROJ リンク + `spatialite_init()` 直接呼びの本実装**: shpx-geom の `bundled-proj` (proj 0.28 / proj-sys) が同梱する libproj を libspatialite からも共有 (PROJ symbol の二重リンク回避)。build.rs で proj-sys の `DEP_PROJ_INCLUDE` / `DEP_PROJ_ROOT` 環境変数 (Cargo の `links` メタデータ経由) を読み、libspatialite ビルドの `cc::Build` に `.include(proj_include)` で feed。`crates/shpx-driver-spatialite/Cargo.toml` の `[package]` に `links = "spatialite_bundled"` を宣言 (cargo の duplicate link 検出のため)。`crates/shpx-cli/Cargo.toml` の `bundled-spatialite` を `["shpx-driver-spatialite/bundled-spatialite", "shpx-geom/bundled-proj"]` に変更し、CLI レイヤで bundled-spatialite が必ず bundled-proj を implies する設計に。build.rs から `OMIT_PROJ` define を外し PROJ 依存ファイルもコンパイル対象に追加。`load_mod_spatialite` の bundled feature 経路から `load_dynamic` への fallback を削除し、`SHPX_SPATIALITE_PATH` env は bundled feature 時 warn で無視。完了基準: `cargo test --features bundled-spatialite -p shpx-driver-spatialite` 全件緑 + `shpx convert in.shp out.sqlite --reproject EPSG:3857` が bundled で動作。
    - **実装メモ (cycle 3 実装時に確定)**: proj-sys 0.25.0 の build.rs は `cargo:include=` / `cargo:root=` を emit しないため、`DEP_PROJ_INCLUDE` / `DEP_PROJ_ROOT` 経由の include 共有は成立しない。cycle 2 の `locate_geos_root` と同パターンで sibling target (`target/<profile>/build/proj-sys-<hash>/out/include/proj.h`) を直接探索する `locate_proj_root` で代替した (`build.rs::locate_sibling_out` に共通化)。link 命令 (`cargo:rustc-link-lib=proj`) は proj-sys 側 (`links = "proj"`) に一本化し、本 driver の build.rs からは PROJ link を出さない。libspatialite 側は `gaiaconfig.h` で `#define PROJ_NEW 1` を出して PROJ 6+ API パス (`proj_create_crs_to_crs` 等) を選択する。`load_mod_spatialite` の fallback 削除は cycle 1 で既に `#[cfg]` 一本化されており実態 no-op。
- **cycle 4 — CI smoke job + docs + 0.6.0 release**（完了）: `.github/workflows/ci.yml` に `bundled-spatialite-smoke` job (ubuntu-latest、`libsqlite3-mod-spatialite` を apt から除外し、`cmake` / `clang` のみ apt 導入、`cargo build -p shpx-cli --features bundled-spatialite --release` 成功 + `cargo test -p shpx-driver-spatialite --features bundled-spatialite` 緑) を追加。`docs/SPATIALITE.md` に「bundled-spatialite ビルド」節を新設 (有効化方法 / vendor 範囲 / ビルドツール / ライセンス制約 / サポート OS / 静的初期化経路 / smoke test) し、内部実装メモの `mod_spatialite` ロード経路を bundled / default で 2 経路に分けて訂正。「スコープ外」節から bundled 関連 bullet を削除。`CHANGELOG.md` に v0.6.0 セクション追加、workspace `Cargo.toml` を `0.6.0` へ bump、release commit + `v0.6.0` annotated tag。CLI feature の `bundled-spatialite` は `bundled-proj` を implies するため、ROADMAP の `--features bundled-spatialite,bundled-proj` 表記は `--features bundled-spatialite` 単体に整理した。

**リスクと縮退判断**:
- **PROJ 二重リンク**: cycle 3 で proj-sys の `links` メタデータ経由 include path 共有が proj-sys の build.rs 出力に依存。proj-sys が必要な `cargo:include` を出していなければ「shpx-geom と shpx-driver-spatialite の双方が `proj-src` を build-dep として共有」へ pivot。
- **libspatialite 5.1.0 の C ソース規模**: 50+ ファイル、PROJ API 6+ 互換。cycle 1 で `OMIT_PROJ` define が利かない / configure 由来の generated header が必要、等の沼にはまった場合は libspatialite 4.3.0a へ降格 (spatialite-sys crate と同じバージョン、PROJ 5.x 互換性問題なし)。
- **GEOS の C++ ABI**: cycle 2 の `geos-src` ビルドが macOS / Linux でクロスプラットフォーム動作するかは要実測。詰まれば cycle 2 を v0.7 に切り出して cycle 3 / 4 のみで v0.6 出荷も検討 (= GEOS 不要な geometry I/O のみの bundled)。
- **全体縮退**: cycle 1 で詰まった場合は v0.7 に再縮退。`bundled-spatialite` feature は v0.5 状態のまま。

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
- 追加 RDB driver（MySQL / MariaDB / Oracle 等）— 既存 PostGIS / SQL Server で共通化済みの `shpx-rdb-common` を利用して URI/CRS/`OnLoss` 周りの boilerplate を共有する想定
- 対話的 REPL モード（`shpx repl`）
- 動的プラグイン（dylib / WASM）
- 追加フォーマット: FileGDB、DXF、KML、GML、TopoJSON、ラスター（GeoTIFF）
- 並列パイプライン最適化（`tokio` task で reader/writer 分離）
- CDC / streaming 同期モード（PostgreSQL → Parquet 増分追記など）
