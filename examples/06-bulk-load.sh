#!/usr/bin/env bash
# 目的: PostGIS への bulk (COPY BINARY) と batch (multi-row INSERT) の
#       経路差を実体験させる。両経路で同じ入力を別テーブルに書き込み、
#       行数と schema が一致することを確認する。
# 前提: docker compose の postgis サービスが起動していること。
# 期待結果: bulk / batch 両方 exit 0、cities_bulk と cities_batch が同じ
#           5 行を持つ。
# 注意: ベンチマーク用途ではない (5 行では cargo run のオーバーヘッドが
#       本体処理を覆い隠す)。実測は scripts/bench-vs-ogr.sh を参照。

set -euo pipefail

SHPX_BIN="${SHPX_BIN:-cargo run --release -p shpx-cli --}"
PG_URL="${PG_URL:-pg://shpx:shpx@localhost:5432/shpx_test}"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

if ! (echo > /dev/tcp/localhost/5432) >/dev/null 2>&1; then
  cat <<EOF >&2
PostGIS が起動していません (localhost:5432 不通)。
  docker compose up -d postgis
を実行してから本スクリプトを再実行してください。
EOF
  exit 2
fi

echo "→ insert-mode=bulk (COPY BINARY)"
$SHPX_BIN convert "$HERE/data/cities.shp" \
  "${PG_URL}?table=public.cities_bulk&create=if-not-exists" \
  --insert-mode=bulk --overwrite

echo
echo "→ insert-mode=batch (multi-row INSERT)"
$SHPX_BIN convert "$HERE/data/cities.shp" \
  "${PG_URL}?table=public.cities_batch&create=if-not-exists" \
  --insert-mode=batch --overwrite

echo
echo "→ Verify both tables"
$SHPX_BIN info "${PG_URL}?table=public.cities_bulk"
echo
$SHPX_BIN info "${PG_URL}?table=public.cities_batch"

echo
echo "✓ Loaded both cities_bulk and cities_batch"
