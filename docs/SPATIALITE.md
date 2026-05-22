# SpatiaLite ドライバ仕様

SQLite + SpatiaLite 拡張の geometry テーブルを `shpx-driver-spatialite` が担当する。GDAL 非依存方針に従い、`rusqlite` (`bundled` SQLite) と `mod_spatialite` 動的ロードのみで構成される。

> **配布方針（[ADR-0006](adr/0006-spatialite-system-dependency-not-bundled.md)）**: SpatiaLite は **shpx の単一バイナリへ bundle しない**唯一の driver である。`mod_spatialite` 共有ライブラリは **runtime に `load_extension` で読み込む**ことだけを前提とし、ユーザーが各 OS の package manager で別途用意する。libspatialite を static link する経路（`bundled-spatialite` feature・in-tree vendor・`build.rs`）は提供しない。理由は「GEOS の C++ 依存・vendor 肥大・脆い build.rs・libspatialite fork の保守コストが long-tail driver の価値に対して過大」（恒久理由）と「libspatialite 5.1.0 が Windows MSVC で C ソース patch なしには build できない」（具体的引き金）。詳細は ADR-0006。

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

`--on-loss=error|warn|skip` の挙動と、SpatiaLite driver が発する loss kind 一覧 (`missing-crs-on-spatialite` / `decimal-on-spatialite` / `uint64-overflow-on-spatialite`) は [`docs/ON_LOSS.md`](ON_LOSS.md) を参照。

driver 固有の補助動作: 未登録 EPSG の SRID 解決時には `spatial_ref_sys` への best-effort `INSERT OR IGNORE` を試みる (WKT 解決可能な場合のみ)。`Decimal128` / `Decimal256` は `TEXT` への文字列化で精度を保つが bit-identical 往復はしない (`Capabilities::supports_decimal = false`)。

## スコープ外（v0.6 以降）

以下は未対応:

- **reader filtering** (`--where` / `--select` / `--query`)。RDB driver 共通の v0.5+ 課題
- **Z / M 座標**、`GeometryCollection`、`EMPTY`
- **複数レイヤ append**（同一ファイルに複数 geometry テーブルを段階的に追加するワークフロー）
- **bulk writer**（`Capabilities::bulk_load = false` 固定。SQLite 単体では行単位 INSERT が pragmatic な最速ルート）
- **libspatialite の static link / 単一バイナリ同梱**（[ADR-0006](adr/0006-spatialite-system-dependency-not-bundled.md)。`mod_spatialite` は runtime 供給を唯一の前提とする）

## 環境変数まとめ

| 変数 | 役割 |
|---|---|
| `SHPX_SPATIALITE_TABLE` | 入力（および書き出しの fallback）テーブル名（URI クエリ `?table=` が優先） |
| `SHPX_SPATIALITE_OUT_TABLE` | 書き出し時のテーブル名 override |
| `SHPX_SPATIALITE_PATH` | `mod_spatialite` 共有ライブラリのパスを明示する**任意の override**。通常は不要（後述「`SHPX_SPATIALITE_PATH` は必要か」） |
| `SHPX_TEST_SPATIALITE` | integration test の有効化フラグ（未設定 / `0` / 空文字列なら test を skip） |

## mod_spatialite を用意する（system 依存）

shpx は SpatiaLite 機能を **bundle しない**（[ADR-0006](adr/0006-spatialite-system-dependency-not-bundled.md)）。`shpx convert spatialite://...` を使うには、各 OS で `mod_spatialite` 共有ライブラリを別途 install する。shpx は起動時にこれを `load_extension` で runtime ロードする。

| OS / Target | 入手方法 | 備考 |
|---|---|---|
| Linux | `sudo apt install libsqlite3-mod-spatialite` | OS のライブラリ検索パスから自動 dlopen。追加設定不要 |
| macOS | `brew install libspatialite` | Homebrew prefix（Apple Silicon `/opt/homebrew/lib`、Intel `/usr/local/lib`）は dlopen 既定検索に含まれないため指定が要る（下記） |
| Windows | 自己完結 zip を展開（下記） | OSGeo4W も可だが GIS スタック一式を入れるため重い |

`mod_spatialite` が見つからない場合は driver から `failed to load mod_spatialite at \`...\`: ...` 形式の明示エラーが出る。その案内に従って install し、OS の既定検索パスに無い場合のみ場所を shpx に教える（次節）。

### `SHPX_SPATIALITE_PATH` は必要か

**通常は不要。** shpx は既定で裸の名前 `mod_spatialite` を SQLite に渡し、OS の動的ローダの既定検索パスから解決させる。`mod_spatialite` がそのパス上にあれば（Linux の `apt` 配置など）何も設定しなくてよい。検索パス外にある場合は、次のどちらでも解決できる:

- **OS 標準のローダ変数** — Linux `LD_LIBRARY_PATH=/path`、macOS `DYLD_LIBRARY_PATH=/path`、Windows は `PATH` にフォルダを追加。
- **`SHPX_SPATIALITE_PATH`** — `mod_spatialite` の絶対 / 相対パスを直接指定（shpx がそのパスを `load_extension` に渡す）。

