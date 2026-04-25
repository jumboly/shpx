# PostGIS ドライバ仕様

PostgreSQL + PostGIS 拡張のテーブルを `shpx-driver-postgis` が担当する。GDAL 非依存方針に従い、`tokio-postgres` (純 Rust の async PostgreSQL クライアント) と自前の EWKB コーデック (`shpx-geom::ewkb`) で構成される。

`Driver` trait は同期 API なので、driver crate 内で `tokio` ランタイムを 1 個保持し、各メソッドの先頭で `block_on` する形で同期化する。利用者から見えるインターフェースは他ドライバと完全に同じ。

## 対応範囲（v0.3 cycle 2 時点）

- 読み:
  - `pg://user:pass@host:port/db?table=<name>` で接続 → 1 テーブル全件 SELECT
  - geometry 列は `ST_AsEWKB()` で取得し、`shpx_geom::ewkb::strip_srid` で標準 WKB と SRID に分離
  - `ST_GeometryType()` で OGC 型名 (`ST_Point` 等) を取得し、`GeometryType` メタに反映
  - SRID は最初の non-NULL geometry 行の `ST_SRID()` を使う（テーブル空のときは Crs 不明）
  - PG `numeric(p, s)` は `pg_attribute.atttypmod` から `(p, s)` を復元して Arrow `Decimal128(p, s)` に復号する。typmod が無い場合は `(38, 0)` フォールバック。
- 書き:
  - **batch 経路** (`--insert-mode=batch` または非 PG driver 既定): `--overwrite` で `DROP TABLE IF EXISTS <table>` → `CREATE TABLE` → 1 トランザクション + prepared `INSERT INTO ... VALUES ($1, ..., ST_GeomFromEWKB($N))` で行単位投入
  - **bulk 経路** (`--insert-mode=bulk` または `auto` の既定、cycle 2): `COPY <table> (<cols>) FROM STDIN BINARY` を `tokio_postgres::CopyInSink` で送る。型ごとの BE 直書きエンコーダは `crates/shpx-driver-postgis/src/copy_binary.rs` に集約。
  - geometry 列は `geometry(<type>, <srid>)` で宣言（CRS の `epsg_code()` を SRID として使用）
  - `--overwrite` 未指定で同名テーブルが既にあればエラー
- ジオメトリ型: Point / LineString / Polygon / MultiPoint / MultiLineString / MultiPolygon（XY のみ）
- CI 上の docker postgis (`postgis/postgis:16-3.4`) で SHP / Parquet ↔ PostGIS の batch/bulk 双方の往復テスト + Decimal128(38, 10) / bytea / timestamptz の bit-identical テストが緑

## サポート対象 URI スキーム

| スキーム | 正規化後 | 備考 |
|---|---|---|
| `pg://` | `pg` | shpx 内部の正規 scheme |
| `postgres://` | `pg` | libpq 互換 |
| `postgresql://` | `pg` | libpq 互換 |

`shpx-core::Uri::from_path` 側で `pg`/`postgres`/`postgresql` を `pg` に正規化する。driver の `supported_schemes` は `["pg"]` のみを宣言する。

## URI と接続オプション

```
pg://user:password@host:5432/dbname?table=schema.name&...
```

- ユーザー名 / パスワード / host / port / dbname は `tokio_postgres::Config::from_str` で libpq URI として解釈
- `?table=<name>` または `?table=<schema>.<name>` でテーブルを指定（クエリパラメータ）
  - schema 省略時は `public`
  - **必須**（v0.3 cycle 1 では `--query` 未対応のため、テーブル名なしではエラー）
- 環境変数 `SHPX_PG_TABLE` でも代替指定可能（URI クエリが優先）
- `?sslmode=...` などの libpq 互換パラメータは `tokio-postgres` がそのまま解釈

## CRS の扱い

### 読み出し

優先順位:

1. `ReadOpts.src_crs`（CLI の `--src-crs`）
2. テーブル先頭の non-NULL geometry の `ST_SRID()`
   - 0 → CRS 不明として扱う（PostGIS 慣習）
   - それ以外 → `Crs::from_epsg(srid)`（PostGIS の SRID は EPSG 互換）

