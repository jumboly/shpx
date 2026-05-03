# CRS（座標参照系）の扱い

shpx は CRS を **EPSG コード優先** + WKT2 / PROJJSON を補助的に保持する。各フォーマットへの出力時には、そのフォーマットが要求する形式へ変換する。

## 内部表現

```rust
pub struct Crs {
    /// 既知の authority + code がある場合（最頻出は ("EPSG", 4326) など）
    pub authority: Option<(String, u32)>,
    /// 元データから取得した WKT2 (ISO 19162:2019)
    pub wkt2: Option<String>,
    /// PROJ で解決した PROJJSON
    pub projjson: Option<String>,
}
```

優先順位:
1. `authority` があれば等価判定はそれを使う（最も軽量・高速）
2. なければ `wkt2` または `projjson` を PROJ に渡して `authority` を逆引き
3. それでも特定できなければ `wkt2` のままパイプラインを流通させる

## CRS 表現形式の比較

| 形式 | 例 | 特徴 |
|---|---|---|
| **EPSG コード** | `EPSG:4326` | 識別子。最短・最頻出 |
| **WKT1** (OGC 01-009) | `GEOGCS["WGS 84", DATUM[...]]` | 旧 GDAL、SHP `.prj` の伝統 |
| **WKT2** (ISO 19162:2019) | `GEOGCRS["WGS 84", DATUM[...], CS[...]]` | GeoParquet / 現代 GPKG。軸順や精度情報が厳密 |
| **PROJJSON** | JSON 構造 | PROJ の中間表現、プログラム処理向き |
| **proj-string** | `+proj=longlat +datum=WGS84` | 古典的、情報が欠落しやすい（非推奨） |

## 各フォーマットからの読み出しマッピング

各 driver は「ファイル / DB に書かれている CRS 情報を `Crs` 構造体へ解決する経路」を持つ。`--src-crs <SPEC>` が CLI で明示された場合はこの解決を上書きする (`--src-crs` > driver 解決 > フォールバック)。

| フォーマット | 読み出し時の解決経路 | 備考 |
|---|---|---|
| **GeoParquet** | Arrow field metadata の `geo` JSON (`columns.<geom>.crs`) を `shpx_geom::projjson::decode` で parse し、`Crs` の authority / wkt2 / projjson を復元 | v0.7 cycle 2 で実装 |
| **GeoPackage** | `gpkg_geometry_columns.srs_id` → `gpkg_spatial_ref_sys` の `definition` (WKT1) と `definition_12_063` (WKT2) を結合 | v0.7 cycle 2 で WKT2 経路を完備 |
| **Shapefile `.prj`** | サイドカー `.prj` (WKT1) を読み EPSG authority を抽出 | v0.1 から |
| **PostGIS** | table モード: `geometry_columns` view → 先頭行 `ST_SRID()` の 2 段で SRID 取得。query モード: サブクエリ経由の先頭 `ST_SRID()` のみ。WKT は `spatial_ref_sys` から引かない (`--src-crs` で WKT 直接指定可) | |
| **SQL Server** | `[col].STSrid` を `STAsBinary` と併走 SELECT で取得 | 空テーブルでは SRID 不明 |
| **SpatiaLite** | `geometry_columns.srid` を参照。`--src-crs` で上書き可 | SRID 0 は SpatiaLite 慣習で unknown |
| **FlatGeobuf** | header の `crs` field (org / code / wkt) を parse | v0.7 cycle 2 で実装 |
| **GeoJSON / GeoJSONL** | top-level `crs` メンバ (旧仕様、`urn:ogc:def:crs:EPSG::NNNN` / `urn:ogc:def:crs:OGC:1.3:CRS84`) を解釈、無ければ既定 EPSG:4326 | RFC 7946 互換 |
| **CSV** | サイドカー `.prj` または `--src-crs` で明示 | CSV 自体には CRS 情報がない |

## 各フォーマットへの出力マッピング

