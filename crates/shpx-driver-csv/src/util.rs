//! ドライバ共通ユーティリティ。
//!
//! `shpx-driver-shp` の同名ファイルとほぼ同じ構造を取る。Date32 や
//! エンコーディング解決の重複は v0.2 サイクル 2 以降で `shpx-core` 側に
//! 切り出す予定（DESIGN.md / docs/CSV.md の Future work 参照）。

use std::sync::OnceLock;

use shpx_core::{Error, OnLoss, Result};

/// このドライバの識別名。
pub const DRIVER_NAME: &str = "csv";

/// 損失種別の識別子。
pub mod loss_kind {
    /// CSV はバイナリ列を表現できないため、geometry 以外の `Binary` 列は損失扱いにする。
    pub const BINARY_ON_CSV: &str = "binary-on-csv";
    /// `List` / `Struct` 等の構造化列は v0.2 サイクル 1 では未サポート。
    pub const STRUCTURED_ON_CSV: &str = "structured-on-csv";
    /// 出力エンコーディングで表現できない文字（CP932 へ絵文字を流す等）。
    pub const ENCODING_UNMAPPABLE: &str = "encoding-unmappable";
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
/// - `Ok(true)` — 続行（`Warn` 経路）
/// - `Ok(false)` — その要素 (列・値) をスキップ
/// - `Err(_)` — `OnLoss::Error` での中断
pub fn apply_on_loss(kind: &'static str, field: &str, on_loss: OnLoss) -> Result<bool> {
    match on_loss {
        OnLoss::Error => Err(Error::OnLoss {
            kind: kind.to_string(),
            field: field.to_string(),
        }),
        OnLoss::Warn => {
            tracing::warn!(target: "shpx::csv", kind, field, "lossy conversion");
            Ok(true)
        }
        OnLoss::Skip => Ok(false),
    }
}

/// Date32 (`days since 1970-01-01`) ↔ `NaiveDate` 変換のヘルパ。
/// SHP ドライバ側と同じ実装の複製（次サイクルで `shpx-core` に集約予定）。
pub mod date32 {
    use super::OnceLock;
    use chrono::NaiveDate;

    fn epoch() -> NaiveDate {
        static E: OnceLock<NaiveDate> = OnceLock::new();
        *E.get_or_init(|| NaiveDate::from_ymd_opt(1970, 1, 1).expect("1970-01-01 valid"))
    }

    /// Date32 から `NaiveDate` を返す。
    pub fn to_naive(days: i32) -> NaiveDate {
        epoch() + chrono::Duration::days(i64::from(days))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_on_loss_error_returns_err() {
        let r = apply_on_loss(loss_kind::BINARY_ON_CSV, "blob", OnLoss::Error);
        assert!(matches!(r, Err(Error::OnLoss { .. })));
    }

    #[test]
    fn apply_on_loss_warn_returns_continue() {
        assert!(apply_on_loss(loss_kind::BINARY_ON_CSV, "blob", OnLoss::Warn).unwrap());
    }

    #[test]
    fn apply_on_loss_skip_returns_skip() {
        assert!(!apply_on_loss(loss_kind::BINARY_ON_CSV, "blob", OnLoss::Skip).unwrap());
    }

    #[test]
    fn date32_epoch_is_zero() {
        assert_eq!(
            date32::to_naive(0),
            chrono::NaiveDate::from_ymd_opt(1970, 1, 1).unwrap()
        );
    }
}
