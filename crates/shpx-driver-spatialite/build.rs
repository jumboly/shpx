//! `bundled-spatialite` feature 有効時に libspatialite を vendor から static link する build script。
//!
//! v0.6 cycle 1 時点のスコープ: GEOS / PROJ / RTTOPO / libxml2 / freexl / iconv / minizip /
//! geopackage を全て off (`OMIT_*` / `ENABLE_*` define で制御) にした最小構成。これにより
//! 純粋な geometry blob I/O (`GeomFromWKB` / `AsBinary`) と R*Tree (R*Tree は SQLite native)、
//! `InitSpatialMetadata(1)` の WGS84 seed のみが利用可能になる。GEOS は cycle 2、PROJ は
//! cycle 3 で追加予定。
//!
//! sqlite3ext.h / sqlite3.h の解決は libsqlite3-sys (rusqlite が依存) の `cargo:include`
//! メタデータ経由で `DEP_SQLITE3_INCLUDE` 環境変数から取得する。

#[cfg(not(feature = "bundled-spatialite"))]
fn main() {
    // feature 無効ビルドでは build.rs は何もしない。
    // `cargo:rerun-if-changed=build.rs` を出力しないことで、Cargo は build.rs 自体の
    // 変更でのみ再走する (vendor/ 以下を触っても rebuild されない)。
}

