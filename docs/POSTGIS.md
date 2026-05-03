# PostGIS ドライバ仕様

PostgreSQL + PostGIS 拡張のテーブルを `shpx-driver-postgis` が担当する。GDAL 非依存方針に従い、`tokio-postgres` (純 Rust の async PostgreSQL クライアント) と自前の EWKB コーデック (`shpx-geom::ewkb`) で構成される。

`Driver` trait は同期 API なので、driver crate 内で `tokio` ランタイムを 1 個保持し、各メソッドの先頭で `block_on` する形で同期化する。利用者から見えるインターフェースは他ドライバと完全に同じ。

## 対応範囲（v0.3 リリース時点）

- 読み:
  - `pg://user:pass@host:port/db?table=<name>` で接続 → 1 テーブル全件 SELECT
  - **行絞り込み (cycle 3a)**: CLI `--where '<sql>'` で `WHERE <sql>` を付与
  - **列絞り込み (cycle 3a)**: CLI `--select col1,col2,...` で投影対象を制限（geometry 列は必須）
  - **任意 SQL (cycle 3a)**: CLI `--query 'SELECT ...'` でユーザ定義 SQL をサブクエリ化して読む（`--where` / `--select` と排他）
  - geometry 列は `ST_AsEWKB()` で取得し、`shpx_geom::ewkb::strip_srid` で標準 WKB と SRID に分離
  - `ST_GeometryType()` で OGC 型名 (`ST_Point` 等) を取得し、`GeometryType` メタに反映
  - SRID は最初の non-NULL geometry 行の `ST_SRID()` を使う（テーブル空のときは Crs 不明）
  - PG `numeric(p, s)` は `pg_attribute.atttypmod` から `(p, s)` を復元して Arrow `Decimal128(p, s)` に復号する。typmod が無い場合は `(38, 0)` フォールバック。
- 書き:
  - **batch 経路** (`--insert-mode=batch`): 1 トランザクション + prepared `INSERT INTO ... VALUES ($1, ..., ST_GeomFromEWKB($N))` で行単位投入
  - **bulk 経路** (`--insert-mode=bulk` または `auto` の既定、cycle 2): `COPY <table> (<cols>) FROM STDIN BINARY` を `tokio_postgres::CopyInSink` で送る。型ごとの BE 直書きエンコーダは `crates/shpx-driver-postgis/src/copy_binary.rs` に集約。
  - **テーブル作成戦略 (cycle 3b)**: CLI `--create-table=if-not-exists|always|never`（既定 `if-not-exists`）。`--overwrite` と直交し、`--overwrite=true && --create-table=never` の組み合わせは整合性エラー。
  - **GIST index 自動生成 (cycle 3b)**: CLI `--create-index=auto|always|never`（既定 `auto`）。`auto` は新規作成テーブルにのみ生成し、bulk 経路では COPY 完了後に発行する。
  - **未登録 EPSG 自動登録 (cycle 3b)**: SRID 解決時に `spatial_ref_sys` を probe し、欠けていれば best-effort で `INSERT ... ON CONFLICT DO NOTHING`。
  - geometry 列は `geometry(<type>, <srid>)` で宣言（CRS の `epsg_code()` を SRID として使用）
  - `--overwrite` 未指定 + `--create-table=always` で同名テーブルが既にあればエラー（PG の `relation already exists`）
- ジオメトリ型: Point / LineString / Polygon / MultiPoint / MultiLineString / MultiPolygon（XY のみ）
- CI 上の docker postgis (`postgis/postgis:16-3.4`) で SHP / Parquet ↔ PostGIS の batch/bulk 双方の往復テスト + Decimal128(38, 10) / bytea / timestamptz の bit-identical テストが緑

## reader filtering（cycle 3a）

3 オプションの優先関係と排他関係:

| 状態 | `?table=` の必要性 | 発行 SQL の概形 |
|---|---|---|
| 全件 | 必要 | `SELECT col1, ..., ST_AsEWKB(geom) FROM "<schema>"."<table>"` |
| `--where '<sql>'` | 必要 | `... FROM "<schema>"."<table>" WHERE <sql>` |
| `--select c1,c2,geom` | 必要 | `SELECT "c1", "c2", ST_AsEWKB("geom") FROM ...`（属性集合のみ絞り込み）|
| `--where` + `--select` | 必要 | 上 2 つを併用 |
| `--query 'SELECT ...'` | 不要 | `SELECT * FROM (<query>) AS shpx_q LIMIT 0` で列メタを取得 → 本番は `SELECT col1, ..., ST_AsEWKB(geom) FROM (<query>) AS shpx_q` |

