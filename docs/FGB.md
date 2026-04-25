# FlatGeobuf ドライバ仕様

`.fgb` (FlatGeobuf) を `shpx-driver-fgb` が担当する。GDAL 非依存方針に従い、公式 Rust 実装である [`flatgeobuf`](https://crates.io/crates/flatgeobuf) クレート (BSD-2-Clause、作者は仕様策定者の Björn Harrtell) を採用する。geometry の入出力は [`geozero`](https://crates.io/crates/geozero) を経由し、shpx 側の WKB 中間表現と FGB 内部の FlatBuffers geometry を相互変換する。

## 対応範囲（v0.2 サイクル 4）

- 読み:
  - `.fgb` ファイル全体を eager-load（巨大ファイルの streaming は v0.3+）
  - ヘッダから geometry_type / columns / CRS を抽出し Arrow `Schema` を構築
  - 各 feature の geometry を `geozero::wkb::WkbWriter` で WKB に書き出し、Arrow `Binary` 列に格納
  - 属性は `flatgeobuf::FgbFeature::process_properties` で 1 列ずつ受け取り、Arrow ArrayBuilder に蓄積
  - DateTime 列は観測値が `YYYY-MM-DD` のみなら `Date32`、`T` を含むなら `Timestamp(Microsecond, Some("UTC"))` に絞り込む
- 書き:
  - `flatgeobuf::FgbWriter::create_with_options` で名前 / geometry_type / CRS を指定して初期化
  - 属性列は `add_column` で先頭に登録、geometry は `add_feature_geom(Wkb(bytes), ..)` で書き出し
  - `finish()` で `BufWriter<File>` に対して `inner.write(...)` を呼び、ヘッダ / index / features を順次書き込む
  - **空間インデックス (packed Hilbert R-Tree) は出力しない** (`FgbWriterOptions { write_index: false, .. }` 固定)。理由は cycle 4 のスコープ抑制と、reader 側でインデックス利用機能 (`select_bbox`) を提供しない方針。v0.3+ で再評価する
- ジオメトリ型: Point / LineString / Polygon / MultiPoint / MultiLineString / MultiPolygon (XY のみ)

## サポート対象拡張子

| 拡張子 | スキーム |
|---|---|
| `.fgb` | fgb |

`Uri::from_path` が拡張子を小文字化済みのため、`.FGB` も透過に扱う。

## レイヤ概念

FGB は **1 ファイル 1 レイヤ** の仕様で、shpx もこれに従う。GPKG のような `?table=...` クエリは無く、URI はパスのみで完結する。dataset name (header の `name` フィールド) には出力ファイルの stem (`places.fgb` → `places`) を入れる。

## CRS の扱い

### 読み出し

優先順位:

1. `ReadOpts.src_crs` (CLI の `--src-crs`)
2. ヘッダの `crs.org` / `crs.code` / `crs.wkt`:
   - `org="EPSG"` (もしくは `org` 省略 = EPSG とみなす) かつ `code != 0` → `Crs::from_epsg(code)`
   - `wkt` のみあれば `Crs { authority: None, wkt: Some(_), wkt_flavor: V2, .. }`
3. それでも CRS が分からなければ `None`

### 書き出し

`Crs.authority` がある場合は header の `crs.org` / `crs.code` をそのまま出し、`wkt` は省略する (FGB は `org/code` 優先で解釈されるため)。`authority` が無く `wkt` のみある場合は `crs.wkt` を載せる。

CRS が `None` の場合は `OnLoss` を経由し、`Error` 経路では拒否、`Warn` 経路では `code=0` で書き出す (loss kind: `missing-crs-on-fgb`)。

## 型マッピング

### Arrow → FGB ColumnType

| Arrow 型 | FGB ColumnType | 備考 |
|---|---|---|
| `Boolean` | `Bool` | |
| `Int8` / `Int16` / `Int32` / `Int64` | `Byte` / `Short` / `Int` / `Long` | |
| `UInt8` / `UInt16` / `UInt32` | `UByte` / `UShort` / `UInt` | |
| `UInt64` | `ULong` | `i64::MAX` 超は `OnLoss` 経由で警告 (kind: `uint64-overflow-on-fgb`) |
| `Float32` / `Float64` | `Float` / `Double` | |
| `Utf8` / `LargeUtf8` | `String` | UTF-8 (FlatBuffers `string` 仕様) |
| `Binary` / `LargeBinary` | `Binary` | |
| `Date32` / `Date64` | `DateTime` | `%Y-%m-%d` の ISO8601 文字列で書き出す |
| `Timestamp(_, _)` | `DateTime` | UTC に正規化した `%Y-%m-%dT%H:%M:%S[.fff]Z` 文字列で書き出す |
| `Decimal128(p,s)` / `Decimal256(p,s)` | `Double` + `precision/scale/width` 注記 | f64 への降格は `OnLoss` 経由で警告 (kind: `decimal-on-fgb`)。FGB に Decimal 型が存在しないため |

### FGB ColumnType → Arrow

| FGB ColumnType | Arrow 型 | 備考 |
|---|---|---|
| `Bool` | `Boolean` | |
| `Byte` / `UByte` | `Int8` / `UInt8` | |
| `Short` / `UShort` | `Int16` / `UInt16` | |
| `Int` / `UInt` | `Int32` / `UInt32` | |
| `Long` / `ULong` | `Int64` / `UInt64` | |
| `Float` / `Double` | `Float32` / `Float64` | |
| `String` / `Json` | `Utf8` | `Json` は v0.2 では構造化として扱わず Utf8 |
| `Binary` | `Binary` | |
| `DateTime` | `Date32` または `Timestamp(Microsecond, Some("UTC"))` | 観測値の形式から refine |

## 損失処理

| 損失種別 | 内容 |
|---|---|
| `decimal-on-fgb` | Arrow `Decimal128/256` を FGB `Double` に降格 |
| `uint64-overflow-on-fgb` | `UInt64` で `i64::MAX` 超の値（roundtrip 安定性に影響） |
| `missing-crs-on-fgb` | CRS なしのデータを `code=0` で書き出す |

`--on-loss=error` で停止、`--on-loss=warn` で警告ログを出して続行、`--on-loss=skip` は Decimal/UInt64 の場合は値ベース処理が必要なため将来検討（cycle 4 では Warn 同等扱い）。

## 制限

- **Z / M 座標は未対応** (XY のみ。FGB header の `has_z` / `has_m` を出力時は常に `false` で固定)
- **GeometryCollection は未対応** (writer 側で `Geometry::GeometryCollection` を渡すと WKB encode 済みであれば書けるが、roundtrip テスト未整備)
- **null geometry の行は writer で拒否** (現状の `flatgeobuf::FgbWriter` は `add_feature_geom` に必須 geometry を要求するため)
- **packed Hilbert R-Tree は出力しない** (常に `index_node_size=0`)
- **`select_bbox` (インデックス利用 bbox 検索) は未提供**
- **HTTP feature は無効** (`flatgeobuf` を `default-features = false` でビルドし、reqwest を持ち込まない)
- **`Json` カラムは Utf8 として扱う**
- **`circularstring` 等の SQL-MM Part 3 曲線型は未対応** (`flatgeobuf` がサポートする型のうち XY-only の Simple Features 範囲のみ)

## Future work

- packed Hilbert R-Tree インデックスの生成と `--bbox` フィルタ
- streaming reader (eager-load から外す)
- Z/M 座標
- `Json` カラム → Arrow `Struct` の双方向マッピング
- HTTP feature の opt-in (`shpx-driver-fgb` の crate feature として再有効化)