`SHPX_SPATIALITE_PATH` が標準のローダ変数より優れる**唯一の実利は macOS**。`DYLD_LIBRARY_PATH` は SIP（System Integrity Protection）により、Apple 署名の保護バイナリ（`/bin`・`/usr/bin`・`/System` 配下など。`/usr/local` と Homebrew prefix は対象外）を `exec` する瞬間に環境ごと消される。**変数を設定した地点と shpx 起動の間に保護バイナリが 1 つでも挟まると `DYLD_*` は失われる**。`SHPX_SPATIALITE_PATH` は `DYLD_` 系ではない通常の app env なので消されず、shpx が絶対パスを直接 `dlopen` に渡すため SIP の影響を受けない。

| 起動経路 | `DYLD_LIBRARY_PATH` | 必要なもの |
|---|---|---|
| Terminal で直接 `export …; shpx …`（`~/.zshrc` 設定含む） | 効く | どちらでも可 |
| `#!/bin/sh`・`#!/bin/bash` ラッパ / `sh -c '…'` / `env shpx …` | 消える | `SHPX_SPATIALITE_PATH` |
| `make` / npm scripts など `/bin/sh` 経由 | 消える | `SHPX_SPATIALITE_PATH` |
| launchd（LaunchAgents / LaunchDaemons）/ cron | 消える | `SHPX_SPATIALITE_PATH` |
| Finder / `open` / Automator など GUI 起動 | 消える | `SHPX_SPATIALITE_PATH` |
| system の `/usr/bin/python3` 等から `subprocess` 起動 | 消える | `SHPX_SPATIALITE_PATH` |

要するに**対話で手打ちする以外のほぼ全ての自動化・常駐・GUI 経路**で剥がれる。オンプレ本番（launchd / cron / 常駐サービス）はこれに該当しがちなので、macOS では `SHPX_SPATIALITE_PATH` を常用するのが最も堅い:

```sh
export SHPX_SPATIALITE_PATH=/opt/homebrew/lib/mod_spatialite.dylib   # Apple Silicon
```

なお Homebrew の dylib は依存（libgeos / libproj）を install name / `@rpath` の絶対パスで参照するため、`mod_spatialite.dylib` を絶対パスでロードすれば依存も解決される。Windows のような「依存 DLL が別途見つからない」follow-on 問題は起きにくい。

**Linux / Windows では `SHPX_SPATIALITE_PATH` 固有の利点はない**（OS のローダ変数で等価。特に Windows は依存 DLL 解決のため `PATH` 設定が必須で、それを行えば裸の名前も解決するため `SHPX_SPATIALITE_PATH` は冗長）。非標準なファイル名や複数バージョンの中から特定の 1 つを名指ししたいときの利便性のみ。

`bundled-proj`（reprojection 用の libproj static link）は SpatiaLite とは独立しており、v1.0 配布バイナリでも有効。`--reproject` や GeoJSON の WGS84 自動変換はバイナリ単体で動く。

### Windows: 自己完結 zip での導入（インストーラ不要・環境を汚さない）