- `--query` は `--where` / `--select` と clap レベルで排他（`conflicts_with_all`）。
- `--query` 指定時は `?table=` / `SHPX_PG_TABLE` も無くて良い（テーブル単一を前提にしない）。
- `--query` の SQL に `;`（末尾またはステートメント区切り）が含まれる場合はサブクエリ化できないため `Error::Driver` で停止する。
- `--select` は **geometry 列を必ず含めること** が必須。geometry 列を抜いた抽出は `Error::Driver`（v0.3 のスコープ上、属性専用テーブル抽出は対象外）。
- `--query` 経由のクエリも、結果列に geometry 列が見当たらないと同様に `Error::Driver`。

geometry 列の検出:

- table モード: `pg_attribute` 由来の `typname IN ('geometry', 'geography')` で検出（cycle 1 から踏襲）。
- query モード: `Statement::columns()` の各 `Type::oid()` が PostgreSQL builtin 範囲外（PostGIS の geometry/geography 動的 OID）かどうかで検出。`pg_type` テーブルから動的 OID を 1 度引き、結果列の OID と突き合わせる。

SRID 解決:

- table モード: `geometry_columns` view → 先頭 non-NULL 行 `ST_SRID()` の 2 段（cycle 1 から踏襲）。
- query モード: テーブル名が無いので `geometry_columns` view は使わない。サブクエリ全体を `SELECT ST_SRID(geom) FROM (<query>) AS shpx_q WHERE geom IS NOT NULL LIMIT 1` で 1 行だけ取り出して使う。

ReadOpts / 環境変数 / URL クエリの優先関係:

- CLI `--where` / `--select` / `--query` は `shpx-core::ReadOpts` 経由で driver に届く。
- 環境変数や URL クエリでこれらをオーバーライドする経路は **設けない**（SQL を文字列で 2 経路から受けると挙動が読みにくくなるため）。
- 既存の `?table=` / `SHPX_PG_TABLE` はそのまま（table モード時のみ参照）。

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

#### 未登録 EPSG の `spatial_ref_sys` 自動登録 (cycle 3b)

SRID が決まったあと、`spatial_ref_sys` に該当行が無ければ best-effort で 1 行 INSERT する。`Crs.wkt`（元データ由来、WKT1/WKT2 どちらでも）→ `shpx_geom::epsg_to_wkt1(code)`（同梱の主要 EPSG マップ: 4326 / 3857 / 4269 / 6668）の順で `srtext` を解決し、どちらも取れなければ INSERT をスキップする（PostGIS の `geometry(_, srid)` 列は `spatial_ref_sys` 行が無くても CREATE / INSERT できるため、ベストエフォートで充分）。

| 列 | 値 |
|---|---|
| `srid` | `Crs.epsg_code()` を i32 化 |
| `auth_name` | `'EPSG'` |
| `auth_srid` | 同上 |
| `srtext` | `Crs.wkt` または `epsg_to_wkt1(code)` |
| `proj4text` | `NULL` |

並列実行と既存登録との競合を避けるため、SQL は `INSERT ... ON CONFLICT (srid) DO NOTHING` を発行する。`--on-loss=error` でも本機能は **常に動作する**（loss を recover する目的のため、error mode は「より厳格に CRS を保存する」意味になる）。`Crs.epsg_code()` が無い CRS（authority が `EPSG` 以外）は `auth_name='EPSG'` と整合しないため登録対象外。

## writer 拡張オプション (cycle 3b)

### `--create-table=if-not-exists|always|never`

| 値 | 挙動 | `--overwrite=true` との組み合わせ |
|---|---|---|
| `if-not-exists`（既定）| `CREATE TABLE IF NOT EXISTS` を発行。既存テーブルがあればそのまま append | DROP→CREATE IF NOT EXISTS（実質 always と同じ） |
| `always` | `CREATE TABLE` を発行。既存テーブルがあれば PG エラー (relation already exists) | DROP→CREATE（旧来の `--overwrite` 単独と同じ挙動）|
| `never` | CREATE を一切発行せず、`pg_class` で存在を検証してから既存テーブルへ append。テーブルが無ければ `Error::Driver` | **整合性エラー**（CLI / `ResolvedWriteOpts::resolve` で reject） |

`never` で既存テーブルへ書き込む際、列スキーマの事前検証は行わない。型・列順・列名のミスマッチは `INSERT` または `COPY` 実行時の PG エラーに任せる（cycle 3c 以降の Future work で要件定義する）。

### `--create-index=auto|always|never`

PostGIS では geometry 列に GIST 空間インデックスを張るのが定石。bulk load では COPY 完了後に index を作る方が桁違いに速いため、本実装はインデックス発行のタイミングを `LayerWriter::finish()` の最後に統一している（batch / bulk 共通）。

