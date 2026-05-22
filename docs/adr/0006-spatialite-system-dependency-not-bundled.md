# SpatiaLite は bundle せず、`mod_spatialite` をユーザー供給の system 依存とする

shpx は native 依存を単一バイナリへ bundle するのを「全 release target で軽量かつクリーンに static link できるものに限る」方針とし、**libproj は bundle する（`bundled-proj`、3 OS で static link 通過済み）が、libspatialite は bundle しない**。SpatiaLite driver は `mod_spatialite` 共有ライブラリを **runtime に `load_extension` で読み込む**ことだけを前提とし、ユーザーが各 OS の package manager（Linux `apt install libsqlite3-mod-spatialite` / macOS `brew install libspatialite` / Windows OSGeo4W）で別途用意する。v0.6.0 で入れた `bundled-spatialite` 一式（in-tree vendor・`build.rs` の static link 経路・`geos-src`/`libz-sys`/`link-cplusplus`・CLI feature・CI smoke job）は撤去し、ソース build しても静的 spatialite を得る逃げ道は残さない。

## Considered Options

- **bundle 継続（v0.6.0 の方針）**: 単一バイナリの一貫性は保てるが、(1) libspatialite が GEOS（C++）を引き込み、`unsafe_code = "deny"` の pure-Rust workspace に CMake ビルドの C++ stdlib リンクと 458 行の sibling-`OUT_DIR` 探索 build.rs という最も壊れやすい部分を恒久的に抱える、(2) 11MB の in-tree vendor がリポジトリを肥大化させる、(3) Windows MSVC で libspatialite 5.1.0 が `gg_shape.c::gaia_win_fopen` 付近の `GAIAGEO_DECLARE` マクロ展開時に C2054 で停止し、上流 fork レベルの C ソース patch なしには越えられない、(4) static link では SQLite の標準 extension ロード経路（`SELECT load_extension(...)` / rusqlite `load_extension`）を使えず、生の `Connection::handle()` に対する独自 `unsafe` FFI 初期化（`spatialite_initialize` → `spatialite_alloc_connection` → `spatialite_init_ex`、cache の手動ライフサイクル管理込み）が必要になり、dynamic 経路との二重保守を強いる。小規模プロジェクトが libspatialite fork を抱え dep bump のたびに追従する保守コストは、long-tail driver である SpatiaLite の価値に対して過大。

## Consequences

- **恒久的な理由は「重さ」**（GEOS の C++ 依存・vendor 肥大・脆い build.rs・fork 保守）。Windows MSVC 破綻は撤去の具体的な引き金だが、仮に将来 Windows 互換が解消しても方針は揺れない。
- **接続経路が単一の標準形に収束する**。dynamic 経路だけになることで、接続は常に SQLite 標準の `load_extension` を通る。static link が要求した独自 FFI 初期化（`spatialite_init_ex` を raw connection handle に直接呼ぶ・extension cache の手動管理）が消え、`unsafe` と二重経路の保守負担が無くなる。
- **副次的にライセンスが単純化する**。libspatialite を bundle すると LGPL 2.1 / MPL 1.1 / GPL 2.0 triple-license の再リンク配布義務（`NOTICE`）が発生したが、ユーザー供給の共有ライブラリを runtime ロードするだけなら shpx はそれを配布しないため義務は消える。
- **「no system deps な単一バイナリ」というブランドは SpatiaLite だけ崩れる**。これは隠さず「shpx は単一バイナリ。ただし SpatiaLite は唯一 system extension を要する optional driver」と明示する。[ADR-0001](0001-gdal-free-pure-rust.md)（GDAL 非依存・pure Rust）の単一バイナリ志向に対する、意図的かつ唯一の例外。
- `bundled-proj`（reprojection 用・v1.0 release で有効）は完全に独立しており、本決定の影響を受けない。