オンプレ本番など「レジストリ・システム PATH を変更したくない」環境向けの推奨手順。SpatiaLite 公式が **依存 DLL（libproj / libgeos / libtiff …）を全て同梱した自己完結アーカイブ** [`mod_spatialite-5.1.0-win-amd64.7z`](https://www.gaia-gis.it/gaia-sins/windows-bin-amd64/mod_spatialite-5.1.0-win-amd64.7z)（[配布元 Gaia-SINS](http://www.gaia-gis.it/gaia-sins/)）を配布している。

> **Windows 固有の注意（依存 DLL 解決）**: `mod_spatialite.dll` をロードする際、その依存 DLL は **dll と同じフォルダからは自動検索されない**。Windows の依存解決順は「`shpx.exe` のあるフォルダ → システムディレクトリ → カレント → `PATH`」であり、`mod_spatialite.dll` 自身のフォルダは含まれない。よって同梱フォルダを **`shpx.exe` と同居させる**か **`PATH` に乗せる**必要がある。どちらの方式でも、フォルダが解決対象に入れば裸の名前 `mod_spatialite` で見つかるので **`SHPX_SPATIALITE_PATH` は不要**（指定しても害はないが冗長）。

1. 7z を任意フォルダに展開（管理者権限・レジストリ不要）。例: `C:\tools\mod_spatialite\`
2. 次の 2 方式どちらか:

**方式A — 完全ポータブル（最も汚さない・本番推奨）**: `shpx.exe` を同梱フォルダに置いて実行。`mod_spatialite.dll` と依存 DLL が exe のフォルダから解決されるため `PATH` すら触らない。撤去はフォルダ削除のみ。

```cmd
copy shpx.exe C:\tools\mod_spatialite\
cd /d C:\tools\mod_spatialite
shpx.exe convert input.shp output.sqlite
```

**方式B — `shpx.exe` を動かさず、セッション限定で `PATH` を通す**: PATH 変更はそのシェルプロセス内だけで、ウィンドウを閉じれば消える（システム環境変数は不変）。

```powershell
# PowerShell（このセッションのみ）
$env:PATH = "C:\tools\mod_spatialite;$env:PATH"
shpx convert input.shp output.sqlite
```

依存 DLL 欠落で落ちる場合（Windows 10+ はどの DLL が欠けたか報告しない）、公式は [Dependency Walker での診断](https://www.gaia-gis.it/fossil/libspatialite/wiki?name=Lodable+Modules+in+5.0)を案内している。

### なぜ bundle しないのか

libspatialite を単一バイナリへ static link する経路（過去の `bundled-spatialite` feature）は **撤去した**。理由は ADR-0006 を参照。要約:

- **重さ（恒久理由）**: libspatialite は GEOS（C++）を引き込み、`unsafe_code = "deny"` の pure-Rust workspace に CMake ビルドの C++ stdlib リンクと脆い build.rs、11MB の in-tree vendor を抱えることになる。long-tail driver の価値に対して保守コストが過大。
- **接続経路の標準化（恒久理由）**: static link では SQLite 標準の `load_extension` を使えず、生の `Connection::handle()` への独自 `unsafe` FFI 初期化（`spatialite_init_ex` + cache の手動管理）が必要で、dynamic 経路との二重保守を強いた。dynamic のみにすることで接続は常に標準経路 1 本に収束する。
- **Windows MSVC 破綻（具体的引き金）**: libspatialite 5.1.0 が `gg_shape.c::gaia_win_fopen` 付近の `GAIAGEO_DECLARE` マクロ展開で C2054 を踏み、上流 fork レベルの C ソース patch なしには build できない。
- **ライセンス（副次的利点）**: bundle すると libspatialite の LGPL 2.1 / MPL 1.1 / GPL 2.0 triple-license 再リンク配布義務が発生するが、ユーザー供給ライブラリの runtime ロードなら shpx は配布しないため義務は消える。

## 内部実装メモ

- `Capabilities { read: true, write: true, bulk_load: false, supports_blob: true, supports_decimal: false, supports_timestamp_tz: true, string_encoding: Fixed("utf-8") }`
- `mod_spatialite` ロード経路（`load_extension` のみ。static link 経路は持たない — ADR-0006）:
    1. `SHPX_SPATIALITE_PATH` env で指定された絶対 / 相対パスを `load_extension`
    2. 既定: `mod_spatialite` を SQLite に渡し、OS のライブラリ検索パス (`LD_LIBRARY_PATH` / `DYLD_LIBRARY_PATH` / `/etc/ld.so.conf`) から dlopen
- `open_write_new` は `journal_mode=WAL` / `synchronous=NORMAL` を pragma 設定して、空 DB で `InitSpatialMetadata(1)` を idempotent 発行する。`FastInit (=1)` は WGS84 系のみ seed する選択肢で、全 EPSG seed (`InitSpatialMetadata(0)`) は数秒かかるため空 DB を頻繁に作る用途では使わない
- writer の `INSERT` は `GeomFromWKB(?, srid)` でジオメトリを SpatiaLite に encode してもらう。自前の `spatialite_blob::encode` で書く実装と差異があると検出が困難なため、I/O 整合性は SpatiaLite 自身の関数に揃える方針。reader 経路でも同様に `AsBinary(<geom>)` で標準 WKB を取り出す
- `--create-table=Always` は `DROP TABLE IF EXISTS <table>` の前に `DELETE FROM geometry_columns WHERE f_table_name = ?` と `SELECT DiscardGeometryColumn(?, ?)` を発行して、SpatiaLite の管理テーブル / R\*Tree からも紐付き行を消してから DROP する
- `shpx-rdb-common` の `validate_overwrite_compat` / `apply_on_loss` / `merge_crs` を再利用（PostGIS / SQL Server と同パターン）。SQLite に schema 概念がないため `split_qualified` / `resolve_table_name` は呼ばない
- INSERT バインドは `chrono` クレート経由で `NaiveDate` (Date32/Date64) / `DateTime<Utc>` (Timestamp) を `rusqlite::types::Value::Text` の ISO8601 文字列にエンコードする。`rusqlite` の `chrono` feature を有効化している
- 内部 `fid` 列を `INTEGER PRIMARY KEY` で常に追加する。`rusqlite` の `last_insert_rowid` 等の挙動と整合させるため、`AddGeometryColumn` の前に PK を確保する設計

## Future work

- Z / M 座標と `GeometryCollection` の対応（`shpx-geom::wkb` 側の拡張と同期）
- `--where` / `--select` / `--query` reader 拡張（PostGIS と同形）
- 複数 geometry テーブルの一括書き出し
- `spatialite_history` 等の SpatiaLite 固有 metadata の取り扱い
