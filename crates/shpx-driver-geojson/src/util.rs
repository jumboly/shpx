//! ドライバ共通ユーティリティ。
//!
//! `shpx-driver-csv` / `shpx-driver-shp` の同名ファイルとほぼ同じ構造を取る。
//! 損失処理 / Date32 変換 / 文字列 → `Error::Driver` ラップは 3 ドライバで重複しており、
//! v0.3 で `shpx-core` 側に切り出す予定（`docs/GEOJSON.md` の Future work 参照）。

use std::sync::OnceLock;

use shpx_core::{Error, OnLoss, Result};

/// このドライバの識別名。
pub const DRIVER_NAME: &str = "geojson";

/// 損失種別の識別子。
pub mod loss_kind {
    /// GeoJSON はバイナリ列を表現できないため、geometry 以外の `Binary` 列は損失扱いにする。
    pub const BINARY_ON_GEOJSON: &str = "binary-on-geojson";
    /// `List` / `Struct` 等の構造化列は v0.2 サイクル 2 では未サポート。
    pub const STRUCTURED_ON_GEOJSON: &str = "structured-on-geojson";
    /// JSON `number` は IEEE754 で精度が落ちるため Decimal は文字列降格 (Warn) または skip。
    pub const DECIMAL_ON_GEOJSON: &str = "decimal-on-geojson";
    /// `UInt64` で `i64::MAX` を超える値（JSON `number` で正確に表現できないため）。
    pub const UINT64_OVERFLOW_ON_GEOJSON: &str = "uint64-overflow-on-geojson";
    /// `f32`/`f64` の `NaN` / `Infinity`（RFC 8259 で JSON Number 表現禁止）。
    pub const NONFINITE_FLOAT_ON_GEOJSON: &str = "nonfinite-float-on-geojson";
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
            tracing::warn!(target: "shpx::geojson", kind, field, "lossy conversion");
            Ok(true)
        }
        OnLoss::Skip => Ok(false),
    }
}

/// Date32 (`days since 1970-01-01`) ↔ `NaiveDate` 変換のヘルパ。
/// CSV / SHP ドライバ側と同じ実装の複製（v0.3 で `shpx-core` に集約予定）。
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
        let r = apply_on_loss(loss_kind::BINARY_ON_GEOJSON, "blob", OnLoss::Error);
        assert!(matches!(r, Err(Error::OnLoss { .. })));
    }

    #[test]
    fn apply_on_loss_warn_returns_continue() {
        assert!(apply_on_loss(loss_kind::BINARY_ON_GEOJSON, "blob", OnLoss::Warn).unwrap());
    }

    #[test]
    fn apply_on_loss_skip_returns_skip() {
        assert!(!apply_on_loss(loss_kind::BINARY_ON_GEOJSON, "blob", OnLoss::Skip).unwrap());
    }

    #[test]
    fn date32_epoch_is_zero() {
        assert_eq!(
            date32::to_naive(0),
            chrono::NaiveDate::from_ymd_opt(1970, 1, 1).unwrap()
        );
    }
}
