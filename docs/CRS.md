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

## 各フォーマットへの出力マッピング

| フォーマット | 出力する CRS 表現 | 備考 |
|---|---|---|
| **GeoParquet** | PROJJSON（fallback: WKT2） | `geo` metadata の `columns.<geom>.crs` に格納 |
| **GeoPackage** | WKT1 + WKT2 | `gpkg_spatial_ref_sys` の `definition` (WKT1) + `definition_12_063` (WKT2) |
| **Shapefile `.prj`** | WKT1 | 互換性優先。PROJ で WKT2 → WKT1 変換 |
| **PostGIS** | SRID（EPSG 整数） | 未登録 EPSG なら `spatial_ref_sys` へ自動 INSERT を試行（`--on-loss` 設定に従う） |
| **SQL Server** | SRID（EPSG 整数） | `geometry::STGeomFromWKB(@wkb, @srid)` で投入 |
| **SpatiaLite** | WKT1 + proj-string | `spatial_ref_sys` テーブルに両方格納（SpatiaLite 慣習） |
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

実装:
- パイプライン上で batch 単位に WKB デコード → PROJ 変換 → WKB 再エンコード
- `rayon` で batch 内並列、`tokio` で batch 間 pipelining
- PROJ context は thread-local（PROJ は非 thread-safe）

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
