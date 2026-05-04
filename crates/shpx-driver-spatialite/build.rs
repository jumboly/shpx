//! `bundled-spatialite` feature 有効時に libspatialite を vendor から static link する build script。
//!
//! v0.6 cycle 3 時点のスコープ: GEOS は `geos-src` crate で同梱 (ON、cycle 2)、
//! PROJ は shpx-geom の `bundled-proj` 経由 proj-sys 0.25.0 (libproj 9.4.x) で同梱 (ON、cycle 3)。
//! RTTOPO / libxml2 / freexl / iconv / minizip / geopackage は OFF のまま。
//! GeomFromWKB / AsBinary / R*Tree に加え、`ST_Buffer` (GEOS) / `Transform` (PROJ) も使える。
//!
//! sqlite3.h / sqlite3ext.h は libsqlite3-sys (`links = "sqlite3"`) が伝搬する
//! `DEP_SQLITE3_INCLUDE` 経由で解決する。
//! geos_c.h / proj.h は `geos-src` 0.2.x / `proj-sys` 0.25.0 がいずれも `cargo:include=` /
//! `DEP_*` を出さないため、本 build.rs から sibling の
//! `target/<profile>/build/{geos-src,proj-sys}-<hash>/out/{include,lib}` を直接探す
//! (`locate_geos_root` / `locate_proj_root` 参照)。
//! link 命令は GEOS は本 build.rs が出すが、PROJ は proj-sys が `links = "proj"` を
//! 宣言しているため Cargo に link 順を任せる (二重 `cargo:rustc-link-lib=proj` を避ける)。

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
    // `gg_relations.c::evalGeosCache` が zlib の `crc32` を要求する。libz-sys の
    // `static` feature が vendored libz.a を build し search path (`out/lib`) を
    // 通すが、Rust 側で symbol を参照しないため link directive は自動伝搬しない。
    // 本 build.rs から明示的に `static=z` を発行する。Windows MSVC でも同経路で
    // vendored zlib を static link する (3 OS で同一バージョン pin)。
    println!("cargo:rustc-link-lib=static=z");

    // PROJ は proj-sys (`links = "proj"`) が `cargo:rustc-link-lib=proj` を出すため、
    // 本 build.rs からは link 命令を出さず Cargo に link 順解決を任せる。
    // include path は proj-sys 0.25.0 が `cargo:include=` を emit しないため、sibling の
    // OUT_DIR (`target/<profile>/build/proj-sys-<hash>/out/include/proj.h`) を直接探す。
    // 将来 proj-sys が `cargo:include=` を出すようになったら `DEP_PROJ_INCLUDE` env で代替する。
    let proj_root = locate_proj_root(&out_dir);

    write_generated_headers(&out_dir);

    let mut build = cc::Build::new();
    build
        // OUT_DIR を最優先にして、shpx 生成の gaiaconfig.h で上流の
        // `vendor/.../headers/spatialite/gaiaconfig.h` (上流の build configuration が
        // baked in されていて ENABLE_RTTOPO=1 等になっている) を上書きする。
        .include(&out_dir)
        .include(&sqlite_include)
        .include(geos_root.join("include"))
        .include(proj_root.join("include"))
        .include(src_dir.join("headers"))
        .define("VERSION", "\"5.1.0\"");
    // libz-sys (`links = "z"`) が `cargo:include=<path>` を emit するため、消費側の本
    // build.rs に `DEP_Z_INCLUDE` 環境変数で渡される。spatialite_private.h が `<zlib.h>`
    // を `#include` するために必要。Linux/macOS は system zlib も system include に居るが、
    // libz-sys vendored を優先することで 3 OS で同一 zlib バージョン (Cargo.lock pin) を保証する。
    if let Ok(zlib_include) = std::env::var("DEP_Z_INCLUDE") {
        build.include(zlib_include);
    }
    build
        // `PROJ_NEW=1` は cc command line で渡す必要がある (gaiaconfig.h で defined しても、
        // 一部 .c (例: srid_aux.c) は `<spatialite/gaiaconfig.h>` を読む前に
        // `#ifdef PROJ_NEW ... #include <proj.h> #else #include <proj_api.h>` を評価する。
        // proj_api.h は PROJ 8+ で削除された legacy header のためここで build が落ちる)。
        .define("PROJ_NEW", "1")
        // Windows MSVC: flex 生成 lex.*.c (gg_*.c が `#include` する) は `#include <unistd.h>`
        // を unconditional に発行するため、`YY_NO_UNISTD_H` を define して skip させる。
        // POSIX 系 (Linux/macOS) では unistd.h が存在するので define しても no-op。
        .define("YY_NO_UNISTD_H", "1")
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
    locate_sibling_out(out_dir, "geos-src-", "include/geos_c.h").unwrap_or_else(|build_root| {
        panic!(
            "could not locate geos-src OUT_DIR (expected {}/geos-src-*/out/include/geos_c.h). \
             Make sure `geos-src` is declared as a build-dependency.",
            build_root.display()
        )
    })
}

