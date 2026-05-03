#!/usr/bin/env bash
# 目的: SHP を PostGIS テーブルに COPY バルクロードする。
# 前提: docker compose の postgis サービスが起動していること。
#       `docker compose up -d postgis` で起動する。
# 期待結果: public.cities テーブルが作成され、5 行が入る。

set -euo pipefail

SHPX_BIN="${SHPX_BIN:-cargo run --release -p shpx-cli --}"
PG_URL="${PG_URL:-pg://shpx:shpx@localhost:5432/shpx_test}"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# ヘルスチェック: 5432 が listen していなければ docker 案内を出して exit。
if ! (echo > /dev/tcp/localhost/5432) >/dev/null 2>&1; then
  cat <<EOF >&2
PostGIS が起動していません (localhost:5432 不通)。
  docker compose up -d postgis
を実行してから本スクリプトを再実行してください。
EOF
  exit 2
fi

echo "→ Bulk load SHP into PostGIS (cities table)"
$SHPX_BIN convert "$HERE/data/cities.shp" \
  "${PG_URL}?table=public.cities&create=if-not-exists" \
  --overwrite

echo
echo "→ Verify with shpx info"
$SHPX_BIN info "${PG_URL}?table=public.cities"

echo
echo "✓ Loaded into ${PG_URL%\?*}, table=public.cities"
