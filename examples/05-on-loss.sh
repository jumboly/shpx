#!/usr/bin/env bash
# 目的: --on-loss=error|warn|skip の挙動の差を CSV → SHP 変換で示す。
#       lossy.csv は >10 bytes の列名 (station_identifier 等) を持ち、
#       SHP の DBF が課す 10-byte 制限に引っ掛かる (loss kind:
#       dbf-name-truncation)。
# 前提: なし (DB 不要)。
# 期待結果:
#   - error 経路: exit 1、変換失敗、SHP 出力なし
#   - warn  経路: exit 0、stderr に WARN ログ × 3、SHP 出力あり
#   - skip  経路: exit 0、stderr に WARN ログ × 3、SHP 出力あり
#                (現状 dbf-name-truncation は skip でも継続する設計)

set -euo pipefail

SHPX_BIN="${SHPX_BIN:-cargo run --release -p shpx-cli --}"
OUT="${OUT:-/tmp/shpx-examples}"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

mkdir -p "$OUT"

CSV="$HERE/data/lossy.csv"

echo "================================================================"
echo "1) --on-loss=error  (default before v0.7) — should fail"
echo "================================================================"
if $SHPX_BIN convert "$CSV" "$OUT/lossy-error.shp" \
    --src-crs EPSG:4326 --on-loss=error --overwrite; then
  ec=0
else
  ec=$?
fi
echo "exit=$ec (expected: 1)"

echo
echo "================================================================"
echo "2) --on-loss=warn  (default since v0.7) — proceeds with WARN"
echo "================================================================"
$SHPX_BIN convert "$CSV" "$OUT/lossy-warn.shp" \
  --src-crs EPSG:4326 --on-loss=warn --overwrite
echo "exit=$? (expected: 0)"

echo
echo "================================================================"
echo "3) --on-loss=skip  — silently drops loss kinds where applicable"
echo "================================================================"
$SHPX_BIN convert "$CSV" "$OUT/lossy-skip.shp" \
  --src-crs EPSG:4326 --on-loss=skip --overwrite
echo "exit=$? (expected: 0)"

echo
echo "→ Inspect truncated DBF column names"
$SHPX_BIN schema "$OUT/lossy-warn.shp" --format=text

echo
echo "✓ Compared --on-loss=error/warn/skip on $CSV"
