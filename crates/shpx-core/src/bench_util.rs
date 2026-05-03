//! ベンチマーク補助ユーティリティ。
//!
//! 主目的: reader streaming 化 (v0.8) の peak RSS 計測。
//! `criterion` の前後で `peak_rss_kib()` を呼んで差分を取り、
//! 「全行を一度メモリに乗せる eager 経路」と「真のストリーミング経路」のメモリ
//! プロファイルを比較する。
//!
//! # OS サポート
//!
//! - Linux: `/proc/self/status` の `VmHWM` 行を読む。`procfs` crate は使わず
//!   標準ライブラリだけで完結 (CI ubuntu-latest で動かすことが目的のため依存追加を避ける)。
//! - macOS / Windows / その他: `None` を返す。CI で計測するのは Linux のみで、
//!   ローカル開発機ではビルドが通れば良い。

#[cfg(target_os = "linux")]
mod linux_impl {
    use std::fs;

    /// `/proc/self/status` の `VmHWM` (高水位 RSS、KiB 単位) を返す。
    ///
    /// `VmHWM` は高水位なので **プロセス全体のピーク**。テスト関数の中でリセットは
    /// できない (Linux カーネル API で reset 手段が無い) ため、bench harness は
    /// 1 計測ごとにサブプロセスに分けるなどの工夫が必要 (criterion `Fork::new` は提供
    /// していないので、簡易には `peak_rss_kib()` の差分を取るだけで良い)。
    pub fn peak_rss_kib() -> Option<u64> {
        let s = fs::read_to_string("/proc/self/status").ok()?;
        for line in s.lines() {
            if let Some(rest) = line.strip_prefix("VmHWM:") {
                let kib_str = rest.split_whitespace().next()?;
                return kib_str.parse::<u64>().ok();
            }
        }
        None
    }
}

#[cfg(not(target_os = "linux"))]
mod fallback_impl {
    /// 非 Linux 環境では peak RSS の取得は未対応。`None` を返す。
    pub fn peak_rss_kib() -> Option<u64> {
        None
    }
}

/// 現プロセスのピーク常駐メモリを KiB 単位で返す (Linux 限定、それ以外は `None`)。
///
/// 高水位 (high-water mark) なので、計測区間より前に大きくなった値を反映する点に注意。
/// reader streaming のメモリプロファイルを比較する用途では、bench 直前に
/// **同等構造の eager 実装で 1 度回す → peak RSS を記録 → ストリーミング実装で 1 度回す
/// → 差分を取る**、もしくは別プロセス実行の方がノイズが少ない。
#[must_use]
pub fn peak_rss_kib() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        linux_impl::peak_rss_kib()
    }
    #[cfg(not(target_os = "linux"))]
    {
        fallback_impl::peak_rss_kib()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peak_rss_kib_returns_some_on_linux_or_none_elsewhere() {
        let v = peak_rss_kib();
        #[cfg(target_os = "linux")]
        {
            assert!(v.is_some(), "Linux must have /proc/self/status");
            assert!(v.unwrap() > 0, "process must consume non-zero memory");
        }
        #[cfg(not(target_os = "linux"))]
        {
            assert!(v.is_none(), "non-Linux returns None");
        }
    }
}
