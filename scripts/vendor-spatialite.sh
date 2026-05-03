#!/bin/bash
# libspatialite を上流 tarball から `crates/shpx-driver-spatialite/vendor/` に再生成する。
# `vendor/SHA256SUMS` の SHA256 で再現性を固定し、shpx の build.rs が必要としない
# 上流ファイル (autotools 生成物 / EPSG seed 大半 / test / examples / 上流 maintainer
# 用 code generator) を除外する。
#
# 実行後の vendor は git diff がゼロになるはず (= upstream + 文書化された除外リスト
# と bit-identical)。除外リストの根拠は `crates/shpx-driver-spatialite/NOTICE` と
# 本スクリプト内 EXCLUDE_* 配列を参照。
#
# 必要コマンド: curl / tar / shasum (macOS) または sha256sum (Linux)

set -euo pipefail

VERSION="${1:-5.1.0}"
TARBALL="libspatialite-${VERSION}.tar.gz"
UPSTREAM_URL="https://www.gaia-gis.it/gaia-sins/libspatialite-sources/${TARBALL}"

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
VENDOR_DIR="${REPO_ROOT}/crates/shpx-driver-spatialite/vendor"
SHA256SUMS="${VENDOR_DIR}/SHA256SUMS"
CACHE_DIR="${REPO_ROOT}/target/vendor-cache"
STAGING="${CACHE_DIR}/libspatialite-${VERSION}"

# ルート直下で残す項目。これ以外はすべて削除 (Android_*.mk / autotools / examples /
# test / nmake / .vc / Doxyfile / pkgconfig.in などの build system / 配布アセット)。
ROOT_KEEP=(AUTHORS configure.ac COPYING README src)

# src/ 配下から削除するサブツリー (上流 maintainer のみが使う code generator)。
SRC_REMOVE_SUBTREES=(
    srsinit/epsg_update
    connection_cache/generator
    gaiageo/lemon
    gaiageo/flex
)

# src/srsinit/epsg_inlined_*.c は OMIT_EPSG 時 12MB のデッドコードになる。
# WGS84 seed (epsg_inlined_wgs84_*.c) と dispatcher (epsg_inlined_extra.c) のみ残す。
EPSG_KEEP_REGEX='^epsg_inlined_(wgs84_[0-9]+|extra)\.c$'

sha256_of() {
    if command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$1" | awk '{print $1}'
    else
        sha256sum "$1" | awk '{print $1}'
    fi
}

expected_sha256() {
    grep "  ${TARBALL}\$" "${SHA256SUMS}" | awk '{print $1}'
}

main() {
    local expected_hash
    expected_hash="$(expected_sha256)"
    if [[ -z "${expected_hash}" ]]; then
        echo "ERROR: ${TARBALL} SHA256 not found in ${SHA256SUMS}" >&2
        exit 1
    fi

    mkdir -p "${CACHE_DIR}"
    local cached_tarball="${CACHE_DIR}/${TARBALL}"

    if [[ ! -f "${cached_tarball}" ]]; then
        echo "==> downloading ${UPSTREAM_URL}"
        curl -fL --progress-bar -o "${cached_tarball}.tmp" "${UPSTREAM_URL}"
        mv "${cached_tarball}.tmp" "${cached_tarball}"
    else
        echo "==> using cached ${cached_tarball}"
    fi

    local actual_hash
    actual_hash="$(sha256_of "${cached_tarball}")"
    if [[ "${actual_hash}" != "${expected_hash}" ]]; then
        echo "ERROR: SHA256 mismatch for ${TARBALL}" >&2
        echo "  expected: ${expected_hash}" >&2
        echo "  actual:   ${actual_hash}" >&2
        echo "  (delete ${cached_tarball} to force re-download)" >&2
        exit 1
    fi
    echo "==> SHA256 verified: ${expected_hash}"

    rm -rf "${STAGING}"
    echo "==> extracting to ${STAGING}"
    tar xzf "${cached_tarball}" -C "${CACHE_DIR}"

    echo "==> applying exclusion list"
    apply_exclusions "${STAGING}"

    local final_dir="${VENDOR_DIR}/libspatialite-${VERSION}"
    rm -rf "${final_dir}"
    mv "${STAGING}" "${final_dir}"

    echo "==> done: ${final_dir}"
    echo "    Verify with: git diff -- crates/shpx-driver-spatialite/vendor/"
}

apply_exclusions() {
    local root="$1"

    # ルート直下: KEEP リストに無いものはすべて削除。
    local entry name keep
    for entry in "${root}"/*; do
        name="$(basename "${entry}")"
        keep=0
        for k in "${ROOT_KEEP[@]}"; do
            if [[ "${name}" == "${k}" ]]; then
                keep=1
                break
            fi
        done
        if [[ ${keep} -eq 0 ]]; then
            rm -rf "${entry}"
        fi
    done

    # src/ サブツリーの削除。
    local sub
    for sub in "${SRC_REMOVE_SUBTREES[@]}"; do
        rm -rf "${root}/src/${sub}"
    done

    # src/srsinit/epsg_inlined_*.c のうち KEEP regex に一致しないものを削除。
    local f base
    for f in "${root}"/src/srsinit/epsg_inlined_*.c; do
        [[ -f "${f}" ]] || continue
        base="$(basename "${f}")"
        if ! [[ "${base}" =~ ${EPSG_KEEP_REGEX} ]]; then
            rm -f "${f}"
        fi
    done
}

main "$@"
