# Changelog

本プロジェクトの変更履歴。フォーマットは [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) に準拠し、バージョン番号は [Semantic Versioning](https://semver.org/spec/v2.0.0.html) に従う。

## [Unreleased]

## [0.2.0] - 2026-04-25

v0.2 マイルストーン「GPKG / GeoJSON / CSV / FlatGeobuf + Reprojection」のリリース。

### Added

- **shpx-driver-fgb**: FlatGeobuf (`.fgb`) reader/writer。公式 Rust 実装 `flatgeobuf` 6.0 (BSD-2-Clause) を採用し、`default-features = false` で HTTP feature を排除。geometry の入出力は `geozero` 経由で WKB ↔ FGB FlatBuffers を変換する。Bool / Byte / UByte / Short / UShort / Int / UInt / Long / ULong / Float / Double / String / Binary / DateTime の各 ColumnType に対応。Date32 と Timestamp は ISO8601 文字列で `DateTime` 列に書き、reader 側は観測値の形式から `Date32` か `Timestamp(Microsecond, UTC)` に絞り込む。CRS は EPSG コード優先 + WKT2 フォールバック。**packed Hilbert R-Tree インデックスは出力しない** (`index_node_size=0` 固定)。Z/M / GeometryCollection / `select_bbox` / null geometry / `Json` 列の構造化保持は未対応 (詳細は `docs/FGB.md` の「制限」を参照)。
- **shpx-driver-csv**: WKT 列付き CSV/TSV reader/writer（`.csv` / `.tsv`）。geometry 列は `geometry` / `geom` / `wkt` / `the_geom` のいずれか、または `SHPX_CSV_GEOMETRY_COLUMN` 環境変数で明示。geometry 以外の列は全て `Utf8` として読み書きする（型推定なし）。CSV 固有オプションは暫定で環境変数経由（`SHPX_CSV_*`）。詳細は `docs/CSV.md` を参照。
- **shpx-driver-geojson**: GeoJSON FeatureCollection (`.geojson`) と GeoJSON Lines / NDJSON (`.geojsonl` / `.ndjson` / `.jsonl`) の reader/writer。属性 (properties) の JSON 型を Arrow 列に推論（Bool / Int64 / Float64 / Utf8、混在は昇格）。出力は RFC 7946 §4 準拠の EPSG:4326 固定で、非 WGS84 入力は内部 `Reprojector` で自動変換する（cycle 5）。旧仕様の top-level `crs` メンバ（`urn:ogc:def:crs:EPSG::NNNN` / `urn:ogc:def:crs:OGC:1.3:CRS84`）の解釈に対応。GeometryCollection と Z/M 座標は未対応。詳細は `docs/GEOJSON.md` を参照。
- **shpx-driver-gpkg**: GeoPackage 1.3 (`.gpkg`) reader/writer。`rusqlite` (`bundled` SQLite) ベースで `gpkg_spatial_ref_sys` / `gpkg_contents` / `gpkg_geometry_columns` を初期化。geometry blob は envelope_type=0 / Standard / LE 固定で書き出し、reader は全 envelope_type と LE/BE を読み飛ばす。テーブル名は URI クエリ `?table=<name>` または環境変数 `SHPX_GPKG_TABLE` / `SHPX_GPKG_OUT_TABLE` で指定可能（未指定で feature テーブル単一なら自動採用、複数なら候補を列挙してエラー）。CRS は EPSG コード優先 + WKT1 フォールバック。Z/M 座標、空間インデックス、複数レイヤ append、`gpkg_extensions` は未対応（`docs/GPKG.md` の Future work 参照）。
- **shpx-geom**: GeoPackage Binary header の encode/decode を `gpkg_blob` モジュールに追加。GPKG/SpatiaLite で再利用できる独立コーデック。
- **shpx-geom**: WKT (Well-Known Text, OGC SFA 1.2.1) の encode/decode を `wkt` モジュールに追加。XY のみ対応、`EMPTY` は未サポート。
- **shpx-geom: Reprojector**: `proj 0.28` クレート（システム libproj を pkg-config で検出）経由で WKB ↔ WKB の座標変換を行う。`Proj::new_known_crs` が `proj_normalize_for_visualization` を適用するため lat-lon 系も traditional XY 順で扱える。`Proj` は `!Send` のため `Reprojector` は CRS spec 文字列のみ保持し、`Proj` は `(src_spec, dst_spec)` キーで thread-local キャッシュする。`shpx_geom::for_each_coord_mut` で `Geom` の各 variant を走査する補助関数も追加。
- **shpx-cli (`--reproject`)**: `convert` サブコマンドに `--reproject <SPEC>` を追加。`EPSG:xxxx` / WKT2 / proj-string / PROJJSON を受理する。入力 CRS が解決できない場合は `--src-crs` の指定を促すエラーで停止する。同一 CRS 指定時は no-op パスにフォールスルー。出力先 driver に渡す `Crs` 引数と Arrow schema の field metadata の双方を target に揃えるため、出力ファイルの CRS タグも反映される。
- **shpx-cli (`bundled-proj` feature)**: 配布用単一バイナリ（`cargo-dist`）向けに `shpx-cli` の `bundled-proj` feature を追加。有効化すると `proj` クレートが libproj/SQLite を C ソースから static link する（`cmake` / `clang` 必須）。default ビルドはシステム libproj を pkg-config 経由で利用するため、これらの追加ビルドツールを要求しない。

### Changed

- workspace MSRV を 1.79 → 1.85 に引き上げ。FlatGeobuf ドライバが依存する `flatgeobuf` 6.0 が `rust-version = 1.85` を要求するため。CI matrix も `["1.85", "stable"]` に更新。
- **GeoJSON writer**: 非 EPSG:4326 入力でのエラー停止を内部 `Reprojector` 経由の自動 WGS84 変換に置き換え（RFC 7946 §4 準拠）。入力 CRS が解決できない場合のみ `Error::Crs` で停止する挙動は維持。

### Build

- CI (`.github/workflows/ci.yml`): clippy / test ジョブで `libproj-dev` / `pkg-config` を apt インストール。`proj` クレートのビルドに必要。

## [0.1.0] - 2026-04-25

v0.1 マイルストーン「コア骨格 / SHP ↔ GeoParquet PoC」のリリース。

### Added

- **shpx-core**: `Driver` / `LayerReader` / `LayerWriter` / `BulkLoadWriter` トレイト、`Schema` / `Capabilities` / `Crs` / `Uri` / `ReadOpts` / `WriteOpts` / `OnLoss` 型。
- **shpx-geom**: WKB encoder/decoder（little/big endian、Point/LineString/Polygon/MultiPoint/MultiLineString/MultiPolygon）、WKT1 `.prj` からの EPSG コード抽出、最小 PROJJSON 生成。
- **shpx-driver-shp**: Shapefile reader/writer。`.shp`/`.shx`/`.dbf`/`.prj`/`.cpg` サイドカー対応、DBF cpg 文字エンコーディング切替（utf-8/cp932/latin-1）、Decimal128 / Date32 / Utf8 の保全。
- **shpx-driver-parquet**: GeoParquet 1.0 reader/writer。Arrow `Binary` 列に WKB を格納、KeyValue メタの `geo` JSON で CRS（PROJJSON）と geometry_type を伝搬。
- **shpx-cli**: `shpx convert <src> <dst>` サブコマンド（`--overwrite` / `--on-loss` / `--encoding` / `--batch-size` / `--src-crs`）、`shpx info <src>` サブコマンド。`-v`/`-vv`/`-vvv` で tracing ログレベル制御、`RUST_LOG` も尊重。
- 拡張子から driver を推論する static レジストリ（v0.2 で `inventory` ベースに移行予定）。
- CI: `cargo fmt --check` / `cargo clippy --all-targets -- -D warnings` / `cargo test --workspace` を MSRV 1.79 と stable の 2 toolchain で実行。

### Notes

- CRS は EPSG コードのみで保持・伝搬する。`--reproject` による座標変換は v0.2 で `proj` クレート統合とともに実装予定。
- PostGIS / SQL Server / SpatiaLite / GeoPackage / GeoJSON / FlatGeobuf / CSV は後続マイルストーン (v0.2–v0.5) で対応する。
- ライセンスは v1.0 までに最終決定する（MIT / Apache-2.0 dual を想定）。

[Unreleased]: https://github.com/jumboly/shpx/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/jumboly/shpx/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/jumboly/shpx/releases/tag/v0.1.0
