#!/usr/bin/env bash
# 目的: SHP を GeoParquet に変換し、出力 schema を text 形式で確認する。
# 前提: なし (DB 不要)。`examples/data/cities.shp` を入力に使う。
# 期待結果: $OUT/cities.parquet が生成され、shpx schema が driver=parquet を返す。

set -euo pipefail

SHPX_BIN="${SHPX_BIN:-cargo run --release -p shpx-cli --}"
OUT="${OUT:-/tmp/shpx-examples}"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

mkdir -p "$OUT"

echo "→ Convert SHP to GeoParquet"
$SHPX_BIN convert "$HERE/data/cities.shp" "$OUT/cities.parquet" --overwrite

echo
echo "→ Schema (text format)"
$SHPX_BIN schema "$OUT/cities.parquet" --format=text

echo
echo "✓ Wrote $OUT/cities.parquet"
