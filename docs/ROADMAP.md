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
- [x] chunk size 1M でも tempdb 溢れなし（chunk ごと commit）— v1.0 cycle 1 の `bench-smoke-mssql` workflow で 10M 行 × 3 runs の完走を確認 (median 138.53s on ubuntu-latest)
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
- [x] SpatiaLite ↔ GPKG / Shapefile の往復
- [x] 空間インデックス（R*Tree）のオプション作成 (`--create-index=always` で `CreateSpatialIndex` 発行)

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

## v0.7 — Driver Feature Parity & Refactor

**背景**: v0.1 → v0.6 の段階的 driver 追加で、新 driver (PostGIS / SQL Server / SpatiaLite) と古い driver (SHP / Parquet / GPKG / GeoJSON / CSV / FGB) の間に整合性ギャップが残った。1.0 を「全 driver 一貫した動作」で切るため、parity backport と refactor を 1 マイルストーンに集約する。3 並列 Explore 監査で発見したギャップ (`/Users/masa/.claude/plans/velvety-percolating-hinton.md` 参照) のうちデータ正しさに直結する欠落を v0.7 で塞ぎ、配布工程は v1.0 に分離する。

**スコープ**:
- データ正しさに直結する OnLoss 経路の欠落修正 (Parquet / GeoJSON)
- 新 driver の reader filtering 機能 (`--where` / `--select` / `--query`) を SpatiaLite に backport
- 入力 CRS metadata の reader 側読み込み (Parquet GeoParquet / FlatGeobuf / GPKG 確認)
- `shpx-rdb-common` の重複コード (`percent_decode` / `apply_on_loss` 呼び出しパターン) を全 RDB-ish driver で統一
- Cross-driver roundtrip matrix を SpatiaLite scope から CLI scope に拡張
- `docs/ON_LOSS.md` 新設、各 driver doc の Limitations / Future Work 章を統一形式に

**完了基準**:
- [ ] `cargo test --workspace` 緑 + 全 env-gated 統合テスト緑
- [ ] `grep "fn percent_decode" crates/` がヒットするのは `crates/shpx-rdb-common/` のみ
- [ ] CLI レベルの cross-driver matrix テスト (`crates/shpx-cli/tests/cross_driver_matrix.rs`) で主要 pair (PostGIS ↔ SQL Server / SpatiaLite、PostGIS ↔ Parquet / FGB / GeoJSON / SHP) が緑
- [ ] `docs/ON_LOSS.md` 新設、driver × loss kind の表が網羅されている

**サブ cycle 構成** (v0.3 以降と同じく cycle ごとに `/clear` して clean に再開する):

- **cycle 1 — data-correctness 修正**（完了）: (1) `crates/shpx-driver-parquet/src/util.rs` に `apply_on_loss` ヘルパと空の `loss_kind` module を整備 (現状の Parquet writer は `coerce_types=false` 固定で発火経路を持たないため、`precision-on-parquet` / `nanosecond-truncation-on-parquet` / `z-on-parquet` / `m-on-parquet` の実定数追加は v0.8+ に繰り延べ。dead_code scaffold + ロスなし確認テストで cycle 1 内は閉じる)、(2) `crates/shpx-driver-geojson/src/writer.rs` の silent demotion (Decimal / Timestamp_tz) を `apply_on_loss` 経由に置換 (`decimal-on-geojson` / `timestamp-precision-on-geojson`)、(3) `crates/shpx-driver-spatialite/src/reader.rs` に PostGIS 同型の table mode / query mode 分岐を実装し `--where` / `--select` / `--query` を backport。`shpx_rdb_common::apply_on_loss` (`crates/shpx-rdb-common/src/on_loss.rs`) を Parquet / GeoJSON でも再利用 (RDB common は名前と裏腹に純粋ヘルパで、ファイル driver から呼んでも問題ない)。CSV driver の OnLoss クロージャ (`crates/shpx-driver-csv/src/util.rs`) を手本にする。
- **cycle 2 — CRS metadata reader & refactor**: (1) Parquet reader で Arrow field metadata の `geo` JSON を `shpx_geom::projjson::decode` で parse、(2) FGB reader で header の `crs` field を parse、(3) GPKG reader の `gpkg_geometry_columns.srs_id` → `gpkg_spatial_ref_sys` 経路を確認し抜けがあれば補完、(4) GPKG / SpatiaLite / SQL Server の自前 `percent_decode` を `shpx_rdb_common::percent_decode` に統一、(5) GPKG `apply_on_loss` を直接 `tracing::warn!` 呼び出しから rdb-common closure パターンに変更、(6) `crates/shpx-cli/tests/cross_driver_matrix.rs` を新設し PostGIS ↔ SQL Server / SpatiaLite / Parquet / FGB / GeoJSON / SHP の主要往復を env-gate で網羅 (既存 `crates/shpx-driver-spatialite/tests/cross_driver_roundtrip.rs` は driver scope の回帰検出として残す)。
- **cycle 3 — docs + ROADMAP 更新 + 0.7.0 release**（完了）: `docs/ROADMAP.md:128-129` の v0.5 完了基準チェック (`[ ]` → `[x]`、実体は v0.5 cycle 2a / 3 で実装済み) を訂正、`docs/ON_LOSS.md` を新設 (driver × loss kind 表 + 各 kind の発生条件 + `--on-loss=error|warn|skip` の動作仕様)、`docs/{CSV,GEOJSON,GPKG,FGB}.md` を `docs/{POSTGIS,SQLSERVER,SPATIALITE}.md` と同形 (Quick Start / Capabilities / Limitations / Future Work) に揃え、`CHANGELOG.md` に v0.7.0 セクション、workspace `Cargo.toml` を `0.7.0` へ bump、release commit + `v0.7.0` annotated tag。

