# SpatiaLite ドライバ仕様

SQLite + SpatiaLite 拡張の geometry テーブルを `shpx-driver-spatialite` が担当する。GDAL 非依存方針に従い、`rusqlite` (`bundled` SQLite) と `mod_spatialite` 動的ロードのみで構成される。

GPKG driver と同じ「ファイルベースの SQLite」だが、メタテーブル (`gpkg_*` vs `geometry_columns` / `spatial_ref_sys`) と geometry 列の blob format (GPKG binary header vs SpatiaLite blob) が異なる。`*.gpkg` は GPKG driver、`*.sqlite` / `*.db` / `*.spatialite` / `sqlite://` は SpatiaLite driver と URI scheme で完全に分離されている。

## 対応範囲（v0.5 リリース時点）

- 読み:
  - `sqlite://path?table=<name>` または `*.sqlite` / `*.db` / `*.spatialite` ファイル拡張子で接続
  - geometry 列は `AsBinary(<col>)` で標準 WKB を取得し、SRID は `geometry_columns.srid` を参照
  - `geometry_columns` テーブルが無いファイルは「SpatiaLite データベースではない」として明示的にエラー
  - `?table=` 未指定時は `geometry_columns` の単一行から自動採用、複数なら候補を列挙してエラー
- 書き:
  - **batch 経路** のみ（`Capabilities::bulk_load = false`）。1 トランザクションで prepared `INSERT INTO ... GeomFromWKB(?, srid)` を行単位投入
  - **テーブル作成戦略**: `--create-table=if-not-exists|always|never`（既定 `if-not-exists`）。PostGIS と同形セマンティクス。`--overwrite=true && --create-table=never` は driver 側で整合性エラー
  - **R\*Tree 自動生成**: `--create-index=auto|always|never`（既定 `auto`）。`Always` は `SELECT CreateSpatialIndex(<table>, <geom>)` を `LayerWriter::finish()` で発行。`Auto` は新規 CREATE TABLE 経路でのみ発行（PostGIS と同形）
  - **未登録 EPSG 自動登録**: SRID 解決時に `spatial_ref_sys` を probe し、欠けていれば best-effort で `INSERT OR IGNORE INTO spatial_ref_sys (...)` を発行
  - geometry 列は `AddGeometryColumn(<table>, <geom>, srid, <type>, 'XY')` で `geometry_columns` に登録
- ジオメトリ型: Point / LineString / Polygon / MultiPoint / MultiLineString / MultiPolygon（XY のみ）
- CI 上の Linux runner で `apt install libsqlite3-mod-spatialite` 経由 + `SHPX_TEST_SPATIALITE=1` で SpatiaLite ↔ GPKG / SpatiaLite ↔ Shapefile の cross-driver 往復テストが緑

## サポート対象 URI スキーム

| スキーム / 拡張子 | 正規化後 scheme | 備考 |
|---|---|---|
| `*.sqlite` | `sqlite` | ファイル拡張子推論 |
| `*.db` | `db` | ファイル拡張子推論 |
| `*.spatialite` | `spatialite` | ファイル拡張子推論 |
| `sqlite://path?table=...` | `sqlite` | URL 形式 |
| `db://...` / `spatialite://...` | 各々同名 | URL 形式（未推奨だが受理） |

`shpx-driver-spatialite` の `supported_schemes` は `["sqlite", "db", "spatialite"]` を宣言する。`*.gpkg` は別 driver（GPKG）が処理するため、SpatiaLite 拡張を有効化済みの GPKG ファイルでも GPKG driver で開く。content-sniffing による自動振り分け（同じ拡張子の SQLite ファイルが GPKG なのか SpatiaLite なのかを内容で判定する）は v1.0 以降の検討事項。

## URI と接続オプション

```
sqlite:///abs/path/to/db.sqlite?table=places
```