| 値 | 挙動 |
|---|---|
| `auto`（既定）| `CreateTable::Never` 経路では作らない（既存テーブルに勝手に index を張らない）。それ以外（`IfNotExists` / `Always`）では発行する。|
| `always` | `--create-table` の値に関わらず常に発行。index 名衝突は `IF NOT EXISTS` で no-op。|
| `never` | 一切発行しない。 |

index 名は `idx_<table>_<geom_col>` を識別子クオートしたもの（例: `"idx_places_geom"`）。index 自体は対象テーブルと同じ schema に作られる（PostgreSQL は schema 修飾子を付けず指定する）。SQL は `CREATE INDEX IF NOT EXISTS {idx} ON {qualified} USING GIST ({geom_col})` を 1 文だけ発行。

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

`--on-loss=error|warn|skip` の挙動と、PostGIS driver が発する loss kind (`missing-crs-on-postgis`) は [`docs/ON_LOSS.md`](ON_LOSS.md) を参照。

driver 固有の補助動作: 未登録 EPSG の SRID 解決時には `spatial_ref_sys` への best-effort `INSERT ... ON CONFLICT (srid) DO NOTHING` を試みる (WKT 解決可能な場合のみ、PostGIS の geometry 列は `spatial_ref_sys` 行が無くても動作するため失敗しても続行)。

## スコープ外（v0.4 以降）

v0.3 リリース時点で以下は未対応:

- **`--create-table=never` での列スキーマ事前検証**（現状は INSERT/COPY 時の PG エラー任せ）
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

## Benchmark

v0.3 完了基準の 1 つ「1000 万行 × 10 属性で `ogr2ogr` の 50% 以上の速度」を計測するための手順と数値を記録する。

### 計測対象

- ベンチスキーマ: 10 属性（Int64 / Boolean / Int32 / Float64 / Utf8 ×2 / Decimal128(38,10) / Date32 / Timestamp(Microsecond, UTC) / Binary）+ `Point(EPSG:4326)` の 11 列。
- 入力: `target/bench-data/points_<rows>.parquet`（`crates/shpx-driver-postgis/benches/gen.rs` が決定論的に生成、再実行時は `MANIFEST.txt` で再利用）。
- 計測対象パス:
  - `shpx convert --insert-mode=bulk --create-table=always --create-index=auto <parquet> pg://...`
  - `ogr2ogr -f PostgreSQL ... -lco SPATIAL_INDEX=NONE -lco PRECISION=NO --config PG_USE_COPY YES`
- 完了基準: shpx の median wall-clock ≤ ogr2ogr の median wall-clock × 2.0（= shpx が 50% 以上の速度）。

### 実行手順

```sh
docker compose up -d postgis
SHPX_TEST_PG_URL=pg://shpx:shpx@localhost:5432/shpx_test \
    scripts/bench-vs-ogr.sh --rows 10000000 --runs 3
```

`scripts/bench-vs-ogr.sh` は実行前に `synchronous_commit=off` / `full_page_writes=off` を `ALTER SYSTEM` で適用し、終了時に `RESET ALL` で元に戻す（`docker-compose.yml` の設定は触らない）。3 回計測の median 値で判定し、達成しなければ exit 1。

### 計測結果

shpx は ogr2ogr の median wall-clock で約 2.2 倍速く（`shpx / ogr2ogr = 0.453`）、完了基準（≤ 2.0）をクリア。

| 入力 row 数 | shpx (median, 3 runs) | ogr2ogr (median, 3 runs) | shpx / ogr2ogr | 判定 |
|---|---|---|---|---|
| 10,000,000 | 28.46 s | 62.84 s | **0.453** | PASS |

個別計測値:

- shpx: 29.63 s / 28.46 s / 27.73 s
- ogr2ogr: 62.84 s / 63.50 s / 60.49 s

### 計測環境

- GDAL: 3.12.3 "Chicoutimi" (2026-03-17 release), Parquet driver 同梱
- PostgreSQL / PostGIS: `postgis/postgis:16-3.4` (Docker, linux/amd64 image)
- ハードウェア: Apple Silicon (A18 Pro, 6 cores), 8 GiB RAM, internal NVMe SSD
- OS: macOS (Darwin 25.4.0 arm64)
- PG パラメタ: `synchronous_commit=off` / `full_page_writes=off` を bench harness が一時設定（終了時に `RESET ALL`）

CI で取得する数値ではないため再現時はこの 4 項目を控えること。同条件で揃えれば bench-vs-ogr.sh が同様の比率を再現する見込み。

## Future work

- `--listen-channel` で `LISTEN/NOTIFY` を購読する CDC モード
- `pg_dump --section=data` 風の COPY ストリーミング読み出し
- `hstore`/`json`/`jsonb` 列の Arrow `Map`/`Struct` マッピング
- 配列型 (`int4[]` など) → Arrow `List<...>`
- パーティションテーブルの一括 SELECT 最適化