---

## v0.8 — Streaming Reader Parity

**背景**: `LayerReader::batches()` (`crates/shpx-core/src/driver.rs:63-75`) は構造的にストリーミング iterator API だが、現状 9 driver 中 **Parquet のみが真のストリーミング** で、残り 8 driver は `open()` 内で全行を `Vec` / `VecDeque` に展開する eager-load 方式。10M 行クラスの入力では reader 開時のピーク RSS が GB 級にスパイクし、v1.0 cycle 2 で導入した進捗バーが「読み終わってから書き始める」体感を悪化させる。writer は既に全 driver で streaming 化されているため、conversion パイプラインのメモリ bottleneck は reader 側にのみ存在する。v1.0 配布工程の前に reader streaming 化を完了させ、`1.0.0` 出荷時のメモリプロファイルを一貫させる。

**スコープ**:
- 全 9 driver の reader を真のストリーミング化（peak RSS が batch サイズ + 接続バッファで頭打ち）
- `LayerReader` trait は **据え置き**（現行シグネチャで全 driver 対応可能）
- Reader 内部実装の置き換えのみ（CLI / writer / tests / benches は呼び出し変更不要）
- メモリプロファイルベンチ整備（peak RSS 測定）
- `docs/STREAMING.md` 新設

**完了基準**:
- [x] 全 driver の reader が `open()` 後に「全行を保持する Vec / VecDeque」を field に持たない（cycle 1〜5 で grep 確認、`Vec<Feature>` の名残は GeoJSON の N=1024 サンプル用バッファのみ、関数スコープに閉じている）
- [x] `cargo test --workspace` 緑 + 全 env-gated 統合テスト緑（PostGIS は cycle 4 でローカル実機 + tests/reader_cancel.rs 追加、SQL Server は CI で確認、SpatiaLite は CI で env-gated）
- [x] 各 RDB driver で 10M 行 reader の peak RSS < 1 GB（cycle 7 の `bench-peak-rss.yml` を `rows=10000000` で 2026-05-04 に dispatch、PostGIS 93.2 MB / SQL Server 94.3 MB。詳細は `docs/STREAMING.md`）
- [x] 各 file driver で 10M feature reader の peak RSS < 256 MB（同上計測で SHP のみ 429 MB と超過し期待値を < 512 MB に緩和、その他 6 driver は < 100 MB。SHP の 429 MB は `shapefile` 0.6 の `iter_shapes_and_records` が SHX index と DBF を内部で全読みするためで、driver 側の chunk 化では削れない。`shapefile` crate の memory map 化は v1.x 候補に切り出し）
- [x] `docs/STREAMING.md` に driver × streaming 戦略 × peak RSS の表が記載

