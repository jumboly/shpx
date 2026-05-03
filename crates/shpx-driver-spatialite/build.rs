//! `bundled-spatialite` feature 有効時に libspatialite を vendor から static link する build script。
//!
//! v0.6 cycle 2 時点のスコープ: GEOS は `geos-src` crate で同梱 (ON)。
//! PROJ / RTTOPO / libxml2 / freexl / iconv / minizip / geopackage は OFF のまま。
//! GeomFromWKB / AsBinary / R*Tree に加え、`ST_Buffer` 等の GEOS 依存関数も使える。
//!
//! sqlite3.h / sqlite3ext.h は libsqlite3-sys (`links = "sqlite3"`) が伝搬する
//! `DEP_SQLITE3_INCLUDE` 経由で解決する。
//! geos_c.h は `geos-src` 0.2.x が `DEP_GEOSSRC_*` を出さないため、本 build.rs から
//! sibling の `target/<profile>/build/geos-src-<hash>/out/{include,lib}` を直接探す
//! (`locate_geos_root` 参照)。

#[cfg(not(feature = "bundled-spatialite"))]
fn main() {
    // build.rs 自体の変更でのみ rerun させ、`vendor/` 以下の編集を default ビルドの
    // rebuild トリガーから外す (bundled OFF では vendor は使わない)。
    println!("cargo:rerun-if-changed=build.rs");
}

#[cfg(feature = "bundled-spatialite")]
fn main() {
    use std::path::PathBuf;

    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let src_dir = manifest_dir.join("vendor/libspatialite-5.1.0/src");
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());

    let sqlite_include = std::env::var("DEP_SQLITE3_INCLUDE")
        .expect("DEP_SQLITE3_INCLUDE not set; libsqlite3-sys with `bundled` must be a direct dep");
    // geos-src 0.2.x は include path を export せず、`cargo:lib=` / `cargo:search=`
    // も build script を実行した crate 自身にしか効かない。`locate_geos_root` で
    // sibling の OUT_DIR を解決し、include を `cc::Build` に feed、lib は消費側で
    // 改めて `cargo:rustc-link-*` する。
    let geos_root = locate_geos_root(&out_dir);
    println!(
        "cargo:rustc-link-search=native={}",
        geos_root.join("lib").display()
    );
    // 順序が重要: libgeos_c は libgeos に依存。
    println!("cargo:rustc-link-lib=static=geos_c");
    println!("cargo:rustc-link-lib=static=geos");
    // `gg_relations.c::evalGeosCache` が zlib の `crc32` を使う。OS 同梱の libz を動的リンク。
    println!("cargo:rustc-link-lib=z");

    write_generated_headers(&out_dir);

    let mut build = cc::Build::new();
    build
        // OUT_DIR を最優先にして、shpx 生成の gaiaconfig.h で上流の
        // `vendor/.../headers/spatialite/gaiaconfig.h` (上流の build configuration が
        // baked in されていて ENABLE_RTTOPO=1 等になっている) を上書きする。
        .include(&out_dir)
        .include(&sqlite_include)
        .include(geos_root.join("include"))
        .include(src_dir.join("headers"))
        .define("VERSION", "\"5.1.0\"")
        .warnings(false);
    for omit in OMIT_FEATURES {
        build.define(&format!("OMIT_{omit}"), None);
    }
    for flag in [
        "-Wno-unused-parameter",
        "-Wno-unused-variable",
        "-Wno-unused-function",
        "-Wno-unused-but-set-variable",
        "-Wno-sign-compare",
        "-Wno-implicit-function-declaration",
        "-Wno-deprecated-declarations",
        "-Wno-format",
        "-Wno-pointer-sign",
    ] {
        build.flag_if_supported(flag);
    }

    // spatialite.c (extension entry) は dxf / stored_procedures / gaiaexif / cutter /
    // control_points / shapefiles / virtualtext / topology / wfs / geopackage の関数を
    // unconditional に参照するため、OMIT したくても link tree からは落とせない。これらの
    // ファイル内 GEOS/PROJ 依存箇所は OMIT_* で stub 化される前提で全部含める。
    let subdirs: &[&str] = &[
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
    ];
    for sub in subdirs {
        let dir = src_dir.join(sub);
        for entry in std::fs::read_dir(&dir)
            .unwrap_or_else(|e| panic!("read_dir {} failed: {e}", dir.display()))
        {
            let path = entry.unwrap().path();
            if path.extension().and_then(|e| e.to_str()) != Some("c") {
                continue;
            }
            let fname = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if EXCLUDED_C_FILES.contains(&fname) {
                continue;
            }
            build.file(&path);
        }
        // ファイル単位の rerun-if-changed は 100+ 件になり Cargo の stat オーバーヘッドが
        // 馬鹿にならないので、サブディレクトリ単位で発行する (Cargo は dir mtime も見るため
        // 内部の .c/.h 追加・削除・編集も検出される)。
        println!("cargo:rerun-if-changed={}", dir.display());
    }
    println!(
        "cargo:rerun-if-changed={}",
        src_dir.join("headers").display()
    );
    println!("cargo:rerun-if-changed=build.rs");
    println!(
        "cargo:rerun-if-changed={}",
        manifest_dir.join("vendor/SHA256SUMS").display()
    );

    build.compile("spatialite_bundled");
}

