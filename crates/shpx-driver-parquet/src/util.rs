//! ドライバ共通ユーティリティ。

use shpx_core::Error;

/// このドライバの識別名。`Error::Driver { name, .. }` 等で参照する。
pub const DRIVER_NAME: &str = "parquet";

/// 任意の `Display` を `Error::Driver` に詰める（`shpx_core::Error::driver` への薄いラッパ）。
pub fn driver_err<E: std::fmt::Display>(e: &E) -> Error {
    Error::driver(DRIVER_NAME, e)
}

/// 文字列メッセージから `Error::Driver` を作る。
pub fn driver_msg(msg: impl Into<String>) -> Error {
    Error::driver_msg(DRIVER_NAME, msg)
}
