# SQL Server ドライバ仕様

Microsoft SQL Server / Azure SQL のテーブルを `shpx-driver-sqlserver` が担当する。GDAL 非依存方針に従い、`tiberius` (純 Rust の async TDS クライアント) と OGC 標準 WKB (`shpx-geom::wkb`) で構成される。

`Driver` trait は同期 API なので、driver crate 内で `tokio` ランタイムを 1 個保持し、各メソッドの先頭で `block_on` する形で同期化する（PostGIS と同形）。

## 対応範囲（v0.4 リリース時点）

- 読み:
  - `mssql://user:pass@host:port/db?table=<name>` で接続 → 1 テーブル全件 SELECT (table モード固定)
  - geometry 列は `[col].STAsBinary() AS [col], [col].STSrid AS [col__shpx_srid]` の併走 SELECT で WKB と SRID を一括取得
  - `STGeometryType()` で OGC 型名 (`Point`, `LineString` など) を取得し、`GeometryType` メタに反映
  - **`--where` / `--select` / `--query` は v0.5+ に先送り**（v0.4 はこれらが指定されると明示エラーで弾く）
- 書き:
  - **batch 経路** (`--insert-mode=batch`): 1 トランザクション + prepared `INSERT INTO ... VALUES (@P1, ..., {geometry|geography}::STGeomFromWKB(@PN, @PS))` で行単位投入
  - **bulk 経路** (`--insert-mode=bulk` または `auto` の既定): **staging テーブル経由 (案 B、`docs/DESIGN.md` L.219-)**。`#shpx_stage_<uuid>` (接続スコープ #temp) に WKB + SRID を `tiberius::Client::bulk_insert` で流し、`INSERT INTO target SELECT ..., {kind}::STGeomFromWKB(...)` で型変換しながら確定テーブルへ転記する。chunk ごとに `BEGIN TRAN` / `COMMIT TRAN` を挟んで tempdb log truncation を可能にする。
  - **テーブル作成戦略**: CLI `--create-table=if-not-exists|always|never`（既定 `if-not-exists`）。PostGIS と同形セマンティクス。`--overwrite=true && --create-table=never` は整合性エラー。
  - **SPATIAL INDEX 生成**: CLI `--create-index=auto|always|never`（既定 `auto`）。
    - **`Auto` は no-op**（PostGIS の Auto と挙動が違う点に注意）。SQL Server `CREATE SPATIAL INDEX` は `geometry` 列で `BOUNDING_BOX` が必須で、未知 SRID では失敗するため、暗黙生成は安全側に倒している。
    - `Always` のみ明示有効化: `geometry` は同梱の bbox 表（4326 全球 / 3857 Web Mercator）から `BOUNDING_BOX` を解決、未知 SRID は明示エラー。`geography` は BOUNDING_BOX 不要。
    - bulk 経路では `INSERT…SELECT` 完了後に発行（PostGIS と同じ位置）。
  - geometry 列は `geometry` または `geography` で宣言（URI クエリ `?geom_type=geometry|geography` で切替、未指定時は `geometry`）
- ジオメトリ型: Point / LineString / Polygon / MultiPoint / MultiLineString / MultiPolygon（XY のみ）
- CI 上の docker mssql (`mcr.microsoft.com/mssql/server:2022-latest`) で SHP / Parquet ↔ SQL Server の batch/bulk 双方の往復テスト + Decimal128(38, 10) / varbinary / datetimeoffset の bit-identical テスト + 型網羅 `bulk_all_types_together` が緑

## URI 仕様

```
mssql://<user>:<password>@<host>[:<port>]/<database>?<key>=<value>&...
```

| クエリ | 既定 | 説明 |
|---|---|---|
| `table` | (`SHPX_MSSQL_TABLE` env、無ければエラー) | `schema.name` または `name`。schema 省略時は `dbo` |
| `geom_type` | `geometry` | `geometry` / `geography` を切替。`geography` は SRID 必須 |
| `auth` | `sql` | `sql` (user/pass) / `integrated` / `windows`。下記「認証」節 |
| `trusted_connection` | `false` | `true` は `?auth=integrated` のエイリアス (後方互換) |

URL 内の特殊文字（パスワードの記号など）は `%xx` で percent encode する。userinfo の `@` を含むパスワードは「最後の `@` を userinfo/host の区切り」とみなす保守的なパースだが、確実を期すため `%40` 推奨。

## 認証

`?auth=` または `?trusted_connection=true` で認証方式を切り替える。`integrated` / `windows` は CLI feature `windows-auth` 有効時のみ動作する (既定は SQL 認証のみ)。

| `?auth=` | URL 例 | 動作プラットフォーム | 必要 feature | 系外依存 |
|---|---|---|---|---|
| `sql` (既定) | `mssql://sa:Pw@host/db` | 全 OS | なし | なし |
| `integrated` | `mssql://host/db?auth=integrated` | Windows / Linux / macOS | `windows-auth` | Unix のみ system `libgssapi-krb5` |
| `windows` | `mssql://DOMAIN%5Cuser:Pw@host/db?auth=windows` | Windows のみ | `windows-auth` | なし (pure Rust SSPI) |

- **`sql`**: SQL Server 認証 (一般 user / sa)。プラットフォーム非依存、追加 install 不要。
- **`integrated`**: 現在ログイン中の OS ユーザーで認証。
  - Windows: pure Rust の SSPI (`tiberius/winauth`)。追加 install 不要。
  - Linux / macOS: Kerberos via `libgssapi-krb5`。**システムに `libkrb5` が必要** (`apt install libkrb5-dev`、`brew install krb5`)。配布バイナリでは bundled しないため、利用時に install してもらう前提。Active Directory 統合済み環境を想定 (`kinit` 等で TGT が発行されている状態)。
- **`windows`**: 明示 user/password による NTLM 認証。Windows でのみ動作。`DOMAIN\user` 形式の `\` は URL 上 `%5C` に percent-encode する。

### feature の有効化

```bash
# driver crate を直接使う場合
cargo build -p shpx-driver-sqlserver --features windows-auth

# CLI バイナリ
cargo build -p shpx-cli --features windows-auth
```

feature 無効時に `?auth=integrated` / `?auth=windows` を指定するとビルド時ではなく接続時に明示エラー (`?auth=integrated requires the windows-auth feature ...`) を返す。

### 配布バイナリでの状況

`cargo-dist` の Release artifact (v1.0):

| OS | `windows-auth` | 備考 |
|---|---|---|
| Windows x64 / arm64 | 有効化 | 追加 install 不要 |
| Linux x64 / arm64 | 無効化 (既定) | `libkrb5` 同梱の影響評価が済むまで opt-in。利用したいユーザーは `cargo install --features windows-auth` で local build |
| macOS x64 / arm64 | 無効化 (既定) | macOS 標準 Kerberos.framework は使えるが、tiberius は `libgssapi-krb5` 経由で見るため Homebrew `krb5` が必要 |

将来 (v1.x) tiberius が `sspi-rs` へ移行すれば pure Rust の Linux/macOS NTLM 認証が可能になり、配布段階で全 OS 有効化を再検討する見込み。

## URI 例

```bash
# SQL 認証 (既定)
shpx convert in.shp 'mssql://sa:Pw@localhost/dev?table=t&geom_type=geometry'

# AD 統合認証 (Windows / Linux Kerberos)
shpx convert in.shp 'mssql://prod-sql.corp.example.com/data?auth=integrated&table=t'

# Windows NTLM (明示 creds)
shpx convert in.shp 'mssql://DOMAIN%5Calice:Pw@host/db?auth=windows&table=t'
```

## CRS と SRID

- writer SRID 解決の優先順位:
  1. `--src-crs EPSG:xxxx`（CLI 引数）
  2. schema field metadata の `Crs`
  3. なし → `apply_on_loss(missing-crs-on-sqlserver)` フォールバック
     - `geometry`: SRID 0
     - `geography`: SRID 4326（geography は valid な geographic CRS が必須のため）
- reader SRID は first non-NULL row の `[col].STSrid` を採用（SQL Server には PostGIS の `geometry_columns` view 相当が無いため、値経由で取る）
- PostGIS と違い、`sys.spatial_reference_systems` は SQL Server 同梱で seed 済みのため SRS 自動登録は **行わない**

## 型マッピング

| Arrow | SQL Server | 備考 |
|---|---|---|
| Boolean | `bit` | |
| Int16 / Int32 / Int64 | `smallint` / `int` / `bigint` | |
| Float32 / Float64 | `real` / `float` | `float` は既定 53bit (= IEEE 754 double) |
| Decimal128(p, s) | `decimal(p, s)` | p 1..=38。`rust_decimal::Decimal` 経由で bit-identical |
| Utf8 / LargeUtf8 | `nvarchar(max)` | UTF-16 表現（最大 2GB） |
| Binary / LargeBinary | `varbinary(max)` | |
| Date32 | `date` | AD 0001-01-01 起点 |
| Timestamp(_, None) | `datetime2` | 既定 precision 7 (100ns 解像度) |
| Timestamp(_, UTC) | `datetimeoffset` | UTC 固定 offset |
| Geometry (WKB) | `geometry` または `geography` | URI クエリ `?geom_type=` で切替 |

未対応型: Int8 / UInt 系 / Date64 / 他 TZ / xml / hierarchyid / sql_variant / Decimal256。

## batch 経路の multi-row VALUES 化

`--insert-mode=batch` (および `--insert-mode=auto` でドライバが bulk 非対応にフォールバックした場合) の `LayerWriter::write_batch` は `INSERT INTO t (...) VALUES (?,?,...), (?,?,...), ...` の multi-row 形式で 1 RPC に複数行を詰める。SQL Server は 1 RPC あたり 2100 param が上限のため、1 行あたりの param 数 (属性数 + WKB + SRID) から `chunk_rows = floor((2100 - safety) / params_per_row)` を schema 確定時に算出し、`write_batch` 内で `chunks(chunk_rows)` ループする。属性 6 列 + Point の典型 schema (`params_per_row = 8`) では `chunk_rows ≈ 260` で動き、1 行 1 RPC だった旧実装と比較して round-trip が ~175× 削減される。トランザクション境界は 1 batch = 1 トランザクションを維持し、chunk 境界では COMMIT しない。

## 損失変換

`--on-loss=error|warn|skip` の挙動と、SQL Server driver が発する loss kind (`missing-crs-on-sqlserver`) は [`docs/ON_LOSS.md`](ON_LOSS.md) を参照。`geometry` 列は CRS 不明時に SRID 0 へフォールバックするが、`geography` 列は valid な geographic CRS が必須のため SRID 4326 へフォールバックする (`STGeomFromWKB` が SRID 0 では失敗するため)。

## staging bulk の動作（案 B）

```
1. CREATE TABLE [#shpx_stage_<short_uuid>] (
       <attr cols matching target>,
       [shpx_geom_wkb] varbinary(max) NULL,
       [shpx_geom_srid] int NOT NULL
   )
2. for each chunk (default 100,000 rows):
   a. BEGIN TRAN
   b. tiberius::Client::bulk_insert("[#shpx_stage_...]") + send(TokenRow) × chunk + finalize
   c. INSERT INTO <target> (<cols>, <geom>) SELECT <cols>,
        {geometry|geography}::STGeomFromWKB([shpx_geom_wkb], [shpx_geom_srid])
      FROM [#shpx_stage_...]
   d. TRUNCATE TABLE [#shpx_stage_...]
   e. COMMIT TRAN
3. DROP TABLE [#shpx_stage_...] (best-effort、接続切断でも消える)
```

- staging テーブルは local temp (`#name`、接続スコープで自動 GC)。同じ Client が生存する限り chunk 間で再利用される。
- chunk size は `SHPX_MSSQL_BULK_CHUNK` env で override 可（既定 100,000）。bench 時のみ 1,000,000 に上げる運用。
- chunk ごとに `COMMIT` を挟むことで `tempdb` の log を切り捨てられ、10M 行投入でも tempdb 溢れが起きない。
- decimal は `rust_decimal::Decimal::new_with_scale(value, scale)` 経由で `tiberius::Numeric` に詰める（tiberius の生 Numeric write は scale 0 以外でバグがあるため、`rust_decimal` feature 必須）。

## ローカル実行

```bash
docker compose up -d mssql
docker exec shpx-mssql /opt/mssql-tools18/bin/sqlcmd \
  -S localhost -U sa -P 'Shpx_test_pw1!' -C \
  -Q "IF DB_ID('shpx_test') IS NULL CREATE DATABASE shpx_test"

export SHPX_TEST_SQLSERVER_URL='mssql://sa:Shpx_test_pw1!@localhost:1433/shpx_test'
cargo test -p shpx-driver-sqlserver --locked
```

## Benchmark

完了基準 (`docs/ROADMAP.md` v0.4): chunk size 1M でも tempdb 溢れずに 10M 行 staging bulk insert が完走する。v1.0 cycle 1 で完了基準を「shpx 単独 wall-clock を公開しつつ tempdb 溢れなし完走」に再定義 (ogr2ogr 比較は完了基準から外した)。`.github/workflows/bench-smoke-mssql.yml` を `rows=10000000 runs=3 SHPX_MSSQL_BULK_CHUNK=1000000` で実行した結果、ubuntu-latest 上で 3 run 全完走、wall-clock は **139.01 / 138.23 / 138.53 s** (median 138.53s ≈ 72k rows/s)。v1.0 cycle 1 の workflow run [25275545973](https://github.com/jumboly/shpx/actions/runs/25275545973) で記録。

bench 入力スキーマは tiberius 0.12 の bulk encode 既知不整合を避けるため、minimal 4 列 (Int64 / Utf8 / Float64 / Point) に絞っている。bit-identical な型網羅検証は別途 `tests/bulk_roundtrip.rs` の単独テストで cover。

ローカル実測手順:

```bash
# bench 入力 Parquet を生成（criterion harness 経由、初回のみ重い）
SHPX_BENCH_ROWS=10000000 \
  cargo bench -q -p shpx-driver-sqlserver --bench bulk_insert -- \
  --quick --warm-up-time 1 --measurement-time 1

# shpx 単独の wall-clock 計測
SHPX_TEST_SQLSERVER_URL='mssql://sa:Shpx_test_pw1!@localhost:1433/shpx_test' \
SHPX_MSSQL_BULK_CHUNK=1000000 \
  /usr/bin/time -p target/release/shpx convert \
    --insert-mode=bulk --create-table=always --create-index=auto \
    target/bench-data/points_mssql_10000000.parquet \
    "${SHPX_TEST_SQLSERVER_URL}?table=bench_shpx"
```

`scripts/bench-vs-ogr-mssql.sh` は ogr2ogr との挙動差を手元で見たい場合の参考 utility (CI 計測には使わず、GDAL Parquet plugin + msodbcsql18 をローカルで揃える前提)。

### Smoke (Apple Silicon, Rosetta/QEMU emulation 経由 SQL Server 2022)

100k 行 1 run: shpx staging bulk **1.28s** (78k rows/s)。動作確認のみ。emulation 経由なので production 値ではなく、Linux x86_64 native では更に速くなる見込み。

## 制限事項 / 既知の落とし穴

- **reader 拡張 (`--where` / `--select` / `--query`) は v0.5+**: v0.4 では指定すると明示エラー。tiberius 経由の動的 UDT 検出と `--query` のサブクエリ化は工数のため後回しにした。
- **`--create-index=Always` は事前 PK 必須**: SQL Server の `CREATE SPATIAL INDEX` は仕様で **clustered primary key を要求** する。shpx writer は汎用 driver として `CREATE TABLE` 時に PK を勝手に付与しないため、`--create-index=Always` を使うには利用者が事前に PK 付きテーブルを CREATE しておき `--create-table=never` で append する運用になる。`--create-index=Auto` は no-op で何もしないので安全側。
- **Windows 認証は opt-in feature**: `?auth=integrated|windows` は CLI feature `windows-auth` を有効化したビルドでのみ動作する。配布バイナリ (cargo-dist) では Windows のみ既定有効化、Linux / macOS は libkrb5 system dep を避けるため既定無効。Azure AD 認証は v1.x 以降の検討。
- **macOS の TLS**: tiberius を `rustls` feature で有効化済み（`native-tls` は SQL Server 2019+ で TLS handshake 失敗の既知問題があるため）。
- **decimal の bit-identical**: `rust_decimal::Decimal` の内部 scale (0..=28) と Arrow `Decimal128` の scale (0..=38) が一致しない場合、reader 側で 10^delta スケーリングする。SQL Server 側の値が schema 通りに格納されている限り delta は 0 になる（schema を介さず生 Numeric 値を流すケースで可能性あり）。
- **`#temp` テーブルのスコープ**: tiberius の `Client` 接続が切断されると `#shpx_stage_<uuid>` も消える。bulk 中に接続が切れた場合は途中までの行が target テーブルに既に COMMIT 済みである可能性があり、再実行時は `--create-table=always` または手動で target を DROP する。
- **tiberius 0.12 bulk encode の既知不整合**: 多列スキーマ (10+ 列) で `decimal(p, s)` と複数の `varbinary(max)` 列が同一テーブルにあると、特定の列で `Token error: Invalid column type from bcp client` を踏むケースがある（`tests/bulk_roundtrip.rs::bulk_all_types_together` を `#[ignore]` で再現可能）。完了基準の各型 (decimal(38, 10) / timestamptz / bytea) は単独テスト (`bulk_decimal_38_10_bit_identical` / `bulk_timestamptz_and_int64_bit_identical` / `bulk_point_with_attributes_roundtrip` の WKB 経路) で bit-identical を確認済み。tiberius 上流に再現報告予定。
- **MS-SSCLRT native UDT bulk**: 真の native binary geometry encoder は v1.x 以降の検討。tiberius 上流に PR を出すかフォーク派生を持つかは未決定（`docs/ROADMAP.md` v1.x 候補）。

## 関連ドキュメント

- 設計案 B の決定: [`docs/DESIGN.md`](DESIGN.md) L.219-229
- マイルストーン進捗: [`docs/ROADMAP.md`](ROADMAP.md) v0.4
- 型マッピング全体: [`docs/DATA_TYPES.md`](DATA_TYPES.md)