/// `target/<profile>/build/proj-sys-<hash>/out` (= cmake install prefix) を探す。
///
/// proj-sys 0.25.0 は `cargo:include=` / `cargo:root=` を emit しないため、依存元から
/// `DEP_PROJ_INCLUDE` で受け取れない。代わりに sibling の build dir を直接探す。
/// `bundled_proj` feature 有効時は cmake で proj 9.4.x を install するため、`out/include/proj.h`
/// と `out/lib/libproj.a` (Linux/macOS) が生成される。
#[cfg(feature = "bundled-spatialite")]
fn locate_proj_root(out_dir: &std::path::Path) -> std::path::PathBuf {
    locate_sibling_out(out_dir, "proj-sys-", "include/proj.h").unwrap_or_else(|build_root| {
        panic!(
            "could not locate proj-sys OUT_DIR (expected {}/proj-sys-*/out/include/proj.h). \
             Make sure `shpx-geom/bundled-proj` is implied by the `bundled-spatialite` feature \
             so that proj-sys is built with `bundled_proj` enabled.",
            build_root.display()
        )
    })
}

/// 自分の OUT_DIR の sibling として `<prefix><hash>/out/<sentinel>` を持つ build dir を
/// 見つけ、`out/` までのパスを返す。複数候補があれば mtime 最新を採用する。
/// 見つからない場合は (primary) build_root を `Err` で返し、呼び出し側でメッセージを組み立てる。
///
/// `cargo build --target=<triple>` 経由 (cargo-dist の Release build など) では
/// `[build-dependencies]` の OUT_DIR が host build dir (`target/<profile>/build/`) に出力される
/// 一方、自分自身は target build dir (`target/<triple>/<profile>/build/`) で動くため、
/// 同 build_root に sibling が居ない。primary search で見つからなければ host build dir も
/// 走査することで、`--target` あり (cargo-dist) / なし (通常 cargo build) の双方を吸収する。
#[cfg(feature = "bundled-spatialite")]
fn locate_sibling_out(
    out_dir: &std::path::Path,
    prefix: &str,
    sentinel_rel: &str,
) -> std::result::Result<std::path::PathBuf, std::path::PathBuf> {
    let primary_root = out_dir
        .parent()
        .and_then(std::path::Path::parent)
        .expect("OUT_DIR has no <build_root> ancestor");

    if let Some(found) = scan_build_root_for_sibling(primary_root, prefix, sentinel_rel) {
        return Ok(found);
    }

    if let Some(host_root) = host_build_root_from_target_out_dir(out_dir) {
        if host_root != primary_root {
            if let Some(found) = scan_build_root_for_sibling(&host_root, prefix, sentinel_rel) {
                return Ok(found);
            }
        }
    }

    Err(primary_root.to_path_buf())
}

/// 単一の `build/` ディレクトリ配下から `<prefix>...` を探し、最新 mtime の `out/` を返す。
#[cfg(feature = "bundled-spatialite")]
fn scan_build_root_for_sibling(
    build_root: &std::path::Path,
    prefix: &str,
    sentinel_rel: &str,
) -> Option<std::path::PathBuf> {
    let mut best: Option<(std::time::SystemTime, std::path::PathBuf)> = None;
    let entries = std::fs::read_dir(build_root).ok()?;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with(prefix) {
            continue;
        }
        let root = entry.path().join("out");
        if !root.join(sentinel_rel).exists() {
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
    best.map(|(_, p)| p)
}

/// `target/<triple>/<profile>/build/<crate>-<hash>/out` 形式の OUT_DIR から、
/// `target/<profile>/build/` (host build dir) を構築する。`--target` 未指定 (= primary == host)
/// の場合は `None` を返す。
#[cfg(feature = "bundled-spatialite")]
fn host_build_root_from_target_out_dir(out_dir: &std::path::Path) -> Option<std::path::PathBuf> {
    // out_dir = .../target/<triple>/<profile>/build/<crate>/out
    //                ^anchor                   ^build (rposition)
    let mut components: Vec<_> = out_dir.components().collect();
    let build_pos = components.iter().rposition(|c| c.as_os_str() == "build")?;
    if build_pos < 3 {
        return None;
    }
    let profile = components[build_pos - 1];
    // build_pos - 2 が <triple> 候補、build_pos - 3 が "target" であることを確認。
    if components[build_pos - 3].as_os_str() != "target" {
        return None;
    }
    let mut host_root = std::path::PathBuf::new();
    for c in components.drain(..build_pos - 2) {
        host_root.push(c.as_os_str());
    }
    host_root.push(profile.as_os_str());
    host_root.push("build");
    Some(host_root)
}

/// libspatialite の OMIT_* スイッチ。`build.rs` から `cc::Build.define` する側と
/// `OUT_DIR/spatialite/gaiaconfig.h` の `#define` 側で共有することで、整合性ズレを防ぐ。
/// v0.6 cycle 2 で `OMIT_GEOS` を解除 (`geos-src` 同梱)、cycle 3 で `OMIT_PROJ` を解除
/// (`shpx-geom/bundled-proj` 経由 proj-sys 同梱)。
#[cfg(feature = "bundled-spatialite")]
const OMIT_FEATURES: &[&str] = &["ICONV", "FREEXL", "MATHSQL", "EPSG", "KNN", "GEOCALLBACKS"];

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
    // Windows MSVC は `#if defined(_WIN32) && !defined(__MINGW32__)` 経路で
    // `config-msvc.h` を読む (`gg_shape.c` 他)。`gaiaconfig.h` 経由 OMIT_*/SPATIALITE_VERSION
    // の Windows 等価物として `gaiaconfig-msvc.h` も同様に必要。空 stub だと SPATIALITE_VERSION
    // / OMIT_* が未定義になり MSVC build が落ちる。本 build.rs では POSIX/MSVC で「ほぼ同じ
    // 内容」を書き、HAVE_DLFCN_H / HAVE_UNISTD_H など POSIX-only の差分のみ調整する。
    write_if_changed(&out_dir.join("config-msvc.h"), &config_msvc_h_body());
    write_if_changed(&spatialite_dir.join("gaiaconfig.h"), &gaiaconfig_h_body());
    write_if_changed(
        &spatialite_dir.join("gaiaconfig-msvc.h"),
        &gaiaconfig_h_body(),
    );
}

