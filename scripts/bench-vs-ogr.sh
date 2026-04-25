#!/bin/bash
# PostGIS bulk write の shpx vs ogr2ogr 比較ハーネス。完了基準は ROADMAP の
# 「shpx が ogr2ogr の 50% 以上の速度」(shpx_secs <= 2.0 * ogr_secs)。
# 詳細は docs/POSTGIS.md の Benchmark 節を参照。
#
# 必要コマンド: cargo / ogr2ogr (GDAL 3.7+ Parquet driver 同梱) / psql
#
# bash 必須 (関数内の `local` を使うため)。

set -eu

usage() {
    cat <<'USAGE'
Usage: bench-vs-ogr.sh [--rows N] [--runs M]

Options:
  --rows N    Bench input row count. Default: 10000000
  --runs M    Number of repetitions per tool. Default: 3 (median wins)
  -h, --help  Show this help

Environment:
  SHPX_TEST_PG_URL  Required. e.g. pg://shpx:shpx@localhost:5432/shpx_test
  SHPX_BIN          shpx binary path. Default: cargo run -p shpx-cli --release --
  OGR2OGR_BIN       ogr2ogr binary. Default: ogr2ogr
USAGE
}

ROWS=10000000
RUNS=3
while [ $# -gt 0 ]; do
    case "$1" in
        --rows) ROWS="$2"; shift 2 ;;
        --runs) RUNS="$2"; shift 2 ;;
        -h|--help) usage; exit 0 ;;
        *) echo "unknown arg: $1" >&2; usage >&2; exit 2 ;;
    esac
done

: "${SHPX_TEST_PG_URL:?SHPX_TEST_PG_URL must be set (e.g. pg://shpx:shpx@localhost:5432/shpx_test)}"
SHPX_BIN_DEFAULT="cargo run -q -p shpx-cli --release --"
SHPX_BIN="${SHPX_BIN:-$SHPX_BIN_DEFAULT}"
OGR2OGR_BIN="${OGR2OGR_BIN:-ogr2ogr}"

# Parquet driver の有無を最初に確認（無ければ早期失敗）。
if ! "$OGR2OGR_BIN" --formats 2>/dev/null | grep -qi "Parquet"; then
    echo "error: $OGR2OGR_BIN does not support Parquet (GDAL 3.7+ required)" >&2
    exit 1
fi

# bench データを ensure する（criterion bench を 1 回流すことで gen.rs が走る）。
cd "$(dirname "$0")/.."
WORKSPACE="$(pwd)"
BENCH_DATA_DIR="$WORKSPACE/target/bench-data"
INPUT="$BENCH_DATA_DIR/points_${ROWS}.parquet"

if [ ! -f "$INPUT" ]; then
    echo "==> generating $INPUT (rows=$ROWS) via cargo bench --no-run prep ..."
    # criterion harness 内 ensure_parquet を呼ぶための短絡として、bench を sample_size 1 で走らせる。
    # 既存生成済みでない場合のみ。
    SHPX_BENCH_ROWS="$ROWS" cargo bench -q -p shpx-driver-postgis --bench copy_binary -- \
        --quick --warm-up-time 1 --measurement-time 1 || true
fi

if [ ! -f "$INPUT" ]; then
    echo "error: bench input not generated: $INPUT" >&2
    exit 1
fi

# PG パラメタを bench 用に一時調整（compose 設定は汚染しない）。
PG_PSQL_ARGS="$SHPX_TEST_PG_URL"
psql_q() { psql "$PG_PSQL_ARGS" -v ON_ERROR_STOP=1 -At -c "$1" >/dev/null; }

echo "==> tuning PG bench parameters (synchronous_commit=off, full_page_writes=off)"
psql_q "ALTER SYSTEM SET synchronous_commit = off"
psql_q "ALTER SYSTEM SET full_page_writes = off"
psql_q "SELECT pg_reload_conf()"

# 終了時に必ずリセット（テーブル残骸も掃除）。
trap 'echo "==> resetting PG params"; \
    psql_q "ALTER SYSTEM RESET synchronous_commit" || true; \
    psql_q "ALTER SYSTEM RESET full_page_writes" || true; \
    psql_q "SELECT pg_reload_conf()" || true; \
    psql_q "DROP TABLE IF EXISTS public.bench_shpx" || true; \
    psql_q "DROP TABLE IF EXISTS public.bench_ogr"  || true' EXIT

run_shpx() {
    local table="$1"
    psql_q "DROP TABLE IF EXISTS public.$table"
    # /usr/bin/time -p は POSIX 時間出力（real / user / sys 各 1 行）
    /usr/bin/time -p sh -c "
        $SHPX_BIN convert \
            --insert-mode=bulk \
            --create-table=always \
            --create-index=auto \
            '$INPUT' \
            '${SHPX_TEST_PG_URL}?table=$table'
    " 2>&1 | awk '/^real/ {print $2}'
}

run_ogr() {
    local table="$1"
    psql_q "DROP TABLE IF EXISTS public.$table"
    /usr/bin/time -p sh -c "
        $OGR2OGR_BIN -f PostgreSQL 'PG:$SHPX_TEST_PG_URL' \
            '$INPUT' \
            -nln '$table' \
            -lco SPATIAL_INDEX=NONE \
            -lco PRECISION=NO \
            -lco GEOMETRY_NAME=geom \
            --config PG_USE_COPY YES \
            -overwrite
    " 2>&1 | awk '/^real/ {print $2}'
}

median() {
    # 標準入力の複数行（数値）の中央値を出す。
    sort -g | awk '{
        a[NR]=$1
    } END {
        if (NR == 0) { print "0"; exit }
        if (NR % 2 == 1) { print a[(NR+1)/2] }
        else             { printf "%.3f\n", (a[NR/2] + a[NR/2 + 1]) / 2 }
    }'
}

echo "==> rows=$ROWS, runs=$RUNS"

shpx_results=""
for i in $(seq 1 "$RUNS"); do
    t=$(run_shpx bench_shpx)
    echo "  shpx run $i: ${t}s"
    shpx_results="$shpx_results
$t"
done
shpx_med=$(printf '%s\n' "$shpx_results" | grep -v '^$' | median)

ogr_results=""
for i in $(seq 1 "$RUNS"); do
    t=$(run_ogr bench_ogr)
    echo "  ogr2ogr run $i: ${t}s"
    ogr_results="$ogr_results
$t"
done
ogr_med=$(printf '%s\n' "$ogr_results" | grep -v '^$' | median)

echo
echo "===== Benchmark results (median of $RUNS runs, rows=$ROWS) ====="
printf "  shpx     : %s s\n" "$shpx_med"
printf "  ogr2ogr  : %s s\n" "$ogr_med"
ratio=$(awk -v s="$shpx_med" -v o="$ogr_med" 'BEGIN { if (o == 0) { print "inf" } else { printf "%.3f", s / o } }')
printf "  shpx/ogr : %s  (must be <= 2.000 for v0.3 release)\n" "$ratio"
echo

if awk -v s="$shpx_med" -v o="$ogr_med" 'BEGIN { exit !(o > 0 && s <= 2.0 * o) }'; then
    echo "PASS: shpx is at least 50% as fast as ogr2ogr."
    exit 0
fi

echo "FAIL: shpx is slower than 50% of ogr2ogr — release blocked." >&2
exit 1
