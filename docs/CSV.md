# CSV / TSV ドライバ仕様

`.csv` / `.tsv` 拡張子を `shpx-driver-csv` が担当する。WKT 文字列カラムをジオメトリとして扱い、それ以外の列はすべて `Utf8` 文字列として読み書きする。

## 対応範囲（v0.7 リリース時点）

- 読み: ヘッダ行ありの CSV/TSV、UTF-8（BOM 付/無し）/ CP932 / Latin-1 等
- 書き: ヘッダ行 + 行データ。geometry 列は WKT で出力、それ以外は型に応じた文字列化
- ジオメトリ型: Point / LineString / Polygon / MultiPoint / MultiLineString / MultiPolygon（XY のみ、`EMPTY` 不可）

## サポート対象拡張子

| 拡張子 | 区切り文字（既定）|
|---|---|
| `.csv` | `,` |
| `.tsv` | `\t` |

URL スキームは持たない (ローカル / NFS パスのみ)。区切り文字は `SHPX_CSV_DELIMITER` 環境変数で 1 バイト指定すれば上書き可能。

## CRS の扱い

CSV フォーマット自体には CRS 情報を持たない。

### 読み出し

優先順位:

1. `ReadOpts.src_crs`（CLI の `--src-crs`）
2. サイドカー `.prj` ファイル (CSV の隣に置かれた WKT1 形式の `.prj`、Shapefile と同型)
3. 解決不能なら `Crs` 無しでパイプラインを流通

### 書き出し

CSV 自体には書き出さない。下流フォーマット (PostGIS / Parquet 等) が CRS を要求する場合のみ `--src-crs` 経由で明示する運用となる。

## ジオメトリ

geometry 列以外の値は WKT (Well-Known Text、`shpx-geom::wkt` モジュール) に依存しない。

### 読み出し時の geometry 列同定

優先順位:

1. 環境変数 `SHPX_CSV_GEOMETRY_COLUMN` で明示された列名
2. ヘッダから case-insensitive で `geometry` / `geom` / `wkt` / `the_geom` を順に検索
3. どれも見つからなければ `Error::Schema`

### 書き出し時

geometry 列は `shpx-geom::wkt::encode` を経由して WKT 文字列として 1 列に書く。XY のみ対応。

## データ型マッピング

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
| `Binary` / `LargeBinary` (geometry 以外) | `--on-loss` 経由で `binary-on-csv` として処理 |
| `Geometry` (Binary + `shpx:geometry` meta) | WKT 文字列 |
| `List` / `Struct` / `Map` | サポート外、`--on-loss` 経由で `structured-on-csv` |

## 損失変換

`--on-loss=error|warn|skip` の挙動と、CSV driver が発する loss kind 一覧は [`docs/ON_LOSS.md`](ON_LOSS.md) を参照。CSV では `binary-on-csv` (列ごと除外) と `structured-on-csv` (warn でも書き出し時に拒否、skip のみ列ごと除外) の 2 種を扱う。

## CSV 固有オプション（暫定: 環境変数）

`WriteOpts` / `ReadOpts` に driver-specific 拡張機構が無いため、現時点では下記の環境変数で受ける (`Future work` 節参照)。

| 環境変数 | 用途 | 既定 |
|---|---|---|
| `SHPX_CSV_DELIMITER` | 区切り文字 1 バイト | 拡張子から推論（`,` / `\t`） |
| `SHPX_CSV_HAS_HEADER` | ヘッダ行有無 | `true` |
| `SHPX_CSV_GEOMETRY_COLUMN` | geometry 列名 | 候補から自動検出 |
| `SHPX_CSV_BOM` | 出力時 BOM ポリシー (`auto` / `always` / `never`) | `auto` |

エンコーディングは CLI の `--encoding cp932` 等で指定。既定 UTF-8、入力 BOM は自動で剥がす、UTF-8 以外では BOM を付与しない。

## スコープ外 / 制限

### `EMPTY` ジオメトリ

`POINT EMPTY` 等の `EMPTY` ジオメトリは未対応で、reader が `Error::Geometry` を返す。回避策は `--on-loss=skip` 等の方針として今後検討する。

### ヘッダなし CSV

`SHPX_CSV_HAS_HEADER=false` 設定時は現状エラーで弾く (未対応)。ヘッダ行を付けるか、固定列名 `c0..cN` での仮割当は Future work。

### 巨大ファイル

UTF-8 はストリーム読み込み、それ以外（CP932 等）は全読込してから `encoding_rs` で復号する。GB 級の非 UTF-8 CSV は v1.0 までに streaming 復号化を入れる課題（DESIGN.md の TODO 参照）。

### 型推定

行わない（上記「データ型マッピング」参照）。整数や日付として CSV ↔ SHP/Parquet の型保全をしたい場合は GeoParquet を経由する。

## 環境変数まとめ

| 変数 | 役割 |
|---|---|
| `SHPX_CSV_DELIMITER` | 区切り文字 1 バイト override |
| `SHPX_CSV_HAS_HEADER` | ヘッダ行有無 |
| `SHPX_CSV_GEOMETRY_COLUMN` | geometry 列名の明示 |
| `SHPX_CSV_BOM` | 出力時 BOM ポリシー |

## 内部実装メモ

- `Capabilities { read: true, write: true, bulk_load: false, supports_blob: false, supports_decimal: true (Utf8 化), supports_timestamp_tz: true, string_encoding: Configurable }`
- 読み出しは `csv::Reader` (UTF-8) または `encoding_rs` で復号後に `csv` crate に流す 2 経路。後者は現状全読込
- 書き出しは `csv::Writer` で 1 行ずつ flush。`Skip` 経路で除外された列は schema 側からも落としてヘッダ行を整合させる
- WKT エンコード / デコードは `shpx-geom::wkt` を共有 (GeoJSON / FGB driver 等と同モジュール)
- `apply_on_loss` ヘルパは `crates/shpx-driver-csv/src/util.rs` で `shpx-rdb-common` の薄ラッパとして定義 (RDB 共通の純粋ユーティリティだが file driver からも呼んで問題ない)

## Future work

- `WriteOpts.driver_specific: BTreeMap<String, String>` 追加と CLI `--driver-opt KEY=VALUE` 生成 (環境変数経由を置き換え)
- `EMPTY` ジオメトリのサポート (`shpx-geom::wkt` 拡張と同期)
- ヘッダなし CSV / 固定列名 `c0..cN` での仮割当
- 非 UTF-8 CSV の streaming 復号 (現状は全読込)
- 型推定の opt-in モード (`SHPX_CSV_INFER_TYPES=true` 等、誤判定リスクは利用者が負う前提)
