#!/usr/bin/env bash
# 目的: Web Mercator (EPSG:3857) の SHP を WGS84 (EPSG:4326) に再投影し、
#       出力 GeoParquet の geometry CRS が WGS84 になっていることを示す。
# 前提: なし (libproj が必要、`bundled-proj` feature でも可)。
# 期待結果: $OUT/cities-wgs84.parquet が生成され、schema text 出力で
#           authority=EPSG:4326 が確認できる。

set -euo pipefail

SHPX_BIN="${SHPX_BIN:-cargo run --release -p shpx-cli --}"
OUT="${OUT:-/tmp/shpx-examples}"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

mkdir -p "$OUT"

echo "→ Source CRS (EPSG:3857)"
$SHPX_BIN info "$HERE/data/cities-3857.shp"

echo
echo "→ Reproject EPSG:3857 → EPSG:4326"
$SHPX_BIN convert "$HERE/data/cities-3857.shp" "$OUT/cities-wgs84.parquet" \
  --reproject EPSG:4326 --overwrite

echo
echo "→ Output schema (look for crs.authority=EPSG,4326)"
$SHPX_BIN schema "$OUT/cities-wgs84.parquet" --format=text

echo
echo "✓ Wrote $OUT/cities-wgs84.parquet (EPSG:4326)"
