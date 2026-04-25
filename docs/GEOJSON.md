# GeoJSON / GeoJSON Lines ドライバ仕様

`.geojson` (RFC 7946 FeatureCollection) と `.geojsonl` / `.ndjson` / `.jsonl` (1 行 1 Feature の NDJSON) を `shpx-driver-geojson` が担当する。

## 対応範囲（v0.2 サイクル 2）

- 読み:
  - `.geojson` — `FeatureCollection` または単発 `Feature`
  - `.geojsonl` / `.ndjson` / `.jsonl` — 1 行 1 Feature。空行と先頭 `#` のコメント行は寛容に skip
  - 旧仕様 top-level `crs` メンバ（`urn:ogc:def:crs:EPSG::NNNN` / `EPSG:NNNN` / `urn:ogc:def:crs:OGC:1.3:CRS84`）の解釈
- 書き:
  - `.geojson` — `{"type":"FeatureCollection","features":[...]}`
  - `.geojsonl` / `.ndjson` / `.jsonl` — 改行区切り 1 Feature/行
  - **出力は EPSG:4326 固定**（RFC 7946 §4 準拠）。非 WGS84 入力は内部 `Reprojector` で自動変換する。`crs` メンバは出力しない
- ジオメトリ型: Point / LineString / Polygon / MultiPoint / MultiLineString / MultiPolygon（XY のみ）

## サポート対象拡張子と出力形式

| 拡張子 | 出力形式 |
|---|---|
| `.geojson` | FeatureCollection |
| `.geojsonl` | GeoJSON Lines (NDJSON) |
| `.ndjson` | GeoJSON Lines (NDJSON) |
| `.jsonl` | GeoJSON Lines (NDJSON) |

`Uri::from_path` が拡張子を小文字化済みのため、`.GeoJSON` も透過に扱う。

## CRS の扱い

### 読み出し

優先順位:

1. `ReadOpts.src_crs`（CLI の `--src-crs`）
2. `.geojson` の場合のみ、top-level `crs` メンバ（旧仕様）
3. RFC 7946 既定の **EPSG:4326**

GeoJSONL には top-level の概念が無いため、上記 1 → 3 の順で補完する。

`crs` メンバは RFC 7946 で非推奨だが、PostGIS / Leaflet / 古い GeoJSON 拡張で広く使われるため寛容に解釈する。`urn:ogc:def:crs:OGC:1.3:CRS84` は EPSG:4326 と同義として扱う。

### 書き込み

RFC 7946 §4 に従い出力は **EPSG:4326 のみ**。v0.2 cycle 5 から、非 WGS84 入力は内部
`Reprojector` で透過的に EPSG:4326 へ変換してから書き出す（明示的な `--reproject` 指定は不要）。
入力 CRS が解決できない場合（ファイルに CRS 情報が無く `--src-crs` も未指定）のみ、
`Error::Crs` で停止する。

`crs` メンバは出力しない（RFC 7946 §3.3）。

## ジオメトリ

サポート型は WKB ↔ GeoJSON で双方向変換する 6 種:

- `Point`
- `LineString`
- `Polygon`（外周 + 穴）
- `MultiPoint`
- `MultiLineString`
- `MultiPolygon`

`null` geometry はそのまま保持し、Arrow `Binary` 列の `null` セルとして読み書きする。

## properties の型推論（読み出し）

全 Feature の properties をスニフして列ごとに Arrow 型を決める。

| JSON 型 | Arrow 型 |
|---|---|
| `null` | 値として `null` を append、列は nullable=true |
| `bool` | `Boolean` |
| `number`（整数, `i64` 範囲内） | `Int64` |
| `number`（小数 / `i64` 範囲外） | `Float64` |
| `string` | `Utf8` |
| 配列 / オブジェクト | `Utf8`（`Value::to_string()` で文字列化、`tracing::warn!` を 1 度だけ出す） |

混在時の昇格規則:

- `Int` ↔ `Int` → `Int64`
- `Int` ↔ `Float`（順不問）→ `Float64`
- `Bool` ↔ `Bool` → `Boolean`
- `String` ↔ `String` → `Utf8`
- 異種混在（例: `Int` と `String`）→ `Utf8`（値は JSON 表現で文字列化）

列の出現順は **最初に出現した Feature のキー順**。後続 Feature で初登場するキーは末尾に append する。あるキーが特定の Feature で missing または `null` の場合、該当行は `null` を append する。