`spatial_ref_sys` から WKT 等を直接引く処理は cycle 1 では行わない（cycle 3 で `--on-loss=warn` 連携で実装予定）。

### 書き出し

- `crs.epsg_code()` あり → CREATE TABLE で `geometry(<type>, <srid>)` を宣言、INSERT 時の EWKB に同 SRID を埋める
- CRS が無い → `--on-loss` に従う:
  - `error`（既定）: `Error::OnLoss { kind: "missing-crs-on-postgis" }` で停止
  - `warn`: `srid=0` で書き込み + tracing 警告
  - `skip`: `srid=0` で書き込み（無音）

未登録 EPSG の `spatial_ref_sys` 自動 INSERT は v0.3 cycle 3 で対応予定。

## ジオメトリ (EWKB)

PostGIS の binary I/O は EWKB（標準 WKB に SRID を埋め込む拡張）。`shpx-geom::ewkb` モジュールで以下を提供:

- `encode_with_srid(wkb, srid)` — 標準 WKB に SRID flag (`0x20000000`) を立てて SRID を挿入
- `strip_srid(ewkb)` — EWKB から標準 WKB と SRID を分離
- `decode(ewkb)` — EWKB を `Geom` と SRID に分解

Reader は `SELECT ST_AsEWKB(<geom>) ...` で取得 → `strip_srid` で WKB を取り出し Arrow `Binary` 列に格納、SRID を `Crs` に反映する。

Writer は WKB を `Crs::epsg_code()` の SRID で EWKB 化 → `INSERT ... ST_GeomFromEWKB($N)` に bytea として bind する。

Z/M / GeometryCollection は `shpx-geom::wkb` 自体が未対応のため、PostGIS driver も未対応。`shpx-geom` 側の Z/M 対応と同時に拡張する。

## データ型マッピング

| Arrow `DataType` | PostgreSQL 型 | 備考 |
|---|---|---|
| `Boolean` | `boolean` | |
| `Int8` | `smallint` | i16 にプロモート |
| `Int16` | `smallint` | |
| `Int32` | `integer` | |
| `Int64` | `bigint` | |
| `UInt8` | `smallint` | i16 にプロモート（PG 側 unsigned 無し） |
| `UInt16` | `integer` | |
| `UInt32` | `bigint` | |
| `UInt64` | `numeric(20, 0)` | i64 上限を超えた値も保全 |
| `Float32` | `real` | |
| `Float64` | `double precision` | |
| `Decimal128(p, s)` | `numeric(p, s)` | 無損失（cycle 2: NBASE=10000 binary 表現を bulk/batch 両経路で実装） |
| `Decimal256(p, s)` | `numeric(p, s)` | 38 < p ≤ 76 は未対応（cycle 1/2 では p ≤ 38 のみテスト） |
| `Utf8` / `LargeUtf8` | `text` | UTF-8 固定 |
| `Binary` / `LargeBinary` | `bytea` | |
| `Date32` / `Date64` | `date` | |
| `Timestamp(_, None)` | `timestamp` (without tz) | |
| `Timestamp(_, Some("UTC"))` | `timestamptz` | |
| `Timestamp(_, Some(offset))` | `timestamptz` | offset を UTC に変換して保存 |
| geometry 列 | `geometry(<type>, <srid>)` | EWKB I/O |

逆方向（PostgreSQL → Arrow）は `tokio_postgres::Column::type_` の OID で判定:

- `BOOL` → `Boolean`
- `INT2` → `Int16`、`INT4` → `Int32`、`INT8` → `Int64`
- `FLOAT4` → `Float32`、`FLOAT8` → `Float64`
- `NUMERIC` → `Decimal128(p, s)`（cycle 2 で対応）。`pg_attribute.atttypmod` から `(p, s)` を復元する。typmod が `-1`（未指定）なら `(38, 0)` フォールバック。
- `TEXT` / `VARCHAR` / `BPCHAR` → `Utf8`
- `BYTEA` → `Binary`
- `DATE` → `Date32`
- `TIMESTAMP` → `Timestamp(Microsecond, None)`
- `TIMESTAMPTZ` → `Timestamp(Microsecond, Some("UTC"))`
- `geometry` (動的 OID) → `Binary` + GeometryMeta