**確定済み設計判断**:
- **`LayerReader` trait は据え置き**: `Box<dyn Iterator + Send + '_>` の `'_ = &'_ mut self` で各 driver は内部 field（reader inner / Connection / mpsc::Receiver）を借用しつつ Iterator を返せる。Parquet パターン (`crates/shpx-driver-parquet/src/reader.rs:139-142`) を SHP / FGB / CSV / GPKG / SpatiaLite に展開、PostGIS / SQL Server は async stream → sync iter ブリッジ。
- **trait に `close()` 追加なし**: streaming reader の cleanup は Drop で自動化（rusqlite `Statement` / tokio_postgres `Client` / tiberius `Client` は全て Drop でクリーンクローズ）。
- **shapefile / flatgeobuf は実は lazy iterator 提供済み**: `shapefile::Reader::iter_shapes_and_records()` は `ShapeRecordIterator<'_>` を返す lazy `Iterator` (shapefile 0.6 reader.rs L.155-167)、`flatgeobuf::FeatureIter<R, NotSeekable>` は `FallibleStreamingIterator` を実装 (flatgeobuf 6.0 file_reader.rs L.237)。eager-load しているのは shpx 側の `Vec::push` ループのみ。`crates/shpx-driver-shp/src/reader.rs:42-45` の「ファイル先頭から再列挙する設計」コメントは誤りなので修正対象。
- **async-to-sync ブリッジ手法**: PostGIS / SQL Server とも background thread + `std::sync::mpsc::sync_channel(buf=2)` 方式。Reader struct が `JoinHandle<()>` を所有し、Drop で channel receiver を drop → background tokio task が send 失敗で自動停止。runtime は既存の `OnceLock<Runtime>` singleton (`crates/shpx-driver-postgis/src/runtime.rs:19`, `crates/shpx-driver-sqlserver/src/runtime.rs`) を再利用。
- **SQLite 系の self-referential 回避**: rowid keyset pagination で batch ごとに `SELECT ..., rowid FROM <table> WHERE rowid > ? ORDER BY rowid LIMIT 65536` を `prepare_cached` で発行（GPKG / SpatiaLite 共通）。`Statement<'_>` と `Rows<'_>` を struct field に同居させる self-referential 設計は採らず、batch 内で `Vec<rusqlite::types::Value>` に展開してから次へ進む（`Statement` / `Rows` lifetime は batches() の next() スコープ内で閉じる）。`--query` モード（任意 SQL）は rowid 列が保証されないため LIMIT/OFFSET フォールバック。
- **GeoJSON FeatureCollection の streaming**: serde_json は features 配列の chunked parse を直接 API として提供しないため、`crates/shpx-driver-geojson/src/stream.rs` を新設し `serde_json::Deserializer::from_reader(...)` を low-level に進めて `"features"` array element を `StreamDeserializer<_, Feature>` 相当で逐次 yield する。NDJSON は `Deserializer::into_iter::<Feature>()` で素直に streaming。
- **GeoJSON 型推論の妥協**: 現在 `infer_columns` は全 Feature を walk するが、streaming 化に伴い「最初の N=1024 feature サンプル」方式に降格。N より後で型不一致が見つかれば既存 `Utf8` demote ロジックで継続。サンプル数は環境変数 `SHPX_GEOJSON_INFER_SAMPLE` で override 可。
- **shpx-rdb-common に streaming helper を追加**: `crates/shpx-rdb-common/src/streaming.rs` を新設し、rowid keyset pagination iterator (`KeysetRowsIter`) を GPKG / SpatiaLite で共有。既存モジュール (`uri.rs`, `on_loss.rs`, `arrow.rs`, `opts.rs`, `table.rs`, `crs.rs`) と同列。
- **メモリベンチ infrastructure**: `crates/shpx-core/src/bench_util.rs` を新設し、Linux 限定で `/proc/self/status` の `VmRSS` を読む `peak_rss_kib()` ヘルパを置く。各 driver の `benches/reader_streaming.rs` で利用。`procfs` crate を workspace dep に追加 (Linux only、cfg gated)。CI 計測は ubuntu-latest 限定。

**サブ cycle 構成** (v0.3 以降と同じく cycle ごとに `/clear` して clean に再開する):

