# データ型マッピング

shpx の中間表現は Apache Arrow。文字列は UTF-8 を中間表現とし、入出力時のみエンコーディング変換を行う。属性カラムの順序は Arrow `Schema.fields` の順序を物理的に保つ。

## 全体マッピング表

| Arrow 内部 | PostGIS | SQL Server | GPKG | SpatiaLite | Parquet | FlatGeobuf | GeoJSON(L) | CSV | SHP/DBF |
|---|---|---|---|---|---|---|---|---|---|
| `Boolean` | `boolean` | `bit` | `BOOLEAN` (0/1) | `BOOLEAN` (0/1) | BOOLEAN | Bool | bool | bool | `L` |
| `Int8` / `Int16` / `Int32` | `smallint`/`integer` | `smallint`/`int` | `INTEGER` | `TINYINT`/`SMALLINT`/`MEDIUMINT` | INT8/16/32 | Byte/Short/Int | number | int | `N`（桁自動） |
| `Int64` | `bigint` | `bigint` | `INTEGER` | `INTEGER` | INT64 | Long | number | int | `N` (>18桁警告) |
| `Float32` / `Float64` | `real` / `double precision` | `real` / `float` | `REAL` | `FLOAT` / `DOUBLE` | FLOAT/DOUBLE | Float/Double | number | float | `F` |
| `Decimal128(p,s)` | `numeric(p,s)` | `decimal(p,s)` | `TEXT` 保全 | `TEXT` 降格（無損失なし） | DECIMAL(p,s) | なし→Double / String | string | string | `N` (`p>18` で警告/エラー) |
| `Decimal256(p,s)` | `numeric(p,s)` | `numeric(38,s)` 上限注意 | `TEXT` 保全 | `TEXT` 降格 | DECIMAL(p,s) | なし→String | string | string | `N` (損失リスク大) |
| `Date32` | `date` | `date` | `TEXT` ISO | `DATE` (ISO8601 文字列) | DATE | なし→String | string ISO | ISO | `D` (YYYYMMDD) |
| `Time64(µs)` | `time` | `time` | `TEXT` ISO | `TEXT` ISO | TIME(µs) | なし→String | string | ISO | 非対応→error |
| `Timestamp(µs, None)` | `timestamp` | `datetime2` | `TEXT` ISO | `DATETIME` (ISO8601 文字列) | TIMESTAMP(µs) | なし→String | string ISO | ISO | 非対応→error |
| `Timestamp(µs, UTC)` | `timestamptz` | `datetimeoffset` | `TEXT` ISO8601 +Z | `DATETIME` (ISO8601 +Z) | TIMESTAMP(µs, UTC) | なし→String | string ISO+Z | ISO+Z | 非対応→error |
| `Utf8` / `LargeUtf8` | `text` | `nvarchar(max)` | `TEXT` | `TEXT` | STRING | String | string | quoted | `C` (cpg) |
| `Binary` / `LargeBinary` | `bytea` | `varbinary(max)` | `BLOB` | `BLOB` | BINARY | なし→String(b64) | 非対応→error | 非対応→error | 非対応→error |
| `List<T>` | `T[]` | 非対応→JSON 文字列 or error | `TEXT` JSON | `TEXT` JSON | LIST<T> | なし→String JSON | array | JSON文字列 | 非対応→error |
| `Struct<...>` | `jsonb` | `nvarchar(max)` JSON | `TEXT` JSON | `TEXT` JSON | GROUP | なし→String JSON | object | JSON文字列 | 非対応→error |
| `Geometry (Binary + meta)` | `geometry(...,srid)` / `geography(...,srid)` | `geometry` / `geography` | `BLOB` (GPKG binary header + WKB) | `BLOB` (SpatiaLite blob、XY のみ) | WKB (GeoParquet) | FlatGeobuf geom | GeoJSON geometry | WKT列 | SHP shape record |

GPKG / SpatiaLite はいずれも SQLite 上に乗るが、メタテーブル (`gpkg_*` vs `geometry_columns` / `spatial_ref_sys`) と geometry blob format が異なるため、shpx では URI scheme で完全に分離する（`*.gpkg` → GPKG driver、`*.sqlite` / `*.db` / `*.spatialite` / `sqlite://` → SpatiaLite driver）。

## 属性順序保存

- 読み手は物理列順を Arrow `Schema.fields` の順に再現する
- 書き手は `Schema.fields` 順に出力する
- RDB writer の `CREATE TABLE` も同じ順で生成
- 既存テーブルへの insert は、テーブル側カラム名と入力 Arrow フィールド名で突き合わせ（順序ではなく名前一致）。順序が違うと警告を出す（`--strict-order` で error 化）

