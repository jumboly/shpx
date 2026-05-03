# GeoPackage ドライバ仕様

`.gpkg` (OGC GeoPackage 1.3) を `shpx-driver-gpkg` が担当する。GDAL 非依存方針に従い、`rusqlite` (`bundled` SQLite) と自前の GeoPackage Binary geometry コーデック (`shpx-geom::gpkg_blob`) で構成される。

## 対応範囲（v0.7 リリース時点）

- 読み:
  - `.gpkg` の feature テーブル（`gpkg_contents.data_type='features'`）
  - 単一 feature テーブルなら自動採用、複数あれば `?table=<name>` または環境変数 `SHPX_GPKG_TABLE` で明示
  - GeoPackage Binary header の全 envelope_type (0/1/2/3/4) と LE/BE 双方を読み飛ばし、内部 WKB を抽出
  - SRS は `gpkg_spatial_ref_sys` から復元（`organization='EPSG'` なら EPSG コード、それ以外は WKT 保持）
- 書き:
  - 既存 GPKG が無ければ新規作成（`PRAGMA application_id` / `user_version` / `journal_mode=WAL` / `synchronous=NORMAL` を設定）
  - 必須メタテーブル（`gpkg_spatial_ref_sys` / `gpkg_contents` / `gpkg_geometry_columns`）と必須 SRS 行 (`-1`, `0`, `4326`) を初期化
  - feature テーブルは `fid INTEGER PRIMARY KEY AUTOINCREMENT` を先頭に持つ（GPKG 仕様で必須）
  - geometry blob は `envelope_type=0` / `binary_type=Standard` / endian=LE 固定
  - `finish()` で `gpkg_contents.bbox` を累積値で UPDATE
- ジオメトリ型: Point / LineString / Polygon / MultiPoint / MultiLineString / MultiPolygon（XY のみ）

## サポート対象拡張子

| 拡張子 | スキーム |
|---|---|
| `.gpkg` | gpkg |

`Uri::from_path` が拡張子を小文字化済みのため、`.GPKG` も透過に扱う。

## テーブル名の指定

shpx は 1 つの GPKG ファイルに対して 1 つの feature テーブルを読み書きする（マルチレイヤは v0.3 以降）。テーブル名の決定順:

### 読み出し

1. URI クエリ `path/to/file.gpkg?table=cities`（`%XX` パーセントエンコーディングと `+` を space に解釈）
2. 環境変数 `SHPX_GPKG_TABLE`
3. `gpkg_contents` 上の feature テーブルが 1 件のみなら自動採用、複数あれば候補を列挙してエラー

### 書き出し

1. 環境変数 `SHPX_GPKG_OUT_TABLE`（書き出し専用）
2. URI クエリ `?table=...` または環境変数 `SHPX_GPKG_TABLE`
3. 出力ファイル名の stem（例: `places.gpkg` → `places`）。SQL 識別子に許される文字以外は `_` に置換し、先頭が数字なら `_` プレフィクス

## CRS の扱い

### 読み出し

優先順位:

1. `ReadOpts.src_crs`（CLI の `--src-crs`）
2. `gpkg_geometry_columns.srs_id` → `gpkg_spatial_ref_sys` を引いて復元
   - `organization='EPSG'` → `Crs::from_epsg(code)`
   - それ以外 → `Crs { authority, wkt: Some(definition), wkt_flavor: V1, .. }`（`definition` は WKT1）
   - srs_id が `-1` または `0`（仕様必須の Undefined SRS）→ CRS 不明として扱う

`gpkg_spatial_ref_sys` に存在しない srs_id を `gpkg_geometry_columns` が指している場合は `Error::Crs("dangling srs_id N")` で停止する。OGC 12-063 拡張で追加される `definition_12_063` 列（WKT2）は v0.7 から拾うようになり、列が存在し非空であれば `Crs.wkt2` に格納する (`definition` の WKT1 と並走)。

### 書き出し

- `crs.epsg_code()` あり → `gpkg_spatial_ref_sys` に EPSG 行を `INSERT OR IGNORE`、`gpkg_contents.srs_id` と `gpkg_geometry_columns.srs_id` に EPSG コードを書く
- `wkt` のみ（EPSG なし）→ `srs_id = MAX(srs_id) + 1` を採番して `organization='shpx'` で新規行を追加（`100_000` 以上にオフセット）
- CRS が無い → `--on-loss` に従う:
  - `error`（既定）: `Error::OnLoss { kind: "missing-crs-on-gpkg" }` で停止
  - `warn`: `srs_id=0`（Undefined geographic）で書き込み + tracing 警告
  - `skip`: `srs_id=0` で書き込み（無音）

## ジオメトリ

GPKG geometry blob は OGC GeoPackage 1.3 §2.1.3.1.1 の StandardGeoPackageBinary 形式:

```
offset size 内容
0      2    magic = 0x47 0x50 ("GP")
2      1    version = 0
3      1    flags : bit0=endian (1=LE) | bit1-3=envelope_type | bit4=empty | bit5=binary_type
4      4    srs_id (i32)
8      ...  envelope (envelope_type に応じて 0/32/48/48/64 bytes)
末尾   ..   標準 WKB ペイロード（WKB 内部の byte order に従う）
```

### 書き出し方針

