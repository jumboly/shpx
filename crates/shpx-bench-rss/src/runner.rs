//! Reader 全消費 + peak RSS 計測の runner。
//!
//! `peak_rss_kib()` は `/proc/self/status::VmHWM` (高水位 RSS) を返す Linux 限定値で、
//! プロセス内では単調増加・リセット不可。したがって本 runner は **1 プロセス 1 計測**
//! を前提とし、prepare → reader 開始 → 全消費 → peak 取得 の順で 1 回だけ走る。

use std::time::Instant;

use serde::Serialize;
use shpx_core::bench_util::peak_rss_kib;
use shpx_core::{LayerReader, Result};

/// 1 計測ぶんの結果。stdout に JSON 1 行として出力する。
#[derive(Debug, Serialize)]
pub struct BenchResult {
    /// 対象 driver 識別子 (CLI `--driver` 引数の正規化値)。
    pub driver: String,
    /// CLI `--rows` 引数 (期待行数)。実際に reader が返した行数 (`row_count`) と一致するはず。
    pub rows: usize,
    /// reader が返した RecordBatch の総行数。
    pub row_count: usize,
    /// `/proc/self/status::VmHWM` (KiB)、Linux 以外は `None`。
    pub peak_rss_kib: Option<u64>,
    /// reader 開始から全消費までの経過時間 (ミリ秒)。
    pub elapsed_ms: u128,
}

/// reader を全 batch 消費して BenchResult を返す。
///
/// 計測は reader 開始 (`batches()` 取得) 直前から `reader` の最後の batch を取り終える
/// までの wall clock。peak RSS は最後の batch を取り終えた直後に取得する (それまでに
/// プロセスが確保した最大 RSS が含まれる)。
pub fn measure_read(
    driver: &str,
    rows: usize,
    reader: &mut dyn LayerReader,
) -> Result<BenchResult> {
    let started = Instant::now();
    let mut row_count: usize = 0;
    {
        let iter = reader.batches();
        for batch in iter {
            let batch = batch?;
            row_count += batch.num_rows();
        }
    }
    let elapsed_ms = started.elapsed().as_millis();
    let peak_rss_kib = peak_rss_kib();
    Ok(BenchResult {
        driver: driver.to_string(),
        rows,
        row_count,
        peak_rss_kib,
        elapsed_ms,
    })
}
