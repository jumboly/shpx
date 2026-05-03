# Changelog

本プロジェクトの変更履歴。フォーマットは [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) に準拠し、バージョン番号は [Semantic Versioning](https://semver.org/spec/v2.0.0.html) に従う。

## [Unreleased]

## [0.5.0] - 2026-05-03

v0.5 マイルストーン「SpatiaLite」のリリース。`shpx-driver-spatialite` で SpatiaLite (`*.sqlite` / `*.db` / `*.spatialite` / `sqlite://`) の read/write を提供する。`rusqlite` (`bundled` SQLite) + `mod_spatialite` 動的ロード、自前 `shpx-geom::spatialite_blob` コーデック、`AddGeometryColumn` 経由の `geometry_columns` 登録、`GeomFromWKB(?, srid)` でのジオメトリ I/O、`--create-table` 3 種、`--create-index` 3 種 (`Always` / `Auto` で `SELECT CreateSpatialIndex(...)` の R\*Tree)、未登録 EPSG の `spatial_ref_sys` への best-effort `INSERT OR IGNORE`、SpatiaLite ↔ GPKG / SpatiaLite ↔ Shapefile の cross-driver 往復テストまでを含む。`bundled-spatialite` feature 宣言は driver / CLI の Cargo.toml に残るが、本実装は v0.6 に繰り延べ（workspace に `build.rs` の足場が無く、libspatialite が GEOS / PROJ にも依存して vendor 範囲が cycle 1 つに収まらないため）。詳細は `docs/SPATIALITE.md`。

### Added

- **shpx-driver-spatialite (v0.5 cycle 1、基盤と最小往復)**: `crates/shpx-driver-spatialite` を新規追加。`SpatialiteDriver` は `supported_schemes = ["sqlite", "db", "spatialite"]` を宣言し、`Capabilities { read, write, !bulk_load, supports_blob, !supports_decimal, supports_timestamp_tz, string_encoding: Fixed("utf-8") }` を提供する。`conn.rs` で `rusqlite::Connection::open_with_flags` → `load_extension(<path>, Some("sqlite3_modspatialite_init"))` → `SELECT InitSpatialMetadata(1)` (FastInit) を idempotent に発行する流れを確立。reader は `geometry_columns` で geometry 列を解決し `AsBinary(<geom>)` で WKB を取り出す。writer は `--overwrite` のみ対応の最小版で、`AddGeometryColumn` で geometry 列を登録 + 1 トランザクション + 行単位 prepared `INSERT INTO ... GeomFromWKB(?, srid)` で投入する。SQLite の declared type ↔ Arrow `DataType` のマッピング (`type_map.rs`) は GPKG driver と同形だが UInt64 のみ `Error::Schema` で拒否（SQLite INTEGER の上限が i64 のため）。env-gated 統合テスト (`SHPX_TEST_SPATIALITE`) で Point / LineString / Polygon / MultiPoint / MultiLineString / MultiPolygon の geometry 往復、Boolean / Int64 / Float64 / Utf8 の属性往復を 1 件ずつ確認。
- **shpx-geom (`spatialite_blob`)**: SpatiaLite blob (geometry binary) の自前 encode / decode を `spatialite_blob` モジュールに追加。XY のみ対応、Z / M / EMPTY / GeometryCollection は `Error::Geometry` で拒否。`encode` は LE 固定で MBR を WKB 走査から算出、`decode` は LE / BE 双方を読める。ユニットテスト 7 件 (roundtrip / MBR 値 / マーカー検証 / big-endian 互換) で I/O 整合性を担保。v0.5 では writer 経路は SpatiaLite 自身の `GeomFromWKB(?, srid)` に encode を委譲する方針 (自前 encode との実装ずれを回避) で、`spatialite_blob` モジュール自体は将来的な direct-bind 経路や v1.0 以降の Z/M 拡張のための基盤。
- **shpx-driver-spatialite (v0.5 cycle 2a、writer 拡張 + R\*Tree)**: `--create-table=if-not-exists|always|never` を PostGIS と同形セマンティクスで 3 種フル対応 (`Always` は `--overwrite` 無しでも DROP→CREATE する契約、`DiscardGeometryColumn` で `geometry_columns` / R\*Tree からも紐付き行を削除してから DROP)。`--create-index=auto|always|never` を実装し、`Always` / `Auto` (新規 CREATE TABLE 経路) で `LayerWriter::finish()` の最後に `SELECT CreateSpatialIndex(<table>, <geom>)` を発行 (PostGIS の GIST index と同方針で、batch 投入完了後に index を組む方が速いため)。SRID 解決は `--src-crs` > schema field metadata > `apply_on_loss` フォールバック (SRID 0 = SpatiaLite 慣習で unknown) の順。`spatial_ref_sys` の未登録 SRID には `Crs.wkt` → `shpx_geom::epsg_to_wkt1(code)` (4326 / 3857 / 4269 / 6668 同梱) の順で `srtext` を解決し、`INSERT OR IGNORE` で best-effort 登録 (PostGIS と同パターン)。`--overwrite=true && --create-table=never` は `shpx_rdb_common::validate_overwrite_compat` で整合性エラー。env-gated 統合テスト 9 件を `tests/writer_options.rs` に追加 (3 種 × 3 種 + 整合性エラー)。
- **shpx-driver-spatialite (v0.5 cycle 2b、bundled-spatialite を v0.6 へ繰り延べ + ROADMAP 訂正)**: `bundled-spatialite` feature の本実装 (libspatialite を C ソースから vendor して static link する build.rs) を v0.6 マイルストーンへ切り出し、v0.5 はシステム libspatialite (Linux: apt の `libsqlite3-mod-spatialite`、macOS: `brew install libspatialite`) 前提で出荷。理由は workspace に `build.rs` ファイルが一つも無く bundled C ビルドの足場がゼロであること、libspatialite が GEOS / PROJ にも依存して vendor 範囲が cycle 1 つに収まらないため。`bundled-spatialite` feature 宣言は `crates/shpx-driver-spatialite/Cargo.toml` と `crates/shpx-cli/Cargo.toml` に v0.6 予約として残し、有効化しても no-op (システム libspatialite を `load_extension` で見る挙動と同じ)。`docs/ROADMAP.md` の v0.5 / v0.6 セクションを訂正し、URI scheme は v0.5 で `sqlite` を SpatiaLite が専有することを確定 (`?mod_spatialite=true` フラグ運用は採用しない、content-sniffing は v1.0 以降)。
- **shpx-driver-spatialite (v0.5 cycle 3、cross-driver e2e + docs + 0.5.0 release)**: 完了基準テスト `tests/cross_driver_roundtrip.rs` を追加し、SpatiaLite ↔ GPKG (Point / LineString / Polygon / MultiPolygon) と SpatiaLite ↔ Shapefile (Point / Utf8 / Float64 / Boolean、SHP の DBF Numeric は Float64 へ降格するため整数列は scope 外) の往復が e2e で機能することを確認。`docs/SPATIALITE.md` を新設 (POSTGIS.md / SQLSERVER.md と同形構造)、`docs/DATA_TYPES.md` の `SQLite/GPKG` 列を `GPKG` / `SpatiaLite` の 2 列に分割、`docs/DESIGN.md` の URI scheme 表を訂正、`docs/CRS.md` / `docs/CONTRIBUTING.md` / `README.md` を更新。

### Internal

- **shpx-rdb-common 新設**: PostGIS / SQL Server の 2 driver で `options.rs` / `util.rs` / `writer.rs` に重複していた純粋ヘルパー（`percent_decode` / `query_pairs` / `split_qualified` / `resolve_table_name` / `validate_overwrite_compat` / `apply_on_loss` / `merge_crs` / `resolve_epsg_srid` / `driver_err` / `driver_msg` / `primitive`）を共通 crate `crates/shpx-rdb-common/` に抽出した。各 driver の API 公開面・エラーメッセージ・tracing target は変更なしで、CLI ユーザー視点の挙動には影響しない。次の RDB driver (MySQL 等) を追加する際の boilerplate 削減と、既存 2 driver の挙動を 1 箇所で揃える目的。`tracing::warn!(target: ...)` の target は const 要求のため、driver 側に `tracing::warn!` 呼び出しごとクロージャで残し、`shpx_rdb_common::apply_on_loss(kind, field, on_loss, warn_fn)` がそれを警告経路でだけ呼び出す設計とした。SpatiaLite driver もこの crate を再利用する (SQLite に schema 概念が無いため `split_qualified` / `resolve_table_name` は呼ばない点が差分)。詳細は `docs/CONTRIBUTING.md` の「RDB driver を追加する場合」節と `docs/DESIGN.md` のリポジトリ構成図を参照。

### Build

- workspace MSRV は 1.85 据え置き。新規依存はなし (`rusqlite` の `bundled` / `blob` / `chrono` / `load_extension` features を SpatiaLite driver で利用するが、いずれも GPKG driver で既に有効化済み)。
- CI (`.github/workflows/ci.yml`): test job (Linux) に `apt-get install -y libsqlite3-mod-spatialite` step を追加し、`SHPX_TEST_SPATIALITE=1` を環境変数で渡す。`crates/shpx-driver-spatialite/tests/{roundtrip,writer_options,cross_driver_roundtrip}.rs` は env 未設定なら eprintln + return で skip するため、SpatiaLite extension が無いローカル環境でも `cargo test --workspace` は緑のまま。
- macOS ローカル開発では `brew install libspatialite` 後に `SHPX_SPATIALITE_PATH=/opt/homebrew/lib/mod_spatialite.dylib` を立てる必要がある (Homebrew は標準のライブラリ検索パスに `mod_spatialite.dylib` を配置しないため)。

### Known Issues

- **`bundled-spatialite` は v0.6 で本実装**: 現状の feature 宣言は no-op で、有効化してもシステム libspatialite を `load_extension` で見る挙動と同じ。配布バイナリ向けの static link は v0.6 の `build.rs` 整備で対応する。それまでは `cargo install shpx --features bundled-spatialite` を実行しても extension のロードはランタイム経路を辿る。
- **SpatiaLite blob の Z / M / EMPTY / GeometryCollection 非対応**: v0.5 は XY のみ。`shpx-geom::wkb` / `shpx-geom::spatialite_blob` 双方の制限であり、PostGIS / SQL Server / GPKG とも同じ制限を共有する (v1.0 以降で拡張予定)。
- **reader filtering (`--where` / `--select` / `--query`) 未対応**: PostGIS の cycle 3a と同様の機能は v0.5+ の Future work。

## [0.4.0] - 2026-05-03

v0.4 マイルストーン「SQL Server」のリリース。`shpx-driver-sqlserver` で Microsoft SQL Server / Azure SQL の read/write を提供し、staging テーブル経由 bulk writer (案 B、`docs/DESIGN.md` L.219-)、`--create-table` 3 種、`--create-index=Always` での SPATIAL INDEX 生成、`?geom_type=geometry|geography` 切替、CI で docker mssql 経由統合テストまでを含む。**1000万行ベンチの完了基準値は Linux x86_64 環境で実測予定**（Apple Silicon の Rosetta/QEMU emulation 経由は参考値止まりのため）。詳細は `docs/SQLSERVER.md`。

### Added

- **shpx-driver-sqlserver (v0.4 cycle 1)**: SQL Server の最小 reader / writer。`mssql://user:pass@host:port/db?table=schema.name&geom_type=geometry|geography` URL で接続し、`tiberius 0.12` (`tds73` / `rustls` / `chrono` / `rust_decimal` features) を採用。driver crate 内 `OnceLock<tokio::runtime::Runtime>` で multi-thread runtime を 1 個共有して `block_on` で同期化（PostGIS と同形）。reader は `INFORMATION_SCHEMA` ではなく `sys.columns` + `sys.types` で列メタを取り、geometry 列は `[col].STAsBinary() AS [col], [col].STSrid AS [col__shpx_srid]` の併走 SELECT で WKB と SRID を一括取得する（SQL Server には PostGIS の `geometry_columns` view 相当が無いため、空テーブルでは SRID が取れない点に注意）。writer batch は `INSERT INTO ... VALUES (@P1, ..., {geometry|geography}::STGeomFromWKB(@PN, @PS))` の prepared INSERT で行単位投入。サポート型: Boolean / Int16-64 / Float32-64 / Decimal128 / Utf8 / Binary / Date32 / Timestamp(_, None|UTC) / geometry。`--where` / `--select` / `--query` reader 拡張は v0.5+。詳細は `docs/SQLSERVER.md` 参照。
- **shpx-driver-sqlserver (v0.4 cycle 2、staging bulk 案 B)**: `BulkLoadWriter` 実装。`Capabilities::bulk_load = true` に切替。tiberius は geometry/geography UDT の直接 bind を許さず TVP も非対応のため、接続スコープ local temp テーブル `#shpx_stage_<short_uuid>` (16 桁、自動 GC) に WKB + SRID を `tiberius::Client::bulk_insert` 経由で流し、`INSERT INTO target SELECT ..., {geometry|geography}::STGeomFromWKB(...) FROM #stage` で型変換しながら確定テーブルに転記する。chunk ごとに `BEGIN TRAN` / `COMMIT TRAN` を挟むことで tempdb log truncation を可能にし、10M 行投入でも tempdb 溢れが起きない設計。chunk size は `SHPX_MSSQL_BULK_CHUNK` env で override 可（既定 100,000、bench 時のみ 1,000,000 に上げる運用）。decimal は `rust_decimal::Decimal` 経由（tiberius 0.12 の生 Numeric write は scale 0 以外でバグがあるため `rust_decimal` feature 必須）。datetime2 / datetimeoffset / Date は tiberius の `IntoSql` impl をそのまま利用。
- **shpx-driver-sqlserver (v0.4 cycle 3a、writer 拡張)**: `--create-table=if-not-exists|always|never` を PostGIS と同形セマンティクスで 3 種フル対応（`Always` は `--overwrite` 無しでも DROP→CREATE する契約）。`--create-index=Always` で `CREATE SPATIAL INDEX [...] WITH (BOUNDING_BOX = (xmin, ymin, xmax, ymax))` を発行。geometry の BOUNDING_BOX は同梱表（4326 全球 / 3857 Web Mercator）から解決し、未知 SRID は明示エラー。geography は BOUNDING_BOX 不要。**`--create-index=Auto` は no-op**（PostGIS の Auto と挙動が違う点に注意）— SQL Server の SPATIAL INDEX は `geometry` 列で BOUNDING_BOX が必須で未知 SRID では失敗するため、暗黙生成は安全側に倒す。SRID 解決は `--src-crs` > schema field metadata > `apply_on_loss` フォールバックの順で、geometry の fallback は SRID 0、geography は 4326（geography は valid な geographic CRS が必須のため）。`--overwrite=true && --create-table=never` は driver 側で整合性エラー。env-gated 統合テスト 7 件を `tests/writer_options.rs` に追加。
- **shpx-driver-sqlserver (v0.4 cycle 3b、bench infra + 完了基準テスト)**: `crates/shpx-driver-sqlserver/benches/{bulk_insert.rs, gen.rs}` で criterion ベンチ harness を整備（`SHPX_TEST_SQLSERVER_URL` env-gate、`SHPX_BENCH_ROWS` で行数切替、`target/bench-data/` にキャッシュ生成）。`scripts/bench-vs-ogr-mssql.sh` は同 Parquet を shpx と `ogr2ogr -f MSSQLSpatial` 双方に流して `/usr/bin/time -p` の wall-clock 中央値を比較し、完了基準を `shpx_secs <= 1.667 * ogr_secs` (= shpx が ogr2ogr の 60% 以上の速度) で判定する。`tests/bulk_roundtrip.rs` に `bulk_all_types_together`（型網羅 bit-identical、tiberius 0.12 の既知不整合により一時的に `#[ignore]`、cover は単独テストで担保）と `bulk_geography_all_geom_types`（Point/LineString/CCW Polygon を geography で書ける確認）を追加。

### Build

- workspace MSRV は 1.85 据え置き。`tiberius` (default-features 切り、`tds73` / `rustls` / `chrono` / `rust_decimal` 有効化) / `tokio-util` (`compat`) / `rust_decimal` / `uuid` (`v4`) を `[workspace.dependencies]` に追加。
- `docker-compose.yml` に mssql service 追加（`mcr.microsoft.com/mssql/server:2022-latest`、Apple Silicon では `platform: linux/amd64` で emulation 起動、`MSSQL_MEMORY_LIMIT_MB=2048` で SA メモリ上限を明示）。image はユーザ DB を自動作成しないため、初回起動後に `docker exec shpx-mssql /opt/mssql-tools18/bin/sqlcmd ... -Q "CREATE DATABASE shpx_test"` を 1 度実行する。
- CI (`.github/workflows/ci.yml`): test job に `services.mssql` を追加し、`SHPX_TEST_SQLSERVER_URL=mssql://sa:Shpx_test_pw1!@localhost:1433/shpx_test` を環境変数で渡す。`Create shpx_test database in mssql` step で `IF DB_ID(...) IS NULL CREATE DATABASE` を冪等に発行。env 未設定時は eprintln + return で skip するため、SQL Server が無いローカル環境でも `cargo test` は緑のまま。

### Known Issues

- **tiberius 0.12 bulk encode の既知不整合**: 多列スキーマ (10+ 列) で `decimal(p, s)` と複数の `varbinary(max)` 列、または `datetime2` / `datetimeoffset` 列が混在すると、特定の列で `Token error: 'Invalid column type from bcp client'` を踏むケースがある。完了基準の各型 (decimal(38, 10) / timestamptz / bytea) は単独テストで bit-identical を確認済みで、`tests/bulk_roundtrip.rs::bulk_all_types_together` のみ一時的に `#[ignore]`。tiberius 上流に再現報告予定。
- **`--create-index=Always` は事前 PK 必須**: SQL Server の `CREATE SPATIAL INDEX` は仕様で clustered primary key を要求する。shpx 汎用 driver は `CREATE TABLE` で PK を勝手に付与しないため、`--create-index=Always` を使うには利用者が事前に PK 付きテーブルを作成して `--create-table=never` で append する運用になる。`--create-index=Auto` は no-op で安全側。
- **完了基準ベンチ値は Linux x86_64 で取得予定**: Apple Silicon では SQL Server image が amd64-only で emulation 必須。100k 行の smoke では shpx 1.28s (78k rows/s) を計測したが、これは emulation 経由の参考値で production を反映しない。10M 行 × 3 runs median は CI もしくは Linux ホストで取得する。

## [0.3.0] - 2026-04-25

v0.3 マイルストーン「PostGIS」のリリース。`shpx-driver-postgis` で PostgreSQL + PostGIS の read/write を提供し、COPY BINARY 経路の `BulkLoadWriter` と Decimal128 / timestamptz / bytea / EWKB の bit-identical 往復、`--where` / `--select` / `--query` reader、`--create-table` / `--create-index` writer、未登録 EPSG の `spatial_ref_sys` 自動 INSERT までを含む。10M 行 × 10 属性ベンチ（`scripts/bench-vs-ogr.sh`）で `ogr2ogr` の約 2.2 倍の速度（shpx 28.46 s / ogr2ogr 62.84 s / 比 0.453）を計測し、ROADMAP の v0.3 完了基準（`shpx ≤ 2.0 × ogr2ogr`）をクリア。詳細は `docs/POSTGIS.md` の Benchmark 節。

### Performance

- **shpx-driver-postgis (v0.3 cycle 3c)**: `crates/shpx-driver-postgis/benches/copy_binary.rs` + `gen.rs` で criterion ベンチ harness を整備（10 列 × 任意行数の合成 Parquet を `target/bench-data/` にキャッシュ生成、`SHPX_BENCH_ROWS` で行数切替、`SHPX_TEST_PG_URL` env-gate）。`scripts/bench-vs-ogr.sh` は同 Parquet を shpx と ogr2ogr 双方に流して `/usr/bin/time -p` の wall-clock 中央値を比較し、完了基準を `shpx_secs <= 2.0 * ogr_secs` で判定する。`.github/workflows/bench-smoke.yml` を `workflow_dispatch` 専用で追加し、CI 上で bench infra の smoke 確認が可能。`tests/bulk_roundtrip.rs::bulk_all_types_together` でベンチスキーマと 1:1 揃った 10 列同居 1k 行の bit-identical 往復テストを追加し、bench データの回帰検出器を兼ねる。

### Added

- **shpx-core (`WriteOpts` 拡張, v0.3 cycle 3b)**: `CreateTable { IfNotExists, Always, Never }` と `CreateIndex { Auto, Always, Never }` の 2 enum を `opts` モジュールに追加し、`WriteOpts` に同名フィールドを追加。RDB driver（PostGIS など）が CREATE TABLE / CREATE INDEX を制御するための受け口。ファイル driver は無視するため後方互換は保たれる。Default は `IfNotExists` / `Auto`。
- **shpx-cli (`convert --create-table / --create-index`, v0.3 cycle 3b)**: `convert` サブコマンドに `--create-table=if-not-exists|always|never`（既定 `if-not-exists`）と `--create-index=auto|always|never`（既定 `auto`）を追加。`--overwrite` とは直交し、`--overwrite && --create-table=never` は driver 側 `ResolvedWriteOpts::resolve` で整合性エラーになる。
- **shpx-driver-postgis (v0.3 cycle 3b)**: writer 拡張。`PostgisWriter::open` で `--create-table` の値に応じて `CREATE TABLE IF NOT EXISTS` / `CREATE TABLE` を切り替え、`Never` は `pg_class` で存在検証してから既存テーブルへ append する（無ければ `Error::Driver`）。GIST index は `LayerWriter::finish()` で `CREATE INDEX IF NOT EXISTS idx_<table>_<geom> ON <qualified> USING GIST (<geom_col>)` を発行し、bulk 経路では COPY 完了後に発行する（COPY 前に index があると遅くなる定石）。SRID 解決時に `spatial_ref_sys` を probe し、欠けていれば `Crs.wkt`（元データ由来、WKT1/WKT2 どちらでも）→ `shpx_geom::epsg_to_wkt1(code)` 同梱マップの順で `srtext` を解決し、`INSERT ... ON CONFLICT (srid) DO NOTHING` で best-effort 登録する。WKT が解決できない場合は INSERT スキップ（PostGIS の geometry 列定義は `spatial_ref_sys` 行が無くても CREATE/INSERT できるため）。env-gated 統合テスト 9 件を `tests/writer_options.rs` に追加。
- **shpx-core (`ReadOpts` 拡張, v0.3 cycle 3a)**: `where_clause` / `select` / `query` の 3 フィールドを追加。RDB driver（PostGIS など）が SQL に埋め込むための CLI 引数受け口。ファイル driver は無視するため後方互換は保たれる。
- **shpx-cli (`convert --where / --select / --query`, v0.3 cycle 3a)**: `convert` サブコマンドに `--where '<sql>'` / `--select c1,c2,...` / `--query 'SELECT ...'` を追加。`--query` は他 2 つと clap の `conflicts_with_all` で排他。`--select` は `value_delimiter = ','` で複数列を 1 引数で受ける。ファイル URI に対してこれらが指定された場合は tracing 警告で告知し、driver は静かに無視する。
- **shpx-driver-postgis (v0.3 cycle 3a)**: reader を 「table モード」と「query モード」の 2 経路に分割。table モードでは `?table=` で解決した完全修飾名に `--where` / `--select` を埋め込み、`SELECT col1, ..., ST_AsEWKB(geom) FROM "schema"."table" [WHERE <sql>]` を生成する。query モードではユーザ SQL を `SELECT * FROM (<query>) AS shpx_q LIMIT 0` でサブクエリ化して `tokio_postgres::Statement::columns()` から列メタを取り、`Type::name() == "geometry"|"geography"` で geometry 列を検出して本番 SQL を再構築する。SRID 解決は table モードでは `geometry_columns` view → 先頭 `ST_SRID()` の 2 段、query モードはサブクエリ経由の先頭 `ST_SRID()` のみ。geometry 列を含まない `--select` / `--query` は `Error::Driver` で停止し、`--query` 中の `;` も同様に停止する。
- **shpx-driver-postgis (v0.3 cycle 2)**: PostgreSQL の binary COPY format を自前エンコードする `BulkLoadWriter` 経路。`crates/shpx-driver-postgis/src/copy_binary.rs` に `BulkRowEncoder` と各型の big-endian エンコーダ（bool / int2-8 / float4-8 / text / bytea / date / timestamp / timestamptz / numeric / geometry-EWKB）を実装。`tokio_postgres::CopyInSink<Bytes>` で `COPY <table> (<cols>) FROM STDIN BINARY` に流し込み、複数 `RecordBatch` をまたいで 1 接続 = 1 COPY セッションで送る。`Capabilities::bulk_load = true` / `supports_decimal = true` に切替。
- **shpx-driver-postgis (Decimal128)**: Arrow `Decimal128(p, s)` ↔ PG `numeric(p, s)` を双方向対応。binary 表現は NBASE=10000 の `PgNumeric { ndigits, weight, sign, dscale, digits[] }` で、`PgNumeric` は `tokio_postgres::types::ToSql` を独自実装し batch / bulk 両経路で同じ encode 結果を共有する。reader は PG `NUMERIC` OID + `pg_attribute.atttypmod` から `(p, s)` を復元（typmod=-1 のときは `(38, 0)` フォールバック）。decimal(38, 10) bit-identical 往復テスト追加。
- **shpx-cli (`--insert-mode=auto|bulk|batch`)**: `convert` サブコマンドに insert mode を追加。既定 `auto` は driver の `Capabilities::bulk_load` が true なら bulk、そうでなければ batch（silently fallback）。`bulk` 明示時は非対応 driver でエラー。`batch` 明示時は常に `LayerWriter::write_batch` 経路。PostGIS 以外の driver は現状 batch 一択のため挙動は変わらない。
- **shpx-core**: `Driver::open_bulk_write` メソッドを default impl (`Ok(None)`) 付きで `Driver` trait に追加。`BulkLoadWriter` を実装する driver はこれを override して `Box<dyn BulkLoadWriter>` を返す。CLI 側は `Capabilities::bulk_load` でゲートしてから呼び出す。
- **shpx-driver-postgis (v0.3 cycle 1)**: PostgreSQL + PostGIS の最小 reader / writer。`pg://` / `postgres://` / `postgresql://` URL で接続し、`?table=schema.name` または環境変数 `SHPX_PG_TABLE` でテーブルを指定する。`tokio-postgres` (`with-chrono-0_4` feature) を採用し、driver crate 内 `OnceLock<tokio::runtime::Runtime>` で multi-thread runtime を 1 個共有して `block_on` で同期化する。reader は `SELECT ST_AsEWKB(geom), ... FROM tbl` を発行し、writer は `--overwrite` で `DROP TABLE IF EXISTS` → `CREATE TABLE` → 1 トランザクション + prepared `INSERT INTO ... VALUES (..., ST_GeomFromEWKB($N))` を行う。サポート型: Boolean / Int16-64 / Float32-64 / Utf8 / Binary / Date32 / Timestamp(_, None|UTC) / geometry。SRID は `Crs::epsg_code()` または `geometry_columns` view → 先頭行 `ST_SRID()` の順で解決する。`--where`/`--select`/`--query`、`--create-table`、GIST index、未登録 EPSG の `spatial_ref_sys` 自動 INSERT、Z/M は cycle 3 で対応。詳細は `docs/POSTGIS.md` 参照。
- **shpx-geom**: PostGIS EWKB (Extended WKB) の encode/decode を `ewkb` モジュールに追加。`encode_with_srid` で標準 WKB に SRID flag (`0x20000000`) を立て SRID i32 を挿入、`strip_srid` / `decode` で EWKB から SRID と標準 WKB を分離する。Z/M flag は cycle 1 では `Error::Geometry` で拒否する。
- **shpx-core**: `Uri::from_path` に URL スキーム検出を追加。先頭が `<scheme>://` 形式なら scheme を抽出し、`pg`/`postgres`/`postgresql` は `pg` に正規化する。ローカルパスの拡張子推論は従来通り。`Uri::is_url()` ヘルパ追加。

### Changed

- **shpx-cli**: `ConvertArgs`/`InfoArgs`/`SchemaArgs` の `src` / `dst` を `PathBuf` から `String` に変更。`pg://...` 等の URL を OS パスとして解釈すると壊れるため（特に Windows のドライブレター扱い）。`commands/{convert,info,schema}.rs` で `Uri::from_path(args.src)` のまま渡す。

### Build

- workspace MSRV は 1.85 据え置き。`tokio` / `tokio-postgres` / `postgres-types` / `bytes` / `futures-util` を `[workspace.dependencies]` に追加。
- `docker-compose.yml` をリポジトリルートに追加（ローカル開発用 PostGIS）。
- CI (`.github/workflows/ci.yml`): test job に `services.postgis` を追加し、`SHPX_TEST_PG_URL=pg://shpx:shpx@localhost:5432/shpx_test` を環境変数で渡す。`shpx-driver-postgis/tests/roundtrip.rs` および cycle 2 で追加した `tests/bulk_roundtrip.rs` は env 未設定なら eprintln + return で skip するため、PostGIS が無いローカル環境でも `cargo test` は緑のまま。

## [0.2.0] - 2026-04-25

v0.2 マイルストーン「GPKG / GeoJSON / CSV / FlatGeobuf + Reprojection」のリリース。

### Added

- **shpx-driver-fgb**: FlatGeobuf (`.fgb`) reader/writer。公式 Rust 実装 `flatgeobuf` 6.0 (BSD-2-Clause) を採用し、`default-features = false` で HTTP feature を排除。geometry の入出力は `geozero` 経由で WKB ↔ FGB FlatBuffers を変換する。Bool / Byte / UByte / Short / UShort / Int / UInt / Long / ULong / Float / Double / String / Binary / DateTime の各 ColumnType に対応。Date32 と Timestamp は ISO8601 文字列で `DateTime` 列に書き、reader 側は観測値の形式から `Date32` か `Timestamp(Microsecond, UTC)` に絞り込む。CRS は EPSG コード優先 + WKT2 フォールバック。**packed Hilbert R-Tree インデックスは出力しない** (`index_node_size=0` 固定)。Z/M / GeometryCollection / `select_bbox` / null geometry / `Json` 列の構造化保持は未対応 (詳細は `docs/FGB.md` の「制限」を参照)。
- **shpx-driver-csv**: WKT 列付き CSV/TSV reader/writer（`.csv` / `.tsv`）。geometry 列は `geometry` / `geom` / `wkt` / `the_geom` のいずれか、または `SHPX_CSV_GEOMETRY_COLUMN` 環境変数で明示。geometry 以外の列は全て `Utf8` として読み書きする（型推定なし）。CSV 固有オプションは暫定で環境変数経由（`SHPX_CSV_*`）。詳細は `docs/CSV.md` を参照。
- **shpx-driver-geojson**: GeoJSON FeatureCollection (`.geojson`) と GeoJSON Lines / NDJSON (`.geojsonl` / `.ndjson` / `.jsonl`) の reader/writer。属性 (properties) の JSON 型を Arrow 列に推論（Bool / Int64 / Float64 / Utf8、混在は昇格）。出力は RFC 7946 §4 準拠の EPSG:4326 固定で、非 WGS84 入力は内部 `Reprojector` で自動変換する（cycle 5）。旧仕様の top-level `crs` メンバ（`urn:ogc:def:crs:EPSG::NNNN` / `urn:ogc:def:crs:OGC:1.3:CRS84`）の解釈に対応。GeometryCollection と Z/M 座標は未対応。詳細は `docs/GEOJSON.md` を参照。
- **shpx-driver-gpkg**: GeoPackage 1.3 (`.gpkg`) reader/writer。`rusqlite` (`bundled` SQLite) ベースで `gpkg_spatial_ref_sys` / `gpkg_contents` / `gpkg_geometry_columns` を初期化。geometry blob は envelope_type=0 / Standard / LE 固定で書き出し、reader は全 envelope_type と LE/BE を読み飛ばす。テーブル名は URI クエリ `?table=<name>` または環境変数 `SHPX_GPKG_TABLE` / `SHPX_GPKG_OUT_TABLE` で指定可能（未指定で feature テーブル単一なら自動採用、複数なら候補を列挙してエラー）。CRS は EPSG コード優先 + WKT1 フォールバック。Z/M 座標、空間インデックス、複数レイヤ append、`gpkg_extensions` は未対応（`docs/GPKG.md` の Future work 参照）。
- **shpx-geom**: GeoPackage Binary header の encode/decode を `gpkg_blob` モジュールに追加。GPKG/SpatiaLite で再利用できる独立コーデック。
- **shpx-geom**: WKT (Well-Known Text, OGC SFA 1.2.1) の encode/decode を `wkt` モジュールに追加。XY のみ対応、`EMPTY` は未サポート。
- **shpx-geom: Reprojector**: `proj 0.28` クレート（システム libproj を pkg-config で検出）経由で WKB ↔ WKB の座標変換を行う。`Proj::new_known_crs` が `proj_normalize_for_visualization` を適用するため lat-lon 系も traditional XY 順で扱える。`Proj` は `!Send` のため `Reprojector` は CRS spec 文字列のみ保持し、`Proj` は `(src_spec, dst_spec)` キーで thread-local キャッシュする。`shpx_geom::for_each_coord_mut` で `Geom` の各 variant を走査する補助関数も追加。
- **shpx-cli (`--reproject`)**: `convert` サブコマンドに `--reproject <SPEC>` を追加。`EPSG:xxxx` / WKT2 / proj-string / PROJJSON を受理する。入力 CRS が解決できない場合は `--src-crs` の指定を促すエラーで停止する。同一 CRS 指定時は no-op パスにフォールスルー。出力先 driver に渡す `Crs` 引数と Arrow schema の field metadata の双方を target に揃えるため、出力ファイルの CRS タグも反映される。
- **shpx-cli (`bundled-proj` feature)**: 配布用単一バイナリ（`cargo-dist`）向けに `shpx-cli` の `bundled-proj` feature を追加。有効化すると `proj` クレートが libproj/SQLite を C ソースから static link する（`cmake` / `clang` 必須）。default ビルドはシステム libproj を pkg-config 経由で利用するため、これらの追加ビルドツールを要求しない。

### Changed

- workspace MSRV を 1.79 → 1.85 に引き上げ。FlatGeobuf ドライバが依存する `flatgeobuf` 6.0 が `rust-version = 1.85` を要求するため。CI matrix も `["1.85", "stable"]` に更新。
- **GeoJSON writer**: 非 EPSG:4326 入力でのエラー停止を内部 `Reprojector` 経由の自動 WGS84 変換に置き換え（RFC 7946 §4 準拠）。入力 CRS が解決できない場合のみ `Error::Crs` で停止する挙動は維持。

### Build

- CI (`.github/workflows/ci.yml`): clippy / test ジョブで `libproj-dev` / `pkg-config` を apt インストール。`proj` クレートのビルドに必要。

## [0.1.0] - 2026-04-25

v0.1 マイルストーン「コア骨格 / SHP ↔ GeoParquet PoC」のリリース。

### Added

- **shpx-core**: `Driver` / `LayerReader` / `LayerWriter` / `BulkLoadWriter` トレイト、`Schema` / `Capabilities` / `Crs` / `Uri` / `ReadOpts` / `WriteOpts` / `OnLoss` 型。
- **shpx-geom**: WKB encoder/decoder（little/big endian、Point/LineString/Polygon/MultiPoint/MultiLineString/MultiPolygon）、WKT1 `.prj` からの EPSG コード抽出、最小 PROJJSON 生成。
- **shpx-driver-shp**: Shapefile reader/writer。`.shp`/`.shx`/`.dbf`/`.prj`/`.cpg` サイドカー対応、DBF cpg 文字エンコーディング切替（utf-8/cp932/latin-1）、Decimal128 / Date32 / Utf8 の保全。
- **shpx-driver-parquet**: GeoParquet 1.0 reader/writer。Arrow `Binary` 列に WKB を格納、KeyValue メタの `geo` JSON で CRS（PROJJSON）と geometry_type を伝搬。
- **shpx-cli**: `shpx convert <src> <dst>` サブコマンド（`--overwrite` / `--on-loss` / `--encoding` / `--batch-size` / `--src-crs`）、`shpx info <src>` サブコマンド。`-v`/`-vv`/`-vvv` で tracing ログレベル制御、`RUST_LOG` も尊重。
- 拡張子から driver を推論する static レジストリ（v0.2 で `inventory` ベースに移行予定）。
- CI: `cargo fmt --check` / `cargo clippy --all-targets -- -D warnings` / `cargo test --workspace` を MSRV 1.79 と stable の 2 toolchain で実行。

### Notes

- CRS は EPSG コードのみで保持・伝搬する。`--reproject` による座標変換は v0.2 で `proj` クレート統合とともに実装予定。
- PostGIS / SQL Server / SpatiaLite / GeoPackage / GeoJSON / FlatGeobuf / CSV は後続マイルストーン (v0.2–v0.5) で対応する。
- ライセンスは v1.0 までに最終決定する（MIT / Apache-2.0 dual を想定）。

[Unreleased]: https://github.com/jumboly/shpx/compare/v0.5.0...HEAD
[0.5.0]: https://github.com/jumboly/shpx/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/jumboly/shpx/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/jumboly/shpx/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/jumboly/shpx/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/jumboly/shpx/releases/tag/v0.1.0