## 型マッピング（書き出し）

| Arrow 型 | JSON 表現 |
|---|---|
| `null` | `null` |
| `Boolean` | `true` / `false` |
| `Int8..Int64` / `UInt8..UInt32` | `Number` |
| `UInt64` | `i64::MAX` 以下は `Number`、超過は `--on-loss=warn` で文字列、`error` で停止 |
| `Float16` / `Float32` / `Float64` | `Number`（NaN / Infinity は `--on-loss=warn` で `null`、`error` で停止。RFC 8259 で禁止のため） |
| `Decimal128(p, s)` | 文字列（精度保全のため。`--on-loss=warn` のみ許容、`error` は停止、`skip` は列除外） |
| `Date32` / `Date64` | `"YYYY-MM-DD"` |
| `Timestamp(unit, None)` | `"YYYY-MM-DDTHH:MM:SS[.fff...]"`（unit ごとに小数桁数を切替） |
| `Timestamp(unit, "UTC"/"Z")` | 末尾 `Z` |
| `Timestamp(unit, "+09:00")` | 末尾 `+09:00` |
| `Utf8` / `LargeUtf8` | string |
| `Binary` / `LargeBinary` (geometry 以外) | `--on-loss=error` 中断、`warn` で `null`、`skip` で列除外 |
| `Geometry` (Binary + `shpx:geometry` meta) | `{"type":"Point",...}` 等の Geometry オブジェクト |
| `List` / `Struct` / `Map` | サポート外（`skip` で列除外、その他は中断） |

## 制約と未対応事項

### Z / M 座標（3D / 4D）

`[x, y, z]` 形式の Point などは v0.2 サイクル 2 では未対応で、reader が `Error::Geometry("3D coordinates are not supported")` を返す。`shpx_geom::Geom` に Z/M を追加する v0.3 で同時に解禁する。

### `GeometryCollection`

GeoJSON `GeometryCollection` は `shpx_geom::Geom` に対応 variant が無いため、reader は `Error::Geometry` で拒否する。writer は中間表現に該当型が存在しないため発生しない。

### Foreign members

Feature / FeatureCollection の独自フィールド（RFC 7946 で許容される `foreign_members`）は **読み捨て** とし、書き出し時にも生成しない。保全したい場合は CSV / GeoParquet を経由して属性カラムに格納する。

### `Feature.id`

GeoJSON `Feature.id` は読み捨てる。v0.3 で `_id` 専用列としてラウンドトリップする選択肢を検討する。

### 巨大 FeatureCollection

現サイクルでは FeatureCollection / GeoJSONL のいずれも全件メモリロードする（`Vec<geojson::Feature>`）。`struson` 等の streaming JSON parser を導入してインクリメンタル読みに切り替えるのは Future work。

### Decimal の精度

JSON `Number` は IEEE754 で精度欠落するため、Decimal128 は文字列降格 (`--on-loss=warn`) のみ許容する。`error` 経路では停止し、`skip` で列除外する。

### NaN / Infinity

RFC 8259 で `Number` 表現が禁じられているため、Float の NaN / Infinity は `--on-loss=warn` で `null`、`error` で停止する。

### RFC 7946 違反入力への寛容性

旧仕様 (GeoJSON 2008) の top-level `crs` メンバは RFC 7946 では非推奨だが、本ドライバは互換性のために解釈する。書き出し時には `crs` メンバを生成しないため、入力に存在しても roundtrip しない。

## GeoJSON 固有オプション（暫定: 環境変数）

`WriteOpts` / `ReadOpts` に driver-specific 拡張機構が無いため、v0.2 サイクル 2 では下記の環境変数で受ける。

| 環境変数 | 用途 | 既定 |
|---|---|---|
| `SHPX_GEOJSON_PRETTY` | FeatureCollection 出力時の pretty-print（`true`/`false`）。GeoJSONL では無視 | `false` |

### Future work

- `--geojson-crs-extension` で非標準 `crs` メンバの書き出し（PostGIS / Leaflet 互換用途）
- `Feature.id` 専用列（`_id`）でのラウンドトリップ
- foreign members の保全（属性カラムまたは driver-specific メタデータ経由）
- `struson` 等での streaming JSON 読み出し
- GeometryCollection / Z / M 座標の対応（`shpx_geom::Geom` の v0.3 拡張と同時）
