# Changelog

本プロジェクトの変更履歴。フォーマットは [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) に準拠し、バージョン番号は [Semantic Versioning](https://semver.org/spec/v2.0.0.html) に従う。

## [Unreleased]

v0.3 マイルストーン「PostGIS」の cycle 1 + cycle 2 + cycle 3a 進捗。cycle 3 は 3a/3b/3c に分割済み（`docs/ROADMAP.md` 参照）。

### Added

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

[Unreleased]: https://github.com/jumboly/shpx/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/jumboly/shpx/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/jumboly/shpx/releases/tag/v0.1.0
