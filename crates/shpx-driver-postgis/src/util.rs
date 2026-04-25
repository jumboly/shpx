//! ドライバ共通ユーティリティ。GPKG/FGB driver の `util.rs` と同形パターン。

use shpx_core::{Error, OnLoss, Result};

/// このドライバの識別名（`Driver::name` 戻り値、`Error::Driver.name`、tracing target に使う）。
pub const DRIVER_NAME: &str = "postgis";

/// 損失種別の識別子。
pub mod loss_kind {
    /// CRS が無いデータを PostGIS に書く際の警告（srid=0 で書く）。
    pub const MISSING_CRS_ON_POSTGIS: &str = "missing-crs-on-postgis";
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
            tracing::warn!(target: "shpx::postgis", kind, field, "lossy conversion");
            Ok(true)
        }
        OnLoss::Skip => Ok(false),
    }
}

/// SQL 識別子（テーブル名・列名）を `"..."` でクオートする。
///
/// 内部の `"` は二重化する（PostgreSQL の標準）。動的 SQL を組み立てる際に
/// ユーザー由来の名前をエスケープなしに展開すると SQL injection の温床になるため、
/// 必ず本関数を経由する。
#[must_use]
pub fn quote_ident(name: &str) -> String {
    let escaped = name.replace('"', "\"\"");
    format!("\"{escaped}\"")
}

/// `schema.name` 形式（または `name`）を quote 済みの完全修飾名にする。
/// schema 省略時は `public` を補う。
#[must_use]
pub fn quote_qualified(schema: &str, table: &str) -> String {
    format!("{}.{}", quote_ident(schema), quote_ident(table))
}

/// PostgreSQL の文字列リテラル `'...'` 用に `'` を `''` でエスケープする。
/// SQL identifier ではない値のリテラル化に使う（PostGIS の geometry type 名等）。
#[must_use]
pub fn quote_literal(s: &str) -> String {
    let escaped = s.replace('\'', "''");
    format!("'{escaped}'")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quote_ident_doubles_internal_quote() {
        assert_eq!(quote_ident("col"), "\"col\"");
        assert_eq!(quote_ident("a\"b"), "\"a\"\"b\"");
    }

    #[test]
    fn quote_qualified_includes_schema() {
        assert_eq!(quote_qualified("public", "x"), "\"public\".\"x\"");
        assert_eq!(quote_qualified("my-schema", "t"), "\"my-schema\".\"t\"");
    }

    #[test]
    fn apply_on_loss_error_returns_err() {
        let r = apply_on_loss(loss_kind::MISSING_CRS_ON_POSTGIS, "x", OnLoss::Error);
        assert!(matches!(r, Err(Error::OnLoss { .. })));
    }

    #[test]
    fn apply_on_loss_warn_continues() {
        assert!(apply_on_loss(loss_kind::MISSING_CRS_ON_POSTGIS, "x", OnLoss::Warn).unwrap());
    }

    #[test]
    fn apply_on_loss_skip_skips() {
        assert!(!apply_on_loss(loss_kind::MISSING_CRS_ON_POSTGIS, "x", OnLoss::Skip).unwrap());
    }
}
