# Changelog

本プロジェクトの変更履歴。フォーマットは [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) に準拠し、バージョン番号は [Semantic Versioning](https://semver.org/spec/v2.0.0.html) に従う。

## [Unreleased]

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

[Unreleased]: https://github.com/jumboly/shpx/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/jumboly/shpx/releases/tag/v0.1.0