| フォーマット | 出力する CRS 表現 | 備考 |
|---|---|---|
| **GeoParquet** | PROJJSON（fallback: WKT2） | `geo` metadata の `columns.<geom>.crs` に格納 |
| **GeoPackage** | WKT1 + WKT2 | `gpkg_spatial_ref_sys` の `definition` (WKT1) + `definition_12_063` (WKT2) |
| **Shapefile `.prj`** | WKT1 | 互換性優先。PROJ で WKT2 → WKT1 変換 |
| **PostGIS** | SRID（EPSG 整数） | 未登録 EPSG なら `spatial_ref_sys` へ自動 INSERT を試行（`Crs.wkt` → `epsg_to_wkt1(code)` の順、ベストエフォート） |
| **SQL Server** | SRID（EPSG 整数） | `geometry::STGeomFromWKB(@wkb, @srid)` で投入 |
| **SpatiaLite** | SRID（EPSG 整数）+ WKT1 | `geometry_columns.srid` を読み出し / 書き出し時の主索引とし、未登録 EPSG は `spatial_ref_sys` に `INSERT OR IGNORE` で best-effort 登録（PostGIS と同パターン、`Crs.wkt` → `epsg_to_wkt1(code)` の順で `srtext` を解決） |
| **FlatGeobuf** | WKT2（header の `crs.wkt`） + EPSG code（`crs.code`） | 仕様で両方サポート |
| **GeoJSON / GeoJSONL** | （RFC 7946 準拠なら出力なし、WGS84 強制 reproject） | `--geojson-crs-extension` 指定時のみ非標準 `crs` メンバを書き出し |
| **CSV** | サイドカー `.prj` ファイル（WKT1）または `--crs` で明示 | CSV 自体には CRS 情報を持たない |

## RFC 7946（GeoJSON）と CRS

GeoJSON は仕様（RFC 7946）で **WGS84（経緯度、EPSG:4326）以外を非推奨** としている。これに従う:

- 既定: 入力が WGS84 でなければ自動 reproject して出力（PROJ 必須）
- `--geojson-crs-extension`: 旧 `crs` メンバを書き出して非 WGS84 を保持（互換性のため）
- `--reproject` で明示指定があればそれを優先

## Reprojection（座標変換）

オプション:
- `--reproject EPSG:3857`
- `--reproject 'PROJCRS["..."]'` (WKT2)
- `--reproject '+proj=...'` (proj-string、非推奨)

実装（v0.2 cycle 5 で完了）:
- `proj` crate（libproj バインディング）を `shpx-geom::Reprojector` から呼び出す
- パイプライン上で batch 単位に geom 列を WKB デコード → PROJ 変換 → WKB 再エンコード
- PROJ context は thread-local。`Proj` 自体が `!Send` なので `Reprojector` は CRS spec
  文字列のみを保持し、`Proj` は (src_spec, dst_spec) キーで thread-local キャッシュする
- `Proj::new_known_crs` が `proj_normalize_for_visualization` を適用するため、
  EPSG:4326 のような lat-lon 系も traditional XY (=lon, lat) で扱える
- 既定はシステム libproj を pkg-config で検出。`shpx-cli` の `bundled-proj` feature を
  有効化すると libproj/SQLite を C ソースから static link する（`cargo-dist` 配布用）

未対応（v0.3 以降）:
- `rayon` による batch 内並列化（v0.2 はシリアル）
- libproj の network grid 取得（`proj` の `network` feature は OFF）

## RFC 7946 (GeoJSON) の自動 reproject

GeoJSON writer は v0.2 cycle 5 から、非 EPSG:4326 入力を内部 `Reprojector` で
透過的に EPSG:4326 へ変換する。`--reproject` の明示指定無しで動作する。
入力 CRS が解決不能な場合のみ `Error::Crs` で停止する。

## EPSG コードの解決

- `proj` crate で `proj_create_from_user_input("EPSG:4326")` を使う
- `proj_get_authorities_from_database()` で authority 一覧
- 等価判定は `proj_is_equivalent_to` を `PJ_COMP_EQUIVALENT` で実行

## CRS が不明な場合（input に CRS なし）

- `--src-crs EPSG:xxxx` で明示指定可能
- 指定なしで CRS を要求するフォーマット（PostGIS の `geometry(...,srid)` など）に書く場合は `--on-loss` に従う:
  - `error`: 中断
  - `warn`: srid=0 で書き込み + 警告
  - `skip`: srid=0 で書き込み（無音）