- **cycle 1 — file 系 easy wins (SHP + FGB + CSV)**（完了）: `crates/shpx-driver-shp/src/reader.rs` の `pending: VecDeque<...>` を削除し、worker thread + `sync_channel(2)` で chunk (65536 行) を main へ送る方式で streaming 化。`shapefile::Reader::iter_shapes_and_records()` は `current_pos = HEADER_SIZE` で常に先頭リセットするため借用ベースの batch iter は成立せず (ROADMAP の「ファイル先頭から再列挙する設計コメントは誤り」記述は誤り、実装で修正)、worker スレッド方式に切替。`crates/shpx-driver-fgb/src/reader.rs` の `rows: VecDeque<Row>` を削除し `feature_iter: FeatureIter<BufReader<File>, NotSeekable>` を field 化、`FallibleStreamingIterator::next()` を `BatchIter::next_batch()` で 65536 回呼ぶ。DateTime → Date32 refine は最初の 65536 件 sample でファイル全体を覆えた場合のみ適用 (越えた場合は header 宣言型 `Timestamp` を据え置き)。`crates/shpx-driver-csv/src/reader.rs` の `body: Option<String>` を `Box<dyn Read + Send>` に置換、`csv::Reader::from_reader(...)` を field 化、UTF-8 は BufReader + BOM strip で真の streaming、非 UTF-8 (Shift_JIS 等の稀ケース) は eager decode + `Cursor` 暫定運用。geometry 型 sniff は file を 2 回 open する 2-pass 方式。`cargo test --workspace` 全件緑、`cargo clippy --workspace --all-targets -- -D warnings` 緑、`grep -rE "load_all|VecDeque<.*Row|all_records|body: .*String" crates/shpx-driver-{shp,fgb,csv}/src` がコード本体ヒットゼロ。peak RSS 数値計測は cycle 6 の bench infra で一括実施。
- **cycle 2 — SQLite 系 (GPKG + SpatiaLite) + 共通 helper**（完了）: `crates/shpx-rdb-common/src/streaming.rs` 新設で `KeysetRowsIter<'a>` (`&'a mut Connection` を借用 — `Connection: !Sync` のため `&Connection` では `Send` を満たせず、`&mut` 借用で Send 維持。`prepare_cached` で reuse、batch ごとに `last_rowid` を保持して `WHERE rowid > ?1 ORDER BY rowid LIMIT ?2` を発行、`Statement` / `Rows` lifetime は `next_batch()` スコープ内に閉じて self-referential を回避) と `--query` モード用 `OffsetRowsIter<'a>` (`LIMIT/OFFSET` 版) を実装。`shpx-rdb-common/Cargo.toml` に `rusqlite = { workspace = true, features = ["bundled"] }` を dep 追加。`crates/shpx-driver-gpkg/src/reader.rs` の `load_all_rows` / `rows: VecDeque<Row>` を削除し、`Connection` / `sql_template` を field に保持、`batches()` で `KeysetRowsIter::new(&mut self.conn, ...)` を返す案 B 構造に置換、`SELECT COUNT(*)` で row_count_hint を 1 度だけ算出。`crates/shpx-driver-spatialite/src/reader.rs` も同型 + `--query` モードは `OffsetRowsIter`、WITHOUT ROWID テーブルは `is_without_rowid_table()` で検出して `Error::Driver` で明示拒否 (LIMIT/OFFSET fallback は採らず、巨大テーブルでスキャン暴発するリスクを回避)。`grep -E "load_rows|load_all_rows" crates/shpx-driver-{gpkg,spatialite}/src` ヒットゼロ。GPKG roundtrip / cross-driver matrix 緑、SpatiaLite は CI Linux で env-gated 緑想定 (macOS dev 環境は mod_spatialite 不在で env-gated test は pre-existing skip)。peak RSS は cycle 6 で計測。
- **cycle 3 — GeoJSON streaming (NDJSON + FeatureCollection)**（完了）: `crates/shpx-driver-geojson/src/stream.rs` を新設し、自前 `FcFeatureStream` (FeatureCollection 用 JSON state machine、`[` までシーク → `,` 区切りで 1 feature ずつ pull) と `NdjsonStream` (NDJSON 用、`BufRead::lines()` ベース) を実装。`reader.rs` は file head 1 MiB を probe して top-level `crs` メンバを抽出 → 先頭 N=1024 feature をサンプリングして型推論 → 本番ストリームはファイル再 open + 真の逐次 yield、の 3-pass 構造に再編。サンプル数は `SHPX_GEOJSON_INFER_SAMPLE` env で override 可。`geojson::FeatureReader` 直用ではなく自前を書いた理由は、上流 0.24 の空配列 panic と Iterator 終了後 panic の 2 バグ回避。
- **cycle 4 — PostGIS reader streaming**（完了）: `Vec<RecordBatch>` field と `chunk_batch` ヘルパを撤廃し、background OS thread + `std::sync::mpsc::sync_channel(2)` で `tokio_postgres::query_raw` の `RowStream` を逐次消費する構造へ置換。worker は reader 専用に新規 connect した `Client` を所有し、`READ_BATCH_SIZE` (65536) 行ごとに `RecordBatch` を組んで channel へ送る。`tests/reader_cancel.rs` を新設し「Reader mid-iter drop で worker が SELECT を解放する」ことを env-gated 検証 (`reader_drop_releases_select_promptly` / `reader_full_consume_drops_cleanly`)。実装メモ: 当初プランの `tokio::select!` ベースは不要で、`tx.send(...).is_err()` のリターンだけで clean abort 可能だった (channel disconnect は send 経由で検知できる)。
- **cycle 5 — SQL Server reader streaming**（完了）: cycle 4 と同パターンで `tiberius::QueryStream` を逐次消費。`exec_select` / `extract_srid_from_first_row` / `chunk_batch` を削除、SRID 解決は probe `SELECT TOP 1 ... STSrid` 一本に集約。`QueryItem::Metadata` トークンは worker 側で読み飛ばす (列メタは describe_columns で別途取得済み)。
- **cycle 6 — メモリベンチ + docs + 0.8.0 release**（完了）: `crates/shpx-core/src/bench_util.rs` を新設し `peak_rss_kib() -> Option<u64>` を提供 (Linux: `/proc/self/status` の `VmHWM`、その他 OS: `None`)。`procfs` crate は使わず std だけで完結 (依存最小化)。`docs/STREAMING.md` を新設し driver × streaming 戦略 × 期待 peak RSS の表、キャンセル挙動、トランザクション保持期間、バッチサイズ影響、関連実装ファイルへのリンクを 1 箇所に集約。`CHANGELOG.md` に v0.8.0 セクション、workspace `Cargo.toml` を `0.8.0` へ bump。peak RSS の数値計測は cycle 7 (post-release follow-up) に分離 (Linux CI でしか計測できないため、リリース commit と独立した PR にする方が review しやすい)。
- **cycle 7 — peak RSS bench infra (v0.8 follow-up、release 後)**: v0.8 完了基準の peak RSS チェックボックス 2 件を消化するベンチ基盤。当初 cycle 6 では「driver 個別の `benches/reader_streaming.rs` に分離」と記載したが、9 driver × ボイラープレート + 共通 schema 同期コストを避けるため、**集約した bin crate `crates/shpx-bench-rss/` 1 個** に方針変更 (`publish = false`、内部ベンチ専用)。CLI: `shpx-bench-rss --driver=<name> --rows=<N> [--input-dir=<path>] [--reset] [--prepare-only] [--read-only] [--postgis-url=<url>] [--sqlserver-url=<url>]`。stdout に `{"driver":..., "rows":..., "row_count":..., "peak_rss_kib":..., "elapsed_ms":...}` の JSON 1 行を出力 (`/proc/self/status::VmHWM` はリセット不可なので **1 プロセス 1 計測**)。`src/data.rs` のスキーマは PostGIS bench `gen.rs` (10 属性 + Point geometry) ベースだが、GPKG / SpatiaLite reader が全 integer 列を Int64 に正規化する仕様 (`crates/shpx-driver-gpkg/src/reader.rs`) と整合させるため `class` 列のみ Int32 → Int64 に置換、cache file 名は `bench_<rows>.parquet` に分離して PostGIS bench `points_<rows>.parquet` と共存させる。サブ cycle 分割 (`/clear` 区切り):
    - **7a (完了)**: skeleton crate + `data.rs` (Parquet 入力 generator) + `runner.rs` (peak RSS 計測) + `BenchDriver` trait + Parquet driver entry。ローカル smoke 緑 (`--driver=parquet --rows=1000` で JSON 出力、macOS では `peak_rss_kib=null`)。
    - **7b (完了)**: file 系 7 driver (SHP / FGB / CSV / GeoJSON FC / GeoJSON NDJSON / GPKG / SpatiaLite) の prepare + open_read 経路を実装。`prepare_via_writer` 共通 helper で Parquet 入力を native format に複製、driver 別 OnLoss policy (SHP / CSV / GeoJSON は `Skip`、FGB / GPKG / SpatiaLite は `Warn`) を渡す。SHP は `.shp/.shx/.dbf/.prj/.cpg` sidecar、SQLite 系 (GPKG / SpatiaLite) は `-wal/-shm` sidecar の cache 再生成時削除を実装。ローカル smoke 緑 (parquet / shp / fgb / csv / geojson-fc / geojson-ndjson / gpkg、`--rows=100`)、SpatiaLite は macOS 環境では `mod_spatialite` 不在で skip (CI で確認)。
    - **7c (完了)**: RDB 系 2 driver (PostGIS / SQL Server) の prepare + open_read 経路を実装。`prepare_via_bulk` 共通 helper で `open_bulk_write` 経路を優先利用 (PostGIS は COPY BINARY、SQL Server は staging bulk)、URL は `clap::env` 経由で `SHPX_TEST_PG_URL` / `SHPX_TEST_SQLSERVER_URL` から取得。table 名は `shpx_bench_rss_<rows>` で毎回 DROP→CREATE。ローカル smoke は docker 不要にしてあるため CI で確認。
    - **7d (完了; CI 数値反映待ち)**: `.github/workflows/bench-peak-rss.yml` を新設 (workflow_dispatch only、`rows` input)。job 構成: `file-drivers` matrix (parquet / shp / fgb / csv / geojson-fc / geojson-ndjson / gpkg、libproj-dev 1 本で動く 7 driver)、`spatialite` 単独 (apt `libsqlite3-mod-spatialite` の dynamic link)、`postgis` services (`postgis/postgis:16-3.4`)、`sqlserver` services (`mcr.microsoft.com/mssql/server:2022-latest`)。各 job が `target/bench-output/<driver>.json` を artifact として upload。ubuntu-latest x86_64 で 10M 行を手動 dispatch して計測値を取得し、`docs/STREAMING.md` の peak RSS 表を「期待値 → 計測値 (10M 行、ubuntu-latest x64、YYYY-MM-DD)」に置換、ROADMAP v0.8 完了基準 2 件を `[x]` に。**バイナリ動作には影響しないため `v0.8.1` patch release は作らず、ROADMAP / docs のみ更新**。