#[cfg(feature = "bundled-spatialite")]
fn main() {
    use std::path::PathBuf;

    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let vendor_dir = manifest_dir.join("vendor").join("libspatialite-5.1.0");
    let src_dir = vendor_dir.join("src");

    // libsqlite3-sys (links = "sqlite3") から伝搬される include path。
    // bundled feature 有効時は sqlite3.h / sqlite3ext.h がここで見つかる。
    let sqlite_include = std::env::var("DEP_SQLITE3_INCLUDE")
        .expect("DEP_SQLITE3_INCLUDE not set; libsqlite3-sys with `bundled` must be a direct dep");

    // OUT_DIR に `config.h` と `gaiaconfig.h` を生成する。libspatialite の C ソースは
    // `#include "config.h"` および `#include <spatialite/gaiaconfig.h>` を期待しており、
    // ここで cycle 1 の最小 OMIT 構成を反映する。
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    write_config_h(&out_dir);
    write_gaiaconfig_h(&out_dir);

    let mut build = cc::Build::new();
    build
        // OUT_DIR を最優先にして、shpx 生成の config.h / gaiaconfig.h で
        // 上流の `vendor/.../headers/spatialite/gaiaconfig.h` (ENABLE_RTTOPO=1 等が
        // 上流配布のままベイクインされている) を上書きする。
        .include(&out_dir)
        .include(&sqlite_include)
        .include(src_dir.join("headers"))
        // 非 loadable な ordinary lib モードでビルドする (sqlite3.h を直接 include)。
        // static link 時は `sqlite3_modspatialite_init` (LOADABLE_EXTENSION 経路、
        // sqlite3_api_routines 経由) を使うと `pApi` が NULL になり SIGSEGV するため、
        // `spatialite_init_ex(db, cache, verbose)` 経路を使う。
        // VERSION マクロは autotools の AC_INIT で埋められるが、cc 経由では渡されない。
        .define("VERSION", "\"5.1.0\"")
        // cycle 1 の最小構成: GEOS / PROJ / iconv / freexl / mathsql / EPSG-full / KNN を OMIT、
        // RTTOPO / libxml2 / minizip / geopackage / GCP は ENABLE しない。
        .define("OMIT_GEOS", None)
        .define("OMIT_PROJ", None)
        .define("OMIT_ICONV", None)
        .define("OMIT_FREEXL", None)
        .define("OMIT_MATHSQL", None)
        .define("OMIT_EPSG", None)
        .define("OMIT_KNN", None)
        .define("OMIT_GEOCALLBACKS", None)
        // 巨大 source の警告抑制 (libspatialite の C スタイルは pedantic 設定だと警告祭り)
        .flag_if_supported("-Wno-unused-parameter")
        .flag_if_supported("-Wno-unused-variable")
        .flag_if_supported("-Wno-unused-function")
        .flag_if_supported("-Wno-unused-but-set-variable")
        .flag_if_supported("-Wno-sign-compare")
        .flag_if_supported("-Wno-implicit-function-declaration")
        .flag_if_supported("-Wno-deprecated-declarations")
        .flag_if_supported("-Wno-format")
        .flag_if_supported("-Wno-pointer-sign")
        .warnings(false);

    // 各サブディレクトリから .c ファイルを集める。
    // cycle 1 では GEOS/PROJ/iconv 依存の機能 (shapefiles/dxf/wfs/topology/geopackage/cutter/
    // control_points/stored_procedures/virtualtext/gaiaexif) は OMIT_* で stub 化される
    // 想定で、コンパイル対象には含めない (header もリンクも要求しないようにする)。
    //
    // EXCLUDE: lemon/flex 由来の parser/lexer 出力 (`Ewkt.c` / `Gml.c` / `Kml.c` /
    // `geoJSON.c` / `vanuatuWkt.c` / `lex.*.c`) は wrapper の `gg_*.c` から `#include` される
    // データファイル (Makefile.am の EXTRA_DIST) であり、直接コンパイルしてはいけない。
    let exclude_files: &[&str] = &[
        "Ewkt.c",
        "geoJSON.c",
        "Gml.c",
        "Kml.c",
        "vanuatuWkt.c",
        "lex.Ewkt.c",
        "lex.GeoJson.c",
        "lex.Gml.c",
        "lex.Kml.c",
        "lex.VanuatuWkt.c",
    ];
    // 追加すべきサブディレクトリ: spatialite.c (extension entry) は dxf / stored_procedures /
    // gaiaexif / cutter / control_points / shapefiles / virtualtext / topology / wfs /
    // geopackage の関数を unconditionally に参照する (`-DOMIT_GEOS` 等で stub 化されない)。
    // これらを link tree から落とすには spatialite.c 側のコード削除が必要だが、cycle 1 では
    // 全部コンパイル対象に含める方針。GEOS / PROJ / RTTOPO / iconv 必要箇所はファイル内の
    // `#ifdef ENABLE_GEOS` 等で stub 化されることに依存する。
    for sub in [
        "spatialite",
        "gaiageo",
        "gaiaaux",
        "gaiaexif",
        "srsinit",
        "connection_cache",
        "versioninfo",
        "md5",
        "shapefiles",
        "dxf",
        "stored_procedures",
        "cutter",
        "control_points",
        "virtualtext",
        "topology",
        "wfs",
        "geopackage",
    ] {
        for entry in std::fs::read_dir(src_dir.join(sub))
            .unwrap_or_else(|e| panic!("read_dir vendor/.../src/{sub} failed: {e}"))
        {
            let entry = entry.unwrap();
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("c") {
                continue;
            }
            let fname = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if exclude_files.contains(&fname) {
                continue;
            }
            build.file(&path);
            println!("cargo:rerun-if-changed={}", path.display());
        }
    }

    // vendor/ 直下のメタデータ変更でも rebuild。
    println!("cargo:rerun-if-changed=build.rs");
    println!(
        "cargo:rerun-if-changed={}",
        manifest_dir.join("vendor/SHA256SUMS").display()
    );

    build.compile("spatialite_bundled");
}

