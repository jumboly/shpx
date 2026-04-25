//! ドライバ共通ユーティリティ。`shpx-driver-gpkg::util` と同形だが、
//! トラッキングログのターゲットと損失種別だけを FGB 用に差し替える。

use shpx_core::{Error, OnLoss, Result};

/// このドライバの識別名（`Driver::name` 戻り値、`Error::Driver.name`、tracing target に使う）。
pub const DRIVER_NAME: &str = "fgb";

/// 損失種別の識別子。
pub mod loss_kind {
    /// `Decimal128/256` を FGB `Double` 列へ降格（FGB に Decimal 型なし）。
    pub const DECIMAL_ON_FGB: &str = "decimal-on-fgb";
    /// `UInt64` で `i64::MAX` を超える値（FGB の整数列は Long(i64) まで）。
    pub const UINT64_OVERFLOW_ON_FGB: &str = "uint64-overflow-on-fgb";
    /// CRS が無いデータを FGB に書く際の警告（header の crs を 0/空で出す）。
    pub const MISSING_CRS_ON_FGB: &str = "missing-crs-on-fgb";
}

/// 任意の `Display` を `Error::Driver` に詰める。
pub fn driver_err<E: std::fmt::Display>(e: &E) -> Error {
    Error::driver(DRIVER_NAME, e)
}

/// 文字列メッセージから `Error::Driver` を作る。
pub fn driver_msg(msg: impl Into<String>) -> Error {
    Error::driver_msg(DRIVER_NAME, msg)
}

/// 損失検出時の挙動を 1 箇所で適用する。
///
/// 戻り値:
/// - `Ok(true)`  — 続行（`Warn` 経路）
/// - `Ok(false)` — その要素 (列・値) をスキップ
/// - `Err(_)`    — `OnLoss::Error` での中断
pub fn apply_on_loss(kind: &'static str, field: &str, on_loss: OnLoss) -> Result<bool> {
    match on_loss {
        OnLoss::Error => Err(Error::OnLoss {
            kind: kind.to_string(),
            field: field.to_string(),
        }),
        OnLoss::Warn => {
            tracing::warn!(target: "shpx::fgb", kind, field, "lossy conversion");
            Ok(true)
        }
        OnLoss::Skip => Ok(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_on_loss_error_returns_err() {
        let r = apply_on_loss(loss_kind::DECIMAL_ON_FGB, "x", OnLoss::Error);
        assert!(matches!(r, Err(Error::OnLoss { .. })));
    }

    #[test]
    fn apply_on_loss_warn_continues() {
        assert!(apply_on_loss(loss_kind::DECIMAL_ON_FGB, "x", OnLoss::Warn).unwrap());
    }

    #[test]
    fn apply_on_loss_skip_skips() {
        assert!(!apply_on_loss(loss_kind::DECIMAL_ON_FGB, "x", OnLoss::Skip).unwrap());
    }
}
