#!/usr/bin/env bash
# 目的: PostGIS テーブルを FlatGeobuf に書き出す。
# 前提: 02-shp-to-postgis.sh が成功し、public.cities が存在すること。
#       postgis サービスが起動していること。
# 期待結果: $OUT/cities.fgb が生成される (random access index 付き)。

set -euo pipefail

SHPX_BIN="${SHPX_BIN:-cargo run --release -p shpx-cli --}"
PG_URL="${PG_URL:-pg://shpx:shpx@localhost:5432/shpx_test}"
OUT="${OUT:-/tmp/shpx-examples}"

if ! (echo > /dev/tcp/localhost/5432) >/dev/null 2>&1; then
  cat <<EOF >&2
PostGIS が起動していません (localhost:5432 不通)。
  docker compose up -d postgis
を実行し、先に 02-shp-to-postgis.sh で public.cities を投入してください。
EOF
  exit 2
fi

mkdir -p "$OUT"

echo "→ Export PostGIS public.cities to FlatGeobuf"
$SHPX_BIN convert "${PG_URL}?table=public.cities" "$OUT/cities.fgb" --overwrite

echo
echo "→ Schema (text format)"
$SHPX_BIN schema "$OUT/cities.fgb" --format=text

echo
echo "✓ Wrote $OUT/cities.fgb"