**リスクと縮退判断**:
- **GeoJSON FeatureCollection low-level parser**: serde_json の Deserializer を root から手動進める実装は複雑度が高く、cycle 3 内で完成しない可能性あり。詰まれば FeatureCollection は eager-load を据え置き、NDJSON のみ streaming で v0.8 出荷（`docs/GEOJSON.md` に「巨大 FeatureCollection は NDJSON 推奨」を明記）。完了基準の peak RSS 閾値も FeatureCollection は除外。
- **SpatiaLite WITHOUT ROWID テーブル**: cycle 2 で WITHOUT ROWID テーブル / 明示 PK 不在のテーブルは LIMIT/OFFSET フォールバックでも対応可能だが大きい OFFSET でスキャンが遅延する。詰まれば「LIMIT/OFFSET フォールバックは小規模テーブル限定、巨大テーブルは rowid 必須」と docs に明記して妥協出荷。
- **async-to-sync mpsc bridge の cancel 伝播**: SIGINT で Reader を Drop した時、tokio task が中途半端な query 状態のまま残る可能性。`tokio::select!` で channel sender close を select 対象に入れて clean abort する。cycle 4-5 で test 化必須。
- **v1.0 への影響**: v0.8 が長引けば v1.0 出荷が遅延する。cycle 4 (PostGIS) または cycle 5 (SQL Server) で詰まれば、その driver は eager-load を据え置きで v0.8 を 0.8.0 release し、残りは v0.9 に切り出す（マイルストーン縮退）。