/// `target/<profile>/build/geos-src-<hash>/out` (= cmake install prefix) を探す。
///
/// 自分の OUT_DIR は `target/<profile>/build/shpx-driver-spatialite-<hash>/out` 形式。
/// 親 (`build/`) を読んで `geos-src-` で始まり、配下に `out/include/geos_c.h` と
/// `out/lib/libgeos_c.a` (Linux/macOS の cmake install) を持つディレクトリを返す。
/// 複数バージョンが残っていた場合は mtime 最新を採用。
#[cfg(feature = "bundled-spatialite")]
fn locate_geos_root(out_dir: &std::path::Path) -> std::path::PathBuf {
    let build_root = out_dir
        .parent()
        .and_then(std::path::Path::parent)
        .expect("OUT_DIR has no <build_root> ancestor");

    let mut best: Option<(std::time::SystemTime, std::path::PathBuf)> = None;
    let entries = std::fs::read_dir(build_root)
        .unwrap_or_else(|e| panic!("read_dir {} failed: {e}", build_root.display()));
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with("geos-src-") {
            continue;
        }
        let root = entry.path().join("out");
        if !root.join("include/geos_c.h").exists() {
            continue;
        }
        let mtime = entry
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(std::time::UNIX_EPOCH);
        if best.as_ref().is_none_or(|(t, _)| mtime > *t) {
            best = Some((mtime, root));
        }
    }
    best.unwrap_or_else(|| {
        panic!(
            "could not locate geos-src OUT_DIR (expected {}/geos-src-*/out/include/geos_c.h). \
             Make sure `geos-src` is declared as a build-dependency.",
            build_root.display()
        )
    })
    .1
}

/// libspatialite の OMIT_* スイッチ。`build.rs` から `cc::Build.define` する側と
/// `OUT_DIR/spatialite/gaiaconfig.h` の `#define` 側で共有することで、整合性ズレを防ぐ。
/// v0.6 cycle 2 で `OMIT_GEOS` を解除 (`geos-src` 同梱)、PROJ は cycle 3 で解除予定。
#[cfg(feature = "bundled-spatialite")]
const OMIT_FEATURES: &[&str] = &[
    "PROJ",
    "ICONV",
    "FREEXL",
    "MATHSQL",
    "EPSG",
    "KNN",
    "GEOCALLBACKS",
];

/// lemon/flex 由来の parser/lexer 出力。wrapper の `gg_*.c` が `#include` する
/// データファイル (`Makefile.am` の EXTRA_DIST) であり、直接 compile してはいけない。
#[cfg(feature = "bundled-spatialite")]
const EXCLUDED_C_FILES: &[&str] = &[
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

#[cfg(feature = "bundled-spatialite")]
fn write_generated_headers(out_dir: &std::path::Path) {
    let spatialite_dir = out_dir.join("spatialite");
    std::fs::create_dir_all(&spatialite_dir).expect("create OUT_DIR/spatialite");
    write_if_changed(&out_dir.join("config.h"), &config_h_body());
    write_if_changed(&out_dir.join("config-msvc.h"), MSVC_STUB);
    write_if_changed(&spatialite_dir.join("gaiaconfig.h"), &gaiaconfig_h_body());
    write_if_changed(&spatialite_dir.join("gaiaconfig-msvc.h"), MSVC_STUB);
}

#[cfg(feature = "bundled-spatialite")]
const MSVC_STUB: &str = "/* shpx: non-MSVC build, intentionally empty */\n";

/// 内容差分があるときだけ書き込む。毎ビルド `fs::write` で mtime を更新すると、
/// cc-rs の incremental 判定 (timestamp 比較) と相性が悪くなるため。
#[cfg(feature = "bundled-spatialite")]
fn write_if_changed(path: &std::path::Path, body: &str) {
    if std::fs::read_to_string(path).map(|cur| cur == body).unwrap_or(false) {
        return;
    }
    std::fs::write(path, body).unwrap_or_else(|e| panic!("write {} failed: {e}", path.display()));
}

#[cfg(feature = "bundled-spatialite")]
fn config_h_body() -> String {
    // libspatialite の `config.h` (autotools の configure 出力) の代替。
    // Linux / macOS の POSIX + 標準 C + sqlite3 を前提とした最小集合のみ宣言する。
    r"#ifndef SPATIALITE_BUNDLED_CONFIG_H
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
"
    .to_string()
}

#[cfg(feature = "bundled-spatialite")]
fn gaiaconfig_h_body() -> String {
    // 上流 `vendor/.../headers/spatialite/gaiaconfig.h` を OUT_DIR で上書きするための公開 API
    // 制御マクロ。OMIT_* リストは `cc::Build.define` 側と共有 (両者がズレると preprocess 結果が
    // ファイルごとに分裂する)。`#undef ENABLE_*` 群は上流 baked-in の有効化を打ち消す。
    let mut body = String::from(
        "#ifndef GAIACONFIG_H_BUNDLED\n#define GAIACONFIG_H_BUNDLED\n\n\
         #undef ENABLE_GCP\n#undef ENABLE_GEOPACKAGE\n#undef ENABLE_LIBXML2\n\
         #undef ENABLE_MINIZIP\n#undef ENABLE_RTTOPO\n\
         #undef GEOS_370\n#undef GEOS_3100\n#undef GEOS_3110\n\
         #undef GEOS_ADVANCED\n#undef GEOS_ONLY_REENTRANT\n#undef GEOS_REENTRANT\n\
         #undef PROJ_NEW\n\n",
    );
    for omit in OMIT_FEATURES {
        use std::fmt::Write;
        writeln!(body, "#define OMIT_{omit} 1").unwrap();
    }
    body.push_str(
        "\n#define SPATIALITE_TARGET_CPU \"shpx-bundled\"\n\
         #define SPATIALITE_VERSION \"5.1.0\"\n\n#endif\n",
    );
    body
}
