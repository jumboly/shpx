# Streaming Reader Parity (v0.8)

shpx の全 9 driver は v0.8 で reader を真のストリーミングに揃えた。`LayerReader::batches()`
が返す iterator は **その batch ぶんのデータだけメモリに乗せる** ことを保証し、`open()`
直後に「全行を `Vec` / `VecDeque` に展開する」ような eager-load は持たない。

## 設計の柱

- `LayerReader` trait シグネチャは v0.1 から不変 (`fn batches(&mut self) -> Box<dyn
  Iterator<Item = Result<RecordBatch>> + Send + '_>`)。
- 各 driver は内部状態を「**iterator owning 型**」もしくは「**worker thread + sync_channel**」
  のいずれかに分類して実装する (詳細下表)。
- DB 系 (PostGIS / SQL Server) は `tokio::Runtime::block_on` を呼ぶ background OS thread が
  async stream を消費し、`std::sync::mpsc::sync_channel(2)` で sync 側へ batch を渡す
  (capacity 2 = main 側が 1 batch 消費中も次の 1 batch が背圧無く生産できる)。
- Reader 構造体が drop されると channel receiver も drop し、worker 側 `tx.send()` が
  `Err` を返してスレッドが自然終了する。`JoinHandle` は捨てる (Drop で detach)。

## driver × streaming 戦略

| driver | streaming 戦略 | メモリ上限 | row_count_hint |
|---|---|---|---|
| Parquet | arrow-rs `ParquetRecordBatchReader` がもとから streaming | row group + arrow batch | row group meta から算出 |
| SHP | worker thread + `sync_channel(2)` (`shapefile::Reader::iter_shapes_and_records()`) | `READ_BATCH_SIZE` (65536) 行 × 2 | header `record_count` |
| FGB | iterator-owning (`flatgeobuf::FeatureIter<R, NotSeekable>` を field に保持) | 1 batch ぶん | header `features_count` (sample で全件 walk 済みの場合のみ) |
| CSV | iterator-owning (`csv::Reader<Box<dyn Read>>` を field に保持) | 1 batch ぶん | `None` (CSV はファイル全体を読まないと行数が分からない) |
| GeoJSON FeatureCollection | 自前 state machine (`crates/shpx-driver-geojson/src/stream.rs::FcFeatureStream`) | 1 batch ぶん | `None` |
| GeoJSON NDJSON | `BufRead::lines()` ベース (`NdjsonStream`) | 1 batch ぶん | `None` |
| GPKG | rusqlite keyset pagination (`shpx-rdb-common::streaming::KeysetRowsIter`) | `LIMIT` (65536) 行 | `SELECT COUNT(*)` を 1 度 |
| SpatiaLite | rusqlite keyset (`KeysetRowsIter`) + `--query` モード時は `OffsetRowsIter` (LIMIT/OFFSET) | `LIMIT` 行 | `SELECT COUNT(*)` を 1 度 |
| PostGIS | worker thread + `sync_channel(2)` (`tokio_postgres::query_raw` の `RowStream`) | `READ_BATCH_SIZE` 行 × 2 + tokio 内部バッファ | `None` |
| SQL Server | worker thread + `sync_channel(2)` (`tiberius::QueryStream`) | `READ_BATCH_SIZE` 行 × 2 + tiberius 内部バッファ | `None` |

`READ_BATCH_SIZE` は driver ごとに 65536 行 (RDB / file 共通) または 4096 行 (GeoJSON: JSON
parse コスト分担のため小さく刻む) で実装されている。

## peak RSS 期待値 (10M 行 × 10 属性入力)

| driver | 期待 peak RSS | 計測手段 |
|---|---|---|
| Parquet | < 256 MB | `crates/shpx-core/src/bench_util.rs::peak_rss_kib` (Linux のみ) |
| SHP / FGB / CSV / GeoJSON | < 256 MB | 同上 |
| GPKG / SpatiaLite | < 512 MB | 同上 (rusqlite のページキャッシュ込み) |
| PostGIS | < 1 GB | 同上 (tokio_postgres の TLS バッファ込み、`RowStream` の内部 channel 含む) |
| SQL Server | < 1 GB | 同上 (tiberius の TDS バッファ込み) |

