# GeoJSON / GeoJSON Lines ドライバ仕様

`.geojson` (RFC 7946 FeatureCollection) と `.geojsonl` / `.ndjson` / `.jsonl` (1 行 1 Feature の NDJSON) を `shpx-driver-geojson` が担当する。

## 対応範囲（v0.7 リリース時点）

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
| `UInt64` | `i64::MAX` 以下は `Number`、超過は `--on-loss` 経由で `uint64-overflow-on-geojson` |
| `Float16` / `Float32` / `Float64` | `Number`（NaN / Infinity は `--on-loss` 経由で `nonfinite-float-on-geojson`、RFC 8259 で禁止のため `null` 化） |
| `Decimal128(p, s)` | `--on-loss` 経由で `decimal-on-geojson`（列除外）。RFC 7946 で `number` への昇格に精度損失が起きうるため |
| `Date32` / `Date64` | `"YYYY-MM-DD"` |
| `Timestamp(unit, None)` | `"YYYY-MM-DDTHH:MM:SS[.fff...]"`（unit ごとに小数桁数を切替） |
| `Timestamp(unit, "UTC"/"Z")` | 末尾 `Z` |
| `Timestamp(unit, "+09:00")` | 末尾 `+09:00` |
| `Utf8` / `LargeUtf8` | string |
| `Binary` / `LargeBinary` (geometry 以外) | `--on-loss` 経由で `binary-on-geojson`（列除外） |
| `Geometry` (Binary + `shpx:geometry` meta) | `{"type":"Point",...}` 等の Geometry オブジェクト |
| `List` / `Struct` / `Map` | `--on-loss` 経由で `structured-on-geojson`（warn でも書き出し時に拒否、skip のみ列除外） |
| `Timestamp(Nanosecond \| Microsecond, _)` | `--on-loss` 経由で `timestamp-precision-on-geojson`（列除外）。RFC 7946 表現は ms 精度までしか保証できないため |

## 損失変換

`--on-loss=error|warn|skip` の挙動と、GeoJSON driver が発する loss kind 一覧 (`binary-on-geojson` / `structured-on-geojson` / `decimal-on-geojson` / `uint64-overflow-on-geojson` / `nonfinite-float-on-geojson` / `timestamp-precision-on-geojson`) は [`docs/ON_LOSS.md`](ON_LOSS.md) を参照。

## スコープ外

### Z / M 座標（3D / 4D）

`[x, y, z]` 形式の Point などは未対応で、reader が `Error::Geometry("3D coordinates are not supported")` を返す。`shpx_geom::Geom` に Z/M を追加するタイミングで同時に解禁する (Future work)。

### `GeometryCollection`

GeoJSON `GeometryCollection` は `shpx_geom::Geom` に対応 variant が無いため、reader は `Error::Geometry` で拒否する。writer は中間表現に該当型が存在しないため発生しない。

### Foreign members

Feature / FeatureCollection の独自フィールド（RFC 7946 で許容される `foreign_members`）は **読み捨て** とし、書き出し時にも生成しない。保全したい場合は CSV / GeoParquet を経由して属性カラムに格納する。

### `Feature.id`

GeoJSON `Feature.id` は読み捨てる。v0.3 で `_id` 専用列としてラウンドトリップする選択肢を検討する。

### 巨大 FeatureCollection

v0.8 cycle 3 で reader を真のストリーミング化した。FeatureCollection は `crates/shpx-driver-geojson/src/stream.rs` の `FcFeatureStream` がカスタム JSON state machine で `[` までシーク → カンマ区切りで Feature を 1 つずつ deserialize、GeoJSONL は `BufRead::lines()` ベースで 1 行 1 Feature を逐次パースする。peak RSS は batch サイズ + I/O バッファに頭打ち。

ただし型推論は **先頭 N=1024 feature サンプル** に降格しているため、1024 件目以降に新しい properties キーが現れても列としては追加されない (既存列に対する型混在の demote は引き続き有効)。サンプル数は環境変数 `SHPX_GEOJSON_INFER_SAMPLE` で override できる (`0` 指定で型推論なし → 全列 Utf8 相当に倒す想定だが、現実装では空サンプルから推論された場合は属性列ゼロになる点に注意)。

### RFC 7946 違反入力への寛容性

旧仕様 (GeoJSON 2008) の top-level `crs` メンバは RFC 7946 では非推奨だが、本ドライバは互換性のために解釈する。書き出し時には `crs` メンバを生成しないため、入力に存在しても roundtrip しない。

## GeoJSON 固有オプション（暫定: 環境変数）

`WriteOpts` / `ReadOpts` に driver-specific 拡張機構が無いため、現時点では下記の環境変数で受ける。

| 環境変数 | 用途 | 既定 |
|---|---|---|
| `SHPX_GEOJSON_PRETTY` | FeatureCollection 出力時の pretty-print（`true`/`false`）。GeoJSONL では無視 | `false` |
| `SHPX_GEOJSON_INFER_SAMPLE` | reader の型推論サンプル件数 (FeatureCollection / NDJSON 共通)。streaming reader は先頭 N feature だけサンプリングして列スキーマを決める | `1024` |

## 環境変数まとめ

| 変数 | 役割 |
|---|---|
| `SHPX_GEOJSON_PRETTY` | FeatureCollection 出力時の pretty-print 切替 |
| `SHPX_GEOJSON_INFER_SAMPLE` | reader の型推論サンプル件数 |

## 内部実装メモ

- `Capabilities { read: true, write: true, bulk_load: false, supports_blob: false, supports_decimal: false (列除外), supports_timestamp_tz: true (秒精度のみ), string_encoding: Fixed("utf-8") }`
- 読み出しは `crates/shpx-driver-geojson/src/stream.rs` の `FcFeatureStream` (FeatureCollection 用、自前 state machine) または `NdjsonStream` (NDJSON 用、`BufRead::lines()` ベース) の 2 経路で、いずれも 1 feature ずつ pull する真のストリーミング
- 書き出しは `serde_json::Value` を組み立てて `serde_json::to_writer` で 1 行ずつ flush
- 非 EPSG:4326 入力の自動 reproject は `shpx-geom::Reprojector` を経由 (`shpx-cli` の `--reproject` 経路と同実装を共有)
- `apply_on_loss` ヘルパは `crates/shpx-driver-geojson/src/util.rs` で `shpx-rdb-common` の薄ラッパとして定義 (`tracing::warn!(target: "shpx::geojson", ...)` で driver target 固定)
- `null` geometry の Feature は Arrow `Binary` 列の `null` セルに対応

## Future work

- `--geojson-crs-extension` で非標準 `crs` メンバの書き出し（PostGIS / Leaflet 互換用途）
- `Feature.id` 専用列（`_id`）でのラウンドトリップ
- foreign members の保全（属性カラムまたは driver-specific メタデータ経由）
- GeometryCollection / Z / M 座標の対応（`shpx_geom::Geom` の拡張と同時）