- ファイルパスは絶対 / 相対のいずれも可
- `?table=<name>` でテーブルを指定（query parameter）
  - 未指定時の reader: `geometry_columns` テーブルから単一行を自動採用、複数あれば候補列挙でエラー
  - 未指定時の writer: ファイル名 stem (`<file>.sqlite` の `<file>`) を `sanitize_table_name` で正規化して使用（最終 fallback は `"features"`）
- 環境変数 `SHPX_SPATIALITE_TABLE` でも代替指定可能（URI クエリが優先）
- 環境変数 `SHPX_SPATIALITE_OUT_TABLE` で writer 専用の override（reader 入力テーブル名と分けたいケース用）

## CRS の扱い

### 読み出し

優先順位:

1. `ReadOpts.src_crs`（CLI の `--src-crs`）
2. `geometry_columns.srid` の値（PostGIS の SRID と互換、SpatiaLite では「unknown」を SRID 0 と扱う慣習）
3. SRID > 0 のとき `Crs::from_epsg(srid)`

`spatial_ref_sys` から `srtext` / `proj4text` を直接引く処理は v0.5 では行わない（v0.3 PostGIS と同方針、必要なら `--src-crs` で WKT を直接指定する）。

### 書き出し

- `crs.epsg_code()` あり → `AddGeometryColumn` の SRID 引数に流す。INSERT 時の `GeomFromWKB(?, srid)` にも同 SRID を埋める
- CRS が無い → `--on-loss` に従う:
  - `error`（既定）: `Error::OnLoss { kind: "missing-crs-on-spatialite" }` で停止
  - `warn`: `srid=0` で書き込み + tracing 警告
  - `skip`: `srid=0` で書き込み（無音）

#### 未登録 EPSG の `spatial_ref_sys` 自動登録

SRID が決まったあと、`spatial_ref_sys` に該当 srid 行が無ければ best-effort で 1 行 INSERT する。`Crs.wkt`（元データ由来、WKT1/WKT2 どちらでも）→ `shpx_geom::epsg_to_wkt1(code)`（同梱の主要 EPSG マップ: 4326 / 3857 / 4269 / 6668）の順で `srtext` を解決する。並列実行と既存登録との競合を避けるため `INSERT OR IGNORE INTO spatial_ref_sys (...)` を発行する。SpatiaLite の geometry 列は `spatial_ref_sys` 行が無くても CREATE / INSERT できるため、ベストエフォートで充分（PostGIS と同方針）。

## writer 拡張オプション

### `--create-table=if-not-exists|always|never`

| 値 | 挙動 | `--overwrite=true` との組み合わせ |
|---|---|---|
| `if-not-exists`（既定）| `CREATE TABLE IF NOT EXISTS` を発行。既存テーブルがあればそのまま append | DROP→CREATE IF NOT EXISTS（実質 always と同じ） |
| `always` | `DROP TABLE IF EXISTS` → `CREATE TABLE`。`geometry_columns` / R\*Tree からも対応行を削除する | 同上 |
| `never` | CREATE を一切発行せず、既存テーブルへ append。テーブルが無ければ `Error::Driver` | **整合性エラー**（CLI / `ResolvedWriteOpts::resolve` で reject） |

`never` で既存テーブルへ書き込む際、列スキーマの事前検証は行わない。型・列順・列名のミスマッチは `INSERT` 実行時の SQLite エラーに任せる（PostGIS と同方針）。

### `--create-index=auto|always|never`

SpatiaLite は R\*Tree モジュールを `SELECT CreateSpatialIndex(<table>, <geom>)` で生成する。生成された R\*Tree は `idx_<table>_<geom>` 等の補助テーブル群に結びつく。

| 値 | 挙動 |
|---|---|
| `auto`（既定）| `CreateTable::Never` 経路では作らない（既存テーブルに勝手に index を張らない）。それ以外（`IfNotExists` / `Always`）では発行する。|
| `always` | `--create-table` の値に関わらず常に発行。既存 R\*Tree がある場合は `CreateSpatialIndex` が SpatiaLite 内部で no-op として扱う。|
| `never` | 一切発行しない。 |

