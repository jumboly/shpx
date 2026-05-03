#!/bin/bash
# shpx vs ogr2ogr の SQL Server staging bulk write 比較ハーネス。
# CI 計測は .github/workflows/bench-smoke-mssql.yml に移行済みで、
# 本スクリプトは ogr2ogr との挙動差をローカルで確認したい開発者向け。
# 詳細は docs/SQLSERVER.md の Benchmark 節を参照。
#
# 必要コマンド: cargo / ogr2ogr (GDAL の MSSQLSpatial driver 同梱) / sqlcmd
#
# bash 必須 (関数内の `local` を使うため)。

set -euo pipefail

usage() {
    cat <<'USAGE'
Usage: bench-vs-ogr-mssql.sh [--rows N] [--runs M]

Options:
  --rows N    Bench input row count. Default: 10000000
  --runs M    Number of repetitions per tool. Default: 3 (median wins)
  -h, --help  Show this help

Environment:
  SHPX_TEST_SQLSERVER_URL  Required. e.g. mssql://sa:Shpx_test_pw1!@localhost:1433/shpx_test
  SHPX_BIN                 shpx binary path. Default: target/release/shpx
  OGR2OGR_BIN              ogr2ogr binary. Default: ogr2ogr
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

: "${SHPX_TEST_SQLSERVER_URL:?SHPX_TEST_SQLSERVER_URL must be set (e.g. mssql://sa:Shpx_test_pw1!@localhost:1433/shpx_test)}"
OGR2OGR_BIN="${OGR2OGR_BIN:-ogr2ogr}"

# `mssql://user:pass@host:port/db` から OGR の MSSQLSpatial driver 用の ado.net 形式
# `MSSQL:server=host,port;database=db;uid=user;pwd=pass;TrustServerCertificate=yes`
# に書き換える。tiberius と OGR で接続文字列の形が完全に違うため。
parse_mssql_url_to_ogr() {
    local url="$1"
    # mssql:// を剥がす
    local rest="${url#mssql://}"
    local userinfo authority
    if [[ "$rest" == *@* ]]; then
        userinfo="${rest%@*}"
        authority="${rest##*@}"
    else
        echo "error: SHPX_TEST_SQLSERVER_URL must include user:pass@host" >&2
        exit 1
    fi
    local user pass
    if [[ "$userinfo" == *:* ]]; then
        user="${userinfo%%:*}"
        pass="${userinfo#*:}"
    else
        user="$userinfo"; pass=""
    fi
    # authority = host[:port]/db?...
    local hostport_db="${authority%%\?*}"
    local host_port="${hostport_db%%/*}"
    local db="${hostport_db#*/}"
    local host port
    if [[ "$host_port" == *:* ]]; then
        host="${host_port%%:*}"
        port="${host_port#*:}"
    else
        host="$host_port"; port="1433"
    fi
    echo "MSSQL:server=${host},${port};database=${db};uid=${user};pwd=${pass};TrustServerCertificate=yes"
}

# Parquet driver の有無を確認。
if ! "$OGR2OGR_BIN" --formats 2>/dev/null | grep -qi "Parquet"; then
    echo "error: $OGR2OGR_BIN does not support Parquet (GDAL 3.7+ required)" >&2
    exit 1
fi
if ! "$OGR2OGR_BIN" --formats 2>/dev/null | grep -qi "MSSQLSpatial"; then
    echo "error: $OGR2OGR_BIN does not support MSSQLSpatial driver" >&2
    exit 1
fi

cd "$(dirname "$0")/.."
WORKSPACE="$(pwd)"
BENCH_DATA_DIR="$WORKSPACE/target/bench-data"
INPUT="$BENCH_DATA_DIR/points_mssql_${ROWS}.parquet"

if [ -z "${SHPX_BIN:-}" ]; then
    SHPX_BIN_PATH="$WORKSPACE/target/release/shpx"
    if [ ! -x "$SHPX_BIN_PATH" ]; then
        echo "==> building shpx (release) ..."
        cargo build --release -p shpx-cli >&2
    fi
    SHPX_BIN="$SHPX_BIN_PATH"
fi

# bench input parquet が無ければ criterion harness の `ensure_parquet` を呼ぶ。
if [ ! -f "$INPUT" ]; then
    echo "==> generating $INPUT (rows=$ROWS) via cargo bench prep ..."
    SHPX_BENCH_ROWS="$ROWS" cargo bench -q -p shpx-driver-sqlserver --bench bulk_insert -- \
        --quick --warm-up-time 1 --measurement-time 1 || true
fi

if [ ! -f "$INPUT" ]; then
    echo "error: bench input not generated: $INPUT" >&2
    exit 1
fi

OGR_MSSQL_URL=$(parse_mssql_url_to_ogr "$SHPX_TEST_SQLSERVER_URL")

# テーブル削除には sqlcmd が必要。host にあれば使う、無ければ docker exec 経由。
SQLCMD_HOST=""
SQLCMD_USER=""
SQLCMD_PASS=""
SQLCMD_DB=""
{
    # 接続情報を URL から抽出
    rest="${SHPX_TEST_SQLSERVER_URL#mssql://}"
    userinfo="${rest%@*}"
    authority="${rest##*@}"
    SQLCMD_USER="${userinfo%%:*}"
    SQLCMD_PASS="${userinfo#*:}"
    hostport_db="${authority%%\?*}"
    host_port="${hostport_db%%/*}"
    SQLCMD_DB="${hostport_db#*/}"
    if [[ "$host_port" == *:* ]]; then
        SQLCMD_HOST="${host_port%%:*},${host_port#*:}"
    else
        SQLCMD_HOST="${host_port},1433"
    fi
}

if command -v sqlcmd >/dev/null 2>&1; then
    sqlcmd_q() {
        sqlcmd -S "$SQLCMD_HOST" -U "$SQLCMD_USER" -P "$SQLCMD_PASS" -d "$SQLCMD_DB" -C -b \
            -Q "$1" >/dev/null
    }
else
    SQLCMD_SERVICE="${SQLCMD_SERVICE:-mssql}"
    sqlcmd_q() {
        docker compose exec -T "$SQLCMD_SERVICE" \
            /opt/mssql-tools18/bin/sqlcmd \
            -S localhost -U "$SQLCMD_USER" -P "$SQLCMD_PASS" -d "$SQLCMD_DB" -C -b \
            -Q "$1" >/dev/null
    }
fi

# 終了時にテーブル残骸を掃除。
trap 'sqlcmd_q "IF OBJECT_ID(N'"'"'[dbo].[bench_shpx]'"'"', '"'"'U'"'"') IS NOT NULL DROP TABLE [dbo].[bench_shpx]" || true; \
      sqlcmd_q "IF OBJECT_ID(N'"'"'[dbo].[bench_ogr]'"'"', '"'"'U'"'"') IS NOT NULL DROP TABLE [dbo].[bench_ogr]"  || true' EXIT

run_tool() {
    local label="$1"; shift
    local out rc
    out=$(/usr/bin/time -p "$@" 2>&1) || rc=$?
    rc=${rc:-0}
    if [ "$rc" -ne 0 ]; then
        echo "$out" >&2
        echo "ERROR: $label exited with $rc" >&2
        return "$rc"
    fi
    echo "$out" | awk '/^real/ {print $2}'
}

run_shpx() {
    local table="$1"
    sqlcmd_q "IF OBJECT_ID(N'[dbo].[$table]', 'U') IS NOT NULL DROP TABLE [dbo].[$table]"
    SHPX_MSSQL_BULK_CHUNK=1000000 run_tool "shpx" "$SHPX_BIN" convert \
        --insert-mode=bulk \
        --create-table=always \
        --create-index=auto \
        "$INPUT" \
        "${SHPX_TEST_SQLSERVER_URL}?table=$table"
}

run_ogr() {
    local table="$1"
    sqlcmd_q "IF OBJECT_ID(N'[dbo].[$table]', 'U') IS NOT NULL DROP TABLE [dbo].[$table]"
    run_tool "ogr2ogr" "$OGR2OGR_BIN" \
        -f MSSQLSpatial "$OGR_MSSQL_URL" \
        "$INPUT" \
        -nln "$table" \
        -lco SCHEMA=dbo \
        -lco SPATIAL_INDEX=NO \
        -lco GEOMETRY_NAME=geom \
        -overwrite
}

median() {
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
printf "  shpx/ogr : %s  (must be <= 1.667 for v0.4 release)\n" "$ratio"
echo

if awk -v s="$shpx_med" -v o="$ogr_med" 'BEGIN { exit !(o > 0 && s <= 1.667 * o) }'; then
    echo "PASS: shpx is at least 60% as fast as ogr2ogr."
    exit 0
fi

echo "FAIL: shpx is slower than 60% of ogr2ogr — release blocked." >&2
exit 1
