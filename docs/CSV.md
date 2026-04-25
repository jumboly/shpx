# CSV / TSV ドライバ仕様

`.csv` / `.tsv` 拡張子を `shpx-driver-csv` が担当する。WKT 文字列カラムをジオメトリとして扱い、それ以外の列はすべて `Utf8` 文字列として読み書きする。

## 対応範囲（v0.2 サイクル 1）

- 読み: ヘッダ行ありの CSV/TSV、UTF-8（BOM 付/無し）/ CP932 / Latin-1 等
- 書き: ヘッダ行 + 行データ。geometry 列は WKT で出力、それ以外は型に応じた文字列化
- ジオメトリ型: Point / LineString / Polygon / MultiPoint / MultiLineString / MultiPolygon（XY のみ、`EMPTY` 不可）

## geometry 列の同定（読み出し時）

優先順位:

1. 環境変数 `SHPX_CSV_GEOMETRY_COLUMN` で明示された列名
2. ヘッダから case-insensitive で `geometry` / `geom` / `wkt` / `the_geom` を順に検索
3. どれも見つからなければ `Error::Schema`

## 区切り文字

- 拡張子 `.csv` → `,`
- 拡張子 `.tsv` → `\t`
- 環境変数 `SHPX_CSV_DELIMITER`（1 バイト）で上書き可能

## エンコーディング

- 既定 UTF-8。`--encoding cp932` 等で変更可能
- 出力時の BOM ポリシーは `SHPX_CSV_BOM=auto|always|never`（既定 `auto`）。UTF-8 以外では BOM を付与しない
- 入力時の BOM は自動で剥がす

## 型マッピング

### 読み出し

geometry 列以外は **すべて `Utf8`** で読む。型推定（数値・日付）は行わない。

> 理由: 型推定は誤判定のリスクが大きく、`shpx convert csv → parquet/shp` の経路で型が破壊されやすい。型保全用途は GeoParquet を使う前提とする。

### 書き出し

| Arrow 型 | CSV 表現 |
|---|---|
| `Boolean` | `true` / `false` |
| `Int*` / `UInt*` / `Float*` | `Display` |
| `Decimal128(p, s)` | 整数 ÷ 10^s を文字列で（無損失） |
| `Date32` / `Date64` | `YYYY-MM-DD` |
| `Timestamp(unit, None)` | `YYYY-MM-DDTHH:MM:SS[.fff...]`（unit ごとに小数桁数を切替） |
| `Timestamp(unit, "UTC"/"Z")` | 末尾 `Z` |
| `Timestamp(unit, "+09:00")` | 末尾 `+09:00` |
| `Utf8` / `LargeUtf8` | そのまま（`csv` crate が quote 処理） |
| `Binary` / `LargeBinary` (geometry 以外) | `--on-loss=error` 中断、`warn` で空文字、`skip` で列除外 |
| `Geometry` (Binary + `shpx:geometry` meta) | WKT 文字列 |
| `List` / `Struct` / `Map` | サポート外（`--on-loss=skip` で列除外、その他は中断） |

## 制約と未対応事項

### `EMPTY` ジオメトリ

`POINT EMPTY` 等の `EMPTY` ジオメトリは v0.2 サイクル 1 では未対応で、reader が `Error::Geometry` を返す。回避策は `--on-loss=skip` 等の方針を v0.3 で検討する。

### ヘッダなし CSV

`SHPX_CSV_HAS_HEADER=false` 設定時は現状エラーで弾く（v0.2 サイクル 1 では未対応）。ヘッダ行を付けるか、固定列名 `c0..cN` での仮割当は次サイクルで導入予定。

### 巨大ファイル

UTF-8 はストリーム読み込み、それ以外（CP932 等）は全読込してから `encoding_rs` で復号する。GB 級の非 UTF-8 CSV は v1.0 までに streaming 復号化を入れる課題（DESIGN.md の TODO 参照）。

### 型推定

行わない（前述）。整数や日付として CSV ↔ SHP/Parquet の型保全をしたい場合は GeoParquet を経由する。

## CSV 固有オプション（暫定: 環境変数）

`WriteOpts` / `ReadOpts` に driver-specific 拡張機構が無いため、v0.2 サイクル 1 では下記の環境変数で受ける。

| 環境変数 | 用途 | 既定 |
|---|---|---|
| `SHPX_CSV_DELIMITER` | 区切り文字 1 バイト | 拡張子から推論（`,` / `\t`） |
| `SHPX_CSV_HAS_HEADER` | ヘッダ行有無 | `true` |
| `SHPX_CSV_GEOMETRY_COLUMN` | geometry 列名 | 候補から自動検出 |
| `SHPX_CSV_BOM` | BOM 出力ポリシー | `auto` |

### Future work

次サイクル（GeoJSON 投入時）で `WriteOpts.driver_specific: BTreeMap<String, String>` を追加し、CLI に `--driver-opt KEY=VALUE` を生やす。それまでは環境変数で運用する。