---

## v1.0 — 仕上げと配布

**スコープ** (v0.7 で parity を済ませた前提で、配布工程に集中):
- v0.4 完了基準の SQL Server 10M 行ベンチ (Linux x86_64) を取り切り、ROADMAP v0.4 のチェックを `[x]` に
- `LICENSE-MIT` / `LICENSE-APACHE` / `NOTICE` をリポジトリルートに配置 (MIT/Apache-2.0 dual)
- `indicatif` で進捗バー (`shpx convert` 時)、`LayerReader::row_count_hint` の各 driver 実装を統一
- `examples/*.sh` 新設、README をチュートリアル形式に再構成
- `shpx schema` / `shpx drivers` に `--format=text|json` を追加
- `cargo-dist` で macOS(arm64/x64) / Linux(x64/arm64) / Windows(x64) のバイナリリリース
- macOS / Windows の `bundled-spatialite` smoke job を CI に追加 (Linux smoke は v0.6 cycle 4 で既存)
- `1.0.0` tag + GitHub Releases 自動 publish

**スコープ外 (v1.x 以降に後送り)**: crates.io 公開 / Homebrew tap / Docker image (ghcr.io) — それぞれ独立した整備工程が必要なため `1.0.0` 出荷後の追加マイルストーンに切り出す。

**完了基準**:
- [ ] SQL Server 10M 行ベンチが Linux x86_64 で `shpx_secs <= 1.667 * ogr_secs` を満たし、`docs/SQLSERVER.md` の Benchmark 節と `CHANGELOG.md` 0.4.0 Known Issues を訂正
- [ ] `LICENSE-MIT` / `LICENSE-APACHE` / `NOTICE` がリポジトリルートに存在
- [ ] `shpx convert` で進捗バー (確定行数なら ProgressBar、不明なら Spinner) が表示され、`--quiet` で抑止できる
- [ ] `examples/01-shp-to-parquet.sh` ほか 5+ シナリオが実機で緑
- [ ] `cargo dist build --target=x86_64-unknown-linux-gnu` で tarball 生成、CI で macOS / Linux / Windows smoke job が緑 (Windows は best-effort 許容)
- [ ] `v1.0.0` tag push → cargo-dist が 5 target の release artifact を GitHub Releases に publish

**サブ cycle 構成** (v0.3 以降と同じく cycle ごとに `/clear` して clean に再開する):

