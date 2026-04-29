//! driver crate 内で共有する tokio runtime。
//!
//! `Driver` trait は同期 API なので、async な `tiberius` を呼ぶにはランタイムが
//! 必要。プロセス内で 1 個だけ multi-thread runtime を保持し、各メソッド冒頭で
//! `block_on` する。
//!
//! `current_thread` ではなく `multi_thread` を使う理由: tiberius の bulk_insert は
//! 内部で複数の future を並行に進める必要があり、`current_thread` の `block_on`
//! 中はそれらが進まずデッドロックする。PostGIS driver の `runtime.rs` と同形。

use std::sync::OnceLock;

use tokio::runtime::Runtime;

use crate::util::driver_msg;

static RUNTIME: OnceLock<Runtime> = OnceLock::new();

/// 共有 multi-thread runtime を返す。初回呼び出しで生成される。
pub fn runtime() -> shpx_core::Result<&'static Runtime> {
    if let Some(rt) = RUNTIME.get() {
        return Ok(rt);
    }
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("shpx-sqlserver")
        .build()
        .map_err(|e| driver_msg(format!("failed to build tokio runtime: {e}")))?;
    // OnceLock::set は既に値があればその値を返す。レースで他スレッドが先に入れた
    // 場合は自分のは捨てて先入れの値を使う。
    let _ = RUNTIME.set(rt);
    RUNTIME
        .get()
        .ok_or_else(|| driver_msg("runtime initialization race"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_is_singleton() {
        let a = std::ptr::from_ref(runtime().unwrap());
        let b = std::ptr::from_ref(runtime().unwrap());
        assert_eq!(a, b);
    }
}
