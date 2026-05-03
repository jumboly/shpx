//! ドライバ共通ユーティリティ。GPKG の `util.rs` と同形だが、
//! tracing target と損失種別を SpatiaLite 向けに差し替える。

use shpx_core::{Error, OnLoss, Result};

/// このドライバの識別名（`Driver::name` 戻り値、`Error::Driver.name`、tracing target に使う）。
pub const DRIVER_NAME: &str = "spatialite";

/// 損失種別の識別子。
pub mod loss_kind {
    /// `Decimal128/256` を SQLite TEXT へ降格。
    pub const DECIMAL_ON_SPATIALITE: &str = "decimal-on-spatialite";
    /// `UInt64` で `i64::MAX` を超える値（SQLite は i64 上限）。
    pub const UINT64_OVERFLOW_ON_SPATIALITE: &str = "uint64-overflow-on-spatialite";
    /// CRS が無いデータを SpatiaLite に書く際の警告（srid=0 で書く）。
    pub const MISSING_CRS_ON_SPATIALITE: &str = "missing-crs-on-spatialite";
}

/// 任意の `Display` を `Error::Driver` に詰める。
pub fn driver_err<E: std::fmt::Display>(e: &E) -> Error {
    Error::driver(DRIVER_NAME, e)
}

/// 文字列メッセージから `Error::Driver` を作る。
pub fn driver_msg(msg: impl Into<String>) -> Error {
    Error::driver_msg(DRIVER_NAME, msg)
}

/// 損失検出時の挙動を 1 箇所で適用する。`shpx_rdb_common::apply_on_loss` の薄いラッパで、
/// `Warn` 経路の tracing target をこの driver 用 (`shpx::spatialite`) に固定する。
///
/// `tracing::warn!` の `target:` フィールドはマクロ展開時に const を要求するため、
/// クロージャ経由で driver 側に target 文字列リテラルを残す設計にしている。
pub fn apply_on_loss(kind: &'static str, field: &str, on_loss: OnLoss) -> Result<bool> {
    shpx_rdb_common::apply_on_loss(kind, field, on_loss, || {
        tracing::warn!(target: "shpx::spatialite", kind, field, "lossy conversion");
    })
}

/// SQL 識別子を `"..."` でクオートする。内部の `"` は二重化する。
#[must_use]
pub fn quote_ident(name: &str) -> String {
    let escaped = name.replace('"', "\"\"");
    format!("\"{escaped}\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_on_loss_error_returns_err() {
        let r = apply_on_loss(loss_kind::DECIMAL_ON_SPATIALITE, "x", OnLoss::Error);
        assert!(matches!(r, Err(Error::OnLoss { .. })));
    }

    #[test]
    fn apply_on_loss_warn_continues() {
        assert!(apply_on_loss(loss_kind::DECIMAL_ON_SPATIALITE, "x", OnLoss::Warn).unwrap());
    }

    #[test]
    fn apply_on_loss_skip_skips() {
        assert!(!apply_on_loss(loss_kind::DECIMAL_ON_SPATIALITE, "x", OnLoss::Skip).unwrap());
    }

    #[test]
    fn quote_ident_doubles_internal_quote() {
        assert_eq!(quote_ident("col"), "\"col\"");
        assert_eq!(quote_ident("a\"b"), "\"a\"\"b\"");
    }
}