/// 内容差分があるときだけ書き込む。毎ビルド `fs::write` で mtime を更新すると、
/// cc-rs の incremental 判定 (timestamp 比較) と相性が悪くなるため。
#[cfg(feature = "bundled-spatialite")]
fn write_if_changed(path: &std::path::Path, body: &str) {
    if std::fs::read_to_string(path).is_ok_and(|cur| cur == body) {
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
fn config_msvc_h_body() -> &'static str {
    // Windows MSVC 用の最小 config。`config_h_body` から POSIX 専用 (DLFCN_H / UNISTD_H /
    // FDATASYNC / FTRUNCATE / LOCALTIME_R / STRCASECMP) を落とし、Windows MSVC で
    // 利用可能な C 標準ヘッダのみ宣言する。文字列比較は libspatialite 側で `_stricmp`
    // (Windows) / `strcasecmp` (POSIX) を `#ifdef` で切り替えるため定義不要。
    "#ifndef SPATIALITE_BUNDLED_CONFIG_MSVC_H
#define SPATIALITE_BUNDLED_CONFIG_MSVC_H

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
#define HAVE_STRING_H 1
#define HAVE_SYS_STAT_H 1
#define HAVE_SYS_TYPES_H 1
#define HAVE_SQLITE3EXT_H 1
#define HAVE_SQLITE3_H 1

#define HAVE_GETCWD 1
#define HAVE_GETTIMEOFDAY 1
#define HAVE_MEMMOVE 1
#define HAVE_MEMSET 1
#define HAVE_STRERROR 1

#define HAVE_DECL_SQLITE_INDEX_CONSTRAINT_LIKE 1

#define _LARGEFILE_SOURCE 1
#define NDEBUG 1

#endif
"
}

#[cfg(feature = "bundled-spatialite")]
fn gaiaconfig_h_body() -> String {
    // 上流 `vendor/.../headers/spatialite/gaiaconfig.h` を OUT_DIR で上書きするための公開 API
    // 制御マクロ。OMIT_* リストは `cc::Build.define` 側と共有 (両者がズレると preprocess 結果が
    // ファイルごとに分裂する)。`#undef ENABLE_*` 群は上流 baked-in の有効化を打ち消す。
    // `PROJ_NEW` は libspatialite が PROJ 6+ API (`proj_create_crs_to_crs` 等) を選択する
    // ためのスイッチ。proj-sys 0.25.0 同梱の libproj 9.4.x は当然 PROJ 6+ のため必須。
    // `gg_transform.c` 等の `#ifdef PROJ_NEW` 分岐が新 API パスに入る。
    // `GEOS_REENTRANT` は libspatialite が `spatialite_alloc_reentrant()` 経路 (PROJ_NEW
    // guard 付き) を選択するためのスイッチ。これを undef にすると `spatialite_alloc_connection`
    // が非 reentrant fallback (`alloc_cache.c` L.705 付近) に落ち、そこには PROJ_NEW guard
    // 無しの `pj_ctx_alloc()` (PROJ 8+ で削除済みの legacy API) があり build が落ちる。
    // libgeos 3.x は完全 reentrant のため有効化して問題なし (上流 baked-in も同設定)。
    let mut body = String::from(
        "#ifndef GAIACONFIG_H_BUNDLED\n#define GAIACONFIG_H_BUNDLED\n\n\
         #undef ENABLE_GCP\n#undef ENABLE_GEOPACKAGE\n#undef ENABLE_LIBXML2\n\
         #undef ENABLE_MINIZIP\n#undef ENABLE_RTTOPO\n\
         #undef GEOS_370\n#undef GEOS_3100\n#undef GEOS_3110\n\
         #undef GEOS_ADVANCED\n#undef GEOS_ONLY_REENTRANT\n\
         #define GEOS_REENTRANT 1\n\
         #define PROJ_NEW 1\n\n",
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