- **cycle 1 — v0.4 ベンチ + LICENSE / NOTICE**: 既存 `crates/shpx-driver-sqlserver/benches/{bulk_insert,gen}.rs` + `scripts/bench-vs-ogr-mssql.sh` を Linux x86_64 (CI workflow_dispatch or 直接ホスト) で実行し 10M 行 × 3-run median を取得。`docs/SQLSERVER.md` Benchmark 節 / `CHANGELOG.md` 0.4.0 Known Issues / `docs/ROADMAP.md:95` を訂正。`/Users/masa/src/shpx/LICENSE-MIT` / `/Users/masa/src/shpx/LICENSE-APACHE` / `/Users/masa/src/shpx/NOTICE` を新設 (NOTICE は libspatialite / GEOS / PROJ / SQLite / arrow-rs 等の third-party listing を aggregate、既存 `crates/shpx-driver-spatialite/NOTICE` は driver scope のまま残す)。
- **cycle 2 — 進捗バー + on-loss completeness**: workspace dep に `indicatif = "0.17"` 追加、`crates/shpx-cli/src/commands/convert.rs` に ProgressBar / Spinner 統合と `--quiet` フラグ追加。各 driver で `LayerReader::row_count_hint` の実装を確認・統一 (DB 系は table mode で `SELECT COUNT(*)`、Parquet は row group meta、SHP は header record count、FGB は `features_count`、GPKG / SpatiaLite-file は `SELECT COUNT(*)`、CSV / GeoJSON / NDJSON は `None`)。v0.7 cycle 3 で新設した `docs/ON_LOSS.md` の loss kind 表を完成させ、抜け落ちている driver × kind を埋める (例: timestamp ns 切り捨ては Parquet と FGB で同じ kind 名)。
- **cycle 3 — examples + README + schema/drivers JSON**: `examples/01-shp-to-parquet.sh` / `02-shp-to-postgis.sh` / `03-postgis-to-fgb.sh` / `04-reproject.sh` / `05-on-loss.sh` / `06-bulk-load.sh` を新設 (test data は `examples/data/` に同梱 or scripts で生成)。`README.md` を再構成し「5 分チュートリアル」(SHP → GeoParquet → PostGIS → reproject) と examples へのリンクを追加。`crates/shpx-cli/src/commands/{schema,drivers}.rs` に `--format=text|json` を追加 (drivers は `[{ name, schemes, capabilities }]` 形式)。
- **cycle 4 — cargo-dist + multi-platform CI**: workspace `Cargo.toml` に `[workspace.metadata.dist]` (targets: 5 OS、`installers = ["shell"]`、`bundled-spatialite` / `bundled-proj` 有効化)、`cargo-dist generate-ci github` で `.github/workflows/release.yml` を生成。`.github/workflows/ci.yml` に `bundled-smoke-macos` / `bundled-smoke-windows` job を追加 (macOS は `brew install cmake`、Windows は chocolatey で `cmake` / `clang`)。Linux smoke (`bundled-spatialite-smoke`) は既存。`docs/SPATIALITE.md` の bundled 節に「macOS arm64 + Linux のみ Release、Windows / macOS x64 は best-effort」の縮退方針を明記。
- **cycle 5 — 1.0.0 release**: `docs/ROADMAP.md` v1.0 完了基準チェックを全部 `[x]`、`CHANGELOG.md` Unreleased → `[1.0.0] - YYYY-MM-DD` (compare URL も `1.0.0...HEAD` に更新)、workspace `Cargo.toml` `version` を `0.8.0` → `1.0.0`、`release: v1.0.0` 1 commit + `v1.0.0` annotated tag。`git push --tags` 後 cargo-dist が GitHub Release を自動生成、shell installer のダウンロード URL を `README.md` の「install 方法」節に反映。後送り項目 (crates.io / Homebrew / Docker / その他 UX) を `docs/ROADMAP.md` v1.x 候補に追記。

**リスクと縮退判断**:
- **macOS / Windows の bundled build**: cycle 4 で Windows の `cmake` / `clang` 経由 `bundled-spatialite` build がリンカ問題で詰まる可能性。詰まれば Windows を best-effort 扱いで Release から除外し、Linux + macOS arm64 のみで `1.0.0` 出荷。`docs/SPATIALITE.md` で対応 OS を明記する。
- **SQL Server ベンチの環境**: cycle 1 で Linux x86_64 環境が手元になく CI 経由でも安定計測できない場合は、ベンチ取得を v1.x に分離して `1.0.0` を切ることを検討 (ただし v0.4 完了基準は引き続き `[ ]` のまま)。

---

## v1.x 以降（候補）

