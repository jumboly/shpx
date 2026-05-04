# Changelog

本プロジェクトの変更履歴。フォーマットは [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) に準拠し、バージョン番号は [Semantic Versioning](https://semver.org/spec/v2.0.0.html) に従う。

## [1.1.0] - 2026-05-04

### Added

- **`shpx-cli` を lib + bin の二本立てに再構成し、`pub fn run()` / `pub fn run_with_app_name(name)` を export**: 業務固有 / 社内 driver を OSS 本体に持ち込まずに 1 バイナリで同居させたい派生プロジェクトが、`[dependencies] shpx-cli = "1.1"` を追加して `shpx_cli::run()` を呼ぶだけで標準 9 driver 込みの shpx CLI を再利用できる。標準 driver の linker pin は `shpx-cli` lib 内の `registry` モジュールが担い、派生側は追加 driver の `use my_extra_driver as _;` 1 行のみで `inventory` 経由の自動登録が成立する。argv\[0\] の file_stem を `clap::Command::name` / `bin_name` に注入する経路で `--help` の "Usage:" 行も派生バイナリ名 (`my-shpx` 等) に切り替わる。詳細は [`docs/EMBEDDING.md`](docs/EMBEDDING.md)。
- **`crates/shpx-cli/examples/embedded.rs`**: 派生バイナリの最小例。`cargo run --example embedded -p shpx-cli -- drivers` で動作確認可。CI が `shpx_cli::run` の API 契約 / argv\[0\] 由来の表示名切替 / inventory rlib pin を smoke test として常時検証する。

### Changed

- **`shpx --help` の `about` 文字列を更新**: v0.3 系のままだった `"v0.3: SHP / GeoParquet / CSV / GeoJSON / GPKG / FlatGeobuf / PostGIS + --reproject"` を、SQL Server / SpatiaLite を含む現状の 9 driver 列挙に修正。

## [1.0.0] - 2026-05-04

v1.0 マイルストーン「仕上げと配布」のリリース。`cargo-dist` ベースの 5 target × 3 OS 配布工程 (`aarch64-apple-darwin` / `aarch64-unknown-linux-gnu` / `x86_64-apple-darwin` / `x86_64-unknown-linux-gnu` / `x86_64-pc-windows-msvc`) を整備し、shell installer と GitHub Releases から単一バイナリで shpx を入手できる体制が整った。同梱するのは 0.8.0 以降に積まれた v1.0 cycle 1〜4 (LICENSE / NOTICE / 進捗バー / examples / README 5 分チュートリアル / `--format=json` / cargo-dist + multi-OS smoke) と v1.x 系の中期改善 (SQL Server tiberius fork で bulk insert bug 修正 + LOGIN7 packet_size 引き上げ + RDB writer batch 経路の multi-row VALUES 化)。9 driver (SHP / Parquet / GPKG / GeoJSON+NDJSON / CSV / FGB / PostGIS / SQL Server / SpatiaLite) のすべてが reader streaming + 統一 OnLoss + LICENSE 整備済みの状態で 1.0 を切る。

### Fixed (v1.0 cycle 5 — Windows MSVC release build)

- **`dist-workspace.toml` で `msvc-crt-static = false`**: cargo-dist 0.31 は Windows MSVC ターゲットに対し default で `+crt-static` を Rust 側に注入するが、`proj-sys 0.25.0` の cmake build は `MSVC_RUNTIME_LIBRARY` 未設定で cmake デフォルト (`/MD` = MultiThreadedDLL) で libproj を build するため、Rust 側 libcmt.lib (static CRT) と libproj 内 dllimport の `__imp_hypot` / `__imp_ceilf` / `__imp__time64` ほか 31 件が衝突し `LNK1120: 31 unresolved externals` で release build が halt していた。`msvc-crt-static = false` で Rust 側を `/MD` 相当に倒し、proj-sys と CRT を整合させる。利用者環境の vcruntime140.dll は rustup 環境では既存、Windows 10+ の ucrt は OS 同梱のため追加 redist 配布は不要 (axodotdev/cargo-dist#496 / georust/proj#192 参照)。

### Performance (v1.x RDB writer batch 経路の multi-row VALUES 化)

- **shpx-driver-sqlserver / shpx-driver-postgis: `LayerWriter::write_batch` を multi-row `VALUES` で 1 RPC に複数行詰めて round-trip を桁違いに削減**: 旧実装は 1 行 1 RPC を発行していたため、`--insert-mode=batch` および bench-rss SQL Server prepare で round-trip 待ちが支配的だった。`shpx-rdb-common::multirow_chunk_rows(params_per_row, max_params_per_rpc, safety_margin)` を共通ヘルパとして導入し、SQL Server (param 上限 2100、属性 6 列で `chunk_rows ≈ 260`、~175× 削減) と PostGIS (PostgreSQL extended protocol Bind の Int16 上限 32767、`chunk_rows ≈ 4680`、~6500× 削減) の双方で同 helper を呼ぶ対称実装。トランザクション境界は 1 batch = 1 トランザクションを維持し chunk 境界では COMMIT しない。bulk 経路 (`COPY FROM STDIN BINARY` / staging + `tiberius::bulk_insert`) は変更なし。SpatiaLite / GPKG はローカル SQLite で network round-trip 無しのため対象外。MySQL / Oracle が後続で追加されるときは同 helper を再利用する。詳細は `docs/SQLSERVER.md`、`docs/POSTGIS.md`、`docs/ROADMAP.md`。

### Fixed (v1.x SQL Server 戦略 — 中期)

- **shpx-driver-sqlserver: `Daten` COLMETADATA length バイトの bulk insert bug を修正**: `tiberius 0.12.3` の `bulk_insert` 経路で `DataType::Date32` 列を含む schema を送ると、`Daten` の COLMETADATA に length バイトが余分に 1 個書かれて後続列の type info を破壊し、SQL Server が `Invalid column type from bcp client for colid N` (error 4816) を返していた。**最小再現は 3 列 / 1 行** (`id Int64 + created Date32 + geom`)、含意は `bench-rss` SQL Server prepare の 1h+ 待ちと `bulk_all_types_together` の `#[ignore]` 化。修正方針として upstream の closed PR #346 (`Daten` 分岐で `dst.put_u8(self.len())` を発行しない) を `jumboly/tiberius` の `shpx-patches` branch に backport し、workspace `Cargo.toml` の `tiberius` dep を `crates.io` 0.12 から git fork 参照に切り替えた。`bulk_all_types_together` の `#[ignore]` も解除済み (`tests/bulk_roundtrip.rs:343`)。詳細は `docs/SQLSERVER_BULK_BUG_REPRO.md` および `docs/ROADMAP.md` v1.x の SQL Server 戦略節。

### Performance (v1.x SQL Server 戦略 — 中期)

- **shpx-driver-sqlserver: TDS LOGIN7 packet_size を 32767 にして bulk insert throughput を改善**: tiberius default の 4KB packet では BCP packet ごとに 1 round-trip 発生し bulk throughput が頭打ちになる。upstream PR #400 (closed-unmerged, [Add packet_size configuration for LOGIN7](https://github.com/prisma/tiberius/pull/400)) を `jumboly/tiberius#shpx-patches` に backport し、`crates/shpx-driver-sqlserver/src/conn.rs::build_config` で `Config::packet_size(32767)` を呼ぶ。SQL Server 側で 16KB あたりに negotiate-down される想定。upstream のベンチで 19.3M 行 bulk が 4KB → 16KB で 186s → 108s (**+42% throughput**) を実証済み。`bench-rss` SQL Server prepare の所要時間も短縮見込み。

### Added (v1.0 cycle 1 完了確認)

- **v1.0 cycle 1 完了基準を再定義**: ogr2ogr 比較 (`shpx_secs <= 1.667 * ogr_secs`) を完了基準から外し、`bench-smoke-mssql.yml` で取得した shpx 単独 wall-clock を絶対値として公開する形に変更し `docs/ROADMAP.md` v1.0 完了基準を `[x]` 化。`scripts/bench-vs-ogr-mssql.sh` はローカル開発者向けの参考 utility として温存 (CI では使わない)。実測値と詳細は `docs/SQLSERVER.md` Benchmark 節を参照。

### Added (v1.0 cycle 4 — cargo-dist + multi-platform CI)

- **`dist-workspace.toml` 新設 + `dist init` で配布工程整備**: cargo-dist 0.31 (binary 名は `dist`) を `.scratch/cargo-dist/bin/dist` に user-local install し、`dist init --yes --hosting github --installer shell` で `dist-workspace.toml` を生成。targets は `aarch64-apple-darwin` / `aarch64-unknown-linux-gnu` / `x86_64-apple-darwin` / `x86_64-unknown-linux-gnu` / `x86_64-pc-windows-msvc` の 5 triples。`features = ["bundled-spatialite"]` を明示し、Release artifact は libspatialite / GEOS / libproj / SQLite を C ソースから static link した単一バイナリで配布する。`Cargo.toml` に `[profile.dist]` (`inherits = "release"`、`lto = "thin"`) も自動追加。`shpx-bench-rss` は `[package.metadata.dist] dist = false` で Release 対象外に。
- **`.github/workflows/release.yml` 新設**: `dist generate` で生成された tag-driven workflow。tag push (`v*.*.*`) で plan → build matrix → host (artifact upload) → announce の 5 phase。手で編集せず、metadata 変更時は再 generate する運用。
- **`.github/workflows/ci.yml` に bundled smoke を 3 OS に拡張**: 既存の `bundled-spatialite-smoke` を `bundled-spatialite-smoke (linux)` に rename し、`bundled-spatialite-smoke (macos-arm64)` (`runs-on: macos-14`、`brew install cmake`) と `bundled-spatialite-smoke (windows)` (`runs-on: windows-latest`、`choco install llvm`、`continue-on-error: true`) を対称に追加。Windows のみ best-effort 扱いで CI red を許容し mainline merge をブロックしない。
- **`docs/SPATIALITE.md` サポート OS 表 (v1.0 縮退方針)**: Release artifact / CI smoke / system dep の 3 列表で 5 target triples を整理。`aarch64-apple-darwin` / `x86_64-unknown-linux-gnu` / `aarch64-unknown-linux-gnu` を `○` (Release 対象、緑必須)、`x86_64-apple-darwin` / `x86_64-pc-windows-msvc` を `△ best-effort` (build を試みるが失敗時は当該 OS の artifact のみ欠落させて他 OS の publish を継続) と明示。詳細縮退手順は `docs/ROADMAP.md` v1.0 リスク節参照。

### Fixed (v1.0 cycle 4)

- **shpx-driver-spatialite/build.rs: `--target` 指定時に geos-src OUT_DIR を見失う問題を修正**: `cargo build --target=<triple>` (cargo-dist の Release build など) では、`[build-dependencies]` である `geos-src` の OUT_DIR は host build dir (`target/<profile>/build/`) に出力されるが、shpx-driver-spatialite 自身は target build dir (`target/<triple>/<profile>/build/`) で動くため、既存の `locate_sibling_out` (sibling 走査のみ) では `geos_c.h` を見失い `could not locate geos-src OUT_DIR ...` で panic していた。`host_build_root_from_target_out_dir` ヘルパを追加し、primary build_root に sibling が居なければ host build_root も走査するフォールバックを入れた。`cargo build` (`--target` なし) の従来経路は primary 走査だけで成立するため挙動変化なし。`dist build --target=aarch64-apple-darwin --artifacts=local` で 6.5 MB の bundled tarball 生成を確認 (`otool -L` で libspatialite / libgeos / libproj への動的依存ゼロ)。

### Added (v1.0 cycle 3)

- **examples/*.sh ×6 + `examples/data/`**: SHP → GeoParquet (01) / SHP → PostGIS (02) / PostGIS → FGB (03) / `--reproject` (04) / `--on-loss=error|warn|skip` 比較 (05) / `--insert-mode=bulk` vs `batch` (06) の 1-shot シナリオを新設。test data は `cities.shp` (5 都市 / WGS84) / `cities-3857.shp` (Web Mercator 派生) / `lossy.csv` (DBF 10-byte 制限に引っ掛かる長い列名) を `examples/data/` にコミットし cold で `bash examples/01-*.sh` が走る。共通環境変数は `SHPX_BIN` (実行コマンド)、`OUT` (出力先 `/tmp/shpx-examples`)、`PG_URL` (PostGIS 接続)。再生成手順は `examples/data/REGENERATE.md`。
- **README.md 5 分チュートリアル形式に再構成**: 「インストール → SHP → GeoParquet → PostGIS bulk load → reproject」の 4 ステップを冒頭に置き、各ステップから `examples/*.sh` への導線を張った。`examples` 章を新設し 6 本の表を掲載。「ライセンス」節を v1.0 cycle 1 で配置済みの `LICENSE-APACHE` / `LICENSE-MIT` / `NOTICE` に合わせて Apache-2.0 OR MIT に確定。`docs/ON_LOSS.md` / `docs/STREAMING.md` への設計ドキュメントリンクも追加。
- **`shpx schema` / `shpx drivers` に `--format=text|json`**: `crates/shpx-core/src/capabilities.rs` の `Capabilities` / `StringEncoding` に `serde::Serialize` を生やし、`StringEncoding` は internally tagged 形式 (`{"kind":"fixed","value":"utf-8"}` / `{"kind":"configurable","value":[...]}`) でシリアライズする。`shpx drivers --format=json` は `[{ name, schemes, capabilities }]` 配列、`shpx schema --format=text` は driver 名 + `name / data_type / nullable / metadata` の表形式。後方互換のためデフォルトは据え置き (`schema=json` / `drivers=text`)、`schema --pretty` は `--format=json` と直交フラグとして残す。`crates/shpx-core/src/capabilities.rs::tests::serializes_to_stable_json_shape` で JSON 形状の互換契約を固定化 (フィールド名 / `kind` タグ / null 表現を変える PR では本テストの期待値を必ず更新する)。

## [0.8.0] - 2026-05-03

v0.8 マイルストーン「Streaming Reader Parity」のリリース。reader 9 driver のうち Parquet 以外で残っていた eager-load (`Vec<Feature>` / `VecDeque<Row>` / `Vec<RecordBatch>` 等) を全廃し、`open()` 直後に全行をメモリへ載せる経路を撲滅した。10M 行クラスの入力でもピーク RSS が batch サイズ + 接続バッファに頭打ちになる構造を全 driver で揃え、v1.0 出荷時のメモリプロファイル一貫性を確保した。`LayerReader` trait シグネチャは v0.1 から不変のまま、内部実装のみを置き換える形での achievement。本リリースには 0.7.0 以降に積まれた v1.0 cycle 1〜2 (LICENSE / NOTICE / 進捗バー / `--quiet`) と v0.4 ベンチ完了確認も同梱する。

### Added (v0.8)

- **shpx-driver-shp / fgb / csv (v0.8 cycle 1、真のストリーミング)** [d61c83f, 既出]: SHP は worker thread + `sync_channel(2)`、FGB は `FeatureIter<R, NotSeekable>` を field に保持、CSV は `csv::Reader<Box<dyn Read>>` を field 化することで eager-load (`VecDeque<Row>` 等) を撲滅。
- **shpx-driver-gpkg / spatialite (v0.8 cycle 2、SQLite keyset pagination)** [d61c83f, 既出]: `crates/shpx-rdb-common/src/streaming.rs` 新設で `KeysetRowsIter` (`WHERE rowid > ? ORDER BY rowid LIMIT ?`) と `OffsetRowsIter` (`LIMIT ? OFFSET ?`) を提供、GPKG / SpatiaLite reader が共有する。SpatiaLite の WITHOUT ROWID テーブルは `Error::Driver` で明示拒否。
- **shpx-driver-geojson (v0.8 cycle 3、真のストリーミング)**: `crates/shpx-driver-geojson/src/stream.rs` を新設し、FeatureCollection 用の自前 `FcFeatureStream` (JSON state machine で `[` までシーク → `,` 区切りで 1 feature ずつ pull) と NDJSON 用の `NdjsonStream` (`BufRead::lines()` ベース) を実装。`reader.rs` は file head 1 MiB を probe して top-level `crs` メンバを抽出 → 先頭 N=1024 feature をサンプリングして型推論 → 本番ストリームはファイル再 open + 真の逐次 yield、の 3-pass 構造に再編。サンプル数は `SHPX_GEOJSON_INFER_SAMPLE` env で override 可。`geojson::FeatureReader` 直用ではなく自前 `FcFeatureStream` を使う理由は、上流 0.24 の空配列 panic + Iterator 終了後 panic の 2 バグ回避。
- **shpx-driver-postgis (v0.8 cycle 4、async-to-sync mpsc)**: `Vec<RecordBatch>` field と `chunk_batch` ヘルパを撤廃し、background OS thread + `std::sync::mpsc::sync_channel(2)` で `tokio_postgres::query_raw` の `RowStream` を逐次消費する構造へ置換。worker は reader 専用に新規 connect した `Client` を所有し、`READ_BATCH_SIZE` (65536) 行ごとに `RecordBatch` を組んで channel へ送る。`tests/reader_cancel.rs` 新設で「Reader を mid-iter で drop した時 worker が SELECT を解放する」ことを env-gated 検証 (`reader_drop_releases_select_promptly` / `reader_full_consume_drops_cleanly`)。
- **shpx-driver-sqlserver (v0.8 cycle 5、async-to-sync mpsc)**: cycle 4 と同じ pattern で `tiberius::QueryStream` を逐次消費。`exec_select` / `extract_srid_from_first_row` / `chunk_batch` を削除、SRID 解決は probe `SELECT TOP 1 ... STSrid` 一本に集約。
- **shpx-core (v0.8 cycle 6、bench infra)**: `crates/shpx-core/src/bench_util.rs` 新設で `peak_rss_kib() -> Option<u64>` を提供 (Linux: `/proc/self/status` の `VmHWM`、その他 OS: `None`)。reader streaming のピーク RSS 計測に使う。
- **`docs/STREAMING.md` 新設 (v0.8 cycle 6)**: driver × streaming 戦略 × 期待 peak RSS 表、キャンセル挙動、トランザクション保持期間、バッチサイズ影響、関連実装ファイルへのリンクを 1 箇所に集約。

### Changed (v0.8)

- **`LayerReader::row_count_hint`**: cycle 4/5 で PostGIS / SQL Server の hint を `Some(row_count)` → `None` に変更 (streaming のため事前に行数は確定しない)。GeoJSON / NDJSON も同じく `None` に。CLI 進捗バーは `None` の場合 Spinner にフォールバック (v1.0 cycle 2 の挙動)。

### Added (v0.4 / v1.0 cycle 1〜2 — 0.8.0 同梱)

- **LICENSE / NOTICE**: MIT / Apache-2.0 dual license の `LICENSE-MIT` / `LICENSE-APACHE` をリポジトリルートに配置、`NOTICE` で third-party 依存 (libspatialite / libgeos / libproj / SQLite / arrow-rs / parquet / tokio-postgres / tiberius / flatgeobuf / geozero / shapefile / geojson / proj / rusqlite ほか) を aggregate listing。`Cargo.toml` の `[workspace.package].license = "MIT OR Apache-2.0"` 宣言は v0.1 から既出だが、ルートに license 本文が無く `cargo-dist` 配布の前提を満たさないため整備した。`crates/shpx-driver-spatialite/NOTICE` は driver scope の詳細 (vendor 範囲・LGPL 2.1 配布要件) を持つためそのまま残す。
- **shpx-cli (v1.0 cycle 2、進捗バー)**: `shpx convert` 実行中に行ベースの進捗バーを stderr に表示する。`LayerReader::row_count_hint()` が `Some(n)` を返す driver (SHP / Parquet / GPKG / FGB / GeoJSON / PostGIS / SQL Server / SpatiaLite) は `{percent}% [{bar}] {pos}/{len} rows {per_sec} ETA {eta}` の ProgressBar、`None` を返す CSV は `{spinner} {pos} rows {per_sec}` の Spinner に倒す。stderr が非 TTY (CI ログ / pipe) のときは `std::io::IsTerminal` 判定で自動的に `ProgressBar::hidden()` に倒し、ANSI escape で CI ログを汚さない。`indicatif = "0.17"` を workspace dep に追加。
- **shpx-cli (v1.0 cycle 2、`--quiet` global flag)**: `-q` / `--quiet` を全サブコマンド共通の global flag として追加し、`init_tracing` を `error` レベルに倒して `shpx::cli` の info ログと進捗バーの両方を抑止する。`--verbose` とは clap の `conflicts_with` で排他。

### Docs

- **`docs/ON_LOSS.md` 精緻化 (v1.0 cycle 2)**: SHP の `z-on-shp` / `m-on-shp` を「将来用、現状の中間表現が XY のみのため未発火」から「reader 経路で `*Z` / `*M` shape を読み込む際に発火する」に訂正 (`crates/shpx-driver-shp/src/geometry.rs:64-85`)。SpatiaLite / PostGIS / SQL Server の loss_kind 発火位置に writer.rs の line ref を追記。CSV `encoding-unmappable` を「定義のみ、現状未発火」表注に格上げ。`row_count_hint` と進捗バーの対応表を新設し、driver ごとの ProgressBar / Spinner モードを 1 表で並べた。kind 命名規約 (`<phenomenon>-on-<driver>` で prefix 共通) を Future Work メモに追加。

### Build

- **`.github/workflows/bench-smoke-mssql.yml` 新設**: SQL Server bench を Linux x86_64 (ubuntu-latest) で実行する `workflow_dispatch` 専用 workflow。`services.mssql` 上で `shpx convert --insert-mode=bulk --create-table=always` を `SHPX_MSSQL_BULK_CHUNK=1000000` 下で 10M 行流し、tempdb 溢れずに完走することを確認する (= v0.4 完了基準判定)。`rows` / `runs` input、`bench-output.txt` を artifact で 30 日保持。`scripts/bench-vs-ogr-mssql.sh` は ogr2ogr 比較用にローカル開発者向けで温存。

### Fixed

- **v0.4 完了基準 (chunk size 1M でも tempdb 溢れなし) を確認**: 上記 bench workflow を `rows=10000000 runs=3` で実行し、ubuntu-latest 上で 3 run 全完走 (139.01 / 138.23 / 138.53 s、median 138.53s)。`docs/ROADMAP.md` v0.4 完了基準 L.95 を `[ ]` → `[x]`、`docs/SQLSERVER.md` Benchmark 節と `CHANGELOG.md` 0.4.0 Known Issues を訂正。

## [0.7.0] - 2026-05-03

v0.7 マイルストーン「Driver Feature Parity & Refactor」のリリース。新 driver (PostGIS / SQL Server / SpatiaLite) と古い driver (SHP / Parquet / GPKG / GeoJSON / CSV / FGB) の間に残っていた data-correctness 直結のギャップ 3 項目 ((1) Parquet writer の OnLoss scaffold 整備、(2) GeoJSON writer の silent demotion を `apply_on_loss` 経由化、(3) SpatiaLite reader への `--where` / `--select` / `--query` backport) を塞ぎ、cycle 2 で reader 側の CRS metadata 経路 (Parquet PROJJSON / FGB header `crs` / GPKG `definition_12_063` WKT2) を完備、`crates/shpx-cli/tests/cross_driver_matrix.rs` で driver 横断 e2e roundtrip matrix を整備した。配布工程 (`cargo-dist`、追加 OS 対応) は v1.0 へ分離する。

### Added

- **shpx-driver-spatialite (v0.7 cycle 1、reader filtering backport)**: `crates/shpx-driver-spatialite/src/reader.rs` を PostGIS 同型の table mode / query mode 2 経路に分け、`ReadOpts.where_clause` / `select` / `query` を CLI から受け取れるようにした。table モードでは `?table=` で解決した名前に `WHERE` / 列絞り SQL を埋め、query モードでは任意 SQL をサブクエリ化して列メタを引き直す。geometry 列を含まない `--select` / `--query` と `--query` 末尾の `;` は `Error::Driver` で拒否 (PostGIS と同方針)。
- **shpx-driver-geojson (v0.7 cycle 1、OnLoss 経由化)**: writer の silent demotion 経路を `apply_on_loss` 経由に置換した。Decimal128/256 列は `decimal-on-geojson` で列除外、`Timestamp(Nanosecond | Microsecond, _)` 列は `timestamp-precision-on-geojson` で列除外、`UInt64` の `i64::MAX` 超過値は `uint64-overflow-on-geojson` で文字列降格、`Float32/64` の NaN / Infinity は `nonfinite-float-on-geojson` で `null` 化。`Binary` / `LargeBinary` と `List` / `Struct` / `Map` の既存経路 (`binary-on-geojson` / `structured-on-geojson`) も `apply_on_loss` ヘルパに統一。
- **shpx-driver-parquet (v0.7 cycle 1、OnLoss scaffold)**: `crates/shpx-driver-parquet/src/util.rs` に `apply_on_loss` ヘルパと空の `loss_kind` module を整備した。現状の Parquet writer は `coerce_types=false` 固定で `Timestamp(Nanosecond)` / `Decimal128(<= 38)` を完全保持し、shpx 中間表現も XY のみのため `precision-on-parquet` / `nanosecond-truncation-on-parquet` / `z-on-parquet` / `m-on-parquet` の発火経路は存在しない (テストでロスなしを裏付け)。`--parquet-coerce-types` 等のフラグを追加した時点で実定数を順次入れる想定。
- **shpx-driver-parquet (v0.7 cycle 2、reader CRS metadata)**: Arrow field metadata の `geo` JSON (`columns.<geom>.crs`) を `shpx_geom::projjson::decode` で parse し、`Crs` の authority / wkt2 / projjson を復元する reader 経路を追加。
- **shpx-driver-fgb (v0.7 cycle 2、reader CRS metadata)**: header の `crs` field (`org` / `code` / `wkt`) を parse し、authority + WKT2 を `Crs` に復元する reader 経路を追加 (それ以前は EPSG 整数のみ拾っていた)。
- **shpx-driver-gpkg (v0.7 cycle 2、WKT2 reader 完備)**: `gpkg_spatial_ref_sys` の OGC 12-063 拡張列 `definition_12_063` (WKT2) を reader が拾うようになった。列が存在し非空であれば `Crs.wkt2` に格納し、`definition` (WKT1) と並走する (どちらを優先するかの `--gpkg-prefer-wkt2` フラグは Future work)。
- **shpx-cli (v0.7 cycle 2、cross-driver matrix)**: `crates/shpx-cli/tests/cross_driver_matrix.rs` を新設し、PostGIS ↔ SQL Server / SpatiaLite / Parquet / FGB / GeoJSON / SHP の主要往復を env-gate で網羅。既存 `crates/shpx-driver-spatialite/tests/cross_driver_roundtrip.rs` は driver scope の回帰検出として残す。

### Internal

- **`shpx_rdb_common::percent_decode` 統一 (v0.7 cycle 2)**: GPKG / SpatiaLite / SQL Server の `options.rs` に重複していた自前 `percent_decode` を `shpx-rdb-common` の共通ヘルパに集約した。各 driver の挙動・エラーメッセージ・tracing target は不変で、CLI ユーザー視点の挙動には影響しない。
- **GPKG `apply_on_loss` の rdb-common closure パターン化 (v0.7 cycle 2)**: GPKG driver で直接 `tracing::warn!` を呼んでいた経路を、他 driver と同じく `shpx_rdb_common::apply_on_loss(kind, field, on_loss, warn_fn)` の closure 経路に揃えた。

### Docs

- **`docs/ON_LOSS.md` 新設**: driver × loss kind の動作表 + driver 別の発火条件 + `--on-loss=error|warn|skip` の動作仕様 + 内部実装メモを 1 箇所に集約。各 driver docs の「損失変換」節は ON_LOSS.md への 1 行リンクへ短縮した。
- **ファイル driver docs の体裁統一**: `docs/CSV.md` / `docs/GEOJSON.md` / `docs/GPKG.md` / `docs/FGB.md` を `docs/POSTGIS.md` / `docs/SQLSERVER.md` / `docs/SPATIALITE.md` と同型構造 (対応範囲 / サポート対象拡張子 / CRS の扱い / ジオメトリ / データ型マッピング / 損失変換 / 環境変数まとめ / スコープ外 / 内部実装メモ / Future work) に揃えた。各 driver 固有の章 (CSV「区切り文字」「エンコーディング」、GeoJSON「properties の型推論」など) は保持。
- **`docs/CRS.md` reader 経路節を追加**: cycle 2 で実装された Parquet PROJJSON / FGB header `crs` / GPKG `definition_12_063` (WKT2) の reader 解決経路を「各フォーマットからの読み出しマッピング」表として追記。
- **`docs/ROADMAP.md` の訂正**: v0.5 完了基準のチェックボックス 2 件 (L.128-129) を `[ ]` → `[x]` に訂正 (実体は v0.5 cycle 2a / 3 で実装済み)。v0.7 cycle 1 description (L.203) を Parquet OnLoss scaffold 実態 (実 LossKind 定数追加は v0.8+) に訂正。

### Build

- workspace MSRV は 1.85 据え置き。新規依存の追加なし。
- workspace `Cargo.toml` の `[workspace.package].version` を `0.6.0` → `0.7.0` へ bump。全 driver / cli は `version.workspace = true` で追従。

### Known Issues

- **Parquet driver の OnLoss は scaffold のみ**: `crates/shpx-driver-parquet/src/util.rs` の `loss_kind` module は空で、実際の損失検出は発火しない。現状の writer が `coerce_types=false` 固定で発火経路を持たないためで、`--parquet-coerce-types` 等のフラグや Z/M 中間表現を導入した時点で `precision-on-parquet` / `nanosecond-truncation-on-parquet` / `z-on-parquet` / `m-on-parquet` を順次追加する。

## [0.6.0] - 2026-05-03

v0.6 マイルストーン「bundled-spatialite + 配布バイナリ準備」のリリース。`crates/shpx-driver-spatialite/build.rs` で libspatialite 5.1.0 / libgeos / libproj を C ソースから vendor + `cc` static link する `bundled-spatialite` feature の本実装、`.github/workflows/ci.yml` への `bundled-spatialite-smoke` job 追加、`docs/SPATIALITE.md` の bundled 節新設までを含む。v0.5.0 で「Known Issues: bundled-spatialite は v0.6 で本実装」と書いた制限を解消する。CI で常時検証するのは Linux x86_64 のみで、macOS (Apple Silicon / Intel) / Windows は v1.0 の `cargo-dist` 配信時に拡張する best-effort 段階。詳細は `docs/SPATIALITE.md` の「bundled-spatialite ビルド」節を参照。

### Added

- **shpx-driver-spatialite (v0.6 cycle 1、build.rs 足場 + libspatialite vendor 最小構成)**: `crates/shpx-driver-spatialite/vendor/libspatialite-5.1.0/` に libspatialite 5.1.0 release tarball を in-tree commit (`vendor/SHA256SUMS` で再現性固定、download は build.rs では行わない)。`crates/shpx-driver-spatialite/build.rs` を新設し、`#[cfg(feature = "bundled-spatialite")]` 内でのみ `cc::Build` で C ソース 100+ ファイルを集めて `.compile("spatialite_bundled")`。GEOS / PROJ / RTTOPO / libxml2 / freexl / iconv / minizip を全て off にする `OMIT_*` define 群と、自動生成する `gaiaconfig.h` で純粋な geometry blob I/O + R\*Tree (R\*Tree は SQLite native) のみで build を通す。`crates/shpx-driver-spatialite/Cargo.toml` に `build = "build.rs"` と `[build-dependencies] cc` を追加。`crates/shpx-driver-spatialite/src/conn.rs` の cfg 分岐に FFI 実体投入 (`extern "C" fn sqlite3_modspatialite_init` を `rusqlite::Connection::handle()` の raw pointer に直接呼ぶ)。`crates/shpx-driver-spatialite/NOTICE` を新設し libspatialite triple license (MPL 1.1 / GPL 2.0 / LGPL 2.1) と vendor バージョンを明記。
- **shpx-driver-spatialite (v0.6 cycle 2、GEOS リンク)**: `geos-src = "0.2"` (libgeos 3.x C++ ソース vendor + CMake build) を build-dep として workspace + driver Cargo.toml に追加。`geos-src` の build script が export する include path / link 命令を build.rs で受け取り、libspatialite ビルドの `cc::Build` に `.include(geos_include)` で feed。build.rs の `OMIT_FEATURES` から `"GEOS"` を削除し、GEOS 依存ファイルもコンパイル対象に追加。`tests/bundled_geos_smoke.rs` で `SELECT ST_Buffer(GeomFromWKB(?, 4326), 0.1)` を 1 件叩く feature-gated smoke test を追加。cycle 1 で入れた局所 patch (`#ifndef OMIT_GEOS` ガード 2 箇所) を物理削除し、vendor を upstream tarball に bit-identical な状態に復元。`NOTICE` の「shpx local modifications」から patch 項目も削除。
- **shpx-driver-spatialite (v0.6 cycle 3、PROJ リンク + `spatialite_init()` 直接呼び)**: `shpx-geom` の `bundled-proj` (proj 0.28 / proj-sys) が同梱する libproj を libspatialite からも共有 (PROJ symbol の二重リンク回避)。proj-sys 0.25.0 が `cargo:include` / `cargo:root` を emit しないため、cycle 2 の `locate_geos_root` と同パターンで `target/<profile>/build/proj-sys-<hash>/out/include/proj.h` を sibling target で探索する `locate_proj_root` を実装し、`build.rs::locate_sibling_out` に共通化。`crates/shpx-driver-spatialite/Cargo.toml` の `[package]` に `links = "spatialite_bundled"` を宣言 (cargo の duplicate link 検出のため)。`crates/shpx-cli/Cargo.toml` の `bundled-spatialite` feature を `["shpx-driver-spatialite/bundled-spatialite", "shpx-geom/bundled-proj"]` に変更し、CLI レイヤで bundled-spatialite が必ず bundled-proj を implies する設計に。`load_mod_spatialite` の bundled feature 経路から `load_dynamic` への fallback を削除し、`SHPX_SPATIALITE_PATH` env は bundled feature 時 warn で無視。`tests/bundled_proj_smoke.rs` で EPSG:4326 → 3857 transform を 1 件叩く feature-gated smoke test を追加。実装メモ: 当初 ROADMAP では `DEP_PROJ_INCLUDE` / `DEP_PROJ_ROOT` 経由の include 共有を想定していたが、proj-sys 0.25.0 が `cargo:include` / `cargo:root` を emit しないため sibling target 探索に pivot した（cycle 2 の `locate_geos_root` と同パターン）。link 命令 (`cargo:rustc-link-lib=proj`) は proj-sys 側の `links = "proj"` に一本化し、本 driver の build.rs からは PROJ link を出さない。libspatialite 側は `gaiaconfig.h` で `#define PROJ_NEW 1` を出して PROJ 6+ API パス (`proj_create_crs_to_crs` 等) を選択する。
- **shpx-driver-spatialite (v0.6 cycle 4、CI smoke job + docs + 0.6.0 release)**: `.github/workflows/ci.yml` に `bundled-spatialite-smoke` job を追加 (ubuntu-latest、`libsqlite3-mod-spatialite` を apt から除外、`cmake` / `clang` のみ apt 導入、`cargo build -p shpx-cli --features bundled-spatialite --release` 成功 + `cargo test -p shpx-driver-spatialite --features bundled-spatialite` 緑、`SHPX_TEST_SPATIALITE=1` で env-gated 統合テストも実行)。`docs/SPATIALITE.md` に「bundled-spatialite ビルド」節新設 (有効化方法 / vendor 範囲 / ビルドツール / ライセンス制約 / サポート OS / 静的初期化経路 / smoke test) と「内部実装メモ」の `mod_spatialite` ロード経路を bundled / default で 2 経路に分けて訂正。「スコープ外」節から bundled-spatialite 関連 bullet を削除。

### Build

- workspace MSRV は 1.85 据え置き。`bundled-spatialite` feature 経由で `geos-src 0.2` / `link-cplusplus 1` / `cc 1` が build-dep として引かれる (workspace dep として cycle 1-2 で追加済み)。default ビルド (feature 未指定) では追加依存なし。
- workspace `Cargo.toml` の `[workspace.package].version` を `0.5.0` → `0.6.0` へ bump。全 driver / cli は `version.workspace = true` で追従。
- CI (`.github/workflows/ci.yml`): `bundled-spatialite-smoke` job 追加（ubuntu-latest、`libsqlite3-mod-spatialite` apt 不在の cleanroom 環境で bundled CLI build + driver test を緑化することを保証）。既存 `fmt` / `clippy` / `test` job は変更なし。

### Known Issues

- **bundled-spatialite の OS サポートは Linux x86_64 のみ CI 検証**: macOS (Apple Silicon / Intel) / Windows は cycle 当初は best-effort。`cargo build --features bundled-spatialite` をローカルで叩いた場合、cmake と clang/gcc 適合バージョンが揃っていれば動くが、CI で常時検証はしていない。v1.0 の `cargo-dist` 配信時に追加 OS の smoke job を整備する予定。
- **bundled feature 時に `SHPX_SPATIALITE_PATH` env は warn で無視**: bundled で static link されている前提のため、ユーザが手元の system mod_spatialite をロードしたい場合は default (feature 未指定) ビルドを使うか、env を立てずに bundled init に任せる。
- **GEOS / PROJ の version pin は indirect**: `geos-src` / `proj-sys` の最新版が引かれるため、上流のメジャー bump で API 不整合が出る可能性がある。v1.0 までに version pin 戦略を確定する。

## [0.5.0] - 2026-05-03

v0.5 マイルストーン「SpatiaLite」のリリース。`shpx-driver-spatialite` で SpatiaLite (`*.sqlite` / `*.db` / `*.spatialite` / `sqlite://`) の read/write を提供する。`rusqlite` (`bundled` SQLite) + `mod_spatialite` 動的ロード、自前 `shpx-geom::spatialite_blob` コーデック、`AddGeometryColumn` 経由の `geometry_columns` 登録、`GeomFromWKB(?, srid)` でのジオメトリ I/O、`--create-table` 3 種、`--create-index` 3 種 (`Always` / `Auto` で `SELECT CreateSpatialIndex(...)` の R\*Tree)、未登録 EPSG の `spatial_ref_sys` への best-effort `INSERT OR IGNORE`、SpatiaLite ↔ GPKG / SpatiaLite ↔ Shapefile の cross-driver 往復テストまでを含む。`bundled-spatialite` feature 宣言は driver / CLI の Cargo.toml に残るが、本実装は v0.6 に繰り延べ（workspace に `build.rs` の足場が無く、libspatialite が GEOS / PROJ にも依存して vendor 範囲が cycle 1 つに収まらないため）。詳細は `docs/SPATIALITE.md`。

### Added

- **shpx-driver-spatialite (v0.5 cycle 1、基盤と最小往復)**: `crates/shpx-driver-spatialite` を新規追加。`SpatialiteDriver` は `supported_schemes = ["sqlite", "db", "spatialite"]` を宣言し、`Capabilities { read, write, !bulk_load, supports_blob, !supports_decimal, supports_timestamp_tz, string_encoding: Fixed("utf-8") }` を提供する。`conn.rs` で `rusqlite::Connection::open_with_flags` → `load_extension(<path>, Some("sqlite3_modspatialite_init"))` → `SELECT InitSpatialMetadata(1)` (FastInit) を idempotent に発行する流れを確立。reader は `geometry_columns` で geometry 列を解決し `AsBinary(<geom>)` で WKB を取り出す。writer は `--overwrite` のみ対応の最小版で、`AddGeometryColumn` で geometry 列を登録 + 1 トランザクション + 行単位 prepared `INSERT INTO ... GeomFromWKB(?, srid)` で投入する。SQLite の declared type ↔ Arrow `DataType` のマッピング (`type_map.rs`) は GPKG driver と同形だが UInt64 のみ `Error::Schema` で拒否（SQLite INTEGER の上限が i64 のため）。env-gated 統合テスト (`SHPX_TEST_SPATIALITE`) で Point / LineString / Polygon / MultiPoint / MultiLineString / MultiPolygon の geometry 往復、Boolean / Int64 / Float64 / Utf8 の属性往復を 1 件ずつ確認。
- **shpx-geom (`spatialite_blob`)**: SpatiaLite blob (geometry binary) の自前 encode / decode を `spatialite_blob` モジュールに追加。XY のみ対応、Z / M / EMPTY / GeometryCollection は `Error::Geometry` で拒否。`encode` は LE 固定で MBR を WKB 走査から算出、`decode` は LE / BE 双方を読める。ユニットテスト 7 件 (roundtrip / MBR 値 / マーカー検証 / big-endian 互換) で I/O 整合性を担保。v0.5 では writer 経路は SpatiaLite 自身の `GeomFromWKB(?, srid)` に encode を委譲する方針 (自前 encode との実装ずれを回避) で、`spatialite_blob` モジュール自体は将来的な direct-bind 経路や v1.0 以降の Z/M 拡張のための基盤。
- **shpx-driver-spatialite (v0.5 cycle 2a、writer 拡張 + R\*Tree)**: `--create-table=if-not-exists|always|never` を PostGIS と同形セマンティクスで 3 種フル対応 (`Always` は `--overwrite` 無しでも DROP→CREATE する契約、`DiscardGeometryColumn` で `geometry_columns` / R\*Tree からも紐付き行を削除してから DROP)。`--create-index=auto|always|never` を実装し、`Always` / `Auto` (新規 CREATE TABLE 経路) で `LayerWriter::finish()` の最後に `SELECT CreateSpatialIndex(<table>, <geom>)` を発行 (PostGIS の GIST index と同方針で、batch 投入完了後に index を組む方が速いため)。SRID 解決は `--src-crs` > schema field metadata > `apply_on_loss` フォールバック (SRID 0 = SpatiaLite 慣習で unknown) の順。`spatial_ref_sys` の未登録 SRID には `Crs.wkt` → `shpx_geom::epsg_to_wkt1(code)` (4326 / 3857 / 4269 / 6668 同梱) の順で `srtext` を解決し、`INSERT OR IGNORE` で best-effort 登録 (PostGIS と同パターン)。`--overwrite=true && --create-table=never` は `shpx_rdb_common::validate_overwrite_compat` で整合性エラー。env-gated 統合テスト 9 件を `tests/writer_options.rs` に追加 (3 種 × 3 種 + 整合性エラー)。
- **shpx-driver-spatialite (v0.5 cycle 2b、bundled-spatialite を v0.6 へ繰り延べ + ROADMAP 訂正)**: `bundled-spatialite` feature の本実装 (libspatialite を C ソースから vendor して static link する build.rs) を v0.6 マイルストーンへ切り出し、v0.5 はシステム libspatialite (Linux: apt の `libsqlite3-mod-spatialite`、macOS: `brew install libspatialite`) 前提で出荷。理由は workspace に `build.rs` ファイルが一つも無く bundled C ビルドの足場がゼロであること、libspatialite が GEOS / PROJ にも依存して vendor 範囲が cycle 1 つに収まらないため。`bundled-spatialite` feature 宣言は `crates/shpx-driver-spatialite/Cargo.toml` と `crates/shpx-cli/Cargo.toml` に v0.6 予約として残し、有効化しても no-op (システム libspatialite を `load_extension` で見る挙動と同じ)。`docs/ROADMAP.md` の v0.5 / v0.6 セクションを訂正し、URI scheme は v0.5 で `sqlite` を SpatiaLite が専有することを確定 (`?mod_spatialite=true` フラグ運用は採用しない、content-sniffing は v1.0 以降)。
- **shpx-driver-spatialite (v0.5 cycle 3、cross-driver e2e + docs + 0.5.0 release)**: 完了基準テスト `tests/cross_driver_roundtrip.rs` を追加し、SpatiaLite ↔ GPKG (Point / LineString / Polygon / MultiPolygon) と SpatiaLite ↔ Shapefile (Point / Utf8 / Float64 / Boolean、SHP の DBF Numeric は Float64 へ降格するため整数列は scope 外) の往復が e2e で機能することを確認。`docs/SPATIALITE.md` を新設 (POSTGIS.md / SQLSERVER.md と同形構造)、`docs/DATA_TYPES.md` の `SQLite/GPKG` 列を `GPKG` / `SpatiaLite` の 2 列に分割、`docs/DESIGN.md` の URI scheme 表を訂正、`docs/CRS.md` / `docs/CONTRIBUTING.md` / `README.md` を更新。

### Internal

- **shpx-rdb-common 新設**: PostGIS / SQL Server の 2 driver で `options.rs` / `util.rs` / `writer.rs` に重複していた純粋ヘルパー（`percent_decode` / `query_pairs` / `split_qualified` / `resolve_table_name` / `validate_overwrite_compat` / `apply_on_loss` / `merge_crs` / `resolve_epsg_srid` / `driver_err` / `driver_msg` / `primitive`）を共通 crate `crates/shpx-rdb-common/` に抽出した。各 driver の API 公開面・エラーメッセージ・tracing target は変更なしで、CLI ユーザー視点の挙動には影響しない。次の RDB driver (MySQL 等) を追加する際の boilerplate 削減と、既存 2 driver の挙動を 1 箇所で揃える目的。`tracing::warn!(target: ...)` の target は const 要求のため、driver 側に `tracing::warn!` 呼び出しごとクロージャで残し、`shpx_rdb_common::apply_on_loss(kind, field, on_loss, warn_fn)` がそれを警告経路でだけ呼び出す設計とした。SpatiaLite driver もこの crate を再利用する (SQLite に schema 概念が無いため `split_qualified` / `resolve_table_name` は呼ばない点が差分)。詳細は `docs/CONTRIBUTING.md` の「RDB driver を追加する場合」節と `docs/DESIGN.md` のリポジトリ構成図を参照。

### Build

- workspace MSRV は 1.85 据え置き。新規依存はなし (`rusqlite` の `bundled` / `blob` / `chrono` / `load_extension` features を SpatiaLite driver で利用するが、いずれも GPKG driver で既に有効化済み)。
- CI (`.github/workflows/ci.yml`): test job (Linux) に `apt-get install -y libsqlite3-mod-spatialite` step を追加し、`SHPX_TEST_SPATIALITE=1` を環境変数で渡す。`crates/shpx-driver-spatialite/tests/{roundtrip,writer_options,cross_driver_roundtrip}.rs` は env 未設定なら eprintln + return で skip するため、SpatiaLite extension が無いローカル環境でも `cargo test --workspace` は緑のまま。
- macOS ローカル開発では `brew install libspatialite` 後に `SHPX_SPATIALITE_PATH=/opt/homebrew/lib/mod_spatialite.dylib` を立てる必要がある (Homebrew は標準のライブラリ検索パスに `mod_spatialite.dylib` を配置しないため)。

### Known Issues

- **`bundled-spatialite` は v0.6 で本実装**: 現状の feature 宣言は no-op で、有効化してもシステム libspatialite を `load_extension` で見る挙動と同じ。配布バイナリ向けの static link は v0.6 の `build.rs` 整備で対応する。それまでは `cargo install shpx --features bundled-spatialite` を実行しても extension のロードはランタイム経路を辿る。
- **SpatiaLite blob の Z / M / EMPTY / GeometryCollection 非対応**: v0.5 は XY のみ。`shpx-geom::wkb` / `shpx-geom::spatialite_blob` 双方の制限であり、PostGIS / SQL Server / GPKG とも同じ制限を共有する (v1.0 以降で拡張予定)。
- **reader filtering (`--where` / `--select` / `--query`) 未対応**: PostGIS の cycle 3a と同様の機能は v0.5+ の Future work。

## [0.4.0] - 2026-05-03

v0.4 マイルストーン「SQL Server」のリリース。`shpx-driver-sqlserver` で Microsoft SQL Server / Azure SQL の read/write を提供し、staging テーブル経由 bulk writer (案 B、`docs/DESIGN.md` L.219-)、`--create-table` 3 種、`--create-index=Always` での SPATIAL INDEX 生成、`?geom_type=geometry|geography` 切替、CI で docker mssql 経由統合テストまでを含む。**1000万行ベンチの完了基準値は Linux x86_64 環境で実測予定**（Apple Silicon の Rosetta/QEMU emulation 経由は参考値止まりのため）。詳細は `docs/SQLSERVER.md`。

### Added

- **shpx-driver-sqlserver (v0.4 cycle 1)**: SQL Server の最小 reader / writer。`mssql://user:pass@host:port/db?table=schema.name&geom_type=geometry|geography` URL で接続し、`tiberius 0.12` (`tds73` / `rustls` / `chrono` / `rust_decimal` features) を採用。driver crate 内 `OnceLock<tokio::runtime::Runtime>` で multi-thread runtime を 1 個共有して `block_on` で同期化（PostGIS と同形）。reader は `INFORMATION_SCHEMA` ではなく `sys.columns` + `sys.types` で列メタを取り、geometry 列は `[col].STAsBinary() AS [col], [col].STSrid AS [col__shpx_srid]` の併走 SELECT で WKB と SRID を一括取得する（SQL Server には PostGIS の `geometry_columns` view 相当が無いため、空テーブルでは SRID が取れない点に注意）。writer batch は `INSERT INTO ... VALUES (@P1, ..., {geometry|geography}::STGeomFromWKB(@PN, @PS))` の prepared INSERT で行単位投入。サポート型: Boolean / Int16-64 / Float32-64 / Decimal128 / Utf8 / Binary / Date32 / Timestamp(_, None|UTC) / geometry。`--where` / `--select` / `--query` reader 拡張は v0.5+。詳細は `docs/SQLSERVER.md` 参照。
- **shpx-driver-sqlserver (v0.4 cycle 2、staging bulk 案 B)**: `BulkLoadWriter` 実装。`Capabilities::bulk_load = true` に切替。tiberius は geometry/geography UDT の直接 bind を許さず TVP も非対応のため、接続スコープ local temp テーブル `#shpx_stage_<short_uuid>` (16 桁、自動 GC) に WKB + SRID を `tiberius::Client::bulk_insert` 経由で流し、`INSERT INTO target SELECT ..., {geometry|geography}::STGeomFromWKB(...) FROM #stage` で型変換しながら確定テーブルに転記する。chunk ごとに `BEGIN TRAN` / `COMMIT TRAN` を挟むことで tempdb log truncation を可能にし、10M 行投入でも tempdb 溢れが起きない設計。chunk size は `SHPX_MSSQL_BULK_CHUNK` env で override 可（既定 100,000、bench 時のみ 1,000,000 に上げる運用）。decimal は `rust_decimal::Decimal` 経由（tiberius 0.12 の生 Numeric write は scale 0 以外でバグがあるため `rust_decimal` feature 必須）。datetime2 / datetimeoffset / Date は tiberius の `IntoSql` impl をそのまま利用。
- **shpx-driver-sqlserver (v0.4 cycle 3a、writer 拡張)**: `--create-table=if-not-exists|always|never` を PostGIS と同形セマンティクスで 3 種フル対応（`Always` は `--overwrite` 無しでも DROP→CREATE する契約）。`--create-index=Always` で `CREATE SPATIAL INDEX [...] WITH (BOUNDING_BOX = (xmin, ymin, xmax, ymax))` を発行。geometry の BOUNDING_BOX は同梱表（4326 全球 / 3857 Web Mercator）から解決し、未知 SRID は明示エラー。geography は BOUNDING_BOX 不要。**`--create-index=Auto` は no-op**（PostGIS の Auto と挙動が違う点に注意）— SQL Server の SPATIAL INDEX は `geometry` 列で BOUNDING_BOX が必須で未知 SRID では失敗するため、暗黙生成は安全側に倒す。SRID 解決は `--src-crs` > schema field metadata > `apply_on_loss` フォールバックの順で、geometry の fallback は SRID 0、geography は 4326（geography は valid な geographic CRS が必須のため）。`--overwrite=true && --create-table=never` は driver 側で整合性エラー。env-gated 統合テスト 7 件を `tests/writer_options.rs` に追加。
- **shpx-driver-sqlserver (v0.4 cycle 3b、bench infra + 完了基準テスト)**: `crates/shpx-driver-sqlserver/benches/{bulk_insert.rs, gen.rs}` で criterion ベンチ harness を整備（`SHPX_TEST_SQLSERVER_URL` env-gate、`SHPX_BENCH_ROWS` で行数切替、`target/bench-data/` にキャッシュ生成）。`scripts/bench-vs-ogr-mssql.sh` は同 Parquet を shpx と `ogr2ogr -f MSSQLSpatial` 双方に流して `/usr/bin/time -p` の wall-clock 中央値を比較し、完了基準を `shpx_secs <= 1.667 * ogr_secs` (= shpx が ogr2ogr の 60% 以上の速度) で判定する。`tests/bulk_roundtrip.rs` に `bulk_all_types_together`（型網羅 bit-identical、tiberius 0.12 の既知不整合により一時的に `#[ignore]`、cover は単独テストで担保）と `bulk_geography_all_geom_types`（Point/LineString/CCW Polygon を geography で書ける確認）を追加。

### Build

- workspace MSRV は 1.85 据え置き。`tiberius` (default-features 切り、`tds73` / `rustls` / `chrono` / `rust_decimal` 有効化) / `tokio-util` (`compat`) / `rust_decimal` / `uuid` (`v4`) を `[workspace.dependencies]` に追加。
- `docker-compose.yml` に mssql service 追加（`mcr.microsoft.com/mssql/server:2022-latest`、Apple Silicon では `platform: linux/amd64` で emulation 起動、`MSSQL_MEMORY_LIMIT_MB=2048` で SA メモリ上限を明示）。image はユーザ DB を自動作成しないため、初回起動後に `docker exec shpx-mssql /opt/mssql-tools18/bin/sqlcmd ... -Q "CREATE DATABASE shpx_test"` を 1 度実行する。
- CI (`.github/workflows/ci.yml`): test job に `services.mssql` を追加し、`SHPX_TEST_SQLSERVER_URL=mssql://sa:Shpx_test_pw1!@localhost:1433/shpx_test` を環境変数で渡す。`Create shpx_test database in mssql` step で `IF DB_ID(...) IS NULL CREATE DATABASE` を冪等に発行。env 未設定時は eprintln + return で skip するため、SQL Server が無いローカル環境でも `cargo test` は緑のまま。

### Known Issues

- **tiberius 0.12 bulk encode の既知不整合**: 多列スキーマ (10+ 列) で `decimal(p, s)` と複数の `varbinary(max)` 列、または `datetime2` / `datetimeoffset` 列が混在すると、特定の列で `Token error: 'Invalid column type from bcp client'` を踏むケースがある。完了基準の各型 (decimal(38, 10) / timestamptz / bytea) は単独テストで bit-identical を確認済みで、`tests/bulk_roundtrip.rs::bulk_all_types_together` のみ一時的に `#[ignore]`。tiberius 上流に再現報告予定。
- **`--create-index=Always` は事前 PK 必須**: SQL Server の `CREATE SPATIAL INDEX` は仕様で clustered primary key を要求する。shpx 汎用 driver は `CREATE TABLE` で PK を勝手に付与しないため、`--create-index=Always` を使うには利用者が事前に PK 付きテーブルを作成して `--create-table=never` で append する運用になる。`--create-index=Auto` は no-op で安全側。
- **完了基準ベンチ値は Linux x86_64 で取得予定** [v1.0 cycle 1 で取得済み]: 当初 100k 行の smoke (Apple Silicon emulation) で shpx 1.28s しか取れていなかったが、v1.0 cycle 1 で `.github/workflows/bench-smoke-mssql.yml` を整備し、ubuntu-latest 上で 10M 行 × 3 runs の median 138.53s (≈ 72k rows/s) を確認、tempdb 溢れなしを担保。

## [0.3.0] - 2026-04-25

v0.3 マイルストーン「PostGIS」のリリース。`shpx-driver-postgis` で PostgreSQL + PostGIS の read/write を提供し、COPY BINARY 経路の `BulkLoadWriter` と Decimal128 / timestamptz / bytea / EWKB の bit-identical 往復、`--where` / `--select` / `--query` reader、`--create-table` / `--create-index` writer、未登録 EPSG の `spatial_ref_sys` 自動 INSERT までを含む。10M 行 × 10 属性ベンチ（`scripts/bench-vs-ogr.sh`）で `ogr2ogr` の約 2.2 倍の速度（shpx 28.46 s / ogr2ogr 62.84 s / 比 0.453）を計測し、ROADMAP の v0.3 完了基準（`shpx ≤ 2.0 × ogr2ogr`）をクリア。詳細は `docs/POSTGIS.md` の Benchmark 節。

### Performance

- **shpx-driver-postgis (v0.3 cycle 3c)**: `crates/shpx-driver-postgis/benches/copy_binary.rs` + `gen.rs` で criterion ベンチ harness を整備（10 列 × 任意行数の合成 Parquet を `target/bench-data/` にキャッシュ生成、`SHPX_BENCH_ROWS` で行数切替、`SHPX_TEST_PG_URL` env-gate）。`scripts/bench-vs-ogr.sh` は同 Parquet を shpx と ogr2ogr 双方に流して `/usr/bin/time -p` の wall-clock 中央値を比較し、完了基準を `shpx_secs <= 2.0 * ogr_secs` で判定する。`.github/workflows/bench-smoke.yml` を `workflow_dispatch` 専用で追加し、CI 上で bench infra の smoke 確認が可能。`tests/bulk_roundtrip.rs::bulk_all_types_together` でベンチスキーマと 1:1 揃った 10 列同居 1k 行の bit-identical 往復テストを追加し、bench データの回帰検出器を兼ねる。

### Added

- **shpx-core (`WriteOpts` 拡張, v0.3 cycle 3b)**: `CreateTable { IfNotExists, Always, Never }` と `CreateIndex { Auto, Always, Never }` の 2 enum を `opts` モジュールに追加し、`WriteOpts` に同名フィールドを追加。RDB driver（PostGIS など）が CREATE TABLE / CREATE INDEX を制御するための受け口。ファイル driver は無視するため後方互換は保たれる。Default は `IfNotExists` / `Auto`。
- **shpx-cli (`convert --create-table / --create-index`, v0.3 cycle 3b)**: `convert` サブコマンドに `--create-table=if-not-exists|always|never`（既定 `if-not-exists`）と `--create-index=auto|always|never`（既定 `auto`）を追加。`--overwrite` とは直交し、`--overwrite && --create-table=never` は driver 側 `ResolvedWriteOpts::resolve` で整合性エラーになる。
- **shpx-driver-postgis (v0.3 cycle 3b)**: writer 拡張。`PostgisWriter::open` で `--create-table` の値に応じて `CREATE TABLE IF NOT EXISTS` / `CREATE TABLE` を切り替え、`Never` は `pg_class` で存在検証してから既存テーブルへ append する（無ければ `Error::Driver`）。GIST index は `LayerWriter::finish()` で `CREATE INDEX IF NOT EXISTS idx_<table>_<geom> ON <qualified> USING GIST (<geom_col>)` を発行し、bulk 経路では COPY 完了後に発行する（COPY 前に index があると遅くなる定石）。SRID 解決時に `spatial_ref_sys` を probe し、欠けていれば `Crs.wkt`（元データ由来、WKT1/WKT2 どちらでも）→ `shpx_geom::epsg_to_wkt1(code)` 同梱マップの順で `srtext` を解決し、`INSERT ... ON CONFLICT (srid) DO NOTHING` で best-effort 登録する。WKT が解決できない場合は INSERT スキップ（PostGIS の geometry 列定義は `spatial_ref_sys` 行が無くても CREATE/INSERT できるため）。env-gated 統合テスト 9 件を `tests/writer_options.rs` に追加。
- **shpx-core (`ReadOpts` 拡張, v0.3 cycle 3a)**: `where_clause` / `select` / `query` の 3 フィールドを追加。RDB driver（PostGIS など）が SQL に埋め込むための CLI 引数受け口。ファイル driver は無視するため後方互換は保たれる。
- **shpx-cli (`convert --where / --select / --query`, v0.3 cycle 3a)**: `convert` サブコマンドに `--where '<sql>'` / `--select c1,c2,...` / `--query 'SELECT ...'` を追加。`--query` は他 2 つと clap の `conflicts_with_all` で排他。`--select` は `value_delimiter = ','` で複数列を 1 引数で受ける。ファイル URI に対してこれらが指定された場合は tracing 警告で告知し、driver は静かに無視する。
- **shpx-driver-postgis (v0.3 cycle 3a)**: reader を 「table モード」と「query モード」の 2 経路に分割。table モードでは `?table=` で解決した完全修飾名に `--where` / `--select` を埋め込み、`SELECT col1, ..., ST_AsEWKB(geom) FROM "schema"."table" [WHERE <sql>]` を生成する。query モードではユーザ SQL を `SELECT * FROM (<query>) AS shpx_q LIMIT 0` でサブクエリ化して `tokio_postgres::Statement::columns()` から列メタを取り、`Type::name() == "geometry"|"geography"` で geometry 列を検出して本番 SQL を再構築する。SRID 解決は table モードでは `geometry_columns` view → 先頭 `ST_SRID()` の 2 段、query モードはサブクエリ経由の先頭 `ST_SRID()` のみ。geometry 列を含まない `--select` / `--query` は `Error::Driver` で停止し、`--query` 中の `;` も同様に停止する。
- **shpx-driver-postgis (v0.3 cycle 2)**: PostgreSQL の binary COPY format を自前エンコードする `BulkLoadWriter` 経路。`crates/shpx-driver-postgis/src/copy_binary.rs` に `BulkRowEncoder` と各型の big-endian エンコーダ（bool / int2-8 / float4-8 / text / bytea / date / timestamp / timestamptz / numeric / geometry-EWKB）を実装。`tokio_postgres::CopyInSink<Bytes>` で `COPY <table> (<cols>) FROM STDIN BINARY` に流し込み、複数 `RecordBatch` をまたいで 1 接続 = 1 COPY セッションで送る。`Capabilities::bulk_load = true` / `supports_decimal = true` に切替。
- **shpx-driver-postgis (Decimal128)**: Arrow `Decimal128(p, s)` ↔ PG `numeric(p, s)` を双方向対応。binary 表現は NBASE=10000 の `PgNumeric { ndigits, weight, sign, dscale, digits[] }` で、`PgNumeric` は `tokio_postgres::types::ToSql` を独自実装し batch / bulk 両経路で同じ encode 結果を共有する。reader は PG `NUMERIC` OID + `pg_attribute.atttypmod` から `(p, s)` を復元（typmod=-1 のときは `(38, 0)` フォールバック）。decimal(38, 10) bit-identical 往復テスト追加。
- **shpx-cli (`--insert-mode=auto|bulk|batch`)**: `convert` サブコマンドに insert mode を追加。既定 `auto` は driver の `Capabilities::bulk_load` が true なら bulk、そうでなければ batch（silently fallback）。`bulk` 明示時は非対応 driver でエラー。`batch` 明示時は常に `LayerWriter::write_batch` 経路。PostGIS 以外の driver は現状 batch 一択のため挙動は変わらない。
- **shpx-core**: `Driver::open_bulk_write` メソッドを default impl (`Ok(None)`) 付きで `Driver` trait に追加。`BulkLoadWriter` を実装する driver はこれを override して `Box<dyn BulkLoadWriter>` を返す。CLI 側は `Capabilities::bulk_load` でゲートしてから呼び出す。
- **shpx-driver-postgis (v0.3 cycle 1)**: PostgreSQL + PostGIS の最小 reader / writer。`pg://` / `postgres://` / `postgresql://` URL で接続し、`?table=schema.name` または環境変数 `SHPX_PG_TABLE` でテーブルを指定する。`tokio-postgres` (`with-chrono-0_4` feature) を採用し、driver crate 内 `OnceLock<tokio::runtime::Runtime>` で multi-thread runtime を 1 個共有して `block_on` で同期化する。reader は `SELECT ST_AsEWKB(geom), ... FROM tbl` を発行し、writer は `--overwrite` で `DROP TABLE IF EXISTS` → `CREATE TABLE` → 1 トランザクション + prepared `INSERT INTO ... VALUES (..., ST_GeomFromEWKB($N))` を行う。サポート型: Boolean / Int16-64 / Float32-64 / Utf8 / Binary / Date32 / Timestamp(_, None|UTC) / geometry。SRID は `Crs::epsg_code()` または `geometry_columns` view → 先頭行 `ST_SRID()` の順で解決する。`--where`/`--select`/`--query`、`--create-table`、GIST index、未登録 EPSG の `spatial_ref_sys` 自動 INSERT、Z/M は cycle 3 で対応。詳細は `docs/POSTGIS.md` 参照。
- **shpx-geom**: PostGIS EWKB (Extended WKB) の encode/decode を `ewkb` モジュールに追加。`encode_with_srid` で標準 WKB に SRID flag (`0x20000000`) を立て SRID i32 を挿入、`strip_srid` / `decode` で EWKB から SRID と標準 WKB を分離する。Z/M flag は cycle 1 では `Error::Geometry` で拒否する。
- **shpx-core**: `Uri::from_path` に URL スキーム検出を追加。先頭が `<scheme>://` 形式なら scheme を抽出し、`pg`/`postgres`/`postgresql` は `pg` に正規化する。ローカルパスの拡張子推論は従来通り。`Uri::is_url()` ヘルパ追加。

### Changed

- **shpx-cli**: `ConvertArgs`/`InfoArgs`/`SchemaArgs` の `src` / `dst` を `PathBuf` から `String` に変更。`pg://...` 等の URL を OS パスとして解釈すると壊れるため（特に Windows のドライブレター扱い）。`commands/{convert,info,schema}.rs` で `Uri::from_path(args.src)` のまま渡す。

### Build

- workspace MSRV は 1.85 据え置き。`tokio` / `tokio-postgres` / `postgres-types` / `bytes` / `futures-util` を `[workspace.dependencies]` に追加。
- `docker-compose.yml` をリポジトリルートに追加（ローカル開発用 PostGIS）。
- CI (`.github/workflows/ci.yml`): test job に `services.postgis` を追加し、`SHPX_TEST_PG_URL=pg://shpx:shpx@localhost:5432/shpx_test` を環境変数で渡す。`shpx-driver-postgis/tests/roundtrip.rs` および cycle 2 で追加した `tests/bulk_roundtrip.rs` は env 未設定なら eprintln + return で skip するため、PostGIS が無いローカル環境でも `cargo test` は緑のまま。

## [0.2.0] - 2026-04-25

v0.2 マイルストーン「GPKG / GeoJSON / CSV / FlatGeobuf + Reprojection」のリリース。

### Added

- **shpx-driver-fgb**: FlatGeobuf (`.fgb`) reader/writer。公式 Rust 実装 `flatgeobuf` 6.0 (BSD-2-Clause) を採用し、`default-features = false` で HTTP feature を排除。geometry の入出力は `geozero` 経由で WKB ↔ FGB FlatBuffers を変換する。Bool / Byte / UByte / Short / UShort / Int / UInt / Long / ULong / Float / Double / String / Binary / DateTime の各 ColumnType に対応。Date32 と Timestamp は ISO8601 文字列で `DateTime` 列に書き、reader 側は観測値の形式から `Date32` か `Timestamp(Microsecond, UTC)` に絞り込む。CRS は EPSG コード優先 + WKT2 フォールバック。**packed Hilbert R-Tree インデックスは出力しない** (`index_node_size=0` 固定)。Z/M / GeometryCollection / `select_bbox` / null geometry / `Json` 列の構造化保持は未対応 (詳細は `docs/FGB.md` の「制限」を参照)。
- **shpx-driver-csv**: WKT 列付き CSV/TSV reader/writer（`.csv` / `.tsv`）。geometry 列は `geometry` / `geom` / `wkt` / `the_geom` のいずれか、または `SHPX_CSV_GEOMETRY_COLUMN` 環境変数で明示。geometry 以外の列は全て `Utf8` として読み書きする（型推定なし）。CSV 固有オプションは暫定で環境変数経由（`SHPX_CSV_*`）。詳細は `docs/CSV.md` を参照。
- **shpx-driver-geojson**: GeoJSON FeatureCollection (`.geojson`) と GeoJSON Lines / NDJSON (`.geojsonl` / `.ndjson` / `.jsonl`) の reader/writer。属性 (properties) の JSON 型を Arrow 列に推論（Bool / Int64 / Float64 / Utf8、混在は昇格）。出力は RFC 7946 §4 準拠の EPSG:4326 固定で、非 WGS84 入力は内部 `Reprojector` で自動変換する（cycle 5）。旧仕様の top-level `crs` メンバ（`urn:ogc:def:crs:EPSG::NNNN` / `urn:ogc:def:crs:OGC:1.3:CRS84`）の解釈に対応。GeometryCollection と Z/M 座標は未対応。詳細は `docs/GEOJSON.md` を参照。
- **shpx-driver-gpkg**: GeoPackage 1.3 (`.gpkg`) reader/writer。`rusqlite` (`bundled` SQLite) ベースで `gpkg_spatial_ref_sys` / `gpkg_contents` / `gpkg_geometry_columns` を初期化。geometry blob は envelope_type=0 / Standard / LE 固定で書き出し、reader は全 envelope_type と LE/BE を読み飛ばす。テーブル名は URI クエリ `?table=<name>` または環境変数 `SHPX_GPKG_TABLE` / `SHPX_GPKG_OUT_TABLE` で指定可能（未指定で feature テーブル単一なら自動採用、複数なら候補を列挙してエラー）。CRS は EPSG コード優先 + WKT1 フォールバック。Z/M 座標、空間インデックス、複数レイヤ append、`gpkg_extensions` は未対応（`docs/GPKG.md` の Future work 参照）。
- **shpx-geom**: GeoPackage Binary header の encode/decode を `gpkg_blob` モジュールに追加。GPKG/SpatiaLite で再利用できる独立コーデック。
- **shpx-geom**: WKT (Well-Known Text, OGC SFA 1.2.1) の encode/decode を `wkt` モジュールに追加。XY のみ対応、`EMPTY` は未サポート。
- **shpx-geom: Reprojector**: `proj 0.28` クレート（システム libproj を pkg-config で検出）経由で WKB ↔ WKB の座標変換を行う。`Proj::new_known_crs` が `proj_normalize_for_visualization` を適用するため lat-lon 系も traditional XY 順で扱える。`Proj` は `!Send` のため `Reprojector` は CRS spec 文字列のみ保持し、`Proj` は `(src_spec, dst_spec)` キーで thread-local キャッシュする。`shpx_geom::for_each_coord_mut` で `Geom` の各 variant を走査する補助関数も追加。
- **shpx-cli (`--reproject`)**: `convert` サブコマンドに `--reproject <SPEC>` を追加。`EPSG:xxxx` / WKT2 / proj-string / PROJJSON を受理する。入力 CRS が解決できない場合は `--src-crs` の指定を促すエラーで停止する。同一 CRS 指定時は no-op パスにフォールスルー。出力先 driver に渡す `Crs` 引数と Arrow schema の field metadata の双方を target に揃えるため、出力ファイルの CRS タグも反映される。
- **shpx-cli (`bundled-proj` feature)**: 配布用単一バイナリ（`cargo-dist`）向けに `shpx-cli` の `bundled-proj` feature を追加。有効化すると `proj` クレートが libproj/SQLite を C ソースから static link する（`cmake` / `clang` 必須）。default ビルドはシステム libproj を pkg-config 経由で利用するため、これらの追加ビルドツールを要求しない。

### Changed

- workspace MSRV を 1.79 → 1.85 に引き上げ。FlatGeobuf ドライバが依存する `flatgeobuf` 6.0 が `rust-version = 1.85` を要求するため。CI matrix も `["1.85", "stable"]` に更新。
- **GeoJSON writer**: 非 EPSG:4326 入力でのエラー停止を内部 `Reprojector` 経由の自動 WGS84 変換に置き換え（RFC 7946 §4 準拠）。入力 CRS が解決できない場合のみ `Error::Crs` で停止する挙動は維持。

### Build

- CI (`.github/workflows/ci.yml`): clippy / test ジョブで `libproj-dev` / `pkg-config` を apt インストール。`proj` クレートのビルドに必要。

## [0.1.0] - 2026-04-25

v0.1 マイルストーン「コア骨格 / SHP ↔ GeoParquet PoC」のリリース。

### Added

- **shpx-core**: `Driver` / `LayerReader` / `LayerWriter` / `BulkLoadWriter` トレイト、`Schema` / `Capabilities` / `Crs` / `Uri` / `ReadOpts` / `WriteOpts` / `OnLoss` 型。
- **shpx-geom**: WKB encoder/decoder（little/big endian、Point/LineString/Polygon/MultiPoint/MultiLineString/MultiPolygon）、WKT1 `.prj` からの EPSG コード抽出、最小 PROJJSON 生成。
- **shpx-driver-shp**: Shapefile reader/writer。`.shp`/`.shx`/`.dbf`/`.prj`/`.cpg` サイドカー対応、DBF cpg 文字エンコーディング切替（utf-8/cp932/latin-1）、Decimal128 / Date32 / Utf8 の保全。
- **shpx-driver-parquet**: GeoParquet 1.0 reader/writer。Arrow `Binary` 列に WKB を格納、KeyValue メタの `geo` JSON で CRS（PROJJSON）と geometry_type を伝搬。
- **shpx-cli**: `shpx convert <src> <dst>` サブコマンド（`--overwrite` / `--on-loss` / `--encoding` / `--batch-size` / `--src-crs`）、`shpx info <src>` サブコマンド。`-v`/`-vv`/`-vvv` で tracing ログレベル制御、`RUST_LOG` も尊重。
- 拡張子から driver を推論する static レジストリ（v0.2 で `inventory` ベースに移行予定）。
- CI: `cargo fmt --check` / `cargo clippy --all-targets -- -D warnings` / `cargo test --workspace` を MSRV 1.79 と stable の 2 toolchain で実行。

### Notes

- CRS は EPSG コードのみで保持・伝搬する。`--reproject` による座標変換は v0.2 で `proj` クレート統合とともに実装予定。
- PostGIS / SQL Server / SpatiaLite / GeoPackage / GeoJSON / FlatGeobuf / CSV は後続マイルストーン (v0.2–v0.5) で対応する。
- ライセンスは v1.0 までに最終決定する（MIT / Apache-2.0 dual を想定）。

[Unreleased]: https://github.com/jumboly/shpx/compare/v1.0.0...HEAD
[1.0.0]: https://github.com/jumboly/shpx/compare/v0.8.0...v1.0.0
[0.8.0]: https://github.com/jumboly/shpx/compare/v0.7.0...v0.8.0
[0.7.0]: https://github.com/jumboly/shpx/compare/v0.6.0...v0.7.0
[0.6.0]: https://github.com/jumboly/shpx/compare/v0.5.0...v0.6.0
[0.5.0]: https://github.com/jumboly/shpx/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/jumboly/shpx/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/jumboly/shpx/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/jumboly/shpx/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/jumboly/shpx/releases/tag/v0.1.0