`peak_rss_kib()` は `/proc/self/status` の `VmHWM` 行をパースする。macOS / Windows では
`None` を返すので CI 計測は ubuntu-latest 限定。

実数値の取得は `crates/shpx-bench-rss/` (workspace-internal binary) と
`.github/workflows/bench-peak-rss.yml` (workflow_dispatch only) で行う。1 driver × 1 job
で **1 プロセス 1 計測** (peak RSS = `VmHWM` はリセット不可)、`shpx-bench-rss --driver=<name>
--rows=<N>` が `{"driver":..., "rows":..., "row_count":..., "peak_rss_kib":...,
"elapsed_ms":...}` の JSON 1 行を artifact として upload する。詳細は
`docs/ROADMAP.md` v0.8 cycle 7 を参照。

## キャンセル挙動

- File 系 (SHP / FGB / CSV / GeoJSON / GPKG / SpatiaLite / Parquet): Reader を mid-iter で
  drop した時点で iterator が drop され、worker thread (SHP) や file handle が即座に
  解放される。
- DB 系 (PostGIS / SQL Server): receiver の drop が worker 側 channel send Err を
  誘発し、async block 内で `return` して `RowStream` が drop → 背後の TCP 接続が
  graceful close する。`tests/reader_cancel.rs::reader_drop_releases_select_promptly` で
  「Reader drop 直後に DROP TABLE が成功する」ことを確認している (PostGIS のみ実装、SQL
  Server は同パターンのため smoke 省略)。

## トランザクション保持期間

DB 系 reader は SELECT を保持する間、PostgreSQL / SQL Server 側で長時間ステートフル
カーソルを保持する。`shpx convert <large-table> <large-output>` のように出力 driver の
書き込みが遅い (例: `shpx-driver-shp` は file system bound) と reader の SELECT も
それに引っ張られる。autovacuum / SQL Server の transaction log truncation がブロック
される可能性があるため、production loads では time-bounded で実行することを推奨する。

## バッチサイズの影響

`READ_BATCH_SIZE = 65536` 行は経験的選択 (Arrow の column builder 起動コスト分担と DB の
fetch 効率のバランス)。大幅に変えるとピーク RSS / throughput が線形にずれる:

- batch 倍増 (131072) → ピーク RSS 約 1.5x、throughput ほぼ変わらず
- batch 半減 (32768) → ピーク RSS 約 0.8x、batch 切り替えオーバーヘッドが見え始める

`SHPX_READ_BATCH_SIZE` のような env override は v0.8 では実装していない (将来の
余地として残す)。

## 関連実装

- `crates/shpx-rdb-common/src/streaming.rs` — SQLite (GPKG / SpatiaLite) で
  共有する `KeysetRowsIter` / `OffsetRowsIter`。self-referential を避けるため
  「1 batch ごとに `prepare_cached` → `query` → drain → drop」スコープに
  `Statement` / `Rows` lifetime を閉じ込めている。
- `crates/shpx-driver-postgis/src/runtime.rs` / `crates/shpx-driver-sqlserver/src/runtime.rs`
  — `OnceLock<Runtime>` シングルトン。reader 用 background thread もこの runtime を
  再利用する。
- `crates/shpx-core/src/bench_util.rs` — peak RSS 計測ヘルパ (Linux のみ)。

## Future work

- `SHPX_READ_BATCH_SIZE` env override で batch サイズを動的調整可能にする。
- macOS / Windows の peak RSS 取得 (mach `task_info` / Windows `GetProcessMemoryInfo`
  経由)。
- `LayerReader::row_count_hint` を file 系で戦略的に提供する (例: GPKG の page count から
  推定するなど)。