- crates.io 公開 (`cargo publish` の metadata 整備とパスワード管理が独立工程のため v1.0 出荷後)
- Homebrew tap (`jumboly/homebrew-shpx`) — formula は cargo-dist 出力に依存
- Docker image (`ghcr.io/jumboly/shpx`) — bundled-spatialite で base image が膨らむため最適化込みで切り出し
- **SQL Server クライアント戦略 (decision tree)** — 現状 `shpx-driver-sqlserver` は `tiberius 0.12.3` (Apache 2.0、prisma/tiberius) を使用しているが、`bulk_insert` に **未修正の combinatorial bug 群** がある (`tests/bulk_roundtrip.rs::bulk_all_types_together` が `#[ignore]`、bench-rss schema も同パターンで踏む)。実証された再現条件: 「多列 + decimal + 連続 `varbinary(max)`」が揃うと `Invalid column type from bcp client for colid N` を踏む。datetimeoffset 単独 / decimal 単独はそれぞれ通る (それぞれ別の test で実証)。時刻型は被害報告位置であり引き金ではない。**tiberius のメンテ状況**: archived ではないが「最後の意味あるコード変更が 2025-02」「最新 release `0.12.3` が 2024-07」「open issues 131、bulk 系は #312/#302/#322/#352/#373/#410 等が長期 open、特に #410 (2026-03) が我々の bug と同種の column-order 依存 BCP 失敗」。upstream の active 開発は事実上停止しており「待つ」戦略は非現実的。**判断ツリー**:
    - **短期 (v1.0 出荷時)**: tiberius のまま据え置き。bench-rss は multi-row `VALUES` batch INSERT (下記 sub-entry) で 1h → 10 分。本番 driver は最小 schema で bulk が動く + 型網羅は batch fallback で動く現状を維持。
    - **中期 (v1.1〜v1.2)**: bug の縮退再現 (列 1 つずつ抜いて trigger 列の組み合わせを特定) → 上流 tiberius に PR、merge を待たず fork (`crates/shpx-tiberius/` または別リポ) で patch を保持。Apache 2.0 なので fork コストは license ゼロ、保守工数のみ。修復後に `bulk_all_types_together` の `#[ignore]` を外し bench-rss も bulk 復帰。
    - **長期 (v2.0 構造変更案)**: tiberius fork でも詰まる / 上流が完全に止まる場合は `odbc-api` (Microsoft 純正 ODBC driver 経由) への乗り換えを検討。**配布哲学のトレードオフ**: shpx は v0.6 以降「pure Rust + 単一バイナリ + system dep ゼロ」を志向しており、odbc-api は `msodbcsql18` のホスト install を要求するため `cargo-dist` 戦略の修正が必要。`bcp` subprocess は bench-rss の prepare 用途のみの限定経路としては有用だが本番 driver には組み込まない (Rust library 哲学外)。`sqlx` は MSSQL backend を 0.7 で削除済みかつ実装が tiberius wrapper だったため代替にならない。
    - **トリガー条件**: 中期に進む合図は (a) v1.x 利用者から bulk + 型網羅の正式要望が届く、(b) 別の tiberius bulk bug を踏む、(c) tiberius repo が archived になる の 3 つのいずれか。
    - **直交する v2 era の最適化**: MS-SSCLRT (Microsoft Spatial CLR Type) UDT エンコーダで `geometry` / `geography` の native binary を直接生成し `BulkLoadRequest` に乗せる案。現状の staging table 経由 (`#shpx_stage_<uuid>` に WKB を流し `INSERT ... SELECT ... STGeomFromWKB(...)` で確定テーブルへ転記) を 1-pass 化できる。上記 client 戦略 (tiberius / fork / odbc-api) どれの上でも実装可能。
- RDB writer の multi-row `VALUES` batch INSERT — 短期に bench-rss SQL Server prepare の 1h を縮める実装案。現状 `LayerWriter::write_batch` は 1 行ごとに 1 RPC を発行する。SQL Server は 1 RPC あたり 2100 param の上限があるので、`chunk_rows = floor((2100 - margin) / params_per_row)` で **自動算出した chunk** を `INSERT INTO t (...) VALUES (?,?,...), (?,?,...), ...` に詰める方式に変更すると round-trip が ~175× 削減される。bench-rss の SQL Server prepare は 1h4m → 8〜15 分の試算 (`docs/STREAMING.md` の SQL Server peak RSS 計測コスト削減)。本番 driver の `--insert-mode=batch` も同時に高速化。実装は `shpx-rdb-common::multirow_chunk_rows(params_per_row, max_params_per_rpc)` を共通ヘルパとして置き、SQL Server / 将来の MySQL (param 上限 65535) / Oracle で再利用。LOB を多く含む schema では payload byte cap (16 MB 目安) を併用。**tiberius bulk bug を回避できる副次効果あり** (bulk_insert を踏まないため)。
- 追加 RDB driver（MySQL / MariaDB / Oracle 等）— 既存 PostGIS / SQL Server で共通化済みの `shpx-rdb-common` を利用して URI/CRS/`OnLoss` 周りの boilerplate を共有する想定
- 対話的 REPL モード（`shpx repl`）
- 動的プラグイン（dylib / WASM）
- 追加フォーマット: FileGDB、DXF、KML、GML、TopoJSON、ラスター（GeoTIFF）
- 並列パイプライン最適化（`tokio` task で reader/writer 分離）
- SHP reader の peak RSS 削減 (10M Point で 429 MB → < 256 MB)。`shapefile` 0.6 の `iter_shapes_and_records` が SHX index / DBF を内部で全読みするため、`shapefile` crate を fork または memory map 経路を入れる必要あり
- CDC / streaming 同期モード（PostgreSQL → Parquet 増分追記など）