R\*Tree は `LayerWriter::finish()` の最後に発行する（batch 経路で大量行を書いた後に一括で組む方が速いため、PostGIS の GIST index と同方針）。

## ジオメトリ (SpatiaLite blob)

SpatiaLite は独自の binary 形式で geometry 列を格納する。`shpx-geom::spatialite_blob` モジュールに以下の I/O を集約している:

- `encode(geom, srid)` — WKB → SpatiaLite blob (LE 固定、MBR は WKB 走査で算出)
- `decode(blob)` — SpatiaLite blob (LE / BE 双方) → `(WKB, SRID)`

ただし v0.5 の writer 経路では encode/decode 整合性確保のため、INSERT 時は SpatiaLite 自身の `GeomFromWKB(?, srid)` 関数に encode を委譲している。reader は `AsBinary(<geom>)` で標準 WKB を取得するため `spatialite_blob::decode` を経由しない（モジュールはユニットテストで I/O 整合性を確認済み）。

対応範囲:

- XY のみ。Z / M / EMPTY / GeometryCollection は `Error::Geometry` で拒否（v1.0 以降で拡張）
- MBR は WKB 走査で算出（`min_x`, `min_y`, `max_x`, `max_y` を 4×f64 で先頭に埋める）

## データ型マッピング

| Arrow `DataType` | SQLite 宣言型 | 備考 |
|---|---|---|
| `Boolean` | `BOOLEAN` | 0 / 1 で保存（SQLite は INTEGER 親和性） |
| `Int8` | `TINYINT` | |
| `Int16` | `SMALLINT` | |
| `Int32` | `MEDIUMINT` | |
| `Int64` / `UInt8` / `UInt16` / `UInt32` | `INTEGER` | SQLite の INTEGER は最大 i64 |
| `UInt64` | — | i64 上限を超え得るため `Error::Schema` で拒否 |
| `Float16` / `Float32` | `FLOAT` | |
| `Float64` | `DOUBLE` | |
| `Utf8` / `LargeUtf8` | `TEXT` | UTF-8 固定 |
| `Binary` / `LargeBinary` | `BLOB` | |
| `Date32` / `Date64` | `DATE` | ISO8601 文字列 (`YYYY-MM-DD`) で保存 |
| `Timestamp(_, _)` | `DATETIME` | ISO8601 (`YYYY-MM-DDTHH:MM:SS[.fff][Z|±HH:MM]`) で保存。tz 付きは UTC 換算後に `Z` で書く |
| `Decimal128(p, s)` / `Decimal256(p, s)` | `TEXT` | 文字列降格（`Capabilities::supports_decimal = false`） |
| geometry 列 | `BLOB` (SpatiaLite blob) | `AddGeometryColumn` で `geometry_columns` に登録 |

逆方向（SQLite → Arrow）は `geometry_columns` の宣言型を SQLite の declared type 文字列として読み、`type_map::decl_to_arrow` で Arrow `DataType` に戻す。`INTEGER` 等の量子化情報を持たない declared type は `Int64` / `Float64` / `Utf8` などの「最大限保全する」型に倒れる。

## 損失変換

- CRS 無し → `apply_on_loss("missing-crs-on-spatialite", ...)`
- 未登録 EPSG → `spatial_ref_sys` への自動 INSERT を実装（WKT 解決可能な場合のみ、ベストエフォート）
- `Decimal128` / `Decimal256` → `TEXT` 文字列化（`supports_decimal = false`）。bit-identical 往復はしない

## スコープ外（v0.6 以降）

v0.5 リリース時点で以下は未対応:

- **`bundled-spatialite` の本実装**（v0.5 では feature 宣言のみ、有効化しても no-op）。v0.6 で `build.rs` から libspatialite を `cc` で vendor して static link する予定。詳細は `docs/ROADMAP.md` の v0.6 節を参照
- **reader filtering** (`--where` / `--select` / `--query`)。RDB driver 共通の v0.5+ 課題
- **Z / M 座標**、`GeometryCollection`、`EMPTY`
- **複数レイヤ append**（同一ファイルに複数 geometry テーブルを段階的に追加するワークフロー）
- **bulk writer**（`Capabilities::bulk_load = false` 固定。SQLite 単体では行単位 INSERT が pragmatic な最速ルート）