#[cfg(feature = "bundled-spatialite")]
fn write_config_h(out_dir: &std::path::Path) {
    // libspatialite の `config.h` (autotools `configure` が生成するもの) の代替。
    // OS チェックや HAVE_* マクロは shpx の cycle 1 で必要な最小限のみ。
    // sqlite3 / 標準 C / POSIX (Linux/macOS) を前提とする。
    let body = r"/* shpx v0.6 cycle 1: minimal config.h replacement for libspatialite-5.1.0
 * (autotools configure を使わず、cc-rs から define を渡す方針)
 */
#ifndef SPATIALITE_BUNDLED_CONFIG_H
#define SPATIALITE_BUNDLED_CONFIG_H

#define HAVE_DLFCN_H 1
#define HAVE_FCNTL_H 1
#define HAVE_FLOAT_H 1
#define HAVE_INTTYPES_H 1
#define HAVE_LIMITS_H 1
#define HAVE_LOCALE_H 1
#define HAVE_MATH_H 1
#define HAVE_MEMORY_H 1
#define HAVE_STDINT_H 1
#define HAVE_STDIO_H 1
#define HAVE_STDLIB_H 1
#define HAVE_STRINGS_H 1
#define HAVE_STRING_H 1
#define HAVE_SYS_STAT_H 1
#define HAVE_SYS_TYPES_H 1
#define HAVE_UNISTD_H 1
#define HAVE_SQLITE3EXT_H 1
#define HAVE_SQLITE3_H 1

#define HAVE_FDATASYNC 1
#define HAVE_FTRUNCATE 1
#define HAVE_GETCWD 1
#define HAVE_GETTIMEOFDAY 1
#define HAVE_LOCALTIME_R 1
#define HAVE_MEMMOVE 1
#define HAVE_MEMSET 1
#define HAVE_STRCASECMP 1
#define HAVE_STRERROR 1

#define HAVE_DECL_SQLITE_INDEX_CONSTRAINT_LIKE 1

#define _LARGEFILE_SOURCE 1
#define NDEBUG 1

#endif
";
    std::fs::write(out_dir.join("config.h"), body).expect("write config.h");
    // libspatialite の一部ファイルは config-msvc.h を参照する。non-MSVC では空で十分。
    std::fs::write(
        out_dir.join("config-msvc.h"),
        "/* shpx: non-MSVC build, config-msvc.h intentionally empty */\n",
    )
    .expect("write config-msvc.h");
}

#[cfg(feature = "bundled-spatialite")]
fn write_gaiaconfig_h(out_dir: &std::path::Path) {
    // `<spatialite/gaiaconfig.h>` は public header に含まれる ABI 制御マクロ。
    // libspatialite の inplace ヘッダを上書きするため、include path 上の優先度を
    // OUT_DIR > vendor/.../headers にしておく必要がある (cc::Build.include() の順序で担保)。
    let dir = out_dir.join("spatialite");
    std::fs::create_dir_all(&dir).expect("create OUT_DIR/spatialite");
    // `"`を含むため raw-string-with-hashes が必要 (clippy::needless_raw_string_hashes
    // は誤検出するため allow)。
    #[allow(clippy::needless_raw_string_hashes)]
    let body = r#"/* shpx v0.6 cycle 1: gaiaconfig.h with all GEOS/PROJ/RTTOPO/etc disabled.
 * ABI 互換のため public macro はすべて #undef または #define で明示する。
 */
#ifndef GAIACONFIG_H_BUNDLED
#define GAIACONFIG_H_BUNDLED

#undef ENABLE_GCP
#undef ENABLE_GEOPACKAGE
#undef ENABLE_LIBXML2
#undef ENABLE_MINIZIP
#undef ENABLE_RTTOPO
#undef GEOS_370
#undef GEOS_3100
#undef GEOS_3110
#undef GEOS_ADVANCED
#undef GEOS_ONLY_REENTRANT
#undef GEOS_REENTRANT
#define OMIT_EPSG 1
#define OMIT_FREEXL 1
#define OMIT_GEOCALLBACKS 1
#define OMIT_GEOS 1
#define OMIT_ICONV 1
#define OMIT_KNN 1
#define OMIT_MATHSQL 1
#define OMIT_PROJ 1
#undef PROJ_NEW
#define SPATIALITE_TARGET_CPU "shpx-bundled"
#define SPATIALITE_VERSION "5.1.0"

#endif
"#;
    std::fs::write(dir.join("gaiaconfig.h"), body).expect("write gaiaconfig.h");
    std::fs::write(
        dir.join("gaiaconfig-msvc.h"),
        "/* shpx: non-MSVC build, gaiaconfig-msvc.h intentionally empty */\n",
    )
    .expect("write gaiaconfig-msvc.h");
}