- envelope_type=0（envelope を持たない）固定。spatial index 未対応の現状で envelope を書いてもメリットが小さく、ヘッダ生成を単純化したい
- binary_type=Standard 固定（GPKG 拡張の `gpkg_extensions` 経由 Extended は未対応）
- header の endian=LE 固定（WKB ペイロードも `shpx-geom::wkb` が LE で生成）

### 読み出し方針

- 全 envelope_type を読み飛ばす（envelope は捨てる）
- LE/BE どちらの header も解釈
- Extended binary_type の blob も WKB 部分を抽出する（保持される拡張型情報は失われる）

## データ型マッピング

| Arrow `DataType` | GPKG/SQLite 宣言型 | 備考 |
|---|---|---|
| `Boolean` | `BOOLEAN` | INTEGER affinity の 0/1 |
| `Int8` | `TINYINT` | 値は SQLite INTEGER (i64) |
| `Int16` | `SMALLINT` | |
| `Int32` | `MEDIUMINT` | |
| `Int64` / `UInt8/16/32` | `INTEGER` | 全て i64 として保持 |
| `UInt64` | エラー | SQLite が i64 上限のため `Error::Schema` |
| `Float16/32` | `FLOAT` | |
| `Float64` | `DOUBLE` | |
| `Utf8` / `LargeUtf8` | `TEXT` | UTF-8 固定 |
| `Binary` / `LargeBinary` | `BLOB` | |
| `Date32` / `Date64` | `DATE` | ISO 8601 文字列 (`YYYY-MM-DD`) |
| `Timestamp(_, None)` | `DATETIME` | ISO 8601 + `Z`（UTC 仮定） |
| `Timestamp(_, Some(tz))` | `DATETIME` | `+HH:MM` offset 付き ISO 8601、name 付き TZ は UTC 化 + 警告 |
| `Decimal128/256` | `TEXT` | 文字列降格（`apply_on_loss(decimal-on-gpkg)`） |
| geometry 列 | `<GEOMETRY_TYPE>` (`POINT`/`LINESTRING`/.../`GEOMETRY`) | BLOB affinity、`gpkg_geometry_columns` に登録 |

逆方向（SQLite → Arrow）は `PRAGMA table_info` の宣言型を最優先。dynamic typing で値型と宣言型がずれた場合は文字列降格して救済する（INTEGER 列に REAL や TEXT が来ても i64 へキャスト試行）。

`fid INTEGER PRIMARY KEY AUTOINCREMENT` 列は GPKG 仕様で必須だが、shpx 中間表現には含めない（writer 側が再生成するため）。reader は `pk=1` かつ宣言型が `INTEGER` の列を auto-PK と判定して読み飛ばす。

## 損失変換

`--on-loss=error|warn|skip` の挙動と、GPKG driver が発する loss kind 一覧 (`decimal-on-gpkg` / `uint64-overflow-on-gpkg` / `missing-crs-on-gpkg`) は [`docs/ON_LOSS.md`](ON_LOSS.md) を参照。

なお、`Timestamp` の name 付き TZ（例: `"Asia/Tokyo"`）は UTC に変換して書き出す (offset で表せないため `--on-loss` 経路は経由せず固定動作)。

## スコープ外

以下は未対応:

- 複数 feature テーブルへの append/replace 書き出し
- `gpkg_extensions` / `gpkg_metadata` / `gpkg_metadata_reference` などの拡張テーブル
- R*Tree 空間インデックス（`gpkg_rtree_index`）
- Z/M 座標（`gpkg_geometry_columns.z`/`m` フラグ + ISO WKB 拡張）
- `definition_12_063` (WKT2) の書き出し優先
- タイル/アトリビュート専用テーブル（features 以外の `gpkg_contents.data_type`）
- `mod_spatialite` 経由の SpatiaLite 互換モード

## 環境変数まとめ

| 変数 | 役割 |
|---|---|
| `SHPX_GPKG_TABLE` | 入力（および書き出しの fallback）テーブル名 |
| `SHPX_GPKG_OUT_TABLE` | 書き出しテーブル名（`SHPX_GPKG_TABLE` より優先） |

URI クエリ `?table=<name>` を指定すると環境変数より優先される。

## 内部実装メモ

- メタテーブル CREATE は `meta.rs` に静的 SQL 定数で埋め込み
- `application_id = 0x47504347` (`'GPKG'`)、`user_version = 10300`（GPKG 1.3）
- 書き出しトランザクション粒度は 1 `write_batch` ごと。bulk load (`COPY BINARY` 相当) は SQLite には無いため、prepared INSERT バッチで十分
- `WriteOpts.overwrite=true` のとき、`.gpkg` 本体に加えて `.gpkg-wal` / `.gpkg-shm` / `.gpkg-journal` のサイドカーも削除する（旧 DB の WAL リプレイで上書きが reverted されるのを防ぐ）

## Future work

- 完全 streaming reader（現状は open() で全行を `Vec<Row>` に展開）
- `definition_12_063` (WKT2) を `definition` (WKT1) より優先して `Crs::wkt2` に格納するか選択する `--gpkg-prefer-wkt2` オプション (現状は両方並走)
- マルチレイヤ書き出しと `--gpkg-table` CLI フラグ
- spatial index 自動生成（`--gpkg-create-spatial-index`）
- Z/M 拡張（`shpx-geom::wkb` の Z/M 対応と同時）