## 環境変数まとめ

| 変数 | 役割 |
|---|---|
| `SHPX_SPATIALITE_TABLE` | 入力（および書き出しの fallback）テーブル名（URI クエリ `?table=` が優先） |
| `SHPX_SPATIALITE_OUT_TABLE` | 書き出し時のテーブル名 override |
| `SHPX_SPATIALITE_PATH` | `mod_spatialite` 共有ライブラリの絶対 / 相対パス上書き（macOS の `/opt/homebrew/lib/mod_spatialite.dylib` など） |
| `SHPX_TEST_SPATIALITE` | integration test の有効化フラグ（未設定 / `0` / 空文字列なら test を skip） |

## 内部実装メモ

- `Capabilities { read: true, write: true, bulk_load: false, supports_blob: true, supports_decimal: false, supports_timestamp_tz: true, string_encoding: Fixed("utf-8") }`
- `mod_spatialite` ロード経路の優先順位:
  1. `feature = "bundled-spatialite"` 時は static link した `spatialite_init` を直接呼ぶ（v0.6 で実装、v0.5 は動的 fallback）
  2. `SHPX_SPATIALITE_PATH` env で指定された絶対 / 相対パスを `load_extension`
  3. 既定: `mod_spatialite` を SQLite に渡し、OS のライブラリ検索パス (`LD_LIBRARY_PATH` / `DYLD_LIBRARY_PATH` / `/etc/ld.so.conf`) から dlopen
- `open_write_new` は `journal_mode=WAL` / `synchronous=NORMAL` を pragma 設定して、空 DB で `InitSpatialMetadata(1)` を idempotent 発行する。`FastInit (=1)` は WGS84 系のみ seed する選択肢で、全 EPSG seed (`InitSpatialMetadata(0)`) は数秒かかるため空 DB を頻繁に作る用途では使わない
- writer の `INSERT` は `GeomFromWKB(?, srid)` でジオメトリを SpatiaLite に encode してもらう。自前の `spatialite_blob::encode` で書く実装と差異があると検出が困難なため、I/O 整合性は SpatiaLite 自身の関数に揃える方針。reader 経路でも同様に `AsBinary(<geom>)` で標準 WKB を取り出す
- `--create-table=Always` は `DROP TABLE IF EXISTS <table>` の前に `DELETE FROM geometry_columns WHERE f_table_name = ?` と `SELECT DiscardGeometryColumn(?, ?)` を発行して、SpatiaLite の管理テーブル / R\*Tree からも紐付き行を消してから DROP する
- `shpx-rdb-common` の `validate_overwrite_compat` / `apply_on_loss` / `merge_crs` を再利用（PostGIS / SQL Server と同パターン）。SQLite に schema 概念がないため `split_qualified` / `resolve_table_name` は呼ばない
- INSERT バインドは `chrono` クレート経由で `NaiveDate` (Date32/Date64) / `DateTime<Utc>` (Timestamp) を `rusqlite::types::Value::Text` の ISO8601 文字列にエンコードする。`rusqlite` の `chrono` feature を有効化している
- 内部 `fid` 列を `INTEGER PRIMARY KEY` で常に追加する。`rusqlite` の `last_insert_rowid` 等の挙動と整合させるため、`AddGeometryColumn` の前に PK を確保する設計

## Future work

- Z / M 座標と `GeometryCollection` の対応（`shpx-geom::wkb` 側の拡張と同期）
- `--where` / `--select` / `--query` reader 拡張（PostGIS と同形）
- `bundled-spatialite` の本実装（v0.6 セクション参照）
- 複数 geometry テーブルの一括書き出し
- `spatialite_history` 等の SpatiaLite 固有 metadata の取り扱い
