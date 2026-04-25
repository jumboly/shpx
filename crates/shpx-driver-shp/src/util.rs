//! ドライバ共通ユーティリティ。

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use shapefile::dbase;
use shpx_core::{Error, OnLoss, Result};

/// このドライバの識別名。`Error::Driver { name, .. }` 等で参照する。
pub const DRIVER_NAME: &str = "shp";

/// 損失種別の識別子。`Error::OnLoss { kind, .. }` に詰める固定文字列を集約する。
pub mod loss_kind {
    pub const Z_ON_SHP: &str = "z-on-shp";
    pub const M_ON_SHP: &str = "m-on-shp";
    pub const BINARY_ON_SHP: &str = "binary-on-shp";
    pub const DECIMAL_PRECISION_ON_DBF: &str = "decimal-precision-on-dbf";
    pub const DBF_NAME_TRUNCATION: &str = "dbf-name-truncation";
    pub const UTF8_CP_UNMAPPABLE: &str = "utf8-cp-unmappable";
    pub const UTF8_LENGTH_ON_DBF: &str = "utf8-length-on-dbf";
    pub const PRJ_WRITE_UNSUPPORTED_EPSG: &str = "prj-write-unsupported-epsg";
    pub const TIMESTAMP_TRUNCATE_ON_DBF: &str = "timestamp-truncate-on-dbf";
    pub const TIMESTAMP_TZ_ON_DBF: &str = "timestamp-tz-on-dbf";
}

/// 任意の `Display` を `Error::Driver` に詰める。`shapefile::Error` / `dbase::Error` を
/// 個別に wrap する関数を作るより、1 関数で両方賄う。
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
            tracing::warn!(target: "shpx::shp", kind, field, "lossy conversion");
            Ok(true)
        }
        OnLoss::Skip => Ok(false),
    }
}

/// `<stem>.<ext>` パスを返す。
pub fn sidecar_path(shp_path: &Path, ext: &str) -> PathBuf {
    shp_path.with_extension(ext)
}

/// `s` を `max_bytes` 以下に文字境界で切り詰めて借用スライスを返す。
pub fn truncate_at_char_boundary(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Date32 (`days since 1970-01-01`) ↔ `NaiveDate` 変換のヘルパ。
pub mod date32 {
    use super::{driver_msg, OnceLock};
    use chrono::NaiveDate;
    use shpx_core::Result;

    fn epoch() -> NaiveDate {
        static E: OnceLock<NaiveDate> = OnceLock::new();
        *E.get_or_init(|| NaiveDate::from_ymd_opt(1970, 1, 1).expect("1970-01-01 valid"))
    }

    /// `(year, month, day)` から Date32 (epoch 起算日数) を返す。
    pub fn from_ymd(y: i32, m: u32, d: u32) -> Result<i32> {
        let nd = NaiveDate::from_ymd_opt(y, m, d)
            .ok_or_else(|| driver_msg(format!("invalid calendar date {y:04}-{m:02}-{d:02}")))?;
        from_naive(nd)
    }

    /// `NaiveDate` から Date32 を返す。
    pub fn from_naive(nd: NaiveDate) -> Result<i32> {
        let days = nd.signed_duration_since(epoch()).num_days();
        i32::try_from(days).map_err(|_| driver_msg(format!("date {nd} out of Date32 range")))
    }

    /// Date32 から `NaiveDate` を返す。
    pub fn to_naive(days: i32) -> NaiveDate {
        epoch() + chrono::Duration::days(i64::from(days))
    }
}

/// `shapefile::ShapeType` の `Debug` を使わずに `&'static str` を返す。
/// reader の field metadata に DBF 元型を残す等で per-row alloc を避ける用途。
pub fn dbf_field_type_label(t: dbase::FieldType) -> &'static str {
    match t {
        dbase::FieldType::Character => "Character",
        dbase::FieldType::Date => "Date",
        dbase::FieldType::Float => "Float",
        dbase::FieldType::Numeric => "Numeric",
        dbase::FieldType::Logical => "Logical",
        dbase::FieldType::Currency => "Currency",
        dbase::FieldType::DateTime => "DateTime",
        dbase::FieldType::Integer => "Integer",
        dbase::FieldType::Double => "Double",
        dbase::FieldType::Memo => "Memo",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    #[test]
    fn apply_on_loss_error_returns_err() {
        let r = apply_on_loss(loss_kind::BINARY_ON_SHP, "blob", OnLoss::Error);
        match r {
            Err(Error::OnLoss { kind, field }) => {
                assert_eq!(kind, "binary-on-shp");
                assert_eq!(field, "blob");
            }
            _ => panic!("expected OnLoss error"),
        }
    }

    #[test]
    fn apply_on_loss_warn_returns_continue() {
        assert!(apply_on_loss(loss_kind::Z_ON_SHP, "geom", OnLoss::Warn).unwrap());
    }

    #[test]
    fn apply_on_loss_skip_returns_skip() {
        assert!(!apply_on_loss(loss_kind::Z_ON_SHP, "geom", OnLoss::Skip).unwrap());
    }

    #[test]
    fn truncate_at_char_boundary_handles_multibyte() {
        assert_eq!(truncate_at_char_boundary("abc", 10), "abc");
        assert_eq!(truncate_at_char_boundary("abcdef", 3), "abc");
        // "あ" は 3 byte。max=2 では境界の関係で空文字に切られる。
        assert_eq!(truncate_at_char_boundary("あい", 2), "");
        assert_eq!(truncate_at_char_boundary("あい", 3), "あ");
    }

    #[test]
    fn date32_roundtrip() {
        let d = date32::from_ymd(2026, 4, 25).unwrap();
        assert_eq!(
            date32::to_naive(d),
            NaiveDate::from_ymd_opt(2026, 4, 25).unwrap()
        );
        assert_eq!(date32::from_ymd(1970, 1, 1).unwrap(), 0);
    }
}