## 文字列エンコーディング

- 中間表現は常に UTF-8
- 出力時の `--encoding utf-8|cp932|latin-1|...`
- 省略時のデフォルト:
  - SHP: utf-8（cpg ファイル自動生成、`UTF-8`）
  - GPKG: utf-8
  - GeoParquet: utf-8
  - CSV: utf-8 BOM 付き
- 読み取り時: SHP は cpg ファイル参照、無い場合 `--encoding` 必須（既定は警告 + utf-8 として読む）

## ジオメトリ表現

中間: `Binary` カラム + Arrow field metadata `"shpx:geometry" = JSON`：

```json
{
  "encoding": "WKB",
  "geometry_type": "Polygon",
  "crs": {"authority": ["EPSG", 4326]},
  "edges": "planar"
}
```

`geometry_type` は `Geometry`（任意） / `Point` / `LineString` / `Polygon` / `MultiPoint` / `MultiLineString` / `MultiPolygon` / `GeometryCollection` のいずれか。`edges` は `planar` / `spherical`（geography 用途）。

GeoParquet 仕様 v1.x との互換のため、Parquet 出力時は `geo` schema metadata に変換。

## decimal の扱い

- Arrow 内部は `Decimal128(p, s)` または `Decimal256(p, s)` を維持
- PostgreSQL `numeric(p, s)` ↔ Arrow `Decimal128/256(p, s)` は無損失
- SQL Server `decimal(p, s)` 上限 38 桁（`Decimal256` で 38 < p ≤ 76 のものは `--on-loss=warn` で `decimal(38, s)` へ切り詰めまたは error）
- DBF `N` フィールドの理論上限は 18 桁、実用上は 15 桁。超過は `--on-loss=error|warn|skip` で挙動制御
  - `warn`: 切り詰め（precision を下げる）+ 警告ログ
  - `skip`: フィールド自体を出力から除外
- SQLite/GPKG/CSV/GeoJSON は文字列として保全（数値型に丸めない）

## 日時の扱い

- Arrow `Timestamp(unit, tz)` で unit ∈ {Second, Millisecond, Microsecond, Nanosecond}、tz ∈ {None, "UTC", "+09:00", ...}
- 入出力での暗黙変換は行わない。tz がある値を tz 非対応形式へ書く場合は `--on-loss` 設定に従う
- SHP の `D` 型は日付のみ。`Timestamp` を SHP に書く場合:
  - `error`（既定）: 中断
  - `warn`: 日付に切り詰め + 警告
  - `skip`: フィールド除外
- ISO 8601 への変換は `chrono` で実装。`Z` サフィックス付きで UTC 明示。

## BLOB の扱い

- Arrow `Binary` / `LargeBinary` を中間に。
- 対応フォーマット: PostGIS (`bytea`)、SQL Server (`varbinary(max)`)、SQLite/GPKG (`BLOB`)、Parquet (`BINARY`)
- 非対応フォーマット: SHP、CSV、GeoJSON、FlatGeobuf
  - `error`（既定）: 中断
  - `warn`: フィールドをスキップして出力 + 警告
  - `skip`: フィールド除外（warn と挙動同じだが警告レベル抑制）
- GeoJSON で BLOB を扱う実用案として `--blob-encoding=base64` オプションを v1 で検討（既定は無効）

## 損失変換ポリシー詳細

`--on-loss <error|warn|skip>` の挙動マトリクス:

| シナリオ | error | warn | skip |
|---|---|---|---|
| BLOB → SHP/CSV/GeoJSON | 中断 | フィールドをスキップ + 警告 | フィールド除外（無音） |
| Decimal(38, s) → DBF | 中断 | 18桁に切り詰め + 警告 | フィールド除外 |
| Timestamp → SHP | 中断 | Date に切り詰め + 警告 | フィールド除外 |
| timestamptz → naive timestamp | 中断 | UTC として書き込み + 警告 | tz 落として書き込み（無音） |
| EPSG コード未登録 → PostGIS | 中断 | `spatial_ref_sys` へ自動 INSERT 試行 + 警告 | INSERT せず srid=0 |
| 文字列が出力エンコーディングで表現不可 | 中断 | `?` で置換 + 警告 | 該当文字を削除 |
| GeoJSON 出力で CRS が WGS84 でない | 中断 | reproject 強制 + 警告 | `--geojson-crs-extension` を強制有効化 |

`--on-loss` のデフォルトは `error`（安全側）。