未対応の OID（`json`/`jsonb`/`uuid`/array 系）は `Error::Schema` で停止する。

## 損失変換

- CRS 無し → `apply_on_loss("missing-crs-on-postgis", ...)`
- 未登録 EPSG → cycle 3 で `spatial_ref_sys` 自動 INSERT、cycle 1 では cycle 3 と同じ kind を使い srid=0 fallback

## スコープ外（cycle 3 以降）

v0.3 cycle 2 完了時点で以下は未対応:

- **`--where`/`--select`/`--query`** reader 側の絞り込み（cycle 3）
- **`--create-table=if-not-exists|always|never`** writer 側の制御（現状は `--overwrite` で DROP するだけ）
- **GIST index 自動生成オプション**（cycle 3）
- **`spatial_ref_sys` への未登録 EPSG 自動 INSERT**（cycle 3）
- **streaming reader**（現状は全件 in-memory）
- Z/M 座標、GeometryCollection
- 複数テーブルの一括書き出し / マテリアライズドビュー
- Decimal256（p > 38）

## 環境変数まとめ

| 変数 | 役割 |
|---|---|
| `SHPX_PG_TABLE` | 入力（および書き出しの fallback）テーブル名（URI クエリ `?table=` が優先） |
| `SHPX_TEST_PG_URL` | integration test の接続先 URL（未設定なら test を skip） |

## 内部実装メモ

- driver crate 内 `OnceLock<tokio::runtime::Runtime>` で multi-thread runtime を 1 個共有。`Connection` future を `tokio::spawn` で別 task に逃がす必要があるため `current_thread` は使わない（`block_on` 中に `Connection` が進まずデッドロックするため）。
- 接続文字列は libpq URI として `tokio_postgres::Config::from_str` に渡す。`?table=` のみ shpx 独自パラメータとして事前に切り出して `Config` には渡さない（`tokio-postgres` が unknown param を `connect_params` で許容するが、念のため除外）。
- batch writer は 1 トランザクション/`write_batch`、prepared `INSERT` を毎行 execute する。Decimal128 は `PgNumeric` newtype で `ToSql` を独自実装し、binary 経路と同じバイト列を bind する。
- bulk writer (`BulkLoadWriter::bulk_write`) は `client.copy_in("COPY \"<schema>\".\"<table>\" (\"col1\", ..., \"geom\") FROM STDIN BINARY")` で `CopyInSink` を取り、`copy_binary::BulkRowEncoder` がエンコードした row bytes を `Bytes` で feed する。最後に trailer `i16(-1)` を送って `finish` で commit。1 接続 = 1 COPY セッションで複数 batch をまたいで送る。
- reader は `client.query("SELECT ST_AsEWKB(geom) AS geom, ... FROM tbl", &[])` を `block_on` で呼んで `Vec<Row>` を取得し、固定 chunk で `RecordBatch` を組む。streaming は `query_raw` 化を将来検討。
- COPY BINARY format の trailer は i16 BE `-1`。各 row の field count は i16 BE。各 field は `i32 length` + payload で、length=-1 が NULL。詳細は PostgreSQL ドキュメント "Binary Format" 節を参照。
- numeric の binary 表現は `i16 ndigits / i16 weight / u16 sign / u16 dscale / [i16 digit; ndigits]`（NBASE=10000）。Decimal128 の i128 値を絶対値化 → 4 桁ごとに分割 → 末尾 0 桁トリム → weight 計算で組み立てる。`PgNumeric` 構造体に encode/decode を集約。

## Future work

- `--listen-channel` で `LISTEN/NOTIFY` を購読する CDC モード
- `pg_dump --section=data` 風の COPY ストリーミング読み出し
- `hstore`/`json`/`jsonb` 列の Arrow `Map`/`Struct` マッピング
- 配列型 (`int4[]` など) → Arrow `List<...>`
- パーティションテーブルの一括 SELECT 最適化
