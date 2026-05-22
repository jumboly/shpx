# GDAL に依存せず全フォーマットコーデックを pure Rust で自前実装する

空間データ変換ツールの大半は GDAL/OGR に依存するが、shpx は **GDAL 非依存**を選び、Shapefile / GPKG / GeoParquet / FlatGeobuf / GeoJSON / CSV / PostGIS / SQL Server / SpatiaLite の読み書きを Rust crate + 自前コーデックで実装する。理由は配布形態にある — GDAL を介すと C/C++ ツールチェインと巨大な共有ライブラリに縛られ、`cargo install` での導入や OS 別の単一バイナリ配布（cargo-dist）が困難になる。バイナリサイズとビルドの単純さ、依存の見通しを優先し、各 Format のコーデックを自前で持つ実装コストを受け入れた。

代償として、GDAL が無償で提供する膨大な Format サポートを自分たちで実装・保守する必要がある（Driver 追加のたびにコーデックを書く）。この境界は意図的なものであり、「なぜ既存の GDAL を使わないのか」という問いへの回答としてここに記録する。

## Consequences

- libproj だけは例外的に C ライブラリ依存が残る（Reprojection に必須）。`bundled-proj` feature で static link し単一バイナリ化できるが、その場合は `cmake` + `clang` を要する。
- ラスター（GeoTIFF 等）は非ゴール。GDAL なしでラスターまで担うのは割に合わない。
