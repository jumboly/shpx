//! ドライバ共通ユーティリティ。`shpx-driver-geojson::util` と同形だが、
//! トラッキングログのターゲットと損失種別だけを GPKG 用に差し替える。

use shpx_core::{Error, OnLoss, Result};
// driver 識別名を rdb-common 経由ではなく driver 側にローカライズする (target が const 文字列に
// しかなれないため)。`Error` の Display も driver 名を埋め込む。

/// このドライバの識別名（`Driver::name` 戻り値、`Error::Driver.name`、tracing target に使う）。
pub const DRIVER_NAME: &str = "gpkg";

/// 損失種別の識別子。
pub mod loss_kind {
    /// `Decimal128/256` を SQLite TEXT へ降格。
    pub const DECIMAL_ON_GPKG: &str = "decimal-on-gpkg";
    /// `UInt64` で `i64::MAX` を超える値（SQLite は i64 上限のため roundtrip 不能）。
    pub const UINT64_OVERFLOW_ON_GPKG: &str = "uint64-overflow-on-gpkg";
    /// CRS が無いデータを GPKG に書く際の警告（srs_id=0 で書く）。
    pub const MISSING_CRS_ON_GPKG: &str = "missing-crs-on-gpkg";
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
/// `Warn` 経路の tracing target をこの driver 用 (`shpx::gpkg`) に固定する。
///
/// `tracing::warn!` の `target:` フィールドはマクロ展開時に const を要求するため、
/// クロージャ経由で driver 側に target 文字列リテラルを残す設計にしている
/// （PostGIS / SpatiaLite / SQL Server と同型）。
pub fn apply_on_loss(kind: &'static str, field: &str, on_loss: OnLoss) -> Result<bool> {
    shpx_rdb_common::apply_on_loss(kind, field, on_loss, || {
        tracing::warn!(target: "shpx::gpkg", kind, field, "lossy conversion");
    })
}

/// SQL 識別子（テーブル名・列名）を `"..."` でクオートする。
///
/// 内部の `"` は二重化する（GPKG 仕様 / SQLite の標準）。動的 SQL を組み立てる際に
/// ユーザー由来の名前をエスケープなしに展開すると SQL injection の温床になるため、
/// 必ず本関数を経由する。
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
        let r = apply_on_loss(loss_kind::DECIMAL_ON_GPKG, "x", OnLoss::Error);
        assert!(matches!(r, Err(Error::OnLoss { .. })));
    }

    #[test]
    fn apply_on_loss_warn_continues() {
        assert!(apply_on_loss(loss_kind::DECIMAL_ON_GPKG, "x", OnLoss::Warn).unwrap());
    }

    #[test]
    fn apply_on_loss_skip_skips() {
        assert!(!apply_on_loss(loss_kind::DECIMAL_ON_GPKG, "x", OnLoss::Skip).unwrap());
    }

    #[test]
    fn quote_ident_doubles_internal_quote() {
        assert_eq!(quote_ident("col"), "\"col\"");
        assert_eq!(quote_ident("a\"b"), "\"a\"\"b\"");
    }
}
